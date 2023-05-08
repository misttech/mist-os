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
    \t$ ffx audio device --id 000 --direction input info \n\n\
    Play a wav file directly to device hardware: \n\
    \t$ cat ~/sine.wav | ffx audio device --id 000 play \n\n
    Record a wav file directly from device hardware: \n\
    \t$ ffx audio device --id 000 record --format 48000,uint8,1ch --duration 1s \n\n
    Mute the stream of the output device: \n
    \t$ ffx audio device --id 000 --direction output mute \n\n
    Set the gain of the output device to -20 dB: \n
    \t$ ffx audio device --id 000 --direction output gain -20 \n\n
    Turn agc on for the input device: \n
    \t$ ffx audio device --id 000 --direction input agc on"
)]
pub struct DeviceCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,

    #[argh(
        option,
        description = "device id. stream device node id from either /dev/audio-input/* or /dev/audio-output/*"
    )]
    pub id: String,

    #[argh(
        option,
        long = "direction",
        description = "device direction. Accepted values: input, output. \
        Play and record will use output and input respectively by default."
    )]
    pub device_direction: Option<DeviceDirection>,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum SubCommand {
    Info(InfoCommand),
    Play(DevicePlayCommand),
    Record(DeviceRecordCommand),
    Gain(DeviceGainCommand),
    Mute(DeviceMuteCommand),
    Unmute(DeviceUnmuteCommand),
    Agc(DeviceAgcCommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "info", description = "List information about a specific audio device.")]
pub struct InfoCommand {
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

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "gain",
    description = "Request to set the gain of the stream in decibels."
)]
pub struct DeviceGainCommand {
    #[argh(option, description = "gain (in decibles) to set the stream to.")]
    pub gain: f32,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "mute", description = "Request to mute a stream.")]
pub struct DeviceMuteCommand {}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "unmute", description = "Request to unmute a stream.")]
pub struct DeviceUnmuteCommand {}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "agc",
    description = "Request to enable or disable automatic gain control for the stream."
)]
pub struct DeviceAgcCommand {
    #[argh(
        positional,
        description = "enable or disable agc. Accepted values: on, off",
        from_str_fn(string_to_enable)
    )]
    pub enable: bool,
}

fn string_to_enable(value: &str) -> Result<bool, String> {
    if value == "on" {
        Ok(true)
    } else if value == "off" {
        Ok(false)
    } else {
        Err(format!("Expected one of: on, off"))
    }
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
pub enum DeviceDirection {
    Input,
    Output,
}

impl FromStr for DeviceDirection {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, anyhow::Error> {
        match s.to_lowercase().as_str() {
            "input" => Ok(DeviceDirection::Input),
            "output" => Ok(DeviceDirection::Output),
            _ => Err(anyhow::anyhow!("invalid device type, {}. Expected one of: input, output", s)),
        }
    }
}

fn parse_duration(value: &str) -> Result<std::time::Duration, String> {
    format_utils::parse_duration(value)
}
