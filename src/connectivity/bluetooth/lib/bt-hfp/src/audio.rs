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

use crate::codec_id::CodecId;
use crate::sco;

#[derive(Error, Debug)]
pub enum Error {
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

impl Error {
    fn audio_core(e: anyhow::Error) -> Self {
        Self::AudioCore { source: e }
    }
}

mod dai;
use dai::DaiControl;

mod inband;
use inband::InbandControl;

mod codec;
pub use codec::CodecControl;

const DEVICE_NAME: &'static str = "Bluetooth HFP";

// Used to build audio device IDs for peers
const HF_INPUT_UUID: Uuid =
    Uuid::new16(bredr::ServiceClassProfileIdentifier::Handsfree.into_primitive());
const HF_OUTPUT_UUID: Uuid =
    Uuid::new16(bredr::ServiceClassProfileIdentifier::HandsfreeAudioGateway.into_primitive());

#[derive(Debug)]
pub enum ControlEvent {
    /// Request from Control to start the audio for a connected Peer.
    /// Control::start() or Control::failed_request() should be called after this event
    /// has been received; incompatible calls may return `Error::InProgress`.
    RequestStart { id: PeerId },
    /// Request from the Control to stop the audio to a connected Peer.
    /// Any calls for which audio is currently routed to the HF for this peer will be transferred
    /// to the AG.
    /// Control::stop() should be called after this event has been received; incompatible
    /// calls made in this state may return `Error::InProgress`
    /// Note that Control::failed_request() may not be called for this event (audio can
    /// always be stopped)
    RequestStop { id: PeerId },
    /// Event produced when an audio path has been started and audio is flowing to/from the peer.
    Started { id: PeerId },
    /// Event produced when the audio path has been stopped and audio is not flowing to/from the
    /// peer.
    /// This event can be spontaeously produced by the Control implementation to indicate an
    /// error in the audio path (either during or after a requested start).
    Stopped { id: PeerId, error: Option<Error> },
}

impl ControlEvent {
    pub fn id(&self) -> PeerId {
        match self {
            ControlEvent::RequestStart { id } => *id,
            ControlEvent::RequestStop { id } => *id,
            ControlEvent::Started { id } => *id,
            ControlEvent::Stopped { id, error: _ } => *id,
        }
    }
}

pub trait Control: Send {
    /// Send to indicate when connected to a peer. `supported_codecs` indicates the set of codecs which are
    /// communicated from the peer.  Depending on the audio control implementation,
    /// this may add a (stopped) media device.  Audio control implementations can request audio be started
    /// for peers that are connected.
    fn connect(&mut self, id: PeerId, supported_codecs: &[CodecId]);

    /// Send to indicate that a peer has been disconnected.  This shall tear down any audio path
    /// set up for the peer and send a `ControlEvent::Stopped` for each.  This shall be idempotent
    /// (calling disconnect on a disconnected PeerId does nothing)
    fn disconnect(&mut self, id: PeerId);

    /// Request to start sending audio to the peer.  If the request succeeds `Ok(())` will be
    /// returned, but audio may not be started until a `ControlEvent::Started` event is
    /// produced in the events.
    fn start(
        &mut self,
        id: PeerId,
        connection: sco::Connection,
        codec: CodecId,
    ) -> Result<(), Error>;

    /// Request to stop the audio to a peer.
    /// If the Audio is not started, an Err(Error::NotStarted) will be returned.
    /// If the requests succeeds `Ok(())` will be returned but audio may not be stopped until a
    /// `ControlEvent::Stopped` is produced in the events.
    fn stop(&mut self, id: PeerId) -> Result<(), Error>;

    /// Get a stream of the events produced by this audio control.
    /// May panic if the event stream has already been taken.
    fn take_events(&self) -> BoxStream<'static, ControlEvent>;

    /// Respond with failure to a request from the event stream.
    /// `request` should be the request that failed.  If a request was not made by this audio
    /// control the failure shall be ignored.
    fn failed_request(&self, request: ControlEvent, error: Error);
}

/// A Control that either sends the audio directly to the controller (using an offload
/// Control) or encodes audio locally and sends it in the SCO channel, depending on
/// whether the codec is in the list of offload-supported codecs.
pub struct PartialOffloadControl {
    offload_codecids: HashSet<CodecId>,
    /// Used to control when the audio can be sent offloaded
    offload: Box<dyn Control>,
    /// Used to encode audio locally and send inband
    inband: InbandControl,
    /// The set of started peers. Value is true if the audio encoding is handled by the controller.
    started: HashMap<PeerId, bool>,
}

impl PartialOffloadControl {
    pub async fn setup_audio_core(
        audio_proxy: media::AudioDeviceEnumeratorProxy,
        offload_supported: HashSet<CodecId>,
    ) -> Result<Self, Error> {
        let dai = DaiControl::discover(audio_proxy.clone()).await?;
        let inband = InbandControl::create(audio_proxy)?;
        Ok(Self {
            offload_codecids: offload_supported,
            offload: Box::new(dai),
            inband,
            started: Default::default(),
        })
    }
}

impl Control for PartialOffloadControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: sco::Connection,
        codec: CodecId,
    ) -> Result<(), Error> {
        if self.started.contains_key(&id) {
            return Err(Error::AlreadyStarted);
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

    fn stop(&mut self, id: PeerId) -> Result<(), Error> {
        let stop_result = match self.started.get(&id) {
            None => return Err(Error::NotStarted),
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

    fn take_events(&self) -> BoxStream<'static, ControlEvent> {
        let inband_events = self.inband.take_events();
        let controller_events = self.offload.take_events();
        futures::stream::select_all([inband_events, controller_events]).boxed()
    }

    fn failed_request(&self, request: ControlEvent, error: Error) {
        // We only support requests from the controller Control (inband does not make
        // requests).
        self.offload.failed_request(request, error);
    }
}

struct TestControlInner {
    started: HashSet<PeerId>,
    connected: HashMap<PeerId, HashSet<CodecId>>,
    connections: HashMap<PeerId, sco::Connection>,
    event_sender: futures::channel::mpsc::Sender<ControlEvent>,
}

#[derive(Clone)]
pub struct TestControl {
    inner: Arc<Mutex<TestControlInner>>,
}

impl TestControl {
    pub fn unexpected_stop(&self, id: PeerId, error: Error) {
        let mut lock = self.inner.lock();
        let _ = lock.started.remove(&id);
        let _ = lock.connections.remove(&id);
        let _ = lock.event_sender.try_send(ControlEvent::Stopped { id, error: Some(error) });
    }

    pub fn is_started(&self, id: PeerId) -> bool {
        let lock = self.inner.lock();
        lock.started.contains(&id) && lock.connections.contains_key(&id)
    }

    pub fn is_connected(&self, id: PeerId) -> bool {
        let lock = self.inner.lock();
        lock.connected.contains_key(&id)
    }
}

impl Default for TestControl {
    fn default() -> Self {
        // Make a disconnected sender, we do not care about whether it succeeds.
        let (event_sender, _) = futures::channel::mpsc::channel(0);
        Self {
            inner: Arc::new(Mutex::new(TestControlInner {
                started: Default::default(),
                connected: Default::default(),
                connections: Default::default(),
                event_sender,
            })),
        }
    }
}

impl Control for TestControl {
    fn start(
        &mut self,
        id: PeerId,
        connection: sco::Connection,
        _codec: CodecId,
    ) -> Result<(), Error> {
        let mut lock = self.inner.lock();
        if !lock.started.insert(id) {
            return Err(Error::AlreadyStarted);
        }
        let _ = lock.connections.insert(id, connection);
        let _ = lock.event_sender.try_send(ControlEvent::Started { id });
        Ok(())
    }

    fn stop(&mut self, id: PeerId) -> Result<(), Error> {
        let mut lock = self.inner.lock();
        if !lock.started.remove(&id) {
            return Err(Error::NotStarted);
        }
        let _ = lock.connections.remove(&id);
        let _ = lock.event_sender.try_send(ControlEvent::Stopped { id, error: None });
        Ok(())
    }

    fn connect(&mut self, id: PeerId, supported_codecs: &[CodecId]) {
        let mut lock = self.inner.lock();
        let _ = lock.connected.insert(id, supported_codecs.iter().cloned().collect());
    }

    fn disconnect(&mut self, id: PeerId) {
        let _ = self.stop(id);
        let mut lock = self.inner.lock();
        let _ = lock.connected.remove(&id);
    }

    fn take_events(&self) -> BoxStream<'static, ControlEvent> {
        let mut lock = self.inner.lock();
        // Replace the sender.
        let (sender, receiver) = futures::channel::mpsc::channel(1);
        lock.event_sender = sender;
        receiver.boxed()
    }

    fn failed_request(&self, _request: ControlEvent, _error: Error) {
        // Nothing to do here for the moment
    }
}
