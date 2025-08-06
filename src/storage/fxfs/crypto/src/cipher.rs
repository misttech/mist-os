// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{UnwrappedKey, WrappedKey};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::inout::InOut;
use aes::cipher::typenum::consts::U16;
use aes::cipher::{BlockBackend, BlockClosure, BlockSizeUser};
use anyhow::Error;
use static_assertions::assert_cfg;
use std::collections::BTreeMap;
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
use zx_status as zx;

mod fscrypt_ino_lblk32;
pub(crate) mod fxfs;

// TODO(https://fxbug.dev/375700939): Support different padding sizes based on SET_ENCRYPTION_POLICY
// flags.
// Note: This constant is used in platform code. It would be nice to move all fscrypt
// internals into fxfs_lib and keep platform as simple as possible.
pub const FSCRYPT_PADDING: usize = 16;
// Fxfs will always use a block size >= 512 bytes, so we just assume a sector size of 512 bytes,
// which will work fine even if a different block size is used by Fxfs or the underlying device.
const SECTOR_SIZE: u64 = 512;

/// Trait defining common methods shared across all ciphers.
pub trait Cipher: std::fmt::Debug + Send + Sync {
    /// Encrypts data in the `buffer`.
    ///
    /// * `offset` is the byte offset within the file.
    /// * `buffer` is mutated in place.
    ///
    /// `buffer` *must* be 16 byte aligned.
    fn encrypt(
        &self,
        ino: u64,
        device_offset: u64,
        file_offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), Error>;

    /// Decrypt the data in `buffer`.
    ///
    /// * `offset` is the byte offset within the file.
    /// * `buffer` is mutated in place.
    ///
    /// `buffer` *must* be 16 byte aligned.
    fn decrypt(
        &self,
        ino: u64,
        device_offset: u64,
        file_offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), Error>;

    /// Encrypts the filename contained in `buffer`.
    fn encrypt_filename(&self, object_id: u64, buffer: &mut Vec<u8>) -> Result<(), Error>;

    /// Decrypts the filename contained in `buffer`.
    fn decrypt_filename(&self, object_id: u64, buffer: &mut Vec<u8>) -> Result<(), Error>;

    /// Returns a hash_code to use.
    /// Note in the case of encrypted filenames, takes the raw encrypted bytes.
    fn hash_code(&self, _raw_filename: &[u8], filename: &str) -> u32;

    /// Returns a case-folded hash_code to use for 'filename'.
    fn hash_code_casefold(&self, _filename: &str) -> u32;
}

/// Helper function to obtain a Cipher for a key.
/// Uses key to interpret the meaning of the UnwrappedKey blob and then creates a
/// cipher instance from the blob, returning it.
#[inline]
pub(crate) fn key_to_cipher(
    key: &WrappedKey,
    unwrapped_key: &UnwrappedKey,
) -> Result<Option<Arc<dyn Cipher>>, zx::Status> {
    match key {
        WrappedKey::Fxfs(_) => Ok(Some(Arc::new(fxfs::FxfsCipher::new(&unwrapped_key)))),
        WrappedKey::FscryptInoLblk32Dir { .. } => {
            Ok(Some(Arc::new(fscrypt_ino_lblk32::FscryptInoLblk32DirCipher::new(&unwrapped_key))))
        }
        WrappedKey::FscryptInoLblk32File { .. } => {
            Ok(Some(Arc::new(fscrypt_ino_lblk32::FscryptInoLblk32FileCipher::new(&unwrapped_key))))
        }
        _ => Err(zx::Status::NOT_SUPPORTED),
    }
}

/// A container that holds ciphers related to a specific object.
#[derive(Clone, Debug, Default)]
pub struct CipherSet(BTreeMap<u64, Option<Arc<dyn Cipher>>>);
impl CipherSet {
    pub fn find_key(self: &Arc<Self>, id: u64) -> FindKeyResult {
        if let Some(cipher) = self.0.get(&id) {
            if let Some(cipher) = cipher {
                FindKeyResult::Key(Arc::clone(cipher))
            } else {
                FindKeyResult::Unavailable
            }
        } else {
            FindKeyResult::NotFound
        }
    }

    pub fn add_key(&mut self, id: u64, cipher: Option<Arc<dyn Cipher>>) {
        self.0.insert(id, cipher);
    }
}
impl From<Vec<(u64, Option<Arc<dyn Cipher>>)>> for CipherSet {
    fn from(keys: Vec<(u64, Option<Arc<dyn Cipher>>)>) -> Self {
        Self(keys.into_iter().collect())
    }
}
impl From<BTreeMap<u64, Option<Arc<dyn Cipher>>>> for CipherSet {
    fn from(keys: BTreeMap<u64, Option<Arc<dyn Cipher>>>) -> Self {
        Self(keys)
    }
}

pub enum FindKeyResult {
    /// No key registered with that key_id.
    NotFound,
    /// The key is known, but not available for use (cannot be unwrapped).
    Unavailable,
    Key(Arc<dyn Cipher>),
}

// This assumes little-endianness which is likely to always be the case.
assert_cfg!(target_endian = "little");
#[derive(IntoBytes, KnownLayout, FromBytes, Immutable)]
#[repr(C)]
struct Tweak(u128);

pub fn xor_in_place(a: &mut [u8], b: &[u8]) {
    for (b1, b2) in a.iter_mut().zip(b.iter()) {
        *b1 ^= *b2;
    }
}

// To be used with encrypt_with_backend.
struct CbcEncryptProcessor<'a> {
    tweak: Tweak,
    data: &'a mut [u8],
}

impl<'a> CbcEncryptProcessor<'a> {
    fn new(tweak: Tweak, data: &'a mut [u8]) -> Self {
        Self { tweak, data }
    }
}

impl BlockSizeUser for CbcEncryptProcessor<'_> {
    type BlockSize = U16;
}

impl BlockClosure for CbcEncryptProcessor<'_> {
    fn call<B: BlockBackend<BlockSize = Self::BlockSize>>(self, backend: &mut B) {
        let Self { mut tweak, data } = self;
        for block in data.chunks_exact_mut(16) {
            xor_in_place(block, &tweak.0.to_le_bytes());
            let chunk: &mut GenericArray<u8, _> = GenericArray::from_mut_slice(block);
            backend.proc_block(InOut::from(chunk));
            tweak.0 = u128::from_le_bytes(block.try_into().unwrap())
        }
    }
}

// To be used with decrypt_with_backend.
struct CbcDecryptProcessor<'a> {
    tweak: Tweak,
    data: &'a mut [u8],
}

impl<'a> CbcDecryptProcessor<'a> {
    fn new(tweak: Tweak, data: &'a mut [u8]) -> Self {
        Self { tweak, data }
    }
}

impl BlockSizeUser for CbcDecryptProcessor<'_> {
    type BlockSize = U16;
}

impl BlockClosure for CbcDecryptProcessor<'_> {
    fn call<B: BlockBackend<BlockSize = Self::BlockSize>>(self, backend: &mut B) {
        let Self { mut tweak, data } = self;
        for block in data.chunks_exact_mut(16) {
            let ciphertext = block.to_vec();
            let chunk = GenericArray::from_mut_slice(block);
            backend.proc_block(InOut::from(chunk));
            xor_in_place(block, &tweak.0.to_le_bytes());
            tweak.0 = u128::from_le_bytes(ciphertext.try_into().unwrap());
        }
    }
}

// To be used with encrypt|decrypt_with_backend.
struct XtsProcessor<'a> {
    tweak: Tweak,
    data: &'a mut [u8],
}

impl<'a> XtsProcessor<'a> {
    // `tweak` should be encrypted.  `data` should be a single sector and *must* be 16 byte aligned.
    fn new(tweak: Tweak, data: &'a mut [u8]) -> Self {
        assert_eq!(data.as_ptr() as usize & 15, 0, "data must be 16 byte aligned");
        Self { tweak, data }
    }
}

impl BlockSizeUser for XtsProcessor<'_> {
    type BlockSize = U16;
}

impl BlockClosure for XtsProcessor<'_> {
    fn call<B: BlockBackend<BlockSize = Self::BlockSize>>(self, backend: &mut B) {
        let Self { mut tweak, data } = self;
        for chunk in data.chunks_exact_mut(16) {
            let ptr = chunk.as_mut_ptr() as *mut u128;
            // SAFETY: We know each chunk is exactly 16 bytes and it should be safe to transmute to
            // u128 and GenericArray<u8, U16>.  There are safe ways of doing the following, but this
            // is extremely performance sensitive, and even seemingly innocuous changes here can
            // have an order-of-magnitude impact on what the compiler produces and that can be seen
            // in our benchmarks.  This assumes little-endianness which is likely to always be the
            // case.
            unsafe {
                *ptr ^= tweak.0;
                let chunk = ptr as *mut GenericArray<u8, U16>;
                backend.proc_block(InOut::from_raw(chunk, chunk));
                *ptr ^= tweak.0;
            }
            tweak.0 = (tweak.0 << 1) ^ ((tweak.0 as i128 >> 127) as u128 & 0x87);
        }
    }
}
