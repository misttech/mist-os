// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::audio::types::{AudioInfo, AudioSettingSource, AudioStream, AudioStreamType};
use crate::base::SettingInfo;
use crate::config::default_settings::DefaultSetting;
use settings_storage::storage_factory::DefaultLoader;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const DEFAULT_VOLUME_LEVEL: f32 = 0.5;
const DEFAULT_VOLUME_MUTED: bool = false;

const DEFAULT_STREAMS: [AudioStream; 5] = [
    create_default_audio_stream(AudioStreamType::Background),
    create_default_audio_stream(AudioStreamType::Media),
    create_default_audio_stream(AudioStreamType::Interruption),
    create_default_audio_stream(AudioStreamType::SystemAgent),
    create_default_audio_stream(AudioStreamType::Communication),
];

const DEFAULT_AUDIO_INFO: AudioInfo =
    AudioInfo { streams: DEFAULT_STREAMS, modified_counters: None };

/// A mapping from stream type to an arbitrary numerical value. This number will
/// change from the number sent in the previous update if the stream type's
/// volume has changed.
pub type ModifiedCounters = HashMap<AudioStreamType, usize>;

pub(crate) fn create_default_modified_counters() -> ModifiedCounters {
    IntoIterator::into_iter([
        AudioStreamType::Background,
        AudioStreamType::Media,
        AudioStreamType::Interruption,
        AudioStreamType::SystemAgent,
        AudioStreamType::Communication,
    ])
    .map(|stream_type| (stream_type, 0))
    .collect()
}

pub(crate) const fn create_default_audio_stream(stream_type: AudioStreamType) -> AudioStream {
    AudioStream {
        stream_type,
        source: AudioSettingSource::User,
        user_volume_level: DEFAULT_VOLUME_LEVEL,
        user_volume_muted: DEFAULT_VOLUME_MUTED,
    }
}

pub fn build_audio_default_settings() -> DefaultSetting<AudioInfo, &'static str> {
    DefaultSetting::new(Some(DEFAULT_AUDIO_INFO), "/config/data/audio_config_data.json")
}

/// Returns a default audio [`AudioInfo`] that is derived from
/// [`DEFAULT_AUDIO_INFO`] with any fields specified in the
/// audio configuration set.
///
/// [`DEFAULT_AUDIO_INFO`]: static@DEFAULT_AUDIO_INFO
#[derive(Clone)]
pub struct AudioInfoLoader {
    audio_default_settings: Arc<Mutex<DefaultSetting<AudioInfo, &'static str>>>,
}

impl AudioInfoLoader {
    pub(crate) fn new(audio_default_settings: DefaultSetting<AudioInfo, &'static str>) -> Self {
        Self { audio_default_settings: Arc::new(Mutex::new(audio_default_settings)) }
    }
}

impl DefaultLoader for AudioInfoLoader {
    type Result = AudioInfo;

    fn default_value(&self) -> Self::Result {
        let mut default_audio_info: AudioInfo = DEFAULT_AUDIO_INFO.clone();

        if let Ok(Some(audio_configuration)) =
            self.audio_default_settings.lock().unwrap().get_cached_value()
        {
            default_audio_info.streams = audio_configuration.streams;
        }
        default_audio_info
    }
}

impl From<AudioInfo> for SettingInfo {
    fn from(audio: AudioInfo) -> SettingInfo {
        SettingInfo::Audio(audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_async::TestExecutor;

    use crate::audio::types::{AudioInfoV1, AudioInfoV2};
    use crate::tests::helpers::move_executor_forward_and_get;
    use settings_storage::device_storage::DeviceStorageCompatible;

    const CONFIG_AUDIO_INFO: AudioInfo = AudioInfo {
        streams: [
            AudioStream {
                stream_type: AudioStreamType::Background,
                source: AudioSettingSource::System,
                user_volume_level: 0.6,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Media,
                source: AudioSettingSource::System,
                user_volume_level: 0.7,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Interruption,
                source: AudioSettingSource::System,
                user_volume_level: 0.2,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::SystemAgent,
                source: AudioSettingSource::User,
                user_volume_level: 0.3,
                user_volume_muted: true,
            },
            AudioStream {
                stream_type: AudioStreamType::Communication,
                source: AudioSettingSource::User,
                user_volume_level: 0.4,
                user_volume_muted: false,
            },
        ],
        modified_counters: None,
    };

    /// Load default settings from disk.
    fn load_default_settings(
        default_settings: &mut DefaultSetting<AudioInfo, &'static str>,
    ) -> AudioInfo {
        default_settings
            .load_default_value()
            .expect("if config exists, it should be parseable")
            .expect("default value should always exist")
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_audio_config() {
        let mut settings = build_audio_default_settings();
        let settings = load_default_settings(&mut settings);
        assert_eq!(CONFIG_AUDIO_INFO, settings);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn test_audio_info_migration_v1_to_v2() {
        let mut default_settings = build_audio_default_settings();
        let mut v1 = AudioInfoV1::default_value(load_default_settings(&mut default_settings));
        let updated_mic_mute_val = !v1.input.mic_mute;
        v1.input.mic_mute = updated_mic_mute_val;

        let serialized_v1 = serde_json::to_string(&v1).expect("default should serialize");

        let v2 = AudioInfoV2::try_deserialize_from(&serialized_v1)
            .expect("deserialization should succeed");

        assert_eq!(v2.input.mic_mute, updated_mic_mute_val);
    }

    #[fuchsia::test]
    fn test_audio_info_migration_v2_to_current() {
        let mut executor = TestExecutor::new_with_fake_time();
        let mut default_settings = build_audio_default_settings();
        let settings = load_default_settings(&mut default_settings);

        let mut v2 = move_executor_forward_and_get(
            &mut executor,
            async { AudioInfoV2::default_value(settings) },
            "Unable to get V2 default value",
        );
        let updated_mic_mute_val = !v2.input.mic_mute;
        v2.input.mic_mute = updated_mic_mute_val;

        let serialized_v2 = serde_json::to_string(&v2).expect("default should serialize");

        let current = AudioInfo::try_deserialize_from(&serialized_v2)
            .expect("deserialization should succeed");

        let loader = AudioInfoLoader::new(default_settings);
        assert_eq!(current, loader.default_value());
    }

    #[fuchsia::test]
    fn test_audio_info_migration_v1_to_current() {
        let mut executor = TestExecutor::new_with_fake_time();
        let mut default_settings = build_audio_default_settings();
        let settings = load_default_settings(&mut default_settings);

        let mut v1 = move_executor_forward_and_get(
            &mut executor,
            async { AudioInfoV1::default_value(settings) },
            "Unable to get V1 default value",
        );
        let updated_mic_mute_val = !v1.input.mic_mute;
        v1.input.mic_mute = updated_mic_mute_val;

        let serialized_v1 = serde_json::to_string(&v1).expect("default should serialize");

        let current = AudioInfo::try_deserialize_from(&serialized_v1)
            .expect("deserialization should succeed");

        let loader = AudioInfoLoader::new(default_settings);
        assert_eq!(current, loader.default_value());
    }
}
