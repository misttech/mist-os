// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::errors::map_to_status;
use async_trait::async_trait;
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_fxfs::{CryptMarker, CryptProxy, KeyPurpose as FidlKeyPurpose};
use fxfs_crypto::{Crypt, KeyPurpose, UnwrappedKey, WrappedKey, WrappedKeyBytes, KEY_SIZE};
use zx;

pub struct RemoteCrypt {
    client: CryptProxy,
}

impl RemoteCrypt {
    pub fn new(client: ClientEnd<CryptMarker>) -> Self {
        Self { client: client.into_proxy() }
    }
}

trait IntoFidlKeyPurpose {
    fn into_fidl(self) -> FidlKeyPurpose;
}

impl IntoFidlKeyPurpose for KeyPurpose {
    fn into_fidl(self) -> FidlKeyPurpose {
        match self {
            KeyPurpose::Data => FidlKeyPurpose::Data,
            KeyPurpose::Metadata => FidlKeyPurpose::Metadata,
        }
    }
}

#[async_trait]
impl Crypt for RemoteCrypt {
    async fn create_key(
        &self,
        owner: u64,
        purpose: KeyPurpose,
    ) -> Result<(WrappedKey, UnwrappedKey), zx::Status> {
        let (wrapping_key_id, key, unwrapped_key) = self
            .client
            .create_key(owner, purpose.into_fidl())
            .await
            .map_err(|e| map_to_status(e.into()))?
            .map_err(|e| zx::Status::from_raw(e))?;
        Ok((
            WrappedKey {
                wrapping_key_id: u128::from_le_bytes(wrapping_key_id),
                key: WrappedKeyBytes::try_from(key).map_err(map_to_status)?,
            },
            UnwrappedKey::new(unwrapped_key.try_into().map_err(|_| zx::Status::INTERNAL)?),
        ))
    }

    async fn create_key_with_id(
        &self,
        owner: u64,
        wrapping_key_id: u128,
    ) -> Result<(WrappedKey, UnwrappedKey), zx::Status> {
        let (key, unwrapped_key) = self
            .client
            .create_key_with_id(owner, &wrapping_key_id.to_le_bytes())
            .await
            .map_err(|e| map_to_status(e.into()))?
            .map_err(|e| zx::Status::from_raw(e))?;
        Ok((
            WrappedKey {
                wrapping_key_id,
                key: WrappedKeyBytes::try_from(key).map_err(map_to_status)?,
            },
            UnwrappedKey::new(unwrapped_key.try_into().map_err(|_| zx::Status::INTERNAL)?),
        ))
    }

    async fn unwrap_key(
        &self,
        wrapped_key: &WrappedKey,
        owner: u64,
    ) -> Result<UnwrappedKey, zx::Status> {
        let unwrapped = self
            .client
            .unwrap_key(&wrapped_key.wrapping_key_id.to_le_bytes(), owner, &wrapped_key.key[..])
            .await
            .map_err(|e| map_to_status(e.into()))?
            .map_err(|e| zx::Status::from_raw(e))?;
        if unwrapped.len() != KEY_SIZE {
            return Err(zx::Status::INTERNAL);
        }
        Ok(UnwrappedKey::new(unwrapped.try_into().unwrap()))
    }
}
