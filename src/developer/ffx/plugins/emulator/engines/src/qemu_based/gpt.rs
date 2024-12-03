// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for preparing GPT full disk images for qemu based emulator runs.

use camino::Utf8PathBuf;
use ffx_config::environment::ExecutableKind;
use ffx_config::EnvironmentContext;
use fho::{return_bug, user_error, Result};
use make_fuchsia_vol::args::{
    Arch, TopLevel as MakeFuchsiaVolCmd, ABR_SIZE, EFI_SIZE, VBMETA_SIZE,
};
use sdk_metadata::CpuArchitecture;
use std::path::PathBuf;

const DEFAULT_ZEDBOOT_CMDLINE: &str = r#"bootloader.default=default
bootloader.timeout=5"#;

// By default, the image will be resized to 20G.
// TODO(https://fxbug.dev/380879811): Calculate this dynamically.
pub(crate) const DEFAULT_IMAGE_SIZE: u64 = 20 * 1024 * 1024 * 1024;

/// Holds the args needed to construct a full GPT disk image
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FuchsiaFullDiskImageBuilder {
    /// Architecture of the image to be generated
    arch: CpuArchitecture,
    /// Path to the zedbood command line file
    cmdline: Option<Utf8PathBuf>,
    /// Product bundle path. If provided, make_fuchsia_vol will look up the product name there.
    product_bundle: Option<Utf8PathBuf>,
    /// Path to mkfs-msdosfs. If provided, make_fuchsia_vol will use it instead of trying to
    /// derive it from the build output directory.
    mkfs_msdosfs_path: Option<Utf8PathBuf>,
    /// Output file of the disk image (full path)
    output_path: Utf8PathBuf,
    /// Create or resize the image to this size in bytes
    resize: Option<u64>,
    /// Use fxfs to store the base system
    use_fxfs: bool,
    /// Path to the ZBI. If not provided, make_fuchsia_vol will look it up from the product bundle.
    zbi: Option<Utf8PathBuf>,
    /// Path to the vbmeta, needs to be provided if the ZBI was modified from the one in the bundle.
    vbmeta: Option<Utf8PathBuf>,
}

fn convert_arch(arch: CpuArchitecture) -> Result<Arch> {
    match arch {
        CpuArchitecture::Arm64 => Ok(Arch::Arm64),
        CpuArchitecture::X64 => Ok(Arch::X64),
        a @ _ => return_bug!("arch {:?} is not supported yet for full disk GPT images", a),
    }
}

impl FuchsiaFullDiskImageBuilder {
    pub fn new() -> Self {
        Self { resize: Some(DEFAULT_IMAGE_SIZE), use_fxfs: true, ..Default::default() }
    }

    pub async fn build(self, context: &EnvironmentContext) -> Result<()> {
        // TODO(https://fxbug.dev/380065101): When we have a fake make-fuchsia-vol, this should be
        // removed to increase test coverage.
        if context.exe_kind() == ExecutableKind::Test {
            tracing::debug!(
                "Building full GPT images as part of a test case is not supported yet, skipping."
            );
            return Ok(());
        }

        let cmd = MakeFuchsiaVolCmd {
            abr_size: ABR_SIZE,
            arch: convert_arch(self.arch)?,
            cmdline: self.cmdline,
            disk_path: self.output_path,
            efi_size: EFI_SIZE,
            mkfs_msdosfs: self.mkfs_msdosfs_path,
            product_bundle: self.product_bundle,
            resize: self.resize,
            use_fxfs: self.use_fxfs,
            vbmeta_a: self.vbmeta.clone(),
            vbmeta_b: self.vbmeta.clone(),
            vbmeta_size: VBMETA_SIZE,
            // make_fuchsia_vol uses zbi for both A and B partitions unless specified otherwise:
            // //tools/make-fuchsia-vol/src/args.rs
            zbi: self.zbi,
            ..Default::default()
        };
        Ok(make_fuchsia_vol::run(cmd)?)
    }

    pub fn arch(mut self, arch: CpuArchitecture) -> Self {
        self.arch = arch;
        self
    }

    pub fn cmdline(mut self, path: &PathBuf) -> Self {
        let p = Utf8PathBuf::from_path_buf(path.clone())
            .expect("error converting cmdline path to UTF-8");
        self.cmdline = Some(p);
        self
    }

    pub fn mkfs_msdosfs_path(mut self, path: &PathBuf) -> Self {
        let p = Utf8PathBuf::from_path_buf(path.clone())
            .expect("error converting mkfs-msdosfs path to UTF-8");
        self.mkfs_msdosfs_path = Some(p);
        self
    }

    pub fn output_path(mut self, path: &PathBuf) -> Self {
        self.output_path = Utf8PathBuf::from_path_buf(path.clone())
            .expect("error converting image output path to UTF-8");
        self
    }

    pub fn product_bundle(mut self, path: &PathBuf) -> Self {
        let p = Utf8PathBuf::from_path_buf(path.clone())
            .expect("error converting product bundle path to UTF-8");
        self.product_bundle = Some(p);
        self
    }

    pub fn resize(mut self, size: u64) -> Self {
        self.resize = Some(size);
        self
    }

    pub fn use_fxfs(mut self, fxfs: bool) -> Self {
        self.use_fxfs = fxfs;
        self
    }

    pub fn vbmeta(mut self, path: Option<PathBuf>) -> Self {
        self.vbmeta = if let Some(p) = path {
            Some(Utf8PathBuf::from_path_buf(p).expect("error converting vbmeta path to UTF-8"))
        } else {
            None
        };
        self
    }

    pub fn zbi(mut self, path: Option<PathBuf>) -> Self {
        self.zbi = if let Some(p) = path {
            Some(Utf8PathBuf::from_path_buf(p).expect("error converting zbi path to UTF-8"))
        } else {
            None
        };
        self
    }
}

pub(crate) fn write_zedboot_cmdline(path: &PathBuf, cmd: Option<&str>) -> Result<()> {
    let cmdline = if let Some(c) = cmd { c } else { DEFAULT_ZEDBOOT_CMDLINE };

    std::fs::write(path, cmdline).map_err(|e| user_error!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile;

    #[fuchsia::test]
    async fn test_create_default_cmdline_file_in_designated_location() {
        let tmpdir = tempfile::tempdir().unwrap().into_path();
        let f = tmpdir.join("cmdline");
        write_zedboot_cmdline(&f, None).unwrap();
        let cmdline = std::fs::read_to_string(f).unwrap();
        assert_eq!(cmdline, DEFAULT_ZEDBOOT_CMDLINE);
    }

    #[fuchsia::test]
    async fn test_construct_default_make_fuchsia_vol_argument() {
        let builder = FuchsiaFullDiskImageBuilder::new();
        assert!(builder.cmdline.is_none());
        assert!(builder.product_bundle.is_none());
        assert!(builder.vbmeta.is_none());
        assert!(builder.zbi.is_none());
        assert!(builder.use_fxfs);
        assert_eq!(builder.arch, CpuArchitecture::X64);
        assert_eq!(builder.output_path, "");
        assert_eq!(builder.resize, Some(DEFAULT_IMAGE_SIZE));
    }

    #[fuchsia::test]
    async fn test_construct_custom_make_fuchsia_vol_argument() {
        const CMDLINE: &str = "/path/to/cmdline";
        const PRODUCT_BUNDLE_PATH: &str = "/path/to/product_bundle";
        const MKFS_MSDOSFS_PATH: &str = "/path/to/mkfs-msdosfs";
        const OUTPUT_PATH: &str = "/path/to/output";
        const VBMETA_PATH: &str = "/path/to/vbmeta";
        const ZBI_PATH: &str = "/path/to/zbi";
        let builder = FuchsiaFullDiskImageBuilder::new()
            .arch(CpuArchitecture::Arm64)
            .cmdline(&PathBuf::from(CMDLINE))
            .output_path(&PathBuf::from(OUTPUT_PATH))
            .mkfs_msdosfs_path(&PathBuf::from(MKFS_MSDOSFS_PATH))
            .product_bundle(&PathBuf::from(PRODUCT_BUNDLE_PATH))
            .vbmeta(Some(VBMETA_PATH.into()))
            .zbi(Some(ZBI_PATH.into()))
            .resize(1000)
            .use_fxfs(false);
        assert!(!builder.use_fxfs);
        assert_eq!(builder.arch, CpuArchitecture::Arm64);
        assert_eq!(builder.cmdline, Some(Utf8PathBuf::from(CMDLINE)));
        assert_eq!(builder.output_path, Utf8PathBuf::from(OUTPUT_PATH));
        assert_eq!(builder.mkfs_msdosfs_path, Some(Utf8PathBuf::from(MKFS_MSDOSFS_PATH)));
        assert_eq!(builder.product_bundle, Some(PRODUCT_BUNDLE_PATH.into()));
        assert_eq!(builder.vbmeta, Some(VBMETA_PATH.into()));
        assert_eq!(builder.zbi, Some(ZBI_PATH.into()));
        assert_eq!(builder.resize, Some(1000));
    }
}
