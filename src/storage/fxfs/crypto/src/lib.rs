// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use aes::cipher::{KeyIvInit, StreamCipher as _, StreamCipherSeek};
use anyhow::anyhow;
use arbitrary::Arbitrary;
use async_trait::async_trait;
use chacha20::{self, ChaCha20};
use fprint::TypeFingerprint;
use futures::stream::FuturesUnordered;
use futures::TryStreamExt as _;
use fxfs_macros::{migrate_nodefault, Migrate};
use serde::de::{Error as SerdeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use zx_status as zx;

mod cipher;
pub mod ff1;

pub use cipher::fxfs::FxfsCipher;
pub use cipher::{Cipher, CipherSet, FindKeyResult};
pub use fidl_fuchsia_fxfs::{
    EmptyStruct, FscryptKeyIdentifier, FscryptKeyIdentifierAndNonce, WrappedKey,
};

pub use cipher::FSCRYPT_PADDING;
pub const FXFS_KEY_SIZE: usize = 256 / 8;
pub const FXFS_WRAPPED_KEY_SIZE: usize = FXFS_KEY_SIZE + 16;

/// Essentially just a vector by another name to indicate that it holds unwrapped key material.
/// The length of an unwrapped key depends on the type of key that is wrapped.
#[derive(Debug)]
pub struct UnwrappedKey(Vec<u8>);
impl UnwrappedKey {
    pub fn new(key: Vec<u8>) -> Self {
        UnwrappedKey(key)
    }
}
impl std::ops::Deref for UnwrappedKey {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A fixed length array of 48 bytes that holds an AES-256-GCM-SIV wrapped key.
// TODO(b/419723745): Move this to Fxfs and keep fxfs-crypto free of versioned types.
pub type WrappedKeyBytes = WrappedKeyBytesV32;
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedKeyBytesV32(pub [u8; FXFS_WRAPPED_KEY_SIZE]);
impl Default for WrappedKeyBytes {
    fn default() -> Self {
        Self([0u8; FXFS_WRAPPED_KEY_SIZE])
    }
}
impl TryFrom<Vec<u8>> for WrappedKeyBytes {
    type Error = anyhow::Error;

    fn try_from(buf: Vec<u8>) -> Result<Self, Self::Error> {
        Ok(Self(buf.try_into().map_err(|_| anyhow!("wrapped key wrong length"))?))
    }
}
impl From<[u8; FXFS_WRAPPED_KEY_SIZE]> for WrappedKeyBytes {
    fn from(buf: [u8; FXFS_WRAPPED_KEY_SIZE]) -> Self {
        Self(buf)
    }
}
impl TypeFingerprint for WrappedKeyBytes {
    fn fingerprint() -> String {
        "WrappedKeyBytes".to_owned()
    }
}

impl std::ops::Deref for WrappedKeyBytes {
    type Target = [u8; FXFS_WRAPPED_KEY_SIZE];
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
                let bytes: [u8; FXFS_WRAPPED_KEY_SIZE] =
                    bytes.try_into().map_err(|_| SerdeError::invalid_length(orig_len, &self))?;
                Ok(WrappedKeyBytes::from(bytes))
            }
        }
        deserializer.deserialize_byte_buf(WrappedKeyVisitor)
    }
}

// TODO(b/419723745): Move this to Fxfs and keep fxfs-crypto free of versioned types.
pub type FxfsKey = FxfsKeyV40;

/// An Fxfs encryption key wrapped in AES-256-GCM-SIV and the associated wrapping key ID.
/// This can be provided to Crypt::unwrap_key to obtain the unwrapped key.
#[derive(Clone, Default, Debug, Serialize, Deserialize, TypeFingerprint, PartialEq)]
pub struct FxfsKeyV40 {
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

impl Into<fidl_fuchsia_fxfs::FxfsKey> for FxfsKey {
    fn into(self) -> fidl_fuchsia_fxfs::FxfsKey {
        fidl_fuchsia_fxfs::FxfsKey {
            wrapping_key_id: self.wrapping_key_id.to_le_bytes(),
            wrapped_key: self.key.0.into(),
        }
    }
}

#[derive(Default, Clone, Migrate, Debug, Serialize, Deserialize, TypeFingerprint)]
#[migrate_nodefault]
pub struct FxfsKeyV32 {
    pub wrapping_key_id: u64,
    pub key: WrappedKeyBytesV32,
}

impl<'a> arbitrary::Arbitrary<'a> for FxfsKey {
    fn arbitrary(_u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        // There doesn't seem to be much point to randomly generate crypto keys.
        return Ok(FxfsKey::default());
    }
}

#[derive(Arbitrary, Clone, Debug, Default, Serialize, Deserialize, PartialEq, TypeFingerprint)]
pub struct WrappedKeysV40(pub Vec<(u64, FxfsKeyV40)>);
impl From<WrappedKeysV32> for WrappedKeysV40 {
    fn from(value: WrappedKeysV32) -> Self {
        Self(value.0.into_iter().map(|(id, key)| (id, key.into())).collect())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, TypeFingerprint)]
pub struct WrappedKeysV32(pub Vec<(u64, FxfsKeyV32)>);
impl From<Vec<(u64, FxfsKeyV40)>> for WrappedKeysV40 {
    fn from(buf: Vec<(u64, FxfsKeyV40)>) -> Self {
        Self(buf)
    }
}
impl std::ops::Deref for WrappedKeysV40 {
    type Target = Vec<(u64, FxfsKeyV40)>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A thin wrapper around a ChaCha20 stream cipher.  This will use a zero nonce. **NOTE**: Great
/// care must be taken not to encrypt different plaintext with the same key and offset (even across
/// multiple boots), so consider if this suits your purpose before using it.
pub struct StreamCipher(ChaCha20);

impl StreamCipher {
    pub fn new(key: &UnwrappedKey, offset: u64) -> Self {
        let mut cipher =
            Self(ChaCha20::new(chacha20::Key::from_slice(key), /* nonce: */ &[0; 12].into()));
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

/// The `Crypt` trait below provides a mechanism to unwrap a key or set of keys.
/// The wrapping keys can be one of these types.
pub enum WrappingKey {
    /// This is used for keys of the type WrappedKey::Fxfs.
    Aes256GcmSiv([u8; 32]),
    /// This is used for legacy fscrypt keys that use a 64-byte main key.
    Fscrypt([u8; 64]),
}
impl From<[u8; 32]> for WrappingKey {
    fn from(value: [u8; 32]) -> Self {
        WrappingKey::Aes256GcmSiv(value)
    }
}
impl From<[u8; 64]> for WrappingKey {
    fn from(value: [u8; 64]) -> Self {
        WrappingKey::Fscrypt(value)
    }
}

/// The keys it unwraps can be wrapped with either Aes256GcmSiv (ideally) or using via
/// legacy fscrypt master key + HKDF.

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
    ) -> Result<(FxfsKey, UnwrappedKey), zx::Status>;

    /// `owner` is intended to be used such that when the key is wrapped, it appears to be different
    /// to that of the same key wrapped by a different owner.  In this way, keys can be shared
    /// amongst different filesystem objects (e.g. for clones), but it is not possible to tell just
    /// by looking at the wrapped keys.
    async fn create_key_with_id(
        &self,
        owner: u64,
        wrapping_key_id: u128,
    ) -> Result<(FxfsKey, UnwrappedKey), zx::Status>;

    /// Unwraps a single key, returning a raw unwrapped key.
    /// This method is generally only used with StreamCipher and FF1.
    async fn unwrap_key(
        &self,
        wrapped_key: &WrappedKey,
        owner: u64,
    ) -> Result<UnwrappedKey, zx::Status>;

    /// Unwraps object keys and stores the result as a CipherSet mapping key_id to:
    ///   - Some(cipher) if unwrapping key was found or
    ///   - None if unwrapping key was missing.
    /// The cipher can be used directly to encrypt/decrypt data.
    async fn unwrap_keys(
        &self,
        keys: &BTreeMap<u64, WrappedKey>,
        owner: u64,
    ) -> Result<CipherSet, zx::Status> {
        let futures: FuturesUnordered<_> = keys
            .iter()
            .map(|(key_id, key)| {
                let key_id = *key_id;
                let owner = owner;
                async move {
                    let unwrapped_key = match self.unwrap_key(&key, owner).await {
                        Ok(unwrapped_key) => unwrapped_key,
                        Err(zx::Status::NOT_FOUND) => return Ok((key_id, None)),
                        Err(e) => return Err(e),
                    };

                    cipher::key_to_cipher(&key, &unwrapped_key).map(|c| (key_id, c))
                }
            })
            .collect();
        let result = futures.try_collect::<BTreeMap<u64, _>>().await?;
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{StreamCipher, UnwrappedKey};

    #[test]
    fn test_stream_cipher_offset() {
        let key = UnwrappedKey::new(vec![
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
}
