// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use aes_gcm_siv::aead::Aead;
use aes_gcm_siv::{Aes256GcmSiv, Key, KeyInit as _, Nonce};
use async_trait::async_trait;
use fscrypt::hkdf::{self, fscrypt_hkdf};
use fuchsia_sync::Mutex;
use fxfs_crypto::{
    Crypt, FscryptKeyIdentifier, FscryptKeyIdentifierAndNonce, FxfsKey, KeyPurpose, UnwrappedKey,
    WrappedKey, WrappedKeyBytes, WrappingKey,
};
use log::error;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use rustc_hash::FxHashMap as HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use zx_status as zx;

pub const DATA_KEY: [u8; 32] = [
    0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xa, 0xb, 0xc, 0xd, 0xe, 0xf, 0x10, 0x11,
    0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
pub const METADATA_KEY: [u8; 32] = [
    0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4, 0xf3, 0xf2, 0xf1, 0xf0,
    0xef, 0xee, 0xed, 0xec, 0xeb, 0xea, 0xe9, 0xe8, 0xe7, 0xe6, 0xe5, 0xe4, 0xe3, 0xe2, 0xe1, 0xe0,
];

/// This struct provides the `Crypt` trait without any strong security.
///
/// It is intended for use only in test code where actual security is inconsequential.
#[derive(Default)]
pub struct InsecureCrypt {
    /// FxfsKey is wrapped using AES256GCM-SIV.
    /// Unwrapping turns a 48-byte signed key into a 32-byte raw key.
    /// This maps from an opaque wrapping_key_id to a specific cipher instance that can unwrap keys.
    gcmsiv_ciphers: Mutex<HashMap<u128, Aes256GcmSiv>>,

    /// Legacy Fscrypt uses a 64-byte raw key identified using a 16-byte HKDF derivation of the key.
    /// "Unwrapping" a legacy fscrypt_key involves mixing the 64-byte key material with additional
    /// data based on the exact encryption scheme used (e.g. sometimes a 16-byte nonce) so we
    /// store these keys verbatim.
    fscrypt_keys: Mutex<HashMap<[u8; 16], [u8; 64]>>,

    /// Legacy fscrypt uses the filesystem UUID to salt encryption keys in some variants.
    /// We don't have direct access to the filesystem so we store the UUID here.
    filesystem_uuid: [u8; 16],

    active_data_key: Option<u128>,
    active_metadata_key: Option<u128>,
    shutdown: AtomicBool,
}
impl InsecureCrypt {
    pub fn new() -> Self {
        Self {
            gcmsiv_ciphers: Mutex::new(HashMap::from_iter([
                (0, Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&DATA_KEY))),
                (1, Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&METADATA_KEY))),
            ])),
            fscrypt_keys: Mutex::new(HashMap::default()),
            filesystem_uuid: [0; 16],
            active_data_key: Some(0),
            active_metadata_key: Some(1),
            ..Default::default()
        }
    }

    /// Simulates a crypt instance prematurely terminating.  All requests will fail.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn add_wrapping_key(&self, id: u128, key: WrappingKey) {
        match key {
            WrappingKey::Aes256GcmSiv(key) => {
                self.gcmsiv_ciphers
                    .lock()
                    .insert(id, Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&key)));
            }
            WrappingKey::Fscrypt(main_key) => {
                self.fscrypt_keys.lock().insert(id.to_le_bytes(), main_key);
            }
        }
    }

    pub fn remove_wrapping_key(&self, id: u128) {
        let _key = self.gcmsiv_ciphers.lock().remove(&id);
        self.fscrypt_keys.lock().remove(&id.to_le_bytes());
    }

    /// Fscrypt in INO_LBLK32 and INO_LBLK64 modes mix the filesystem_uuid into key derivation
    /// functions. Crypt should be told the uuid ahead of time to support decryption of migrated
    /// data. (Note that we make an assumption that there is only one filesystem.)
    pub fn set_filesystem_uuid(&mut self, uuid: &[u8; 16]) {
        self.filesystem_uuid = *uuid;
    }
}

#[async_trait]
impl Crypt for InsecureCrypt {
    async fn create_key(
        &self,
        owner: u64,
        purpose: KeyPurpose,
    ) -> Result<(FxfsKey, UnwrappedKey), zx::Status> {
        if self.shutdown.load(Ordering::Relaxed) {
            error!("Crypt was shut down");
            return Err(zx::Status::INTERNAL);
        }
        let wrapping_key_id = match purpose {
            KeyPurpose::Data => self.active_data_key.as_ref(),
            KeyPurpose::Metadata => self.active_metadata_key.as_ref(),
        }
        .ok_or(zx::Status::INVALID_ARGS)?;
        let ciphers = self.gcmsiv_ciphers.lock();
        let cipher = ciphers.get(wrapping_key_id).ok_or(zx::Status::NOT_FOUND)?;
        let mut nonce = Nonce::default();
        nonce.as_mut_slice()[..8].copy_from_slice(&owner.to_le_bytes());

        let mut key = [0u8; 32];
        StdRng::from_os_rng().fill_bytes(&mut key);

        let wrapped: Vec<u8> = cipher.encrypt(&nonce, &key[..]).map_err(|e| {
            error!("Failed to wrap key: {:?}", e);
            zx::Status::INTERNAL
        })?;
        let wrapped = WrappedKeyBytes::try_from(wrapped).map_err(|_| zx::Status::INTERNAL)?;
        Ok((
            FxfsKey { wrapping_key_id: *wrapping_key_id, key: wrapped },
            UnwrappedKey::new(key.to_vec()),
        ))
    }

    async fn create_key_with_id(
        &self,
        owner: u64,
        wrapping_key_id: u128,
    ) -> Result<(FxfsKey, UnwrappedKey), zx::Status> {
        if self.shutdown.load(Ordering::Relaxed) {
            error!("Crypt was shut down");
            return Err(zx::Status::INTERNAL);
        }
        let ciphers = self.gcmsiv_ciphers.lock();
        let cipher = ciphers.get(&(wrapping_key_id as u128)).ok_or(zx::Status::NOT_FOUND)?;
        let mut nonce = Nonce::default();
        nonce.as_mut_slice()[..8].copy_from_slice(&owner.to_le_bytes());

        let mut key = [0u8; 32];
        StdRng::from_os_rng().fill_bytes(&mut key);

        let wrapped: Vec<u8> = cipher.encrypt(&nonce, &key[..]).map_err(|e| {
            error!("Failed to wrap key: {:?}", e);
            zx::Status::INTERNAL
        })?;
        let wrapped = WrappedKeyBytes::try_from(wrapped).map_err(|_| zx::Status::BAD_STATE)?;
        Ok((
            FxfsKey { wrapping_key_id: wrapping_key_id as u128, key: wrapped },
            UnwrappedKey::new(key.to_vec()),
        ))
    }
    async fn unwrap_key(
        &self,
        wrapped_key: &WrappedKey,
        owner: u64,
    ) -> Result<UnwrappedKey, zx::Status> {
        if self.shutdown.load(Ordering::Relaxed) {
            error!("Crypt was shut down");
            return Err(zx::Status::INTERNAL);
        }
        let ciphers = self.gcmsiv_ciphers.lock();
        Ok(match wrapped_key {
            WrappedKey::Fxfs(fxfs_key) => {
                let cipher = ciphers
                    .get(&u128::from_le_bytes(fxfs_key.wrapping_key_id))
                    .ok_or(zx::Status::NOT_FOUND)?;
                let mut nonce = Nonce::default();
                nonce.as_mut_slice()[..8].copy_from_slice(&owner.to_le_bytes());
                UnwrappedKey::new(
                    cipher
                        .decrypt(&nonce, &fxfs_key.wrapped_key[..])
                        .map_err(|e| {
                            error!("unwrap keys failed: {:?}", e);
                            zx::Status::INTERNAL
                        })?
                        .try_into()
                        .map_err(|_| {
                            error!("Unexpected wrapped key length");
                            zx::Status::INTERNAL
                        })?,
                )
            }
            WrappedKey::FscryptInoLblk32Dir(FscryptKeyIdentifierAndNonce {
                key_identifier,
                nonce,
            }) => {
                if let Some(main_key) = self.fscrypt_keys.lock().get(key_identifier) {
                    // Creates a shared key from the main key and filesystem UUID.
                    // In this mode the inode is hashed and mixed into the tweak.
                    // The nonce is not needed for file contents.
                    let mut hdkf_info = [0; 17];
                    hdkf_info[1..17].copy_from_slice(&self.filesystem_uuid);
                    hdkf_info[0] = fscrypt::ENCRYPTION_MODE_AES_256_CTS;
                    let cts_key = fscrypt_hkdf::<32>(
                        main_key,
                        &hdkf_info,
                        hkdf::HKDF_CONTEXT_IV_INO_LBLK_32_KEY,
                    );
                    let ino_hash_key =
                        fscrypt_hkdf::<16>(main_key, &[], hkdf::HKDF_CONTEXT_INODE_HASH_KEY);
                    let dirhash_key =
                        fscrypt_hkdf::<16>(main_key, nonce, hkdf::HKDF_CONTEXT_DIRHASH_KEY);
                    // Output is the concatenation of cts_key, ino_hash, dirhash.
                    let mut out = cts_key.to_vec();
                    out.extend_from_slice(&ino_hash_key[..]);
                    out.extend_from_slice(&dirhash_key[..]);
                    UnwrappedKey::new(out)
                } else {
                    return Err(zx::Status::NOT_FOUND);
                }
            }
            WrappedKey::FscryptInoLblk32File(FscryptKeyIdentifier { key_identifier }) => {
                if let Some(main_key) = self.fscrypt_keys.lock().get(key_identifier) {
                    // Creates a shared key from the main key and filesystem UUID.
                    // In this mode the inode is hashed and mixed into the tweak.
                    // The nonce is not needed for file contents.
                    let mut hdkf_info = [0; 17];
                    hdkf_info[1..17].copy_from_slice(&self.filesystem_uuid);
                    hdkf_info[0] = fscrypt::ENCRYPTION_MODE_AES_256_XTS;
                    let xts_key = fscrypt_hkdf::<64>(
                        main_key,
                        &hdkf_info,
                        hkdf::HKDF_CONTEXT_IV_INO_LBLK_32_KEY,
                    );
                    let ino_hash_key =
                        fscrypt_hkdf::<16>(main_key, &[], hkdf::HKDF_CONTEXT_INODE_HASH_KEY);
                    // Output is the concatenation of key1, key2, ino_hash.
                    let mut out = xts_key.to_vec();
                    out.extend_from_slice(&ino_hash_key[..]);
                    UnwrappedKey::new(out)
                } else {
                    return Err(zx::Status::NOT_FOUND);
                }
            }
            _ => {
                error!("Unsupported wrapped key {wrapped_key:?}");
                return Err(zx::Status::NOT_SUPPORTED);
            }
        })
    }
}
