// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{format_sources, get_policy, unseal_sources, KeyConsumer},
    anyhow::{anyhow, Context, Error},
    fidl::endpoints::Proxy,
    fidl_fuchsia_component::{self as fcomponent, RealmMarker},
    fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose, MountOptions},
    fidl_fuchsia_io as fio,
    fs_management::filesystem::{ServingMultiVolumeFilesystem, ServingVolume},
    fuchsia_component::client::{
        connect_to_protocol, connect_to_protocol_at_dir_root, open_childs_exposed_directory,
    },
    fuchsia_zircon as zx,
    key_bag::{Aes256Key, KeyBagManager, WrappingKey, AES128_KEY_SIZE, AES256_KEY_SIZE},
    std::{
        ops::Deref,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    },
};

async fn unwrap_or_create_keys(
    mut keybag: KeyBagManager,
    create: bool,
) -> Result<(Aes256Key, Aes256Key), Error> {
    let policy = get_policy().await?;
    let sources = if create { format_sources(policy) } else { unseal_sources(policy) };

    let mut last_err = anyhow!("no keys?");
    for source in sources {
        let key = source.get_key(KeyConsumer::Fxfs).await?;
        let wrapping_key = match key.len() {
            // unwrap is safe because we know the length of the requested array is the same length
            // as the Vec in both branches.
            AES128_KEY_SIZE => WrappingKey::Aes128(key.try_into().unwrap()),
            AES256_KEY_SIZE => WrappingKey::Aes256(key.try_into().unwrap()),
            _ => {
                tracing::warn!("key from {:?} source was an invalid size - skipping", source);
                last_err = anyhow!("invalid key size");
                continue;
            }
        };

        let mut unwrap_fn = |slot| {
            if create {
                keybag.new_key(slot, &wrapping_key).context("new key")
            } else {
                keybag.unwrap_key(slot, &wrapping_key).context("unwrapping key")
            }
        };

        let data_unwrapped = match unwrap_fn(0) {
            Ok(data_unwrapped) => data_unwrapped,
            Err(e) => {
                last_err = e.context("data key");
                continue;
            }
        };
        let metadata_unwrapped = match unwrap_fn(1) {
            Ok(metadata_unwrapped) => metadata_unwrapped,
            Err(e) => {
                last_err = e.context("metadata key");
                continue;
            }
        };
        return Ok((data_unwrapped, metadata_unwrapped));
    }
    Err(last_err)
}

// Unwraps the data volume in `fs`.  Any failures should be treated as fatal and the filesystem
// should be reformatted and re-initialized.
// Returns the name of the data volume as well as a reference to it.
pub async fn unlock_data_volume<'a>(
    fs: &'a mut ServingMultiVolumeFilesystem,
    config: &'a fshost_config::Config,
) -> Result<(CryptService, String, &'a mut ServingVolume), Error> {
    unlock_or_init_data_volume(fs, config, false).await
}

// Initializes the data volume in `fs`, which should be freshly reformatted.
// Returns the name of the data volume as well as a reference to it.
pub async fn init_data_volume<'a>(
    fs: &'a mut ServingMultiVolumeFilesystem,
    config: &'a fshost_config::Config,
) -> Result<(CryptService, String, &'a mut ServingVolume), Error> {
    unlock_or_init_data_volume(fs, config, true).await
}

async fn unlock_or_init_data_volume<'a>(
    fs: &'a mut ServingMultiVolumeFilesystem,
    config: &'a fshost_config::Config,
    create: bool,
) -> Result<(CryptService, String, &'a mut ServingVolume), Error> {
    // Open up the unencrypted volume so that we can access the key-bag for data.
    let root_vol = if create {
        fs.create_volume("unencrypted", None).await.context("Failed to create unencrypted")?
    } else {
        if config.check_filesystems {
            fs.check_volume("unencrypted", None).await.context("Failed to verify unencrypted")?;
        }
        fs.open_volume("unencrypted", MountOptions { crypt: None, as_blob: false })
            .await
            .context("Failed to open unencrypted")?
    };
    root_vol.bind_to_path("/unencrypted_volume")?;
    if create {
        std::fs::create_dir("/unencrypted_volume/keys").map_err(|e| anyhow!(e))?;
    }
    let keybag = KeyBagManager::open(Path::new("/unencrypted_volume/keys/fxfs-data"))
        .map_err(|e| anyhow!(e))?;

    let (data_unwrapped, metadata_unwrapped) = unwrap_or_create_keys(keybag, create).await?;

    // Make sure we unbind the path we used, in case another fxfs instance goes through unlock
    // or init. This needs to be after we are done using the keybag, as it keeps using the path
    // in it's operations.
    // TODO(fxbug.dev/122966): when keybag takes a proxy instead of a path, we don't need to
    // worry about managing the namespace binding and can remove this.
    root_vol.unbind_path();

    let crypt_service = CryptService::new(data_unwrapped, metadata_unwrapped)
        .await
        .context("init_crypt_service")?;
    let crypt = Some(
        connect_to_protocol_at_dir_root::<CryptMarker>(&crypt_service.exposed_dir)
            .expect("Unable to connect to Crypt service")
            .into_channel()
            .unwrap()
            .into_zx_channel()
            .into(),
    );

    let volume = if create {
        fs.create_volume("data", crypt).await.context("Failed to create data")?
    } else {
        let crypt = if config.check_filesystems {
            fs.check_volume("data", crypt).await.context("Failed to verify data")?;
            Some(
                connect_to_protocol_at_dir_root::<CryptMarker>(&crypt_service.exposed_dir)
                    .expect("Unable to connect to Crypt service")
                    .into_channel()
                    .unwrap()
                    .into_zx_channel()
                    .into(),
            )
        } else {
            crypt
        };
        fs.open_volume("data", MountOptions { crypt, as_blob: false })
            .await
            .context("Failed to open data")?
    };

    Ok((crypt_service, "data".to_string(), volume))
}

static FXFS_CRYPT_COLLECTION_NAME: &str = "fxfs-crypt";

pub struct CryptService {
    component_name: String,
    exposed_dir: fio::DirectoryProxy,
}

impl CryptService {
    async fn new(data_key: Aes256Key, metadata_key: Aes256Key) -> Result<Self, Error> {
        static INSTANCE: AtomicU64 = AtomicU64::new(1);

        let collection_ref = fdecl::CollectionRef { name: FXFS_CRYPT_COLLECTION_NAME.to_string() };

        let component_name = format!("fxfs-crypt.{}", INSTANCE.fetch_add(1, Ordering::SeqCst));

        let child_decl = fdecl::Child {
            name: Some(component_name.clone()),
            url: Some("#meta/fxfs-crypt.cm".to_string()),
            startup: Some(fdecl::StartupMode::Lazy),
            ..Default::default()
        };

        let realm_proxy = connect_to_protocol::<RealmMarker>()?;

        realm_proxy
            .create_child(&collection_ref, &child_decl, fcomponent::CreateChildArgs::default())
            .await?
            .map_err(|e| anyhow!("create_child failed: {:?}", e))?;

        let exposed_dir = open_childs_exposed_directory(
            component_name.clone(),
            Some(FXFS_CRYPT_COLLECTION_NAME.to_string()),
        )
        .await?;

        let crypt_management =
            connect_to_protocol_at_dir_root::<CryptManagementMarker>(&exposed_dir)?;
        crypt_management
            .add_wrapping_key(0, data_key.deref())
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .add_wrapping_key(1, metadata_key.deref())
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Data, 0)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Metadata, 1)
            .await?
            .map_err(zx::Status::from_raw)?;

        Ok(CryptService { component_name, exposed_dir })
    }
}

impl Drop for CryptService {
    fn drop(&mut self) {
        if let Ok(realm_proxy) = connect_to_protocol::<RealmMarker>() {
            let _ = realm_proxy.destroy_child(&fdecl::ChildRef {
                name: self.component_name.clone(),
                collection: Some(FXFS_CRYPT_COLLECTION_NAME.to_string()),
            });
        }
    }
}
