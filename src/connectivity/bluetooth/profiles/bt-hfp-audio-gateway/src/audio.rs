// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::{PeerId, Uuid};
use std::collections::HashSet;
use thiserror::Error;
use {fidl_fuchsia_bluetooth_bredr as bredr, fidl_fuchsia_media as media};

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

mod codec;

const DEVICE_NAME: &'static str = "Bluetooth HFP";

// Used to build audio device IDs for peers
const HF_INPUT_UUID: Uuid =
    Uuid::new16(bredr::ServiceClassProfileIdentifier::Handsfree.into_primitive());
const HF_OUTPUT_UUID: Uuid =
    Uuid::new16(bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway.into_primitive());

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

/// An AudioControl that either sends the audio directly to the controller (using an offload
/// AudioControl) or encodes audio locally and sends it in the SCO channel, depending on
/// whether the codec is in the list of offload-supported codecs.
pub struct PartialOffloadAudioControl {
    offload_codecids: HashSet<CodecId>,
    /// Used to control when the audio can be sent offloaded
    offload: Box<dyn AudioControl>,
    /// Used to encode audio locally and send inband
    inband: InbandAudioControl,
    started: bool,
}

impl PartialOffloadAudioControl {
    pub async fn setup(
        audio_proxy: media::AudioDeviceEnumeratorProxy,
        offload_supported: HashSet<CodecId>,
    ) -> Result<Self, AudioError> {
        let dai = DaiAudioControl::discover(audio_proxy.clone()).await?;
        let inband = InbandAudioControl::create(audio_proxy)?;
        Ok(Self {
            offload_codecids: offload_supported,
            offload: Box::new(dai),
            inband,
            started: false,
        })
    }
}

impl AudioControl for PartialOffloadAudioControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        codec: CodecId,
    ) -> Result<(), AudioError> {
        if self.started {
            return Err(AudioError::AlreadyStarted);
        }
        let result = if self.offload_codecids.contains(&codec) {
            self.offload.start(id, connection, codec)
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
        let res = self.offload.stop();
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
