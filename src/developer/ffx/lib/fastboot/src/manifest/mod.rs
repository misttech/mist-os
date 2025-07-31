// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::{Boot, Flash, Unlock};
use crate::file_resolver::resolvers::{Resolver, ZipArchiveResolver};
use crate::file_resolver::FileResolver;
use crate::manifest::resolvers::{
    ArchiveResolver, FlashManifestResolver, FlashManifestTarResolver, ManifestResolver,
};
use crate::util::Event;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use camino::Utf8Path;
use errors::ffx_bail;
use ffx_fastboot_interface::fastboot_interface::FastbootInterface;
use ffx_flash_manifest::{BootParams, Command, FlashManifestVersion, ManifestParams};
use pbms::load_product_bundle;
use product_bundle::ProductBundle;
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;

pub mod resolvers;
pub mod v1;
pub mod v2;
pub mod v3;

#[derive(Default, Deserialize)]
pub struct Image {
    pub name: String,
    pub path: String,
    // Ignore the rest of the fields
}

#[async_trait(?Send)]
impl Flash for FlashManifestVersion {
    async fn flash<F, T>(
        &self,
        messenger: &Sender<Event>,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        match self {
            Self::V1(v) => v.flash(messenger, file_resolver, fastboot_interface, cmd).await?,
            Self::V2(v) => v.flash(messenger, file_resolver, fastboot_interface, cmd).await?,
            Self::V3(v) => v.flash(messenger, file_resolver, fastboot_interface, cmd).await?,
        };
        Ok(())
    }
}

#[async_trait(?Send)]
impl Unlock for FlashManifestVersion {
    async fn unlock<F, T>(
        &self,
        messenger: &Sender<Event>,
        file_resolver: &mut F,
        fastboot_interface: &mut T,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        match self {
            Self::V1(v) => v.unlock(messenger, file_resolver, fastboot_interface).await?,
            Self::V2(v) => v.unlock(messenger, file_resolver, fastboot_interface).await?,
            Self::V3(v) => v.unlock(messenger, file_resolver, fastboot_interface).await?,
        };
        Ok(())
    }
}

#[async_trait(?Send)]
impl Boot for FlashManifestVersion {
    async fn boot<F, T>(
        &self,
        messenger: Sender<Event>,
        file_resolver: &mut F,
        slot: String,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()>
    where
        F: FileResolver + Sync,
        T: FastbootInterface,
    {
        match self {
            Self::V1(v) => v.boot(messenger, file_resolver, slot, fastboot_interface, cmd).await?,
            Self::V2(v) => v.boot(messenger, file_resolver, slot, fastboot_interface, cmd).await?,
            Self::V3(v) => v.boot(messenger, file_resolver, slot, fastboot_interface, cmd).await?,
        };
        Ok(())
    }
}

pub async fn from_sdk<F: FastbootInterface>(
    messenger: &Sender<Event>,
    fastboot_interface: &mut F,
    cmd: ManifestParams,
) -> Result<()> {
    log::debug!("fastboot manifest from_sdk");
    match cmd.product_bundle.as_ref() {
        Some(b) => {
            let product_bundle = load_product_bundle(&Some(b.to_string())).await?.into();
            FlashManifest {
                resolver: Resolver::new(PathBuf::from(b))?,
                version: FlashManifestVersion::from_product_bundle(&product_bundle)?,
            }
            .flash(messenger, fastboot_interface, cmd)
            .await
        }
        None => ffx_bail!(
            "Please supply the `--product-bundle` option to identify which product bundle to flash"
        ),
    }
}

pub async fn from_local_product_bundle<F: FastbootInterface>(
    messenger: &Sender<Event>,
    path: PathBuf,
    fastboot_interface: &mut F,
    cmd: ManifestParams,
) -> Result<()> {
    log::debug!("fastboot manifest from_local_product_bundle");
    let path = Utf8Path::from_path(&*path).ok_or_else(|| anyhow!("Error getting path"))?;
    let product_bundle = ProductBundle::try_load_from(path)?;

    let flash_manifest_version = FlashManifestVersion::from_product_bundle(&product_bundle)?;

    match (path.is_file(), path.extension()) {
        (true, Some("zip")) => {
            FlashManifest {
                resolver: ZipArchiveResolver::new(path.into())?,
                version: flash_manifest_version,
            }
            .flash(messenger, fastboot_interface, cmd)
            .await
        }
        (true, extension) => Err(anyhow!(
            "Attempting to flash using a Product Bundle file with unsupported extension: {:#?}",
            extension
        )),
        (false, _) => {
            FlashManifest { resolver: Resolver::new(path.into())?, version: flash_manifest_version }
                .flash(messenger, fastboot_interface, cmd)
                .await
        }
    }
}

pub async fn from_in_tree<T: FastbootInterface>(
    messenger: &Sender<Event>,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()> {
    log::debug!("fastboot manifest from_in_tree");
    if cmd.product_bundle.is_some() {
        log::debug!("in tree, but product bundle specified, use in-tree sdk");
        from_sdk(messenger, fastboot_interface, cmd).await
    } else {
        bail!("manifest or product_bundle must be specified")
    }
}

pub async fn from_path<T: FastbootInterface>(
    messenger: &Sender<Event>,
    path: PathBuf,
    fastboot_interface: &mut T,
    cmd: ManifestParams,
) -> Result<()> {
    log::debug!("fastboot manifest from_path");
    match path.extension() {
        Some(ext) => {
            if ext == "zip" {
                let r = ArchiveResolver::new(path)?;
                load_flash_manifest(r).await?.flash(messenger, fastboot_interface, cmd).await
            } else if ext == "tgz" || ext == "tar.gz" || ext == "tar" {
                let r = FlashManifestTarResolver::new(path)?;
                load_flash_manifest(r).await?.flash(messenger, fastboot_interface, cmd).await
            } else {
                let r = FlashManifestResolver::new(path)?;
                load_flash_manifest(r).await?.flash(messenger, fastboot_interface, cmd).await
            }
        }
        _ => {
            let r = FlashManifestResolver::new(path)?;
            load_flash_manifest(r).await?.flash(messenger, fastboot_interface, cmd).await
        }
    }
}

async fn load_flash_manifest<F: ManifestResolver + FileResolver + Sync>(
    resolver: F,
) -> Result<FlashManifest<impl FileResolver + Sync>> {
    let reader = File::open(resolver.get_manifest_path().await).map(BufReader::new)?;
    Ok(FlashManifest { resolver, version: FlashManifestVersion::load(reader)? })
}

pub struct FlashManifest<F: FileResolver + Sync> {
    resolver: F,
    version: FlashManifestVersion,
}

impl<F: FileResolver + Sync> FlashManifest<F> {
    pub async fn flash<T: FastbootInterface>(
        &mut self,
        messenger: &Sender<Event>,
        fastboot_interface: &mut T,
        cmd: ManifestParams,
    ) -> Result<()> {
        match &cmd.op {
            Command::Flash => {
                self.version.flash(messenger, &mut self.resolver, fastboot_interface, cmd).await
            }
            Command::Unlock(_) => {
                // Using the manifest, don't need the unlock credential from the UnlockCommand
                // here.
                self.version.unlock(messenger, &mut self.resolver, fastboot_interface).await
            }
            Command::Boot(BootParams { slot, .. }) => {
                self.version
                    .boot(
                        messenger.clone(),
                        &mut self.resolver,
                        slot.to_owned(),
                        fastboot_interface,
                        cmd,
                    )
                    .await
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use assembly_partitions_config::{BootloaderPartition, BootstrapPartition, PartitionsConfig};
    use camino::Utf8PathBuf;
    use ffx_flash_manifest::v3::{FlashManifest as FlashManifestV3, Partition};
    use ffx_flash_manifest::ManifestFile;
    use product_bundle::ProductBundleV2;
    use serde_json::from_str;

    const UNKNOWN_VERSION: &'static str = r#"{
        "version": 99999,
        "manifest": "test"
    }"#;

    const MANIFEST: &'static str = r#"{
        "version": 1,
        "manifest": []
    }"#;

    const ARRAY_MANIFEST: &'static str = r#"[{
        "version": 1,
        "manifest": []
    }]"#;

    #[test]
    fn test_deserialization() -> Result<()> {
        let _manifest: ManifestFile = from_str(MANIFEST)?;
        Ok(())
    }

    #[test]
    fn test_serialization() -> Result<()> {
        let manifest = FlashManifestVersion::V3(FlashManifestV3 {
            hw_revision: "board".into(),
            credentials: vec![],
            products: vec![],
        });
        let mut buf = Vec::new();
        manifest.write(&mut buf).unwrap();
        let str = String::from_utf8(buf).unwrap();
        assert_eq!(
            str,
            r#"{
  "manifest": {
    "hw_revision": "board"
  },
  "version": 3
}"#
        );
        Ok(())
    }

    #[test]
    fn test_loading_unknown_version() {
        let manifest_contents = UNKNOWN_VERSION.to_string();
        let result = FlashManifestVersion::load(BufReader::new(manifest_contents.as_bytes()));
        assert!(result.is_err());
    }

    #[fuchsia::test]
    async fn test_loading_version_1() -> Result<()> {
        let manifest_contents = MANIFEST.to_string();
        FlashManifestVersion::load(BufReader::new(manifest_contents.as_bytes())).map(|_| ())
    }

    #[fuchsia::test]
    async fn test_loading_version_1_from_array() -> Result<()> {
        let manifest_contents = ARRAY_MANIFEST.to_string();
        FlashManifestVersion::load(BufReader::new(manifest_contents.as_bytes())).map(|_| ())
    }

    #[test]
    fn test_from_product_bundle_bootstrap_partitions() {
        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![BootstrapPartition {
                    name: "bootstrap_part".into(),
                    condition: None,
                    image: Utf8PathBuf::from("bootstrap_image"),
                }],
                bootloader_partitions: vec![BootloaderPartition {
                    name: Some("bootloader_part".into()),
                    image: Utf8PathBuf::from("bootloader_image"),
                    partition_type: "".into(),
                }],
                partitions: vec![],
                hardware_revision: String::default(),
                unlock_credentials: vec![],
            },
            sdk_version: String::default(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
            release_info: None,
        });
        let manifest = match FlashManifestVersion::from_product_bundle(&pb).unwrap() {
            FlashManifestVersion::V3(manifest) => manifest,
            _ => panic!("Expected a V3 FlashManifest"),
        };
        let bootstrap_product = manifest.products.iter().find(|&p| p.name == "bootstrap").unwrap();
        // The important piece here is that the bootstrap partition comes first.
        assert_eq!(
            bootstrap_product.bootloader_partitions,
            vec![
                Partition {
                    name: "bootstrap_part".into(),
                    path: "bootstrap_image".into(),
                    condition: None
                },
                Partition {
                    name: "bootloader_part".into(),
                    path: "bootloader_image".into(),
                    condition: None
                },
            ]
        )
    }
}
