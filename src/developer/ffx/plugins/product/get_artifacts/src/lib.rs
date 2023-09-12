// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for the paths of a group of artifacts inside product bundle.

use anyhow::{Context, Result};
use assembly_manifest::Image;
use camino::{Utf8Path, Utf8PathBuf};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_product_get_artifacts_args::GetArtifactsCommand;
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx_ext::RepositoryConfig;
use sdk_metadata::{ProductBundle, Type};
use std::io::Write;
use utf8_path::path_relative_from;

/// This plugin will get the paths of a group of artifacts from
/// the product bundle. This group can be used for flashing,
/// emulator or updating the device depends on the
/// artifact_group parameter passed in.
#[ffx_plugin("ffx_product_get_artifacts")]
pub async fn pb_get_artifacts(
    cmd: GetArtifactsCommand,
    #[ffx(machine = Vec<RepositoryConfig>)] mut writer: Writer,
) -> Result<()> {
    let product_bundle = ProductBundle::try_load_from(&cmd.product_bundle)
        .context("Failed to load product bundle")?;
    let artifacts = match cmd.artifacts_group {
        Type::Flash => extract_flashing_artifacts(product_bundle, cmd)?,
        Type::Bootloader => extract_bootloaders(product_bundle, cmd)?,
        Type::Emu => extract_emu_artifacts(product_bundle, cmd)?,
        _ => ffx_bail!("OTA artifacts is not supported as of now"),
    };
    let artifact_string = artifacts.join("\n");
    if writer.is_machine() {
        writer.machine(&artifact_string)?;
    } else {
        writeln!(writer, "{}", artifact_string)?;
    }
    Ok(())
}

/// Extract bootloaders will list all the bootloaders used by this product. It
/// will list the bootloader type along with the path. Example output:
///
///   firmware_b12:path/to/bootloader
///   firmware:path/to/bootloader2
///
fn extract_bootloaders(
    product_bundle: ProductBundle,
    cmd: GetArtifactsCommand,
) -> Result<Vec<String>> {
    let mut product_bundle = match product_bundle {
        ProductBundle::V1(_) => ffx_bail!("Only v2 product bundles are supported"),
        ProductBundle::V2(pb) => pb,
    };

    let compute_path = |path: &Utf8Path| -> Result<Utf8PathBuf> {
        if cmd.relative_path {
            path_relative_from(path, &cmd.product_bundle)
        } else {
            Ok(path.clone().into())
        }
    };

    let mut artifacts = Vec::new();
    for part in &mut product_bundle.partitions.bootloader_partitions {
        let path = compute_path(&part.image)?;
        let bootloader_string = if part.partition_type == "" {
            format!("firmware:{}", &path)
        } else {
            format!("firmware_{}:{}", part.partition_type, &path)
        };

        artifacts.push(bootloader_string);
    }
    Ok(artifacts)
}

fn extract_flashing_artifacts(
    product_bundle: ProductBundle,
    cmd: GetArtifactsCommand,
) -> Result<Vec<String>> {
    let mut product_bundle = match product_bundle {
        ProductBundle::V1(_) => ffx_bail!("Only v2 product bundles are supported"),
        ProductBundle::V2(pb) => pb,
    };

    let compute_path = |path: &Utf8Path| -> Result<Utf8PathBuf> {
        if cmd.relative_path {
            path_relative_from(path, &cmd.product_bundle)
        } else {
            Ok(path.clone().into())
        }
    };

    let mut artifacts = vec![Utf8PathBuf::from("product_bundle.json")];
    for part in &mut product_bundle.partitions.bootstrap_partitions {
        artifacts.push(compute_path(&part.image)?);
    }
    for part in &mut product_bundle.partitions.bootloader_partitions {
        artifacts.push(compute_path(&part.image)?);
    }
    for cred in &mut product_bundle.partitions.unlock_credentials {
        artifacts.push(compute_path(&cred)?);
    }

    // Collect the systems artifacts.
    let mut collect_system_artifacts = |system: &mut Option<Vec<Image>>| -> Result<()> {
        if let Some(system) = system {
            for image in system.iter_mut() {
                match image {
                    Image::ZBI { path: _, signed: _ }
                    | Image::VBMeta(_)
                    | Image::FVMFastboot(_) => artifacts.push(compute_path(&image.source())?),
                    _ => continue,
                }
            }
        }
        Ok(())
    };
    collect_system_artifacts(&mut product_bundle.system_a)?;
    collect_system_artifacts(&mut product_bundle.system_b)?;
    collect_system_artifacts(&mut product_bundle.system_r)?;
    Ok(artifacts.iter().map(|x| x.to_string()).collect::<Vec<_>>())
}

fn extract_emu_artifacts(
    product_bundle: ProductBundle,
    cmd: GetArtifactsCommand,
) -> Result<Vec<String>> {
    let mut product_bundle = match product_bundle {
        ProductBundle::V1(_) => ffx_bail!("Only v2 product bundles are supported"),
        ProductBundle::V2(pb) => pb,
    };

    let compute_path = |path: &Utf8Path| -> Result<Utf8PathBuf> {
        if cmd.relative_path {
            path_relative_from(path, &cmd.product_bundle)
        } else {
            Ok(path.clone().into())
        }
    };

    let mut artifacts = vec![Utf8PathBuf::from("product_bundle.json")];

    // Collect the systems artifacts.
    let mut collect_system_artifacts = |system: &mut Option<Vec<Image>>| -> Result<()> {
        if let Some(system) = system {
            for image in system.iter_mut() {
                match image {
                    Image::ZBI { path: _, signed: _ } | Image::QemuKernel(_) | Image::FVM(_) => {
                        artifacts.push(compute_path(&image.source())?)
                    }
                    _ => continue,
                }
            }
        }
        Ok(())
    };

    collect_system_artifacts(&mut product_bundle.system_a)?;
    collect_system_artifacts(&mut product_bundle.system_b)?;
    collect_system_artifacts(&mut product_bundle.system_r)?;

    if let Some(path) = product_bundle.virtual_devices_path {
        artifacts.push(compute_path(&path)?);
    }
    Ok(artifacts.iter().map(|x| x.to_string()).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_partitions_config::PartitionsConfig;
    use sdk_metadata::ProductBundleV2;

    #[test]
    fn test_get_flashing_artifacts() {
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "tpl",
                        name: "firmware_tpl",
                        image: "bootloader/path",
                    }
                ],
                partitions: [
                    {
                        type: "ZBI",
                        name: "zircon_a",
                        slot: "A",
                    },
                    {
                        type: "VBMeta",
                        name: "vbmeta_b",
                        slot: "B",
                    },
                    {
                        type: "FVM",
                        name: "fvm",
                    },
                    {
                        type: "Fxfs",
                        name: "fxfs",
                    },
                ],
                hardware_revision: "hw",
                unlock_credentials: [
                    "credential/path",
                ],
            }
        "#;
        let mut cursor = std::io::Cursor::new(json);
        let config: PartitionsConfig = PartitionsConfig::from_reader(&mut cursor).unwrap();

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: config,
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: Utf8PathBuf::from("zbi/path"), signed: false },
                Image::FVM(Utf8PathBuf::from("/tmp/product_bundle/system_a/fvm.blk")),
                Image::FVMFastboot(Utf8PathBuf::from(
                    "/tmp/product_bundle/system_a/fvm_fastboot.blk",
                )),
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let cmd = GetArtifactsCommand {
            product_bundle: Utf8PathBuf::new(),
            relative_path: false,
            artifacts_group: Type::Flash,
        };
        let artifacts = extract_flashing_artifacts(pb.clone(), cmd).unwrap();
        let expected_artifacts = vec![
            String::from("product_bundle.json"),
            String::from("bootloader/path"),
            String::from("credential/path"),
            String::from("zbi/path"),
            String::from("/tmp/product_bundle/system_a/fvm_fastboot.blk"),
        ];
        assert_eq!(expected_artifacts, artifacts);
    }

    #[test]
    fn test_get_emu_artifacts() {
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "tpl",
                        name: "firmware_tpl",
                        image: "bootloader/path",
                    }
                ],
                partitions: [
                    {
                        type: "ZBI",
                        name: "zircon_a",
                        slot: "A",
                    },
                    {
                        type: "VBMeta",
                        name: "vbmeta_b",
                        slot: "B",
                    },
                    {
                        type: "FVM",
                        name: "fvm",
                    },
                    {
                        type: "Fxfs",
                        name: "fxfs",
                    },
                ],
                hardware_revision: "hw",
                unlock_credentials: [
                    "credential/path",
                ],
            }
        "#;
        let mut cursor = std::io::Cursor::new(json);
        let config: PartitionsConfig = PartitionsConfig::from_reader(&mut cursor).unwrap();

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: config,
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: Utf8PathBuf::from("zbi/path"), signed: false },
                Image::FVM(Utf8PathBuf::from("/tmp/product_bundle/system_a/fvm.blk")),
                Image::FVMFastboot(Utf8PathBuf::from(
                    "/tmp/product_bundle/system_a/fvm_fastboot.blk",
                )),
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let cmd = GetArtifactsCommand {
            product_bundle: Utf8PathBuf::new(),
            relative_path: false,
            artifacts_group: Type::Flash,
        };
        let artifacts = extract_emu_artifacts(pb.clone(), cmd).unwrap();
        let expected_artifacts = vec![
            String::from("product_bundle.json"),
            String::from("zbi/path"),
            String::from("/tmp/product_bundle/system_a/fvm.blk"),
            String::from("qemu/path"),
        ];
        assert_eq!(expected_artifacts, artifacts);
    }

    #[test]
    fn test_get_bootloaders() {
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "",
                        name: "firmware_tpl",
                        image: "bootloader/path",
                    },
                    {
                        type: "bl2",
                        name: "firmware_tpl",
                        image: "bootloader/path2",
                    }
                ],
                partitions: [],
                hardware_revision: "hw",
                unlock_credentials: [
                    "credential/path",
                ],
            }
        "#;
        let mut cursor = std::io::Cursor::new(json);
        let config: PartitionsConfig = PartitionsConfig::from_reader(&mut cursor).unwrap();

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: config,
            sdk_version: "".to_string(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let cmd = GetArtifactsCommand {
            product_bundle: Utf8PathBuf::new(),
            relative_path: false,
            artifacts_group: Type::Bootloader,
        };
        let artifacts = extract_bootloaders(pb.clone(), cmd).unwrap();
        let expected_artifacts = vec![
            String::from("firmware:bootloader/path"),
            String::from("firmware_bl2:bootloader/path2"),
        ];
        assert_eq!(expected_artifacts, artifacts);
    }
}
