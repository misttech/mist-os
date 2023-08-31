// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result, argh::FromArgs, ffx_core::ffx_command, fidl_fuchsia_media::AudioCaptureUsage,
    format_utils::Format, std::time::Duration,
};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "record",
    description = "Records audio data from audio_core AudioCapturer API and outputs a WAV file to stdout.",
    example = "$ ffx audio record --duration 1s --format 48000,uint8,1ch --usage SYSTEM-AGENT > ~/recording.wav"
)]
pub struct RecordCommand {
    #[argh(
        option,
        description = "duration of output signal. Examples: 5ms or 3s. If not specified,\
        press ENTER to stop recording.",
        from_str_fn(parse_duration)
    )]
    pub duration: Option<Duration>,

    #[argh(option, description = "output format (see 'ffx audio help' for more information).")]
    pub format: Format,

    #[argh(
        option,
        description = "purpose of the stream being recorded.\
        Accepted values: BACKGROUND, FOREGROUND, SYSTEM-AGENT, COMMUNICATION, ULTRASOUND,\
        or LOOPBACK. Default: COMMUNICATION.",
        from_str_fn(str_to_usage),
        default = "AudioCaptureUsageExtended::Communication(AudioCaptureUsage::Communication)"
    )]
    pub usage: AudioCaptureUsageExtended,

    #[argh(
        option,
        description = "buffer size (bytes) to allocate on device VMO.\
        Used to retrieve audio data from AudioCapturer.\
        Defaults to size to hold 1 second of audio data."
    )]
    pub buffer_size: Option<u64>,

    #[argh(
        option,
        description = "explicitly set the capturer's reference clock. By default,\
        SetReferenceClock is not called, which leads to a flexible clock. \
        Options include: 'flexible', 'monotonic', and 'custom,<rate adjustment>,<offset>' where \
        rate adjustment and offset are integers. To set offset without rate adjustment, pass 0\
        in place of rate adjustment.",
        from_str_fn(str_to_clock),
        default = "fidl_fuchsia_audio_controller::ClockType::Flexible(fidl_fuchsia_audio_controller::Flexible)"
    )]
    pub clock: fidl_fuchsia_audio_controller::ClockType,

    #[argh(
        option,
        description = "gain (decibels) for the capturer. Default: 0 dB",
        default = "0.0f32"
    )]
    pub gain: f32,

    #[argh(option, description = "mute the capturer. Default: false", default = "false")]
    pub mute: bool,
}

#[derive(Debug, PartialEq)]
pub enum AudioCaptureUsageExtended {
    Background(AudioCaptureUsage),
    Foreground(AudioCaptureUsage),
    SystemAgent(AudioCaptureUsage),
    Communication(AudioCaptureUsage),
    Ultrasound,
    Loopback,
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
        "ULTRASOUND" => Ok(AudioCaptureUsageExtended::Ultrasound),
        "LOOPBACK" => Ok(AudioCaptureUsageExtended::Loopback),
        _ => Err(String::from(
            "Couldn't parse usage. Expected one of:
        BACKGROUND, FOREGROUND, SYSTEM-AGENT, COMMUNICATION, ULTRASOUND, or LOOPBACK",
        )),
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    format_utils::parse_duration(value)
}

fn str_to_clock(value: &str) -> Result<fidl_fuchsia_audio_controller::ClockType, String> {
    format_utils::str_to_clock(value)
}
