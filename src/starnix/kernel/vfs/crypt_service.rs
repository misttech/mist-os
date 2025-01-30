// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::EncryptionKeyId;
use aes_gcm_siv::aead::Aead;
use aes_gcm_siv::{Aes256GcmSiv, Key, KeyInit as _, Nonce};
use anyhow::{anyhow, Error};
use fidl_fuchsia_fxfs::{
    CryptCreateKeyResult, CryptCreateKeyWithIdResult, CryptRequest, CryptRequestStream,
    CryptUnwrapKeyResult, KeyPurpose,
};
use futures::stream::StreamExt;
use linux_uapi::FSCRYPT_KEY_IDENTIFIER_SIZE;
use rand::{thread_rng, Rng};
use starnix_logging::{log_error, log_info};
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use std::collections::hash_map::{Entry, HashMap};
use std::sync::Mutex;

#[derive(Clone)]
pub struct KeyInfo {
    users: Vec<u32>,
    cipher: Aes256GcmSiv,
}

#[derive(Default)]
pub struct CryptServiceInner {
    ciphers: HashMap<EncryptionKeyId, KeyInfo>,
    metadata_key: Option<EncryptionKeyId>,
}

impl CryptServiceInner {
    pub fn ciphers(&self) -> HashMap<EncryptionKeyId, KeyInfo> {
        self.ciphers.clone()
    }
}

#[derive(Default)]
pub struct CryptService {
    inner: Mutex<CryptServiceInner>,
}

fn zero_extended_nonce(val: u64) -> Nonce {
    let mut nonce = Nonce::default();
    nonce.as_mut_slice()[..8].copy_from_slice(&val.to_le_bytes());
    nonce
}

impl CryptService {
    pub fn new() -> Self {
        Self { inner: Mutex::new(CryptServiceInner::default()) }
    }

    pub fn contains_key(&self, key: EncryptionKeyId) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.ciphers.contains_key(&key)
    }

    pub fn get_users_for_key(&self, key: EncryptionKeyId) -> Option<Vec<u32>> {
        let inner = self.inner.lock().unwrap();
        inner.ciphers.get(&key).map(|x| x.users.clone())
    }

    fn create_key(&self, owner: u64, _purpose: KeyPurpose) -> CryptCreateKeyResult {
        let inner = self.inner.lock().unwrap();
        let wrapping_key_id =
            inner.metadata_key.as_ref().ok_or_else(|| zx::Status::BAD_STATE.into_raw())?;
        let cipher = inner
            .ciphers
            .get(wrapping_key_id)
            .ok_or_else(|| zx::Status::BAD_STATE.into_raw())?
            .clone()
            .cipher;
        let nonce = zero_extended_nonce(owner);

        let mut key = [0u8; 32];
        thread_rng().fill(&mut key[..]);

        let wrapped = cipher.encrypt(&nonce, &key[..]).map_err(|e| {
            log_error!("Failed to wrap key error: {:?}", e);
            zx::Status::INTERNAL.into_raw()
        })?;

        Ok((wrapping_key_id.as_raw(), wrapped.into(), key.into()))
    }

    fn create_key_with_id(
        &self,
        owner: u64,
        wrapping_key_id: EncryptionKeyId,
    ) -> CryptCreateKeyWithIdResult {
        let inner = self.inner.lock().unwrap();
        let cipher = inner
            .ciphers
            .get(&wrapping_key_id)
            .ok_or_else(|| zx::Status::NOT_FOUND.into_raw())?
            .clone()
            .cipher;
        let nonce = zero_extended_nonce(owner);

        let mut key = [0u8; 32];
        thread_rng().fill(&mut key[..]);

        let wrapped = cipher.encrypt(&nonce, &key[..]).map_err(|error| {
            log_error!("Failed to wrap key error: {:?}", error);
            zx::Status::INTERNAL.into_raw()
        })?;

        Ok((wrapped.into(), key.into()))
    }

    fn unwrap_key(
        &self,
        wrapping_key_id: EncryptionKeyId,
        owner: u64,
        key: Vec<u8>,
    ) -> CryptUnwrapKeyResult {
        let inner = self.inner.lock().unwrap();
        let cipher = inner
            .ciphers
            .get(&wrapping_key_id)
            .ok_or_else(|| zx::Status::NOT_FOUND.into_raw())?
            .clone()
            .cipher;
        let nonce = zero_extended_nonce(owner);

        cipher.decrypt(&nonce, &key[..]).map_err(|_| zx::Status::IO_DATA_INTEGRITY.into_raw())
    }

    pub fn add_wrapping_key(
        &self,
        wrapping_key_id: [u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize],
        key: Vec<u8>,
        uid: u32,
    ) -> Result<(), Errno> {
        let mut inner = self.inner.lock().unwrap();
        match inner.ciphers.entry(EncryptionKeyId::from(wrapping_key_id)) {
            Entry::Occupied(mut e) => {
                let users = &mut e.get_mut().users;
                if !users.contains(&uid) {
                    users.push(uid);
                }
                Ok(())
            }
            Entry::Vacant(vacant) => {
                log_info!("Adding wrapping key with id: {:?}", &wrapping_key_id);
                vacant.insert(KeyInfo {
                    users: vec![uid],
                    cipher: Aes256GcmSiv::new(Key::<Aes256GcmSiv>::from_slice(&key[..])),
                });
                Ok(())
            }
        }
    }

    pub fn forget_wrapping_key(
        &self,
        wrapping_key_id: [u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize],
        uid: u32,
    ) -> Result<(), Errno> {
        log_info!("Removing wrapping key with id: {:?}", &wrapping_key_id);
        let mut inner = self.inner.lock().unwrap();
        match inner.ciphers.entry(EncryptionKeyId::from(wrapping_key_id)) {
            Entry::Occupied(mut e) => {
                let user_ids = &mut e.get_mut().users;
                if !user_ids.contains(&uid) {
                    return Err(errno!(ENOKEY));
                } else {
                    let index = user_ids.iter().position(|x: &u32| *x == uid).unwrap();
                    user_ids.remove(index);
                    if user_ids.is_empty() {
                        e.remove();
                    }
                }
            }
            Entry::Vacant(_) => {
                return Err(errno!(ENOKEY));
            }
        }
        Ok(())
    }

    pub fn set_metadata_key(
        &self,
        wrapping_key_id: [u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize],
    ) -> Result<(), Errno> {
        let mut inner = self.inner.lock().unwrap();
        let key_id = EncryptionKeyId::from(wrapping_key_id);
        if !inner.ciphers.contains_key(&key_id) {
            return Err(errno!(ENOENT));
        }
        inner.metadata_key = Some(key_id);
        Ok(())
    }

    pub async fn handle_connection(&self, mut stream: CryptRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.next().await {
            match request {
                Ok(CryptRequest::CreateKey { owner, purpose, responder }) => {
                    responder
                        .send(match &self.create_key(owner, purpose) {
                            Ok((id, ref wrapped, ref key)) => Ok((id, wrapped, key)),
                            Err(e) => Err(*e),
                        })
                        .unwrap_or_else(|e| {
                            log_error!("Failed to send CreateKey response {:?}", e)
                        });
                }
                Ok(CryptRequest::CreateKeyWithId { owner, wrapping_key_id, responder }) => {
                    responder
                        .send(
                            match self
                                .create_key_with_id(owner, EncryptionKeyId::from(wrapping_key_id))
                            {
                                Ok((ref wrapped, ref key)) => Ok((wrapped, key)),
                                Err(e) => Err(e),
                            },
                        )
                        .unwrap_or_else(|e| {
                            log_error!("Failed to send CreateKeyWithId response {:?}", e)
                        });
                }
                Ok(CryptRequest::UnwrapKey { wrapping_key_id, owner, key, responder }) => {
                    responder
                        .send(
                            match self.unwrap_key(
                                EncryptionKeyId::from(wrapping_key_id),
                                owner,
                                key,
                            ) {
                                Ok(ref unwrapped) => Ok(unwrapped),
                                Err(e) => Err(e),
                            },
                        )
                        .unwrap_or_else(|e| {
                            log_error!("Failed to send UnwrapKey response {:?}", e)
                        });
                }
                Err(e) => {
                    log_error!("Error in CryptRequestStream: {:?}", e);
                    return Err(anyhow!(e));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::task::EncryptionKeyId;

    use super::CryptService;
    use fidl_fuchsia_fxfs::KeyPurpose;
    use starnix_uapi::errno;

    #[test]
    fn create_key_without_setting_metadata_key() {
        let service = CryptService::new();
        let key = vec![0xABu8; 32];
        service.add_wrapping_key(u128::to_le_bytes(1), key, 0).expect("add wrapping key failed");
        assert_eq!(
            service
                .create_key(0, KeyPurpose::Data)
                .expect_err("create_key should fail without a metadata key set"),
            zx::Status::BAD_STATE.into_raw()
        );
        service.set_metadata_key(u128::to_le_bytes(1)).expect("failed to set metadata key");
        service.create_key(0, KeyPurpose::Data).expect("create_key failed");
    }

    #[test]
    fn add_and_forget_wrapping_keys() {
        let service = CryptService::new();
        let key = vec![0xABu8; 32];
        assert_eq!(
            service
                .forget_wrapping_key(u128::to_le_bytes(1), 0)
                .expect_err("forget wrapping key should fail if the key was never added"),
            errno!(ENOKEY)
        );
        // Add the wrapping key for users 0 and 1
        service
            .add_wrapping_key(u128::to_le_bytes(1), key.clone(), 0)
            .expect("add wrapping key failed");
        service
            .add_wrapping_key(u128::to_le_bytes(1), key.clone(), 1)
            .expect("add wrapping key failed");
        // A user should be able to add the same key multiple times.
        service
            .add_wrapping_key(u128::to_le_bytes(1), key.clone(), 1)
            .expect("add wrapping key failed");

        {
            let inner = service.inner.lock().unwrap();
            assert_eq!(
                inner.ciphers.get(&EncryptionKeyId::from(u128::to_le_bytes(1))).unwrap().users,
                [0, 1]
            );
        }

        // User 1 forgets the wrapping key. Since user 0 still has the key added,
        // create_key_with_id should still succeed.
        service.forget_wrapping_key(u128::to_le_bytes(1), 1).expect("forget wrapping key failed");
        service
            .create_key_with_id(0, EncryptionKeyId::from(u128::to_le_bytes(1)))
            .expect("create key with id failed");

        // User 1 cannot forget the same key a second time.
        assert_eq!(
            service.forget_wrapping_key(u128::to_le_bytes(1), 1).expect_err(
                "forget wrapping key should fail if the key was already removed by this user"
            ),
            errno!(ENOKEY)
        );
        // Once both users remove the key, create_key_with_id should fail.
        service.forget_wrapping_key(u128::to_le_bytes(1), 0).expect("forget wrapping key failed");
        assert_eq!(
            service.create_key_with_id(0, EncryptionKeyId::from(u128::to_le_bytes(1))).expect_err(
                "create_key_with_id should fail if the key hasn't been added by the caller"
            ),
            zx::Status::NOT_FOUND.into_raw()
        );
        service
            .add_wrapping_key(u128::to_le_bytes(1), key.clone(), 0)
            .expect("add wrapping key failed");
    }

    #[test]
    fn wrap_unwrap_key() {
        let service = CryptService::new();
        let key = vec![0xABu8; 32];
        service.add_wrapping_key(u128::to_le_bytes(1), key.clone(), 0).expect("add_key failed");
        service.set_metadata_key(u128::to_le_bytes(1)).expect("set metadata key failed");

        let (wrapping_key_id, wrapped, unwrapped) =
            service.create_key(0, KeyPurpose::Data).expect("create_key failed");
        let unwrap_result = service
            .unwrap_key(EncryptionKeyId::from(wrapping_key_id), 0, wrapped)
            .expect("unwrap_key failed");
        assert_eq!(unwrap_result, unwrapped);

        // Do it twice to make sure the service can use the same key repeatedly.
        let (wrapping_key_id, wrapped, unwrapped) =
            service.create_key(1, KeyPurpose::Data).expect("create_key failed");
        let unwrap_result = service
            .unwrap_key(EncryptionKeyId::from(wrapping_key_id), 1, wrapped)
            .expect("unwrap_key failed");
        assert_eq!(unwrap_result, unwrapped);
    }

    #[test]
    fn wrap_unwrap_key_with_arbitrary_wrapping_key() {
        let service = CryptService::new();
        let key = vec![0xABu8; 32];
        service
            .add_wrapping_key(u128::to_le_bytes(1), key.clone(), 0)
            .expect("add wrapping key failed");

        let (wrapped, unwrapped) = service
            .create_key_with_id(0, u128::to_le_bytes(1).into())
            .expect("create_key_with_id failed");
        let unwrap_result =
            service.unwrap_key(u128::to_le_bytes(1).into(), 0, wrapped).expect("unwrap_key failed");
        assert_eq!(unwrap_result, unwrapped);

        // Do it twice to make sure the service can use the same key repeatedly.
        let (wrapped, unwrapped) = service
            .create_key_with_id(1, u128::to_le_bytes(1).into())
            .expect("create_key_with_id failed");
        let unwrap_result =
            service.unwrap_key(u128::to_le_bytes(1).into(), 1, wrapped).expect("unwrap_key failed");
        assert_eq!(unwrap_result, unwrapped);
    }

    #[test]
    fn create_key_with_wrapping_key_that_does_not_exist() {
        let service = CryptService::new();
        service
            .create_key_with_id(0, u128::to_le_bytes(1).into())
            .expect_err("create_key_with_id should fail if the wrapping key does not exist");

        let wrapping_key = vec![0xABu8; 32];
        service
            .add_wrapping_key(u128::to_le_bytes(1), wrapping_key.clone(), 0)
            .expect("add wrapping key failed");

        let (wrapped, unwrapped) = service
            .create_key_with_id(0, u128::to_le_bytes(1).into())
            .expect("create_key_with_id failed");
        let unwrap_result =
            service.unwrap_key(u128::to_le_bytes(1).into(), 0, wrapped).expect("unwrap_key failed");
        assert_eq!(unwrap_result, unwrapped);
    }

    #[test]
    fn unwrap_key_wrong_key() {
        let service = CryptService::new();
        let key = vec![0xABu8; 32];
        service.add_wrapping_key(u128::to_le_bytes(1), key.clone(), 0).expect("add_key failed");
        service.set_metadata_key(u128::to_le_bytes(1)).expect("set_active_key failed");

        let (wrapping_key_id, mut wrapped, _) =
            service.create_key(0, KeyPurpose::Data).expect("create_key failed");
        for byte in &mut wrapped {
            *byte ^= 0xff;
        }
        service
            .unwrap_key(EncryptionKeyId::from(wrapping_key_id), 0, wrapped)
            .expect_err("unwrap_key should fail");
    }

    #[test]
    fn unwrap_key_wrong_owner() {
        let service = CryptService::new();
        let key = vec![0xABu8; 32];
        service
            .add_wrapping_key(u128::to_le_bytes(1), key.clone(), 0)
            .expect("add wrapping key failed");
        service.set_metadata_key(u128::to_le_bytes(1)).expect("set metadata key failed");

        let (wrapping_key_id, wrapped, _) =
            service.create_key(0, KeyPurpose::Data).expect("create_key failed");
        service
            .unwrap_key(EncryptionKeyId::from(wrapping_key_id), 1, wrapped)
            .expect_err("unwrap_key should fail");
    }
}
