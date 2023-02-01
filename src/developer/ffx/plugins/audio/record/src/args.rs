// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, argh::FromArgs, ffx_core::ffx_command, fidl_fuchsia_media::AudioCaptureUsage,
    format_utils::Format, std::time::Duration,
};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "record", description = "record", example = "ffx audio record")]
pub struct RecordCommand {
    #[argh(
        option,
        description = "duration of output signal. Examples: 5ms or 3s.",
        from_str_fn(parse_duration)
    )]
    pub duration: Duration,

    #[argh(option, description = "output format (see 'ffx audio help' for more information).")]
    pub format: Format,

    #[argh(subcommand)]
    pub subcommand: SubCommand,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
pub enum SubCommand {
    Capture(CaptureCommand),
}

#[derive(Debug, PartialEq)]
pub enum AudioCaptureUsageExtended {
    Background(AudioCaptureUsage),
    Foreground(AudioCaptureUsage),
    SystemAgent(AudioCaptureUsage),
    Communication(AudioCaptureUsage),
    Loopback,
}
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "capture", description = "Retrieve audio data from AudioCapturer.")]
pub struct CaptureCommand {
    #[argh(
        option,
        description = "purpose of the stream being used to capture audio. One of: 
        BACKGROUND, FOREGROUND, SYSTEM-AGENT, COMMUNICATION, or LOOPBACK",
        from_str_fn(str_to_usage)
    )]
    pub usage: AudioCaptureUsageExtended,

    #[argh(
        option,
        description = "buffer size (bytes) to allocate on device VMO. Used to send audio data from 
        ffx tool to AudioRenderer. If not specified, defaults to size to hold 1 second of data."
    )]
    pub buffer_size: Option<u64>,

    #[argh(option, description = "gain (in decibels) for the capturer.")]
    pub gain: f32,

    #[argh(option, description = "mute the capturer.")]
    pub mute: bool,
}

fn str_to_usage(src: &str) -> Result<AudioCaptureUsageExtended, String> {
    match src.to_uppercase().as_str() {
        "BACKGROUND" => Ok(AudioCaptureUsageExtended::Background(AudioCaptureUsage::Background)),
        "FOREGROUND" => Ok(AudioCaptureUsageExtended::Foreground(AudioCaptureUsage::Foreground)),
        "SYSTEM-AGENT" => {
            Ok(AudioCaptureUsageExtended::SystemAgent(AudioCaptureUsage::SystemAgent))
        }
        "COMMUNICATION" => {
            Ok(AudioCaptureUsageExtended::Communication(AudioCaptureUsage::Communication))
        }
        "LOOPBACK" => Ok(AudioCaptureUsageExtended::Loopback),
        _ => Err(String::from(
            "Couldn't parse usage. Expected one of: 
        BACKGROUND, FOREGROUND, SYSTEM-AGENT, COMMUNICATION, or LOOPBACK",
        )),
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    format_utils::parse_duration(value)
}
