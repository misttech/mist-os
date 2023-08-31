// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Generate a build archive from a product bundle.

use anyhow::{bail, Context, Result};
use argh::FromArgs;
use assembly_manifest::{AssemblyManifest, Image};
use camino::Utf8PathBuf;
use sdk_metadata::ProductBundle;

/// Generate a build archive using the specified `args`.
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "generate-build-archive")]
pub struct GenerateBuildArchive {
    /// path to a product bundle.
    #[argh(option)]
    product_bundle: Utf8PathBuf,

    /// path to the directory to write a build archive into.
    #[argh(option)]
    out_dir: Utf8PathBuf,
}

impl GenerateBuildArchive {
    pub fn generate(self) -> Result<()> {
        println!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
        println!("@");
        println!("@  The `pbtool generate-build-archive` is deprecated.");
        println!("@");
        println!("@  Please flash using a product bundle (v2) instead.");
        println!("@");
        println!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
        let product_bundle = ProductBundle::try_load_from(&self.product_bundle)?;
        let mut product_bundle = match product_bundle {
            ProductBundle::V1(_) => bail!("Only v2 product bundles are supported"),
            ProductBundle::V2(pb) => pb,
        };

        // Ensure the `out_dir` exists.
        std::fs::create_dir_all(&self.out_dir)
            .with_context(|| format!("Creating the out_dir: {}", &self.out_dir))?;

        // Collect the Images with the final destinations to add to an images manifest later.
        let mut images = vec![];

        let copy_artifact = |path: &Utf8PathBuf, name: &str| -> Result<()> {
            // Copy the image to the out_dir.
            let destination = self.out_dir.join(name);
            std::fs::copy(&path, &destination)
                .with_context(|| format!("Copying artifact {} to {}", path, destination))?;
            Ok(())
        };

        for part in &mut product_bundle.partitions.bootstrap_partitions {
            let filename = part
                .image
                .file_name()
                .context(format!("Misformatted bootstrap partition: {}", &part.image))?;
            copy_artifact(&part.image, filename)?;
        }
        for part in &mut product_bundle.partitions.bootloader_partitions {
            if part.name.is_none() {
                continue;
            }
            let name = if part.partition_type == "" {
                "firmware.img".to_owned()
            } else {
                format!("{}_{}.img", "firmware", part.partition_type)
            };
            copy_artifact(&part.image, &name)?;
        }
        for cred in &mut product_bundle.partitions.unlock_credentials {
            let filename =
                cred.file_name().context(format!("Misformatted credential: {}", &cred))?;
            copy_artifact(&cred, filename)?;
        }

        // Pull out the relevant files.
        if let Some(a) = product_bundle.system_a {
            for image in a.iter() {
                let entry = match &image {
                    Image::ZBI { path, signed: _ } => Some((path, "zircon-a.zbi")),
                    Image::VBMeta(path) => Some((path, "zircon-a.vbmeta")),
                    Image::FVM(path) => Some((path, "storage-full.blk")),
                    Image::QemuKernel(path) => Some((path, "qemu-kernel.kernel")),
                    Image::FVMFastboot(path) => Some((path, "fvm.fastboot.blk")),
                    _ => None,
                };
                if let Some((path, name)) = entry {
                    copy_artifact(path, name)?;

                    // Create a new Image with the new path.
                    let destination = self.out_dir.join(name);
                    let mut new_image = image.clone();
                    new_image.set_source(destination);
                    images.push(new_image);
                }
            }
        }

        if let Some(r) = product_bundle.system_r {
            for image in r.iter() {
                let entry = match &image {
                    Image::ZBI { path, signed: _ } => Some((path, "zircon-r.zbi")),
                    Image::VBMeta(path) => Some((path, "zircon-r.vbmeta")),
                    _ => None,
                };
                if let Some((path, name)) = entry {
                    copy_artifact(path, name)?;
                }
            }
        }

        // Write the images manifest with the rebased image paths.
        let images_manifest = AssemblyManifest { images };
        let images_manifest_path = self.out_dir.join("images.json");
        images_manifest.write(images_manifest_path).context("Writing images manifest")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_manifest::Image;
    use assembly_partitions_config::PartitionsConfig;
    use camino::Utf8Path;
    use sdk_metadata::ProductBundleV2;
    use serde_json::Value;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_generate_build_archive() {
        let tmp = tempdir().unwrap();
        let tempdir = Utf8Path::from_path(tmp.path()).unwrap();

        let json = r#"
            {
                "bootloader_partitions" : [
                    {
                        "image" : "TEMPDIR/u-boot.bin.signed.b4",
                        "name" : "bootloader",
                        "type" : "skip_metadata"
                    }
                ],
                "bootstrap_partitions" : [
                    {
                        "condition" : {
                        "value" : "0xe9000000",
                        "variable" : "emmc-total-bytes"
                        },
                        "image" : "TEMPDIR/gpt.fuchsia.3728.bin",
                        "name" : "gpt"
                    },
                    {
                        "condition" : {
                        "value" : "0xec000000",
                        "variable" : "emmc-total-bytes"
                        },
                        "image" : "TEMPDIR/gpt.fuchsia.3776.bin",
                        "name" : "gpt"
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
                    "TEMPDIR/unlock_creds.zip",
                ],
            }
        "#;
        let mut cursor = std::io::Cursor::new(json.replace("TEMPDIR", tempdir.as_str()));
        let config: PartitionsConfig = PartitionsConfig::from_reader(&mut cursor).unwrap();

        let create_temp_file = |name: &str| {
            let path = tempdir.join(name);
            let mut file = File::create(path).unwrap();
            write!(file, "{}", name).unwrap();
        };

        create_temp_file("unlock_creds.zip");
        create_temp_file("gpt.fuchsia.3728.bin");
        create_temp_file("gpt.fuchsia.3776.bin");
        create_temp_file("u-boot.bin.signed.b4");
        create_temp_file("fuchsia.zbi");
        create_temp_file("fuchsia.vbmeta");
        create_temp_file("fvm.blk");
        create_temp_file("fvm.fastboot.blk");
        create_temp_file("kernel");
        create_temp_file("zedboot.zbi");
        create_temp_file("zedboot.vbmeta");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "".to_string(),
            product_version: "".to_string(),
            partitions: config,
            sdk_version: "".to_string(),
            system_a: Some(vec![
                Image::ZBI { path: tempdir.join("fuchsia.zbi"), signed: false },
                Image::VBMeta(tempdir.join("fuchsia.vbmeta")),
                Image::FVM(tempdir.join("fvm.blk")),
                Image::FVMFastboot(tempdir.join("fvm.fastboot.blk")),
                Image::QemuKernel(tempdir.join("kernel")),
            ]),
            system_b: None,
            system_r: Some(vec![
                Image::ZBI { path: tempdir.join("zedboot.zbi"), signed: false },
                Image::VBMeta(tempdir.join("zedboot.vbmeta")),
            ]),
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        let pb_path = tempdir.join("product_bundle");
        std::fs::create_dir_all(&pb_path).unwrap();
        pb.write(&pb_path).unwrap();

        let ba_path = tempdir.join("build_archive");
        let cmd =
            GenerateBuildArchive { product_bundle: pb_path.clone(), out_dir: ba_path.clone() };
        cmd.generate().unwrap();

        assert!(ba_path.join("unlock_creds.zip").exists());
        assert!(ba_path.join("gpt.fuchsia.3728.bin").exists());
        assert!(ba_path.join("gpt.fuchsia.3776.bin").exists());
        assert!(ba_path.join("firmware_skip_metadata.img").exists());
        assert!(ba_path.join("zircon-a.zbi").exists());
        assert!(ba_path.join("zircon-a.vbmeta").exists());
        assert!(ba_path.join("fvm.fastboot.blk").exists());
        assert!(ba_path.join("storage-full.blk").exists());
        assert!(ba_path.join("qemu-kernel.kernel").exists());
        assert!(ba_path.join("zircon-r.zbi").exists());
        assert!(ba_path.join("zircon-r.vbmeta").exists());

        let images_manifest_file = File::open(ba_path.join("images.json")).unwrap();
        let images_manifest: Value = serde_json::from_reader(images_manifest_file).unwrap();
        assert_eq!(
            images_manifest,
            serde_json::from_str::<Value>(
                r#"
            [
                {
                    "name": "zircon-a",
                    "type": "zbi",
                    "path": "zircon-a.zbi",
                    "signed": false
                },
                {
                    "name": "zircon-a",
                    "type": "vbmeta",
                    "path": "zircon-a.vbmeta"
                },
                {
                    "type": "blk",
                    "name": "storage-full",
                    "path": "storage-full.blk"
                },
                {
                    "name" : "fvm.fastboot",
                    "path" : "fvm.fastboot.blk",
                    "type" : "blk"
                },
                {
                    "type": "kernel",
                    "name": "qemu-kernel",
                    "path": "qemu-kernel.kernel"
                }
            ]
            "#
            )
            .unwrap()
        );
    }
}
