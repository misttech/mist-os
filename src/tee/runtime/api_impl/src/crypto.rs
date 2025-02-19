// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::{RefCell, RefMut};
use std::cmp::min;
use std::collections::HashMap;
use std::rc::Rc;

use digest::DynDigest as Digest;
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};
use tee_internal::{Algorithm, EccCurve, Error, Mode, OperationHandle, Result as TeeResult, Usage};

use crate::storage::{AesKey, Key, KeyType as _, NoKey, Object};

pub fn is_algorithm_supported(alg: Algorithm, element: EccCurve) -> bool {
    if element != EccCurve::None {
        return false;
    }
    match alg {
        Algorithm::Sha1
        | Algorithm::Sha224
        | Algorithm::Sha256
        | Algorithm::Sha384
        | Algorithm::Sha512 => true,
        _ => false,
    }
}

// Encapsulated an abstracted helper classes particular to supported
// algorithms.
enum Helper {
    Digest(Box<dyn Digest>),
    // TODO(https://fxbug.dev/360942581): Add more...
}

impl Helper {
    fn new(algorithm: Algorithm) -> TeeResult<Self> {
        match algorithm {
            Algorithm::Sha1 => Ok(Helper::Digest(Box::new(Sha1::default()))),
            Algorithm::Sha224 => Ok(Helper::Digest(Box::new(Sha224::default()))),
            Algorithm::Sha256 => Ok(Helper::Digest(Box::new(Sha256::default()))),
            Algorithm::Sha384 => Ok(Helper::Digest(Box::new(Sha384::default()))),
            Algorithm::Sha512 => Ok(Helper::Digest(Box::new(Sha512::default()))),
            _ => Err(Error::NotSupported),
        }
    }

    fn initialize(&mut self, key: &Key) {
        match self {
            Helper::Digest(digest) => {
                // Digests do not need initialization.
                assert!(matches!(key, Key::Data(NoKey {})));
                digest.reset()
            }
        }
    }

    fn reset(&mut self) {
        match self {
            Helper::Digest(digest) => digest.reset(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum OpState {
    Initial,
    Active,
    // Holds the finalized data yet to be fully extracted, along with an index
    // pointing to the next byte to extract.
    Extracting((Vec<u8>, usize)),
}

pub struct Operation {
    algorithm: Algorithm,
    mode: Mode,
    key: Key,
    max_key_size: u32, // The initial, allocated max key size.
    state: OpState,
    helper: Helper,
}

impl Operation {
    fn new(algorithm: Algorithm, mode: Mode, max_key_size: u32) -> TeeResult<Self> {
        Ok(Self {
            algorithm,
            mode,
            key: Key::Data(NoKey {}),
            max_key_size,
            state: OpState::Initial,
            helper: Helper::new(algorithm)?,
        })
    }

    fn as_digest(&mut self) -> &mut Box<dyn Digest> {
        let Helper::Digest(ref mut digest) = &mut self.helper;
        digest
    }

    // Returns whether the operation is in the extracting state and, if so, the
    // number of remaining bytes left to extract.
    fn is_extracting(&self) -> (bool, usize) {
        if let OpState::Extracting((ref data, ref pos)) = self.state {
            (true, data.len() - pos)
        } else {
            (false, 0)
        }
    }

    fn reset(&mut self) {
        self.helper.reset();
        self.state = OpState::Initial;
    }

    fn set_key(&mut self, obj: Rc<RefCell<dyn Object>>) -> TeeResult {
        let obj = obj.borrow();
        let key = obj.key();

        assert!(
            key.max_size() <= self.max_key_size,
            "Provided key size ({}) exceeds configured max ({})",
            key.max_size(),
            self.max_key_size
        );

        assert_eq!(
            self.state,
            OpState::Initial,
            "Operation must be in the initial state (not {:?})",
            self.state
        );

        match self.algorithm {
            Algorithm::AesCbcNopad | Algorithm::AesEcbNopad => match self.mode {
                Mode::Encrypt | Mode::Decrypt => {
                    let usage = obj.usage();
                    if self.mode == Mode::Encrypt {
                        assert!(usage.contains(Usage::ENCRYPT | Usage::VERIFY));
                    } else {
                        assert!(usage.contains(Usage::DECRYPT | Usage::SIGN));
                    }
                    if !matches!(key, Key::Aes(_)) {
                        panic!("Wrong key type ({:?}) - expected AES", key.get_type());
                    }
                }
                _ => return Err(Error::NotImplemented),
            },
            Algorithm::Md5
            | Algorithm::Sha1
            | Algorithm::Sha224
            | Algorithm::Sha256
            | Algorithm::Sha384
            | Algorithm::Sha512
            | Algorithm::Sha3_224
            | Algorithm::Sha3_256
            | Algorithm::Sha3_384
            | Algorithm::Sha3_512
            | Algorithm::Shake128
            | Algorithm::Shake256 => {
                panic!("Algorithm {:?} has no associated object type", self.algorithm);
            }
            _ => return Err(Error::NotImplemented),
        };
        self.key = key.clone();
        self.helper.initialize(&self.key);
        Ok(())
    }

    fn clear_key(&mut self) -> TeeResult {
        self.key = Key::Data(NoKey {});
        self.state = OpState::Initial;
        Ok(())
    }

    // Provided the operation is in its extracting state, this reads as many
    // bytes of that data as possible into the provided buffer, returning the
    // size of the read.
    fn extract_finalized(&mut self, buf: &mut [u8]) -> usize {
        let OpState::Extracting((ref data, ref mut pos)) = self.state else {
            panic!("Operation is not in the extracting state: {:?}", self.state);
        };
        if buf.is_empty() || *pos >= data.len() {
            return 0;
        }
        let read_size = min(data.len() - *pos, buf.len());
        let in_chunk = &data.as_slice()[*pos..(*pos + read_size)];
        let out_chunk = &mut buf[..read_size];
        out_chunk.copy_from_slice(in_chunk);
        *pos += read_size;
        read_size
    }

    // See TEE_DigestUpdate().
    fn update_digest(&mut self, chunk: &[u8]) {
        assert_eq!(self.mode, Mode::Digest);
        assert!(self.state == OpState::Initial || self.state == OpState::Active);
        self.as_digest().update(chunk);
        self.state = OpState::Active;
    }

    // See TEE_DigestDoFinal().
    //
    // This should be two separate operations each with clean semantics:
    // update + finalize. However, the spec wants the two zipped together here
    // where the first can't happen if the preconditions of the second aren't
    // met, adding complication.
    fn update_and_finalize_digest_into(
        &mut self,
        last_chunk: &[u8],
        buf: &mut [u8],
    ) -> Result<(), usize> {
        assert_eq!(self.mode, Mode::Digest);

        if let (true, left_to_extract) = self.is_extracting() {
            assert!(last_chunk.is_empty());

            if left_to_extract > buf.len() {
                return Err(left_to_extract);
            }

            let written = self.extract_digest(buf);
            debug_assert_eq!(written, left_to_extract);
            self.state = OpState::Initial;
            return Ok(());
        }

        let buf = {
            let digest = self.as_digest();
            let output_size = digest.output_size();
            if output_size > buf.len() {
                return Err(output_size);
            }

            if !last_chunk.is_empty() {
                digest.update(last_chunk);
            }
            &mut buf[..output_size]
        };

        self.as_digest().finalize_into_reset(buf).unwrap();
        self.state = OpState::Initial;
        Ok(())
    }

    // Finalizes the digest and puts the operation in the extracting state. If
    // already in the extracting state, this is a no-op.
    fn finalize_digest(&mut self) {
        assert_eq!(self.mode, Mode::Digest);
        let (extracting, _) = self.is_extracting();
        if extracting {
            return;
        }

        let bytes = self.as_digest().finalize_reset();
        self.state = OpState::Extracting((Vec::from(bytes), 0));
    }

    // See TEE_DigestExtract().
    fn extract_digest(&mut self, buf: &mut [u8]) -> usize {
        self.finalize_digest();
        self.extract_finalized(buf)
    }
}

pub struct Operations {
    operations: HashMap<OperationHandle, RefCell<Operation>>,
    next_operation_handle_value: OperationHandle,
}

impl Operations {
    pub fn new() -> Self {
        Self {
            operations: HashMap::new(),
            next_operation_handle_value: OperationHandle::from_value(1),
        }
    }

    pub fn allocate(
        &mut self,
        algorithm: Algorithm,
        mode: Mode,
        max_key_size: u32,
    ) -> TeeResult<OperationHandle> {
        // We could directly check `FooKey::is_valid_size(max_key_size)` in
        // each match arm, but by forwarding the appropriate key size check
        // function pointer and doing it indirectly after the match statement,
        // we ensure that the check is always made and reduce a bit of
        // boilerplate while we're at it.
        let is_valid_key_size = match algorithm {
            Algorithm::AesCbcNopad | Algorithm::AesEcbNopad => {
                match mode {
                    Mode::Encrypt | Mode::Decrypt => {}
                    _ => {
                        return Err(Error::NotSupported);
                    }
                };
                AesKey::is_valid_size
            }
            Algorithm::Md5
            | Algorithm::Sha1
            | Algorithm::Sha224
            | Algorithm::Sha256
            | Algorithm::Sha384
            | Algorithm::Sha512
            | Algorithm::Sha3_224
            | Algorithm::Sha3_256
            | Algorithm::Sha3_384
            | Algorithm::Sha3_512
            | Algorithm::Shake128
            | Algorithm::Shake256 => {
                if mode != Mode::Digest {
                    return Err(Error::NotSupported);
                }
                NoKey::is_valid_size
            }
            _ => {
                inspect_stubs::track_stub!(
                    TODO("https://fxbug.dev/360942581"),
                    "unsupported algorithm",
                );
                return Err(Error::NotImplemented);
            }
        };
        if !is_valid_key_size(max_key_size) {
            return Err(Error::NotSupported);
        }
        let operation = Operation::new(algorithm, mode, max_key_size)?;
        let handle = self.allocate_operation_handle();
        let prev = self.operations.insert(handle, RefCell::new(operation));
        debug_assert!(prev.is_none());
        Ok(handle)
    }

    fn allocate_operation_handle(&mut self) -> OperationHandle {
        let handle = self.next_operation_handle_value;
        self.next_operation_handle_value = OperationHandle::from_value(*handle + 1);
        handle
    }

    fn get_mut(&self, operation: OperationHandle) -> RefMut<'_, Operation> {
        self.operations.get(&operation).unwrap().borrow_mut()
    }

    pub fn free(&mut self, operation: OperationHandle) {
        if operation.is_null() {
            return;
        }
        let _ = self.operations.remove(&operation).unwrap();
    }

    pub fn reset(&mut self, operation: OperationHandle) {
        self.get_mut(operation).reset()
    }

    pub fn set_key(
        &mut self,
        operation: OperationHandle,
        key: Rc<RefCell<dyn Object>>,
    ) -> TeeResult {
        self.get_mut(operation).set_key(key)
    }

    pub fn clear_key(&mut self, operation: OperationHandle) -> TeeResult {
        self.get_mut(operation).clear_key()
    }

    pub fn update_digest(&mut self, operation: OperationHandle, chunk: &[u8]) {
        self.get_mut(operation).update_digest(chunk);
    }

    pub fn update_and_finalize_digest_into(
        &mut self,
        operation: OperationHandle,
        last_chunk: &[u8],
        buf: &mut [u8],
    ) -> Result<(), usize> {
        self.get_mut(operation).update_and_finalize_digest_into(last_chunk, buf)
    }

    pub fn extract_digest<'a>(&mut self, operation: OperationHandle, buf: &'a mut [u8]) -> usize {
        self.get_mut(operation).extract_digest(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn operation_lifecycle() -> Result<(), Error> {
        let mut operations = Operations::new();

        let operation = operations.allocate(Algorithm::Sha256, Mode::Digest, 0).unwrap();

        operations.free(operation);

        Ok(())
    }
}
