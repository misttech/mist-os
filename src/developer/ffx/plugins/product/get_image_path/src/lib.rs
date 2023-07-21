// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for get a path of the image inside product bundle.

use anyhow::{anyhow, Context, Result};
use assembly_manifest::Image;
use camino::Utf8PathBuf;
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_product_get_image_path_args::{GetImagePathCommand, ImageType, Slot};
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx_ext::RepositoryConfig;
use sdk_metadata::ProductBundle;
use std::io::Write;

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
        writeln!(
            writer,
            "{}",
            path.ok_or_else(|| anyhow!("Cannot find corresponding image in slot"))?
        )?;
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
    let system = match cmd.slot {
        Slot::A => product_bundle.system_a,
        Slot::B => product_bundle.system_b,
        Slot::R => product_bundle.system_r,
    };
    let system = system.ok_or_else(|| anyhow!("No image found in slot"))?;
    let path = system.iter().find_map(|i| match (i, cmd.image_type) {
        (Image::ZBI { path, .. }, ImageType::Zbi)
        | (Image::VBMeta(path), ImageType::VBMeta)
        | (Image::FVM(path), ImageType::Fvm)
        | (Image::Fxfs { path, .. }, ImageType::Fxfs)
        | (Image::QemuKernel(path), ImageType::QemuKernel) => Some(path.clone().into()),
        _ => None,
    });
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_partitions_config::PartitionsConfig;
    use sdk_metadata::ProductBundleV2;

    #[test]
    fn test_get_image_path() {
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
            slot: Slot::A,
            image_type: ImageType::Zbi,
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("zbi/path");
        assert_eq!(expected_path, path);

        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::new(),
            slot: Slot::A,
            image_type: ImageType::QemuKernel,
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("qemu/path");
        assert_eq!(expected_path, path);

        let cmd = GetImagePathCommand {
            product_bundle: Utf8PathBuf::new(),
            slot: Slot::A,
            image_type: ImageType::Fvm,
        };
        let path = extract_image_path(pb.clone(), cmd).unwrap().unwrap();
        let expected_path = Utf8PathBuf::from("fvm/path");
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
            slot: Slot::A,
            image_type: ImageType::VBMeta,
        };
        let path = extract_image_path(pb, cmd).unwrap();
        assert_eq!(None, path);
    }
}
