// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs};
use ffx_core::ffx_command;
use fuchsia_audio::{
    device::{Direction, HardwareType},
    Format,
};

#[ffx_command()]
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "device",
    description = "Interact directly with device hardware.",
    example = "Show information about a specific device:

    $ ffx audio device --id 3d99d780 --direction input info

Play a WAV file directly to device hardware:

    $ cat ~/sine.wav | ffx audio device --id a70075f2 play
    $ ffx audio device --id a70075f2 play --file ~/sine.wav

Record a WAV file directly from device hardware:

    $ ffx audio device --id 3d99d780 record --format 48000,uint8,1ch --duration 1s

Mute the stream of an output device:

    $ ffx audio device --id a70075f2 --direction output mute

Set the gain of an output device to -20 dB:

    $ ffx audio device --id a70075f2 --direction output gain -20

Turn AGC on for an input device:

    $ ffx audio device --id 3d99d780 --direction input agc on"
)]
pub struct DeviceCommand {
    #[argh(subcommand)]
    pub subcommand: SubCommand,

    #[argh(
        option,
        description = "device ID. Specify a devfs node name, \
        e.g. 3d99d780 for /dev/class/audio-input/3d99d780.
        If not specified, defaults to the first device alphabetically listed."
    )]
    pub id: Option<String>,

    #[argh(
        option,
        long = "direction",
        description = "device direction. Accepted values: input, output. \
        Play and record will use output and input respectively by default."
    )]
    pub device_direction: Option<Direction>,

    #[argh(
        option,
        long = "type",
        description = "device type. Accepted values: StreamConfig, Composite."
    )]
    pub device_type: Option<HardwareType>,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum SubCommand {
    List(ListCommand),
    Info(InfoCommand),
    Play(DevicePlayCommand),
    Record(DeviceRecordCommand),
    Gain(DeviceGainCommand),
    Mute(DeviceMuteCommand),
    Unmute(DeviceUnmuteCommand),
    Agc(DeviceAgcCommand),
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "list",
    description = "Lists audio devices.",
    example = "ffx audio device --type StreamConfig list"
)]
pub struct ListCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "info",
    description = "Show information about a specific audio device.",
    example = "ffx audio device --type StreamConfig --direction input info"
)]
pub struct InfoCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "play", description = "Send audio data directly to device ring buffer.")]
pub struct DevicePlayCommand {
    #[argh(
        option,
        description = "file in WAV format containing audio signal. \
        If not specified, ffx command will read from stdin."
    )]
    pub file: Option<String>,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "record", description = "Capture audio data directly from ring buffer.")]
pub struct DeviceRecordCommand {
    #[argh(
        option,
        description = "duration of output signal. Examples: 5ms or 3s. \
        If not specified, press ENTER to stop recording.",
        from_str_fn(parse_duration)
    )]
    pub duration: Option<std::time::Duration>,

    #[argh(option, description = "output format (see 'ffx audio help' for more information).")]
    pub format: Format,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "gain",
    description = "Request to set the gain of the stream, in decibels."
)]
pub struct DeviceGainCommand {
    #[argh(option, description = "gain, in decibels, to set the stream to.")]
    pub gain: f32,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "mute", description = "Request to mute a stream.")]
pub struct DeviceMuteCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "unmute", description = "Request to unmute a stream.")]
pub struct DeviceUnmuteCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "agc",
    description = "Request to enable or disable automatic gain control for the stream."
)]
pub struct DeviceAgcCommand {
    #[argh(
        positional,
        description = "enable or disable AGC. Accepted values: on, off",
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

fn parse_duration(value: &str) -> Result<std::time::Duration, String> {
    fuchsia_audio::parse_duration(value)
}
