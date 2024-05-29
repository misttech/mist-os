// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::audio_default_settings::default_audio_info;
use crate::audio::{create_default_modified_counters, ModifiedCounters};
use anyhow::{anyhow, Error};
use serde::{Deserialize, Serialize};
use settings_storage::device_storage::DeviceStorageCompatible;
use settings_storage::storage_factory::NoneT;
use std::collections::HashMap;
use std::ops::RangeInclusive;

const RANGE: RangeInclusive<f32> = 0.0..=1.0;

#[derive(PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize)]
pub enum AudioSettingSource {
    User,
    System,
    SystemWithFeedback,
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize, Hash, Eq)]
pub enum AudioStreamType {
    Background,
    Media,
    Interruption,
    SystemAgent,
    Communication,
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioStream {
    pub stream_type: AudioStreamType,
    pub source: AudioSettingSource,
    pub user_volume_level: f32,
    pub user_volume_muted: bool,
}

impl AudioStream {
    pub(crate) fn has_valid_volume_level(&self) -> bool {
        RANGE.contains(&self.user_volume_level)
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub struct SetAudioStream {
    pub stream_type: AudioStreamType,
    pub source: AudioSettingSource,
    pub user_volume_level: Option<f32>,
    pub user_volume_muted: Option<bool>,
}

impl SetAudioStream {
    pub(crate) fn has_valid_volume_level(&self) -> bool {
        self.user_volume_level.map(|v| RANGE.contains(&v)).unwrap_or(true)
    }

    pub(crate) fn is_valid_payload(&self) -> bool {
        self.user_volume_level.is_some() || self.user_volume_muted.is_some()
    }
}

impl From<AudioStream> for SetAudioStream {
    fn from(stream: AudioStream) -> Self {
        Self {
            stream_type: stream.stream_type,
            source: stream.source,
            user_volume_level: Some(stream.user_volume_level),
            user_volume_muted: Some(stream.user_volume_muted),
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfo {
    pub streams: [AudioStream; 5],
    pub modified_counters: Option<ModifiedCounters>,
}

impl DeviceStorageCompatible for AudioInfo {
    type Loader = NoneT;
    const KEY: &'static str = "audio_info";

    fn try_deserialize_from(value: &str) -> Result<Self, Error> {
        Self::extract(value).or_else(|_| AudioInfoV2::try_deserialize_from(value).map(Self::from))
    }
}

impl Default for AudioInfo {
    fn default() -> Self {
        default_audio_info()
    }
}

////////////////////////////////////////////////////////////////
/// Past versions of AudioInfo.
////////////////////////////////////////////////////////////////

#[derive(PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioInputInfo {
    pub mic_mute: bool,
}

/// The following struct should never be modified. It represents an old
/// version of the audio settings.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfoV2 {
    pub streams: [AudioStream; 5],
    pub input: AudioInputInfo,
    pub modified_counters: Option<ModifiedCounters>,
}

impl AudioInfoV2 {
    pub(super) fn try_deserialize_from(value: &str) -> Result<Self, Error> {
        serde_json::from_str(value)
            .map_err(|e| anyhow!("could not deserialize: {e:?}"))
            .or_else(|_| AudioInfoV1::try_deserialize_from(value).map(Self::from))
    }

    #[cfg(test)]
    pub(super) fn default_value() -> Self {
        AudioInfoV2 {
            streams: default_audio_info().streams,
            input: AudioInputInfo { mic_mute: false },
            modified_counters: None,
        }
    }
}

impl From<AudioInfoV2> for AudioInfo {
    fn from(v2: AudioInfoV2) -> AudioInfo {
        AudioInfo { streams: v2.streams, modified_counters: v2.modified_counters }
    }
}

/// The following struct should never be modified. It represents an old
/// version of the audio settings.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfoV1 {
    pub streams: [AudioStream; 5],
    pub input: AudioInputInfo,
    pub modified_timestamps: Option<HashMap<AudioStreamType, String>>,
}

impl AudioInfoV1 {
    pub(super) fn try_deserialize_from(value: &str) -> Result<Self, Error> {
        serde_json::from_str(value)
            .map_err(|e| anyhow!("could not deserialize: {e:?}"))
            .or_else(|_| AudioInfoV1::try_deserialize_from(value).map(Self::from))
    }

    #[cfg(test)]
    pub(super) fn default_value() -> Self {
        AudioInfoV1 {
            streams: default_audio_info().streams,
            input: AudioInputInfo { mic_mute: false },
            modified_timestamps: None,
        }
    }
}

impl From<AudioInfoV1> for AudioInfoV2 {
    fn from(v1: AudioInfoV1) -> Self {
        AudioInfoV2 {
            streams: v1.streams,
            input: v1.input,
            modified_counters: Some(create_default_modified_counters()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_AUDIO_STREAM: AudioStream = AudioStream {
        stream_type: AudioStreamType::Background,
        source: AudioSettingSource::User,
        user_volume_level: 0.5,
        user_volume_muted: false,
    };
    const INVALID_NEGATIVE_AUDIO_STREAM: AudioStream =
        AudioStream { user_volume_level: -0.1, ..VALID_AUDIO_STREAM };
    const INVALID_GREATER_THAN_ONE_AUDIO_STREAM: AudioStream =
        AudioStream { user_volume_level: 1.1, ..VALID_AUDIO_STREAM };

    #[fuchsia::test]
    fn test_volume_level_validation() {
        assert!(VALID_AUDIO_STREAM.has_valid_volume_level());
        assert!(!INVALID_NEGATIVE_AUDIO_STREAM.has_valid_volume_level());
        assert!(!INVALID_GREATER_THAN_ONE_AUDIO_STREAM.has_valid_volume_level());
    }

    #[fuchsia::test]
    fn test_set_audio_stream_validation() {
        let valid_set_audio_stream = SetAudioStream::from(VALID_AUDIO_STREAM);
        assert!(valid_set_audio_stream.has_valid_volume_level());
        let invalid_set_audio_stream = SetAudioStream::from(INVALID_NEGATIVE_AUDIO_STREAM);
        assert!(!invalid_set_audio_stream.has_valid_volume_level());
        let invalid_set_audio_stream = SetAudioStream::from(INVALID_GREATER_THAN_ONE_AUDIO_STREAM);
        assert!(!invalid_set_audio_stream.has_valid_volume_level());
    }
}
