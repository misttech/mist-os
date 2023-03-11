// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, argh::FromArgs, ffx_core::ffx_command, format_utils::Format, std::str::FromStr,
};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "device",
    description = "Interact directly with device hardware.",
    example = "\
    To print information about a specific device: \n\
    \t$ ffx audio device --id 000 info --type input --output text \n\n\
    Play a wav file directly to device hardware: \n\
    \t$ cat ~/sine.wav | ffx audio device --id 000 play \n"
)]
pub struct DeviceCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,

    #[argh(
        option,
        description = "device id. stream device node id from either /dev/audio-input/* or /dev/audio-output/*"
    )]
    pub id: String,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum SubCommand {
    Info(InfoCommand),
    Play(DevicePlayCommand),
    Record(DeviceRecordCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "info", description = "List information about a specific audio device.")]
pub struct InfoCommand {
    #[argh(option, long = "type", description = "device type. Accepted values: input, output")]
    pub device_type: DeviceType,

    #[argh(
        option,
        description = "output format: accepted options are 'text' for readable text, or 'json' for a JSON dictionary. Default: text",
        default = "InfoOutputFormat::Text"
    )]
    pub output: InfoOutputFormat,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "play", description = "Send audio data directly to device ring buffer.")]
pub struct DevicePlayCommand {}

#[derive(Debug, PartialEq)]
pub enum InfoOutputFormat {
    Json,
    Text,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "record", description = "Capture audio data directly from ring buffer.")]
pub struct DeviceRecordCommand {
    #[argh(
        option,
        description = "duration of output signal. Examples: 5ms or 3s. If not specified,\
        press ENTER to stop recording.",
        from_str_fn(parse_duration)
    )]
    pub duration: Option<std::time::Duration>,

    #[argh(option, description = "output format (see 'ffx audio help' for more information).")]
    pub format: Format,
}

impl FromStr for InfoOutputFormat {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_lowercase().as_str() {
            "json" => Ok(InfoOutputFormat::Json),
            "text" => Ok(InfoOutputFormat::Text),
            _ => {
                Err(anyhow::anyhow!("invalid format argument, {}. Expected one of: JSON, text", s))
            }
        }
    }
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
            _ => Err(anyhow::anyhow!("invalid device type, {}. Expected one of: input, output", s)),
        }
    }
}

fn parse_duration(value: &str) -> Result<std::time::Duration, String> {
    format_utils::parse_duration(value)
}
