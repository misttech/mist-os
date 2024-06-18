// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_media as media;
use fuchsia_bluetooth::types::PeerId;
use std::collections::HashSet;
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
    #[error("FIDL Error: {:?}", .0)]
    Fidl(#[from] fidl::Error),
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

mod dai;
use dai::DaiAudioControl;

mod inband;
use inband::InbandAudioControl;

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

/// An AudioControl that either sends the audio directly to the controller (using the DAI)
/// or encodes the audio locally and sends it in the SCO channel, depending on whether the
/// codec is in the list of controller_supported codecs.
pub struct CodecAudioControl {
    controller_codecids: HashSet<CodecId>,
    dai: DaiAudioControl,
    inband: InbandAudioControl,
    started: bool,
}

impl CodecAudioControl {
    pub async fn setup(
        audio_proxy: media::AudioDeviceEnumeratorProxy,
        controller_supported: HashSet<CodecId>,
    ) -> Result<Self, AudioError> {
        let dai = DaiAudioControl::discover(audio_proxy.clone()).await?;
        let inband = InbandAudioControl::create(audio_proxy)?;
        Ok(Self { controller_codecids: controller_supported, dai, inband, started: false })
    }
}

impl AudioControl for CodecAudioControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        codec: CodecId,
    ) -> Result<(), AudioError> {
        if self.started {
            return Err(AudioError::AlreadyStarted);
        }
        let result = if self.controller_codecids.contains(&codec) {
            self.dai.start(id, connection, codec)
        } else {
            self.inband.start(id, connection, codec)
        };
        if result.is_ok() {
            self.started = true;
        }
        result
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if !self.started {
            return Err(AudioError::NotStarted);
        }
        match self.inband.stop() {
            Err(AudioError::NotStarted) => {}
            Ok(()) => {
                self.started = false;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let res = self.dai.stop();
        if res.is_ok() {
            self.started = false;
        }
        res
    }
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
