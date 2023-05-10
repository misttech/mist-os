// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Version 2 of the Product Bundle format.
//!
//! This format is drastically different from Version 1 in that all the contents
//! are expected to stay as implementation detail of ffx. The outputs of
//! assembly are fed directly into the fields of the Product Bundle, and the
//! flash and emulator manifests are not constructed until the Product Bundle
//! is read by `ffx emu start` and `ffx target flash`. This makes the format
//! simpler, and more aligned with how images are assembled.
//!
//! Note on paths
//! -------------
//! PBv2 is a directory containing images and other artifacts necessary to
//! flash, emulator, and update a product. When a Product Bundle is written to
//! disk, the paths inside _must_ all be relative to the Product Bundle itself,
//! to ensure that the directory remains portable (can be moved, zipped, tarred,
//! downloaded on another machine).

use anyhow::{anyhow, Context, Result};
use assembly_manifest::Image;
use assembly_partitions_config::PartitionsConfig;
use camino::{Utf8Path, Utf8PathBuf};
use fidl_fuchsia_developer_ffx::ListFields;
use fuchsia_merkle::Hash;
use fuchsia_repo::{repo_client::RepoClient, repository::FileSystemRepository};
use pathdiff::diff_utf8_paths;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Description of the data needed to set up (flash) a device.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductBundleV2 {
    /// The <product>.<board> for the product bundle.
    pub product_name: String,

    /// Unique version of this <product>.<board>.
    pub product_version: String,

    /// The physical partitions of the target to place images into.
    pub partitions: PartitionsConfig,

    /// Fuchsia SDK version used to build this product bundle.
    pub sdk_version: String,

    /// An assembled system that should be placed in slot A on the target.
    #[serde(default)]
    pub system_a: Option<Vec<Image>>,

    /// An assembled system that should be placed in slot B on the target.
    #[serde(default)]
    pub system_b: Option<Vec<Image>>,

    /// An assembled system that should be placed in slot R on the target.
    #[serde(default)]
    pub system_r: Option<Vec<Image>>,

    /// The repositories that hold the TUF metadata, packages, and blobs.
    #[serde(default)]
    pub repositories: Vec<Repository>,

    /// The merkle-root of the update package that is added to the repository.
    #[serde(default)]
    pub update_package_hash: Option<Hash>,

    /// The relative path from the product bundle base directory to a file
    /// containing the Virtual Device manifest.
    #[serde(default)]
    pub virtual_devices_path: Option<Utf8PathBuf>,
}

/// A repository that holds all the packages, blobs, and keys.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// The unique name of the repository, such as "fuchsia.com".
    pub name: String,

    /// The path to the TUF repository, relative to the product bundle directory.
    pub metadata_path: Utf8PathBuf,

    /// The path to the blobs directory, relative to the product bundle
    /// directory.
    pub blobs_path: Utf8PathBuf,

    /// Specify the delivery blob type of each blob, None means the blobs are
    /// uncompressed and not delivery blobs.
    /// This is used by the repo server, not the product bundle per se.
    /// See //src/storage/blobfs/delivery_blob.h for the list of types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_blob_type: Option<u32>,

    /// The path to the file containing all the root metadata private keys.
    pub root_private_key_path: Option<Utf8PathBuf>,

    /// The path to the file containing all the targets metadata private keys.
    pub targets_private_key_path: Option<Utf8PathBuf>,

    /// The path to the file containing all the snapshot metadata private keys.
    pub snapshot_private_key_path: Option<Utf8PathBuf>,

    /// The path to the file containing all the timestamp metadata private keys.
    pub timestamp_private_key_path: Option<Utf8PathBuf>,
}

impl Repository {
    /// Return the path to the targets.json file that contains a list of all the
    /// packages.
    pub fn targets_path(&self) -> Utf8PathBuf {
        self.metadata_path.join("targets.json")
    }

    /// Return the paths-on-host of the blobs contained in the product bundle.
    pub async fn blobs(&self) -> Result<HashSet<Utf8PathBuf>> {
        let mut all_blobs = HashSet::<Utf8PathBuf>::new();
        let repo = FileSystemRepository::new(self.metadata_path.clone(), self.blobs_path.clone());
        let mut client =
            RepoClient::from_trusted_remote(&repo).await.context("creating the repo client")?;
        client.update().await.context("updating the repo metadata")?;
        let packages =
            client.list_packages(ListFields::empty()).await.context("listing packages")?;
        for package in &packages {
            if let Some(name) = &package.name {
                if let Some(blobs) =
                    client.show_package(name.clone()).await.context("showing package")?
                {
                    all_blobs.extend(blobs.iter().filter_map(|e| e.hash.clone()).map(|p| p.into()));
                }
            }
        }
        Ok(all_blobs)
    }
}

impl ProductBundleV2 {
    /// Convert all the paths from relative to absolute, assuming
    /// `product_bundle_dir` is the current base all the paths are relative to.
    ///
    /// Note: This function is intentionally only accessible inside the crate to
    /// ensure this method is only called during deserialization. Clients should
    /// not be canonicalizing their own paths.
    pub(crate) fn canonicalize_paths(
        &mut self,
        product_bundle_dir: impl AsRef<Utf8Path>,
    ) -> Result<()> {
        let product_bundle_dir = product_bundle_dir.as_ref();

        // Canonicalize the partitions.
        for part in &mut self.partitions.bootstrap_partitions {
            part.image = product_bundle_dir.join(&part.image).canonicalize_utf8()?;
        }
        for part in &mut self.partitions.bootloader_partitions {
            part.image = product_bundle_dir.join(&part.image).canonicalize_utf8()?;
        }
        for cred in &mut self.partitions.unlock_credentials {
            *cred = product_bundle_dir.join(&cred).canonicalize_utf8()?;
        }

        // Canonicalize the systems.
        let canonicalize_system = |system: &mut Option<Vec<Image>>| -> Result<()> {
            if let Some(system) = system {
                for image in system.iter_mut() {
                    image.set_source(product_bundle_dir.join(image.source()).canonicalize_utf8()?);
                }
            }
            Ok(())
        };
        canonicalize_system(&mut self.system_a)?;
        canonicalize_system(&mut self.system_b)?;
        canonicalize_system(&mut self.system_r)?;

        for repository in &mut self.repositories {
            let canonicalize_dir = |path: &Utf8PathBuf| -> Result<Utf8PathBuf> {
                let dir = product_bundle_dir.join(path);
                // Create the directory to ensure that canonicalize will work.
                if !dir.exists() {
                    std::fs::create_dir_all(&dir)
                        .with_context(|| format!("Creating the directory: {}", dir))?;
                }
                let path = dir.canonicalize_utf8()?;
                Ok(path)
            };
            repository.metadata_path = canonicalize_dir(&repository.metadata_path)?;
            repository.blobs_path = canonicalize_dir(&repository.blobs_path)?;
            if let Some(path) = &repository.root_private_key_path {
                repository.root_private_key_path = Some(canonicalize_dir(path)?);
            }
            if let Some(path) = &repository.targets_private_key_path {
                repository.targets_private_key_path = Some(canonicalize_dir(path)?);
            }
            if let Some(path) = &repository.snapshot_private_key_path {
                repository.snapshot_private_key_path = Some(canonicalize_dir(path)?);
            }
            if let Some(path) = &repository.timestamp_private_key_path {
                repository.timestamp_private_key_path = Some(canonicalize_dir(path)?);
            }
        }

        // Canonicalize the virtual device specifications path.
        if let Some(path) = &self.virtual_devices_path {
            let virtual_devices_path = product_bundle_dir.join(path);
            let dir = virtual_devices_path
                .parent()
                .ok_or(anyhow!("No parent: {}", virtual_devices_path))?;
            let base = virtual_devices_path
                .file_name()
                .ok_or(anyhow!("No file name: {}", virtual_devices_path))?;

            // Create the directory to ensure that canonicalize will work.
            if !dir.exists() {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("Creating the directory: {}", dir))?;
            }
            // Only canonicalize the directory.
            // This prevents problems when virtual_devices_path is a symlink.
            self.virtual_devices_path = Some(dir.canonicalize_utf8()?.join(base));
        }

        Ok(())
    }

    /// Convert all the paths from absolute to relative, assuming
    /// `product_bundle_dir` is the new base all the paths should be relative
    /// to.
    ///
    /// Note: This function is intentionally only accessible inside the crate to
    /// ensure this method is only called during deserialization. Clients should
    /// not be canonicalizing their own paths.
    pub(crate) fn relativize_paths(
        &mut self,
        product_bundle_dir: impl AsRef<Utf8Path>,
    ) -> Result<()> {
        let product_bundle_dir = product_bundle_dir.as_ref();

        // Relativize the partitions.
        for part in &mut self.partitions.bootstrap_partitions {
            part.image =
                diff_utf8_paths(&part.image, &product_bundle_dir).context("rebasing file path")?;
        }
        for part in &mut self.partitions.bootloader_partitions {
            part.image =
                diff_utf8_paths(&part.image, &product_bundle_dir).context("rebasing file path")?;
        }
        for cred in &mut self.partitions.unlock_credentials {
            *cred = diff_utf8_paths(&cred, &product_bundle_dir).context("rebasing file path")?;
        }

        // Relativize the systems.
        let relativize_system = |system: &mut Option<Vec<Image>>| -> Result<()> {
            if let Some(system) = system {
                for image in system.iter_mut() {
                    let path = diff_utf8_paths(&image.source(), &product_bundle_dir)
                        .ok_or(anyhow!("failed to rebase the file"))?;
                    image.set_source(path);
                }
            }
            Ok(())
        };
        relativize_system(&mut self.system_a)?;
        relativize_system(&mut self.system_b)?;
        relativize_system(&mut self.system_r)?;

        for repository in &mut self.repositories {
            let relativize_dir = |path: &Utf8PathBuf| -> Result<Utf8PathBuf> {
                diff_utf8_paths(&path, &product_bundle_dir).context("rebasing repository")
            };
            repository.metadata_path = relativize_dir(&repository.metadata_path)?;
            repository.blobs_path = relativize_dir(&repository.blobs_path)?;
            if let Some(path) = &repository.root_private_key_path {
                repository.root_private_key_path = Some(relativize_dir(path)?);
            }
            if let Some(path) = &repository.targets_private_key_path {
                repository.targets_private_key_path = Some(relativize_dir(path)?);
            }
            if let Some(path) = &repository.snapshot_private_key_path {
                repository.snapshot_private_key_path = Some(relativize_dir(path)?);
            }
            if let Some(path) = &repository.timestamp_private_key_path {
                repository.timestamp_private_key_path = Some(relativize_dir(path)?);
            }
        }

        // Relativize the virtual device specifications.
        if let Some(path) = &self.virtual_devices_path {
            self.virtual_devices_path =
                Some(diff_utf8_paths(&path, &product_bundle_dir).context("rebasing file path")?);
        }

        Ok(())
    }

    /// Return the path to the Virtual Device Manifest.
    pub fn get_virtual_devices_path(&self) -> Option<Utf8PathBuf> {
        self.virtual_devices_path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_manifest::Image;
    use assembly_partitions_config::{BootloaderPartition, BootstrapPartition, Partition, Slot};
    use camino::Utf8Path;
    use fuchsia_repo::test_utils;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_canonicalize_no_paths() {
        let mut pb = ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![],
                bootloader_partitions: vec![],
                partitions: vec![],
                hardware_revision: "board".into(),
                unlock_credentials: vec![],
            },
            sdk_version: "".to_string(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        let result = pb.canonicalize_paths(&Utf8PathBuf::from("path/to/product_bundle"));
        assert!(result.is_ok(), "{:?}", result.unwrap_err());
    }

    #[test]
    fn test_canonicalize_with_paths() {
        let tmp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(tmp.path()).unwrap();

        let create_temp_file = |name: &str| {
            let path = tempdir.join(name);
            let mut file = File::create(path).unwrap();
            write!(file, "{}", name).unwrap();
        };

        // These files must exist for canonicalize() to work.
        create_temp_file("bootstrap");
        create_temp_file("bootloader");
        create_temp_file("zbi");
        create_temp_file("vbmeta");
        create_temp_file("fvm");
        create_temp_file("unlock_credentials");
        create_temp_file("device");

        let mut pb = ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![BootstrapPartition {
                    name: "bootstrap".into(),
                    image: "bootstrap".into(),
                    condition: None,
                }],
                bootloader_partitions: vec![BootloaderPartition {
                    partition_type: "bl2".into(),
                    name: None,
                    image: "bootloader".into(),
                }],
                partitions: vec![
                    Partition::ZBI { name: "zbi".into(), slot: Slot::A },
                    Partition::VBMeta { name: "vbmeta".into(), slot: Slot::A },
                    Partition::FVM { name: "fvm".into() },
                ],
                hardware_revision: "board".into(),
                unlock_credentials: vec!["unlock_credentials".into()],
            },
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: "zbi".into(), signed: false },
                Image::VBMeta("vbmeta".into()),
                Image::FVM("fvm".into()),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: Some("device".into()),
        };
        let result = pb.canonicalize_paths(tempdir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_relativize_no_paths() {
        let mut pb = ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![],
                bootloader_partitions: vec![],
                partitions: vec![],
                hardware_revision: "board".into(),
                unlock_credentials: vec![],
            },
            sdk_version: "".to_string(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        let result = pb.relativize_paths(&Utf8PathBuf::from("path/to/product_bundle"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_relativize_with_paths() {
        let tmp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(tmp.path()).unwrap();
        let vd_manifest = tempdir.join("virtual_devices.json");
        File::create(&vd_manifest).expect("Create vd manifest file");

        let mut pb = ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig {
                bootstrap_partitions: vec![BootstrapPartition {
                    name: "bootstrap".into(),
                    image: tempdir.join("bootstrap"),
                    condition: None,
                }],
                bootloader_partitions: vec![BootloaderPartition {
                    partition_type: "bl2".into(),
                    name: None,
                    image: tempdir.join("bootloader"),
                }],
                partitions: vec![
                    Partition::ZBI { name: "zbi".into(), slot: Slot::A },
                    Partition::VBMeta { name: "vbmeta".into(), slot: Slot::A },
                    Partition::FVM { name: "fvm".into() },
                ],
                hardware_revision: "board".into(),
                unlock_credentials: vec![tempdir.join("unlock_credentials")],
            },
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: tempdir.join("zbi"), signed: false },
                Image::VBMeta(tempdir.join("vbmeta")),
                Image::FVM(tempdir.join("fvm")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: Some(vd_manifest),
        };
        let result = pb.relativize_paths(tempdir);
        assert!(result.is_ok());
    }

    #[fuchsia::test]
    async fn test_blobs() {
        let tempdir = TempDir::new().unwrap();
        let dir = Utf8Path::from_path(&tempdir.path()).unwrap();
        let _repo = test_utils::make_pm_repo_dir(&tempdir.path()).await;

        let pb = ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![Repository {
                name: "fuchsia.com".into(),
                metadata_path: dir.join("repository"),
                blobs_path: dir.join("repository").join("blobs"),
                delivery_blob_type: None,
                root_private_key_path: None,
                targets_private_key_path: None,
                snapshot_private_key_path: None,
                timestamp_private_key_path: None,
            }],
            update_package_hash: None,
            virtual_devices_path: None,
        };

        let expected = HashSet::from([
            "050907f009ff634f9aa57bff541fb9e9c2c62b587c23578e77637cda3bd69458".into(),
            "2881455493b5870aaea36537d70a2adc635f516ac2092598f4b6056dabc6b25d".into(),
            "548981eb310ddc4098fb5c63692e19ac4ae287b13d0e911fbd9f7819ac22491c".into(),
            "72e1e7a504f32edf4f23e7e8a3542c1d77d12541142261cfe272decfa75f542d".into(),
            "8a8a5f07f935a4e8e1fd1a1eda39da09bb2438ec0adfb149679ddd6e7e1fbb4f".into(),
            "ecc11f7f4b763c5a21be2b4159c9818bbe22ca7e6d8100a72f6a41d3d7b827a9".into(),
        ]);

        let blobs = pb.repositories[0].blobs().await.unwrap();
        assert_eq!(expected, blobs);
    }

    #[test]
    fn test_targets_path() {
        let pb = ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig::default(),
            sdk_version: String::default(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![Repository {
                name: "fuchsia.com".into(),
                metadata_path: "repository".into(),
                blobs_path: "blobs".into(),
                delivery_blob_type: None,
                root_private_key_path: None,
                targets_private_key_path: None,
                snapshot_private_key_path: None,
                timestamp_private_key_path: None,
            }],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        assert_eq!("repository/targets.json", pb.repositories[0].targets_path());
    }
}
