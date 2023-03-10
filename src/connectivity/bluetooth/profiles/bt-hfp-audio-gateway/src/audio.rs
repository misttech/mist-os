// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::PeerId;
use thiserror::Error;

use crate::features::CodecId;
use crate::sco_connector::ScoConnection;

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("Parameters aren't supported {:?}", .source)]
    UnsupportedParameters { source: anyhow::Error },
    #[error("Audio is already started")]
    AlreadyStarted,
    #[error("AudioCore Error: {:?}", .source)]
    AudioCore { source: anyhow::Error },
    #[error("Audio is not started")]
    NotStarted,
    #[error("Could not find suitable devices")]
    DiscoveryFailed,
}

impl AudioError {
    fn audio_core(e: anyhow::Error) -> Self {
        Self::AudioCore { source: e }
    }
}

pub mod dai_audio_control;
pub use dai_audio_control::DaiAudioControl;

pub mod inband_audio_control;
pub use inband_audio_control::InbandAudioControl;

pub trait AudioControl: Send {
    /// Start the audio, adding the audio device to the audio core and routing audio.
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        codec: CodecId,
    ) -> Result<(), AudioError>;

    /// Stop the audio, removing audio devices from the audio core.
    /// If the Audio is not started, this returns Err(AudioError::NotStarted).
    fn stop(&mut self) -> Result<(), AudioError>;
}

#[derive(Default)]
pub struct TestAudioControl {
    started: bool,
    _connection: Option<ScoConnection>,
}

impl AudioControl for TestAudioControl {
    fn start(
        &mut self,
        _id: PeerId,
        connection: ScoConnection,
        _codec: CodecId,
    ) -> Result<(), AudioError> {
        if self.started {
            return Err(AudioError::AlreadyStarted);
        }
        self.started = true;
        self._connection = Some(connection);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if !self.started {
            return Err(AudioError::NotStarted);
        }
        self.started = false;
        self._connection = None;
        Ok(())
    }
}
