// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    errors::ffx_bail,
    fidl_fuchsia_media::AudioSampleFormat,
    hound::WavSpec,
    std::{convert::From, str::FromStr, time::Duration},
};

pub const DURATION_REGEX: &'static str = r"^(\d+)(h|m|s|ms)$";

// Size of WAV (RIFF) header is 44 bytes.
pub const WAV_HEADER_BYTE_COUNT: u32 = 44;

/// Parses a Duration from string.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let re = regex::Regex::new(DURATION_REGEX).unwrap();
    let captures = re
        .captures(&value)
        .ok_or(format!("Durations must be specified in the form {}.", DURATION_REGEX))?;
    let number: u64 = captures[1].parse().unwrap();
    let unit = &captures[2];

    match unit {
        "ms" => Ok(Duration::from_millis(number)),
        "s" => Ok(Duration::from_secs(number)),
        "m" => Ok(Duration::from_secs(number * 60)),
        "h" => Ok(Duration::from_secs(number * 3600)),
        _ => Err(format!(
            "Invalid duration string \"{}\"; must be of the form {}.",
            value, DURATION_REGEX
        )),
    }
}

pub fn wav_spec_to_sample_format(spec: WavSpec) -> AudioSampleFormat {
    match spec.bits_per_sample {
        0..=8 => AudioSampleFormat::Unsigned8,
        9..=16 => AudioSampleFormat::Signed16,
        17.. => match spec.sample_format {
            hound::SampleFormat::Int => AudioSampleFormat::Signed24In32,
            hound::SampleFormat::Float => AudioSampleFormat::Float,
        },
    }
}

pub fn bytes_per_frame(spec: WavSpec) -> u32 {
    let output_format = AudioOutputFormat::from(spec);
    output_format.bytes_per_frame()
}

#[derive(Debug, Eq, PartialEq)]
pub struct AudioOutputFormat {
    pub sample_rate: u32,
    pub sample_type: CommandSampleType,
    pub channels: u16,
}

impl AudioOutputFormat {
    pub fn bytes_per_sample(&self) -> u32 {
        match self.sample_type {
            CommandSampleType::Uint8 => 1,
            CommandSampleType::Int16 => 2,
            CommandSampleType::Int32 | CommandSampleType::Float32 => 4,
        }
    }

    pub fn bytes_per_frame(&self) -> u32 {
        self.bytes_per_sample() * self.channels as u32
    }
}

impl FromStr for AudioOutputFormat {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.len() == 0 {
            ffx_bail!("No format specified.")
        }

        let splits: Vec<&str> = s.split(",").collect();

        if splits.len() != 3 {
            ffx_bail!("Expected 3 comma-separated values: <SampleRate>,<SampleType>,<Channels> but have {}.", splits.len())
        }

        let sample_rate = match splits[0].parse::<u32>() {
            Ok(sample_rate) => sample_rate,
            Err(_) => ffx_bail!("First value (sample rate) should be an integer."),
        };

        let sample_type = match CommandSampleType::from_str(splits[1]) {
            Ok(sample_type) => sample_type,
            Err(_) => ffx_bail!(
                "Second value (sample type) should be one of: uint8, int16, int32, float32."
            ),
        };

        let channels = match splits[2].strip_suffix("ch") {
            Some(channels) => match channels.parse::<u16>() {
                Ok(channels) => channels,
                Err(_) => ffx_bail!("Third value (channels) should have form \"<uint>ch\"."),
            },
            None => ffx_bail!("Channel argument should have form \"<uint>ch\"."),
        };

        Ok(Self { sample_rate: sample_rate, sample_type: sample_type, channels: channels })
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum CommandSampleType {
    Uint8,
    Int16,
    Int32,
    Float32,
}

impl FromStr for CommandSampleType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "uint8" => Ok(CommandSampleType::Uint8),
            "int16" => Ok(CommandSampleType::Int16),
            "int32" => Ok(CommandSampleType::Int32),
            "float32" => Ok(CommandSampleType::Float32),
            _ => ffx_bail!("Invalid sampletype: {}.", s),
        }
    }
}

impl From<WavSpec> for AudioOutputFormat {
    fn from(item: WavSpec) -> Self {
        AudioOutputFormat {
            sample_rate: item.sample_rate,
            sample_type: match item.bits_per_sample {
                0..=8 => CommandSampleType::Uint8,
                9..=16 => CommandSampleType::Int16,
                17.. => match item.sample_format {
                    hound::SampleFormat::Int => CommandSampleType::Int32,
                    hound::SampleFormat::Float => CommandSampleType::Float32,
                },
            },
            channels: item.channels,
        }
    }
}

impl From<&AudioOutputFormat> for WavSpec {
    fn from(item: &AudioOutputFormat) -> Self {
        WavSpec {
            channels: item.channels,
            sample_rate: item.sample_rate,
            bits_per_sample: match item.sample_type {
                CommandSampleType::Uint8 => 8,
                CommandSampleType::Int16 => 16,
                CommandSampleType::Int32 => 32,
                CommandSampleType::Float32 => 32,
            },
            sample_format: match item.sample_type {
                CommandSampleType::Uint8 | CommandSampleType::Int16 | CommandSampleType::Int32 => {
                    hound::SampleFormat::Int
                }
                CommandSampleType::Float32 => hound::SampleFormat::Float,
            },
        }
    }
}

impl From<&AudioOutputFormat> for AudioSampleFormat {
    fn from(item: &AudioOutputFormat) -> Self {
        match item.sample_type {
            CommandSampleType::Uint8 => AudioSampleFormat::Unsigned8,
            CommandSampleType::Int16 => AudioSampleFormat::Signed16,
            CommandSampleType::Int32 => AudioSampleFormat::Signed24In32,
            CommandSampleType::Float32 => AudioSampleFormat::Float,
        }
    }
}

impl From<CommandSampleType> for fidl_fuchsia_audio::SampleType {
    fn from(item: CommandSampleType) -> fidl_fuchsia_audio::SampleType {
        match item {
            CommandSampleType::Uint8 => fidl_fuchsia_audio::SampleType::Uint8,
            CommandSampleType::Int16 => fidl_fuchsia_audio::SampleType::Int16,
            CommandSampleType::Int32 => fidl_fuchsia_audio::SampleType::Int32,
            CommandSampleType::Float32 => fidl_fuchsia_audio::SampleType::Float32,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
pub mod test {
    use super::*;

    fn example_formats() -> Vec<AudioOutputFormat> {
        vec![
            AudioOutputFormat {
                sample_rate: 48000,
                sample_type: CommandSampleType::Uint8,
                channels: 2,
            },
            AudioOutputFormat {
                sample_rate: 44100,
                sample_type: CommandSampleType::Float32,
                channels: 1,
            },
        ]
    }

    #[test]
    fn test_format_parse() {
        let example_formats = example_formats();

        pretty_assertions::assert_eq!(
            example_formats[0],
            AudioOutputFormat::from_str("48000,uint8,2ch").unwrap()
        );

        pretty_assertions::assert_eq!(
            example_formats[1],
            AudioOutputFormat::from_str("44100,float32,1ch").unwrap()
        );

        // malformed inputs
        assert!(AudioOutputFormat::from_str("44100,float,1ch").is_err());

        assert!(AudioOutputFormat::from_str("44100").is_err());

        assert!(AudioOutputFormat::from_str("44100,float32,1").is_err());

        assert!(AudioOutputFormat::from_str("44100,float32").is_err());

        assert!(AudioOutputFormat::from_str(",,").is_err());
    }
}
