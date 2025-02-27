// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::{RefCell, RefMut};
use std::cmp::min;
use std::collections::HashMap;
use std::iter;
use std::marker::PhantomData;
use std::rc::Rc;

use aes::{Aes128, Aes192, Aes256};
use cbc::{Decryptor as CbcDecryptor, Encryptor as CbcEncryptor};
use cmac::Cmac;
use crypto_common::{KeyInit, KeyIvInit};
use digest::DynDigest as Digest;
use ecb::{Decryptor as EcbDecryptor, Encryptor as EcbEncryptor};
use hmac::Hmac;
use rand_core::{CryptoRng, RngCore};
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};
use tee_internal::{Algorithm, EccCurve, Error, Mode, OperationHandle, Result as TeeResult, Usage};

use crate::storage::{
    AesKey, HmacSha1Key, HmacSha224Key, HmacSha256Key, HmacSha384Key, HmacSha512Key, Key,
    KeyType as _, NoKey, Object,
};

type AesCmac128 = Cmac<Aes128>;
type AesCmac192 = Cmac<Aes192>;
type AesCmac256 = Cmac<Aes256>;
type HmacSha1 = Hmac<Sha1>;
type HmacSha224 = Hmac<Sha224>;
type HmacSha256 = Hmac<Sha256>;
type HmacSha384 = Hmac<Sha384>;
type HmacSha512 = Hmac<Sha512>;

pub fn is_algorithm_supported(alg: Algorithm, element: EccCurve) -> bool {
    if element != EccCurve::None {
        return false;
    }
    match alg {
        Algorithm::Sha1
        | Algorithm::Sha224
        | Algorithm::Sha256
        | Algorithm::Sha384
        | Algorithm::Sha512
        | Algorithm::AesCbcNopad
        | Algorithm::AesEcbNopad
        | Algorithm::AesCmac
        | Algorithm::HmacSha1
        | Algorithm::HmacSha224
        | Algorithm::HmacSha256
        | Algorithm::HmacSha384
        | Algorithm::HmacSha512 => true,
        _ => false,
    }
}

// An RNG abstraction in the shape expected by RustCrypto APIs.
pub(crate) struct Rng {}

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let val = 0u32;
        self.fill_bytes(&mut val.to_le_bytes());
        val
    }

    fn next_u64(&mut self) -> u64 {
        let val = 0u64;
        self.fill_bytes(&mut val.to_le_bytes());
        val
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        zx::cprng_draw(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for Rng {}

// A MAC abstraction conveniently shaped for our API glue needs.
trait Mac {
    fn output_size(&self) -> usize;

    fn update(&mut self, data: &[u8]);

    fn reset(&mut self);

    fn finalize_into_reset(&mut self, out: &mut [u8]);

    // Returns Error::MacInvalid in the case of failure.
    fn verify_reset(&mut self, expected: &[u8]) -> TeeResult;
}

// Implementations for the hmac digest types.
impl<M> Mac for M
where
    M: digest::FixedOutputReset + digest::MacMarker + digest::Update,
{
    fn output_size(&self) -> usize {
        // OutputSizeUser is a subtrait of FixedOutputReset.
        <Self as digest::OutputSizeUser>::output_size()
    }

    fn update(&mut self, data: &[u8]) {
        <Self as digest::Update>::update(self, data)
    }

    fn reset(&mut self) {
        // Reset is a subtrait of FixedOutputReset.
        <Self as digest::Reset>::reset(self)
    }

    fn finalize_into_reset(&mut self, out: &mut [u8]) {
        <Self as digest::FixedOutputReset>::finalize_into_reset(self, out.into())
    }

    fn verify_reset(&mut self, expected: &[u8]) -> TeeResult {
        let finalized = <Self as digest::FixedOutputReset>::finalize_fixed_reset(self);
        if finalized.as_slice() == expected {
            Ok(())
        } else {
            Err(Error::MacInvalid)
        }
    }
}

// Supported MAC algorithm types.
enum MacType {
    AesCmac,
    HmacSha1,
    HmacSha224,
    HmacSha256,
    HmacSha384,
    HmacSha512,
}

// A cipher abstraction conveniently shaped for our API glue needs.
trait Cipher {
    fn block_size(&self) -> usize;
    fn set_iv(&mut self, iv: &[u8]);
    fn reset(&mut self);
    fn encrypt(&self, input: &[u8], output: &mut [u8]);
    fn encrypt_in_place(&self, inout: &mut [u8]);
    fn decrypt(&self, input: &[u8], output: &mut [u8]);
    fn decrypt_in_place(&self, inout: &mut [u8]);
}

impl<C: PreCipher> Cipher for C {
    fn set_iv(&mut self, iv: &[u8]) {
        self.set_iv(iv)
    }

    fn reset(&mut self) {
        self.reset()
    }

    fn block_size(&self) -> usize {
        debug_assert_eq!(C::Encryptor::block_size(), C::Decryptor::block_size());
        C::Encryptor::block_size()
    }

    fn encrypt(&self, input: &[u8], output: &mut [u8]) {
        self.new_encryptor().encrypt(input, output);
    }

    fn encrypt_in_place(&self, inout: &mut [u8]) {
        self.new_encryptor().encrypt_in_place(inout)
    }

    fn decrypt(&self, input: &[u8], output: &mut [u8]) {
        self.new_decryptor().decrypt(input, output)
    }

    fn decrypt_in_place(&self, inout: &mut [u8]) {
        self.new_decryptor().decrypt_in_place(inout)
    }
}

// Ideally, we'd just use a trait like this in place of Cipher, but the
// presence of associated types makes it non-dyn-compatible.
trait PreCipher {
    type Encryptor: Encryptor;
    type Decryptor: Decryptor;

    fn set_iv(&mut self, iv: &[u8]);
    fn reset(&mut self);

    // The minting of new encryptors or decryptors in general should happen
    // only after set_iv() has been called.
    fn new_encryptor(&self) -> Self::Encryptor;
    fn new_decryptor(&self) -> Self::Decryptor;
}

trait Encryptor {
    fn block_size() -> usize;
    fn encrypt(&mut self, input: &[u8], output: &mut [u8]);
    fn encrypt_in_place(&mut self, inout: &mut [u8]);
}

trait Decryptor {
    fn block_size() -> usize;
    fn decrypt(&mut self, input: &[u8], output: &mut [u8]);
    fn decrypt_in_place(&mut self, inout: &mut [u8]);
}

// A general cipher type that requires an initialization vector.
struct CipherWithIv<E, D>
where
    E: Encryptor + KeyIvInit,
    D: Decryptor + KeyIvInit,
{
    key: Vec<u8>,
    iv: Vec<u8>,
    phantom: PhantomData<(E, D)>,
}

impl<E, D> CipherWithIv<E, D>
where
    E: Encryptor + KeyIvInit,
    D: Decryptor + KeyIvInit,
{
    fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec(), iv: Vec::new(), phantom: PhantomData::default() }
    }
}

impl<E, D> PreCipher for CipherWithIv<E, D>
where
    E: Encryptor + KeyIvInit,
    D: Decryptor + KeyIvInit,
{
    type Encryptor = E;
    type Decryptor = D;

    fn set_iv(&mut self, iv: &[u8]) {
        self.iv = iv.to_vec()
    }

    fn reset(&mut self) {
        self.iv.clear()
    }

    fn new_encryptor(&self) -> E {
        E::new_from_slices(&self.key, &self.iv).unwrap()
    }

    fn new_decryptor(&self) -> D {
        D::new_from_slices(&self.key, &self.iv).unwrap()
    }
}

// A general cipher type that does not require an initialization vector.
struct CipherWithoutIv<E, D>
where
    E: Encryptor + KeyInit,
    D: Decryptor + KeyInit,
{
    key: Vec<u8>,
    phantom: PhantomData<(E, D)>,
}

impl<E, D> CipherWithoutIv<E, D>
where
    E: Encryptor + KeyInit,
    D: Decryptor + KeyInit,
{
    fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec(), phantom: PhantomData::default() }
    }
}

impl<E, D> PreCipher for CipherWithoutIv<E, D>
where
    E: Encryptor + KeyInit,
    D: Decryptor + KeyInit,
{
    type Encryptor = E;
    type Decryptor = D;

    // Why not panic? Two reasons:
    // * the spec does not prescribe any behaviour for calling CipherInit(iv)
    //   for an algorithm that does require an IV, though it does prescribe
    //   ignoring any IVs passed to MAC algorithms with MacInit(), so there's
    //   an argument for consistency;
    // * it simplifies the one intended callsite of CipherInit() to make
    //   set_iv() and unconditional call.
    fn set_iv(&mut self, _iv: &[u8]) {}

    fn reset(&mut self) {}

    fn new_encryptor(&self) -> E {
        E::new_from_slice(&self.key).unwrap()
    }
    fn new_decryptor(&self) -> D {
        D::new_from_slice(&self.key).unwrap()
    }
}

// Provides Encryptor and Decryptor implementations for some of the
// RustCrypto-shaped encryptors and decryptors (which sadly don't implement
// some official trait themselves encoding their API).
//
// We use token trees in the macro matcher to permit the use of `$encryptor<C>`
// and `$decryptor<C>`, which wouldn't parse if specified more naturally as
// type paths.
macro_rules! rustcrypto_encryptor_and_decryptor {
    ($encryptor:tt, $decryptor:tt) => {
        impl<C> Encryptor for $encryptor<C>
        where
            C: cipher::BlockCipher + cipher::BlockEncryptMut,
        {
            fn block_size() -> usize {
                C::block_size()
            }

            fn encrypt(&mut self, input: &[u8], output: &mut [u8]) {
                use cipher::BlockEncryptMut;

                assert!(output.len() >= input.len());
                let block_size = C::block_size();
                let chunks =
                    iter::zip(input.chunks_exact(block_size), output.chunks_exact_mut(block_size));
                for (in_block, out_block) in chunks {
                    self.encrypt_block_b2b_mut(in_block.into(), out_block.into());
                }
            }

            fn encrypt_in_place(&mut self, inout: &mut [u8]) {
                use cipher::BlockEncryptMut;

                for block in inout.chunks_exact_mut(C::block_size()) {
                    self.encrypt_block_mut(block.into())
                }
            }
        }

        impl<C> Decryptor for $decryptor<C>
        where
            C: cipher::BlockCipher + cipher::BlockDecryptMut,
        {
            fn block_size() -> usize {
                C::block_size()
            }

            fn decrypt(&mut self, input: &[u8], output: &mut [u8]) {
                use cipher::BlockDecryptMut;

                assert!(output.len() >= input.len());
                let block_size = C::block_size();
                let chunks =
                    iter::zip(input.chunks_exact(block_size), output.chunks_exact_mut(block_size));
                for (in_block, out_block) in chunks {
                    self.decrypt_block_b2b_mut(in_block.into(), out_block.into());
                }
            }

            fn decrypt_in_place(&mut self, inout: &mut [u8]) {
                use cipher::BlockDecryptMut;

                for block in inout.chunks_exact_mut(C::block_size()) {
                    self.decrypt_block_mut(block.into())
                }
            }
        }
    };
}

rustcrypto_encryptor_and_decryptor!(CbcEncryptor, CbcDecryptor);
rustcrypto_encryptor_and_decryptor!(EcbEncryptor, EcbDecryptor);

type AesCbcNopad<C> = CipherWithIv<cbc::Encryptor<C>, cbc::Decryptor<C>>;
type Aes128CbcNopad = AesCbcNopad<Aes128>;
type Aes192CbcNopad = AesCbcNopad<Aes192>;
type Aes256CbcNopad = AesCbcNopad<Aes256>;

type AesEcbNopad<C> = CipherWithoutIv<ecb::Encryptor<C>, ecb::Decryptor<C>>;
type Aes128EcbNopad = AesEcbNopad<Aes128>;
type Aes192EcbNopad = AesEcbNopad<Aes192>;
type Aes256EcbNopad = AesEcbNopad<Aes256>;

enum CipherType {
    AesCbcNopad,
    AesEcbNopad,
}

// Encapsulated an abstracted helper classes particular to supported
// algorithms.
enum Helper {
    Digest(Box<dyn Digest>),
    Cipher(Option<Box<dyn Cipher>>, CipherType),
    Mac(Option<Box<dyn Mac>>, MacType),
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
            Algorithm::AesCbcNopad => Ok(Helper::Cipher(None, CipherType::AesCbcNopad)),
            Algorithm::AesEcbNopad => Ok(Helper::Cipher(None, CipherType::AesEcbNopad)),
            Algorithm::AesCmac => Ok(Helper::Mac(None, MacType::AesCmac)),
            Algorithm::HmacSha1 => Ok(Helper::Mac(None, MacType::HmacSha1)),
            Algorithm::HmacSha224 => Ok(Helper::Mac(None, MacType::HmacSha224)),
            Algorithm::HmacSha256 => Ok(Helper::Mac(None, MacType::HmacSha256)),
            Algorithm::HmacSha384 => Ok(Helper::Mac(None, MacType::HmacSha384)),
            Algorithm::HmacSha512 => Ok(Helper::Mac(None, MacType::HmacSha512)),
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
            Helper::Cipher(cipher, cipher_type) => {
                let Key::Aes(AesKey { secret }) = key else {
                    panic!("Wrong key type ({:?}) - expected AES", key.get_type());
                };

                match cipher_type {
                    CipherType::AesCbcNopad => {
                        let cbc: Box<dyn Cipher> = match secret.len() {
                            16 => Box::new(Aes128CbcNopad::new(&secret)),
                            24 => Box::new(Aes192CbcNopad::new(&secret)),
                            32 => Box::new(Aes256CbcNopad::new(&secret)),
                            len => panic!("Invalid AES key length: {len}"),
                        };
                        *cipher = Some(cbc);
                    }
                    CipherType::AesEcbNopad => {
                        let ecb: Box<dyn Cipher> = match secret.len() {
                            16 => Box::new(Aes128EcbNopad::new(&secret)),
                            24 => Box::new(Aes192EcbNopad::new(&secret)),
                            32 => Box::new(Aes256EcbNopad::new(&secret)),
                            len => panic!("Invalid AES key length: {len}"),
                        };
                        *cipher = Some(ecb);
                    }
                }
            }
            Helper::Mac(mac, mac_type) => match mac_type {
                MacType::AesCmac => {
                    let Key::Aes(AesKey { secret }) = key else {
                        panic!("Wrong key type ({:?}) - expected AES", key.get_type());
                    };
                    let cmac: Box<dyn Mac> = match secret.len() {
                        16 => Box::new(AesCmac128::new_from_slice(&secret).unwrap()),
                        24 => Box::new(AesCmac192::new_from_slice(&secret).unwrap()),
                        32 => Box::new(AesCmac256::new_from_slice(&secret).unwrap()),
                        len => panic!("Invalid AES key length: {len}"),
                    };
                    *mac = Some(cmac);
                }
                MacType::HmacSha1 => {
                    let Key::HmacSha1(HmacSha1Key { secret }) = key else {
                        panic!("Wrong key type ({:?}) - expected HMAC SHA1", key.get_type());
                    };
                    *mac = Some(Box::new(HmacSha1::new_from_slice(&secret).unwrap()))
                }
                MacType::HmacSha224 => {
                    let Key::HmacSha224(HmacSha224Key { secret }) = key else {
                        panic!("Wrong key type ({:?}) - expected HMAC SHA224", key.get_type());
                    };
                    *mac = Some(Box::new(HmacSha224::new_from_slice(&secret).unwrap()))
                }
                MacType::HmacSha256 => {
                    let Key::HmacSha256(HmacSha256Key { secret }) = key else {
                        panic!("Wrong key type ({:?}) - expected HMAC SHA256", key.get_type());
                    };
                    *mac = Some(Box::new(HmacSha256::new_from_slice(&secret).unwrap()))
                }
                MacType::HmacSha384 => {
                    let Key::HmacSha384(HmacSha384Key { secret }) = key else {
                        panic!("Wrong key type ({:?}) - expected HMAC SHA384", key.get_type());
                    };
                    *mac = Some(Box::new(HmacSha384::new_from_slice(&secret).unwrap()))
                }
                MacType::HmacSha512 => {
                    let Key::HmacSha512(HmacSha512Key { secret }) = key else {
                        panic!("Wrong key type ({:?}) - expected HMAC SHA512", key.get_type());
                    };
                    *mac = Some(Box::new(HmacSha512::new_from_slice(&secret).unwrap()))
                }
            },
        }
    }

    fn reset(&mut self) {
        match self {
            Helper::Digest(digest) => digest.reset(),
            Helper::Cipher(cipher, _) => {
                if let Some(cipher) = cipher {
                    cipher.reset()
                }
            }
            Helper::Mac(mac, _) => {
                if let Some(mac) = mac {
                    mac.reset()
                }
            }
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
        if let Helper::Digest(ref mut digest) = &mut self.helper {
            digest
        } else {
            panic!("{:?} is not a digest algorithm", self.algorithm)
        }
    }

    fn as_cipher(&mut self) -> &mut Box<dyn Cipher> {
        if let Helper::Cipher(ref mut cipher, _) = &mut self.helper {
            cipher.as_mut().expect("TEE_SetKey() has not yet been called")
        } else {
            panic!("{:?} is not a cipher algorithm", self.algorithm)
        }
    }

    fn as_mac(&mut self) -> &mut Box<dyn Mac> {
        if let Helper::Mac(ref mut mac, _) = &mut self.helper {
            mac.as_mut().expect("TEE_SetKey() has not yet been called")
        } else {
            panic!("{:?} is not a MAC algorithm", self.algorithm)
        }
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
            Algorithm::AesCmac
            | Algorithm::HmacSha1
            | Algorithm::HmacSha224
            | Algorithm::HmacSha256
            | Algorithm::HmacSha384
            | Algorithm::HmacSha512 => {}
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

    // See TEE_CipherInit()
    fn init_cipher(&mut self, iv: &[u8]) {
        if self.state == OpState::Active {
            self.as_cipher().reset();
        } else {
            assert_eq!(self.state, OpState::Initial);
        }

        self.as_cipher().set_iv(iv);

        // Currently supported MAC algorithms don't deal in initialization vectors.
        self.state = OpState::Active;
    }

    // The error value indicates the minimum required size of the output buffer
    // (i.e., the total number of full blocks to encrypt/decrypt).
    fn update_cipher(&mut self, src: &[u8], dest: &mut [u8]) -> Result<(), usize> {
        assert_eq!(self.state, OpState::Active);

        let block_size = self.as_cipher().block_size();
        let num_blocks_in = src.len() / block_size;
        let num_blocks_out = dest.len() / block_size;

        // The output buffer size should be at least the total size of the
        // number of full blocks in `src` to encrypt/decrypt.
        if num_blocks_in > num_blocks_out {
            return Err(num_blocks_in * block_size);
        }

        if self.mode == Mode::Encrypt {
            self.as_cipher().encrypt(src, dest);
        } else {
            assert_eq!(self.mode, Mode::Decrypt);
            self.as_cipher().decrypt(src, dest);
        }
        Ok(())
    }

    fn update_cipher_in_place(&mut self, inout: &mut [u8]) {
        assert_eq!(self.state, OpState::Active);

        if self.mode == Mode::Encrypt {
            self.as_cipher().encrypt_in_place(inout);
        } else {
            assert_eq!(self.mode, Mode::Decrypt);
            self.as_cipher().decrypt_in_place(inout);
        }
    }

    // The error value indicates the minimum required size of the output buffer
    // (i.e., the total number of full blocks to encrypt/decrypt, which should
    // be the same size as `src` itself).
    fn finalize_cipher(&mut self, src: &[u8], dest: &mut [u8]) -> Result<(), usize> {
        let block_size = self.as_cipher().block_size();
        assert_eq!(src.len() % block_size, 0);
        assert!(dest.len() >= src.len());
        self.update_cipher(src, dest)?;
        self.state = OpState::Initial;
        Ok(())
    }

    fn finalize_cipher_in_place(&mut self, inout: &mut [u8]) {
        let block_size = self.as_cipher().block_size();
        assert_eq!(inout.len() % block_size, 0);
        self.update_cipher_in_place(inout);
        self.state = OpState::Initial;
    }

    // See TEE_MACInit().
    fn init_mac(&mut self, _iv: &[u8]) {
        assert_eq!(self.mode, Mode::Mac);
        assert!(self.state == OpState::Initial || self.state == OpState::Active);

        if self.state == OpState::Active {
            self.as_mac().reset();
        }

        // Currently supported MAC algorithms don't deal in initialization
        // vectors; the spec say to ignore the provided one in that case.

        self.state = OpState::Active;
    }

    // See TEE_MACUpdate().
    fn update_mac(&mut self, chunk: &[u8]) {
        assert_eq!(self.mode, Mode::Mac);
        assert_eq!(self.state, OpState::Active);

        let mac = self.as_mac();
        if !chunk.is_empty() {
            mac.update(chunk);
        }
    }

    // See TEE_MACComputeFinal().
    fn compute_final_mac(&mut self, message: &[u8], output: &mut [u8]) -> Result<(), usize> {
        assert_eq!(self.mode, Mode::Mac);
        assert_eq!(self.state, OpState::Active);

        let output_size = self.as_mac().output_size();
        if output.len() < output_size {
            return Err(output_size);
        }

        // Make sure we validate the output buffer size before updating the
        // digest.
        let mac = self.as_mac();
        if !message.is_empty() {
            mac.update(message);
        }
        mac.finalize_into_reset(&mut output[..output_size]);
        self.state = OpState::Initial;
        Ok(())
    }

    // See TEE_MACCompareFinal().
    fn compare_final_mac(&mut self, message: &[u8], expected: &[u8]) -> TeeResult {
        self.update_mac(message);
        let result = self.as_mac().verify_reset(expected);
        self.state = OpState::Initial;
        result
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
            Algorithm::AesCmac => {
                if mode != Mode::Mac {
                    return Err(Error::NotSupported);
                }
                AesKey::is_valid_size
            }
            Algorithm::HmacSha1 => {
                if mode != Mode::Mac {
                    return Err(Error::NotSupported);
                }
                HmacSha1Key::is_valid_size
            }
            Algorithm::HmacSha224 => {
                if mode != Mode::Mac {
                    return Err(Error::NotSupported);
                }
                HmacSha224Key::is_valid_size
            }
            Algorithm::HmacSha256 => {
                if mode != Mode::Mac {
                    return Err(Error::NotSupported);
                }
                HmacSha256Key::is_valid_size
            }
            Algorithm::HmacSha384 => {
                if mode != Mode::Mac {
                    return Err(Error::NotSupported);
                }
                HmacSha384Key::is_valid_size
            }
            Algorithm::HmacSha512 => {
                if mode != Mode::Mac {
                    return Err(Error::NotSupported);
                }
                HmacSha512Key::is_valid_size
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

    pub fn init_cipher(&mut self, operation: OperationHandle, iv: &[u8]) {
        self.get_mut(operation).init_cipher(iv)
    }

    pub fn update_cipher(
        &mut self,
        operation: OperationHandle,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), usize> {
        self.get_mut(operation).update_cipher(input, output)
    }

    pub fn update_cipher_in_place(&mut self, operation: OperationHandle, inout: &mut [u8]) {
        self.get_mut(operation).update_cipher_in_place(inout)
    }

    pub fn finalize_cipher(
        &mut self,
        operation: OperationHandle,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), usize> {
        self.get_mut(operation).finalize_cipher(input, output)
    }

    pub fn finalize_cipher_in_place(&mut self, operation: OperationHandle, inout: &mut [u8]) {
        self.get_mut(operation).finalize_cipher_in_place(inout)
    }

    pub fn init_mac(&mut self, operation: OperationHandle, iv: &[u8]) {
        self.get_mut(operation).init_mac(iv)
    }

    pub fn update_mac(&mut self, operation: OperationHandle, chunk: &[u8]) {
        self.get_mut(operation).update_mac(chunk)
    }

    pub fn compute_final_mac(
        &mut self,
        operation: OperationHandle,
        message: &[u8],
        mac: &mut [u8],
    ) -> Result<(), usize> {
        self.get_mut(operation).compute_final_mac(message, mac)
    }

    pub fn compare_final_mac(
        &mut self,
        operation: OperationHandle,
        message: &[u8],
        mac: &[u8],
    ) -> TeeResult {
        self.get_mut(operation).compare_final_mac(message, mac)
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
