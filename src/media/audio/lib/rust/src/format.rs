// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error, Result};
use fidl_fuchsia_audio as faudio;
use fidl_fuchsia_audio_controller as fac;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_media as fmedia;
use regex::Regex;
use std::fmt::Display;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::{str::FromStr, time::Duration};

pub const DURATION_REGEX: &'static str = r"^(\d+)(h|m|s|ms)$";

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct Format {
    pub sample_type: SampleType,
    pub frames_per_second: u32,
    pub channels: u32,
}

impl Format {
    pub const fn bytes_per_frame(&self) -> u32 {
        self.sample_type.bytes_per_sample() * self.channels
    }

    pub fn frames_in_duration(&self, duration: std::time::Duration) -> u64 {
        (self.frames_per_second as f64 * duration.as_secs_f64()).ceil() as u64
    }

    pub fn wav_header_for_duration(&self, duration: Duration) -> Result<Vec<u8>, Error> {
        // A valid Wav File Header must have the data format and data length fields.
        // We need all values corresponding to wav header fields set on the cursor_writer
        // before writing to stdout.
        let mut cursor_writer = Cursor::new(Vec::<u8>::new());

        {
            // Creation of WavWriter writes the Wav File Header to cursor_writer.
            // This written header has the file size field and data chunk size field both set
            // to 0, since the number of samples (and resulting file and chunk sizes) are
            // unknown to the WavWriter at this point.
            let _writer = hound::WavWriter::new(&mut cursor_writer, (*self).into())
                .map_err(|e| anyhow!("Failed to create WavWriter from spec: {}", e))?;
        }

        // The file and chunk size fields are set to 0 as placeholder values by the
        // construction of the WavWriter above. We can compute the actual values based on the
        // command arguments for format and duration, and set the file size and chunk size
        // fields to the computed values in the cursor_writer before writing to stdout.

        let bytes_to_capture: u32 =
            self.frames_in_duration(duration) as u32 * self.bytes_per_frame();
        let total_header_bytes = 44;
        // The File Size field of a WAV header. 32-bit int starting at position 4, represents
        // the size of the overall file minus 8 bytes (exclude RIFF description and
        // file size description)
        let file_size_bytes: u32 = bytes_to_capture as u32 + total_header_bytes - 8;

        cursor_writer.seek(SeekFrom::Start(4))?;
        cursor_writer.write_all(&file_size_bytes.to_le_bytes()[..])?;

        // Data size field of a WAV header. For PCM, this is a 32-bit int starting at
        // position 40 and represents the size of the data section.
        cursor_writer.seek(SeekFrom::Start(40))?;
        cursor_writer.write_all(&bytes_to_capture.to_le_bytes()[..])?;

        // Write the completed WAV header to stdout. We then write the raw sample
        // values from the packets received directly to stdout.
        Ok(cursor_writer.into_inner())
    }

    pub fn is_supported_by(&self, supported_formats: &Vec<fhaudio::SupportedFormats>) -> bool {
        let hardware_format = fhaudio::Format::from(self.clone());
        let mut is_format_supported = false;

        for supported_format in supported_formats {
            let pcm_formats = supported_format.pcm_supported_formats.as_ref().unwrap().clone();

            if pcm_formats.frame_rates.unwrap().contains(&self.frames_per_second)
                && pcm_formats
                    .bytes_per_sample
                    .unwrap()
                    .contains(&(self.sample_type.bytes_per_sample() as u8))
                && pcm_formats
                    .sample_formats
                    .unwrap()
                    .contains(&hardware_format.pcm_format.unwrap().sample_format)
                && pcm_formats
                    .valid_bits_per_sample
                    .unwrap()
                    .contains(&(self.sample_type.valid_bits_per_sample() as u8))
                && pcm_formats.channel_sets.unwrap().into_iter().any(|channel_set| {
                    channel_set.attributes.unwrap().len() == self.channels as usize
                })
            {
                is_format_supported = true;
                break;
            }
        }
        is_format_supported
    }
}

impl From<hound::WavSpec> for Format {
    fn from(value: hound::WavSpec) -> Self {
        Format {
            sample_type: (value.sample_format, value.bits_per_sample).into(),
            frames_per_second: value.sample_rate,
            channels: value.channels as u32,
        }
    }
}

impl From<fmedia::AudioStreamType> for Format {
    fn from(value: fmedia::AudioStreamType) -> Self {
        Format {
            sample_type: value.sample_format.into(),
            frames_per_second: value.frames_per_second,
            channels: value.channels,
        }
    }
}

impl From<Format> for fhaudio::PcmFormat {
    fn from(value: Format) -> Self {
        Self {
            number_of_channels: value.channels as u8,
            sample_format: value.sample_type.into(),
            bytes_per_sample: value.sample_type.bytes_per_sample() as u8,
            valid_bits_per_sample: value.sample_type.valid_bits_per_sample() as u8,
            frame_rate: value.frames_per_second,
        }
    }
}

impl From<Format> for fhaudio::Format {
    fn from(value: Format) -> Self {
        fhaudio::Format { pcm_format: Some(value.into()), ..Default::default() }
    }
}

impl From<Format> for faudio::Format {
    fn from(value: Format) -> Self {
        Self {
            sample_type: Some(value.sample_type.into()),
            channel_count: Some(value.channels),
            frames_per_second: Some(value.frames_per_second),
            channel_layout: {
                let channel_config = match value.channels {
                    1 => faudio::ChannelConfig::Mono,
                    2 => faudio::ChannelConfig::Stereo,
                    // The following are just a guess.
                    3 => faudio::ChannelConfig::Surround3,
                    4 => faudio::ChannelConfig::Surround4,
                    6 => faudio::ChannelConfig::Surround51,
                    _ => panic!("channel count not representable as a ChannelConfig"),
                };
                Some(faudio::ChannelLayout::Config(channel_config))
            },
            ..Default::default()
        }
    }
}

impl TryFrom<faudio::Format> for Format {
    type Error = String;

    fn try_from(value: faudio::Format) -> Result<Self, Self::Error> {
        let sample_type = value.sample_type.ok_or_else(|| "missing sample_type".to_string())?;
        let channel_count =
            value.channel_count.ok_or_else(|| "missing channel_count".to_string())?;
        let frames_per_second =
            value.frames_per_second.ok_or_else(|| "missing frames_per_second".to_string())?;
        Ok(Self {
            sample_type: SampleType::try_from(sample_type)?,
            channels: channel_count,
            frames_per_second,
        })
    }
}

impl From<Format> for hound::WavSpec {
    fn from(value: Format) -> Self {
        Self {
            channels: value.channels as u16,
            sample_format: value.sample_type.into(),
            sample_rate: value.frames_per_second,
            bits_per_sample: value.sample_type.bits_per_sample() as u16,
        }
    }
}

impl From<Format> for fmedia::AudioStreamType {
    fn from(value: Format) -> Self {
        Self {
            sample_format: value.sample_type.into(),
            channels: value.channels,
            frames_per_second: value.frames_per_second,
        }
    }
}

impl Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{},{}ch", self.frames_per_second, self.sample_type, self.channels)
    }
}

impl FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        if s.len() == 0 {
            return Err(anyhow!("No format specified."));
        };

        let splits: Vec<&str> = s.split(",").collect();

        if splits.len() != 3 {
            return Err(anyhow!(
                "Expected 3 comma-separated values: 
            <SampleRate>,<SampleType>,<Channels> but have {}.",
                splits.len()
            ));
        }

        let frame_rate = match splits[0].parse::<u32>() {
            Ok(sample_rate) => Ok(sample_rate),
            Err(_) => Err(anyhow!("First value (sample rate) should be an integer.")),
        }?;

        let sample_type = match SampleType::from_str(splits[1]) {
            Ok(sample_type) => Ok(sample_type),
            Err(_) => Err(anyhow!(
                "Second value (sample type) should be one of: uint8, int16, int32, float32."
            )),
        }?;

        let channels = match splits[2].strip_suffix("ch") {
            Some(channels) => match channels.parse::<u32>() {
                Ok(channels) => Ok(channels),
                Err(_) => Err(anyhow!("Third value (channels) should have form \"<uint>ch\".")),
            },
            None => Err(anyhow!("Channel argument should have form \"<uint>ch\".")),
        }?;

        Ok(Self { frames_per_second: frame_rate, sample_type, channels })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleType {
    Uint8,
    Int16,
    Int32,
    Float32,
}

impl SampleType {
    pub const fn bytes_per_sample(&self) -> u32 {
        match self {
            Self::Uint8 => 1,
            Self::Int16 => 2,
            Self::Int32 => 4,
            Self::Float32 => 4,
        }
    }

    pub const fn bits_per_sample(&self) -> u32 {
        match self {
            Self::Uint8 => 8,
            Self::Int16 => 16,
            Self::Int32 => 32,
            Self::Float32 => 32,
        }
    }

    pub const fn valid_bits_per_sample(&self) -> u32 {
        match self {
            Self::Uint8 => 8,
            Self::Int16 => 16,
            // Assuming Int32 is really "24 in 32".
            Self::Int32 => 24,
            Self::Float32 => 32,
        }
    }

    pub const fn silence_value(&self) -> u8 {
        match self {
            Self::Uint8 => 128,
            _ => 0,
        }
    }
}

impl Display for SampleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Uint8 => "uint8",
            Self::Int16 => "int16",
            Self::Int32 => "int32",
            Self::Float32 => "float32",
        };
        f.write_str(s)
    }
}

impl FromStr for SampleType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "uint8" => Ok(Self::Uint8),
            "int16" => Ok(Self::Int16),
            "int32" => Ok(Self::Int32),
            "float32" => Ok(Self::Float32),
            _ => Err(anyhow!("Invalid sampletype: {}.", s)),
        }
    }
}

impl From<SampleType> for fmedia::AudioSampleFormat {
    fn from(item: SampleType) -> fmedia::AudioSampleFormat {
        match item {
            SampleType::Uint8 => Self::Unsigned8,
            SampleType::Int16 => Self::Signed16,
            SampleType::Int32 => Self::Signed24In32,
            SampleType::Float32 => Self::Float,
        }
    }
}

impl From<SampleType> for faudio::SampleType {
    fn from(value: SampleType) -> Self {
        match value {
            SampleType::Uint8 => Self::Uint8,
            SampleType::Int16 => Self::Int16,
            SampleType::Int32 => Self::Int32,
            SampleType::Float32 => Self::Float32,
        }
    }
}

impl From<SampleType> for fhaudio::SampleFormat {
    fn from(value: SampleType) -> Self {
        match value {
            SampleType::Uint8 => Self::PcmUnsigned,
            SampleType::Int16 | SampleType::Int32 => Self::PcmSigned,
            SampleType::Float32 => Self::PcmFloat,
        }
    }
}

impl From<SampleType> for hound::SampleFormat {
    fn from(value: SampleType) -> Self {
        match value {
            SampleType::Uint8 | SampleType::Int16 | SampleType::Int32 => Self::Int,
            SampleType::Float32 => Self::Float,
        }
    }
}

impl From<fmedia::AudioSampleFormat> for SampleType {
    fn from(item: fmedia::AudioSampleFormat) -> Self {
        match item {
            fmedia::AudioSampleFormat::Unsigned8 => Self::Uint8,
            fmedia::AudioSampleFormat::Signed16 => Self::Int16,
            fmedia::AudioSampleFormat::Signed24In32 => Self::Int32,
            fmedia::AudioSampleFormat::Float => Self::Float32,
        }
    }
}

impl TryFrom<faudio::SampleType> for SampleType {
    type Error = String;

    fn try_from(value: faudio::SampleType) -> Result<Self, Self::Error> {
        match value {
            faudio::SampleType::Uint8 => Ok(Self::Uint8),
            faudio::SampleType::Int16 => Ok(Self::Int16),
            faudio::SampleType::Int32 => Ok(Self::Int32),
            faudio::SampleType::Float32 => Ok(Self::Float32),
            other => Err(format!("unsupported SampleType: {:?}", other)),
        }
    }
}

impl From<(hound::SampleFormat, u16)> for SampleType {
    fn from(value: (hound::SampleFormat, u16)) -> Self {
        let (sample_format, bits_per_sample) = value;
        match sample_format {
            hound::SampleFormat::Int => match bits_per_sample {
                0..=8 => Self::Uint8,
                9..=16 => Self::Int16,
                17.. => Self::Int32,
            },
            hound::SampleFormat::Float => Self::Float32,
        }
    }
}

/// Parses a Duration from string.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let re = Regex::new(DURATION_REGEX).map_err(|e| format!("Could not create regex: {}", e))?;
    let captures = re
        .captures(&value)
        .ok_or(format!("Durations must be specified in the form {}.", DURATION_REGEX))?;
    let number: u64 = captures[1].parse().map_err(|e| format!("Could not parse number: {}", e))?;
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

pub fn str_to_clock(src: &str) -> Result<fac::ClockType, String> {
    match src.to_lowercase().as_str() {
        "flexible" => Ok(fac::ClockType::Flexible(fac::Flexible)),
        "monotonic" => Ok(fac::ClockType::SystemMonotonic(fac::SystemMonotonic)),
        _ => {
            let splits: Vec<&str> = src.split(",").collect();
            if splits[0] == "custom" {
                let rate_adjust = match splits[1].parse::<i32>() {
                    Ok(rate_adjust) => Some(rate_adjust),
                    Err(_) => None,
                };

                let offset = match splits[2].parse::<i32>() {
                    Ok(offset) => Some(offset),
                    Err(_) => None,
                };

                Ok(fac::ClockType::Custom(fac::CustomClockConfig {
                    rate_adjust,
                    offset,
                    ..Default::default()
                }))
            } else {
                Err(format!("Invalid clock argument: {}.", src))
            }
        }
    }
}

#[cfg(test)]
pub mod test {
    use super::*;

    #[test]
    fn test_format_parse() {
        assert_eq!(
            Format { frames_per_second: 48000, sample_type: SampleType::Uint8, channels: 2 },
            Format::from_str("48000,uint8,2ch").unwrap()
        );

        assert_eq!(
            Format { frames_per_second: 44100, sample_type: SampleType::Float32, channels: 1 },
            Format::from_str("44100,float32,1ch").unwrap()
        );

        // malformed inputs
        assert!(Format::from_str("44100,float,1ch").is_err());

        assert!(Format::from_str("44100").is_err());

        assert!(Format::from_str("44100,float32,1").is_err());

        assert!(Format::from_str("44100,float32").is_err());

        assert!(Format::from_str(",,").is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(
            Format { frames_per_second: 48000, sample_type: SampleType::Uint8, channels: 2 }
                .to_string(),
            "48000,uint8,2ch",
        );

        assert_eq!(
            Format { frames_per_second: 44100, sample_type: SampleType::Float32, channels: 1 }
                .to_string(),
            "44100,float32,1ch"
        );
    }
}
