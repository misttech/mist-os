// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Helpers to serialize fuchsia_audio types with serde.

use fuchsia_audio::{
    device::{ClockDomain, GainCapabilities, GainState},
    format::SampleType,
    format_set::{ChannelAttributes, ChannelSet, PcmFormatSet},
};
use serde::{ser::SerializeSeq, Serialize, Serializer};

/// Serialize an optional value that can be converted to a string to either the string, or none.
pub fn serialize_option_tostring<S>(
    value: &Option<impl ToString>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let Some(value) = value else { return serializer.serialize_none() };
    serializer.serialize_str(&value.to_string())
}

/// Serialize a vector of values that can be converted to a string to either a sequence
/// of strings, or none.
pub fn serialize_vec_tostring<S>(
    value: &Vec<impl ToString>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(value.len()))?;
    for item in value {
        seq.serialize_element(&item.to_string())?;
    }
    seq.end()
}

/// Serialize an optional [ClockDomain] to its number value, or none.
pub fn serialize_option_clockdomain<S>(
    clock_domain: &Option<ClockDomain>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let Some(clock_domain) = clock_domain else { return serializer.serialize_none() };
    serializer.serialize_u32(clock_domain.0)
}

// Mirror type for serialization.
// This exists to avoid the fuchsia-audio library from depending on serde.
#[derive(Serialize)]
#[serde(remote = "GainState")]
pub struct GainStateDef {
    pub gain_db: f32,
    pub muted: Option<bool>,
    pub agc_enabled: Option<bool>,
}

pub fn serialize_option_gainstate<S>(
    gain_state: &Option<GainState>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let Some(gain_state) = gain_state else { return serializer.serialize_none() };
    GainStateDef::serialize(gain_state, serializer)
}

// Mirror type for serialization.
// This exists to avoid the fuchsia-audio library from depending on serde.
#[derive(Serialize)]
#[serde(remote = "GainCapabilities")]
pub struct GainCapabilitiesDef {
    pub min_gain_db: f32,
    pub max_gain_db: f32,
    pub gain_step_db: f32,
    pub can_mute: Option<bool>,
    pub can_agc: Option<bool>,
}

pub fn serialize_option_gaincapabilities<S>(
    gain_caps: &Option<GainCapabilities>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let Some(gain_caps) = gain_caps else { return serializer.serialize_none() };
    GainCapabilitiesDef::serialize(gain_caps, serializer)
}

// Mirror type for serialization.
// This exists to avoid the fuchsia-audio library from depending on serde.
#[derive(Serialize)]
#[serde(remote = "PcmFormatSet")]
pub struct PcmFormatSetDef {
    #[serde(serialize_with = "serialize_vec_channelset")]
    pub channel_sets: Vec<ChannelSet>,
    #[serde(serialize_with = "serialize_vec_tostring")]
    pub sample_types: Vec<SampleType>,
    pub frame_rates: Vec<u32>,
}

pub fn serialize_option_vec_pcmformatset<S>(
    format_sets: &Option<Vec<PcmFormatSet>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let Some(format_sets) = format_sets else { return serializer.serialize_none() };

    #[derive(Serialize)]
    struct Wrapper<'a>(#[serde(with = "PcmFormatSetDef")] &'a PcmFormatSet);

    let mut seq = serializer.serialize_seq(Some(format_sets.len()))?;
    for format_set in format_sets {
        seq.serialize_element(&Wrapper(format_set))?;
    }
    seq.end()
}

// Mirror type for serialization.
// This exists to avoid the fuchsia-audio library from depending on serde.
#[derive(Serialize)]
#[serde(remote = "ChannelSet")]
pub struct ChannelSetDef {
    #[serde(serialize_with = "serialize_vec_channelattributes")]
    pub attributes: Vec<ChannelAttributes>,
}

pub fn serialize_vec_channelset<S>(
    channel_sets: &Vec<ChannelSet>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    #[derive(Serialize)]
    struct Wrapper<'a>(#[serde(with = "ChannelSetDef")] &'a ChannelSet);

    let mut seq = serializer.serialize_seq(Some(channel_sets.len()))?;
    for channel_set in channel_sets {
        seq.serialize_element(&Wrapper(channel_set))?;
    }
    seq.end()
}

// Mirror type for serialization.
// This exists to avoid the fuchsia-audio library from depending on serde.
#[derive(Serialize)]
#[serde(remote = "ChannelAttributes")]
pub struct ChannelAttributesDef {
    pub min_frequency: Option<u32>,
    pub max_frequency: Option<u32>,
}

pub fn serialize_vec_channelattributes<S>(
    channel_attributes: &Vec<ChannelAttributes>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    #[derive(Serialize)]
    struct Wrapper<'a>(#[serde(with = "ChannelAttributesDef")] &'a ChannelAttributes);

    let mut seq = serializer.serialize_seq(Some(channel_attributes.len()))?;
    for attributes in channel_attributes {
        seq.serialize_element(&Wrapper(attributes))?;
    }
    seq.end()
}
