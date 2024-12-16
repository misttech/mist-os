// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use aes_gcm_siv::aead::{Aead as _, Payload};
use aes_gcm_siv::{Aes128GcmSiv, Key, KeyInit as _, Nonce};
use anyhow::Error;
use crypt_policy::{unseal_sources, KeyConsumer, Policy};
use fidl::endpoints::{create_request_stream, ClientEnd};
use fidl_fuchsia_fxfs::CryptRequest;
use futures::{FutureExt, TryStreamExt};
use hkdf::Hkdf;
use std::future::Future;
use std::pin::pin;
use uuid::Uuid;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct ZxcryptHeader {
    magic: u128,
    guid: [u8; 16],
    version: u32,
}

const ZXCRYPT_MAGIC: u128 = 0x74707972_63787a80_e7116db3_00f8e85f;
const ZXCRYPT_VERSION: u32 = 0x01000000;

async fn unwrap_zxcrypt_key(policy: Policy, wrapped_key: &[u8]) -> Result<Vec<u8>, zx::Status> {
    if wrapped_key.len() != 132 {
        return Err(zx::Status::INVALID_ARGS);
    }

    let sources = unseal_sources(policy);

    let (header, _) = ZxcryptHeader::read_from_prefix(wrapped_key).unwrap();

    for source in sources {
        let key = source.get_key(KeyConsumer::Zxcrypt).await.map_err(|_| zx::Status::INTERNAL)?;
        let hk = Hkdf::<sha2::Sha256>::new(Some(&header.guid), &key);
        let mut wrap_key = [0; 16];
        let mut wrap_iv = [0; 12];
        hk.expand("wrap key 0".as_bytes(), &mut wrap_key).unwrap();
        hk.expand("wrap iv 0".as_bytes(), &mut wrap_iv).unwrap();

        let header_size = std::mem::size_of::<ZxcryptHeader>();

        if let Ok(unwrapped) = Aes128GcmSiv::new(Key::<Aes128GcmSiv>::from_slice(&wrap_key))
            .decrypt(
                &Nonce::from_slice(&wrap_iv),
                Payload { msg: &wrapped_key[header_size..], aad: &wrapped_key[..header_size] },
            )
        {
            return Ok(unwrapped);
        }
    }
    tracing::warn!("Failed to unwrap zxcrypt key!");
    Err(zx::Status::IO_DATA_INTEGRITY)
}

async fn create_zxcrypt_key(policy: Policy) -> Result<([u8; 16], Vec<u8>, Vec<u8>), zx::Status> {
    let sources = unseal_sources(policy);

    let header = ZxcryptHeader {
        magic: ZXCRYPT_MAGIC,
        guid: *Uuid::new_v4().as_bytes(),
        version: ZXCRYPT_VERSION,
    };

    let mut unwrapped_key = vec![0; 80];
    zx::cprng_draw(&mut unwrapped_key);

    if let Some(source) = sources.first() {
        let key = source.get_key(KeyConsumer::Zxcrypt).await.map_err(|_| zx::Status::INTERNAL)?;
        let hk = Hkdf::<sha2::Sha256>::new(Some(&header.guid), &key);
        let mut wrap_key = [0; 16];
        let mut wrap_iv = [0; 12];
        hk.expand("wrap key 0".as_bytes(), &mut wrap_key).unwrap();
        hk.expand("wrap iv 0".as_bytes(), &mut wrap_iv).unwrap();

        let wrapped = Aes128GcmSiv::new(Key::<Aes128GcmSiv>::from_slice(&wrap_key))
            .encrypt(
                &Nonce::from_slice(&wrap_iv),
                Payload { msg: &unwrapped_key, aad: &header.as_bytes() },
            )
            .unwrap();

        let mut header_and_key = header.as_bytes().to_vec();
        header_and_key.extend(wrapped);

        Ok(([0; 16], header_and_key, unwrapped_key))
    } else {
        tracing::warn!("No keys sources to create zxcrypt key");
        Err(zx::Status::INTERNAL)
    }
}

/// Runs `f` with a scoped crypt service instance.  The instance will be automatically terminated on
/// completion.
pub async fn with_crypt_service<R, Fut: Future<Output = Result<R, Error>>>(
    policy: Policy,
    f: impl FnOnce(ClientEnd<fidl_fuchsia_fxfs::CryptMarker>) -> Fut,
) -> Result<R, Error> {
    let (crypt, mut stream) = create_request_stream::<fidl_fuchsia_fxfs::CryptMarker>();

    // Run a crypt service to unwrap the zxcrypt keys.
    let mut crypt_service = pin!(async {
        while let Some(request) = stream.try_next().await? {
            match request {
                CryptRequest::CreateKey { responder, .. } => responder.send(
                    create_zxcrypt_key(policy)
                        .await
                        .as_ref()
                        .map(|(id, w, u)| (id, &w[..], &u[..]))
                        .map_err(|s| s.into_raw()),
                )?,
                CryptRequest::CreateKeyWithId { responder, .. } => {
                    responder.send(Err(zx::Status::BAD_PATH.into_raw()))?
                }
                CryptRequest::UnwrapKey { responder, key, .. } => responder.send(
                    unwrap_zxcrypt_key(policy, &key)
                        .await
                        .as_ref()
                        .map(|u| &u[..])
                        .map_err(|s| s.into_raw()),
                )?,
            }
        }
        Ok::<(), Error>(())
    }
    .fuse());

    let mut fut = pin!(f(crypt).fuse());

    loop {
        futures::select! {
            _ = crypt_service => {}
            result = fut => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{with_crypt_service, ZxcryptHeader, ZXCRYPT_MAGIC, ZXCRYPT_VERSION};
    use crypt_policy::Policy;
    use zerocopy::FromBytes;

    fn entropy(data: &[u8]) -> f64 {
        let mut frequencies = [0; 256];
        for b in data {
            frequencies[*b as usize] += 1;
        }
        -frequencies
            .into_iter()
            .map(|f| {
                if f > 0 {
                    let p = f as f64 / data.len() as f64;
                    p * p.log2()
                } else {
                    0.0
                }
            })
            .sum::<f64>()
            / (data.len() as f64).log2()
    }

    #[fuchsia::test]
    async fn test_keys() {
        with_crypt_service(Policy::Null, |crypt| async {
            let crypt = crypt.into_proxy();
            let (_, key, unwrapped_key) = crypt
                .create_key(0, fidl_fuchsia_fxfs::KeyPurpose::Data)
                .await
                .unwrap()
                .expect("create_key failed");

            // Check that unwrapped_key has high entropy.
            assert!(entropy(&unwrapped_key) > 0.5);

            // Check that key has the correct fields set.
            let (header, _) = ZxcryptHeader::read_from_prefix(&key).unwrap();

            let magic = header.magic;
            assert_eq!(magic, ZXCRYPT_MAGIC);
            assert!(entropy(&header.guid) > 0.5);
            let version = header.version;
            assert_eq!(version, ZXCRYPT_VERSION);

            // Check that we can unwrap the returned key.
            let wrapping_key_id_0 = [0; 16];
            let unwrapped_key2 = crypt
                .unwrap_key(&wrapping_key_id_0, 0, &key)
                .await
                .unwrap()
                .expect("unwrap_key failed");

            assert_eq!(unwrapped_key, unwrapped_key2);
            Ok(())
        })
        .await
        .unwrap();
    }
}
