// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for constructing product bundles, which are distributable containers for a product's
//! images and packages, and can be used to emulate, flash, or update a product.

use anyhow::{Context, Result};
use assembled_system::AssembledSystem;
use assembly_container::AssemblyContainer;
use assembly_partitions_config::{PartitionsConfig, Slot as PartitionSlot};
use assembly_sdk::SdkToolProvider;
use assembly_tool::ToolProvider;
use ffx_config::sdk::{in_tree_sdk_version, SdkVersion};
use ffx_config::EnvironmentContext;
use ffx_flash_manifest::FlashManifestVersion;
use ffx_product_create_old_args::CreateCommand;
use ffx_writer::SimpleWriter;
use fho::{return_bug, FfxMain, FfxTool};
use product_bundle::ProductBundleBuilder;
use sdk_metadata::VirtualDevice;
use std::fs::File;

/// Default delivery blob type to use for products.
const DEFAULT_DELIVERY_BLOB_TYPE: u32 = 1;

#[derive(FfxTool)]
pub struct ProductCreateTool {
    #[command]
    pub cmd: CreateCommand,
    ctx: EnvironmentContext,
}

fho::embedded_plugin!(ProductCreateTool);

/// Create a product bundle.
#[async_trait::async_trait(?Send)]
impl FfxMain for ProductCreateTool {
    type Writer = SimpleWriter;
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let sdk = self.ctx.get_sdk().context("getting sdk env context")?;
        let sdk_version = match sdk.get_version() {
            SdkVersion::Version(version) => version.to_string(),
            SdkVersion::InTree => in_tree_sdk_version(),
            SdkVersion::Unknown => return_bug!("Unable to determine SDK version"),
        };
        let tools = SdkToolProvider::try_new()?;
        pb_create_with_sdk_version(self.cmd, &sdk_version, Box::new(tools))
            .await
            .map_err(Into::into)
    }
}

/// Create a product bundle using the provided sdk.
pub async fn pb_create_with_sdk_version(
    cmd: CreateCommand,
    sdk_version: &str,
    tools: Box<dyn ToolProvider>,
) -> Result<()> {
    // We build an update package if `update_version_file` or `update_epoch` is provided.
    // If we decide to build an update package, we need to ensure that both of them
    // are provided.
    let update_details =
        if cmd.update_package_version_file.is_some() || cmd.update_package_epoch.is_some() {
            if cmd.tuf_keys.is_none() {
                anyhow::bail!("TUF keys must be provided to build an update package");
            }
            let version = cmd.update_package_version_file.clone().ok_or_else(|| {
                anyhow::anyhow!("A version file must be provided to build an update package")
            })?;
            let epoch = cmd.update_package_epoch.ok_or_else(|| {
                anyhow::anyhow!("A epoch must be provided to build an update package")
            })?;
            Some((version, epoch))
        } else {
            None
        };

    // Build a product bundle.
    let mut pb_builder =
        ProductBundleBuilder::new(cmd.product_name.clone(), cmd.product_version.clone())
            .sdk_version(sdk_version.to_string());
    if let Some(path) = &cmd.partitions {
        let partitions = PartitionsConfig::from_dir(&path)
            .with_context(|| format!("Parsing partitions config: {}", &path))?;
        pb_builder = pb_builder.partitions(partitions);
    }
    if let Some(system_path) = &cmd.system_a {
        let system = AssembledSystem::from_dir(system_path)?;
        pb_builder = pb_builder.system(system, PartitionSlot::A);
    }
    if let Some(system_path) = &cmd.system_b {
        let system = AssembledSystem::from_dir(system_path)?;
        pb_builder = pb_builder.system(system, PartitionSlot::B);
    }
    if let Some(system_path) = &cmd.system_r {
        let system = AssembledSystem::from_dir(system_path)?;
        pb_builder = pb_builder.system(system, PartitionSlot::R);
    }
    if let Some((version, epoch)) = &update_details {
        pb_builder = pb_builder.update_package(version, *epoch);
    }
    if let Some(tuf_keys) = &cmd.tuf_keys {
        let delivery_blob_type =
            cmd.delivery_blob_type.unwrap_or(DEFAULT_DELIVERY_BLOB_TYPE).try_into()?;
        pb_builder = pb_builder.repository(delivery_blob_type, tuf_keys);
    }
    for path in &cmd.virtual_device {
        let device = VirtualDevice::try_load_from(&path)
            .with_context(|| format!("Parsing file as virtual device: '{}'", path))?;
        let file_name =
            path.file_name().unwrap_or_else(|| panic!("Path has no file name: '{}'", path));
        pb_builder = pb_builder.virtual_device(file_name, device);
    }
    if let Some(recommended_device) = &cmd.recommended_device {
        pb_builder = pb_builder.recommended_virtual_device(recommended_device.clone());
    }
    if let Some(gerrit_size_report) = &cmd.gerrit_size_report {
        pb_builder = pb_builder.gerrit_size_report(gerrit_size_report);
    }
    let product_bundle =
        pb_builder.build(tools, &cmd.out_dir).await.context("Building the product bundle")?;

    if cmd.with_deprecated_flash_manifest {
        let manifest_path = cmd.out_dir.join("flash.json");
        let flash_manifest_file = File::create(&manifest_path)
            .with_context(|| format!("Couldn't create flash.json '{}'", manifest_path))?;
        FlashManifestVersion::from_product_bundle(&product_bundle)?.write(flash_manifest_file)?
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use assembled_system::Image;
    use assembly_release_info::{ProductBundleReleaseInfo, SystemReleaseInfo};
    use assembly_tool::testing::{blobfs_side_effect, FakeToolProvider};
    use camino::{Utf8Path, Utf8PathBuf};
    use fuchsia_repo::test_utils;
    use product_bundle::{ProductBundle, ProductBundleV2, Repository};
    use sdk_metadata::{VirtualDeviceManifest, VirtualDeviceV1};
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    #[fuchsia::test]
    async fn test_pb_create_minimal() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: Some(partitions_dir),
                system_a: None,
                system_b: None,
                system_r: None,
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: None,
                system_b: None,
                system_r: None,
                repositories: vec![],
                update_package_hash: None,
                virtual_devices_path: None,
                release_info: Some(ProductBundleReleaseInfo {
                    name: String::default(),
                    version: String::default(),
                    sdk_version: String::default(),
                    system_a: None,
                    system_b: None,
                    system_r: None
                })
            })
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_a_and_r() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let system_dir = tempdir.join("system");
        fs::create_dir(&system_dir).unwrap();
        AssembledSystem {
            images: Default::default(),
            board_name: "my_board".into(),
            partitions_config: None,
            system_release_info: SystemReleaseInfo::new_for_testing(),
        }
        .write_to_dir(&system_dir, None::<Utf8PathBuf>)
        .unwrap();

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: Some(partitions_dir),
                system_a: Some(system_dir.clone()),
                system_b: None,
                system_r: Some(system_dir.clone()),
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: Some(vec![]),
                system_b: None,
                system_r: Some(vec![]),
                repositories: vec![],
                update_package_hash: None,
                virtual_devices_path: None,
                release_info: Some(ProductBundleReleaseInfo {
                    name: String::default(),
                    version: String::default(),
                    sdk_version: String::default(),
                    system_a: Some(SystemReleaseInfo::new_for_testing()),
                    system_b: None,
                    system_r: Some(SystemReleaseInfo::new_for_testing()),
                }),
            })
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_a_and_r_with_multiple_zbi() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let system_dir = tempdir.join("system");
        fs::create_dir(&system_dir).unwrap();
        let mut manifest = AssembledSystem {
            images: Default::default(),
            board_name: "my_board".into(),
            partitions_config: None,
            system_release_info: SystemReleaseInfo::new_for_testing(),
        };
        manifest.images = vec![
            Image::ZBI { path: tempdir.join("path1"), signed: false },
            Image::ZBI { path: tempdir.join("path2"), signed: true },
        ];
        std::fs::write(&tempdir.join("path1"), "").unwrap();
        std::fs::write(&tempdir.join("path2"), "").unwrap();
        manifest.write_to_dir(&system_dir, None::<Utf8PathBuf>).unwrap();

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        assert!(pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: Some(partitions_dir),
                system_a: Some(system_dir.clone()),
                system_b: None,
                system_r: Some(system_dir.clone()),
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .is_err());
    }

    #[fuchsia::test]
    async fn test_pb_create_a_and_r_and_repository() {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap().canonicalize_utf8().unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let system_dir = tempdir.join("system");
        fs::create_dir(&system_dir).unwrap();
        AssembledSystem {
            images: Default::default(),
            board_name: "my_board".into(),
            partitions_config: None,
            system_release_info: SystemReleaseInfo::new_for_testing(),
        }
        .write_to_dir(&system_dir, None::<Utf8PathBuf>)
        .unwrap();

        let tuf_keys = tempdir.join("keys");
        test_utils::make_repo_keys_dir(&tuf_keys);

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: Some(partitions_dir),
                system_a: Some(system_dir.clone()),
                system_b: None,
                system_r: Some(system_dir.clone()),
                tuf_keys: Some(tuf_keys),
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: Some(1),
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(&pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: Some(vec![]),
                system_b: None,
                system_r: Some(vec![]),
                repositories: vec![Repository {
                    name: "fuchsia.com".into(),
                    metadata_path: pb_dir.join("repository"),
                    blobs_path: pb_dir.join("blobs"),
                    delivery_blob_type: 1,
                    root_private_key_path: Some(pb_dir.join("keys/root.json")),
                    targets_private_key_path: Some(pb_dir.join("keys/targets.json")),
                    snapshot_private_key_path: Some(pb_dir.join("keys/snapshot.json")),
                    timestamp_private_key_path: Some(pb_dir.join("keys/timestamp.json")),
                }],
                update_package_hash: None,
                virtual_devices_path: None,
                release_info: Some(ProductBundleReleaseInfo {
                    name: String::default(),
                    version: String::default(),
                    sdk_version: String::default(),
                    system_a: Some(SystemReleaseInfo::new_for_testing()),
                    system_b: None,
                    system_r: Some(SystemReleaseInfo::new_for_testing()),
                }),
            })
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_with_update() {
        let tmp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(tmp.path()).unwrap().canonicalize_utf8().unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path).unwrap();
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default()).unwrap();

        let version_path = tempdir.join("version.txt");
        std::fs::write(&version_path, "").unwrap();

        let tuf_keys = tempdir.join("keys");
        test_utils::make_repo_keys_dir(&tuf_keys);

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: Some(partitions_dir),
                system_a: None,
                system_b: None,
                system_r: None,
                tuf_keys: Some(tuf_keys),
                update_package_version_file: Some(version_path),
                update_package_epoch: Some(1),
                virtual_device: vec![],
                recommended_device: None,
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: false,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(&pb_dir).unwrap();
        // NB: do not assert on the package hash because this test is not hermetic; platform
        // changes such as API level bumps may change the package hash and such changes are
        // immaterial to the code under test here.
        assert_matches::assert_matches!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: _,
                product_version: _,
                partitions,
                sdk_version: _,
                system_a: None,
                system_b: None,
                system_r: None,
                repositories,
                update_package_hash: Some(_),
                virtual_devices_path: None,
                release_info: Some(_)
            }) if partitions == Default::default() && repositories == &[Repository {
                name: "fuchsia.com".into(),
                metadata_path: pb_dir.join("repository"),
                blobs_path: pb_dir.join("blobs"),
                delivery_blob_type: DEFAULT_DELIVERY_BLOB_TYPE,
                root_private_key_path: Some(pb_dir.join("keys/root.json")),
                targets_private_key_path: Some(pb_dir.join("keys/targets.json")),
                snapshot_private_key_path: Some(pb_dir.join("keys/snapshot.json")),
                timestamp_private_key_path: Some(pb_dir.join("keys/timestamp.json")),
            }]
        );
    }

    #[fuchsia::test]
    async fn test_pb_create_with_virtual_devices() -> Result<()> {
        let temp = TempDir::new().unwrap();
        let tempdir = Utf8Path::from_path(temp.path()).unwrap().canonicalize_utf8().unwrap();
        let pb_dir = tempdir.join("pb");

        let partitions_dir = tempdir.join("partitions");
        fs::create_dir(&partitions_dir).unwrap();
        let partitions_path = partitions_dir.join("partitions_config.json");
        let partitions_file = File::create(&partitions_path)?;
        serde_json::to_writer(&partitions_file, &PartitionsConfig::default())?;

        let vd_path1 = tempdir.join("device_1.json");
        let vd_path2 = tempdir.join("device_2.json");
        let mut vd_file1 = File::create(&vd_path1)?;
        let mut vd_file2 = File::create(&vd_path2)?;
        File::create(tempdir.join("device.json.template"))?;
        const DEVICE_1: &str =
            include_str!("../../../../../../../build/sdk/meta/test_data/virtual_device.json");
        const DEVICE_2: &str =
            include_str!("../../../../../../../build/sdk/meta/test_data/virtual_device2.json");
        vd_file1.write_all(DEVICE_1.as_bytes())?;
        vd_file2.write_all(DEVICE_2.as_bytes())?;

        let tool_provider = Box::new(FakeToolProvider::new_with_side_effect(blobfs_side_effect));

        pb_create_with_sdk_version(
            CreateCommand {
                product_name: String::default(),
                product_version: String::default(),
                partitions: Some(partitions_dir),
                system_a: None,
                system_b: None,
                system_r: None,
                tuf_keys: None,
                update_package_version_file: None,
                update_package_epoch: None,
                virtual_device: vec![vd_path1, vd_path2],
                recommended_device: Some("device2".to_string()),
                out_dir: pb_dir.clone(),
                delivery_blob_type: None,
                with_deprecated_flash_manifest: true,
                gerrit_size_report: None,
            },
            /*sdk_version=*/ "",
            tool_provider,
        )
        .await
        .unwrap();

        let pb = ProductBundle::try_load_from(&pb_dir).unwrap();
        assert_eq!(
            pb,
            ProductBundle::V2(ProductBundleV2 {
                product_name: String::default(),
                product_version: String::default(),
                partitions: PartitionsConfig::default(),
                sdk_version: String::default(),
                system_a: None,
                system_b: None,
                system_r: None,
                repositories: vec![],
                update_package_hash: None,
                virtual_devices_path: Some(pb_dir.join("virtual_devices/manifest.json")),
                release_info: Some(ProductBundleReleaseInfo {
                    name: String::default(),
                    version: String::default(),
                    sdk_version: String::default(),
                    system_a: None,
                    system_b: None,
                    system_r: None
                }),
            })
        );

        let internal_pb = match pb {
            ProductBundle::V2(pb) => pb,
        };

        let path = internal_pb.get_virtual_devices_path();
        let manifest =
            VirtualDeviceManifest::from_path(&path).context("Manifest file from_path")?;
        let default = manifest.default_device();
        assert!(matches!(default, Ok(Some(VirtualDevice::V1(_)))), "{:?}", default);

        let devices = manifest.device_names();
        assert_eq!(devices.len(), 2);
        assert!(devices.contains(&"device".to_string()));
        assert!(devices.contains(&"device2".to_string()));

        let device1 = manifest.device("device");
        assert!(device1.is_ok(), "{:?}", device1.unwrap_err());
        assert!(matches!(device1, Ok(VirtualDevice::V1(VirtualDeviceV1 { .. }))));

        let device2 = manifest.device("device2");
        assert!(device2.is_ok(), "{:?}", device2.unwrap_err());
        assert!(matches!(device2, Ok(VirtualDevice::V1(VirtualDeviceV1 { .. }))));

        Ok(())
    }
}
