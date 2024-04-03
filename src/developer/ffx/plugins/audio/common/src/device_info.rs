// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    camino::Utf8PathBuf,
    fidl_fuchsia_audio_controller as fac,
    fidl_fuchsia_hardware_audio::{ChannelSet, PlugDetectCapabilities, SampleFormat},
    fuchsia_audio::device::Selector,
    serde::{Deserialize, Serialize},
    std::fmt::Display,
};

// Wrapper types needed here since we cannot derive the Serializable trait directly on FIDL types.

// See  fidl_fuchsia_hardware_audio::PlugDetectCapabilities.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum PdCapsWrapper {
    Hardwired,
    CanAsyncNotify,
}

// See fidl_fuchsia_hardware_audio::SampleFormat.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SampleFormatWrapper {
    PcmSigned,
    PcmUnsigned,
    PcmFloat,
}

impl From<SampleFormat> for SampleFormatWrapper {
    fn from(sample_format: SampleFormat) -> Self {
        match sample_format {
            SampleFormat::PcmSigned => SampleFormatWrapper::PcmSigned,
            SampleFormat::PcmUnsigned => SampleFormatWrapper::PcmUnsigned,
            SampleFormat::PcmFloat => SampleFormatWrapper::PcmFloat,
        }
    }
}

// See fidl_fuchsia_hardware_audio::ChannelSet.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ChannelSetWrapper {
    pub attributes: Option<Vec<ChannelAttributesWrapper>>,
}

// See fidl_fuchsia_hardware_audio::ChannelAttributes.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ChannelAttributesWrapper {
    pub min_frequency: Option<u32>,
    pub max_frequency: Option<u32>,
}

impl From<ChannelSet> for ChannelSetWrapper {
    fn from(channel_set: ChannelSet) -> Self {
        ChannelSetWrapper {
            attributes: channel_set.attributes.map(|attributes| {
                attributes
                    .into_iter()
                    .map(|attribute| ChannelAttributesWrapper {
                        min_frequency: attribute.min_frequency,
                        max_frequency: attribute.max_frequency,
                    })
                    .collect()
            }),
        }
    }
}
// See fidl_fuchsia_hardware_audio::PcmSupportedFormats.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct PcmSupportedFormatsWrapper {
    pub channel_sets: Option<Vec<ChannelSetWrapper>>,
    pub sample_formats: Option<Vec<SampleFormatWrapper>>,
    pub bytes_per_sample: Option<Vec<u8>>,
    pub valid_bits_per_sample: Option<Vec<u8>>,
    pub frame_rates: Option<Vec<u32>>,
}

impl Display for PcmSupportedFormatsWrapper {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match &self.sample_formats {
            None => {
                write!(out, "No formats available")?;
            }
            Some(sample_formats) => {
                write!(out, "Number of format sets: {}", sample_formats.len())?;
                for format in sample_formats {
                    writeln!(out, "\n\t Format set")?;
                    write!(out, "\t\t Sample format         : {:?}\n", format)?;
                    write!(out, "\t\t Number of channels    :")?;

                    let mut has_attributes = false;
                    for channel_set in self.channel_sets.clone().unwrap_or(Vec::new()) {
                        let num_channels = match channel_set.attributes.clone() {
                            Some(attributes) => {
                                format!("{}", attributes.len())
                            }
                            None => format!("Number of channels unavailable"),
                        };

                        write!(out, " {}", num_channels)?;
                        for attribute in channel_set.attributes.unwrap_or(Vec::new()) {
                            if attribute.min_frequency.is_some() {
                                has_attributes = true;
                            }
                            if attribute.max_frequency.is_some() {
                                has_attributes = true;
                            }
                        }
                    }
                    if has_attributes {
                        write!(out, " \n\t Channels attributes     :")?;
                        for channel_set in self.channel_sets.clone().unwrap_or(Vec::new()) {
                            for attribute in channel_set.clone().attributes.unwrap_or(Vec::new()) {
                                match attribute.min_frequency {
                                    Some(min_frequency) => println!("  {}", min_frequency),
                                    None => {}
                                }
                                match attribute.max_frequency {
                                    Some(max_frequency) => println!("  {}", max_frequency),
                                    None => {}
                                }
                            }
                            print!(
                                "(min/max Hz for {} channels)",
                                channel_set.attributes.unwrap_or(Vec::new()).len()
                            );
                        }
                    }

                    write!(out, "\n\t\t Frame rate            :")?;
                    for frame_rate in self.frame_rates.clone().unwrap_or(Vec::new()) {
                        write!(out, " {frame_rate}Hz")?;
                    }

                    write!(out, "\n\t\t Bits per channel      :")?;
                    for bytes_per_sample in self.bytes_per_sample.clone().unwrap_or(Vec::new()) {
                        write!(out, " {}", bytes_per_sample * 8)?;
                    }

                    write!(out, "\n\t\t Valid bits per channel:")?;
                    for valid_bits_per_channel in
                        self.valid_bits_per_sample.clone().unwrap_or(Vec::new())
                    {
                        writeln!(out, " {valid_bits_per_channel}")?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct DeviceInfoResult {
    pub device_path: Option<Utf8PathBuf>,
    pub manufacturer: Option<String>,
    pub product_name: Option<String>,
    pub current_gain_db: Option<f32>,
    pub mute_state: Option<bool>,
    pub agc_state: Option<bool>,
    pub min_gain: Option<f32>,
    pub max_gain: Option<f32>,
    pub gain_step: Option<f32>,
    pub can_mute: Option<bool>,
    pub can_agc: Option<bool>,
    pub plugged: Option<bool>,
    pub plug_time: Option<i64>,
    pub pd_caps: Option<PdCapsWrapper>,
    pub supported_formats: Option<Vec<PcmSupportedFormatsWrapper>>,
    pub unique_id: Option<String>,
    pub clock_domain: Option<u32>,
}

impl Default for DeviceInfoResult {
    fn default() -> Self {
        DeviceInfoResult {
            device_path: None,
            manufacturer: None,
            product_name: None,
            current_gain_db: None,
            mute_state: None,
            agc_state: None,
            min_gain: None,
            max_gain: None,
            gain_step: None,
            can_mute: None,
            can_agc: None,
            plugged: None,
            plug_time: None,
            pd_caps: None,
            supported_formats: None,
            unique_id: None,
            clock_domain: None,
        }
    }
}

impl Display for DeviceInfoResult {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let current_gain_db = match self.current_gain_db {
            Some(gain_db) => format!("{} dB", gain_db),
            None => format!("Gain dB not available"),
        };

        let muted = match self.mute_state {
            Some(muted) => format!("{}", if muted { "muted" } else { "unmuted" },),
            None => format!("Muted not available"),
        };

        let agc = match self.agc_state {
            Some(agc) => format!("{}", if agc { "AGC on" } else { "AGC off" }),
            None => format!("AGC not available"),
        };
        // Gain capabilities
        let gain_statement = match self.min_gain {
            Some(min_gain) => match self.max_gain {
                Some(max_gain) => {
                    if min_gain == max_gain {
                        format!("fixed 0 dB gain")
                    } else {
                        format!("gain range [{}, {}]", min_gain, max_gain)
                    }
                }
                None => {
                    format!("Min gain {}. Max gain unavailable.", min_gain)
                }
            },
            None => format!(
                "Min gain unavailable. Max gain {}",
                if self.max_gain.is_some() {
                    format!("{}", self.max_gain.unwrap())
                } else {
                    format!("unavailable.")
                }
            ),
        };

        let gain_step = match self.gain_step {
            Some(gain_step) => {
                if gain_step == 0.0f32 {
                    format!("{} dB (continuous)", gain_step)
                } else {
                    format!("{} in dB steps", gain_step)
                }
            }
            None => format!("Gain step unavailable"),
        };

        let can_mute = match self.can_mute {
            Some(can_mute) => format!("{}", if can_mute { "can mute" } else { "cannot mute" }),
            None => format!("Can mute unavailable"),
        };

        let can_agc = match self.can_agc {
            Some(can_agc) => format!("{}", if can_agc { "can agc" } else { "cannot agc" }),
            None => format!("Can agc unavailable"),
        };

        writeln!(
            out,
            "Info for audio device at path {}",
            self.device_path.as_ref().map_or("Unavailable", AsRef::as_ref)
        )?;
        writeln!(
            out,
            "\t Unique ID    : {}",
            self.unique_id.as_ref().map_or("Unavailable", AsRef::as_ref)
        )?;
        writeln!(
            out,
            "\t Manufacturer : {}",
            self.manufacturer.as_ref().map_or("Unavailable", AsRef::as_ref)
        )?;
        writeln!(
            out,
            "\t Product      : {}",
            self.product_name.as_ref().map_or("Unavailable", AsRef::as_ref)
        )?;
        writeln!(out, "\t Current Gain : {} ({}, {})", current_gain_db, muted, agc)?;
        write!(out, "\t Gain Caps    : ")?;
        write!(out, "{} {}", gain_statement, gain_step)?;
        write!(out, "; {}; {} \n", can_mute, can_agc)?;
        writeln!(
            out,
            "\t Plug State   : {}",
            self.plugged.map_or("Unavailable", |plugged| if plugged {
                "plugged"
            } else {
                "unplugged"
            })
        )?;
        writeln!(
            out,
            "\t Plug Time    : {}",
            self.plug_time.map_or(format!("Unavailable"), |time| format!("{time}"))
        )?;
        writeln!(
            out,
            "\t PD Caps      : {}",
            self.pd_caps.clone().map_or(format!("Unavailable"), |pd_caps| format!(
                "{}",
                match pd_caps {
                    PdCapsWrapper::Hardwired => "Hardwired",
                    PdCapsWrapper::CanAsyncNotify => "Can async notify",
                }
            ))
        )?;

        match &self.supported_formats {
            Some(formats) => {
                writeln!(out, "\t Supported Formats: ")?;
                for format in formats {
                    writeln!(out, "\t {}", format)?
                }
            }
            None => writeln!(out, "\t Supported Formats: Unavailable")?,
        }

        writeln!(
            out,
            "\t Clock Domain : {}",
            self.clock_domain.map_or(format!("Unavailable"), |c| format!("{c}"))
        )
    }
}

impl From<(fac::DeviceInfo, Selector)> for DeviceInfoResult {
    fn from(t: (fac::DeviceInfo, Selector)) -> Self {
        let (device_info, selector) = t;

        let device_path = match selector {
            Selector::Devfs(devfs) => Some(devfs.path()),
        };

        match device_info {
            fac::DeviceInfo::StreamConfig(stream_info) => {
                let stream_properties = stream_info.stream_properties;
                let gain_state = stream_info.gain_state;
                let plug_state_info = stream_info.plug_state;
                let pcm_supported_formats = stream_info.supported_formats;

                match stream_properties {
                    Some(stream_properties) => Self {
                        device_path,
                        manufacturer: stream_properties.manufacturer.clone(),
                        product_name: stream_properties.product.clone(),
                        current_gain_db: gain_state.clone().and_then(|g| g.gain_db),
                        mute_state: gain_state.clone().and_then(|g| g.muted),
                        agc_state: gain_state.clone().and_then(|g| g.agc_enabled),
                        min_gain: stream_properties.min_gain_db,
                        max_gain: stream_properties.max_gain_db,
                        gain_step: stream_properties.gain_step_db,
                        can_mute: stream_properties.can_mute,
                        can_agc: stream_properties.can_agc,
                        plugged: plug_state_info.clone().and_then(|p| p.plugged),
                        plug_time: plug_state_info.clone().and_then(|p| p.plug_state_time),
                        pd_caps: match stream_properties.plug_detect_capabilities {
                            Some(PlugDetectCapabilities::CanAsyncNotify) => {
                                Some(PdCapsWrapper::CanAsyncNotify)
                            }
                            Some(PlugDetectCapabilities::Hardwired) => {
                                Some(PdCapsWrapper::Hardwired)
                            }
                            None => None,
                        },
                        supported_formats: pcm_supported_formats.and_then(|formats| {
                            formats
                                .clone()
                                .into_iter()
                                .map(|supported_format| {
                                    supported_format.pcm_supported_formats.map(|format| {
                                        PcmSupportedFormatsWrapper {
                                            sample_formats: format.sample_formats.and_then(
                                                |formats| {
                                                    Some(
                                                        formats
                                                            .into_iter()
                                                            .map(|sample_format| {
                                                                sample_format.into()
                                                            })
                                                            .collect(),
                                                    )
                                                },
                                            ),
                                            bytes_per_sample: format.bytes_per_sample,
                                            valid_bits_per_sample: format.valid_bits_per_sample,
                                            frame_rates: format.frame_rates,
                                            channel_sets: format.channel_sets.map(|sets| {
                                                sets.into_iter().map(|set| set.into()).collect()
                                            }),
                                        }
                                    })
                                })
                                .collect()
                        }),
                        unique_id: stream_properties.unique_id.and_then(|id| {
                            let formatted: [String; 16] = id
                                .into_iter()
                                .map(|byte| format!("{:02x}", byte))
                                .collect::<Vec<String>>()
                                .try_into()
                                .unwrap_or_default();
                            Some(format!(
                                "{:}-{:}",
                                formatted[0..7].concat(),
                                formatted[8..15].concat()
                            ))
                        }),
                        clock_domain: None,
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
                        clock_domain: composite_properties.clock_domain,
                        ..Default::default()
                    },
                    None => Self { device_path, ..Default::default() },
                }
            }
            _ => panic!("Unsupported device type"),
        }
    }
}
