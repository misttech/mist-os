// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Error};
use argh::FromArgs;
use std::str::FromStr;

#[derive(PartialEq)]
pub enum ContainerArchitecture {
    X64,
    Arm64,
}

impl FromStr for ContainerArchitecture {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "x64" => Ok(ContainerArchitecture::X64),
            "arm64" => Ok(ContainerArchitecture::Arm64),
            other => bail!("Invalid architecture: {}", other),
        }
    }
}

pub enum InputFormat {
    /// A tarball containing the root filesystem.
    Tarball,

    /// A Docker archive (created with "docker save").
    DockerArchive,
}

impl FromStr for InputFormat {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "tarball" => Ok(InputFormat::Tarball),
            "docker-archive" => Ok(InputFormat::DockerArchive),
            other => bail!("Invalid input format: {}", other),
        }
    }
}

#[derive(FromArgs)]
/// Converts an input archive into a FAR package that can be loaded in Starnix.
pub struct Command {
    /// container architecture; available formats: "x64", "arm64"
    #[argh(option)]
    pub arch: Option<ContainerArchitecture>,

    /// container features
    #[argh(option)]
    pub features: Vec<String>,

    /// input format; available formats: "tarball", "docker-archive"
    #[argh(option)]
    pub input_format: InputFormat,

    /// input archive path
    #[argh(positional)]
    pub input_path: String,

    /// output FAR file
    #[argh(positional)]
    pub output_file: String,
}
