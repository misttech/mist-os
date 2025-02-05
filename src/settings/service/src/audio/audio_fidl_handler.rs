// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use fidl::prelude::*;
use fidl_fuchsia_media::{AudioRenderUsage, AudioRenderUsage2};
use fidl_fuchsia_settings::{
    AudioMarker, AudioRequest, AudioSet2Responder, AudioSet2Result, AudioSetResponder,
    AudioSetResult, AudioSettings, AudioSettings2, AudioStreamSettingSource, AudioStreamSettings,
    AudioStreamSettings2, AudioWatch2Responder, AudioWatchResponder, Volume,
};
use fuchsia_trace as ftrace;

use crate::audio::types::{AudioSettingSource, AudioStream, AudioStreamType, SetAudioStream};
use crate::base::{SettingInfo, SettingType};
use crate::handler::base::Request;
use crate::ingress::{request, watch, Scoped};
use crate::job::source::{Error as JobError, ErrorResponder};
use crate::job::Job;
use crate::{trace, trace_guard};

/// Custom responder that wraps the real FIDL responder plus a tracing guard. The guard is stored
/// here so that it's active until a response is sent and this responder is dropped.
struct AudioSetTraceResponder {
    responder: AudioSetResponder,
    _guard: Option<ftrace::AsyncScope>,
}
impl request::Responder<Scoped<AudioSetResult>> for AudioSetTraceResponder {
    fn respond(self, Scoped(response): Scoped<AudioSetResult>) {
        let _ = self.responder.send(response);
    }
}
impl ErrorResponder for AudioSetTraceResponder {
    fn id(&self) -> &'static str {
        "Audio_Set"
    }

    fn respond(self: Box<Self>, error: fidl_fuchsia_settings::Error) -> Result<(), fidl::Error> {
        self.responder.send(Err(error))
    }
}
impl request::Responder<Scoped<AudioSetResult>> for AudioSetResponder {
    fn respond(self, Scoped(response): Scoped<AudioSetResult>) {
        let _ = self.send(response);
    }
}

impl watch::Responder<AudioSettings, zx::Status> for AudioWatchResponder {
    fn respond(self, response: Result<AudioSettings, zx::Status>) {
        match response {
            Ok(settings) => {
                let _ = self.send(&settings);
            }
            Err(error) => {
                self.control_handle().shutdown_with_epitaph(error);
            }
        }
    }
}

// Set2 and Watch2 implementations are derived directly from the corresponding Set and Watch
// counterparts, using AudioSettings2 instead of AudioSettings.
struct AudioSet2TraceResponder {
    responder: AudioSet2Responder,
    _guard: Option<ftrace::AsyncScope>,
}
impl request::Responder<Scoped<AudioSet2Result>> for AudioSet2TraceResponder {
    fn respond(self, Scoped(response): Scoped<AudioSet2Result>) {
        let _ = self.responder.send(response);
    }
}
impl ErrorResponder for AudioSet2TraceResponder {
    fn id(&self) -> &'static str {
        "Audio_Set2"
    }

    fn respond(self: Box<Self>, error: fidl_fuchsia_settings::Error) -> Result<(), fidl::Error> {
        self.responder.send(Err(error))
    }
}
impl request::Responder<Scoped<AudioSet2Result>> for AudioSet2Responder {
    fn respond(self, Scoped(response): Scoped<AudioSet2Result>) {
        let _ = self.send(response);
    }
}

impl watch::Responder<AudioSettings2, zx::Status> for AudioWatch2Responder {
    fn respond(self, response: Result<AudioSettings2, zx::Status>) {
        match response {
            Ok(settings) => {
                let _ = self.send(&settings);
            }
            Err(error) => {
                self.control_handle().shutdown_with_epitaph(error);
            }
        }
    }
}

impl TryFrom<AudioRequest> for Job {
    type Error = JobError;

    fn try_from(item: AudioRequest) -> Result<Self, Self::Error> {
        #[allow(unreachable_patterns)]
        match item {
            AudioRequest::Set { settings, responder } => {
                let id = ftrace::Id::new();
                let guard = trace_guard!(id, c"audio fidl handler set");
                let responder = AudioSetTraceResponder { responder, _guard: guard };
                match to_request(settings, id) {
                    Ok(request) => {
                        Ok(request::Work::new(SettingType::Audio, request, responder).into())
                    }
                    Err(err) => {
                        log::error!(
                            "{}: Failed to process 'set' request: {:?}",
                            AudioMarker::DEBUG_NAME,
                            err
                        );
                        Err(JobError::InvalidInput(Box::new(responder)))
                    }
                }
            }
            AudioRequest::Set2 { settings, responder } => {
                let id = ftrace::Id::new();
                let guard = trace_guard!(id, c"audio fidl handler set2");
                let responder = AudioSet2TraceResponder { responder, _guard: guard };
                match to_request2(settings, id) {
                    Ok(request) => {
                        Ok(request::Work::new(SettingType::Audio, request, responder).into())
                    }
                    Err(err) => {
                        log::error!(
                            "{}: Failed2 to process 'set2' request: {:?}",
                            AudioMarker::DEBUG_NAME,
                            err
                        );
                        Err(JobError::InvalidInput(Box::new(responder)))
                    }
                }
            }
            AudioRequest::Watch { responder } => {
                let mut hasher = DefaultHasher::new();
                "audio_watch".hash(&mut hasher);
                // Because we increment the modification counters stored in AudioInfo for every
                // change, clients would be notified for every change, even if the streams are
                // identical. A custom change function is used here so only stream CHANGES
                // (and only in "legacy" stream types) trigger the Watch() notification.
                Ok(watch::Work::new_job_with_change_function(
                    SettingType::Audio,
                    responder,
                    watch::ChangeFunction::new(
                        hasher.finish(),
                        Box::new(move |old: &SettingInfo, new: &SettingInfo| match (old, new) {
                            (SettingInfo::Audio(old_info), SettingInfo::Audio(new_info)) => {
                                let mut old_streams = old_info.streams.iter();
                                let new_streams = new_info.streams.iter();
                                for new_stream in new_streams {
                                    let old_stream = old_streams
                                        .find(|stream| stream.stream_type == new_stream.stream_type)
                                        .expect("stream type should be found in existing streams");
                                    // Watch() notifies upon changes to "legacy" stream types.
                                    if (old_stream != new_stream)
                                        && new_stream.stream_type.is_legacy()
                                    {
                                        return true;
                                    }
                                }
                                false
                            }
                            _ => false,
                        }),
                    ),
                ))
            }
            AudioRequest::Watch2 { responder } => {
                let mut hasher = DefaultHasher::new();
                "audio_watch2".hash(&mut hasher);
                // Because we increment the modification counters stored in AudioInfo for every
                // change, clients would be notified for every change, even if the streams are
                // identical. A custom change function is used here so only stream CHANGES
                // trigger the Watch2() notification.
                Ok(watch::Work::new_job_with_change_function(
                    SettingType::Audio,
                    responder,
                    watch::ChangeFunction::new(
                        hasher.finish(),
                        Box::new(move |old: &SettingInfo, new: &SettingInfo| match (old, new) {
                            (SettingInfo::Audio(old_info), SettingInfo::Audio(new_info)) => {
                                let mut old_streams = old_info.streams.iter();
                                let new_streams = new_info.streams.iter();
                                for new_stream in new_streams {
                                    let old_stream = old_streams
                                        .find(|stream| stream.stream_type == new_stream.stream_type)
                                        .expect("stream type should be found in existing streams");
                                    // Watch2() notifies upon changes to any stream type.
                                    if old_stream != new_stream {
                                        return true;
                                    }
                                }
                                false
                            }
                            _ => false,
                        }),
                    ),
                ))
            }
            _ => {
                log::warn!("Received a call to an unsupported API: {:?}", item);
                Err(JobError::Unsupported)
            }
        }
    }
}

impl From<SettingInfo> for AudioSettings {
    fn from(response: SettingInfo) -> Self {
        if let SettingInfo::Audio(info) = response {
            let mut streams = Vec::new();
            for stream in &info.streams {
                let stream_settings = AudioStreamSettings::try_from(*stream);
                if stream_settings.is_ok() {
                    streams.push(stream_settings.unwrap());
                }
            }

            AudioSettings { streams: Some(streams), ..Default::default() }
        } else {
            panic!("incorrect value sent to audio");
        }
    }
}

impl From<SettingInfo> for AudioSettings2 {
    fn from(response: SettingInfo) -> Self {
        if let SettingInfo::Audio(info) = response {
            let mut streams = Vec::new();
            for stream in &info.streams {
                streams.push(AudioStreamSettings2::from(*stream));
            }

            AudioSettings2 { streams: Some(streams), ..Default::default() }
        } else {
            panic!("incorrect value sent to audio");
        }
    }
}

impl TryFrom<AudioStream> for AudioStreamSettings {
    type Error = ();

    fn try_from(stream: AudioStream) -> Result<Self, ()> {
        match AudioRenderUsage::try_from(stream.stream_type) {
            Err(_) => Err(()),
            Ok(stream_type) => Ok(AudioStreamSettings {
                stream: Some(stream_type),
                source: Some(AudioStreamSettingSource::from(stream.source)),
                user_volume: Some(Volume {
                    level: Some(stream.user_volume_level),
                    muted: Some(stream.user_volume_muted),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        }
    }
}

impl From<AudioStream> for AudioStreamSettings2 {
    fn from(stream: AudioStream) -> Self {
        AudioStreamSettings2 {
            stream: Some(AudioRenderUsage2::from(stream.stream_type)),
            source: Some(AudioStreamSettingSource::from(stream.source)),
            user_volume: Some(Volume {
                level: Some(stream.user_volume_level),
                muted: Some(stream.user_volume_muted),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

impl From<AudioRenderUsage> for AudioStreamType {
    fn from(usage: AudioRenderUsage) -> Self {
        match usage {
            AudioRenderUsage::Background => AudioStreamType::Background,
            AudioRenderUsage::Communication => AudioStreamType::Communication,
            AudioRenderUsage::Interruption => AudioStreamType::Interruption,
            AudioRenderUsage::Media => AudioStreamType::Media,
            AudioRenderUsage::SystemAgent => AudioStreamType::SystemAgent,
        }
    }
}

impl TryFrom<AudioStreamType> for AudioRenderUsage {
    type Error = ();
    fn try_from(usage: AudioStreamType) -> Result<Self, Self::Error> {
        match usage {
            AudioStreamType::Accessibility => Err(()),
            AudioStreamType::Background => Ok(AudioRenderUsage::Background),
            AudioStreamType::Communication => Ok(AudioRenderUsage::Communication),
            AudioStreamType::Interruption => Ok(AudioRenderUsage::Interruption),
            AudioStreamType::Media => Ok(AudioRenderUsage::Media),
            AudioStreamType::SystemAgent => Ok(AudioRenderUsage::SystemAgent),
        }
    }
}

impl From<AudioStreamType> for AudioRenderUsage2 {
    fn from(usage: AudioStreamType) -> Self {
        match usage {
            AudioStreamType::Accessibility => AudioRenderUsage2::Accessibility,
            AudioStreamType::Background => AudioRenderUsage2::Background,
            AudioStreamType::Communication => AudioRenderUsage2::Communication,
            AudioStreamType::Interruption => AudioRenderUsage2::Interruption,
            AudioStreamType::Media => AudioRenderUsage2::Media,
            AudioStreamType::SystemAgent => AudioRenderUsage2::SystemAgent,
        }
    }
}

impl TryFrom<AudioRenderUsage2> for AudioStreamType {
    type Error = ();
    fn try_from(usage: AudioRenderUsage2) -> Result<Self, Self::Error> {
        match usage {
            AudioRenderUsage2::Accessibility => Ok(AudioStreamType::Accessibility),
            AudioRenderUsage2::Background => Ok(AudioStreamType::Background),
            AudioRenderUsage2::Communication => Ok(AudioStreamType::Communication),
            AudioRenderUsage2::Interruption => Ok(AudioStreamType::Interruption),
            AudioRenderUsage2::Media => Ok(AudioStreamType::Media),
            AudioRenderUsage2::SystemAgent => Ok(AudioStreamType::SystemAgent),
            _ => Err(()),
        }
    }
}

impl From<AudioStreamSettingSource> for AudioSettingSource {
    fn from(source: AudioStreamSettingSource) -> Self {
        match source {
            AudioStreamSettingSource::User => AudioSettingSource::User,
            AudioStreamSettingSource::System => AudioSettingSource::System,
            AudioStreamSettingSource::SystemWithFeedback => AudioSettingSource::SystemWithFeedback,
        }
    }
}

impl From<AudioSettingSource> for AudioStreamSettingSource {
    fn from(source: AudioSettingSource) -> Self {
        match source {
            AudioSettingSource::User => AudioStreamSettingSource::User,
            AudioSettingSource::System => AudioStreamSettingSource::System,
            AudioSettingSource::SystemWithFeedback => AudioStreamSettingSource::SystemWithFeedback,
        }
    }
}

// Clippy warns about all variants starting with the same prefix `No`.
#[allow(clippy::enum_variant_names)]
#[derive(thiserror::Error, Debug, PartialEq)]
enum Error {
    #[error("request has no streams")]
    NoStreams,
    #[error("missing user_volume at stream {0}")]
    NoUserVolume(usize),
    #[error("missing user_volume.level and user_volume.muted at stream {0}")]
    MissingVolumeAndMuted(usize),
    #[error("missing stream at stream {0}")]
    NoStreamType(usize),
    #[error("missing source at stream {0}")]
    NoSource(usize),
    #[error("request has an unknown stream type")]
    UnrecognizedStreamType,
}

fn to_request(settings: AudioSettings, id: ftrace::Id) -> Result<Request, Error> {
    trace!(id, c"to_request");
    settings
        .streams
        .map(|streams| {
            streams
                .into_iter()
                .enumerate()
                .map(|(i, stream)| {
                    let user_volume = stream.user_volume.ok_or(Error::NoUserVolume(i))?;
                    let user_volume_level = user_volume.level;
                    let user_volume_muted = user_volume.muted;
                    let stream_type = stream.stream.ok_or(Error::NoStreamType(i))?.into();
                    let source = stream.source.ok_or(Error::NoSource(i))?.into();
                    let request = SetAudioStream {
                        stream_type,
                        source,
                        user_volume_level,
                        user_volume_muted,
                    };
                    if request.is_valid_payload() {
                        Ok(request)
                    } else {
                        Err(Error::MissingVolumeAndMuted(i))
                    }
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|streams| Request::SetVolume(streams, id))
        })
        .unwrap_or(Err(Error::NoStreams))
}

fn to_request2(settings: AudioSettings2, id: ftrace::Id) -> Result<Request, Error> {
    trace!(id, c"to_request2");
    settings
        .streams
        .map(|streams| {
            streams
                .into_iter()
                .enumerate()
                .map(|(i, stream)| {
                    let user_volume = stream.user_volume.ok_or(Error::NoUserVolume(i))?;
                    let user_volume_level = user_volume.level;
                    let user_volume_muted = user_volume.muted;
                    let stream_type = match stream.stream.ok_or(Error::NoStreamType(i))?.try_into()
                    {
                        Ok(stream_type) => Ok(stream_type),
                        Err(_) => Err(Error::UnrecognizedStreamType),
                    }?;
                    let source = stream.source.ok_or(Error::NoSource(i))?.into();
                    let request = SetAudioStream {
                        stream_type,
                        source,
                        user_volume_level,
                        user_volume_muted,
                    };
                    if request.is_valid_payload() {
                        Ok(request)
                    } else {
                        Err(Error::MissingVolumeAndMuted(i))
                    }
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|streams| Request::SetVolume(streams, id))
        })
        .unwrap_or(Err(Error::NoStreams))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stream() -> AudioStreamSettings {
        AudioStreamSettings {
            stream: Some(fidl_fuchsia_media::AudioRenderUsage::Media),
            source: Some(AudioStreamSettingSource::User),
            user_volume: Some(Volume {
                level: Some(0.6),
                muted: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn test_stream2() -> AudioStreamSettings2 {
        AudioStreamSettings2 {
            stream: Some(fidl_fuchsia_media::AudioRenderUsage2::Media),
            source: Some(AudioStreamSettingSource::User),
            user_volume: Some(Volume {
                level: Some(0.6),
                muted: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // Verifies that an entirely empty settings request results in an appropriate error.
    #[fuchsia::test]
    fn test_request_from_settings_empty() {
        let id = ftrace::Id::new();
        let request = to_request(AudioSettings::default(), id);

        assert_eq!(request, Err(Error::NoStreams));
    }

    // Verifies that an entirely empty settings request2 results in an appropriate error.
    #[fuchsia::test]
    fn test_request2_from_settings_empty() {
        let id = ftrace::Id::new();
        let request = to_request2(AudioSettings2::default(), id);

        assert_eq!(request, Err(Error::NoStreams));
    }

    // Verifies that a settings request missing user volume info results in an appropriate error.
    #[fuchsia::test]
    fn test_request_missing_user_volume() {
        let mut stream = test_stream();
        stream.user_volume = None;

        let audio_settings = AudioSettings { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request(audio_settings, id);

        assert_eq!(request, Err(Error::NoUserVolume(0)));
    }

    // Verifies that a settings request2 missing user volume info results in an appropriate error.
    #[fuchsia::test]
    fn test_request2_missing_user_volume() {
        let mut stream = test_stream2();
        stream.user_volume = None;

        let audio_settings = AudioSettings2 { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request2(audio_settings, id);

        assert_eq!(request, Err(Error::NoUserVolume(0)));
    }

    // Verifies that a settings request missing the stream type results in an appropriate error.
    #[fuchsia::test]
    fn test_request_missing_stream_type() {
        let mut stream = test_stream();
        stream.stream = None;

        let audio_settings = AudioSettings { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request(audio_settings, id);

        assert_eq!(request, Err(Error::NoStreamType(0)));
    }

    // Verifies that a settings request2 missing the stream type results in an appropriate error.
    #[fuchsia::test]
    fn test_request2_missing_stream_type() {
        let mut stream = test_stream2();
        stream.stream = None;

        let audio_settings = AudioSettings2 { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request2(audio_settings, id);

        assert_eq!(request, Err(Error::NoStreamType(0)));
    }

    // Verifies that a settings request missing the source results in an appropriate error.
    #[fuchsia::test]
    fn test_request_missing_source() {
        let mut stream = test_stream();
        stream.source = None;

        let audio_settings = AudioSettings { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request(audio_settings, id);

        assert_eq!(request, Err(Error::NoSource(0)));
    }

    // Verifies that a settings request2 missing the source results in an appropriate error.
    #[fuchsia::test]
    fn test_request2_missing_source() {
        let mut stream = test_stream2();
        stream.source = None;

        let audio_settings = AudioSettings2 { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request2(audio_settings, id);

        assert_eq!(request, Err(Error::NoSource(0)));
    }

    // Verifies that a settings request missing both the user volume level and mute state results in
    // an appropriate error.
    #[fuchsia::test]
    fn test_request_missing_user_volume_level_and_muted() {
        let mut stream = test_stream();
        stream.user_volume = Some(Volume { level: None, muted: None, ..Default::default() });

        let audio_settings = AudioSettings { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request(audio_settings, id);

        assert_eq!(request, Err(Error::MissingVolumeAndMuted(0)));
    }

    // Verifies that a settings request2 missing both the user volume level and mute state results in
    // an appropriate error.
    #[fuchsia::test]
    fn test_request2_missing_user_volume_level_and_muted() {
        let mut stream = test_stream2();
        stream.user_volume = Some(Volume { level: None, muted: None, ..Default::default() });

        let audio_settings = AudioSettings2 { streams: Some(vec![stream]), ..Default::default() };

        let id = ftrace::Id::new();
        let request = to_request2(audio_settings, id);

        assert_eq!(request, Err(Error::MissingVolumeAndMuted(0)));
    }
}
