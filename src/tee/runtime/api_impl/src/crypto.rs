// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use tee_internal::{
    Algorithm, AttributeId, Error, Mode, OperationHandle, Result as TeeResult, Usage,
};

use crate::storage::{Key, KeyType, Object};

// TODO(https://fxbug.dev/360942581): Figure out operation state transitions.
#[allow(dead_code)]
enum OpState {
    Initial,
    Active,
    Extracting,
}

pub struct Operation {
    algorithm: Algorithm,
    mode: Mode,
    data: Vec<u8>,
    state: OpState,
}

impl Operation {
    fn new(algorithm: Algorithm, mode: Mode, max_key_size: u32) -> Self {
        Self {
            algorithm,
            mode,
            data: Vec::with_capacity(max_key_size as usize),
            state: OpState::Initial,
        }
    }

    fn set_key(&mut self, key: Rc<RefCell<dyn Object>>) -> TeeResult {
        let key = key.borrow();
        match self.algorithm {
            Algorithm::AesCbcNopad | Algorithm::AesEcbNopad => match self.mode {
                Mode::Encrypt | Mode::Decrypt => {
                    let usage = key.usage();
                    if self.mode == Mode::Encrypt {
                        assert!(usage.contains(Usage::ENCRYPT | Usage::VERIFY));
                    } else {
                        assert!(usage.contains(Usage::DECRYPT | Usage::SIGN));
                    }
                    let aes = if let Key::Aes(aes) = key.key() {
                        aes
                    } else {
                        panic!("Wrong key type - expected AES");
                    };
                    assert!(self.data.len() + aes.size() as usize <= self.data.capacity());
                    let secret = aes.buffer_attribute(AttributeId::SecretValue).unwrap();
                    self.data.extend_from_slice(&secret);
                    Ok(())
                }
                _ => Err(Error::NotImplemented),
            },
            _ => Err(Error::NotImplemented),
        }
    }

    fn clear_key(&mut self) -> TeeResult {
        self.data.clear();
        self.state = OpState::Initial;
        Ok(())
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
        match algorithm {
            Algorithm::AesCbcNopad | Algorithm::AesEcbNopad => {
                match mode {
                    Mode::Encrypt | Mode::Decrypt => {}
                    _ => {
                        return Err(Error::NotSupported);
                    }
                };
                // TODO: check that max_key_size matches table 5-9
                let operation = Operation::new(algorithm, mode, max_key_size);
                let handle = self.allocate_operation_handle();
                let prev = self.operations.insert(handle, RefCell::new(operation));
                debug_assert!(prev.is_none());
                Ok(handle)
            }
            _ => {
                inspect_stubs::track_stub!(
                    TODO("https://fxbug.dev/360942581"),
                    "unsupported algorithm",
                );
                Err(Error::NotImplemented)
            }
        }
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
        let _ = self.operations.remove(&operation).unwrap();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn operation_lifecycle() -> Result<(), Error> {
        let mut operations = Operations::new();

        let operation = operations.allocate(Algorithm::AesCbcNopad, Mode::Encrypt, 128).unwrap();

        operations.free(operation);

        Ok(())
    }
}
