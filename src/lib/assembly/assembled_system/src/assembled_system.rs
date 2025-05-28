// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Image;

use anyhow::{anyhow, Context, Result};
use assembly_container::{
    assembly_container, AssemblyContainer, DirectoryPathBuf, FileType, WalkPaths,
};
use camino::{Utf8Path, Utf8PathBuf};
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
}

impl AssembledSystem {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        };

        let manifest = manifest.relativize(Utf8PathBuf::from("path/to")).unwrap();
        assert_eq!(expected_manifest, manifest);
    }
}
