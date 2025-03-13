// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use argh::{ArgsInfo, FromArgs};
use camino::Utf8PathBuf;
use ffx_core::ffx_command;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Copy)]
pub enum Slot {
    A,
    B,
    R,
}

impl FromStr for Slot {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Slot, anyhow::Error> {
        match value.to_lowercase().as_str() {
            "a" => Ok(Slot::A),
            "b" => Ok(Slot::B),
            "r" => Ok(Slot::R),
            _ => Err(anyhow!("Invalid slot: {}. Expect one of : a, b, r", value)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Copy)]
pub enum ImageType {
    Zbi,
    VBMeta,
    Fvm,
    Fxfs,
    FxfsFastboot,
    QemuKernel,
    Dtbo,
}

impl FromStr for ImageType {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<ImageType, anyhow::Error> {
        match value.to_lowercase().as_str() {
            "zbi" => Ok(ImageType::Zbi),
            "vbmeta" => Ok(ImageType::VBMeta),
            "fvm" => Ok(ImageType::Fvm),
            "fxfs" => Ok(ImageType::Fxfs),
            "fxfs.fastboot" => Ok(ImageType::FxfsFastboot),
            "qemu-kernel" => Ok(ImageType::QemuKernel),
            "dtbo" => Ok(ImageType::Dtbo),
            _ => Err(anyhow!(
                "Invalid image_type: {}. Expect one of : zbi, vbmeta, fvm, fxfs, fxfs.fastboot, qemu-kernel, dtbo",
                value
            )),
        }
    }
}

/// Get the path of an image inside a Product Bundle based on type and slot.
#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "get-image-path")]
pub struct GetImagePathCommand {
    /// path to product bundle directory. Defaults to the configured
    /// path in `product.path`.
    #[argh(positional)]
    pub product_bundle: Option<Utf8PathBuf>,

    /// the slot where image will be located in. Valid slots are A,B,R.
    #[argh(option)]
    pub slot: Option<Slot>,

    /// the type of image. Supported types are fvm, fxfs, qemu-kernel, vbmeta, zbi.
    #[argh(option)]
    pub image_type: Option<ImageType>,

    /// the type of bootloader.
    #[argh(option, short = 'b')]
    pub bootloader: Option<String>,

    /// return relative path or not
    #[argh(switch, short = 'r')]
    pub relative_path: bool,
}
