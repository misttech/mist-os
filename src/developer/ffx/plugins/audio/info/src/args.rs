// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Result},
    argh::FromArgs,
    ffx_core::ffx_command,
    std::str::FromStr,
};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "info",
    description = "Prints information about a target",
    example = "ffx audio info --output text device --id 1 --type input"
)]
pub struct InfoCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,

    #[argh(
        option,
        description = "output format: accepted options are 'text' for readable text, or 'json' for a JSON dictionary",
        default = "default_output_format()"
    )]
    pub output: InfoOutputFormat,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum SubCommand {
    DeviceInfo(DeviceInfoCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "device",
    description = "List information about a specific audio device."
)]
pub struct DeviceInfoCommand {
    #[argh(
        option,
        description = "device id. stream device node id from either /dev/audio-input/* or /dev/audio-output/*"
    )]
    pub id: String,

    #[argh(option, long = "type", description = "device type. Accepted values: input, output")]
    pub device_type: DeviceType,
}

#[derive(Debug, PartialEq)]
pub enum InfoOutputFormat {
    Json,
    Text,
}

impl FromStr for InfoOutputFormat {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_lowercase().as_str() {
            "json" => Ok(InfoOutputFormat::Json),
            "text" => Ok(InfoOutputFormat::Text),
            _ => Err(anyhow!("invalid format argument, {}. Expected one of: JSON, text", s)),
        }
    }
}

fn default_output_format() -> InfoOutputFormat {
    InfoOutputFormat::Text
}

#[derive(Debug, PartialEq)]
pub enum DeviceType {
    Input,
    Output,
}

impl FromStr for DeviceType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_lowercase().as_str() {
            "input" => Ok(DeviceType::Input),
            "output" => Ok(DeviceType::Output),
            _ => Err(anyhow!("invalid device type, {}. Expected one of: input, output", s)),
        }
    }
}
