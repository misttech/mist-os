// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::constants::{DATA_VOLUME_LABEL, UNENCRYPTED_VOLUME_LABEL};
use anyhow::{anyhow, Context, Error};
use crypt_policy::{format_sources, get_policy, unseal_sources, KeyConsumer};
use fidl::endpoints::{ClientEnd, Proxy};
use fidl_fuchsia_component::{self as fcomponent, RealmMarker};
use fidl_fuchsia_fs_startup::{CreateOptions, MountOptions};
use fidl_fuchsia_fxfs::{CryptManagementMarker, CryptMarker, KeyPurpose};
use fs_management::filesystem::{ServingMultiVolumeFilesystem, ServingVolume};
use fuchsia_component::client::{
    connect_to_protocol, connect_to_protocol_at_dir_root, open_childs_exposed_directory,
};
use key_bag::{Aes256Key, KeyBagManager, WrappingKey, AES128_KEY_SIZE, AES256_KEY_SIZE};
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use {fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio};

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
                log::warn!("key from {:?} source was an invalid size - skipping", source);
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

/// Unwraps the data volume in `fs`.  Any failures should be treated as fatal and the filesystem
/// should be reformatted and re-initialized.  If Ok(None) is returned, it means the keybag was
/// shredded, so a reformat is required.
/// Returns the name of the data volume as well as a reference to it.
pub async fn unlock_data_volume<'a>(
    fs: &'a mut ServingMultiVolumeFilesystem,
    config: &'a fshost_config::Config,
) -> Result<Option<(CryptService, String, &'a mut ServingVolume)>, Error> {
    // Open up the unencrypted volume so that we can access the key-bag for data.
    if config.check_filesystems {
        fs.check_volume(UNENCRYPTED_VOLUME_LABEL, None)
            .await
            .context("Failed to verify unencrypted")?;
    }
    let root_vol = fs
        .open_volume(UNENCRYPTED_VOLUME_LABEL, MountOptions::default())
        .await
        .context("Failed to open unencrypted")?;
    let keybag_dir = fuchsia_fs::directory::open_directory(
        root_vol.root(),
        "keys",
        fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .await
    .context("Failed to open keys dir")?;
    let keybag_dir_fd =
        fdio::create_fd(keybag_dir.into_channel().unwrap().into_zx_channel().into())?;
    let keybag = match KeyBagManager::open(keybag_dir_fd, Path::new("fxfs-data"))? {
        Some(keybag) => keybag,
        None => return Ok(None),
    };

    let (data_unwrapped, metadata_unwrapped) = unwrap_or_create_keys(keybag, false).await?;

    let crypt_service =
        CryptService::new(data_unwrapped, metadata_unwrapped, &config.fxfs_crypt_url)
            .await
            .context("init_crypt_service")?;
    if config.check_filesystems {
        fs.check_volume(DATA_VOLUME_LABEL, Some(crypt_service.connect()))
            .await
            .context("Failed to verify data")?;
    }
    let crypt = Some(crypt_service.connect());

    let volume = fs
        .open_volume(DATA_VOLUME_LABEL, MountOptions { crypt, ..MountOptions::default() })
        .await
        .context("Failed to open data")?;

    Ok(Some((crypt_service, DATA_VOLUME_LABEL.to_string(), volume)))
}

/// Initializes the data volume in `fs`, which should be freshly reformatted.
/// Returns the name of the data volume as well as a reference to it.
pub async fn init_data_volume<'a>(
    fs: &'a mut ServingMultiVolumeFilesystem,
    config: &'a fshost_config::Config,
) -> Result<(CryptService, String, &'a mut ServingVolume), Error> {
    // Open up the unencrypted volume so that we can access the key-bag for data.
    let root_vol = fs
        .create_volume(UNENCRYPTED_VOLUME_LABEL, CreateOptions::default(), MountOptions::default())
        .await
        .context("Failed to create unencrypted")?;
    let keybag_dir = fuchsia_fs::directory::create_directory(
        root_vol.root(),
        "keys",
        fio::PERM_READABLE | fio::PERM_WRITABLE,
    )
    .await
    .context("Failed to create keys dir")?;
    let keybag_dir_fd =
        fdio::create_fd(keybag_dir.into_channel().unwrap().into_zx_channel().into())?;
    let keybag = KeyBagManager::create(keybag_dir_fd, Path::new("fxfs-data"))?;

    let (data_unwrapped, metadata_unwrapped) = unwrap_or_create_keys(keybag, true).await?;

    let crypt_service =
        CryptService::new(data_unwrapped, metadata_unwrapped, &config.fxfs_crypt_url)
            .await
            .context("init_crypt_service")?;
    let crypt = Some(crypt_service.connect());

    let volume = fs
        .create_volume(
            DATA_VOLUME_LABEL,
            CreateOptions::default(),
            MountOptions { crypt, ..MountOptions::default() },
        )
        .await
        .context("Failed to create data")?;

    Ok((crypt_service, DATA_VOLUME_LABEL.to_string(), volume))
}

static FXFS_CRYPT_COLLECTION_NAME: &str = "fxfs-crypt";

pub struct CryptService {
    component_name: String,
    exposed_dir: fio::DirectoryProxy,
}

impl CryptService {
    async fn new(
        data_key: Aes256Key,
        metadata_key: Aes256Key,
        fxfs_crypt_url: &str,
    ) -> Result<Self, Error> {
        static INSTANCE: AtomicU64 = AtomicU64::new(1);

        let collection_ref = fdecl::CollectionRef { name: FXFS_CRYPT_COLLECTION_NAME.to_string() };

        let component_name = format!("fxfs-crypt.{}", INSTANCE.fetch_add(1, Ordering::SeqCst));

        let child_decl = fdecl::Child {
            name: Some(component_name.clone()),
            url: Some(fxfs_crypt_url.to_string()),
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
        let wrapping_key_id_0 = [0; 16];
        let mut wrapping_key_id_1 = [0; 16];
        wrapping_key_id_1[0] = 1;
        crypt_management
            .add_wrapping_key(&wrapping_key_id_0, data_key.deref())
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .add_wrapping_key(&wrapping_key_id_1, metadata_key.deref())
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Data, &wrapping_key_id_0)
            .await?
            .map_err(zx::Status::from_raw)?;
        crypt_management
            .set_active_key(KeyPurpose::Metadata, &wrapping_key_id_1)
            .await?
            .map_err(zx::Status::from_raw)?;

        Ok(CryptService { component_name, exposed_dir })
    }

    fn connect(&self) -> ClientEnd<CryptMarker> {
        // The assumption is if the crypt service child exists at all, the exposed directory will
        // have the crypt protocol, so we `expect` it.
        connect_to_protocol_at_dir_root::<CryptMarker>(&self.exposed_dir)
            .expect("Unable to connect to Crypt service")
            .into_channel()
            .unwrap()
            .into_zx_channel()
            .into()
    }

    pub fn exposed_dir(&self) -> &fio::DirectoryProxy {
        &self.exposed_dir
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

pub async fn shred_key_bag(unencrypted_volume: &ServingVolume) -> Result<(), Error> {
    let dir = fuchsia_fs::directory::open_directory(
        unencrypted_volume.root(),
        "keys",
        fio::PERM_WRITABLE,
    )
    .await
    .context("Failed to open keys dir")?;
    dir.unlink("fxfs-data", &fio::UnlinkOptions::default())
        .await?
        .map_err(|e| anyhow!(zx::Status::from_raw(e)))
        .context("Failed to remove keybag")?;
    Ok(())
}
