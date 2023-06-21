// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {anyhow::Result, argh::FromArgs, ffx_core::ffx_command, fidl_fuchsia_media::AudioRenderUsage};

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "play",
    description = "Reads a WAV file from stdin and sends the audio data to audio_core AudioRenderer API.",
    example = "$ ffx audio gen sine --duration 1s --frequency 440 --amplitude 0.5 --format 48000,int16,2ch | ffx audio play \n\
            $ ffx audio play --file ~/path/to/sine.wav"
)]
pub struct PlayCommand {
    #[argh(
        option,
        description = "purpose of the stream being used to render audio.\
        Accepted values: BACKGROUND, MEDIA, SYSTEM-AGENT, COMMUNICATION, INTERRUPTION,\
        ULTRASOUND. Default: MEDIA.",
        from_str_fn(str_to_usage),
        default = "AudioRenderUsageExtended::Media(AudioRenderUsage::Media)"
    )]
    pub usage: AudioRenderUsageExtended,

    #[argh(
        option,
        description = "buffer size (bytes) to allocate on device VMO.\
        Used to send audio data from ffx tool to AudioRenderer.\
        Defaults to size to hold 1 second of audio data. "
    )]
    pub buffer_size: Option<u32>,

    #[argh(
        option,
        description = "how many packets to use when sending data to an AudioRenderer.\
        Defaults to 4 packets."
    )]
    pub packet_count: Option<u32>,

    #[argh(
        option,
        description = "gain (decibels) for the renderer. Default: 0 dB",
        default = "0.0f32"
    )]
    pub gain: f32,

    #[argh(option, description = "mute the renderer. Default: false", default = "false")]
    pub mute: bool,

    #[argh(
        option,
        description = "explicitly set the renderer's reference clock. By default,\
        SetReferenceClock is not called, which leads to a flexible clock. \
        Options include: 'flexible', 'monotonic', and 'custom,<rate adjustment>,<offset>' where \
        rate adjustment and offset are integers. To set offset without rate adjustment, pass 0\
        in place of rate adjustment.",
        from_str_fn(str_to_clock),
        default = "fidl_fuchsia_audio_ffxdaemon::ClockType::Flexible(fidl_fuchsia_audio_ffxdaemon::Flexible)"
    )]
    pub clock: fidl_fuchsia_audio_ffxdaemon::ClockType,

    #[argh(
        option,
        description = "file in WAV format containing audio signal. If not specified,\
        ffx command will read from stdin."
    )]
    pub file: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum AudioRenderUsageExtended {
    Background(AudioRenderUsage),
    Media(AudioRenderUsage),
    SystemAgent(AudioRenderUsage),
    Communication(AudioRenderUsage),
    Interruption(AudioRenderUsage),
    Ultrasound,
}

fn str_to_usage(src: &str) -> Result<AudioRenderUsageExtended, String> {
    match src.to_uppercase().as_str() {
        "BACKGROUND" => Ok(AudioRenderUsageExtended::Background(AudioRenderUsage::Background)),
        "MEDIA" => Ok(AudioRenderUsageExtended::Media(AudioRenderUsage::Media)),
        "INTERRUPTION" => {
            Ok(AudioRenderUsageExtended::Interruption(AudioRenderUsage::Interruption))
        }
        "SYSTEM-AGENT" => Ok(AudioRenderUsageExtended::SystemAgent(AudioRenderUsage::SystemAgent)),
        "COMMUNICATION" => {
            Ok(AudioRenderUsageExtended::Communication(AudioRenderUsage::Communication))
        }
        "ULTRASOUND" => Ok(AudioRenderUsageExtended::Ultrasound),
        _ => Err(String::from("Couldn't parse usage.")),
    }
}

fn str_to_clock(src: &str) -> Result<fidl_fuchsia_audio_ffxdaemon::ClockType, String> {
    format_utils::str_to_clock(src)
}
