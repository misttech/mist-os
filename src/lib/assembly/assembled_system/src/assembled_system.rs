// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::base_package::construct_base_package;
use crate::fvm::construct_fvm;
use crate::fxfs::{construct_fxfs, ConstructedFxfs};
use crate::image::Image;
use crate::{vbmeta, zbi};

use anyhow::{anyhow, Context, Result};
use assembly_constants::PackageDestination;
use assembly_container::{
    assembly_container, AssemblyContainer, DirectoryPathBuf, FileType, WalkPaths,
};
use assembly_images_config::{FilesystemImageMode, Fvm, Fxfs, VBMeta, Zbi};
use assembly_release_info::SystemReleaseInfo;
use assembly_tool::ToolProvider;
use camino::{Utf8Path, Utf8PathBuf};
use image_assembly_config::ImageAssemblyConfig;
use log::info;
use pathdiff::diff_paths;
use serde::{Deserialize, Serialize};
use std::fs::File;

/// A manifest containing all the images necessary to boot a Fuchsia system.
///
/// ```
/// use assembled_system::AssembledSystem;
///
/// let manifest = AssembledSystem {
///     images: vec![
///         Image::ZBI {
///             path: "path/to/fuchsia.zbi",
///             signed: false,
///         },
///         Image::VBMeta("path/to/fuchsia.vbmeta"),
///         Image::Dtbo("path/to/dtbo"),
///         Image::FVM("path/to/fvm.blk"),
///         Image::FVMSparse("path/to/fvm.sparse.blk"),
///     ],
///     board_name: "my_board".into(),
/// };
/// println!("{:?}", serde_json::to_value(manifest).unwrap());
/// ```
///
#[derive(Clone, Debug, Eq, Hash, Deserialize, Serialize, PartialEq, WalkPaths)]
#[assembly_container(assembled_system.json)]
pub struct AssembledSystem {
    /// List of images in the manifest.
    #[walk_paths]
    pub images: Vec<Image>,

    /// The board name that these images can be OTA'd to, which will be used to create an update
    /// package. OTAs will fail if this board_name changes across builds.
    ///
    /// The images contain this name inside build_info, and the software delivery code asserts that
    /// build_info.board.name == update_package.board_name.
    pub board_name: String,

    /// The partitions these images can be flashed to.
    #[walk_paths]
    pub partitions_config: Option<DirectoryPathBuf>,

    /// Release information for all input artifacts that contributed to this system.
    pub system_release_info: SystemReleaseInfo,
}

impl AssembledSystem {
    /// Construct a new Fuchsia system.
    pub async fn new(
        image_assembly_config: ImageAssemblyConfig,
        include_account: bool,
        gendir: &Utf8PathBuf,
        tools: &impl ToolProvider,
        base_package_name: Option<String>,
    ) -> Result<Self> {
        let mut system = Self {
            images: vec![],
            board_name: image_assembly_config.board_name.clone(),
            partitions_config: image_assembly_config
                .partitions_config
                .as_ref()
                .map(|p| DirectoryPathBuf::new(p.clone())),
            system_release_info: image_assembly_config.system_release_info.clone(),
        };

        let base_package_name =
            base_package_name.unwrap_or_else(|| PackageDestination::Base.to_string());
        let images_config = &image_assembly_config.images_config;
        let mode = image_assembly_config.image_mode;

        if let Some(devicetree_overlay) = &image_assembly_config.devicetree_overlay {
            system.images.push(Image::Dtbo(devicetree_overlay.clone()));
        }
        system.images.push(Image::QemuKernel(image_assembly_config.qemu_kernel.clone()));

        // Create the base package if needed.
        let base_package = if has_base_package(&image_assembly_config) {
            info!("Creating base package");
            Some(construct_base_package(
                &mut system,
                gendir,
                &base_package_name,
                &image_assembly_config,
            )?)
        } else {
            info!("Skipping base package creation");
            None
        };

        // Get the FVM config.
        let fvm_config: Option<&Fvm> = images_config.images.iter().find_map(|i| match i {
            assembly_images_config::Image::Fvm(fvm) => Some(fvm),
            _ => None,
        });
        let fxfs_config: Option<&Fxfs> = images_config.images.iter().find_map(|i| match i {
            assembly_images_config::Image::Fxfs(fxfs) => Some(fxfs),
            _ => None,
        });

        // Create all the filesystems and FVMs.
        if let Some(fvm_config) = fvm_config {
            // Determine whether blobfs should be compressed.
            // We refrain from compressing blobfs if the FVM is destined for the ZBI, because the ZBI
            // compression will be more optimized.
            let compress_blobfs = !matches!(&mode, FilesystemImageMode::Ramdisk);

            // TODO: warn if bootfs_only mode
            if let Some(base_package) = &base_package {
                construct_fvm(
                    gendir,
                    tools,
                    &mut system,
                    &image_assembly_config,
                    fvm_config.clone(),
                    compress_blobfs,
                    include_account,
                    base_package,
                )?;
            }
        } else if let Some(fxfs_config) = fxfs_config {
            info!("Constructing Fxfs image <EXPERIMENTAL!>");
            if let Some(base_package) = &base_package {
                let ConstructedFxfs { image_path, sparse_image_path, contents } =
                    construct_fxfs(gendir, &image_assembly_config, base_package, fxfs_config)
                        .await?;
                system.images.push(Image::Fxfs(image_path));
                system.images.push(Image::FxfsSparse { path: sparse_image_path, contents });
            }
        } else {
            info!("Skipping fvm creation");
        };

        // Find the first standard disk image that was generated.
        let disk_image_for_zbi: Option<Utf8PathBuf> = match &mode {
            FilesystemImageMode::Ramdisk => system.images.iter().find_map(|i| match i {
                Image::FVM(path) => Some(path.clone()),
                Image::Fxfs(path) => Some(path.clone()),
                _ => None,
            }),
            _ => None,
        };

        // Get the ZBI config.
        let zbi_config: Option<&Zbi> = images_config.images.iter().find_map(|i| match i {
            assembly_images_config::Image::Zbi(zbi) => Some(zbi),
            _ => None,
        });

        let zbi_path: Option<Utf8PathBuf> = if let Some(zbi_config) = zbi_config {
            Some(zbi::construct_zbi(
                tools.get_tool("zbi")?,
                &mut system,
                gendir,
                &image_assembly_config,
                zbi_config,
                base_package.as_ref(),
                disk_image_for_zbi,
            )?)
        } else {
            info!("Skipping zbi creation");
            None
        };

        // Building a ZBI is expected, therefore throw an error otherwise.
        let zbi_path = zbi_path.ok_or_else(|| anyhow!("Missing a ZBI in the images config"))?;

        // Get the VBMeta config.
        let vbmeta_config: Option<&VBMeta> = images_config.images.iter().find_map(|i| match i {
            assembly_images_config::Image::VBMeta(vbmeta) => Some(vbmeta),
            _ => None,
        });

        if let Some(vbmeta_config) = vbmeta_config {
            info!("Creating the VBMeta image");
            vbmeta::construct_vbmeta(&mut system, gendir, vbmeta_config, &zbi_path)
                .context("Creating the VBMeta image")?;
        } else {
            info!("Skipping vbmeta creation");
        }

        // If the board specifies a vendor-specific signing script, use that to
        // post-process the ZBI.
        if let Some(zbi_config) = zbi_config {
            #[allow(clippy::single_match)]
            match &zbi_config.postprocessing_script {
                Some(script) => {
                    let tool_path = match &script.path {
                        Some(path) => path.clone().to_utf8_pathbuf(),
                        None => script
                            .board_script_path
                            .clone()
                            .expect("Either `path` or `board_script_path` should be specified")
                            .to_utf8_pathbuf(),
                    };
                    let signing_tool = tools.get_tool_with_path(tool_path.into())?;
                    zbi::vendor_sign_zbi(signing_tool, &mut system, gendir, zbi_config, &zbi_path)
                        .context("Vendor-signing the ZBI")?;
                }
                _ => {}
            }
        } else {
            info!("Skipping zbi signing");
        }

        Ok(system)
    }

    /// Relativize all paths in the manifest to `reference`, but don't copy any
    /// files. The returned object will not be writable or readable.
    fn relativize(mut self, reference: impl AsRef<Utf8Path>) -> Result<Self> {
        self.walk_paths(&mut |path: &mut Utf8PathBuf, _dest: Utf8PathBuf, _filetype: FileType| {
            let new_path = diff_paths(&path, reference.as_ref())
                .ok_or_else(|| anyhow!("Failed to make the path relative: {}", &path))?;
            *path = Utf8PathBuf::try_from(new_path)?;
            Ok(())
        })
        .with_context(|| format!("Making all paths relative to: {}", reference.as_ref()))?;
        Ok(self)
    }

    /// Construct an AssembledSystem from a config file with relative paths.
    pub fn from_relative_config_path(path: impl AsRef<Utf8Path>) -> Result<Self> {
        // Read the config to a string first because it offers better
        // performance for serde.
        let data = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Reading config: {}", path.as_ref()))?;
        let mut config: Self = serde_json5::from_str(&data)
            .with_context(|| format!("Parsing config: {}", path.as_ref()))?;

        // Make all the paths absolute.
        let dir = path
            .as_ref()
            .parent()
            .with_context(|| format!("Getting parent directory for: {}", path.as_ref()))?;
        config
            .walk_paths(&mut |path: &mut Utf8PathBuf, _dest: Utf8PathBuf, _filetype: FileType| {
                *path = dir.join(&path);
                Ok(())
            })
            .context("Making all config paths absolute")?;
        Ok(config)
    }

    /// Write an assembly manifest to a directory on disk at `path` using the 'old' format.
    /// Make the paths recorded in the file relative to the file's location
    /// so it is portable.
    pub fn write_old(self, path: impl AsRef<Utf8Path>) -> Result<()> {
        let path = path.as_ref();
        // Relativize the paths in the Assembly Manifest
        let assembled_system =
            self.relativize(path.parent().with_context(|| format!("Invalid output path {path}"))?)?;

        // Write the images manifest.
        let images_json = File::create(path).context("Creating assembly manifest")?;
        serde_json::to_writer_pretty(images_json, &assembled_system.images)
            .context("Writing assembly manifest")?;
        Ok(())
    }
}

fn has_base_package(image_assembly_config: &ImageAssemblyConfig) -> bool {
    !(image_assembly_config.base.is_empty()
        && image_assembly_config.cache.is_empty()
        && image_assembly_config.system.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_release_info::SystemReleaseInfo;

    #[test]
    fn relativize() {
        let manifest = AssembledSystem {
            images: vec![
                Image::BasePackage("path/to/base.far".into()),
                Image::ZBI { path: "path/to/fuchsia.zbi".into(), signed: true },
                Image::VBMeta("path/to/fuchsia.vbmeta".into()),
                Image::Dtbo("path/to/dtbo".into()),
                Image::BlobFS { path: "path/to/blob.blk".into(), contents: Default::default() },
                Image::FVM("path/to/fvm.blk".into()),
                Image::FVMSparse("path/to/fvm.sparse.blk".into()),
                Image::FVMFastboot("path/to/fvm.fastboot.blk".into()),
                Image::Fxfs("path/to/fxfs.blk".into()),
                Image::FxfsSparse {
                    path: "path/to/fxfs.sparse.blk".into(),
                    contents: Default::default(),
                },
                Image::QemuKernel("path/to/qemu/kernel".into()),
            ],
            board_name: "my_board".into(),
            partitions_config: None,
            system_release_info: SystemReleaseInfo::new_for_testing(),
        };
        let expected_manifest = AssembledSystem {
            images: vec![
                Image::BasePackage("base.far".into()),
                Image::ZBI { path: "fuchsia.zbi".into(), signed: true },
                Image::VBMeta("fuchsia.vbmeta".into()),
                Image::Dtbo("dtbo".into()),
                Image::BlobFS { path: "blob.blk".into(), contents: Default::default() },
                Image::FVM("fvm.blk".into()),
                Image::FVMSparse("fvm.sparse.blk".into()),
                Image::FVMFastboot("fvm.fastboot.blk".into()),
                Image::Fxfs("fxfs.blk".into()),
                Image::FxfsSparse { path: "fxfs.sparse.blk".into(), contents: Default::default() },
                Image::QemuKernel("qemu/kernel".into()),
            ],
            board_name: "my_board".into(),
            partitions_config: None,
            system_release_info: SystemReleaseInfo::new_for_testing(),
        };

        let manifest = manifest.relativize(Utf8PathBuf::from("path/to")).unwrap();
        assert_eq!(expected_manifest, manifest);
    }
}
