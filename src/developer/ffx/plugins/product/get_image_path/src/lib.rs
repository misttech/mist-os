// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for get a path of the image inside product bundle.

use anyhow::{anyhow, Context, Result};
use assembly_manifest::Image;
use camino::{Utf8Path, Utf8PathBuf};
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_product_get_image_path_args::{GetImagePathCommand, ImageType, Slot};
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx_ext::RepositoryConfig;
use sdk_metadata::ProductBundle;
use std::io::Write;
use utf8_path::path_relative_from;

/// This plugin will get the path of image from the product bundle, based on the slot and image_type passed in.
#[ffx_plugin("ffx_product_get_image_path")]
pub async fn pb_get_image_path(
    cmd: GetImagePathCommand,
    #[ffx(machine = Vec<RepositoryConfig>)] mut writer: Writer,
) -> Result<()> {
    let product_bundle = ProductBundle::try_load_from(&cmd.product_bundle)
        .context("Failed to load product bundle")?;
    let path = extract_image_path(product_bundle, cmd)?;
    if writer.is_machine() {
        writer.machine(&path)?;
    } else {
        writeln!(writer, "{}", path.ok_or(anyhow!("Cannot find corresponding image"))?)?;
    }
    Ok(())
}

fn extract_image_path(
    product_bundle: ProductBundle,
    cmd: GetImagePathCommand,
) -> Result<Option<Utf8PathBuf>> {
    let product_bundle = match product_bundle {
        ProductBundle::V1(_) => ffx_bail!("Only v2 product bundles are supported"),
        ProductBundle::V2(pb) => pb,
    };

    if cmd.bootloader.is_some() && cmd.slot.is_some() {
        ffx_bail!("Cannot pass in both --bootloader and --slot parameters");
    }

    let compute_path = |path: &Utf8Path| -> Result<Utf8PathBuf> {
        if cmd.relative_path {
            path_relative_from(path, &cmd.product_bundle)
        } else {
            Ok(path.clone().into())
        }
    };

    if let Some(bootloader) = cmd.bootloader {
        let bootloader_type = bootloader
            .strip_prefix("firmware")
            .ok_or(anyhow!("Bootloader name must begin with 'firmware'"))?
            .trim_start_matches("_");
        return product_bundle
            .partitions
            .bootloader_partitions
            .iter()
            .find_map(|b| {
                if b.partition_type == bootloader_type {
                    Some(compute_path(&b.image))
                } else {
                    None
                }
            })
            .transpose();
    }

    let system = match cmd.slot {
        Some(Slot::A) => product_bundle.system_a,
        Some(Slot::B) => product_bundle.system_b,
        Some(Slot::R) => product_bundle.system_r,
        _ => ffx_bail!("No valid slot passed in {:#?}", cmd.slot),
    }
    .ok_or(anyhow!("No image found in slot"))?;

    system
        .iter()
        .find_map(|i| match (i, cmd.image_type) {
            (Image::ZBI { path, .. }, Some(ImageType::Zbi))
            | (Image::VBMeta(path), Some(ImageType::VBMeta))
            | (Image::FVM(path), Some(ImageType::Fvm))
            | (Image::Fxfs { path, .. }, Some(ImageType::Fxfs))
            | (Image::QemuKernel(path), Some(ImageType::QemuKernel)) => Some(compute_path(path)),
            _ => None,
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_partitions_config::PartitionsConfig;
    use sdk_metadata::ProductBundleV2;

    #[test]
    fn test_get_image_path() {
        let json = r#"
            {
                bootloader_partitions: [
                    {
                        type: "bl2",
                        name: "bl2",
                        image: "/tmp/product_bundle/bootloader/path",
                    },
                    {
                        type: "",
                        name: "bootloader",
                        image: "/tmp/product_bundle/bootloader/empty/path",
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
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::new(),
            slot: Some(Slot::A),
            image_type: Some(ImageType::Zbi),
            relative_path: false,
            bootloader: None,
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("zbi/path");
        assert_eq!(expected_path, path);

        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::new(),
            slot: Some(Slot::A),
            image_type: Some(ImageType::QemuKernel),
            relative_path: false,
            bootloader: None,
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("qemu/path");
        assert_eq!(expected_path, path);

        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::from("/tmp/product_bundle"),
            slot: Some(Slot::A),
            image_type: Some(ImageType::Fvm),
            relative_path: true,
            bootloader: None,
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("system_a/fvm.blk");
        assert_eq!(expected_path, path);

        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::from("/tmp/product_bundle"),
            slot: None,
            image_type: None,
            relative_path: true,
            bootloader: Some(String::from("firmware_bl2")),
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("bootloader/path");
        assert_eq!(expected_path, path);

        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::from("/tmp/product_bundle"),
            slot: None,
            image_type: None,
            relative_path: true,
            bootloader: Some(String::from("firmware")),
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("bootloader/empty/path");
        assert_eq!(expected_path, path);
    }

    #[test]
    fn test_get_image_path_not_found() {
        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: PartitionsConfig::default(),
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: Utf8PathBuf::from("zbi/path"), signed: false },
                Image::FVM(Utf8PathBuf::from("fvm/path")),
                Image::QemuKernel(Utf8PathBuf::from("qemu/path")),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::new(),
            slot: Some(Slot::A),
            image_type: Some(ImageType::VBMeta),
            relative_path: false,
            bootloader: None,
        };
        let path = extract_image_path(pb, cmd).unwrap();
        assert_eq!(None, path);
    }
}
