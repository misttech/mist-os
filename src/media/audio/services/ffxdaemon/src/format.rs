// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl_fuchsia_audio_ffxdaemon::DeviceSelector,
    fidl_fuchsia_media::AudioSampleFormat,
    regex::Regex,
    std::io::{Cursor, Seek, SeekFrom, Write},
    std::{convert::From, str::FromStr, time::Duration},
};

pub const DURATION_REGEX: &'static str = r"^(\d+)(h|m|s|ms)$";

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Format {
    pub sample_type: fidl_fuchsia_media::AudioSampleFormat,
    pub frames_per_second: u32,
    pub channels: u32,
}

impl Format {
    pub const fn bytes_per_frame(&self) -> u32 {
        self.bytes_per_sample() * self.channels
    }

    pub const fn bytes_per_sample(&self) -> u32 {
        match self.sample_type {
            fidl_fuchsia_media::AudioSampleFormat::Unsigned8 => 1,
            fidl_fuchsia_media::AudioSampleFormat::Signed16 => 2,
            fidl_fuchsia_media::AudioSampleFormat::Signed24In32 => 4,
            fidl_fuchsia_media::AudioSampleFormat::Float => 4,
        }
    }

    pub fn frames_in_duration(&self, duration: std::time::Duration) -> u64 {
        (self.frames_per_second as f64 * duration.as_secs_f64()).ceil() as u64
    }

    pub const fn valid_bits_per_sample(&self) -> u32 {
        match self.sample_type {
            fidl_fuchsia_media::AudioSampleFormat::Unsigned8 => 8,
            fidl_fuchsia_media::AudioSampleFormat::Signed16 => 16,
            fidl_fuchsia_media::AudioSampleFormat::Signed24In32 => 24,
            fidl_fuchsia_media::AudioSampleFormat::Float => 32,
        }
    }

    pub const fn silence_value(&self) -> u8 {
        match self.sample_type {
            fidl_fuchsia_media::AudioSampleFormat::Unsigned8 => 128,
            _ => 0,
        }
    }

    pub fn wav_header_for_duration(&self, duration: std::time::Duration) -> Result<Vec<u8>, Error> {
        // A valid Wav File Header must have the data format and data length fields.
        // We need all values corresponding to wav header fields set on the cursor_writer
        // before writing to stdout.
        let mut cursor_writer = Cursor::new(Vec::<u8>::new());

        {
            // Creation of WavWriter writes the Wav File Header to cursor_writer.
            // This written header has the file size field and data chunk size field both set
            // to 0, since the number of samples (and resulting file and chunk sizes) are
            // unknown to the WavWriter at this point.
            let _writer = hound::WavWriter::new(&mut cursor_writer, self.into())
                .map_err(|e| anyhow::anyhow!("Failed to create WavWriter from spec: {}", e))?;
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

    pub fn is_supported_by(
        &self,
        supported_formats: &Vec<fidl_fuchsia_hardware_audio::SupportedFormats>,
    ) -> bool {
        let hardware_format = fidl_fuchsia_hardware_audio::Format::from(self);
        let mut is_format_supported = false;

        for supported_format in supported_formats {
            let pcm_formats = supported_format.pcm_supported_formats.as_ref().unwrap().clone();

            if pcm_formats.frame_rates.unwrap().contains(&self.frames_per_second)
                && pcm_formats.bytes_per_sample.unwrap().contains(&(self.bytes_per_sample() as u8))
                && pcm_formats
                    .sample_formats
                    .unwrap()
                    .contains(&hardware_format.pcm_format.unwrap().sample_format)
                && pcm_formats
                    .valid_bits_per_sample
                    .unwrap()
                    .contains(&(self.valid_bits_per_sample() as u8))
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

impl From<&hound::WavSpec> for Format {
    fn from(item: &hound::WavSpec) -> Self {
        Format {
            sample_type: match item.sample_format {
                hound::SampleFormat::Int => match item.bits_per_sample {
                    0..=8 => fidl_fuchsia_media::AudioSampleFormat::Unsigned8,
                    9..=16 => fidl_fuchsia_media::AudioSampleFormat::Signed16,
                    17.. => fidl_fuchsia_media::AudioSampleFormat::Signed24In32,
                },
                hound::SampleFormat::Float => fidl_fuchsia_media::AudioSampleFormat::Float,
            },
            frames_per_second: item.sample_rate,
            channels: item.channels as u32,
        }
    }
}

impl From<&fidl_fuchsia_media::AudioStreamType> for Format {
    fn from(item: &fidl_fuchsia_media::AudioStreamType) -> Self {
        Format {
            sample_type: item.sample_format,
            frames_per_second: item.frames_per_second,
            channels: item.channels,
        }
    }
}

impl From<&Format> for fidl_fuchsia_hardware_audio::Format {
    fn from(item: &Format) -> fidl_fuchsia_hardware_audio::Format {
        fidl_fuchsia_hardware_audio::Format {
            pcm_format: Some(fidl_fuchsia_hardware_audio::PcmFormat {
                number_of_channels: item.channels as u8,
                sample_format: match item.sample_type {
                    AudioSampleFormat::Unsigned8 => {
                        fidl_fuchsia_hardware_audio::SampleFormat::PcmUnsigned
                    }
                    AudioSampleFormat::Signed16 => {
                        fidl_fuchsia_hardware_audio::SampleFormat::PcmSigned
                    }
                    AudioSampleFormat::Signed24In32 => {
                        fidl_fuchsia_hardware_audio::SampleFormat::PcmSigned
                    }
                    AudioSampleFormat::Float => fidl_fuchsia_hardware_audio::SampleFormat::PcmFloat,
                },
                bytes_per_sample: item.bytes_per_sample() as u8,
                valid_bits_per_sample: item.valid_bits_per_sample() as u8,
                frame_rate: item.frames_per_second,
            }),
            ..Default::default()
        }
    }
}

impl From<&Format> for hound::WavSpec {
    fn from(item: &Format) -> Self {
        hound::WavSpec {
            channels: item.channels as u16,
            sample_format: match item.sample_type {
                fidl_fuchsia_media::AudioSampleFormat::Float => hound::SampleFormat::Float,
                _ => hound::SampleFormat::Int,
            },
            sample_rate: item.frames_per_second,
            bits_per_sample: match item.sample_type {
                fidl_fuchsia_media::AudioSampleFormat::Unsigned8 => 8,
                fidl_fuchsia_media::AudioSampleFormat::Signed16 => 16,
                fidl_fuchsia_media::AudioSampleFormat::Signed24In32 => 32,
                fidl_fuchsia_media::AudioSampleFormat::Float => 32,
            },
        }
    }
}

impl From<&Format> for fidl_fuchsia_media::AudioStreamType {
    fn from(item: &Format) -> Self {
        fidl_fuchsia_media::AudioStreamType {
            sample_format: item.sample_type,
            channels: item.channels,
            frames_per_second: item.frames_per_second,
        }
    }
}

impl FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        if s.len() == 0 {
            return Err(anyhow::anyhow!("No format specified."));
        };

        let splits: Vec<&str> = s.split(",").collect();

        if splits.len() != 3 {
            return Err(anyhow::anyhow!(
                "Expected 3 comma-separated values: 
            <SampleRate>,<SampleType>,<Channels> but have {}.",
                splits.len()
            ));
        }

        let frame_rate = match splits[0].parse::<u32>() {
            Ok(sample_rate) => Ok(sample_rate),
            Err(_) => Err(anyhow::anyhow!("First value (sample rate) should be an integer.")),
        }?;

        let sample_type = match CommandSampleType::from_str(splits[1]) {
            Ok(sample_type) => Ok(sample_type),
            Err(_) => Err(anyhow::anyhow!(
                "Second value (sample type) should be one of: uint8, int16, int32, float32."
            )),
        }?;

        let channels = match splits[2].strip_suffix("ch") {
            Some(channels) => match channels.parse::<u16>() {
                Ok(channels) => Ok(channels as u32),
                Err(_) => {
                    Err(anyhow::anyhow!("Third value (channels) should have form \"<uint>ch\"."))
                }
            },
            None => Err(anyhow::anyhow!("Channel argument should have form \"<uint>ch\".")),
        }?;

        Ok(Self {
            frames_per_second: frame_rate,
            sample_type: AudioSampleFormat::from(sample_type),
            channels,
        })
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
    fn from_str(s: &str) -> Result<Self, Error> {
        match s {
            "uint8" => Ok(CommandSampleType::Uint8),
            "int16" => Ok(CommandSampleType::Int16),
            "int32" => Ok(CommandSampleType::Int32),
            "float32" => Ok(CommandSampleType::Float32),
            _ => Err(anyhow::anyhow!("Invalid sampletype: {}.", s)),
        }
    }
}

impl From<CommandSampleType> for fidl_fuchsia_media::AudioSampleFormat {
    fn from(item: CommandSampleType) -> fidl_fuchsia_media::AudioSampleFormat {
        match item {
            CommandSampleType::Uint8 => fidl_fuchsia_media::AudioSampleFormat::Unsigned8,
            CommandSampleType::Int16 => fidl_fuchsia_media::AudioSampleFormat::Signed16,
            CommandSampleType::Int32 => fidl_fuchsia_media::AudioSampleFormat::Signed24In32,
            CommandSampleType::Float32 => fidl_fuchsia_media::AudioSampleFormat::Float,
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

// TODO(fxbug.dev/126775): Generalize to DAI & Codec types.
pub fn path_for_selector(device_selector: &DeviceSelector) -> Result<String, Error> {
    let input = device_selector
        .is_input
        .map(|is_input| if is_input { "input" } else { "output" })
        .ok_or(anyhow::anyhow!("Input/output missing"))?;
    let id = device_selector.id.clone().ok_or(anyhow::anyhow!("Device id missing"))?;
    Ok(format!("/dev/class/audio-{}/{}", input, id))
}

pub fn device_id_for_path(path: &std::path::Path) -> Result<String> {
    let device_id = path.file_name().ok_or(anyhow::anyhow!("Can't get filename from path"))?;
    let id_str =
        device_id.to_str().ok_or(anyhow::anyhow!("Could not convert device id to string"))?;
    Ok(id_str.to_string())
}

pub fn str_to_clock(src: &str) -> Result<fidl_fuchsia_audio_ffxdaemon::ClockType, String> {
    match src.to_lowercase().as_str() {
        "flexible" => Ok(fidl_fuchsia_audio_ffxdaemon::ClockType::Flexible(
            fidl_fuchsia_audio_ffxdaemon::Flexible,
        )),
        "monotonic" => Ok(fidl_fuchsia_audio_ffxdaemon::ClockType::Monotonic(
            fidl_fuchsia_audio_ffxdaemon::Monotonic,
        )),
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

                Ok(fidl_fuchsia_audio_ffxdaemon::ClockType::Custom(
                    fidl_fuchsia_audio_ffxdaemon::CustomClockInfo {
                        rate_adjust,
                        offset,
                        ..Default::default()
                    },
                ))
            } else {
                return Err(format!("Invalid clock argument: {}.", src));
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests
#[cfg(test)]
pub mod test {
    use super::*;

    fn example_formats() -> Vec<Format> {
        vec![
            Format {
                frames_per_second: 48000,
                sample_type: AudioSampleFormat::Unsigned8,
                channels: 2,
            },
            Format { frames_per_second: 44100, sample_type: AudioSampleFormat::Float, channels: 1 },
        ]
    }

    #[test]
    fn test_format_parse() {
        let example_formats = example_formats();

        pretty_assertions::assert_eq!(
            example_formats[0],
            Format::from_str("48000,uint8,2ch").unwrap()
        );

        pretty_assertions::assert_eq!(
            example_formats[1],
            Format::from_str("44100,float32,1ch").unwrap()
        );

        // malformed inputs
        assert!(Format::from_str("44100,float,1ch").is_err());

        assert!(Format::from_str("44100").is_err());

        assert!(Format::from_str("44100,float32,1").is_err());

        assert!(Format::from_str("44100,float32").is_err());

        assert!(Format::from_str(",,").is_err());
    }
}
