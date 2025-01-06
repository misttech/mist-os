// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_async::{DurationExt, Task, TimeoutExt};
use fuchsia_bluetooth::types::{A2dpDirection, Channel};
use fuchsia_sync::Mutex;
use futures::stream::Stream;
use futures::{io, FutureExt};
use log::warn;
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, RwLock, Weak};
use std::task::{Context, Poll};
use zx::{MonotonicDuration, Status};

use crate::types::{
    EndpointType, Error, ErrorCode, MediaCodecType, MediaType, Result as AvdtpResult,
    ServiceCapability, ServiceCategory, StreamEndpointId, StreamInformation,
};
use crate::{Peer, SimpleResponder};

pub type StreamEndpointUpdateCallback = Box<dyn Fn(&StreamEndpoint) -> () + Sync + Send>;

/// The state of a StreamEndpoint.
#[derive(PartialEq, Debug, Default, Clone, Copy)]
pub enum StreamState {
    #[default]
    Idle,
    Configured,
    // An Open command has been accepted, but streams have not been established yet.
    Opening,
    Open,
    Streaming,
    Closing,
    Aborting,
}

/// An AVDTP StreamEndpoint. StreamEndpoints represent a particular capability of the application
/// to be a source of sink of media. Included here to aid negotiating the stream connection.
/// See Section 5.3 of the AVDTP 1.3 Specification for more information about the Stream Endpoint
/// Architecture.
pub struct StreamEndpoint {
    /// Local stream endpoint id.  This should be unique per AVDTP Peer.
    id: StreamEndpointId,
    /// The type of endpoint this is (TSEP), Source or Sink.
    endpoint_type: EndpointType,
    /// The media type this stream represents.
    media_type: MediaType,
    /// Current state the stream is in. See Section 6.5 for an overview.
    state: Arc<Mutex<StreamState>>,
    /// The media transport channel
    /// This should be Some(channel) when state is Open or Streaming.
    transport: Option<Arc<RwLock<Channel>>>,
    /// True when the MediaStream is held.
    /// Prevents multiple threads from owning the media stream.
    stream_held: Arc<Mutex<bool>>,
    /// The capabilities of this endpoint.
    capabilities: Vec<ServiceCapability>,
    /// The remote stream endpoint id.  None if the stream has never been configured.
    remote_id: Option<StreamEndpointId>,
    /// The current configuration of this endpoint.  Empty if the stream has never been configured.
    configuration: Vec<ServiceCapability>,
    /// Callback that is run whenever the endpoint is updated
    update_callback: Option<StreamEndpointUpdateCallback>,
    /// In-progress task. This is only used for the Release procedure which places the state in Closing
    /// and must wait for the peer to close transport channels.
    in_progress: Option<Task<()>>,
}

impl fmt::Debug for StreamEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamEndpoint")
            .field("id", &self.id.0)
            .field("endpoint_type", &self.endpoint_type)
            .field("media_type", &self.media_type)
            .field("state", &self.state)
            .field("capabilities", &self.capabilities)
            .field("remote_id", &self.remote_id.as_ref().map(|id| id.to_string()))
            .field("configuration", &self.configuration)
            .finish()
    }
}

impl StreamEndpoint {
    /// Make a new StreamEndpoint.
    /// |id| must be in the valid range for a StreamEndpointId (0x01 - 0x3E).
    /// StreamEndpoints start in the Idle state.
    pub fn new(
        id: u8,
        media_type: MediaType,
        endpoint_type: EndpointType,
        capabilities: Vec<ServiceCapability>,
    ) -> AvdtpResult<StreamEndpoint> {
        let seid = StreamEndpointId::try_from(id)?;
        Ok(StreamEndpoint {
            id: seid,
            capabilities,
            media_type,
            endpoint_type,
            state: Default::default(),
            transport: None,
            stream_held: Arc::new(Mutex::new(false)),
            remote_id: None,
            configuration: vec![],
            update_callback: None,
            in_progress: None,
        })
    }

    pub fn as_new(&self) -> Self {
        StreamEndpoint::new(
            self.id.0,
            self.media_type.clone(),
            self.endpoint_type.clone(),
            self.capabilities.clone(),
        )
        .expect("as_new")
    }

    /// Set the state to the given value and run the `update_callback` afterwards
    fn set_state(&mut self, state: StreamState) {
        *self.state.lock() = state;
        self.update_callback();
    }

    /// Pass update callback to StreamEndpoint that will be called anytime `StreamEndpoint` is
    /// modified.
    pub fn set_update_callback(&mut self, callback: Option<StreamEndpointUpdateCallback>) {
        self.update_callback = callback;
    }

    fn update_callback(&self) {
        if let Some(cb) = self.update_callback.as_ref() {
            cb(self);
        }
    }

    /// Build a new StreamEndpoint from a StreamInformation and associated Capabilities.
    /// This makes it easy to build from AVDTP Discover and GetCapabilities procedures.
    /// StreamEndpooints start in the Idle state.
    pub fn from_info(
        info: &StreamInformation,
        capabilities: Vec<ServiceCapability>,
    ) -> StreamEndpoint {
        StreamEndpoint {
            id: info.id().clone(),
            capabilities,
            media_type: info.media_type().clone(),
            endpoint_type: info.endpoint_type().clone(),
            state: Default::default(),
            transport: None,
            stream_held: Arc::new(Mutex::new(false)),
            remote_id: None,
            configuration: vec![],
            update_callback: None,
            in_progress: None,
        }
    }

    /// Checks that the state is in the set of states.
    /// If not, returns Err(ErrorCode::BadState).
    fn state_is(&self, state: StreamState) -> Result<(), ErrorCode> {
        (*self.state.lock() == state).then_some(()).ok_or(ErrorCode::BadState)
    }

    /// Attempt to Configure this stream using the capabilities given.
    /// If the stream is not in an Idle state, fails with Err(InvalidState).
    /// Used for the Stream Configuration procedure, see Section 6.9
    pub fn configure(
        &mut self,
        remote_id: &StreamEndpointId,
        capabilities: Vec<ServiceCapability>,
    ) -> Result<(), (ServiceCategory, ErrorCode)> {
        self.state_is(StreamState::Idle).map_err(|e| (ServiceCategory::None, e))?;
        self.remote_id = Some(remote_id.clone());
        for cap in &capabilities {
            if !self
                .capabilities
                .iter()
                .any(|y| std::mem::discriminant(cap) == std::mem::discriminant(y))
            {
                return Err((cap.category(), ErrorCode::UnsupportedConfiguration));
            }
        }
        self.configuration = capabilities;
        self.set_state(StreamState::Configured);
        Ok(())
    }

    /// Attempt to reconfigure this stream with the capabilities given.  If any capability is not
    /// valid to set, fails with the first such category and InvalidCapabilities If the stream is
    /// not in the Open state, fails with Err((None, BadState)) Used for the Stream Reconfiguration
    /// procedure, see Section 6.15.
    pub fn reconfigure(
        &mut self,
        mut capabilities: Vec<ServiceCapability>,
    ) -> Result<(), (ServiceCategory, ErrorCode)> {
        self.state_is(StreamState::Open).map_err(|e| (ServiceCategory::None, e))?;
        // Only application capabilities are allowed to be reconfigured. See Section 8.11.1
        if let Some(cap) = capabilities.iter().find(|x| !x.is_application()) {
            return Err((cap.category(), ErrorCode::InvalidCapabilities));
        }
        // Should only replace the capabilities that have been configured. See Section 8.11.2
        let to_replace: std::vec::Vec<_> =
            capabilities.iter().map(|x| std::mem::discriminant(x)).collect();
        self.configuration.retain(|x| {
            let disc = std::mem::discriminant(x);
            !to_replace.contains(&disc)
        });
        self.configuration.append(&mut capabilities);
        self.update_callback();
        Ok(())
    }

    /// Get the current configuration of this stream.
    /// If the stream is not configured, returns None.
    /// Used for the Steam Get Configuration Procedure, see Section 6.10
    pub fn get_configuration(&self) -> Option<&Vec<ServiceCapability>> {
        if self.configuration.is_empty() {
            return None;
        }
        Some(&self.configuration)
    }

    // 100 milliseconds chosen based on end of range testing, to allow for recovery after normal
    // packet delivery continues.
    const SRC_FLUSH_TIMEOUT: MonotonicDuration = MonotonicDuration::from_millis(100);

    /// When a L2CAP channel is received after an Open command is accepted, it should be
    /// delivered via receive_channel.
    /// Returns true if this Endpoint expects more channels to be established before
    /// streaming is started.
    /// Returns Err(InvalidState) if this Endpoint is not expecting a channel to be established,
    /// closing |c|.
    pub fn receive_channel(&mut self, c: Channel) -> AvdtpResult<bool> {
        if self.state_is(StreamState::Opening).is_err() || self.transport.is_some() {
            return Err(Error::InvalidState);
        }
        self.transport = Some(Arc::new(RwLock::new(c)));
        self.try_flush_timeout(Self::SRC_FLUSH_TIMEOUT);
        self.stream_held = Arc::new(Mutex::new(false));
        // TODO(jamuraa, https://fxbug.dev/42051664, https://fxbug.dev/42051776): Reporting and Recovery channels
        self.set_state(StreamState::Open);
        Ok(false)
    }

    /// Begin opening this stream.  The stream must be in a Configured state.
    /// See Stream Establishment, Section 6.11
    pub fn establish(&mut self) -> Result<(), ErrorCode> {
        if self.state_is(StreamState::Configured).is_err() || self.transport.is_some() {
            return Err(ErrorCode::BadState);
        }
        self.set_state(StreamState::Opening);
        Ok(())
    }

    /// Attempts to set audio direction priority of the MediaTransport channel based on
    /// whether the stream is a source or sink endpoint if `active` is true.  If `active` is
    /// false, set the priority to Normal instead.  Does nothing on failure.
    pub fn try_priority(&self, active: bool) {
        let priority = match (active, &self.endpoint_type) {
            (false, _) => A2dpDirection::Normal,
            (true, EndpointType::Source) => A2dpDirection::Source,
            (true, EndpointType::Sink) => A2dpDirection::Sink,
        };
        let fut = match self.transport.as_ref().unwrap().try_read() {
            Err(_) => return,
            Ok(channel) => channel.set_audio_priority(priority).map(|_| ()),
        };
        // TODO(https://fxbug.dev/331621666): We should avoid detaching this.
        Task::spawn(fut).detach();
    }

    /// Attempts to set the flush timeout for the MediaTransport channel, for source endpoints.
    pub fn try_flush_timeout(&self, timeout: MonotonicDuration) {
        if self.endpoint_type != EndpointType::Source {
            return;
        }
        let fut = match self.transport.as_ref().unwrap().try_write() {
            Err(_) => return,
            Ok(channel) => channel.set_flush_timeout(Some(timeout)).map(|_| ()),
        };
        // TODO(https://fxbug.dev/331621666): We should avoid detaching this.
        Task::spawn(fut).detach();
    }

    /// Close this stream.  This procedure will wait until media channels are closed before
    /// transitioning to Idle.  If the channels are not closed in 3 seconds, we initiate an abort
    /// procedure with the remote |peer| to force a transition to Idle.
    pub fn release(&mut self, responder: SimpleResponder, peer: &Peer) -> AvdtpResult<()> {
        {
            let lock = self.state.lock();
            if *lock != StreamState::Open && *lock != StreamState::Streaming {
                return responder.reject(ErrorCode::BadState);
            }
        }
        self.set_state(StreamState::Closing);
        responder.send()?;
        let release_wait_fut = {
            // Take our transport and remote id - after this procedure it will be closed.
            // These must be Some(_) because we are in Open / Streaming state.
            let seid = self.remote_id.take().unwrap();
            let transport = self.transport.take().unwrap();
            let peer = peer.clone();
            let state = self.state.clone();
            async move {
                let Ok(transport) = transport.try_read() else {
                    warn!("unable to lock transport channel, dropping and assuming closed");
                    *state.lock() = StreamState::Idle;
                    return;
                };
                let closed_fut = transport
                    .closed()
                    .on_timeout(MonotonicDuration::from_seconds(3).after_now(), || {
                        Err(Status::TIMED_OUT)
                    });
                if let Err(Status::TIMED_OUT) = closed_fut.await {
                    let _ = peer.abort(&seid).await;
                    *state.lock() = StreamState::Aborting;
                    // As the initiator of the Abort, we close our channel.
                    drop(transport);
                }
                *state.lock() = StreamState::Idle;
            }
        };
        self.in_progress = Some(Task::local(release_wait_fut));
        // Closing will return this endpoint to the Idle state, one way or another with no
        // configuration
        self.configuration.clear();
        self.update_callback();
        Ok(())
    }

    /// Returns the current state of this endpoint.
    pub fn state(&self) -> StreamState {
        *self.state.lock()
    }

    /// Start this stream.  This can be done only from the Open State.
    /// Used for the Stream Start procedure, See Section 6.12
    pub fn start(&mut self) -> Result<(), ErrorCode> {
        self.state_is(StreamState::Open)?;
        self.try_priority(true);
        self.set_state(StreamState::Streaming);
        Ok(())
    }

    /// Suspend this stream.  This can be done only from the Streaming state.
    /// Used for the Stream Suspend procedure, See Section 6.14
    pub fn suspend(&mut self) -> Result<(), ErrorCode> {
        self.state_is(StreamState::Streaming)?;
        self.set_state(StreamState::Open);
        self.try_priority(false);
        Ok(())
    }

    /// Abort this stream.  This can be done from any state, and will always return the state
    /// to Idle.  We are initiating this procedure so will wait for a response and all our
    /// channels will be closed.
    pub async fn initiate_abort<'a>(&'a mut self, peer: &'a Peer) {
        if let Some(seid) = self.remote_id.take() {
            let _ = peer.abort(&seid).await;
            self.set_state(StreamState::Aborting);
        }
        self.abort()
    }

    /// Abort this stream.  This can be done from any state, and will always return the state
    /// to Idle.  We are receiving this abort from the peer, and all our channels will close.
    pub fn abort(&mut self) {
        self.set_state(StreamState::Aborting);
        self.configuration.clear();
        self.remote_id = None;
        self.transport = None;
        self.set_state(StreamState::Idle);
    }

    /// Capabilities of this StreamEndpoint.
    /// Provides support for the Get Capabilities and Get All Capabilities signaling procedures.
    /// See Sections 6.7 and 6.8
    pub fn capabilities(&self) -> &Vec<ServiceCapability> {
        &self.capabilities
    }

    /// Returns the CodecType of this StreamEndpoint.
    /// Returns None if there is no MediaCodec capability in the endpoint.
    /// Note: a MediaCodec capability is required by all endpoints by the spec.
    pub fn codec_type(&self) -> Option<&MediaCodecType> {
        self.capabilities.iter().find_map(|cap| match cap {
            ServiceCapability::MediaCodec { codec_type, .. } => Some(codec_type),
            _ => None,
        })
    }

    /// Returns the local StreamEndpointId for this endpoint.
    pub fn local_id(&self) -> &StreamEndpointId {
        &self.id
    }

    /// Returns the remote StreamEndpointId for this endpoint, if it's configured.
    pub fn remote_id(&self) -> Option<&StreamEndpointId> {
        self.remote_id.as_ref()
    }

    /// Returns the EndpointType of this endpoint
    pub fn endpoint_type(&self) -> &EndpointType {
        &self.endpoint_type
    }

    /// Make a StreamInformation which represents the current state of this stream.
    pub fn information(&self) -> StreamInformation {
        let in_use = self.state_is(StreamState::Idle).is_err();
        StreamInformation::new(
            self.id.clone(),
            in_use,
            self.media_type.clone(),
            self.endpoint_type.clone(),
        )
    }

    /// Take the media transport channel, which transmits (or receives) any media for this
    /// StreamEndpoint.  Returns None if the channel is held already, or if the channel has not
    /// been opened.
    pub fn take_transport(&mut self) -> Option<MediaStream> {
        let mut stream_held = self.stream_held.lock();
        if *stream_held || self.transport.is_none() {
            return None;
        }

        *stream_held = true;

        Some(MediaStream::new(
            self.stream_held.clone(),
            Arc::downgrade(self.transport.as_ref().unwrap()),
        ))
    }
}

/// Represents a media transport stream.
/// If a sink, produces the bytes that have been delivered from the peer.
/// If a source, can send bytes using `send`
pub struct MediaStream {
    in_use: Arc<Mutex<bool>>,
    channel: Weak<RwLock<Channel>>,
}

impl MediaStream {
    pub fn new(in_use: Arc<Mutex<bool>>, channel: Weak<RwLock<Channel>>) -> Self {
        Self { in_use, channel }
    }

    fn try_upgrade(&self) -> Result<Arc<RwLock<Channel>>, io::Error> {
        self.channel
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "lost connection"))
    }

    pub fn max_tx_size(&self) -> Result<usize, io::Error> {
        match self.try_upgrade()?.try_read() {
            Err(_e) => return Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock")),
            Ok(lock) => Ok(lock.max_tx_size()),
        }
    }
}

impl Drop for MediaStream {
    fn drop(&mut self) {
        let mut l = self.in_use.lock();
        *l = false;
    }
}

impl Stream for MediaStream {
    type Item = AvdtpResult<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let arc_chan = match self.try_upgrade() {
            Err(_e) => return Poll::Ready(None),
            Ok(c) => c,
        };
        let lock = match arc_chan.try_write() {
            Err(_e) => return Poll::Ready(None),
            Ok(lock) => lock,
        };
        let mut pin_chan = Pin::new(lock);
        match pin_chan.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(res))) => Poll::Ready(Some(Ok(res))),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(Error::PeerRead(e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl io::AsyncWrite for MediaStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let arc_chan = match self.try_upgrade() {
            Err(e) => return Poll::Ready(Err(e)),
            Ok(c) => c,
        };
        let lock = match arc_chan.try_write() {
            Err(_) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock")))
            }
            Ok(lock) => lock,
        };
        let mut pin_chan = Pin::new(lock);
        pin_chan.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let arc_chan = match self.try_upgrade() {
            Err(e) => return Poll::Ready(Err(e)),
            Ok(c) => c,
        };
        let lock = match arc_chan.try_write() {
            Err(_) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock")))
            }
            Ok(lock) => lock,
        };
        let mut pin_chan = Pin::new(lock);
        pin_chan.as_mut().poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let arc_chan = match self.try_upgrade() {
            Err(e) => return Poll::Ready(Err(e)),
            Ok(c) => c,
        };
        let lock = match arc_chan.try_write() {
            Err(_) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::WouldBlock, "couldn't lock")))
            }
            Ok(lock) => lock,
        };
        let mut pin_chan = Pin::new(lock);
        pin_chan.as_mut().poll_close(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{expect_remote_recv, setup_peer};
    use crate::Request;

    use assert_matches::assert_matches;
    use fidl::endpoints::create_request_stream;
    use futures::io::AsyncWriteExt;
    use futures::stream::StreamExt;
    use {
        fidl_fuchsia_bluetooth as fidl_bt, fidl_fuchsia_bluetooth_bredr as bredr,
        fuchsia_async as fasync,
    };

    const REMOTE_ID_VAL: u8 = 1;
    const REMOTE_ID: StreamEndpointId = StreamEndpointId(REMOTE_ID_VAL);

    #[test]
    fn make() {
        let s = StreamEndpoint::new(
            REMOTE_ID_VAL,
            MediaType::Audio,
            EndpointType::Sink,
            vec![ServiceCapability::MediaTransport],
        );
        assert!(s.is_ok());
        let s = s.unwrap();
        assert_eq!(&StreamEndpointId(1), s.local_id());

        let info = s.information();
        assert!(!info.in_use());

        let no = StreamEndpoint::new(
            0,
            MediaType::Audio,
            EndpointType::Sink,
            vec![ServiceCapability::MediaTransport],
        );
        assert!(no.is_err());
    }

    fn establish_stream(s: &mut StreamEndpoint) -> Channel {
        assert_matches!(s.establish(), Ok(()));
        let (chan, remote) = Channel::create();
        assert_matches!(s.receive_channel(chan), Ok(false));
        remote
    }

    #[test]
    fn from_info() {
        let seid = StreamEndpointId::try_from(5).unwrap();
        let info =
            StreamInformation::new(seid.clone(), false, MediaType::Audio, EndpointType::Sink);
        let capabilities = vec![ServiceCapability::MediaTransport];

        let endpoint = StreamEndpoint::from_info(&info, capabilities);

        assert_eq!(&seid, endpoint.local_id());
        assert_eq!(&false, endpoint.information().in_use());
        assert_eq!(1, endpoint.capabilities().len());
    }

    #[test]
    fn codec_type() {
        let s = StreamEndpoint::new(
            REMOTE_ID_VAL,
            MediaType::Audio,
            EndpointType::Sink,
            vec![
                ServiceCapability::MediaTransport,
                ServiceCapability::MediaCodec {
                    media_type: MediaType::Audio,
                    codec_type: MediaCodecType::new(0x40),
                    codec_extra: vec![0xDE, 0xAD, 0xBE, 0xEF], // Meaningless test data.
                },
            ],
        )
        .unwrap();

        assert_eq!(Some(&MediaCodecType::new(0x40)), s.codec_type());

        let s = StreamEndpoint::new(
            REMOTE_ID_VAL,
            MediaType::Audio,
            EndpointType::Sink,
            vec![ServiceCapability::MediaTransport],
        )
        .unwrap();

        assert_eq!(None, s.codec_type());
    }

    fn test_endpoint(r#type: EndpointType) -> StreamEndpoint {
        StreamEndpoint::new(
            REMOTE_ID_VAL,
            MediaType::Audio,
            r#type,
            vec![
                ServiceCapability::MediaTransport,
                ServiceCapability::MediaCodec {
                    media_type: MediaType::Audio,
                    codec_type: MediaCodecType::new(0x40),
                    codec_extra: vec![0xDE, 0xAD, 0xBE, 0xEF], // Meaningless test data.
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn stream_configure_reconfigure() {
        let _exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);

        // Can't configure items that aren't in range.
        assert_matches!(
            s.configure(&REMOTE_ID, vec![ServiceCapability::Reporting]),
            Err((ServiceCategory::Reporting, ErrorCode::UnsupportedConfiguration))
        );

        assert_matches!(
            s.configure(
                &REMOTE_ID,
                vec![
                    ServiceCapability::MediaTransport,
                    ServiceCapability::MediaCodec {
                        media_type: MediaType::Audio,
                        codec_type: MediaCodecType::new(0x40),
                        // Change the codec_extra which is typical, ex. SBC (A2DP Spec 4.3.2.6)
                        codec_extra: vec![0x0C, 0x0D, 0x02, 0x51],
                    }
                ]
            ),
            Ok(())
        );

        // Note: we allow endpoints to be configured (and reconfigured) again when they
        // are only configured, even though this is probably not allowed per the spec.

        // Can't configure while open
        let _channel = establish_stream(&mut s);

        assert_matches!(
            s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]),
            Err((_, ErrorCode::BadState))
        );

        let reconfiguration = vec![ServiceCapability::MediaCodec {
            media_type: MediaType::Audio,
            codec_type: MediaCodecType::new(0x40),
            // Reconfigure to yet another different codec_extra value.
            codec_extra: vec![0x0C, 0x0D, 0x0E, 0x0F],
        }];

        // The new configuration should match the previous one, but with the reconfigured
        // capabilities updated.
        let new_configuration = vec![ServiceCapability::MediaTransport, reconfiguration[0].clone()];

        // Reconfiguring while open is fine though.
        assert_matches!(s.reconfigure(reconfiguration.clone()), Ok(()));

        assert_eq!(Some(&new_configuration), s.get_configuration());

        // Can't reconfigure non-application types
        assert_matches!(
            s.reconfigure(vec![ServiceCapability::MediaTransport]),
            Err((ServiceCategory::MediaTransport, ErrorCode::InvalidCapabilities))
        );

        // Can't configure or reconfigure while streaming
        assert_matches!(s.start(), Ok(()));

        assert_matches!(
            s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]),
            Err((_, ErrorCode::BadState))
        );

        assert_matches!(s.reconfigure(reconfiguration.clone()), Err((_, ErrorCode::BadState)));

        assert_matches!(s.suspend(), Ok(()));

        // Reconfigure should be fine again in open state.
        assert_matches!(s.reconfigure(reconfiguration.clone()), Ok(()));

        // Configure is still not allowed.
        assert_matches!(
            s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]),
            Err((_, ErrorCode::BadState))
        );
    }

    #[test]
    fn stream_establishment() {
        let _exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);

        let (remote, transport) = Channel::create();

        // Can't establish before configuring
        assert_matches!(s.establish(), Err(ErrorCode::BadState));

        // Trying to receive a channel in the wrong state closes the channel
        assert_matches!(s.receive_channel(transport), Err(Error::InvalidState));

        let buf: &mut [u8] = &mut [0; 1];

        assert_matches!(remote.read(buf), Err(zx::Status::PEER_CLOSED));

        assert_matches!(s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]), Ok(()));

        assert_matches!(s.establish(), Ok(()));

        // And we should be able to give a channel now.
        let (_remote, transport) = Channel::create();
        assert_matches!(s.receive_channel(transport), Ok(false));
    }

    fn setup_peer_for_release(exec: &mut fasync::TestExecutor) -> (Peer, Channel, SimpleResponder) {
        let (peer, signaling) = setup_peer();
        // Send a close from the other side to produce an event we can respond to.
        let _ = signaling.write(&[0x40, 0x08, 0x04]).expect("signaling write");
        let mut req_stream = peer.take_request_stream();
        let mut req_fut = req_stream.next();
        let complete = exec.run_until_stalled(&mut req_fut);
        let responder = match complete {
            Poll::Ready(Some(Ok(Request::Close { responder, .. }))) => responder,
            _ => panic!("Expected a close request"),
        };
        (peer, signaling, responder)
    }

    #[test]
    fn stream_release_without_abort() {
        let mut exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);

        assert_matches!(s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]), Ok(()));

        let remote_transport = establish_stream(&mut s);

        let (peer, signaling, responder) = setup_peer_for_release(&mut exec);

        // We expect release to succeed in this state.
        s.release(responder, &peer).unwrap();
        // Expect a "yes" response.
        expect_remote_recv(&[0x42, 0x08], &signaling);

        // Close the transport channel by dropping it.
        drop(remote_transport);

        // After the transport is closed we should transition to Idle.
        let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        assert_eq!(s.state(), StreamState::Idle);
    }

    #[test]
    fn test_mediastream() {
        let mut exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);

        assert_matches!(s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]), Ok(()));

        // Before the stream is opened, we shouldn't be able to take the transport.
        assert!(s.take_transport().is_none());

        let remote_transport = establish_stream(&mut s);

        // Should be able to get the transport from the stream now.
        let temp_stream = s.take_transport();
        assert!(temp_stream.is_some());

        // But only once
        assert!(s.take_transport().is_none());

        // Until you drop the stream
        drop(temp_stream);

        let media_stream = s.take_transport();
        assert!(media_stream.is_some());
        let mut media_stream = media_stream.unwrap();

        // Max TX size is taken from the underlying channel.
        assert_matches!(media_stream.max_tx_size(), Ok(Channel::DEFAULT_MAX_TX));

        // Writing to the media stream should send it through the transport channel.
        let hearts = &[0xF0, 0x9F, 0x92, 0x96, 0xF0, 0x9F, 0x92, 0x96];
        let mut write_fut = media_stream.write(hearts);

        assert_matches!(exec.run_until_stalled(&mut write_fut), Poll::Ready(Ok(8)));

        expect_remote_recv(hearts, &remote_transport);

        // Closing the media stream should close the channel.
        let mut close_fut = media_stream.close();
        assert_matches!(exec.run_until_stalled(&mut close_fut), Poll::Ready(Ok(())));
        // Note: there's no effect on the other end of the channel when a close occurs,
        // until the channel is dropped.

        drop(s);

        // Reading from the remote end should fail.
        let mut result = vec![0];
        assert_matches!(remote_transport.read(&mut result[..]), Err(zx::Status::PEER_CLOSED));

        // After the stream is gone, any write should return an Err
        let mut write_fut = media_stream.write(&[0xDE, 0xAD]);
        assert_matches!(exec.run_until_stalled(&mut write_fut), Poll::Ready(Err(_)));

        // After the stream is gone, the stream should be fused done.
        let mut next_fut = media_stream.next();
        assert_matches!(exec.run_until_stalled(&mut next_fut), Poll::Ready(None));

        // And the Max TX should be an error.
        assert_matches!(media_stream.max_tx_size(), Err(_));
    }

    #[test]
    fn stream_release_with_abort() {
        let mut exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);

        assert_matches!(s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]), Ok(()));
        let remote_transport = establish_stream(&mut s);
        let (peer, mut signaling, responder) = setup_peer_for_release(&mut exec);

        // We expect release to succeed in this state, then start the task to wait for the close.
        s.release(responder, &peer).unwrap();
        // Expect a "yes" response.
        expect_remote_recv(&[0x42, 0x08], &signaling);

        // Should get an abort
        let next = std::pin::pin!(signaling.next());
        let received =
            exec.run_singlethreaded(next).expect("channel not closed").expect("successful read");
        assert_eq!(0x0A, received[1]);
        let txlabel = received[0] & 0xF0;
        // Send a response
        assert!(signaling.write(&[txlabel | 0x02, 0x0A]).is_ok());

        let _ = exec.run_singlethreaded(&mut remote_transport.closed());

        // We will then end up in Idle.
        while s.state() != StreamState::Idle {
            let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        }
    }

    #[test]
    fn start_and_suspend() {
        let mut exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);

        // Can't start or suspend until configured and open.
        assert_matches!(s.start(), Err(ErrorCode::BadState));
        assert_matches!(s.suspend(), Err(ErrorCode::BadState));

        assert_matches!(s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]), Ok(()));

        assert_matches!(s.start(), Err(ErrorCode::BadState));
        assert_matches!(s.suspend(), Err(ErrorCode::BadState));

        assert_matches!(s.establish(), Ok(()));

        assert_matches!(s.start(), Err(ErrorCode::BadState));
        assert_matches!(s.suspend(), Err(ErrorCode::BadState));

        let (remote, local) = zx::Socket::create_datagram();
        let (client_end, mut direction_request_stream) =
            create_request_stream::<bredr::AudioDirectionExtMarker>();
        let ext = bredr::Channel {
            socket: Some(local),
            channel_mode: Some(fidl_bt::ChannelMode::Basic),
            max_tx_sdu_size: Some(1004),
            ext_direction: Some(client_end),
            ..Default::default()
        };
        let transport = Channel::try_from(ext).unwrap();
        assert_matches!(s.receive_channel(transport), Ok(false));

        // Should be able to start but not suspend now.
        assert_matches!(s.suspend(), Err(ErrorCode::BadState));
        assert_matches!(s.start(), Ok(()));

        match exec.run_until_stalled(&mut direction_request_stream.next()) {
            Poll::Ready(Some(Ok(bredr::AudioDirectionExtRequest::SetPriority {
                priority,
                responder,
            }))) => {
                assert_eq!(bredr::A2dpDirectionPriority::Sink, priority);
                responder.send(Ok(())).expect("response to send cleanly");
            }
            x => panic!("Expected a item to be ready on the request stream, got {:?}", x),
        };

        // Are started, so we should be able to suspend but not start again here.
        assert_matches!(s.start(), Err(ErrorCode::BadState));
        assert_matches!(s.suspend(), Ok(()));

        match exec.run_until_stalled(&mut direction_request_stream.next()) {
            Poll::Ready(Some(Ok(bredr::AudioDirectionExtRequest::SetPriority {
                priority,
                responder,
            }))) => {
                assert_eq!(bredr::A2dpDirectionPriority::Normal, priority);
                responder.send(Ok(())).expect("response to send cleanly");
            }
            x => panic!("Expected a item to be ready on the request stream, got {:?}", x),
        };

        // Now we're suspended, so we can start it again.
        assert_matches!(s.start(), Ok(()));
        assert_matches!(s.suspend(), Ok(()));

        // After we close, we are back at idle and can't start / stop
        let (peer, signaling, responder) = setup_peer_for_release(&mut exec);

        {
            s.release(responder, &peer).unwrap();
            // Expect a "yes" response.
            expect_remote_recv(&[0x42, 0x08], &signaling);
            // Close the transport channel by dropping it.
            drop(remote);
            while s.state() != StreamState::Idle {
                let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
            }
        }

        // Shouldn't be able to start or suspend again.
        assert_matches!(s.start(), Err(ErrorCode::BadState));
        assert_matches!(s.suspend(), Err(ErrorCode::BadState));
    }

    fn receive_l2cap_params_channel(
        s: &mut StreamEndpoint,
    ) -> (zx::Socket, bredr::L2capParametersExtRequestStream) {
        assert_matches!(s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport]), Ok(()));
        assert_matches!(s.establish(), Ok(()));

        let (remote, local) = zx::Socket::create_datagram();
        let (client_end, l2cap_params_requests) =
            create_request_stream::<bredr::L2capParametersExtMarker>();
        let ext = bredr::Channel {
            socket: Some(local),
            channel_mode: Some(fidl_bt::ChannelMode::Basic),
            max_tx_sdu_size: Some(1004),
            ext_l2cap: Some(client_end),
            ..Default::default()
        };
        let transport = Channel::try_from(ext).unwrap();
        assert_matches!(s.receive_channel(transport), Ok(false));
        (remote, l2cap_params_requests)
    }

    #[test]
    fn sets_flush_timeout_for_source_transports() {
        let mut exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Source);
        let (_remote, mut l2cap_params_requests) = receive_l2cap_params_channel(&mut s);

        // Should request to set the flush timeout.
        match exec.run_until_stalled(&mut l2cap_params_requests.next()) {
            Poll::Ready(Some(Ok(bredr::L2capParametersExtRequest::RequestParameters {
                request,
                responder,
            }))) => {
                assert_eq!(
                    Some(StreamEndpoint::SRC_FLUSH_TIMEOUT.into_nanos()),
                    request.flush_timeout
                );
                responder.send(&request).expect("response to send cleanly");
            }
            x => panic!("Expected a item to be ready on the request stream, got {:?}", x),
        };
    }

    #[test]
    fn no_flush_timeout_for_sink_transports() {
        let mut exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);
        let (_remote, mut l2cap_params_requests) = receive_l2cap_params_channel(&mut s);

        // Should NOT request to set the flush timeout.
        match exec.run_until_stalled(&mut l2cap_params_requests.next()) {
            Poll::Pending => {}
            x => panic!("Expected no request to set flush timeout, got {:?}", x),
        };
    }

    #[test]
    fn get_configuration() {
        let mut s = test_endpoint(EndpointType::Sink);

        // Can't get configuration if we aren't configured.
        assert!(s.get_configuration().is_none());

        let config = vec![
            ServiceCapability::MediaTransport,
            ServiceCapability::MediaCodec {
                media_type: MediaType::Audio,
                codec_type: MediaCodecType::new(0),
                // Change the codec_extra which is typical, ex. SBC (A2DP Spec 4.3.2.6)
                codec_extra: vec![0x60, 0x0D, 0x02, 0x55],
            },
        ];

        assert_matches!(s.configure(&REMOTE_ID, config.clone()), Ok(()));

        match s.get_configuration() {
            Some(c) => assert_eq!(&config, c),
            x => panic!("Expected Ok from get_configuration but got {:?}", x),
        };

        // Abort this stream, putting it back to the idle state.
        s.abort();

        assert!(s.get_configuration().is_none());
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Create a callback that tracks how many times it has been called
    fn call_count_callback() -> (Option<StreamEndpointUpdateCallback>, Arc<AtomicUsize>) {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_reader = call_count.clone();
        let count_cb: StreamEndpointUpdateCallback = Box::new(move |_stream: &StreamEndpoint| {
            let _ = call_count.fetch_add(1, Ordering::SeqCst);
        });
        (Some(count_cb), call_count_reader)
    }

    /// Test that the update callback is run at least once for all methods that mutate the state of
    /// the StreamEndpoint. This is done through an atomic counter in the callback that increments
    /// when the callback is run.
    ///
    /// Note that the _results_ of calling these mutating methods on the state of StreamEndpoint are
    /// not validated here. They are validated in other tests.
    #[test]
    fn update_callback() {
        // Need an executor to make a socket
        let _exec = fasync::TestExecutor::new();
        let mut s = test_endpoint(EndpointType::Sink);
        let (cb, call_count) = call_count_callback();
        s.set_update_callback(cb);

        s.configure(&REMOTE_ID, vec![ServiceCapability::MediaTransport])
            .expect("Configure to succeed in test");
        assert!(call_count.load(Ordering::SeqCst) > 0, "Update callback called at least once");
        call_count.store(0, Ordering::SeqCst); // clear call count

        s.establish().expect("Establish to succeed in test");
        assert!(call_count.load(Ordering::SeqCst) > 0, "Update callback called at least once");
        call_count.store(0, Ordering::SeqCst); // clear call count

        let (_, transport) = Channel::create();
        assert_eq!(
            s.receive_channel(transport).expect("Receive channel to succeed in test"),
            false
        );
        assert!(call_count.load(Ordering::SeqCst) > 0, "Update callback called at least once");
        call_count.store(0, Ordering::SeqCst); // clear call count

        s.start().expect("Start to succeed in test");
        assert!(call_count.load(Ordering::SeqCst) > 0, "Update callback called at least once");
        call_count.store(0, Ordering::SeqCst); // clear call count

        s.suspend().expect("Suspend to succeed in test");
        assert!(call_count.load(Ordering::SeqCst) > 0, "Update callback called at least once");
        call_count.store(0, Ordering::SeqCst); // clear call count

        s.reconfigure(vec![]).expect("Reconfigure to succeed in test");
        assert!(call_count.load(Ordering::SeqCst) > 0, "Update callback called at least once");
        call_count.store(0, Ordering::SeqCst); // clear call count

        // Abort this stream, putting it back to the idle state.
        s.abort();
    }
}
