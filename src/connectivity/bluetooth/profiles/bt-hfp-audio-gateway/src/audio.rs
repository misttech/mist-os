// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::{PeerId, Uuid};
use fuchsia_sync::Mutex;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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
    #[error("Operation is in progress: {}", .description)]
    InProgress { description: String },
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

#[derive(Debug)]
pub enum AudioControlEvent {
    /// Request from AudioControl to start the audio for a connected Peer.
    /// AudioControl::start() or AudioControl::failed_request() should be called after this event
    /// has been received; incompatible calls may return `AudioError::InProgress`.
    RequestStart { id: PeerId },
    /// Request from the AudioControl to stop the audio to a connected Peer.
    /// Any calls for which audio is currently routed to the HF for this peer will be transferred
    /// to the AG.
    /// AudioControl::stop() should be called after this event has been received; incompatible
    /// calls made in this state may return `AudioError::InProgress`
    /// Note that AudioControl::failed_request() may not be called for this event (audio can
    /// always be stopped)
    RequestStop { id: PeerId },
    /// Event produced when an audio path has been started and audio is flowing to/from the peer.
    Started { id: PeerId },
    /// Event produced when the audio path has been stopped and audio is not flowing to/from the
    /// peer.
    /// This event can be spontaeously produced by the AudioControl implementation to indicate an
    /// error in the audio path (either during or after a requested start).
    Stopped { id: PeerId, error: Option<AudioError> },
}

impl AudioControlEvent {
    pub fn id(&self) -> PeerId {
        match self {
            AudioControlEvent::RequestStart { id } => *id,
            AudioControlEvent::RequestStop { id } => *id,
            AudioControlEvent::Started { id } => *id,
            AudioControlEvent::Stopped { id, error: _ } => *id,
        }
    }
}

pub trait AudioControl: Send {
    /// Send to indicate when connected to a peer. `supported_codecs` indicates the set of codecs which are
    /// communicated from the peer.  Depending on the audio control implementation,
    /// this may add a (stopped) media device.  Audio control implementations can request audio be started
    /// for peers that are connected.
    fn connect(&mut self, id: PeerId, supported_codecs: &[CodecId]);

    /// Send to indicate that a peer has been disconnected.  This shall tear down any audio path
    /// set up for the peer and send a `AudioControlEvent::Stopped` for each.  This shall be idempotent
    /// (calling disconnect on a disconnected PeerId does nothing)
    fn disconnect(&mut self, id: PeerId);

    /// Request to start sending audio to the peer.  If the request succeeds `Ok(())` will be
    /// returned, but audio may not be started until a `AudioControlEvent::Started` event is
    /// produced in the events.
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        codec: CodecId,
    ) -> Result<(), AudioError>;

    /// Request to stop the audio to a peer.
    /// If the Audio is not started, an Err(AudioError::NotStarted) will be returned.
    /// If the requests succeeds `Ok(())` will be returned but audio may not be stopped until a
    /// `AudioControlEvent::Stopped` is produced in the events.
    fn stop(&mut self, id: PeerId) -> Result<(), AudioError>;

    /// Get a stream of the events produced by this audio control.
    /// May panic if the event stream has already been taken.
    fn take_events(&self) -> BoxStream<'static, AudioControlEvent>;

    /// Respond with failure to a request from the event stream.
    /// `request` should be the request that failed.  If a request was not made by this audio
    /// control the failure shall be ignored.
    fn failed_request(&self, request: AudioControlEvent, error: AudioError);
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
    /// The set of started peers. Value is true if the audio encoding is handled by the controller.
    started: HashMap<PeerId, bool>,
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
            started: Default::default(),
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
        if self.started.contains_key(&id) {
            return Err(AudioError::AlreadyStarted);
        }
        let result = if self.offload_codecids.contains(&codec) {
            self.offload.start(id, connection, codec)
        } else {
            self.inband.start(id, connection, codec)
        };
        if result.is_ok() {
            let _ = self.started.insert(id, self.offload_codecids.contains(&codec));
        }
        result
    }

    fn stop(&mut self, id: PeerId) -> Result<(), AudioError> {
        let stop_result = match self.started.get(&id) {
            None => return Err(AudioError::NotStarted),
            Some(true) => self.offload.stop(id),
            Some(false) => self.inband.stop(id),
        };
        if stop_result.is_ok() {
            let _ = self.started.remove(&id);
        }
        stop_result
    }

    fn connect(&mut self, id: PeerId, supported_codecs: &[CodecId]) {
        // TODO(b/341114499): Consider not connecting this here, since it could create a device we
        // don't want to use.
        self.inband.connect(id, supported_codecs);
        if supported_codecs.iter().any(|i| self.offload_codecids.contains(i)) {
            self.offload.connect(id, supported_codecs);
        }
    }

    fn disconnect(&mut self, id: PeerId) {
        self.inband.disconnect(id);
        self.offload.disconnect(id);
    }

    fn take_events(&self) -> BoxStream<'static, AudioControlEvent> {
        let inband_events = self.inband.take_events();
        let controller_events = self.offload.take_events();
        futures::stream::select_all([inband_events, controller_events]).boxed()
    }

    fn failed_request(&self, request: AudioControlEvent, error: AudioError) {
        // We only support requests from the controller AudioControl (inband does not make
        // requests).
        self.offload.failed_request(request, error);
    }
}

struct TestAudioControlInner {
    started: HashSet<PeerId>,
    _connected: HashMap<PeerId, HashSet<CodecId>>,
    connections: HashMap<PeerId, ScoConnection>,
    event_sender: futures::channel::mpsc::Sender<AudioControlEvent>,
}

#[derive(Clone)]
pub struct TestAudioControl {
    inner: Arc<Mutex<TestAudioControlInner>>,
}

#[cfg(test)]
impl TestAudioControl {
    pub fn unexpected_stop(&self, id: PeerId, error: AudioError) {
        let mut lock = self.inner.lock();
        let _ = lock.started.remove(&id);
        let _ = lock.connections.remove(&id);
        let _ = lock.event_sender.try_send(AudioControlEvent::Stopped { id, error: Some(error) });
    }

    pub fn is_started(&self, id: PeerId) -> bool {
        let lock = self.inner.lock();
        lock.started.contains(&id) && lock.connections.contains_key(&id)
    }
}

impl Default for TestAudioControl {
    fn default() -> Self {
        // Make a disconnected sender, we do not care about whether it succeeds.
        let (event_sender, _) = futures::channel::mpsc::channel(0);
        Self {
            inner: Arc::new(Mutex::new(TestAudioControlInner {
                started: Default::default(),
                _connected: Default::default(),
                connections: Default::default(),
                event_sender,
            })),
        }
    }
}

impl AudioControl for TestAudioControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: ScoConnection,
        _codec: CodecId,
    ) -> Result<(), AudioError> {
        let mut lock = self.inner.lock();
        if !lock.started.insert(id) {
            return Err(AudioError::AlreadyStarted);
        }
        let _ = lock.connections.insert(id, connection);
        let _ = lock.event_sender.try_send(AudioControlEvent::Started { id });
        Ok(())
    }

    fn stop(&mut self, id: PeerId) -> Result<(), AudioError> {
        let mut lock = self.inner.lock();
        if !lock.started.remove(&id) {
            return Err(AudioError::NotStarted);
        }
        let _ = lock.connections.remove(&id);
        let _ = lock.event_sender.try_send(AudioControlEvent::Stopped { id, error: None });
        Ok(())
    }

    fn connect(&mut self, id: PeerId, supported_codecs: &[CodecId]) {
        let mut lock = self.inner.lock();
        let _ = lock._connected.insert(id, supported_codecs.iter().cloned().collect());
    }

    fn disconnect(&mut self, id: PeerId) {
        let _ = self.stop(id);
        let mut lock = self.inner.lock();
        let _ = lock._connected.remove(&id);
    }

    fn take_events(&self) -> BoxStream<'static, AudioControlEvent> {
        let mut lock = self.inner.lock();
        // Replace the sender.
        let (sender, receiver) = futures::channel::mpsc::channel(1);
        lock.event_sender = sender;
        receiver.boxed()
    }

    fn failed_request(&self, _request: AudioControlEvent, _error: AudioError) {
        // Nothing to do here for the moment
    }
}
