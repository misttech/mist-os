// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{connect, serde_ext};
use camino::Utf8PathBuf;
use ffx_command::{bug, user_error, FfxContext};
use fidl_fuchsia_audio_device as fadevice;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_hardware_audio_signalprocessing as fhaudio_sigproc;
use fidl_fuchsia_io as fio;
use fuchsia_audio::{
    dai::{DaiFormatSet, DaiFrameFormat, DaiFrameFormatClocking, DaiFrameFormatJustification},
    device::{
        ClockDomain, DevfsSelector, GainCapabilities, GainState, Info as DeviceInfo,
        PlugDetectCapabilities, PlugEvent, Selector, UniqueInstanceId,
    },
    format_set::{ChannelAttributes, ChannelSet, PcmFormatSet},
    Registry,
};
use fuchsia_zircon_status::Status;
use itertools::Itertools;
use lazy_static::lazy_static;
use prettytable::{cell, format, row, Table};
use serde::Serialize;
use std::collections::BTreeMap;
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

/// Formatter for [PlugEvent].
struct PlugEventText<'a>(&'a PlugEvent);

impl Display for PlugEventText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.0.state, self.0.time)
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
struct SupportedRingBufferFormatsText<'a>(&'a BTreeMap<fadevice::ElementId, Vec<PcmFormatSet>>);

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
                if self.0.len() == 1 { "element" } else { "elements" },
            )]);
            for (element_id, format_sets) in self.0 {
                table.add_row(row![format!(
                    "Element {} has {} format {}:",
                    element_id,
                    format_sets.len(),
                    if format_sets.len() == 1 { "set" } else { "sets" }
                )]);
                for format_set in format_sets {
                    table.add_row(row![PcmFormatSetText(format_set)]);
                }
            }
        }
        table.fmt(f)
    }
}

/// Formatter for a vector of [DaiFrameFormat]s.
struct DaiFrameFormatsText<'a>(&'a Vec<DaiFrameFormat>);

impl Display for DaiFrameFormatsText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_EMPTY);
        for dai_frame_format in self.0 {
            match dai_frame_format {
                DaiFrameFormat::Standard(standard_format) => {
                    table.add_row(row![standard_format.to_string()]);
                }
                DaiFrameFormat::Custom(custom_format) => {
                    let mut format_table = Table::new();
                    format_table.set_format(*TABLE_FORMAT_NORMAL);
                    table.set_titles(row![format!("Custom format: {}", custom_format)]);
                    format_table.add_row(row![
                        r->"Justification:",
                        match custom_format.justification {
                            DaiFrameFormatJustification::Left => "Left",
                            DaiFrameFormatJustification::Right => "Right",
                        }
                    ]);
                    format_table.add_row(row![
                        r->"Clocking:",
                        match custom_format.clocking {
                            DaiFrameFormatClocking::RaisingSclk => "Raising sclk",
                            DaiFrameFormatClocking::FallingSclk => "Falling sclk",
                        }
                    ]);
                    format_table.add_row(row![
                        r->"Frame sync offset (sclks):",
                        custom_format.frame_sync_sclks_offset
                    ]);
                    format_table.add_row(
                        row![r->"Frame sync size (sclks):", custom_format.frame_sync_size],
                    );
                    table.add_row(row![format_table]);
                }
            }
        }
        table.fmt(f)
    }
}

/// Formatter for [DaiFormatSet].
struct DaiFormatSetText<'a>(&'a DaiFormatSet);

impl Display for DaiFormatSetText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dai_format_set = self.0;
        let number_of_channels =
            dai_format_set.number_of_channels.iter().map(ToString::to_string).join(", ");
        let sample_formats =
            dai_format_set.sample_formats.iter().map(ToString::to_string).join(", ");
        let frame_formats = DaiFrameFormatsText(&dai_format_set.frame_formats);
        let frame_rates = dai_format_set.frame_rates.iter().map(ToString::to_string).join(", ");
        let bits_per_slot = dai_format_set.bits_per_slot.iter().map(ToString::to_string).join(", ");
        let bits_per_sample =
            dai_format_set.bits_per_sample.iter().map(ToString::to_string).join(", ");

        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_NESTED);
        table.add_row(row!(r->"Number of channels:", number_of_channels));
        table.add_row(row!(r->"Sample formats:", sample_formats));
        table.add_row(row!(r->"Frame formats:", frame_formats));
        table.add_row(row!(r->"Frame rates (Hz):", frame_rates));
        table.add_row(row!(r->"Bits per slot:", bits_per_slot));
        table.add_row(row!(r->"Bits per sample:", bits_per_sample));
        table.fmt(f)
    }
}

/// Formatter for a map of DAI interconnect element IDs to their supported formats.
struct SupportedDaiFormatsText<'a>(&'a BTreeMap<fadevice::ElementId, Vec<DaiFormatSet>>);

impl Display for SupportedDaiFormatsText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_EMPTY);
        if self.0.is_empty() {
            table.add_row(row!["No format sets available."]);
        } else {
            table.set_titles(row![format!(
                "{} {} with DAI format sets:",
                self.0.len(),
                if self.0.len() == 1 { "element" } else { "elements" },
            )]);
            for (element_id, dai_format_sets) in self.0 {
                table.add_row(row![format!(
                    "Element {} has {} DAI format {}:",
                    element_id,
                    dai_format_sets.len(),
                    if dai_format_sets.len() == 1 { "set" } else { "sets" }
                )]);
                for dai_format_set in dai_format_sets {
                    table.add_row(row![DaiFormatSetText(dai_format_set)]);
                }
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

    #[serde(serialize_with = "serde_ext::serialize_option_plugevent")]
    pub plug_event: Option<PlugEvent>,

    #[serde(serialize_with = "serde_ext::serialize_option_tostring")]
    pub plug_detect_capabilities: Option<PlugDetectCapabilities>,

    #[serde(serialize_with = "serde_ext::serialize_option_clockdomain")]
    pub clock_domain: Option<ClockDomain>,

    #[serde(serialize_with = "serde_ext::serialize_option_map_daiformatset")]
    pub supported_dai_formats: Option<BTreeMap<fadevice::ElementId, Vec<DaiFormatSet>>>,

    #[serde(serialize_with = "serde_ext::serialize_option_map_pcmformatset")]
    pub supported_ring_buffer_formats: Option<BTreeMap<fadevice::ElementId, Vec<PcmFormatSet>>>,
}

impl From<InfoResult> for Table {
    fn from(value: InfoResult) -> Self {
        let mut table = Table::new();
        table.set_format(*TABLE_FORMAT_NORMAL);
        table.add_row(row!(r->"Path:", or_unknown(&value.device_path)));
        table.add_row(row!(r->"Unique ID:", or_unknown(&value.unique_id)));
        table.add_row(row!(r->"Manufacturer:", or_unknown(&value.manufacturer)));
        table.add_row(row!(r->"Product:", or_unknown(&value.product_name)));
        table.add_row(row!(
            r->"Current gain:",
            or_unknown(&value.gain_state.as_ref().map(GainStateText)),
        ));
        table.add_row(row!(
            r->"Gain capabilities:",
            or_unknown(&value.gain_capabilities.as_ref().map(GainCapabilitiesText)),
        ));
        table.add_row(row!(
            r->"Plug state:",
            or_unknown(&value.plug_event.as_ref().map(PlugEventText)),
        ));
        table.add_row(row!(r->"Plug detection:", or_unknown(&value.plug_detect_capabilities)));
        table.add_row(row!(r->"Clock domain:", or_unknown(&value.clock_domain)));
        table.add_row(row!(
            r->"DAI formats:",
            or_unknown(&value.supported_dai_formats.as_ref().map(SupportedDaiFormatsText)),
        ));
        table.add_row(row!(
            r->"Ring buffer formats:",
            or_unknown(
                &value.supported_ring_buffer_formats.as_ref().map(SupportedRingBufferFormatsText),
            ),
        ));
        table
    }
}

/// Returns the [Display] representation of the option value, if it exists, or a placeholder.
fn or_unknown(value: &Option<impl ToString>) -> String {
    value.as_ref().map_or("<unknown>".to_string(), |value| value.to_string())
}

impl From<(Info, Selector)> for InfoResult {
    fn from(value: (Info, Selector)) -> Self {
        let (info, selector) = value;

        let device_path = match selector {
            Selector::Devfs(devfs) => Some(devfs.path()),
            Selector::Registry(_) => None,
        };

        Self {
            device_path,
            unique_id: info.unique_instance_id(),
            manufacturer: info.manufacturer(),
            product_name: info.product_name(),
            gain_state: info.gain_state(),
            gain_capabilities: info.gain_capabilities(),
            plug_event: info.plug_event(),
            plug_detect_capabilities: info.plug_detect_capabilities(),
            clock_domain: info.clock_domain(),
            supported_ring_buffer_formats: info.supported_ring_buffer_formats(),
            supported_dai_formats: info.supported_dai_formats(),
        }
    }
}

pub struct HardwareCompositeInfo {
    properties: fhaudio::CompositeProperties,
    dai_formats: BTreeMap<fhaudio_sigproc::ElementId, Vec<fhaudio::DaiSupportedFormats>>,
    ring_buffer_formats: BTreeMap<fhaudio_sigproc::ElementId, Vec<fhaudio::SupportedFormats>>,
}

pub struct HardwareCodecInfo {
    properties: fhaudio::CodecProperties,
    dai_formats: Vec<fhaudio::DaiSupportedFormats>,
    plug_state: fhaudio::PlugState,
}

pub struct HardwareDaiInfo {
    properties: fhaudio::DaiProperties,
    dai_formats: Vec<fhaudio::DaiSupportedFormats>,
    ring_buffer_formats: Vec<fhaudio::SupportedFormats>,
}

pub struct HardwareStreamConfigInfo {
    properties: fhaudio::StreamProperties,
    supported_formats: Vec<fhaudio::SupportedFormats>,
    gain_state: fhaudio::GainState,
    plug_state: fhaudio::PlugState,
}

/// Information about a device from its hardware protocol.
pub enum HardwareInfo {
    Composite(HardwareCompositeInfo),
    Codec(HardwareCodecInfo),
    Dai(HardwareDaiInfo),
    StreamConfig(HardwareStreamConfigInfo),
}

impl HardwareInfo {
    pub fn unique_instance_id(&self) -> Option<UniqueInstanceId> {
        let id = match self {
            HardwareInfo::Composite(composite) => composite.properties.unique_id,
            HardwareInfo::Codec(codec) => codec.properties.unique_id,
            HardwareInfo::Dai(dai) => dai.properties.unique_id,
            HardwareInfo::StreamConfig(stream_config) => stream_config.properties.unique_id,
        };
        id.map(UniqueInstanceId)
    }

    pub fn manufacturer(&self) -> Option<String> {
        match self {
            HardwareInfo::Composite(composite) => composite.properties.manufacturer.clone(),
            HardwareInfo::Codec(codec) => codec.properties.manufacturer.clone(),
            HardwareInfo::Dai(dai) => dai.properties.manufacturer.clone(),
            HardwareInfo::StreamConfig(stream_config) => {
                stream_config.properties.manufacturer.clone()
            }
        }
    }

    pub fn product_name(&self) -> Option<String> {
        match self {
            HardwareInfo::Composite(composite) => composite.properties.product.clone(),
            HardwareInfo::Codec(codec) => codec.properties.product.clone(),
            HardwareInfo::Dai(dai) => dai.properties.product_name.clone(),
            HardwareInfo::StreamConfig(stream_config) => stream_config.properties.product.clone(),
        }
    }

    pub fn gain_capabilities(&self) -> Option<GainCapabilities> {
        match self {
            HardwareInfo::StreamConfig(stream_config) => {
                GainCapabilities::try_from(&stream_config.properties).ok()
            }
            // TODO(https://fxbug.dev/333120537): Support gain caps for hardware Composite/Codec/Dai
            _ => None,
        }
    }

    pub fn plug_detect_capabilities(&self) -> Option<PlugDetectCapabilities> {
        match self {
            HardwareInfo::Codec(codec) => {
                codec.properties.plug_detect_capabilities.map(PlugDetectCapabilities::from)
            }
            HardwareInfo::StreamConfig(stream_config) => {
                stream_config.properties.plug_detect_capabilities.map(PlugDetectCapabilities::from)
            }
            // TODO(https://fxbug.dev/333120537): Support plug detect caps for hardware Composite/Dai
            _ => None,
        }
        .clone()
    }

    pub fn clock_domain(&self) -> Option<ClockDomain> {
        match self {
            HardwareInfo::Composite(composite) => {
                composite.properties.clock_domain.map(ClockDomain::from)
            }
            HardwareInfo::StreamConfig(stream_config) => {
                stream_config.properties.clock_domain.map(ClockDomain::from)
            }
            _ => None,
        }
    }

    pub fn supported_ring_buffer_formats(
        &self,
    ) -> Option<BTreeMap<fadevice::ElementId, Vec<PcmFormatSet>>> {
        fn supported_formats_to_pcm_format_sets(
            supported_formats: &Vec<fhaudio::SupportedFormats>,
        ) -> Vec<PcmFormatSet> {
            supported_formats
                .iter()
                .filter_map(|supported_formats| {
                    let pcm_supported_formats = supported_formats.pcm_supported_formats.clone()?;
                    PcmFormatSet::try_from(pcm_supported_formats).ok()
                })
                .collect()
        }

        match self {
            HardwareInfo::Composite(composite) => Some(
                composite
                    .ring_buffer_formats
                    .iter()
                    .map(|(element_id, supported_formats)| {
                        (*element_id, supported_formats_to_pcm_format_sets(supported_formats))
                    })
                    .collect(),
            ),
            HardwareInfo::Codec(_) => None,
            HardwareInfo::Dai(dai) => Some({
                let pcm_format_sets =
                    supported_formats_to_pcm_format_sets(&dai.ring_buffer_formats);
                let mut map = BTreeMap::new();
                map.insert(fadevice::DEFAULT_RING_BUFFER_ELEMENT_ID, pcm_format_sets);
                map
            }),
            HardwareInfo::StreamConfig(stream_config) => Some({
                let pcm_format_sets =
                    supported_formats_to_pcm_format_sets(&stream_config.supported_formats);
                let mut map = BTreeMap::new();
                map.insert(fadevice::DEFAULT_RING_BUFFER_ELEMENT_ID, pcm_format_sets);
                map
            }),
        }
    }

    pub fn supported_dai_formats(
        &self,
    ) -> Option<BTreeMap<fadevice::ElementId, Vec<DaiFormatSet>>> {
        fn dai_supported_formats_to_dai_format_sets(
            dai_supported_formats: &Vec<fhaudio::DaiSupportedFormats>,
        ) -> Vec<DaiFormatSet> {
            dai_supported_formats
                .iter()
                .map(|dai_supported_formats| DaiFormatSet::from(dai_supported_formats.clone()))
                .collect()
        }

        match self {
            HardwareInfo::Composite(composite) => Some(
                composite
                    .dai_formats
                    .iter()
                    .map(|(element_id, dai_supported_formats)| {
                        (
                            *element_id,
                            dai_supported_formats_to_dai_format_sets(dai_supported_formats),
                        )
                    })
                    .collect(),
            ),
            HardwareInfo::Codec(codec) => Some({
                let dai_format_sets = dai_supported_formats_to_dai_format_sets(&codec.dai_formats);
                let mut map = BTreeMap::new();
                map.insert(fadevice::DEFAULT_DAI_INTERCONNECT_ELEMENT_ID, dai_format_sets);
                map
            }),
            HardwareInfo::Dai(dai) => Some({
                let dai_format_sets = dai_supported_formats_to_dai_format_sets(&dai.dai_formats);
                let mut map = BTreeMap::new();
                map.insert(fadevice::DEFAULT_DAI_INTERCONNECT_ELEMENT_ID, dai_format_sets);
                map
            }),
            HardwareInfo::StreamConfig(_) => None,
        }
    }

    pub fn gain_state(&self) -> Option<GainState> {
        match self {
            // TODO(https://fxbug.dev/334981374): Support gain state for hardware Composite
            HardwareInfo::Composite(_) => None,
            HardwareInfo::Codec(_) | HardwareInfo::Dai(_) => None,
            HardwareInfo::StreamConfig(stream_config) => {
                stream_config.gain_state.clone().try_into().ok()
            }
        }
    }

    pub fn plug_event(&self) -> Option<PlugEvent> {
        match self {
            // TODO(https://fxbug.dev/334980316): Support plug state for hardware Composite
            HardwareInfo::Composite(_) => None,
            HardwareInfo::Codec(codec) => codec.plug_state.clone().try_into().ok(),
            HardwareInfo::Dai(_) => None,
            HardwareInfo::StreamConfig(stream_config) => {
                stream_config.plug_state.clone().try_into().ok()
            }
        }
    }
}

/// Information about a device from `fuchsia.audio.device.Registry`.
pub struct RegistryInfo {
    device_info: DeviceInfo,
    gain_state: Option<GainState>,
    plug_event: Option<PlugEvent>,
}

pub enum Info {
    Hardware(HardwareInfo),
    Registry(RegistryInfo),
}

impl Info {
    pub fn unique_instance_id(&self) -> Option<UniqueInstanceId> {
        match self {
            Info::Hardware(hw_info) => hw_info.unique_instance_id(),
            Info::Registry(registry_info) => registry_info.device_info.unique_instance_id(),
        }
    }

    pub fn manufacturer(&self) -> Option<String> {
        match self {
            Info::Hardware(hw_info) => hw_info.manufacturer(),
            Info::Registry(registry_info) => registry_info.device_info.0.manufacturer.clone(),
        }
    }

    pub fn product_name(&self) -> Option<String> {
        match self {
            Info::Hardware(hw_info) => hw_info.product_name(),
            Info::Registry(registry_info) => registry_info.device_info.0.product.clone(),
        }
    }

    pub fn gain_state(&self) -> Option<GainState> {
        match self {
            Info::Hardware(hw_info) => hw_info.gain_state(),
            Info::Registry(registry_info) => registry_info.gain_state.clone(),
        }
    }

    pub fn gain_capabilities(&self) -> Option<GainCapabilities> {
        match self {
            Info::Hardware(hw_info) => hw_info.gain_capabilities(),
            Info::Registry(registry_info) => registry_info.device_info.gain_capabilities(),
        }
    }

    pub fn plug_event(&self) -> Option<PlugEvent> {
        match self {
            Info::Hardware(hw_info) => hw_info.plug_event(),
            Info::Registry(registry_info) => registry_info.plug_event.clone(),
        }
    }

    pub fn plug_detect_capabilities(&self) -> Option<PlugDetectCapabilities> {
        match self {
            Info::Hardware(hw_info) => hw_info.plug_detect_capabilities(),
            Info::Registry(registry_info) => registry_info.device_info.plug_detect_capabilities(),
        }
    }

    pub fn clock_domain(&self) -> Option<ClockDomain> {
        match self {
            Info::Hardware(hw_info) => hw_info.clock_domain(),
            Info::Registry(registry_info) => registry_info.device_info.clock_domain(),
        }
    }

    pub fn supported_ring_buffer_formats(
        &self,
    ) -> Option<BTreeMap<fadevice::ElementId, Vec<PcmFormatSet>>> {
        match self {
            Info::Hardware(hw_info) => hw_info.supported_ring_buffer_formats(),
            Info::Registry(registry_info) => {
                registry_info.device_info.supported_ring_buffer_formats().ok()
            }
        }
    }

    pub fn supported_dai_formats(
        &self,
    ) -> Option<BTreeMap<fadevice::ElementId, Vec<DaiFormatSet>>> {
        match self {
            Info::Hardware(hw_info) => hw_info.supported_dai_formats(),
            Info::Registry(registry_info) => registry_info.device_info.supported_dai_formats().ok(),
        }
    }
}

/// Returns information about a Codec device from its hardware protocol.
async fn get_hw_codec_info(codec: &fhaudio::CodecProxy) -> fho::Result<HardwareCodecInfo> {
    let properties =
        codec.get_properties().await.bug_context("Failed to call Codec.GetProperties")?;

    let dai_formats = codec
        .get_dai_formats()
        .await
        .bug_context("Failed to call Codec.GetDaiFormats")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to get DAI formats")?;

    let plug_state =
        codec.watch_plug_state().await.bug_context("Failed to call Codec.WatchPlugState")?;

    Ok(HardwareCodecInfo { properties, dai_formats, plug_state })
}

/// Returns information about a Dai device from its hardware protocol.
async fn get_hw_dai_info(dai: &fhaudio::DaiProxy) -> fho::Result<HardwareDaiInfo> {
    let properties = dai.get_properties().await.bug_context("Failed to call Dai.GetProperties")?;

    let dai_formats = dai
        .get_dai_formats()
        .await
        .bug_context("Failed to call Dai.GetDaiFormats")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to get DAI formats")?;

    let ring_buffer_formats = dai
        .get_ring_buffer_formats()
        .await
        .bug_context("Failed to call Dai.GetRingBufferFormats")?
        .map_err(|status| Status::from_raw(status))
        .bug_context("Failed to get ring buffer formats")?;

    Ok(HardwareDaiInfo { properties, dai_formats, ring_buffer_formats })
}

/// Returns information about a Composite device from its hardware protocol.
async fn get_hw_composite_info(
    composite: &fhaudio::CompositeProxy,
) -> fho::Result<HardwareCompositeInfo> {
    let properties =
        composite.get_properties().await.bug_context("Failed to call Composite.GetProperties")?;

    // TODO(https://fxbug.dev/333120537): Support fetching DAI formats for hardware Composite
    let dai_formats = BTreeMap::new();

    // TODO(https://fxbug.dev/333120537): Support fetching ring buffer formats for hardware Composite
    let ring_buffer_formats = BTreeMap::new();

    Ok(HardwareCompositeInfo { properties, dai_formats, ring_buffer_formats })
}

/// Returns information about a StreamConfig device from its hardware protocol.
async fn get_hw_stream_config_info(
    stream_config: &fhaudio::StreamConfigProxy,
) -> fho::Result<HardwareStreamConfigInfo> {
    let properties = stream_config
        .get_properties()
        .await
        .bug_context("Failed to call StreamConfig.GetProperties")?;

    let supported_formats = stream_config
        .get_supported_formats()
        .await
        .bug_context("Failed to call StreamConfig.GetSupportedFormats")?;

    let gain_state = stream_config
        .watch_gain_state()
        .await
        .bug_context("Failed to call StreamConfig.WatchGainState")?;

    let plug_state = stream_config
        .watch_plug_state()
        .await
        .bug_context("Failed to call StreamConfig.WatchPlugState")?;

    Ok(HardwareStreamConfigInfo { properties, supported_formats, gain_state, plug_state })
}

/// Returns information about a device from its hardware protocol in devfs.
async fn get_hardware_info(
    dev_class: &fio::DirectoryProxy,
    selector: DevfsSelector,
) -> fho::Result<HardwareInfo> {
    let protocol_path = selector.relative_path();

    match selector.device_type().0 {
        fadevice::DeviceType::Codec => {
            let codec = connect::connect_hw_codec(dev_class, protocol_path.as_str())?;
            let codec_info = get_hw_codec_info(&codec).await?;
            Ok(HardwareInfo::Codec(codec_info))
        }
        fadevice::DeviceType::Composite => {
            let composite = connect::connect_hw_composite(dev_class, protocol_path.as_str())?;
            let composite_info = get_hw_composite_info(&composite).await?;
            Ok(HardwareInfo::Composite(composite_info))
        }
        fadevice::DeviceType::Dai => {
            let dai = connect::connect_hw_dai(dev_class, protocol_path.as_str())?;
            let dai_info = get_hw_dai_info(&dai).await?;
            Ok(HardwareInfo::Dai(dai_info))
        }
        fadevice::DeviceType::Input | fadevice::DeviceType::Output => {
            let stream_config =
                connect::connect_hw_streamconfig(dev_class, protocol_path.as_str())?;
            let stream_config_info = get_hw_stream_config_info(&stream_config).await?;
            Ok(HardwareInfo::StreamConfig(stream_config_info))
        }
        _ => Err(bug!("Unsupported device type")),
    }
}

/// Returns a device info from the target.
///
/// If the selector is a [RegistrySelector] for a registry device,
/// `registry` must be Some.
pub async fn get_info(
    dev_class: &fio::DirectoryProxy,
    registry: Option<&Registry>,
    selector: Selector,
) -> fho::Result<Info> {
    match selector {
        Selector::Devfs(devfs_selector) => {
            let hw_info = get_hardware_info(dev_class, devfs_selector).await?;
            Ok(Info::Hardware(hw_info))
        }
        Selector::Registry(registry_selector) => {
            let device_info = registry
                .ok_or_else(|| bug!("Registry not available"))?
                .get(registry_selector.token_id())
                .await
                .ok_or_else(|| user_error!("No device with given ID exists in registry"))?;
            let registry_info = RegistryInfo {
                device_info,
                // TODO(https://fxbug.dev/329150383): Supports gain_state/plug_state/plug_time for ADR devices
                gain_state: None,
                plug_event: None,
            };
            Ok(Info::Registry(registry_info))
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_audio_device as fadevice;
    use fidl_fuchsia_hardware_audio as fhaudio;
    use fuchsia_audio::format::SampleType;
    use serde_json::json;

    lazy_static! {
        pub static ref TEST_INFO_RESULT: InfoResult = InfoResult {
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
            plug_event: Some(PlugEvent {
                state: fadevice::PlugState::Plugged.into(),
                time: 123456789,
            }),
            plug_detect_capabilities: Some(fadevice::PlugDetectCapabilities::Hardwired.into()),
            clock_domain: Some(ClockDomain(fhaudio::CLOCK_DOMAIN_MONOTONIC)),
            supported_dai_formats: Some({
                let mut map = BTreeMap::new();
                map.insert(
                    fadevice::DEFAULT_DAI_INTERCONNECT_ELEMENT_ID,
                    vec![fhaudio::DaiSupportedFormats {
                        number_of_channels: vec![1, 2],
                        sample_formats: vec![
                            fhaudio::DaiSampleFormat::PcmSigned,
                            fhaudio::DaiSampleFormat::PcmUnsigned,
                        ],
                        frame_formats: vec![
                            fhaudio::DaiFrameFormat::FrameFormatStandard(
                                fhaudio::DaiFrameFormatStandard::StereoLeft,
                            ),
                            fhaudio::DaiFrameFormat::FrameFormatStandard(
                                fhaudio::DaiFrameFormatStandard::StereoRight,
                            ),
                        ],
                        frame_rates: vec![16000, 22050, 32000, 44100, 48000, 88200, 96000],
                        bits_per_slot: vec![32],
                        bits_per_sample: vec![8, 16],
                    }
                    .into()],
                );
                map.insert(
                    123,
                    vec![fhaudio::DaiSupportedFormats {
                        number_of_channels: vec![1],
                        sample_formats: vec![fhaudio::DaiSampleFormat::PcmFloat],
                        frame_formats: vec![fhaudio::DaiFrameFormat::FrameFormatCustom(
                            fhaudio::DaiFrameFormatCustom {
                                left_justified: false,
                                sclk_on_raising: true,
                                frame_sync_sclks_offset: 1,
                                frame_sync_size: 2,
                            },
                        )],
                        frame_rates: vec![16000, 22050, 32000, 44100, 48000, 88200, 96000],
                        bits_per_slot: vec![16, 32],
                        bits_per_sample: vec![16],
                    }
                    .into()],
                );
                map
            }),
            supported_ring_buffer_formats: Some({
                let mut map = BTreeMap::new();
                map.insert(
                    fadevice::DEFAULT_RING_BUFFER_ELEMENT_ID,
                    vec![PcmFormatSet {
                        channel_sets: vec![
                            ChannelSet::try_from(vec![ChannelAttributes::default()]).unwrap(),
                            ChannelSet::try_from(vec![
                                ChannelAttributes::default(),
                                ChannelAttributes::default(),
                            ])
                            .unwrap(),
                        ],
                        sample_types: vec![SampleType::Int16],
                        frame_rates: vec![16000, 22050, 32000, 44100, 48000, 88200, 96000],
                    }],
                );
                map.insert(
                    123,
                    vec![
                        PcmFormatSet {
                            channel_sets: vec![ChannelSet::try_from(vec![
                                ChannelAttributes::default(),
                            ])
                            .unwrap()],
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
                    ],
                );
                map
            }),
        };
    }

    #[test]
    fn test_info_result_table() {
        let output = Table::from(TEST_INFO_RESULT.clone()).to_string();

        let expected = r#"
                 Path:  /dev/class/audio-input/0c8301e0
            Unique ID:  000102030405060708090a0b0c0d0e0f
         Manufacturer:  Test manufacturer
              Product:  Test product
         Current gain:  -3 dB (unmuted, AGC off)
    Gain capabilities:  Gain range [-100 dB, 0 dB]; 0 dB step (continuous); can mute; cannot AGC
           Plug state:  Plugged at 123456789
       Plug detection:  Hardwired
         Clock domain:  0 (monotonic)
          DAI formats:  2 elements with DAI format sets:
                        Element 1 has 1 DAI format set:
                        ┌──────────────────────────────────────────────────────────────────────┐
                        │ Number of channels:  1, 2                                            │
                        │     Sample formats:  pcm_signed, pcm_unsigned                        │
                        │      Frame formats:  stereo_left                                     │
                        │                      stereo_right                                    │
                        │   Frame rates (Hz):  16000, 22050, 32000, 44100, 48000, 88200, 96000 │
                        │      Bits per slot:  32                                              │
                        │    Bits per sample:  8, 16                                           │
                        └──────────────────────────────────────────────────────────────────────┘
                        Element 123 has 1 DAI format set:
                        ┌──────────────────────────────────────────────────────────────────────┐
                        │ Number of channels:  1                                               │
                        │     Sample formats:  pcm_float                                       │
                        │      Frame formats:  Custom format: right_justified,raising_sclk,1,2 │
                        │                                    Justification:  Right             │
                        │                                         Clocking:  Raising sclk      │
                        │                        Frame sync offset (sclks):  1                 │
                        │                          Frame sync size (sclks):  2                 │
                        │   Frame rates (Hz):  16000, 22050, 32000, 44100, 48000, 88200, 96000 │
                        │      Bits per slot:  16, 32                                          │
                        │    Bits per sample:  16                                              │
                        └──────────────────────────────────────────────────────────────────────┘
  Ring buffer formats:  2 elements with format sets:
                        Element 0 has 1 format set:
                        ┌────────────────────────────────────────────────────────────────────────────────────┐
                        │       Sample types:  int16                                                         │
                        │   Frame rates (Hz):  16000, 22050, 32000, 44100, 48000, 88200, 96000               │
                        │ Number of channels:  1, 2                                                          │
                        │ Channel attributes:  1 channel:   Channel 1:  Min frequency (Hz):  <unknown>       │
                        │                                               Max frequency (Hz):  <unknown>       │
                        │                      2 channels:  Channel 1:  Min frequency (Hz):  <unknown>       │
                        │                                               Max frequency (Hz):  <unknown>       │
                        │                                   Channel 2:  Min frequency (Hz):  <unknown>       │
                        │                                               Max frequency (Hz):  <unknown>       │
                        └────────────────────────────────────────────────────────────────────────────────────┘
                        Element 123 has 2 format sets:
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

    #[test]
    pub fn test_info_result_json() {
        let output = serde_json::to_value(&*TEST_INFO_RESULT).unwrap();

        let expected = json!({
            "device_path": "/dev/class/audio-input/0c8301e0",
            "unique_id": "000102030405060708090a0b0c0d0e0f",
            "manufacturer": "Test manufacturer",
            "product_name": "Test product",
            "gain_state": {
                "gain_db": -3.0,
                "muted": false,
                "agc_enabled": false
            },
            "gain_capabilities": {
                "min_gain_db": -100.0,
                "max_gain_db": 0.0,
                "gain_step_db": 0.0,
                "can_mute": true,
                "can_agc": false
            },
            "plug_event": {
                "state": "Plugged",
                "time": 123456789,
            },
            "plug_detect_capabilities": "Hardwired",
            "clock_domain": 0,
            "supported_dai_formats": {
                "1": [
                    {
                        "number_of_channels": [1, 2],
                        "sample_formats": ["pcm_signed", "pcm_unsigned"],
                        "frame_formats": ["stereo_left", "stereo_right"],
                        "frame_rates": [16000, 22050, 32000, 44100, 48000, 88200, 96000],
                        "bits_per_slot": [32],
                        "bits_per_sample": [8, 16]
                    }
                ],
                "123": [
                    {
                        "number_of_channels": [1],
                        "sample_formats": ["pcm_float"],
                        "frame_formats": ["custom:right_justified,raising_sclk,1,2"],
                        "frame_rates": [16000, 22050, 32000, 44100, 48000, 88200, 96000],
                        "bits_per_slot": [16, 32],
                        "bits_per_sample": [16]
                    }
                ]
            },
            "supported_ring_buffer_formats": {
                "0": [
                    {
                        "channel_sets": [
                            {
                                "attributes": [
                                    { "min_frequency": null, "max_frequency": null }
                                ]
                            },
                            {
                                "attributes": [
                                    { "min_frequency": null, "max_frequency": null },
                                    { "min_frequency": null, "max_frequency": null }
                                ]
                            }
                        ],
                        "sample_types": ["int16"],
                        "frame_rates": [16000, 22050, 32000, 44100, 48000, 88200, 96000]
                    }
                ],
                "123": [
                    {
                        "channel_sets": [
                            {
                                "attributes": [
                                    { "min_frequency": null, "max_frequency": null }
                                ]
                            }
                        ],
                        "sample_types": ["uint8", "int16"],
                        "frame_rates": [16000, 22050, 32000]
                    },
                    {
                        "channel_sets": [
                            {
                                "attributes": [
                                    { "min_frequency": null, "max_frequency": null }
                                ]
                            },
                            {
                                "attributes": [
                                    { "min_frequency": null, "max_frequency": null },
                                    { "min_frequency": null, "max_frequency": null }
                                ]
                            }
                        ],
                        "sample_types": ["float32"],
                        "frame_rates": [44100, 48000, 88200, 96000]
                    }
                ]
            }
        });

        assert_eq!(output, expected);
    }
}
