// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use aes_gcm_siv::aead::Aead;
use aes_gcm_siv::{Aes256GcmSiv, Key, KeyInit as _, Nonce};
use async_trait::async_trait;
use fxfs_crypto::{Crypt, KeyPurpose, UnwrappedKey, WrappedKey, WrappedKeyBytes};
use log::error;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use rustc_hash::FxHashMap as HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
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
    ciphers: Mutex<HashMap<u128, Aes256GcmSiv>>,
    active_data_key: Option<u128>,
    active_metadata_key: Option<u128>,
    shutdown: AtomicBool,
}
impl InsecureCrypt {
    pub fn new() -> Self {
        Self {
            ciphers: Mutex::new(HashMap::from_iter([
                (0, Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&DATA_KEY))),
                (1, Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&METADATA_KEY))),
            ])),
            active_data_key: Some(0),
            active_metadata_key: Some(1),
            ..Default::default()
        }
    }

    /// Simulates a crypt instance prematurely terminating.  All requests will fail.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn add_wrapping_key(&self, id: u128, key: [u8; 32]) {
        self.ciphers
            .lock()
            .unwrap()
            .insert(id, Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&key)));
    }

    pub fn remove_wrapping_key(&self, id: u128) {
        let _key = self.ciphers.lock().unwrap().remove(&id);
    }
}

#[async_trait]
impl Crypt for InsecureCrypt {
    async fn create_key(
        &self,
        owner: u64,
        purpose: KeyPurpose,
    ) -> Result<(WrappedKey, UnwrappedKey), zx::Status> {
        if self.shutdown.load(Ordering::Relaxed) {
            error!("Crypt was shut down");
            return Err(zx::Status::INTERNAL);
        }
        let wrapping_key_id = match purpose {
            KeyPurpose::Data => self.active_data_key.as_ref(),
            KeyPurpose::Metadata => self.active_metadata_key.as_ref(),
        }
        .ok_or(zx::Status::INVALID_ARGS)?;
        let ciphers = self.ciphers.lock().unwrap();
        let cipher = ciphers.get(wrapping_key_id).ok_or(zx::Status::NOT_FOUND)?;
        let mut nonce = Nonce::default();
        nonce.as_mut_slice()[..8].copy_from_slice(&owner.to_le_bytes());

        let mut key = [0u8; 32];
        StdRng::from_entropy().fill_bytes(&mut key);

        let wrapped: Vec<u8> = cipher.encrypt(&nonce, &key[..]).map_err(|e| {
            error!("Failed to wrap key: {:?}", e);
            zx::Status::INTERNAL
        })?;
        let wrapped = WrappedKeyBytes::try_from(wrapped).map_err(|_| zx::Status::INTERNAL)?;
        Ok((WrappedKey { wrapping_key_id: *wrapping_key_id, key: wrapped }, UnwrappedKey::new(key)))
    }

    async fn create_key_with_id(
        &self,
        owner: u64,
        wrapping_key_id: u128,
    ) -> Result<(WrappedKey, UnwrappedKey), zx::Status> {
        if self.shutdown.load(Ordering::Relaxed) {
            error!("Crypt was shut down");
            return Err(zx::Status::INTERNAL);
        }
        let ciphers = self.ciphers.lock().unwrap();
        let cipher = ciphers.get(&(wrapping_key_id as u128)).ok_or(zx::Status::NOT_FOUND)?;
        let mut nonce = Nonce::default();
        nonce.as_mut_slice()[..8].copy_from_slice(&owner.to_le_bytes());

        let mut key = [0u8; 32];
        StdRng::from_entropy().fill_bytes(&mut key);

        let wrapped: Vec<u8> = cipher.encrypt(&nonce, &key[..]).map_err(|e| {
            error!("Failed to wrap key: {:?}", e);
            zx::Status::INTERNAL
        })?;
        let wrapped = WrappedKeyBytes::try_from(wrapped).map_err(|_| zx::Status::BAD_STATE)?;
        Ok((
            WrappedKey { wrapping_key_id: wrapping_key_id as u128, key: wrapped },
            UnwrappedKey::new(key),
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
        let ciphers = self.ciphers.lock().unwrap();
        let cipher = ciphers.get(&wrapped_key.wrapping_key_id).ok_or(zx::Status::NOT_FOUND)?;
        let mut nonce = Nonce::default();
        nonce.as_mut_slice()[..8].copy_from_slice(&owner.to_le_bytes());
        Ok(UnwrappedKey::new(
            cipher
                .decrypt(&nonce, &wrapped_key.key.0[..])
                .map_err(|e| {
                    error!("unwrap keys failed: {:?}", e);
                    zx::Status::INTERNAL
                })?
                .try_into()
                .map_err(|_| {
                    error!("Unexpected wrapped key length");
                    zx::Status::INTERNAL
                })?,
        ))
    }
}
