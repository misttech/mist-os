// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::serde_ext;
use camino::Utf8PathBuf;
use fidl_fuchsia_audio_controller as fac;
use fuchsia_audio::{
    device::{
        ClockDomain, GainCapabilities, GainState, PlugDetectCapabilities, PlugState, Selector,
        UniqueInstanceId,
    },
    format_set::{ChannelAttributes, ChannelSet, PcmFormatSet},
};
use itertools::Itertools;
use lazy_static::lazy_static;
use prettytable::{cell, format, row, Table};
use serde::Serialize;
use std::fmt::Display;

lazy_static! {
    // No padding, no borders.
    pub static ref TABLE_FORMAT_EMPTY: format::TableFormat = format::FormatBuilder::new().build();

    // Left padding, used for the outer table.
    pub static ref TABLE_FORMAT_NORMAL: format::TableFormat = format::FormatBuilder::new().padding(2, 0).build();

    // Right padding, no borders, used for channel sets/attributes.
    pub static ref TABLE_FORMAT_CHANNEL_SETS: format::TableFormat = format::FormatBuilder::new().padding(0, 2).build();

    // With borders, used nested tables.
    pub static ref TABLE_FORMAT_NESTED: format::TableFormat = format::FormatBuilder::new()
        .borders('│')
        .separators(&[format::LinePosition::Top], format::LineSeparator::new('─', '┬', '┌', '┐'))
        .separators(&[format::LinePosition::Bottom], format::LineSeparator::new('─', '┴', '└', '┘'))
        .padding(1, 1)
        .build();
}

// The Zircon Rust library cannot be built on host, so we can't use `zx::Time`.
type ZxTime = i64;

/// Formatter for [GainState].
struct GainStateText<'a>(&'a GainState);

impl Display for GainStateText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let muted = match self.0.muted {
            Some(muted) => {
                if muted {
                    "muted"
                } else {
                    "unmuted"
                }
            }
            None => "Muted not available",
        };

        let agc_enabled = match self.0.agc_enabled {
            Some(agc) => {
                if agc {
                    "AGC on"
                } else {
                    "AGC off"
                }
            }
            None => "AGC not available",
        };

        write!(f, "{} dB ({}, {})", self.0.gain_db, muted, agc_enabled)
    }
}

/// Formatter for [GainCapabilities].
struct GainCapabilitiesText<'a>(&'a GainCapabilities);

impl Display for GainCapabilitiesText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gain_range = if self.0.min_gain_db == self.0.max_gain_db {
            format!("Fixed {} dB gain", self.0.min_gain_db)
        } else {
            format!("Gain range [{} dB, {} dB]", self.0.min_gain_db, self.0.max_gain_db)
        };

        let gain_step = if self.0.gain_step_db == 0.0f32 {
            "0 dB step (continuous)".to_string()
        } else {
            format!("{} dB step", self.0.gain_step_db)
        };

        let can_mute = match self.0.can_mute {
            Some(can_mute) => {
                if can_mute {
                    "can mute"
                } else {
                    "cannot mute"
                }
            }
            None => "Can Mute unavailable",
        };

        let can_agc = match self.0.can_agc {
            Some(can_agc) => {
                if can_agc {
                    "can AGC"
                } else {
                    "cannot AGC"
                }
            }
            None => "Can AGC unavailable",
        };

        write!(f, "{}; {}; {}; {}", gain_range, gain_step, can_mute, can_agc)
    }
}

/// Formatter for a vector of [ChannelSet]s.
struct ChannelSetsText<'a>(&'a Vec<ChannelSet>);

impl Display for ChannelSetsText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_CHANNEL_SETS);
        for channel_set in self.0 {
            let key = format!(
                "{} {}:",
                channel_set.attributes.len(),
                if channel_set.attributes.len() == 1 { "channel" } else { "channels" }
            );
            let value = ChannelSetText(&channel_set);
            table.add_row(row!(key, value));
        }
        table.fmt(f)
    }
}

/// Formatter for [ChannelSet].
struct ChannelSetText<'a>(&'a ChannelSet);

impl Display for ChannelSetText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_CHANNEL_SETS);
        for (idx, attributes) in self.0.attributes.iter().enumerate() {
            let key = format!("Channel {}:", idx + 1);
            let value = ChannelAttributesText(&attributes);
            table.add_row(row!(key, value));
        }
        table.fmt(f)
    }
}

/// Formatter for [ChannelAttributes].
struct ChannelAttributesText<'a>(&'a ChannelAttributes);

impl Display for ChannelAttributesText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_CHANNEL_SETS);
        table.add_row(row!("Min frequency (Hz):", or_unknown(&self.0.min_frequency)));
        table.add_row(row!("Max frequency (Hz):", or_unknown(&self.0.max_frequency)));
        table.fmt(f)
    }
}

/// Formatter for [PcmFormatSet].
struct PcmFormatSetText<'a>(&'a PcmFormatSet);

impl Display for PcmFormatSetText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sample_types = self.0.sample_types.iter().map(ToString::to_string).join(", ");
        let frame_rates = self.0.frame_rates.iter().map(ToString::to_string).join(", ");
        let num_channels = self.0.channel_sets.iter().map(|set| set.channels()).join(", ");

        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_NESTED);
        table.add_row(row!(r->"Sample types:", sample_types));
        table.add_row(row!(r->"Frame rates (Hz):", frame_rates));
        table.add_row(row!(r->"Number of channels:", num_channels));
        table.add_row(row!(r->"Channel attributes:", ChannelSetsText(&self.0.channel_sets)));
        table.fmt(f)
    }
}

/// Formatter for a map of ring buffer element IDs to their supported formats.
struct SupportedRingBufferFormatsText<'a>(&'a Vec<PcmFormatSet>);

impl Display for SupportedRingBufferFormatsText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_EMPTY);
        if self.0.is_empty() {
            table.add_row(row!["No format sets available."]);
        } else {
            table.set_titles(row![format!(
                "{} {} with format sets:",
                self.0.len(),
                if self.0.len() == 1 { "element" } else { "elements" }
            )]);
            for format_set in self.0 {
                table.add_row(row![PcmFormatSetText(format_set)]);
            }
        }
        table.fmt(f)
    }
}

#[derive(Default, Debug, Serialize, PartialEq, Clone)]
pub struct InfoResult {
    pub device_path: Option<Utf8PathBuf>,

    #[serde(serialize_with = "serde_ext::serialize_option_tostring")]
    pub unique_id: Option<UniqueInstanceId>,

    pub manufacturer: Option<String>,

    pub product_name: Option<String>,

    #[serde(serialize_with = "serde_ext::serialize_option_gainstate")]
    pub gain_state: Option<GainState>,

    #[serde(serialize_with = "serde_ext::serialize_option_gaincapabilities")]
    pub gain_capabilities: Option<GainCapabilities>,

    #[serde(serialize_with = "serde_ext::serialize_option_tostring")]
    pub plug_state: Option<PlugState>,

    pub plug_time: Option<ZxTime>,

    #[serde(serialize_with = "serde_ext::serialize_option_tostring")]
    pub plug_detect_capabilities: Option<PlugDetectCapabilities>,

    #[serde(serialize_with = "serde_ext::serialize_option_clockdomain")]
    pub clock_domain: Option<ClockDomain>,

    #[serde(serialize_with = "serde_ext::serialize_option_vec_pcmformatset")]
    pub supported_ring_buffer_formats: Option<Vec<PcmFormatSet>>,
}

impl From<InfoResult> for Table {
    fn from(value: InfoResult) -> Self {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_NORMAL);
        table.add_row(row!(r->"Path:", or_unknown(&value.device_path)));
        table.add_row(row!(r->"Unique ID:", or_unknown(&value.unique_id)));
        table.add_row(row!(r->"Manufacturer:", or_unknown(&value.manufacturer)));
        table.add_row(row!(r->"Product:", or_unknown(&value.product_name)));
        table.add_row(
            row!(r->"Current gain:", or_unknown(&value.gain_state.as_ref().map(GainStateText))),
        );
        table.add_row(
            row!(r->"Gain capabilities:", or_unknown(&value.gain_capabilities.as_ref().map(GainCapabilitiesText))),
        );
        table.add_row(row!(r->"Plug state:", or_unknown(&value.plug_state)));
        table.add_row(row!(r->"Plug time:", or_unknown(&value.plug_time)));
        table.add_row(row!(r->"Plug detection:", or_unknown(&value.plug_detect_capabilities)));
        table.add_row(row!(r->"Clock domain:", or_unknown(&value.clock_domain)));
        table.add_row(row!(r->"Ring buffer formats:", or_unknown(&value.supported_ring_buffer_formats.as_ref().map(SupportedRingBufferFormatsText))));
        table
    }
}

/// Returns the [Display] representation of the option value, if it exists, or a placeholder.
fn or_unknown(value: &Option<impl ToString>) -> String {
    value.as_ref().map_or("<unknown>".to_string(), |value| value.to_string())
}

impl From<(fac::DeviceInfo, Selector)> for InfoResult {
    fn from(t: (fac::DeviceInfo, Selector)) -> Self {
        let (device_info, selector) = t;

        let device_path = match selector {
            Selector::Devfs(devfs) => Some(devfs.path()),
            Selector::Registry(_) => None,
        };

        match device_info {
            fac::DeviceInfo::StreamConfig(stream_info) => {
                let stream_properties = stream_info.stream_properties;
                let gain_state = stream_info.gain_state;
                let plug_state = stream_info.plug_state;
                let pcm_supported_formats = stream_info.supported_formats;

                match stream_properties {
                    Some(stream_properties) => Self {
                        device_path,
                        unique_id: stream_properties.unique_id.map(UniqueInstanceId::from),
                        manufacturer: stream_properties.manufacturer.clone(),
                        product_name: stream_properties.product.clone(),
                        gain_state: gain_state
                            .and_then(|gain_state| GainState::try_from(gain_state).ok()),
                        gain_capabilities: GainCapabilities::try_from(&stream_properties).ok(),
                        plug_state: plug_state
                            .clone()
                            .map(PlugState::try_from)
                            .transpose()
                            .unwrap_or_default(),
                        plug_time: plug_state.clone().and_then(|p| p.plug_state_time),
                        plug_detect_capabilities: stream_properties
                            .plug_detect_capabilities
                            .map(Into::into),
                        clock_domain: None,
                        supported_ring_buffer_formats: pcm_supported_formats.map(|formats| {
                            formats
                                .into_iter()
                                .map(PcmFormatSet::try_from)
                                .collect::<Result<Vec<_>, _>>()
                                .unwrap()
                        }),
                    },
                    None => Self { device_path, ..Default::default() },
                }
            }
            fac::DeviceInfo::Composite(composite_info) => {
                let composite_properties = composite_info.composite_properties;
                match composite_properties {
                    Some(composite_properties) => Self {
                        device_path,
                        manufacturer: composite_properties.manufacturer,
                        product_name: composite_properties.product,
                        clock_domain: composite_properties.clock_domain.map(ClockDomain::from),
                        ..Default::default()
                    },
                    None => Self { device_path, ..Default::default() },
                }
            }
            _ => panic!("Unsupported device type"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_audio_device as fadevice;
    use fidl_fuchsia_hardware_audio as fhaudio;
    use fuchsia_audio::format::SampleType;

    #[test]
    fn test_info_result_table() {
        let info_result = InfoResult {
            device_path: Some(Utf8PathBuf::from("/dev/class/audio-input/0c8301e0")),
            unique_id: Some(UniqueInstanceId([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ])),
            manufacturer: Some("Test manufacturer".to_string()),
            product_name: Some("Test product".to_string()),
            gain_state: Some(GainState {
                gain_db: -3.0_f32,
                muted: Some(false),
                agc_enabled: Some(false),
            }),
            gain_capabilities: Some(GainCapabilities {
                min_gain_db: -100.0_f32,
                max_gain_db: 0.0_f32,
                gain_step_db: 0.0_f32,
                can_mute: Some(true),
                can_agc: Some(false),
            }),
            plug_state: Some(fadevice::PlugState::Plugged.into()),
            plug_time: Some(123456789),
            plug_detect_capabilities: Some(fadevice::PlugDetectCapabilities::Hardwired.into()),
            clock_domain: Some(ClockDomain(fhaudio::CLOCK_DOMAIN_MONOTONIC)),
            supported_ring_buffer_formats: Some(vec![
                PcmFormatSet {
                    channel_sets: vec![
                        ChannelSet::try_from(vec![ChannelAttributes::default()]).unwrap()
                    ],
                    sample_types: vec![SampleType::Uint8, SampleType::Int16],
                    frame_rates: vec![16000, 22050, 32000],
                },
                PcmFormatSet {
                    channel_sets: vec![
                        ChannelSet::try_from(vec![ChannelAttributes::default()]).unwrap(),
                        ChannelSet::try_from(vec![
                            ChannelAttributes::default(),
                            ChannelAttributes::default(),
                        ])
                        .unwrap(),
                    ],
                    sample_types: vec![SampleType::Float32],
                    frame_rates: vec![44100, 48000, 88200, 96000],
                },
            ]),
        };

        let output = Table::from(info_result).to_string();

        let expected = r#"
                 Path:  /dev/class/audio-input/0c8301e0
            Unique ID:  000102030405060708090a0b0c0d0e0f
         Manufacturer:  Test manufacturer
              Product:  Test product
         Current gain:  -3 dB (unmuted, AGC off)
    Gain capabilities:  Gain range [-100 dB, 0 dB]; 0 dB step (continuous); can mute; cannot AGC
           Plug state:  Plugged
            Plug time:  123456789
       Plug detection:  Hardwired
         Clock domain:  0 (monotonic)
  Ring buffer formats:  2 elements with format sets:
                        ┌───────────────────────────────────────────────────────────────────────────────────┐
                        │       Sample types:  uint8, int16                                                 │
                        │   Frame rates (Hz):  16000, 22050, 32000                                          │
                        │ Number of channels:  1                                                            │
                        │ Channel attributes:  1 channel:  Channel 1:  Min frequency (Hz):  <unknown>       │
                        │                                              Max frequency (Hz):  <unknown>       │
                        └───────────────────────────────────────────────────────────────────────────────────┘
                        ┌────────────────────────────────────────────────────────────────────────────────────┐
                        │       Sample types:  float32                                                       │
                        │   Frame rates (Hz):  44100, 48000, 88200, 96000                                    │
                        │ Number of channels:  1, 2                                                          │
                        │ Channel attributes:  1 channel:   Channel 1:  Min frequency (Hz):  <unknown>       │
                        │                                               Max frequency (Hz):  <unknown>       │
                        │                      2 channels:  Channel 1:  Min frequency (Hz):  <unknown>       │
                        │                                               Max frequency (Hz):  <unknown>       │
                        │                                   Channel 2:  Min frequency (Hz):  <unknown>       │
                        │                                               Max frequency (Hz):  <unknown>       │
                        └────────────────────────────────────────────────────────────────────────────────────┘
"#
        .trim_start_matches('\n');

        assert_eq!(expected, output);
    }
}
