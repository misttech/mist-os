// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::inout::InOut;
use aes::cipher::typenum::consts::U16;
use aes::cipher::{
    BlockBackend, BlockClosure, BlockDecrypt, BlockEncrypt, BlockSizeUser, KeyInit, KeyIvInit,
    StreamCipher as _, StreamCipherSeek,
};
use aes::Aes256;
use anyhow::{anyhow, Error};
use async_trait::async_trait;
use chacha20::{self, ChaCha20};
use fprint::TypeFingerprint;
use futures::stream::FuturesUnordered;
use futures::TryStreamExt as _;
use fxfs_macros::{migrate_nodefault, Migrate};
use serde::de::{Error as SerdeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use static_assertions::assert_cfg;
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
use zx_status as zx;

pub mod ff1;

pub const KEY_SIZE: usize = 256 / 8;
pub const WRAPPED_KEY_SIZE: usize = KEY_SIZE + 16;
// TODO(https://fxbug.dev/375700939): Support different padding sizes based on SET_ENCRYPTION_POLICY
// flags.
pub const FSCRYPT_PADDING: usize = 16;

// Fxfs will always use a block size >= 512 bytes, so we just assume a sector size of 512 bytes,
// which will work fine even if a different block size is used by Fxfs or the underlying device.
const SECTOR_SIZE: u64 = 512;

pub type KeyBytes = [u8; KEY_SIZE];

#[derive(Debug)]
pub struct UnwrappedKey {
    key: KeyBytes,
}

impl UnwrappedKey {
    pub fn new(key: KeyBytes) -> Self {
        UnwrappedKey { key }
    }

    pub fn key(&self) -> &KeyBytes {
        &self.key
    }
}

pub type UnwrappedKeys = Vec<(u64, Option<UnwrappedKey>)>;

pub type WrappedKeyBytes = WrappedKeyBytesV32;

#[repr(transparent)]
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedKeyBytesV32(pub [u8; WRAPPED_KEY_SIZE]);

impl Default for WrappedKeyBytes {
    fn default() -> Self {
        Self([0u8; WRAPPED_KEY_SIZE])
    }
}

impl TryFrom<Vec<u8>> for WrappedKeyBytes {
    type Error = anyhow::Error;

    fn try_from(buf: Vec<u8>) -> Result<Self, Self::Error> {
        Ok(Self(buf.try_into().map_err(|_| anyhow!("wrapped key wrong length"))?))
    }
}

impl From<[u8; WRAPPED_KEY_SIZE]> for WrappedKeyBytes {
    fn from(buf: [u8; WRAPPED_KEY_SIZE]) -> Self {
        Self(buf)
    }
}

impl TypeFingerprint for WrappedKeyBytes {
    fn fingerprint() -> String {
        "WrappedKeyBytes".to_owned()
    }
}

impl std::ops::Deref for WrappedKeyBytes {
    type Target = [u8; WRAPPED_KEY_SIZE];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for WrappedKeyBytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// Because default impls of Serialize/Deserialize for [T; N] are only defined for N in 0..=32, we
// have to define them ourselves.
impl Serialize for WrappedKeyBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self[..])
    }
}

impl<'de> Deserialize<'de> for WrappedKeyBytes {
    fn deserialize<D>(deserializer: D) -> Result<WrappedKeyBytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct WrappedKeyVisitor;

        impl<'d> Visitor<'d> for WrappedKeyVisitor {
            type Value = WrappedKeyBytes;

            fn expecting(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_str("Expected wrapped keys to be 48 bytes")
            }

            fn visit_bytes<E>(self, bytes: &[u8]) -> Result<WrappedKeyBytes, E>
            where
                E: SerdeError,
            {
                self.visit_byte_buf(bytes.to_vec())
            }

            fn visit_byte_buf<E>(self, bytes: Vec<u8>) -> Result<WrappedKeyBytes, E>
            where
                E: SerdeError,
            {
                let orig_len = bytes.len();
                let bytes: [u8; WRAPPED_KEY_SIZE] =
                    bytes.try_into().map_err(|_| SerdeError::invalid_length(orig_len, &self))?;
                Ok(WrappedKeyBytes::from(bytes))
            }
        }
        deserializer.deserialize_byte_buf(WrappedKeyVisitor)
    }
}

pub type WrappedKey = WrappedKeyV40;

#[derive(Clone, Debug, Serialize, Deserialize, TypeFingerprint, PartialEq)]
pub struct WrappedKeyV40 {
    /// The identifier of the wrapping key.  The identifier has meaning to whatever is doing the
    /// unwrapping.
    pub wrapping_key_id: u128,
    /// AES 256 requires a 512 bit key, which is made of two 256 bit keys, one for the data and one
    /// for the tweak.  It is safe to use the same 256 bit key for both (see
    /// https://csrc.nist.gov/CSRC/media/Projects/Block-Cipher-Techniques/documents/BCM/Comments/XTS/follow-up_XTS_comments-Ball.pdf)
    /// which is what we do here.  Since the key is wrapped with AES-GCM-SIV, there are an
    /// additional 16 bytes paid per key (so the actual key material is 32 bytes once unwrapped).
    pub key: WrappedKeyBytesV32,
}

#[derive(Default, Clone, Migrate, Debug, Serialize, Deserialize, TypeFingerprint)]
#[migrate_nodefault]
pub struct WrappedKeyV32 {
    pub wrapping_key_id: u64,
    pub key: WrappedKeyBytesV32,
}

/// To support key rolling and clones, a file can have more than one key.  Each key has an ID that
/// unique to the file.
pub type WrappedKeys = WrappedKeysV40;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub struct WrappedKeysV40(Vec<(u64, WrappedKeyV40)>);

impl From<WrappedKeysV32> for WrappedKeysV40 {
    fn from(value: WrappedKeysV32) -> Self {
        Self(value.0.into_iter().map(|(id, key)| (id, key.into())).collect())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, TypeFingerprint)]
pub struct WrappedKeysV32(pub Vec<(u64, WrappedKeyV32)>);

impl From<Vec<(u64, WrappedKey)>> for WrappedKeys {
    fn from(buf: Vec<(u64, WrappedKey)>) -> Self {
        Self(buf)
    }
}

impl std::ops::Deref for WrappedKeys {
    type Target = Vec<(u64, WrappedKey)>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for WrappedKeys {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl WrappedKeys {
    pub fn get_wrapping_key_with_id(&self, key_id: u64) -> Option<[u8; 16]> {
        let wrapped_key_entry = self.0.iter().find(|(x, _)| *x == key_id);
        wrapped_key_entry.map(|(_, wrapped_key)| wrapped_key.wrapping_key_id.to_le_bytes())
    }
}

#[derive(Clone, Debug)]
pub struct XtsCipher {
    id: u64,
    // This is None if the key isn't present.
    cipher: Option<Aes256>,
}

impl XtsCipher {
    pub fn new(id: u64, key: &UnwrappedKey) -> Self {
        Self { id, cipher: Some(Aes256::new(GenericArray::from_slice(key.key()))) }
    }

    pub fn unavailable(id: u64) -> Self {
        XtsCipher { id, cipher: None }
    }

    pub fn key(&self) -> Option<&Aes256> {
        self.cipher.as_ref()
    }
}

/// References a specific key in the cipher set.
pub struct Key {
    keys: Arc<XtsCipherSet>,
    // Index in the XtsCipherSet array for the key.
    index: usize,
}

impl Key {
    fn key(&self) -> &Aes256 {
        self.keys.0[self.index].cipher.as_ref().unwrap()
    }

    pub fn key_id(&self) -> u64 {
        self.keys.0[self.index].id
    }

    /// Encrypts data in the `buffer`.
    ///
    /// * `offset` is the byte offset within the file.
    /// * `buffer` is mutated in place.
    ///
    /// `buffer` *must* be 16 byte aligned.
    pub fn encrypt(&self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        fxfs_trace::duration!(c"encrypt", "len" => buffer.len());
        assert_eq!(offset % SECTOR_SIZE, 0);
        let cipher = &self.key();
        let mut sector_offset = offset / SECTOR_SIZE;
        for sector in buffer.chunks_exact_mut(SECTOR_SIZE as usize) {
            let mut tweak = Tweak(sector_offset as u128);
            // The same key is used for encrypting the data and computing the tweak.
            cipher.encrypt_block(GenericArray::from_mut_slice(tweak.as_mut_bytes()));
            cipher.encrypt_with_backend(XtsProcessor::new(tweak, sector));
            sector_offset += 1;
        }
        Ok(())
    }

    /// Decrypt the data in `buffer`.
    ///
    /// * `offset` is the byte offset within the file.
    /// * `buffer` is mutated in place.
    ///
    /// `buffer` *must* be 16 byte aligned.
    pub fn decrypt(&self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        fxfs_trace::duration!(c"decrypt", "len" => buffer.len());
        assert_eq!(offset % SECTOR_SIZE, 0);
        let cipher = &self.key();
        let mut sector_offset = offset / SECTOR_SIZE;
        for sector in buffer.chunks_exact_mut(SECTOR_SIZE as usize) {
            let mut tweak = Tweak(sector_offset as u128);
            // The same key is used for encrypting the data and computing the tweak.
            cipher.encrypt_block(GenericArray::from_mut_slice(tweak.as_mut_bytes()));
            cipher.decrypt_with_backend(XtsProcessor::new(tweak, sector));
            sector_offset += 1;
        }
        Ok(())
    }

    /// Encrypts the filename contained in `buffer`.
    pub fn encrypt_filename(&self, object_id: u64, buffer: &mut Vec<u8>) -> Result<(), Error> {
        // Pad the buffer such that its length is a multiple of FSCRYPT_PADDING.
        buffer.resize(buffer.len().next_multiple_of(FSCRYPT_PADDING), 0);
        let cipher = self.key();
        cipher.encrypt_with_backend(CbcEncryptProcessor::new(Tweak(object_id as u128), buffer));
        Ok(())
    }

    /// Decrypts the filename contained in `buffer`.
    pub fn decrypt_filename(&self, object_id: u64, buffer: &mut Vec<u8>) -> Result<(), Error> {
        let cipher = self.key();
        cipher.decrypt_with_backend(CbcDecryptProcessor::new(Tweak(object_id as u128), buffer));
        // Remove the padding
        if let Some(i) = buffer.iter().rposition(|x| *x != 0) {
            let new_len = i + 1;
            buffer.truncate(new_len);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct XtsCipherSet(Vec<XtsCipher>);

impl From<Vec<XtsCipher>> for XtsCipherSet {
    fn from(value: Vec<XtsCipher>) -> Self {
        Self(value)
    }
}

impl XtsCipherSet {
    pub fn new(keys: &UnwrappedKeys) -> Self {
        Self(
            keys.iter()
                .map(|(id, k)| match k {
                    Some(k) => XtsCipher::new(*id, k),
                    None => XtsCipher::unavailable(*id),
                })
                .collect(),
        )
    }

    pub fn ciphers(&self) -> &[XtsCipher] {
        &self.0
    }

    pub fn cipher(&self, id: u64) -> Option<(usize, &XtsCipher)> {
        self.0.iter().enumerate().find(|(_, x)| x.id == id)
    }

    pub fn contains_key_id(&self, id: u64) -> bool {
        self.0.iter().find(|x| x.id == id).is_some()
    }

    pub fn find_key(self: &Arc<Self>, id: u64) -> FindKeyResult {
        let Some((index, cipher)) = self.0.iter().enumerate().find(|(_, x)| x.id == id) else {
            return FindKeyResult::NotFound;
        };
        if cipher.key().is_some() {
            FindKeyResult::Key(Key { keys: self.clone(), index })
        } else {
            FindKeyResult::Unavailable
        }
    }
}

pub enum FindKeyResult {
    NotFound,
    Unavailable,
    Key(Key),
}

/// A thin wrapper around a ChaCha20 stream cipher.  This will use a zero nonce. **NOTE**: Great
/// care must be taken not to encrypt different plaintext with the same key and offset (even across
/// multiple boots), so consider if this suits your purpose before using it.
pub struct StreamCipher(ChaCha20);

impl StreamCipher {
    pub fn new(key: &UnwrappedKey, offset: u64) -> Self {
        let mut cipher = Self(ChaCha20::new(
            chacha20::Key::from_slice(&key.key),
            /* nonce: */ &[0; 12].into(),
        ));
        cipher.0.seek(offset);
        cipher
    }

    pub fn encrypt(&mut self, buffer: &mut [u8]) {
        fxfs_trace::duration!(c"StreamCipher::encrypt", "len" => buffer.len());
        self.0.apply_keystream(buffer);
    }

    pub fn decrypt(&mut self, buffer: &mut [u8]) {
        fxfs_trace::duration!(c"StreamCipher::decrypt", "len" => buffer.len());
        self.0.apply_keystream(buffer);
    }

    pub fn offset(&self) -> u64 {
        self.0.current_pos()
    }
}

/// Different keys are used for metadata and data in order to make certain operations requiring a
/// metadata key rotation (e.g. secure erase) more efficient.
pub enum KeyPurpose {
    /// The key will be used to wrap user data.
    Data,
    /// The key will be used to wrap internal metadata.
    Metadata,
}

/// An interface trait with the ability to wrap and unwrap encryption keys.
///
/// Note that existence of this trait does not imply that an object will **securely**
/// wrap and unwrap keys; rather just that it presents an interface for wrapping operations.
#[async_trait]
pub trait Crypt: Send + Sync {
    /// `owner` is intended to be used such that when the key is wrapped, it appears to be different
    /// to that of the same key wrapped by a different owner.  In this way, keys can be shared
    /// amongst different filesystem objects (e.g. for clones), but it is not possible to tell just
    /// by looking at the wrapped keys.
    async fn create_key(
        &self,
        owner: u64,
        purpose: KeyPurpose,
    ) -> Result<(WrappedKey, UnwrappedKey), zx::Status>;

    /// `owner` is intended to be used such that when the key is wrapped, it appears to be different
    /// to that of the same key wrapped by a different owner.  In this way, keys can be shared
    /// amongst different filesystem objects (e.g. for clones), but it is not possible to tell just
    /// by looking at the wrapped keys.
    async fn create_key_with_id(
        &self,
        owner: u64,
        wrapping_key_id: u128,
    ) -> Result<(WrappedKey, UnwrappedKey), zx::Status>;

    // Unwraps a single key.
    async fn unwrap_key(
        &self,
        wrapped_key: &WrappedKey,
        owner: u64,
    ) -> Result<UnwrappedKey, zx::Status>;

    /// Unwraps the keys and stores the result in UnwrappedKeys.
    async fn unwrap_keys(
        &self,
        keys: &WrappedKeys,
        owner: u64,
    ) -> Result<UnwrappedKeys, zx::Status> {
        let futures = FuturesUnordered::new();
        for (key_id, key) in keys.iter() {
            futures.push(async move {
                match self.unwrap_key(key, owner).await {
                    Ok(unwrapped_key) => Ok((*key_id, Some(unwrapped_key))),
                    Err(zx::Status::NOT_FOUND) => Ok((*key_id, None)),
                    Err(e) => Err(e),
                }
            });
        }
        Ok(futures.try_collect::<UnwrappedKeys>().await?)
    }
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

#[cfg(test)]
mod tests {
    use crate::{Key, XtsCipher, XtsCipherSet};

    use super::{StreamCipher, UnwrappedKey};

    #[test]
    fn test_stream_cipher_offset() {
        let key = UnwrappedKey::new([
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32,
        ]);
        let mut cipher1 = StreamCipher::new(&key, 0);
        let mut p1 = [1, 2, 3, 4];
        let mut c1 = p1.clone();
        cipher1.encrypt(&mut c1);

        let mut cipher2 = StreamCipher::new(&key, 1);
        let p2 = [5, 6, 7, 8];
        let mut c2 = p2.clone();
        cipher2.encrypt(&mut c2);

        let xor_fn = |buf1: &mut [u8], buf2| {
            for (b1, b2) in buf1.iter_mut().zip(buf2) {
                *b1 ^= b2;
            }
        };

        // Check that c1 ^ c2 != p1 ^ p2 (which would be the case if the same offset was used for
        // both ciphers).
        xor_fn(&mut c1, &c2);
        xor_fn(&mut p1, &p2);
        assert_ne!(c1, p1);
    }

    /// Output produced via:
    /// echo -n filename > in.txt ; truncate -s 16 in.txt
    /// openssl aes-256-cbc -e -iv 02000000000000000000000000000000 -nosalt -K 1fcdf30b7d191bd95d3161fe08513b864aa15f27f910f1c66eec8cfa93e9893b -in in.txt -out out.txt -nopad
    /// hexdump out.txt -e "16/1 \"%02x\" \"\n\"" -v
    #[test]
    fn test_encrypt_filename() {
        let raw_key_hex = "1fcdf30b7d191bd95d3161fe08513b864aa15f27f910f1c66eec8cfa93e9893b";
        let raw_key_bytes: [u8; 32] =
            hex::decode(raw_key_hex).expect("decode failed").try_into().unwrap();
        let unwrapped_key = UnwrappedKey::new(raw_key_bytes);
        let cipher_set = XtsCipherSet::from(vec![XtsCipher::new(0, &unwrapped_key)]);
        let key = Key { keys: std::sync::Arc::new(cipher_set), index: 0 };
        let object_id = 2;
        let mut text = "filename".to_string().as_bytes().to_vec();
        key.encrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(text, hex::decode("52d56369103a39b3ea1e09c85dd51546").expect("decode failed"));
    }

    /// Output produced via:
    /// openssl aes-256-cbc -d -iv 02000000000000000000000000000000 -nosalt -K 1fcdf30b7d191bd95d3161fe08513b864aa15f27f910f1c66eec8cfa93e9893b -in out.txt -out in.txt
    /// cat in.txt
    #[test]
    fn test_decrypt_filename() {
        let raw_key_hex = "1fcdf30b7d191bd95d3161fe08513b864aa15f27f910f1c66eec8cfa93e9893b";
        let raw_key_bytes: [u8; 32] =
            hex::decode(raw_key_hex).expect("decode failed").try_into().unwrap();
        let unwrapped_key = UnwrappedKey::new(raw_key_bytes);
        let cipher_set = XtsCipherSet::from(vec![XtsCipher::new(0, &unwrapped_key)]);
        let key = Key { keys: std::sync::Arc::new(cipher_set), index: 0 };
        let object_id = 2;
        let mut text = hex::decode("52d56369103a39b3ea1e09c85dd51546").expect("decode failed");
        key.decrypt_filename(object_id, &mut text).expect("encrypt filename failed");
        assert_eq!(text, "filename".to_string().as_bytes().to_vec());
    }
}
