// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context as _;
use bt_avdtp::{
    self as avdtp, MediaCodecType, ServiceCapability, ServiceCategory, StreamEndpoint,
    StreamEndpointId,
};
use fidl_fuchsia_bluetooth_bredr::{
    ChannelParameters, ConnectParameters, L2capParameters, ProfileDescriptor, ProfileProxy,
    PSM_AVDTP,
};
use fuchsia_async::{self as fasync, DurationExt};
use fuchsia_bluetooth::{
    inspect::DebugExt,
    types::{Channel, PeerId},
};
use fuchsia_inspect as inspect;
use fuchsia_inspect_derive::{AttachError, Inspect};
use fuchsia_zircon as zx;
use futures::{
    channel::mpsc,
    future::BoxFuture,
    select,
    stream::FuturesUnordered,
    task::{Context, Poll, Waker},
    Future, FutureExt, StreamExt,
};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    convert::TryInto,
    pin::Pin,
    sync::{Arc, Weak},
};
use tracing::{error, info, trace, warn};

/// For sending out-of-band commands over the A2DP peer.
mod controller;
pub use controller::ControllerPool;

use crate::permits::{Permit, Permits};
use crate::stream::{Stream, Streams};

/// A Peer represents an A2DP peer which may be connected to this device.
/// Only one A2DP peer should exist for each Bluetooth peer.
#[derive(Inspect)]
pub struct Peer {
    /// The id of the peer we are connected to.
    id: PeerId,
    /// Inner keeps track of the peer and the streams.
    #[inspect(forward)]
    inner: Arc<Mutex<PeerInner>>,
    /// Profile Proxy to connect new transport channels
    profile: ProfileProxy,
    /// The profile descriptor for this peer, if it has been discovered.
    descriptor: Mutex<Option<ProfileDescriptor>>,
    /// Wakers that are to be woken when the peer disconnects.  If None, the peers have been woken
    /// and this peer is disconnected.  Shared weakly with ClosedPeer future objects that complete
    /// when the peer disconnects.
    closed_wakers: Arc<Mutex<Option<Vec<Waker>>>>,
    /// Used to report peer metrics to Cobalt.
    metrics: bt_metrics::MetricsLogger,
    /// A task waiting to start a stream if it hasn't been started yet.
    start_stream_task: Mutex<Option<fasync::Task<avdtp::Result<()>>>>,
}

/// StreamPermits handles reserving and retrieving permits for streaming audio.
/// Reservations are automatically retrieved for streams that are revoked, and when the
/// reservation is completed, the permit is stored and a StreamPermit is sent so it can be started.
#[derive(Clone)]
struct StreamPermits {
    permits: Permits,
    open_streams: Arc<Mutex<HashMap<StreamEndpointId, Permit>>>,
    reserved_streams: Arc<Mutex<HashSet<StreamEndpointId>>>,
    inner: Weak<Mutex<PeerInner>>,
    peer_id: PeerId,
    sender: mpsc::UnboundedSender<BoxFuture<'static, StreamPermit>>,
}

struct StreamPermit {
    local_id: StreamEndpointId,
    open_streams: Arc<Mutex<HashMap<StreamEndpointId, Permit>>>,
}

impl StreamPermit {
    fn local_id(&self) -> &StreamEndpointId {
        &self.local_id
    }
}

impl Drop for StreamPermit {
    fn drop(&mut self) {
        let _ = self.open_streams.lock().remove(&self.local_id);
    }
}

impl StreamPermits {
    fn new(
        inner: Weak<Mutex<PeerInner>>,
        peer_id: PeerId,
        permits: Permits,
    ) -> (Self, mpsc::UnboundedReceiver<BoxFuture<'static, StreamPermit>>) {
        let (sender, reservations_receiver) = futures::channel::mpsc::unbounded();
        (
            Self {
                inner,
                permits,
                peer_id,
                sender,
                open_streams: Default::default(),
                reserved_streams: Default::default(),
            },
            reservations_receiver,
        )
    }

    fn label_for(&self, local_id: &StreamEndpointId) -> String {
        format!("{} {}", self.peer_id, local_id)
    }

    /// Get a permit to stream on the stream with id `local_id`.
    /// Returns Some() if there is a permit available.
    fn get(&self, local_id: StreamEndpointId) -> Option<StreamPermit> {
        let revoke_fn = self.make_revocation_fn(&local_id);
        let Some(permit) = self.permits.get_revokable(revoke_fn) else {
            info!("No permits available: {:?}", self.permits);
            return None;
        };
        permit.relabel(self.label_for(&local_id));
        if let Some(_) = self.open_streams.lock().insert(local_id.clone(), permit) {
            warn!(id = %self.peer_id, "Started stream {local_id:?} twice, dropping previous permit");
        }
        Some(StreamPermit { local_id, open_streams: self.open_streams.clone() })
    }

    /// Get a reservation that will resolve to a StreamPermit to start a stream with the id
    /// `local_id`
    fn setup_reservation_for(&self, local_id: StreamEndpointId) {
        if !self.reserved_streams.lock().insert(local_id.clone()) {
            // Already reserved.
            return;
        }
        let restart_stream_available_fut = {
            let self_revoke_fn = Self::make_revocation_fn(&self, &local_id);
            let reservation = self.permits.reserve_revokable(self_revoke_fn);
            let open_streams = self.open_streams.clone();
            let reserved_streams = self.reserved_streams.clone();
            let label = self.label_for(&local_id);
            let local_id = local_id.clone();
            async move {
                let permit = reservation.await;
                permit.relabel(label);
                if open_streams.lock().insert(local_id.clone(), permit).is_some() {
                    warn!("Reservation replaces acquired permit for {}", local_id.clone());
                }
                if !reserved_streams.lock().remove(&local_id) {
                    warn!(%local_id, "Unrecorded reservation resolved");
                }
                StreamPermit { local_id, open_streams }
            }
        };
        if let Err(e) = self.sender.unbounded_send(restart_stream_available_fut.boxed()) {
            warn!(id = %self.peer_id, %local_id, ?e, "Couldn't queue reservation to finish");
        }
    }

    /// Revokes a permit that was previously delivered, suspending the local stream and signaling
    /// the peer.
    /// The permit must have been previously received through StreamPermits::get or a have been
    /// restarted after being revoked, otherwise will panic.
    fn revocation_fn(self, local_id: StreamEndpointId) -> Permit {
        if let Ok(peer) = PeerInner::upgrade(self.inner.clone()) {
            {
                let mut lock = peer.lock();
                match lock.suspend_local_stream(&local_id) {
                    Ok(remote_id) => drop(lock.peer.suspend(&[remote_id.clone()])),
                    Err(e) => error!("Couldn't stop local stream {local_id:?}: {e:?}"),
                }
            }
            self.setup_reservation_for(local_id.clone());
        }
        self.open_streams.lock().remove(&local_id).expect("permit revoked but don't have it")
    }

    fn make_revocation_fn(&self, local_id: &StreamEndpointId) -> impl FnOnce() -> Permit {
        let local_id = local_id.clone();
        let cloned = self.clone();
        move || cloned.revocation_fn(local_id)
    }
}

impl Peer {
    /// Make a new Peer which is connected to the peer `id` using the AVDTP `peer`.
    /// The `streams` are the local endpoints available to the peer.
    /// `profile` will be used to initiate connections for Media Transport.
    /// The `permits`, if provided, will acquire a permit before starting streams on this peer.
    /// If `metrics` is included, metrics for codec availability will be reported.
    /// This also starts a task on the executor to handle incoming events from the peer.
    pub fn create(
        id: PeerId,
        peer: avdtp::Peer,
        streams: Streams,
        permits: Option<Permits>,
        profile: ProfileProxy,
        metrics: bt_metrics::MetricsLogger,
    ) -> Self {
        let inner = Arc::new(Mutex::new(PeerInner::new(peer, id, streams, metrics.clone())));
        let reservations_receiver = if let Some(permits) = permits {
            let (stream_permits, receiver) =
                StreamPermits::new(Arc::downgrade(&inner), id, permits);
            inner.lock().permits = Some(stream_permits);
            receiver
        } else {
            let (_, receiver) = mpsc::unbounded();
            receiver
        };
        let res = Self {
            id,
            inner,
            profile,
            descriptor: Mutex::new(None),
            closed_wakers: Arc::new(Mutex::new(Some(Vec::new()))),
            metrics,
            start_stream_task: Mutex::new(None),
        };
        res.start_requests_task(reservations_receiver);
        res
    }

    pub fn set_descriptor(&self, descriptor: ProfileDescriptor) -> Option<ProfileDescriptor> {
        self.descriptor.lock().replace(descriptor)
    }

    /// How long to wait after a non-local establishment of a stream to start the stream.
    /// Chosen to produce reasonably quick startup while allowing for peer start.
    const STREAM_DWELL: zx::Duration = zx::Duration::from_millis(500);

    /// Receive a channel from the peer that was initiated remotely.
    /// This function should be called whenever the peer associated with this opens an L2CAP channel.
    /// If this completes opening a stream, streams that are suspended will be scheduled to start.
    pub fn receive_channel(&self, channel: Channel) -> avdtp::Result<()> {
        let mut lock = self.inner.lock();
        if lock.receive_channel(channel)? {
            let weak = Arc::downgrade(&self.inner);
            let mut task_lock = self.start_stream_task.lock();
            *task_lock = Some(fasync::Task::local(async move {
                trace!("Dwelling to start remotely-opened stream..");
                fasync::Timer::new(Self::STREAM_DWELL.after_now()).await;
                PeerInner::start_opened(weak).await
            }));
        }
        Ok(())
    }

    /// Return a handle to the AVDTP peer, to use as initiator of commands.
    pub fn avdtp(&self) -> avdtp::Peer {
        let lock = self.inner.lock();
        lock.peer.clone()
    }

    /// Returns the stream endpoints discovered by this peer.
    pub fn remote_endpoints(&self) -> Option<Vec<avdtp::StreamEndpoint>> {
        self.inner.lock().remote_endpoints()
    }

    /// Perform Discovery and Collect Capabilities to enumerate the endpoints and capabilities of
    /// the connected peer.
    /// Returns a future which performs the work and resolves to a vector of peer stream endpoints.
    pub fn collect_capabilities(
        &self,
    ) -> impl Future<Output = avdtp::Result<Vec<avdtp::StreamEndpoint>>> {
        let avdtp = self.avdtp();
        let get_all = self.descriptor.lock().map_or(false, a2dp_version_check);
        let inner = self.inner.clone();
        let metrics = self.metrics.clone();
        let peer_id = self.id;
        async move {
            if let Some(caps) = inner.lock().remote_endpoints() {
                return Ok(caps);
            }
            trace!("Discovering peer streams..");
            let infos = avdtp.discover().await?;
            trace!("Discovered {} streams", infos.len());
            let mut remote_streams = Vec::new();
            for info in infos {
                let capabilities = if get_all {
                    avdtp.get_all_capabilities(info.id()).await
                } else {
                    avdtp.get_capabilities(info.id()).await
                };
                match capabilities {
                    Ok(capabilities) => {
                        trace!("Stream {:?}", info);
                        for cap in &capabilities {
                            trace!("  - {:?}", cap);
                        }
                        remote_streams.push(avdtp::StreamEndpoint::from_info(&info, capabilities));
                    }
                    Err(e) => {
                        info!(%peer_id, "Stream {} capabilities failed: {:?}, skipping", info.id(), e);
                    }
                };
            }
            inner.lock().set_remote_endpoints(&remote_streams);
            Self::record_cobalt_metrics(metrics, &remote_streams);
            Ok(remote_streams)
        }
    }

    fn record_cobalt_metrics(metrics: bt_metrics::MetricsLogger, endpoints: &[StreamEndpoint]) {
        let codec_metrics: HashSet<_> = endpoints
            .iter()
            .filter_map(|endpoint| {
                endpoint.codec_type().map(|t| codectype_to_availability_metric(t) as u32)
            })
            .collect();
        metrics
            .log_occurrences(bt_metrics::A2DP_CODEC_AVAILABILITY_MIGRATED_METRIC_ID, codec_metrics);

        let cap_metrics: HashSet<_> = endpoints
            .iter()
            .flat_map(|endpoint| {
                endpoint
                    .capabilities()
                    .iter()
                    .filter_map(|t| capability_to_metric(t))
                    .chain(std::iter::once(
                        bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::Basic,
                    ))
                    .map(|t| t as u32)
            })
            .collect();
        metrics.log_occurrences(bt_metrics::A2DP_REMOTE_PEER_CAPABILITIES_METRIC_ID, cap_metrics);
    }

    fn transport_channel_params() -> L2capParameters {
        L2capParameters {
            psm: Some(PSM_AVDTP),
            parameters: Some(ChannelParameters {
                max_rx_sdu_size: Some(65535),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Open and start a media transport stream, connecting a compatible local stream to the remote
    /// stream `remote_id`, configuring it with the `capabilities` provided.
    /// Returns a future which should be awaited on.
    /// The future returns Ok(()) if successfully started, and an appropriate error otherwise.
    pub fn stream_start(
        &self,
        remote_id: StreamEndpointId,
        capabilities: Vec<ServiceCapability>,
    ) -> impl Future<Output = avdtp::Result<()>> {
        let peer = Arc::downgrade(&self.inner);
        let peer_id = self.id.clone();
        let avdtp = self.avdtp();
        let profile = self.profile.clone();

        async move {
            let codec_params =
                capabilities.iter().find(|x| x.is_codec()).ok_or(avdtp::Error::InvalidState)?;
            let (local_id, local_capabilities) = {
                let peer = PeerInner::upgrade(peer.clone())?;
                let lock = peer.lock();
                let stream = lock.find_compatible_local(codec_params, &remote_id)?;
                (stream.local_id().clone(), stream.capabilities().clone())
            };

            let local_categories: HashSet<_> =
                local_capabilities.iter().map(ServiceCapability::category).collect();
            // Filter things out if they don't have a match in the local capabilities.
            let shared_capabilities: Vec<ServiceCapability> = capabilities
                .into_iter()
                .filter(|cap| local_categories.contains(&cap.category()))
                .collect();

            trace!("Starting stream {local_id} to remote {remote_id} with {shared_capabilities:?}");

            avdtp.set_configuration(&remote_id, &local_id, &shared_capabilities).await?;
            {
                let strong = PeerInner::upgrade(peer.clone())?;
                strong.lock().set_opening(&local_id, &remote_id, shared_capabilities)?;
            }
            avdtp.open(&remote_id).await?;

            info!(%peer_id, "Connecting transport channel");
            let channel = profile
                .connect(
                    &mut peer_id.into(),
                    &mut ConnectParameters::L2cap(Self::transport_channel_params()),
                )
                .await
                .context("FIDL error: {}")?
                .or(Err(avdtp::Error::PeerDisconnected))?;
            trace!(%peer_id, "Connected transport channel, converting to local Channel");
            let channel = match channel.try_into() {
                Err(e) => {
                    warn!(%peer_id, ?e, "Couldn't connect media transport: no channel");
                    return Err(avdtp::Error::PeerDisconnected);
                }
                Ok(c) => c,
            };

            trace!(%peer_id, "Connected transport channel, passing to Peer..");

            {
                let strong = PeerInner::upgrade(peer.clone())?;
                let _ = strong.lock().receive_channel(channel)?;
            }
            // Start streams immediately if the channel is locally initiated.
            PeerInner::start_opened(peer).await
        }
    }

    /// Query whether any streams are currently started or scheduled to start.
    pub fn streaming_active(&self) -> bool {
        self.inner.lock().is_streaming() || self.will_start_streaming()
    }

    /// Polls the task scheduled to start streaming, returning true if the task is still scheduled
    /// to start streaming.
    fn will_start_streaming(&self) -> bool {
        let mut task_lock = self.start_stream_task.lock();
        if task_lock.is_none() {
            return false;
        }
        // This is the only thing that can poll the start task, so it is okay to ignore the wakeup.
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        if let Poll::Pending = task_lock.as_mut().unwrap().poll_unpin(&mut cx) {
            return true;
        }
        // Reset the task to None so that we don't try to re-poll it.
        let _ = task_lock.take();
        false
    }

    /// Suspend a media transport stream `local_id`.
    /// It's possible that the stream is not active - a suspend will be attempted, but an
    /// error from the command will be returned.
    /// Returns the result of the suspend command.
    pub fn stream_suspend(
        &self,
        local_id: StreamEndpointId,
    ) -> impl Future<Output = avdtp::Result<()>> {
        let peer = Arc::downgrade(&self.inner);
        PeerInner::suspend(peer, local_id)
    }

    /// Start an asynchronous task to handle any requests from the AVDTP peer.
    /// This task completes when the remote end closes the signaling connection.
    fn start_requests_task(
        &self,
        mut reservations_receiver: mpsc::UnboundedReceiver<BoxFuture<'static, StreamPermit>>,
    ) {
        let lock = self.inner.lock();
        let mut request_stream = lock.peer.take_request_stream();
        let id = self.id.clone();
        let peer = Arc::downgrade(&self.inner);
        let mut stream_reservations = FuturesUnordered::new();
        let disconnect_wakers = Arc::downgrade(&self.closed_wakers);
        fuchsia_async::Task::local(async move {
            loop {
                select! {
                    request = request_stream.next().fuse() => {
                        match request {
                            None => break,
                            Some(Err(e)) => info!(peer_id = %id, ?e, "Request stream error"),
                            Some(Ok(request)) => match peer.upgrade() {
                                None => return,
                                Some(p) => {
                                    let mut lock = p.lock();
                                    if let Err(e) = lock.handle_request(request).await {
                                        warn!(peer_id = %id, ?e, "Error handling request");
                                    }
                                }
                            },
                        }
                    },
                    reservation_fut = reservations_receiver.select_next_some() => {
                        stream_reservations.push(reservation_fut)
                    },
                    permit = stream_reservations.select_next_some() => {
                        if let Err(e) = PeerInner::start_permit(peer.clone(), permit).await {
                            warn!(peer_id = %id, ?e, "Couldn't start stream after unpause");
                        }
                    }
                    complete => break,
                }
            }
            info!(peer_id = %id, "disconnected");
            if let Some(wakers) = disconnect_wakers.upgrade() {
                for waker in wakers.lock().take().unwrap_or_else(Vec::new) {
                    waker.wake();
                }
            }
        })
        .detach();
    }

    /// Returns a future that will complete when the peer disconnects.
    pub fn closed(&self) -> ClosedPeer {
        ClosedPeer { inner: Arc::downgrade(&self.closed_wakers) }
    }
}

/// Future which completes when the A2DP peer has closed the control conection.
/// See `Peer::closed`
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct ClosedPeer {
    inner: Weak<Mutex<Option<Vec<Waker>>>>,
}

impl Future for ClosedPeer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.inner.upgrade() {
            None => Poll::Ready(()),
            Some(inner) => match inner.lock().as_mut() {
                None => Poll::Ready(()),
                Some(wakers) => {
                    wakers.push(cx.waker().clone());
                    Poll::Pending
                }
            },
        }
    }
}

/// Determines if Peer profile version is newer (>= 1.3) or older (< 1.3)
fn a2dp_version_check(profile: ProfileDescriptor) -> bool {
    (profile.major_version == 1 && profile.minor_version >= 3) || profile.major_version > 1
}

/// Peer handles the communication with the AVDTP layer, and provides responses as appropriate
/// based on the current state of local streams available.
/// Each peer has its own set of local stream endpoints, and tracks a set of remote peer endpoints.
struct PeerInner {
    /// AVDTP peer communicating to this.
    peer: avdtp::Peer,
    /// The PeerId that this peer is representing
    peer_id: PeerId,
    /// Some(local_id) if an endpoint has been configured but hasn't finished opening.
    /// Per AVDTP Sec 6.11 only up to one stream can be in this state.
    opening: Option<StreamEndpointId>,
    /// The local stream endpoint collection
    local: Streams,
    /// The permits that are available for this peer.
    permits: Option<StreamPermits>,
    /// Tasks watching for the end of a started stream. Key is the local stream id.
    started: HashMap<StreamEndpointId, WatchedStream>,
    /// The inspect node for this peer
    inspect: fuchsia_inspect::Node,
    /// The set of discovered remote endpoints. None until set.
    remote_endpoints: Option<Vec<StreamEndpoint>>,
    /// The inspect node representing the remote endpoints.
    remote_inspect: fuchsia_inspect::Node,
    /// Cobalt logger used to report peer metrics.
    metrics: bt_metrics::MetricsLogger,
}

impl Inspect for &mut PeerInner {
    // Set up the StreamEndpoint to update the state
    // The MediaTask node will be created when the media task is started.
    fn iattach(self, parent: &inspect::Node, name: impl AsRef<str>) -> Result<(), AttachError> {
        self.inspect = parent.create_child(name.as_ref());
        self.inspect.record_string("id", self.peer_id.to_string());
        self.local.iattach(&self.inspect, "local_streams")
    }
}

impl PeerInner {
    pub fn new(
        peer: avdtp::Peer,
        peer_id: PeerId,
        local: Streams,
        metrics: bt_metrics::MetricsLogger,
    ) -> Self {
        Self {
            peer,
            peer_id,
            opening: None,
            local,
            permits: None,
            started: HashMap::new(),
            inspect: Default::default(),
            remote_endpoints: None,
            remote_inspect: Default::default(),
            metrics,
        }
    }

    /// Returns an endpoint from the local set or a BadAcpSeid error if it doesn't exist.
    fn get_mut(&mut self, local_id: &StreamEndpointId) -> Result<&mut Stream, avdtp::ErrorCode> {
        self.local.get_mut(&local_id).ok_or(avdtp::ErrorCode::BadAcpSeid)
    }

    fn set_remote_endpoints(&mut self, endpoints: &[StreamEndpoint]) {
        self.remote_inspect = self.inspect.create_child("remote_endpoints");
        for endpoint in endpoints {
            self.remote_inspect.record_child(inspect::unique_name("remote_"), |node| {
                node.record_string("endpoint_id", endpoint.local_id().debug());
                node.record_string("capabilities", endpoint.capabilities().debug());
                node.record_string("type", endpoint.endpoint_type().debug());
            });
        }
        self.remote_endpoints = Some(endpoints.iter().map(StreamEndpoint::as_new).collect());
    }

    /// If the remote endpoints have been set, returns a copy of the endpoints.
    fn remote_endpoints(&self) -> Option<Vec<StreamEndpoint>> {
        self.remote_endpoints.as_ref().map(|v| v.iter().map(StreamEndpoint::as_new).collect())
    }

    /// If the remote endpoint with endpoint `id` exists, return a copy of the endpoint.
    fn remote_endpoint(&self, id: &StreamEndpointId) -> Option<StreamEndpoint> {
        self.remote_endpoints
            .as_ref()
            .and_then(|v| v.iter().find(|v| v.local_id() == id).map(StreamEndpoint::as_new))
    }

    /// Returns true if there is at least one stream in the started state for this peer.
    fn is_streaming(&self) -> bool {
        self.local.streaming().next().is_some() || self.opening.is_some()
    }

    fn set_opening(
        &mut self,
        local_id: &StreamEndpointId,
        remote_id: &StreamEndpointId,
        capabilities: Vec<ServiceCapability>,
    ) -> avdtp::Result<()> {
        if self.opening.is_some() {
            return Err(avdtp::Error::InvalidState);
        }
        let peer_id = self.peer_id;
        let stream = self.get_mut(&local_id).map_err(|e| avdtp::Error::RequestInvalid(e))?;
        stream
            .configure(&peer_id, &remote_id, capabilities)
            .map_err(|(cat, c)| avdtp::Error::RequestInvalidExtra(c, (&cat).into()))?;
        stream.endpoint_mut().establish().or(Err(avdtp::Error::InvalidState))?;
        self.opening = Some(local_id.clone());
        Ok(())
    }

    fn upgrade(weak: Weak<Mutex<Self>>) -> avdtp::Result<Arc<Mutex<Self>>> {
        weak.upgrade().ok_or(avdtp::Error::PeerDisconnected)
    }

    /// Start the stream that is opening, completing the opened procedure.
    async fn start_opened(weak: Weak<Mutex<Self>>) -> avdtp::Result<()> {
        let (avdtp, stream_pairs) = {
            let peer = Self::upgrade(weak.clone())?;
            let peer = peer.lock();
            let stream_pairs: Vec<(StreamEndpointId, StreamEndpointId)> = peer
                .local
                .open()
                .filter_map(|stream| {
                    let endpoint = stream.endpoint();
                    endpoint.remote_id().cloned().map(|id| (endpoint.local_id().clone(), id))
                })
                .collect();
            (peer.peer.clone(), stream_pairs)
        };
        for (local_id, remote_id) in stream_pairs {
            let permit_result =
                Self::upgrade(weak.clone())?.lock().get_permit_or_reserve(&local_id);
            if let Ok(permit) = permit_result {
                Self::initiated_start(avdtp.clone(), weak.clone(), permit, &local_id, &remote_id)
                    .await?;
            }
        }
        Ok(())
    }

    async fn start_permit(weak: Weak<Mutex<Self>>, permit: StreamPermit) -> avdtp::Result<()> {
        let local_id = permit.local_id().clone();
        let (avdtp, remote_id) = {
            let peer = Self::upgrade(weak.clone())?;
            let mut peer = peer.lock();
            let remote_id = peer
                .get_mut(&local_id)
                .map_err(|e| avdtp::Error::RequestInvalid(e))?
                .endpoint()
                .remote_id()
                .ok_or(avdtp::Error::InvalidState)?
                .clone();
            (peer.peer.clone(), remote_id)
        };
        Self::initiated_start(avdtp, weak, Some(permit), &local_id, &remote_id).await
    }

    /// Start a stream for a local reason.  Requires a Permit to start streaming for the local stream.
    async fn initiated_start(
        avdtp: avdtp::Peer,
        weak: Weak<Mutex<Self>>,
        permit: Option<StreamPermit>,
        local_id: &StreamEndpointId,
        remote_id: &StreamEndpointId,
    ) -> avdtp::Result<()> {
        let to_start = &[remote_id.clone()];
        avdtp.start(to_start).await?;
        let peer = Self::upgrade(weak.clone())?;
        let (peer_id, start_result) = {
            let mut peer = peer.lock();
            (peer.peer_id, peer.start_local_stream(permit, &local_id))
        };
        if let Err(e) = start_result {
            warn!(
                "{}: Failed media start of {}: {:?}, suspend remote {}",
                peer_id, local_id, e, remote_id
            );
            avdtp.suspend(to_start).await?;
        }
        Ok(())
    }

    /// Suspend a stream locally, returning a future to get the result from the peer.
    fn suspend(
        weak: Weak<Mutex<Self>>,
        local_id: StreamEndpointId,
    ) -> impl Future<Output = avdtp::Result<()>> {
        let res = (move || {
            let peer = Self::upgrade(weak.clone())?;
            let mut peer = peer.lock();
            Ok((peer.peer.clone(), peer.suspend_local_stream(&local_id)?))
        })();
        let (avdtp, remote_id) = match res {
            Err(e) => return futures::future::err(e).left_future(),
            Ok(r) => r,
        };
        let to_suspend = &[remote_id];
        avdtp.suspend(to_suspend).right_future()
    }

    /// Finds a stream in the local stream set which is compatible with the remote_id given the codec config.
    /// Returns the local stream ID if found, or OutOfRange if one could not be found.
    pub fn find_compatible_local(
        &self,
        codec_params: &ServiceCapability,
        remote_id: &StreamEndpointId,
    ) -> avdtp::Result<&StreamEndpoint> {
        let config = codec_params.try_into()?;
        let our_direction = self.remote_endpoint(remote_id).map(|e| e.endpoint_type().opposite());
        self.local
            .compatible(config)
            .find(|s| our_direction.map_or(true, |d| &d == s.endpoint().endpoint_type()))
            .map(|s| s.endpoint())
            .ok_or(avdtp::Error::OutOfRange)
    }

    /// Attempts to acquire a permit for streaming, if the permits are set.
    /// Returns Ok if is is okay to stream, and Err if the permit was not available and a
    /// reservation was made.
    fn get_permit_or_reserve(
        &self,
        local_id: &StreamEndpointId,
    ) -> Result<Option<StreamPermit>, ()> {
        let Some(permits) = self.permits.as_ref() else {
            return Ok(None);
        };
        if let Some(permit) = permits.get(local_id.clone()) {
            return Ok(Some(permit));
        }
        info!(peer_id = %self.peer_id, %local_id, "No permit to start stream, adding a reservation");
        permits.setup_reservation_for(local_id.clone());
        Err(())
    }

    /// Starts the stream which is in the local Streams with `local_id`.
    /// Requires a permit to stream.
    fn start_local_stream(
        &mut self,
        permit: Option<StreamPermit>,
        local_id: &StreamEndpointId,
    ) -> avdtp::Result<()> {
        let peer_id = self.peer_id;
        let stream = self.get_mut(&local_id).map_err(|e| avdtp::Error::RequestInvalid(e))?;
        info!(%peer_id, ?stream, "Starting");
        let stream_finished = stream.start().map_err(|c| avdtp::Error::RequestInvalid(c))?;
        // TODO(fxbug.dev/68238): if streaming stops unexpectedly, send a suspend to match to peer
        let watched_stream = WatchedStream::new(permit, stream_finished);
        if self.started.insert(local_id.clone(), watched_stream).is_some() {
            warn!(%peer_id, %local_id, "Stream that was already started");
        }
        Ok(())
    }

    /// Suspend a stream on the local side. Returns the remote StreamEndpointId if the stream was suspended,
    /// or a RequestInvalid error eith the error code otherwise.
    fn suspend_local_stream(
        &mut self,
        local_id: &StreamEndpointId,
    ) -> avdtp::Result<StreamEndpointId> {
        let peer_id = self.peer_id;
        let stream = self.get_mut(&local_id).map_err(|e| avdtp::Error::RequestInvalid(e))?;
        let remote_id = stream.endpoint().remote_id().cloned().ok_or(avdtp::Error::InvalidState)?;
        info!(%peer_id, "Suspend stream local {local_id} <-> {remote_id} remote");
        stream.suspend().map_err(|c| avdtp::Error::RequestInvalid(c))?;
        let _ = self.started.remove(local_id);
        Ok(remote_id)
    }

    /// Provide a new established L2CAP channel to this remote peer.
    /// This function should be called whenever the remote associated with this peer opens an
    /// L2CAP channel after the first.
    /// Returns true if this channel completed the opening sequence.
    fn receive_channel(&mut self, channel: Channel) -> avdtp::Result<bool> {
        let stream_id = self.opening.as_ref().cloned().ok_or(avdtp::Error::InvalidState)?;
        let stream = self.get_mut(&stream_id).map_err(|e| avdtp::Error::RequestInvalid(e))?;
        let done = !stream.endpoint_mut().receive_channel(channel)?;
        if done {
            self.opening = None;
        }
        info!(peer_id = %self.peer_id, %stream_id, "Transport connected");
        Ok(done)
    }

    /// Handle a single request event from the avdtp peer.
    async fn handle_request(&mut self, request: avdtp::Request) -> avdtp::Result<()> {
        use avdtp::ErrorCode;
        use avdtp::Request::*;
        trace!("Handling {request:?} from peer..");
        match request {
            Discover { responder } => responder.send(&self.local.information()),
            GetCapabilities { responder, stream_id }
            | GetAllCapabilities { responder, stream_id } => match self.local.get(&stream_id) {
                None => responder.reject(ErrorCode::BadAcpSeid),
                Some(stream) => responder.send(stream.endpoint().capabilities()),
            },
            Open { responder, stream_id } => {
                if self.opening.is_none() {
                    return responder.reject(ErrorCode::BadState);
                }
                let Ok(stream) = self.get_mut(&stream_id) else {
                    return responder.reject(ErrorCode::BadAcpSeid);
                };
                match stream.endpoint_mut().establish() {
                    Ok(()) => responder.send(),
                    Err(_) => responder.reject(ErrorCode::BadState),
                }
            }
            Close { responder, stream_id } => {
                let peer = self.peer.clone();
                let Ok(stream) = self.get_mut(&stream_id) else {
                    return responder.reject(ErrorCode::BadAcpSeid);
                };
                stream.release(responder, &peer).await
            }
            SetConfiguration { responder, local_stream_id, remote_stream_id, capabilities } => {
                if self.opening.is_some() {
                    return responder.reject(ServiceCategory::None, ErrorCode::BadState);
                }
                let peer_id = self.peer_id;
                let Ok(stream) = self.get_mut(&local_stream_id) else {
                    return responder.reject(ServiceCategory::None, ErrorCode::BadAcpSeid);
                };
                match stream.configure(&peer_id, &remote_stream_id, capabilities) {
                    Ok(_) => {
                        self.opening = Some(local_stream_id);
                        responder.send()
                    }
                    Err((category, code)) => responder.reject(category, code),
                }
            }
            GetConfiguration { stream_id, responder } => {
                let Some(stream) = self.local.get(&stream_id) else {
                     return responder.reject(ErrorCode::BadAcpSeid);
                };
                match stream.endpoint().get_configuration() {
                    Some(c) => responder.send(&c),
                    // Only happens when the stream is in the wrong state
                    None => responder.reject(ErrorCode::BadState),
                }
            }
            Reconfigure { responder, local_stream_id, capabilities } => {
                let Ok(stream) = self.get_mut(&local_stream_id) else {
                    return responder.reject(ServiceCategory::None, ErrorCode::BadAcpSeid);
                };
                match stream.reconfigure(capabilities) {
                    Ok(_) => responder.send(),
                    Err((cat, code)) => responder.reject(cat, code),
                }
            }
            Start { responder, stream_ids } => {
                let mut immediate_suspend = Vec::new();
                // Fail on the first failed endpoint, as per the AVDTP spec 8.13 Note 5
                let result = stream_ids.into_iter().try_for_each(|seid| {
                    let Some(stream) = self.local.get(&seid) else {
                        return Err((seid, ErrorCode::BadAcpSeid));
                    };
                    let Some(remote_id) = stream.endpoint().remote_id().cloned() else {
                        return Err((seid, ErrorCode::BadState));
                    };
                    let Ok(permit) = self.get_permit_or_reserve(&seid) else {
                            // Happens when we cannot start because of permits.
                            // Accept this one, then queue up for suspend.
                            // We are already reserved for a permit.
                            immediate_suspend.push(remote_id);
                            return Ok(());
                    };
                    match self.start_local_stream(permit, &seid) {
                        Ok(()) => Ok(()),
                        Err(avdtp::Error::RequestInvalid(code)) => Err((seid, code)),
                        Err(_) => Err((seid, ErrorCode::BadState)),
                    }
                });
                let response_result = match result {
                    Ok(()) => responder.send(),
                    Err((seid, code)) => responder.reject(&seid, code),
                };
                if !immediate_suspend.is_empty() {
                    return self.peer.suspend(immediate_suspend.as_slice()).await;
                }
                response_result
            }
            Suspend { responder, stream_ids } => {
                for seid in stream_ids {
                    match self.suspend_local_stream(&seid) {
                        Ok(_remote_id) => {}
                        Err(avdtp::Error::RequestInvalid(code)) => {
                            return responder.reject(&seid, code)
                        }
                        Err(_e) => return responder.reject(&seid, ErrorCode::BadState),
                    }
                }
                responder.send()
            }
            Abort { responder, stream_id } => {
                let Ok(stream) = self.get_mut(&stream_id) else {
                    // No response is sent on an invalid ID for an Abort
                    return Ok(());
                };
                stream.abort(None).await;
                self.opening = self.opening.take().filter(|local_id| local_id != &stream_id);
                responder.send()
            }
            DelayReport { responder, delay, stream_id } => {
                // Delay is in 1/10 ms
                let delay_ns = delay as u64 * 100000;
                // Record delay to cobalt.
                self.metrics.log_integer(
                    bt_metrics::AVDTP_DELAY_REPORT_IN_NANOSECONDS_METRIC_ID,
                    delay_ns.try_into().unwrap_or(-1),
                    vec![],
                );
                // Report should only come after a stream is configured
                let Some(stream) = self.local.get_mut(&stream_id) else {
                    return responder.reject(avdtp::ErrorCode::BadAcpSeid);
                };
                let delay_str = format!("delay {}.{} ms", delay / 10, delay % 10);
                let peer = self.peer_id;
                match stream.set_delay(std::time::Duration::from_nanos(delay_ns)) {
                    Ok(()) => info!(%peer, %stream_id, "reported {delay_str}"),
                    Err(avdtp::ErrorCode::BadState) => {
                        info!(%peer, %stream_id, "bad state {delay_str}");
                        return responder.reject(avdtp::ErrorCode::BadState);
                    }
                    Err(e) => info!(%peer, %stream_id, ?e, "failed {delay_str}"),
                };
                // Can't really respond with an Error
                responder.send()
            }
        }
    }
}

/// A WatchedStream holds a task tracking a started stream and ensures actions are performed when
/// the stream media task finishes.
struct WatchedStream {
    _permit_task: fasync::Task<()>,
}

impl WatchedStream {
    fn new(
        permit: Option<StreamPermit>,
        finish_fut: BoxFuture<'static, Result<(), anyhow::Error>>,
    ) -> Self {
        let permit_task = fasync::Task::spawn(async move {
            let _ = finish_fut.await;
            drop(permit);
        });
        Self { _permit_task: permit_task }
    }
}

fn codectype_to_availability_metric(
    codec_type: &MediaCodecType,
) -> bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec {
    match codec_type {
        &MediaCodecType::AUDIO_SBC => {
            bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Sbc
        }
        &MediaCodecType::AUDIO_MPEG12 => {
            bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Mpeg12
        }
        &MediaCodecType::AUDIO_AAC => {
            bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Aac
        }
        &MediaCodecType::AUDIO_ATRAC => {
            bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Atrac
        }
        &MediaCodecType::AUDIO_NON_A2DP => {
            bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::VendorSpecific
        }
        _ => bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Unknown,
    }
}

fn capability_to_metric(
    cap: &ServiceCapability,
) -> Option<bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability> {
    match cap {
        ServiceCapability::DelayReporting => {
            Some(bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::DelayReport)
        }
        ServiceCapability::Reporting => {
            Some(bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::Reporting)
        }
        ServiceCapability::Recovery { .. } => {
            Some(bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::Recovery)
        }
        ServiceCapability::ContentProtection { .. } => {
            Some(bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::ContentProtection)
        }
        ServiceCapability::HeaderCompression { .. } => {
            Some(bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::HeaderCompression)
        }
        ServiceCapability::Multiplexing { .. } => {
            Some(bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::Multiplexing)
        }
        // We ignore capabilities that we don't care to track.
        other => {
            trace!("untracked remote peer capability: {:?}", other);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_utils::PollExt;
    use bt_metrics::respond_to_metrics_req_for_test;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_bluetooth::ErrorCode;
    use fidl_fuchsia_bluetooth_bredr::{
        ProfileMarker, ProfileRequest, ProfileRequestStream, ServiceClassProfileIdentifier,
    };
    use fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload};
    use futures::{future::Either, pin_mut};
    use std::convert::TryInto;
    use std::task::Poll;

    use crate::media_task::tests::{TestMediaTask, TestMediaTaskBuilder};
    use crate::media_types::*;
    use crate::stream::tests::make_sbc_endpoint;

    fn fake_metrics(
    ) -> (bt_metrics::MetricsLogger, fidl_fuchsia_metrics::MetricEventLoggerRequestStream) {
        let (c, s) = fidl::endpoints::create_proxy_and_stream::<
            fidl_fuchsia_metrics::MetricEventLoggerMarker,
        >()
        .expect("failed to create MetricsEventLogger proxy");
        (bt_metrics::MetricsLogger::from_proxy(c), s)
    }

    fn setup_avdtp_peer() -> (avdtp::Peer, Channel) {
        let (remote, signaling) = Channel::create();
        let peer = avdtp::Peer::new(signaling);
        (peer, remote)
    }

    fn build_test_streams() -> Streams {
        let mut streams = Streams::new();
        let source = Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Source),
            TestMediaTaskBuilder::new_delayable().builder(),
        );
        streams.insert(source);
        let sink = Stream::build(
            make_sbc_endpoint(2, avdtp::EndpointType::Sink),
            TestMediaTaskBuilder::new().builder(),
        );
        streams.insert(sink);
        streams
    }

    pub(crate) fn recv_remote(remote: &Channel) -> Result<Vec<u8>, zx::Status> {
        let waiting = remote.as_ref().outstanding_read_bytes();
        assert!(waiting.is_ok());
        let mut response: Vec<u8> = vec![0; waiting.unwrap()];
        let response_read = remote.as_ref().read(response.as_mut_slice())?;
        assert_eq!(response.len(), response_read);
        Ok(response)
    }

    /// Creates a Peer object, returning a channel connected ot the remote end, a
    /// ProfileRequestStream connected to the profile_proxy, and the Peer object.
    fn setup_peer_test(
        use_cobalt: bool,
    ) -> (
        Channel,
        ProfileRequestStream,
        Option<fidl_fuchsia_metrics::MetricEventLoggerRequestStream>,
        Peer,
    ) {
        let (avdtp, remote) = setup_avdtp_peer();
        let (metrics_logger, cobalt_receiver) = if use_cobalt {
            let (l, r) = fake_metrics();
            (l, Some(r))
        } else {
            (bt_metrics::MetricsLogger::default(), None)
        };
        let (profile_proxy, requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let peer = Peer::create(
            PeerId(1),
            avdtp,
            build_test_streams(),
            None,
            profile_proxy,
            metrics_logger,
        );

        (remote, requests, cobalt_receiver, peer)
    }

    fn expect_get_capabilities_and_respond(
        remote: &Channel,
        expected_seid: u8,
        response_capabilities: &[u8],
    ) {
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x02, received[1]); // 0x02 = Get Capabilities
        assert_eq!(expected_seid << 2, received[2]);

        let txlabel_raw = received[0] & 0xF0;

        // Expect a get capabilities and respond.
        #[rustfmt::skip]
        let mut get_capabilities_rsp = vec![
            txlabel_raw << 4 | 0x2, // TxLabel (same) + ResponseAccept (0x02)
            0x02 // GetCapabilities
        ];

        get_capabilities_rsp.extend_from_slice(response_capabilities);

        assert!(remote.as_ref().write(&get_capabilities_rsp).is_ok());
    }

    fn expect_get_all_capabilities_and_respond(
        remote: &Channel,
        expected_seid: u8,
        response_capabilities: &[u8],
    ) {
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x0C, received[1]); // 0x0C = Get All Capabilities
        assert_eq!(expected_seid << 2, received[2]);

        let txlabel_raw = received[0] & 0xF0;

        // Expect a get capabilities and respond.
        #[rustfmt::skip]
        let mut get_capabilities_rsp = vec![
            txlabel_raw << 4 | 0x2, // TxLabel (same) + ResponseAccept (0x02)
            0x0C // GetAllCapabilities
        ];

        get_capabilities_rsp.extend_from_slice(response_capabilities);

        assert!(remote.as_ref().write(&get_capabilities_rsp).is_ok());
    }

    #[fuchsia::test]
    fn disconnected() {
        let mut exec = fasync::TestExecutor::new();
        let (proxy, _stream) =
            create_proxy_and_stream::<ProfileMarker>().expect("Profile proxy should be created");
        let (remote, signaling) = Channel::create();

        let id = PeerId(1);

        let avdtp = avdtp::Peer::new(signaling);
        let peer = Peer::create(
            id,
            avdtp,
            Streams::new(),
            None,
            proxy,
            bt_metrics::MetricsLogger::default(),
        );

        let closed_fut = peer.closed();

        pin_mut!(closed_fut);

        assert!(exec.run_until_stalled(&mut closed_fut).is_pending());

        // Close the remote channel
        drop(remote);

        assert!(exec.run_until_stalled(&mut closed_fut).is_ready());
    }

    #[fuchsia::test]
    fn peer_collect_capabilities_success() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, _, cobalt_receiver, peer) = setup_peer_test(true);

        let p: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 1,
            minor_version: 2,
        };
        let _ = peer.set_descriptor(p);

        let collect_future = peer.collect_capabilities();
        pin_mut!(collect_future);

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a discover command.
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x01, received[1]); // 0x01 = Discover

        let txlabel_raw = received[0] & 0xF0;

        // Respond with a set of streams.
        let response: &[u8] = &[
            txlabel_raw << 4 | 0x0 << 2 | 0x2, // txlabel (same), Single (0b00), Response Accept (0b10)
            0x01,                              // Discover
            0x3E << 2 | 0x0 << 1,              // SEID (3E), Not In Use (0b0)
            0x00 << 4 | 0x1 << 3,              // Audio (0x00), Sink (0x01)
            0x01 << 2 | 0x1 << 1,              // SEID (1), In Use (0b1)
            0x00 << 4 | 0x1 << 3,              // Audio (0x00), Sink (0x01)
        ];
        assert!(remote.as_ref().write(response).is_ok());

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a get capabilities and respond.
        #[rustfmt::skip]
        let capabilities_rsp = &[
            // MediaTransport (Length of Service Capability = 0)
            0x01, 0x00,
            // Media Codec (LOSC = 2 + 4), Media Type Audio (0x00), Codec type (0x04), Codec specific 0xF09F9296
            0x07, 0x06, 0x00, 0x04, 0xF0, 0x9F, 0x92, 0x96
        ];
        expect_get_capabilities_and_respond(&remote, 0x3E, capabilities_rsp);

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a get capabilities and respond.
        #[rustfmt::skip]
        let capabilities_rsp = &[
            // MediaTransport (Length of Service Capability = 0)
            0x01, 0x00,
            // Media Codec (LOSC = 2 + 2), Media Type Audio (0x00), Codec type (0x00), Codec specific 0xC0DE
            0x07, 0x04, 0x00, 0x00, 0xC0, 0xDE
        ];
        expect_get_capabilities_and_respond(&remote, 0x01, capabilities_rsp);

        match exec.run_until_stalled(&mut collect_future) {
            Poll::Pending => panic!("collect capabilities should be complete"),
            Poll::Ready(Err(e)) => panic!("collect capabilities should have succeeded: {}", e),
            Poll::Ready(Ok(endpoints)) => {
                let first_seid: StreamEndpointId = 0x3E_u8.try_into().unwrap();
                let second_seid: StreamEndpointId = 0x01_u8.try_into().unwrap();
                for stream in endpoints {
                    if stream.local_id() == &first_seid {
                        let expected_caps = vec![
                            ServiceCapability::MediaTransport,
                            ServiceCapability::MediaCodec {
                                media_type: avdtp::MediaType::Audio,
                                codec_type: avdtp::MediaCodecType::new(0x04),
                                codec_extra: vec![0xF0, 0x9F, 0x92, 0x96],
                            },
                        ];
                        assert_eq!(&expected_caps, stream.capabilities());
                    } else if stream.local_id() == &second_seid {
                        let expected_codec_type = avdtp::MediaCodecType::new(0x00);
                        assert_eq!(Some(&expected_codec_type), stream.codec_type());
                    } else {
                        panic!("Unexpected endpoint in the streams collected");
                    }
                }
            }
        }

        // Collect reported cobalt logs.
        let mut recv = cobalt_receiver.expect("should have receiver");
        let mut log_events = Vec::new();
        while let Poll::Ready(Some(Ok(req))) = exec.run_until_stalled(&mut recv.next()) {
            log_events.push(respond_to_metrics_req_for_test(req));
        }

        // Should have sent two metric events for codec and one for capability.
        assert_eq!(3, log_events.len());
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_CODEC_AVAILABILITY_MIGRATED_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Sbc as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_CODEC_AVAILABILITY_MIGRATED_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Atrac as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_REMOTE_PEER_CAPABILITIES_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::Basic as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));

        // The second time, we don't expect to ask the peer again.
        let collect_future = peer.collect_capabilities();
        pin_mut!(collect_future);

        match exec.run_until_stalled(&mut collect_future) {
            Poll::Ready(Ok(endpoints)) => assert_eq!(2, endpoints.len()),
            x => panic!("Expected get remote capabilities to be done, got {:?}", x),
        };
    }

    #[fuchsia::test]
    fn peer_collect_all_capabilities_success() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, _, cobalt_receiver, peer) = setup_peer_test(true);
        let p: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 1,
            minor_version: 3,
        };
        let _ = peer.set_descriptor(p);

        let collect_future = peer.collect_capabilities();
        pin_mut!(collect_future);

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a discover command.
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x01, received[1]); // 0x01 = Discover

        let txlabel_raw = received[0] & 0xF0;

        // Respond with a set of streams.
        let response: &[u8] = &[
            txlabel_raw << 4 | 0x0 << 2 | 0x2, // txlabel (same), Single (0b00), Response Accept (0b10)
            0x01,                              // Discover
            0x3E << 2 | 0x0 << 1,              // SEID (3E), Not In Use (0b0)
            0x00 << 4 | 0x1 << 3,              // Audio (0x00), Sink (0x01)
            0x01 << 2 | 0x1 << 1,              // SEID (1), In Use (0b1)
            0x00 << 4 | 0x1 << 3,              // Audio (0x00), Sink (0x01)
        ];
        assert!(remote.as_ref().write(response).is_ok());

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a get all capabilities and respond.
        #[rustfmt::skip]
        let capabilities_rsp = &[
            // MediaTransport (Length of Service Capability = 0)
            0x01, 0x00,
            // Media Codec (LOSC = 2 + 4), Media Type Audio (0x00), Codec type (0x40), Codec specific 0xF09F9296
            0x07, 0x06, 0x00, 0x40, 0xF0, 0x9F, 0x92, 0x96,
            // Delay Reporting (LOSC = 0)
            0x08, 0x00
        ];
        expect_get_all_capabilities_and_respond(&remote, 0x3E, capabilities_rsp);

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a get all capabilities and respond.
        #[rustfmt::skip]
        let capabilities_rsp = &[
            // MediaTransport (Length of Service Capability = 0)
            0x01, 0x00,
            // Media Codec (LOSC = 2 + 2), Media Type Audio (0x00), Codec type (0x00), Codec specific 0xC0DE
            0x07, 0x04, 0x00, 0x00, 0xC0, 0xDE
        ];
        expect_get_all_capabilities_and_respond(&remote, 0x01, capabilities_rsp);

        match exec.run_until_stalled(&mut collect_future) {
            Poll::Pending => panic!("collect capabilities should be complete"),
            Poll::Ready(Err(e)) => panic!("collect capabilities should have succeeded: {}", e),
            Poll::Ready(Ok(endpoints)) => {
                let first_seid: StreamEndpointId = 0x3E_u8.try_into().unwrap();
                let second_seid: StreamEndpointId = 0x01_u8.try_into().unwrap();
                for stream in endpoints {
                    if stream.local_id() == &first_seid {
                        let expected_caps = vec![
                            ServiceCapability::MediaTransport,
                            ServiceCapability::MediaCodec {
                                media_type: avdtp::MediaType::Audio,
                                codec_type: avdtp::MediaCodecType::new(0x40),
                                codec_extra: vec![0xF0, 0x9F, 0x92, 0x96],
                            },
                            ServiceCapability::DelayReporting,
                        ];
                        assert_eq!(&expected_caps, stream.capabilities());
                    } else if stream.local_id() == &second_seid {
                        let expected_codec_type = avdtp::MediaCodecType::new(0x00);
                        assert_eq!(Some(&expected_codec_type), stream.codec_type());
                    } else {
                        panic!("Unexpected endpoint in the streams collected");
                    }
                }
            }
        }

        // Collect reported cobalt logs.
        let mut recv = cobalt_receiver.expect("should have receiver");
        let mut log_events = Vec::new();
        while let Poll::Ready(Some(Ok(req))) = exec.run_until_stalled(&mut recv.next()) {
            log_events.push(respond_to_metrics_req_for_test(req));
        }

        // Should have sent two metric events for codec and two for capability.
        assert_eq!(4, log_events.len());
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_CODEC_AVAILABILITY_MIGRATED_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Unknown as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_CODEC_AVAILABILITY_MIGRATED_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpCodecAvailabilityMigratedMetricDimensionCodec::Sbc as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_REMOTE_PEER_CAPABILITIES_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::Basic as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));
        assert!(log_events.contains(&MetricEvent {
            metric_id: bt_metrics::A2DP_REMOTE_PEER_CAPABILITIES_METRIC_ID,
            event_codes: vec![
                bt_metrics::A2dpRemotePeerCapabilitiesMetricDimensionCapability::DelayReport as u32
            ],
            payload: MetricEventPayload::Count(1),
        }));

        // The second time, we don't expect to ask the peer again.
        let collect_future = peer.collect_capabilities();
        pin_mut!(collect_future);

        match exec.run_until_stalled(&mut collect_future) {
            Poll::Ready(Ok(endpoints)) => assert_eq!(2, endpoints.len()),
            x => panic!("Expected get remote capabilities to be done, got {:?}", x),
        };
    }

    #[fuchsia::test]
    fn peer_collect_capabilities_discovery_fails() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, _, _, peer) = setup_peer_test(false);

        let collect_future = peer.collect_capabilities();
        pin_mut!(collect_future);

        // Shouldn't finish yet.
        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a discover command.
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x01, received[1]); // 0x01 = Discover

        let txlabel_raw = received[0] & 0xF0;

        // Respond with an eror.
        let response: &[u8] = &[
            txlabel_raw | 0x0 << 2 | 0x3, // txlabel (same), Single (0b00), Response Reject (0b11)
            0x01,                         // Discover
            0x31,                         // BAD_STATE
        ];
        assert!(remote.as_ref().write(response).is_ok());

        // Should be done with an error.
        // Should finish!
        match exec.run_until_stalled(&mut collect_future) {
            Poll::Pending => panic!("Should be ready after discovery failure"),
            Poll::Ready(Ok(x)) => panic!("Should be an error but returned {x:?}"),
            Poll::Ready(Err(avdtp::Error::RemoteRejected(e))) => {
                assert_eq!(Some(Ok(avdtp::ErrorCode::BadState)), e.error_code());
            }
            Poll::Ready(Err(e)) => panic!("Should have been a RemoteRejected was was {e:?}"),
        }
    }

    #[fuchsia::test]
    fn peer_collect_capabilities_get_capability_fails() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, _, _, peer) = setup_peer_test(true);

        let collect_future = peer.collect_capabilities();
        pin_mut!(collect_future);

        // Shouldn't finish yet.
        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a discover command.
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x01, received[1]); // 0x01 = Discover

        let txlabel_raw = received[0] & 0xF0;

        // Respond with a set of streams.
        let response: &[u8] = &[
            txlabel_raw << 4 | 0x0 << 2 | 0x2, // txlabel (same), Single (0b00), Response Accept (0b10)
            0x01,                              // Discover
            0x3E << 2 | 0x0 << 1,              // SEID (3E), Not In Use (0b0)
            0x00 << 4 | 0x1 << 3,              // Audio (0x00), Sink (0x01)
            0x01 << 2 | 0x1 << 1,              // SEID (1), In Use (0b1)
            0x00 << 4 | 0x1 << 3,              // Audio (0x00), Sink (0x01)
        ];
        assert!(remote.as_ref().write(response).is_ok());

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a get capabilities request
        let expected_seid = 0x3E;
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x02, received[1]); // 0x02 = Get Capabilities
        assert_eq!(expected_seid << 2, received[2]);

        let txlabel_raw = received[0] & 0xF0;

        let response: &[u8] = &[
            txlabel_raw | 0x0 << 2 | 0x3, // txlabel (same), Single (0b00), Response Reject (0b11)
            0x02,                         // Get Capabilities
            0x12,                         // BAD_ACP_SEID
        ];
        assert!(remote.as_ref().write(response).is_ok());

        assert!(exec.run_until_stalled(&mut collect_future).is_pending());

        // Expect a get capabilities request (skipped the last one)
        let expected_seid = 0x01;
        let received = recv_remote(&remote).unwrap();
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(0x02, received[1]); // 0x02 = Get Capabilities
        assert_eq!(expected_seid << 2, received[2]);

        let txlabel_raw = received[0] & 0xF0;

        let response: &[u8] = &[
            txlabel_raw | 0x0 << 2 | 0x3, // txlabel (same), Single (0b00), Response Reject (0b11)
            0x02,                         // Get Capabilities
            0x12,                         // BAD_ACP_SEID
        ];
        assert!(remote.as_ref().write(response).is_ok());

        // Should be done without an error, but with no streams.
        match exec.run_until_stalled(&mut collect_future) {
            Poll::Pending => panic!("Should be ready after discovery failure"),
            Poll::Ready(Err(e)) => panic!("Shouldn't be an error but returned {:?}", e),
            Poll::Ready(Ok(map)) => assert_eq!(0, map.len()),
        }
    }

    fn receive_simple_accept(remote: &Channel, signal_id: u8) {
        let received = recv_remote(&remote).expect("expected a packet");
        // Last half of header must be Single (0b00) and Command (0b00)
        assert_eq!(0x00, received[0] & 0xF);
        assert_eq!(signal_id, received[1]);

        let txlabel_raw = received[0] & 0xF0;

        let response: &[u8] = &[
            txlabel_raw | 0x0 << 2 | 0x2, // txlabel (same), Single (0b00), Response Accept (0b10)
            signal_id,
        ];
        assert!(remote.as_ref().write(response).is_ok());
    }

    #[fuchsia::test]
    fn peer_stream_start_success() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, mut profile_request_stream, _, peer) = setup_peer_test(false);

        let remote_seid = 2_u8.try_into().unwrap();

        let codec_params = ServiceCapability::MediaCodec {
            media_type: avdtp::MediaType::Audio,
            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
            codec_extra: vec![0x11, 0x45, 51, 51],
        };

        let start_future = peer.stream_start(remote_seid, vec![codec_params]);
        pin_mut!(start_future);

        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => {}
            x => panic!("Expected pending, but got {x:?}"),
        };

        receive_simple_accept(&remote, 0x03); // Set Configuration

        assert!(exec.run_until_stalled(&mut start_future).is_pending());

        receive_simple_accept(&remote, 0x06); // Open

        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => {}
            Poll::Ready(Err(e)) => panic!("Expected to be pending but error: {:?}", e),
            Poll::Ready(Ok(_)) => panic!("Expected to be pending but finished!"),
        };

        // Should connect the media channel after open.
        let (_, transport) = Channel::create();

        let request = exec.run_until_stalled(&mut profile_request_stream.next());
        match request {
            Poll::Ready(Some(Ok(ProfileRequest::Connect { peer_id, connection, responder }))) => {
                assert_eq!(PeerId(1), peer_id.into());
                assert_eq!(connection, ConnectParameters::L2cap(Peer::transport_channel_params()));
                let channel = transport.try_into().unwrap();
                responder.send(&mut Ok(channel)).expect("responder sends");
            }
            x => panic!("Should have sent a open l2cap request, but got {:?}", x),
        };

        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => {}
            Poll::Ready(Err(e)) => panic!("Expected to be pending but error: {:?}", e),
            Poll::Ready(Ok(_)) => panic!("Expected to be pending but finished!"),
        };

        receive_simple_accept(&remote, 0x07); // Start

        // Should return the media stream (which should be connected)
        // Should be done without an error, but with no streams.
        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => panic!("Should be ready after start succeeds"),
            Poll::Ready(Err(e)) => panic!("Shouldn't be an error but returned {:?}", e),
            // TODO: confirm the stream is usable
            Poll::Ready(Ok(_stream)) => {}
        }
    }

    #[fuchsia::test]
    fn peer_stream_start_picks_correct_direction() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, _, _, peer) = setup_peer_test(false);
        let remote = avdtp::Peer::new(remote);
        let mut remote_events = remote.take_request_stream();

        // Respond as if we have a single SBC Source Stream
        fn remote_handle_request(req: avdtp::Request) {
            let expected_stream_id: StreamEndpointId = 4_u8.try_into().unwrap();
            let res = match req {
                avdtp::Request::Discover { responder } => {
                    let infos = [avdtp::StreamInformation::new(
                        expected_stream_id,
                        false,
                        avdtp::MediaType::Audio,
                        avdtp::EndpointType::Source,
                    )];
                    responder.send(&infos)
                }
                avdtp::Request::GetAllCapabilities { stream_id, responder }
                | avdtp::Request::GetCapabilities { stream_id, responder } => {
                    assert_eq!(expected_stream_id, stream_id);
                    let caps = vec![
                        ServiceCapability::MediaTransport,
                        ServiceCapability::MediaCodec {
                            media_type: avdtp::MediaType::Audio,
                            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
                            codec_extra: vec![0x11, 0x45, 51, 250],
                        },
                    ];
                    responder.send(&caps[..])
                }
                avdtp::Request::Open { responder, stream_id } => {
                    assert_eq!(expected_stream_id, stream_id);
                    responder.send()
                }
                avdtp::Request::SetConfiguration {
                    responder,
                    local_stream_id,
                    remote_stream_id,
                    ..
                } => {
                    assert_eq!(local_stream_id, expected_stream_id);
                    // This is the "sink" local stream id.
                    assert_eq!(remote_stream_id, 2_u8.try_into().unwrap());
                    responder.send()
                }
                x => panic!("Unexpected request: {:?}", x),
            };
            res.expect("should be able to respond");
        }

        // Need to discover the remote streams first, or the stream start will not work.
        let collect_capabilities_fut = peer.collect_capabilities();
        pin_mut!(collect_capabilities_fut);

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_pending());

        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a discovery request").unwrap());

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a get_capabilities request").unwrap());

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_ready());

        // Try to start the stream.  It should continue to configure and connect.
        let remote_seid = 4_u8.try_into().unwrap();

        let codec_params = ServiceCapability::MediaCodec {
            media_type: avdtp::MediaType::Audio,
            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
            codec_extra: vec![0x11, 0x45, 51, 51],
        };
        let start_future = peer.stream_start(remote_seid, vec![codec_params]);
        pin_mut!(start_future);

        assert!(exec.run_until_stalled(&mut start_future).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a set_capabilities request").unwrap());

        assert!(exec.run_until_stalled(&mut start_future).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have an open request").unwrap());
    }

    #[fuchsia::test]
    fn peer_stream_start_strips_unsupported_local_capabilities() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, _, _, peer) = setup_peer_test(false);
        let remote = avdtp::Peer::new(remote);
        let mut remote_events = remote.take_request_stream();

        // Respond as if we have a single SBC Source Stream
        fn remote_handle_request(req: avdtp::Request) {
            let expected_stream_id: StreamEndpointId = 4_u8.try_into().unwrap();
            let res = match req {
                avdtp::Request::Discover { responder } => {
                    let infos = [avdtp::StreamInformation::new(
                        expected_stream_id,
                        false,
                        avdtp::MediaType::Audio,
                        avdtp::EndpointType::Source,
                    )];
                    responder.send(&infos)
                }
                avdtp::Request::GetAllCapabilities { stream_id, responder }
                | avdtp::Request::GetCapabilities { stream_id, responder } => {
                    assert_eq!(expected_stream_id, stream_id);
                    let caps = vec![
                        ServiceCapability::MediaTransport,
                        // We don't have a local delay-reporting, so this shouldn't be requested.
                        ServiceCapability::DelayReporting,
                        ServiceCapability::MediaCodec {
                            media_type: avdtp::MediaType::Audio,
                            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
                            codec_extra: vec![0x11, 0x45, 51, 250],
                        },
                    ];
                    responder.send(&caps[..])
                }
                avdtp::Request::Open { responder, stream_id } => {
                    assert_eq!(expected_stream_id, stream_id);
                    responder.send()
                }
                avdtp::Request::SetConfiguration {
                    responder,
                    local_stream_id,
                    remote_stream_id,
                    capabilities,
                } => {
                    assert_eq!(local_stream_id, expected_stream_id);
                    // This is the "sink" local stream id.
                    assert_eq!(remote_stream_id, 2_u8.try_into().unwrap());
                    // Make sure we didn't request a DelayReport since the local Sink doesn't
                    // support it.
                    assert!(!capabilities.contains(&ServiceCapability::DelayReporting));
                    responder.send()
                }
                x => panic!("Unexpected request: {:?}", x),
            };
            res.expect("should be able to respond");
        }

        // Need to discover the remote streams first, or the stream start will not work.
        let collect_capabilities_fut = peer.collect_capabilities();
        pin_mut!(collect_capabilities_fut);

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_pending());

        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a discovery request").unwrap());

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a get_capabilities request").unwrap());

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_ready());

        // Try to start the stream.  It should continue to configure and connect.
        let remote_seid = 4_u8.try_into().unwrap();

        let codec_params = ServiceCapability::MediaCodec {
            media_type: avdtp::MediaType::Audio,
            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
            codec_extra: vec![0x11, 0x45, 51, 51],
        };
        let start_future =
            peer.stream_start(remote_seid, vec![codec_params, ServiceCapability::DelayReporting]);
        pin_mut!(start_future);

        assert!(exec.run_until_stalled(&mut start_future).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a set_configuration request").unwrap());

        assert!(exec.run_until_stalled(&mut start_future).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have an open request").unwrap());
    }

    #[fuchsia::test]
    fn peer_stream_start_fails_wrong_direction() {
        let mut exec = fasync::TestExecutor::new();

        // Setup peers with only one Source Stream.
        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");

        let mut streams = Streams::new();
        let source = Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Source),
            TestMediaTaskBuilder::new().builder(),
        );
        streams.insert(source);

        let peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            None,
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote = avdtp::Peer::new(remote);
        let mut remote_events = remote.take_request_stream();

        // Respond as if we have a single SBC Source Stream
        fn remote_handle_request(req: avdtp::Request) {
            let expected_stream_id: StreamEndpointId = 2_u8.try_into().unwrap();
            let res = match req {
                avdtp::Request::Discover { responder } => {
                    let infos = [avdtp::StreamInformation::new(
                        expected_stream_id,
                        false,
                        avdtp::MediaType::Audio,
                        avdtp::EndpointType::Source,
                    )];
                    responder.send(&infos)
                }
                avdtp::Request::GetAllCapabilities { stream_id, responder }
                | avdtp::Request::GetCapabilities { stream_id, responder } => {
                    assert_eq!(expected_stream_id, stream_id);
                    let caps = vec![
                        ServiceCapability::MediaTransport,
                        ServiceCapability::MediaCodec {
                            media_type: avdtp::MediaType::Audio,
                            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
                            codec_extra: vec![0x11, 0x45, 51, 250],
                        },
                    ];
                    responder.send(&caps[..])
                }
                avdtp::Request::Open { responder, .. } => responder.send(),
                avdtp::Request::SetConfiguration { responder, .. } => responder.send(),
                x => panic!("Unexpected request: {:?}", x),
            };
            res.expect("should be able to respond");
        }

        // Need to discover the remote streams first, or the stream start will always work.
        let collect_capabilities_fut = peer.collect_capabilities();
        pin_mut!(collect_capabilities_fut);

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_pending());

        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a discovery request").unwrap());

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_pending());
        let request = exec.run_singlethreaded(&mut remote_events.next());
        remote_handle_request(request.expect("should have a get_capabilities request").unwrap());

        assert!(exec.run_until_stalled(&mut collect_capabilities_fut).is_ready());

        // Try to start the stream.  It should fail with OutOfRange because we can't connect a Source to a Source.
        let remote_seid = 2_u8.try_into().unwrap();

        let codec_params = ServiceCapability::MediaCodec {
            media_type: avdtp::MediaType::Audio,
            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
            codec_extra: vec![0x11, 0x45, 51, 51],
        };
        let start_future = peer.stream_start(remote_seid, vec![codec_params]);
        pin_mut!(start_future);

        match exec.run_until_stalled(&mut start_future) {
            Poll::Ready(Err(avdtp::Error::OutOfRange)) => {}
            x => panic!("Expected a ready OutOfRange error but got {:?}", x),
        };
    }

    #[fuchsia::test]
    fn peer_stream_start_fails_to_connect() {
        let mut exec = fasync::TestExecutor::new();

        let (remote, mut profile_request_stream, _, peer) = setup_peer_test(false);

        let remote_seid = 2_u8.try_into().unwrap();

        let codec_params = ServiceCapability::MediaCodec {
            media_type: avdtp::MediaType::Audio,
            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
            codec_extra: vec![0x11, 0x45, 51, 51],
        };

        let start_future = peer.stream_start(remote_seid, vec![codec_params]);
        pin_mut!(start_future);

        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => {}
            x => panic!("was expecting pending but got {x:?}"),
        };

        receive_simple_accept(&remote, 0x03); // Set Configuration

        assert!(exec.run_until_stalled(&mut start_future).is_pending());

        receive_simple_accept(&remote, 0x06); // Open

        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => {}
            Poll::Ready(x) => panic!("Expected to be pending but {x:?}"),
        };

        // Should connect the media channel after open.
        let request = exec.run_until_stalled(&mut profile_request_stream.next());
        match request {
            Poll::Ready(Some(Ok(ProfileRequest::Connect { peer_id, responder, .. }))) => {
                assert_eq!(PeerId(1), peer_id.into());
                responder.send(&mut Err(ErrorCode::Failed)).expect("responder sends");
            }
            x => panic!("Should have sent a open l2cap request, but got {:?}", x),
        };

        // Should return an error.
        // Should be done without an error, but with no streams.
        match exec.run_until_stalled(&mut start_future) {
            Poll::Pending => panic!("Should be ready after start fails"),
            Poll::Ready(Ok(_stream)) => panic!("Shouldn't have succeeded stream here"),
            Poll::Ready(Err(_)) => {}
        }
    }

    /// Test that the delay reports get acknowledged and they are sent to cobalt.
    #[fuchsia::test]
    async fn peer_delay_report() {
        let (remote, _profile_requests, cobalt_recv, peer) = setup_peer_test(true);
        let remote_peer = avdtp::Peer::new(remote);
        let mut remote_events = remote_peer.take_request_stream();

        // Respond as if we have a single SBC Sink Stream
        async fn remote_handle_request(req: avdtp::Request, peer: &avdtp::Peer) {
            let expected_stream_id: StreamEndpointId = 4_u8.try_into().unwrap();
            // "peer" in this case is the test code Peer stream
            let expected_peer_stream_id: StreamEndpointId = 1_u8.try_into().unwrap();
            use avdtp::Request::*;
            match req {
                Discover { responder } => {
                    let infos = [avdtp::StreamInformation::new(
                        expected_stream_id,
                        false,
                        avdtp::MediaType::Audio,
                        avdtp::EndpointType::Sink,
                    )];
                    responder.send(&infos).expect("response should succeed");
                }
                GetAllCapabilities { stream_id, responder }
                | GetCapabilities { stream_id, responder } => {
                    assert_eq!(expected_stream_id, stream_id);
                    let caps = vec![
                        ServiceCapability::MediaTransport,
                        ServiceCapability::MediaCodec {
                            media_type: avdtp::MediaType::Audio,
                            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
                            codec_extra: vec![0x11, 0x45, 51, 250],
                        },
                    ];
                    responder.send(&caps[..]).expect("response should succeed");
                    // Sending a delayreport before the stream is configured is not allowed, it's a
                    // bad state.
                    assert!(peer.delay_report(&expected_peer_stream_id, 0xc0de).await.is_err());
                }
                Open { responder, stream_id } => {
                    // Configuration has happened but open not succeeded yet, send delay reports.
                    assert!(peer.delay_report(&expected_stream_id, 0xc0de).await.is_err());
                    // Send a delay report to the peer.
                    peer.delay_report(&expected_peer_stream_id, 0xc0de)
                        .await
                        .expect("should get acked correctly");
                    assert_eq!(expected_stream_id, stream_id);
                    responder.send().expect("response should succeed");
                }
                SetConfiguration { responder, local_stream_id, remote_stream_id, .. } => {
                    assert_eq!(local_stream_id, expected_stream_id);
                    assert_eq!(remote_stream_id, expected_peer_stream_id);
                    responder.send().expect("should send back response without issue");
                }
                x => panic!("Unexpected request: {:?}", x),
            };
        }

        let collect_fut = peer.collect_capabilities();
        pin_mut!(collect_fut);

        // Discover then a GetCapabilities.
        let Either::Left((request, collect_fut)) = futures::future::select(
            remote_events.next(), collect_fut).await else {
            panic!("Collect future shouldn't finish first");
        };
        pin_mut!(collect_fut);
        remote_handle_request(request.expect("a request").unwrap(), &remote_peer).await;
        let Either::Left((request, collect_fut)) = futures::future::select(
            remote_events.next(), collect_fut).await else {
            panic!("Collect future shouldn't finish first");
        };
        remote_handle_request(request.expect("a request").unwrap(), &remote_peer).await;

        // Collect future should be able to finish now.
        assert_eq!(1, collect_fut.await.expect("should get the remote endpoints back").len());

        // Try to start the stream.  It should go through the normal motions,
        let remote_seid = 4_u8.try_into().unwrap();

        let codec_params = ServiceCapability::MediaCodec {
            media_type: avdtp::MediaType::Audio,
            codec_type: avdtp::MediaCodecType::AUDIO_SBC,
            codec_extra: vec![0x11, 0x45, 51, 51],
        };

        // We don't expect this task to finish before being dropped, since we never respond to the
        // request to open the transport channel.
        let _start_task = fasync::Task::spawn(async move {
            let _ = peer.stream_start(remote_seid, vec![codec_params]).await;
            panic!("stream start task finished");
        });

        let request = remote_events.next().await.expect("should have set_config").unwrap();
        remote_handle_request(request, &remote_peer).await;

        let request = remote_events.next().await.expect("should have open").unwrap();
        remote_handle_request(request, &remote_peer).await;

        let mut cobalt = cobalt_recv.expect("should have receiver");

        let mut got_ids = HashMap::new();
        let delay_metric_id = bt_metrics::AVDTP_DELAY_REPORT_IN_NANOSECONDS_METRIC_ID;
        while got_ids.len() < 3 || *got_ids.get(&delay_metric_id).unwrap_or(&0) < 3 {
            let report = respond_to_metrics_req_for_test(cobalt.next().await.unwrap().unwrap());
            let _ = got_ids.entry(report.metric_id).and_modify(|x| *x += 1).or_insert(1);
            // All the delay reports should report the same value correctly.
            if report.metric_id == delay_metric_id {
                assert_eq!(MetricEventPayload::IntegerValue(0xc0de * 100000), report.payload);
            }
        }
        assert!(got_ids.contains_key(&bt_metrics::A2DP_CODEC_AVAILABILITY_MIGRATED_METRIC_ID));
        assert!(got_ids.contains_key(&bt_metrics::A2DP_REMOTE_PEER_CAPABILITIES_METRIC_ID));
        assert!(got_ids.contains_key(&delay_metric_id));
        // There should have been three reports.
        // We report the delay amount even if it fails to work.
        assert_eq!(got_ids.get(&delay_metric_id).cloned(), Some(3));
    }

    fn sbc_capabilities() -> Vec<ServiceCapability> {
        let sbc_codec_info = SbcCodecInfo::new(
            SbcSamplingFrequency::FREQ48000HZ,
            SbcChannelMode::JOINT_STEREO,
            SbcBlockCount::SIXTEEN,
            SbcSubBands::EIGHT,
            SbcAllocation::LOUDNESS,
            /* min_bpv= */ 53,
            /* max_bpv= */ 53,
        )
        .expect("sbc codec info");

        vec![avdtp::ServiceCapability::MediaTransport, sbc_codec_info.into()]
    }

    /// Test that the remote end can configure and start a stream.
    #[fuchsia::test]
    fn peer_as_acceptor() {
        let mut exec = fasync::TestExecutor::new();

        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let mut streams = Streams::new();
        let mut test_builder = TestMediaTaskBuilder::new();
        streams.insert(Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Source),
            test_builder.builder(),
        ));

        let peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            None,
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote_peer = avdtp::Peer::new(remote);

        let discover_fut = remote_peer.discover();
        pin_mut!(discover_fut);

        let expected = vec![make_sbc_endpoint(1, avdtp::EndpointType::Source).information()];
        match exec.run_until_stalled(&mut discover_fut) {
            Poll::Ready(Ok(res)) => assert_eq!(res, expected),
            x => panic!("Expected discovery to complete and got {:?}", x),
        };

        let sbc_endpoint_id = 1_u8.try_into().expect("should be able to get sbc endpointid");
        let unknown_endpoint_id = 2_u8.try_into().expect("should be able to get sbc endpointid");

        let get_caps_fut = remote_peer.get_capabilities(&sbc_endpoint_id);
        pin_mut!(get_caps_fut);

        match exec.run_until_stalled(&mut get_caps_fut) {
            // There are two caps (mediatransport, mediacodec) in the sbc endpoint.
            Poll::Ready(Ok(caps)) => assert_eq!(2, caps.len()),
            x => panic!("Get capabilities should be ready but got {:?}", x),
        };

        let get_caps_fut = remote_peer.get_capabilities(&unknown_endpoint_id);
        pin_mut!(get_caps_fut);

        match exec.run_until_stalled(&mut get_caps_fut) {
            Poll::Ready(Err(avdtp::Error::RemoteRejected(e))) => {
                assert_eq!(Some(Ok(avdtp::ErrorCode::BadAcpSeid)), e.error_code())
            }
            x => panic!("Get capabilities should be a ready error but got {:?}", x),
        };

        let get_caps_fut = remote_peer.get_all_capabilities(&sbc_endpoint_id);
        pin_mut!(get_caps_fut);

        match exec.run_until_stalled(&mut get_caps_fut) {
            // There are two caps (mediatransport, mediacodec) in the sbc endpoint.
            Poll::Ready(Ok(caps)) => assert_eq!(2, caps.len()),
            x => panic!("Get capabilities should be ready but got {:?}", x),
        };

        let sbc_caps = sbc_capabilities();
        let set_config_fut =
            remote_peer.set_configuration(&sbc_endpoint_id, &sbc_endpoint_id, &sbc_caps);
        pin_mut!(set_config_fut);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Set capabilities should be ready but got {:?}", x),
        };

        let open_fut = remote_peer.open(&sbc_endpoint_id);
        pin_mut!(open_fut);
        match exec.run_until_stalled(&mut open_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Open should be ready but got {:?}", x),
        };

        // Establish a media transport stream
        let (_remote_transport, transport) = Channel::create();

        assert_eq!(Some(()), peer.receive_channel(transport).ok());

        let stream_ids = vec![sbc_endpoint_id.clone()];
        let start_fut = remote_peer.start(&stream_ids);
        pin_mut!(start_fut);
        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Start should be ready but got {:?}", x),
        };

        // The task should be created locally and started.
        let media_task = test_builder.expect_task();
        assert!(media_task.is_started());

        let suspend_fut = remote_peer.suspend(&stream_ids);
        pin_mut!(suspend_fut);
        match exec.run_until_stalled(&mut suspend_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Start should be ready but got {:?}", x),
        };

        // Should have stopped the media task on suspend.
        assert!(!media_task.is_started());
    }

    #[fuchsia::test]
    fn peer_set_config_reject_first() {
        let mut exec = fasync::TestExecutor::new();

        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let mut streams = Streams::new();
        let test_builder = TestMediaTaskBuilder::new();
        streams.insert(Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Source),
            test_builder.builder(),
        ));

        let _peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            None,
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote_peer = avdtp::Peer::new(remote);

        let sbc_endpoint_id = 1_u8.try_into().expect("should be able to get sbc endpointid");

        let wrong_freq_sbc = &[SbcCodecInfo::new(
            SbcSamplingFrequency::FREQ44100HZ, // 44.1 is not supported by the caps from above.
            SbcChannelMode::JOINT_STEREO,
            SbcBlockCount::SIXTEEN,
            SbcSubBands::EIGHT,
            SbcAllocation::LOUDNESS,
            /* min_bpv= */ 53,
            /* max_bpv= */ 53,
        )
        .expect("sbc codec info")
        .into()];

        let set_config_fut =
            remote_peer.set_configuration(&sbc_endpoint_id, &sbc_endpoint_id, wrong_freq_sbc);
        pin_mut!(set_config_fut);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Err(avdtp::Error::RemoteRejected(e))) => {
                assert!(e.service_category().is_some())
            }
            x => panic!("Set capabilities should have been rejected but got {:?}", x),
        };

        let sbc_caps = sbc_capabilities();
        let set_config_fut =
            remote_peer.set_configuration(&sbc_endpoint_id, &sbc_endpoint_id, &sbc_caps);
        pin_mut!(set_config_fut);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Set capabilities should be ready but got {:?}", x),
        };
    }

    #[fuchsia::test]
    fn peer_starts_waiting_streams() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        exec.set_fake_time(fasync::Time::from_nanos(5_000_000_000));

        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let mut streams = Streams::new();
        let mut test_builder = TestMediaTaskBuilder::new();
        streams.insert(Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Source),
            test_builder.builder(),
        ));

        let peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            None,
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote_peer = avdtp::Peer::new(remote);

        let sbc_endpoint_id = 1_u8.try_into().expect("should be able to get sbc endpointid");

        let sbc_caps = sbc_capabilities();
        let set_config_fut =
            remote_peer.set_configuration(&sbc_endpoint_id, &sbc_endpoint_id, &sbc_caps);
        pin_mut!(set_config_fut);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Set capabilities should be ready but got {:?}", x),
        };

        let open_fut = remote_peer.open(&sbc_endpoint_id);
        pin_mut!(open_fut);
        match exec.run_until_stalled(&mut open_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Open should be ready but got {:?}", x),
        };

        // Establish a media transport stream
        let (_remote_transport, transport) = Channel::create();
        assert_eq!(Some(()), peer.receive_channel(transport).ok());

        // The remote end should get a start request after the timeout.
        let mut remote_requests = remote_peer.take_request_stream();
        let next_remote_request_fut = remote_requests.next();
        pin_mut!(next_remote_request_fut);

        // Nothing should happen immediately.
        assert!(exec.run_until_stalled(&mut next_remote_request_fut).is_pending());

        // After the timeout has passed..
        exec.set_fake_time(zx::Duration::from_seconds(3).after_now());
        let _ = exec.wake_expired_timers();

        let stream_ids = match exec.run_until_stalled(&mut next_remote_request_fut) {
            Poll::Ready(Some(Ok(avdtp::Request::Start { responder, stream_ids }))) => {
                responder.send().unwrap();
                stream_ids
            }
            x => panic!("Expected to receive a start request for the stream, got {:?}", x),
        };

        // We should start the media task, so the task should be created locally
        let media_task =
            exec.run_until_stalled(&mut test_builder.next_task()).expect("ready").unwrap();
        assert!(media_task.is_started());

        // Remote peer should still be able to suspend the stream.
        let suspend_fut = remote_peer.suspend(&stream_ids);
        pin_mut!(suspend_fut);
        match exec.run_until_stalled(&mut suspend_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Suspend should be ready but got {:?}", x),
        };

        // Should have stopped the media task on suspend.
        assert!(!media_task.is_started());
    }

    #[fuchsia::test]
    fn needs_permit_to_start_streams() {
        let mut exec = fasync::TestExecutor::new();

        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let mut streams = Streams::new();
        let mut test_builder = TestMediaTaskBuilder::new();
        streams.insert(Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Sink),
            test_builder.builder(),
        ));
        streams.insert(Stream::build(
            make_sbc_endpoint(2, avdtp::EndpointType::Sink),
            test_builder.builder(),
        ));
        let mut next_task_fut = test_builder.next_task();

        let permits = Permits::new(1);
        let taken_permit = permits.get().expect("permit taken");

        let peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            Some(permits.clone()),
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote_peer = avdtp::Peer::new(remote);

        let sbc_endpoint_id = 1_u8.try_into().unwrap();

        let sbc_caps = sbc_capabilities();
        let mut set_config_fut =
            remote_peer.set_configuration(&sbc_endpoint_id, &sbc_endpoint_id, &sbc_caps);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Set capabilities should be ready but got {:?}", x),
        };

        let mut open_fut = remote_peer.open(&sbc_endpoint_id);
        match exec.run_until_stalled(&mut open_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Open should be ready but got {:?}", x),
        };

        // Establish a media transport stream
        let (_remote_transport, transport) = Channel::create();
        assert_eq!(Some(()), peer.receive_channel(transport).ok());

        // Do the same, but for the OTHER stream.
        let sbc_endpoint_two = 2_u8.try_into().unwrap();

        let mut set_config_fut =
            remote_peer.set_configuration(&sbc_endpoint_two, &sbc_endpoint_two, &sbc_caps);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Set capabilities should be ready but got {:?}", x),
        };

        let mut open_fut = remote_peer.open(&sbc_endpoint_two);
        match exec.run_until_stalled(&mut open_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Open should be ready but got {:?}", x),
        };

        // Establish a media transport stream
        let (_remote_transport_two, transport_two) = Channel::create();
        assert_eq!(Some(()), peer.receive_channel(transport_two).ok());

        // Remote peer should still be able to try to start the stream, and we will say yes, but
        // that last seid looks wonky.
        let unknown_endpoint_id: StreamEndpointId = 9_u8.try_into().unwrap();
        let stream_ids = [sbc_endpoint_id.clone(), unknown_endpoint_id.clone()];
        let mut start_fut = remote_peer.start(&stream_ids);
        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Err(avdtp::Error::RemoteRejected(rejection))) => {
                assert_eq!(avdtp::ErrorCode::BadAcpSeid, rejection.error_code().unwrap().unwrap());
                assert_eq!(unknown_endpoint_id, rejection.stream_id().unwrap());
            }
            x => panic!("Start should be ready but got {:?}", x),
        };

        // We can't get a permit (none are available) so we suspend the one we didn't error on.
        let mut remote_requests = remote_peer.take_request_stream();

        let suspended_stream_ids = match exec.run_singlethreaded(&mut remote_requests.next()) {
            Some(Ok(avdtp::Request::Suspend { responder, stream_ids })) => {
                responder.send().unwrap();
                stream_ids
            }
            x => panic!("Expected to receive a suspend request for the stream, got {:?}", x),
        };

        assert!(suspended_stream_ids.contains(&sbc_endpoint_id));
        assert_eq!(1, suspended_stream_ids.len());

        // And we should have not tried to start a task.
        match exec.run_until_stalled(&mut next_task_fut) {
            Poll::Pending => {}
            x => panic!("Local task should not have been created at this point: {:?}", x),
        };

        // No matter how many times they ask to start, we will still suspend (but not queue another
        // reservation for the same id)
        let mut start_fut = remote_peer.start(&[sbc_endpoint_id.clone()]);
        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Start should be ready but got {:?}", x),
        }

        let suspended_stream_ids = match exec.run_until_stalled(&mut remote_requests.next()) {
            Poll::Ready(Some(Ok(avdtp::Request::Suspend { responder, stream_ids }))) => {
                responder.send().unwrap();
                stream_ids
            }
            x => panic!("Expected to receive a suspend request for the stream, got {:?}", x),
        };
        assert!(suspended_stream_ids.contains(&sbc_endpoint_id));

        // After a permit is available, should try to start the first endpoint that failed.
        drop(taken_permit);

        match exec.run_singlethreaded(&mut remote_requests.next()) {
            Some(Ok(avdtp::Request::Start { responder, stream_ids })) => {
                assert_eq!(stream_ids, &[sbc_endpoint_id.clone()]);
                responder.send().unwrap();
            }
            x => panic!("Expected start on permit available but got {x:?}"),
        };

        // And we should start a task.
        let media_task = match exec.run_until_stalled(&mut next_task_fut) {
            Poll::Ready(Some(task)) => task,
            x => panic!("Local task should be created at this point: {:?}", x),
        };

        assert!(media_task.is_started());

        // If the remote asks to start another one, we still suspend it immediately.
        let mut start_fut = remote_peer.start(&[sbc_endpoint_two.clone()]);
        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Start should be ready but got {:?}", x),
        }

        let suspended_stream_ids = match exec.run_until_stalled(&mut remote_requests.next()) {
            Poll::Ready(Some(Ok(avdtp::Request::Suspend { responder, stream_ids }))) => {
                responder.send().unwrap();
                stream_ids
            }
            x => panic!("Expected to receive a suspend request for the stream, got {:?}", x),
        };

        assert!(suspended_stream_ids.contains(&sbc_endpoint_two));
        assert_eq!(1, suspended_stream_ids.len());

        // Once the first one is done, the second can start.
        let mut suspend_fut = remote_peer.suspend(&[sbc_endpoint_id.clone()]);
        match exec.run_until_stalled(&mut suspend_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Start should be ready but got {:?}", x),
        }

        match exec.run_singlethreaded(&mut remote_requests.next()) {
            Some(Ok(avdtp::Request::Start { responder, stream_ids })) => {
                assert_eq!(stream_ids, &[sbc_endpoint_two]);
                responder.send().unwrap();
            }
            x => panic!("Expected start on permit available but got {x:?}"),
        };
    }

    fn start_sbc_stream(
        exec: &mut fasync::TestExecutor,
        media_test_builder: &mut TestMediaTaskBuilder,
        peer: &Peer,
        remote_peer: &avdtp::Peer,
        local_id: &StreamEndpointId,
        remote_id: &StreamEndpointId,
    ) -> TestMediaTask {
        let sbc_caps = sbc_capabilities();
        let set_config_fut = remote_peer.set_configuration(&local_id, &remote_id, &sbc_caps);
        pin_mut!(set_config_fut);

        match exec.run_until_stalled(&mut set_config_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Set capabilities should be ready but got {:?}", x),
        };

        let open_fut = remote_peer.open(&local_id);
        pin_mut!(open_fut);
        match exec.run_until_stalled(&mut open_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Open should be ready but got {:?}", x),
        };

        // Establish a media transport stream
        let (_remote_transport, transport) = Channel::create();
        assert_eq!(Some(()), peer.receive_channel(transport).ok());

        // Remote peer should still be able to try to start the stream, and we will say yes.
        let stream_ids = [local_id.clone()];
        let start_fut = remote_peer.start(&stream_ids);
        pin_mut!(start_fut);
        match exec.run_until_stalled(&mut start_fut) {
            Poll::Ready(Ok(())) => {}
            x => panic!("Start should be ready but got {:?}", x),
        };

        // And we should start a media task.
        let media_task = media_test_builder.expect_task();
        assert!(media_task.is_started());

        let _ = exec.run_until_stalled(&mut futures::future::pending::<()>());
        media_task
    }

    #[fuchsia::test]
    fn permits_can_be_revoked_and_reinstated_all() {
        let mut exec = fasync::TestExecutor::new();

        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let mut streams = Streams::new();
        let mut test_builder = TestMediaTaskBuilder::new();
        streams.insert(Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Sink),
            test_builder.builder(),
        ));
        let sbc_endpoint_id = 1_u8.try_into().unwrap();
        let remote_sbc_endpoint_id = 7_u8.try_into().unwrap();

        streams.insert(Stream::build(
            make_sbc_endpoint(2, avdtp::EndpointType::Sink),
            test_builder.builder(),
        ));
        let sbc2_endpoint_id = 2_u8.try_into().unwrap();
        let remote_sbc2_endpoint_id = 6_u8.try_into().unwrap();

        let permits = Permits::new(2);

        let peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            Some(permits.clone()),
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote_peer = avdtp::Peer::new(remote);

        let one_media_task = start_sbc_stream(
            &mut exec,
            &mut test_builder,
            &peer,
            &remote_peer,
            &sbc_endpoint_id,
            &remote_sbc_endpoint_id,
        );
        let two_media_task = start_sbc_stream(
            &mut exec,
            &mut test_builder,
            &peer,
            &remote_peer,
            &sbc2_endpoint_id,
            &remote_sbc2_endpoint_id,
        );

        // Someone comes along and revokes our permits.
        let taken_permits = permits.seize();

        let remote_endpoints: HashSet<_> =
            [&remote_sbc_endpoint_id, &remote_sbc2_endpoint_id].iter().cloned().collect();

        // We should send a suspend to the other end, for both of them.
        let mut remote_requests = remote_peer.take_request_stream();
        let mut expected_suspends = remote_endpoints.clone();
        while !expected_suspends.is_empty() {
            match exec.run_until_stalled(&mut remote_requests.next()) {
                Poll::Ready(Some(Ok(avdtp::Request::Suspend { responder, stream_ids }))) => {
                    for stream_id in stream_ids {
                        assert!(expected_suspends.remove(&stream_id));
                    }
                    responder.send().expect("send response okay");
                }
                x => panic!("Expected suspension and got {:?}", x),
            }
        }

        // And the media tasks should be stopped.
        assert!(!one_media_task.is_started());
        assert!(!two_media_task.is_started());

        // After the permits are available again, we send a start, and start the media stream.
        drop(taken_permits);

        let mut expected_starts = remote_endpoints.clone();
        while !expected_starts.is_empty() {
            match exec.run_singlethreaded(&mut remote_requests.next()) {
                Some(Ok(avdtp::Request::Start { responder, stream_ids })) => {
                    for stream_id in stream_ids {
                        assert!(expected_starts.remove(&stream_id));
                    }
                    responder.send().expect("send response okay");
                }
                x => panic!("Expected start and got {:?}", x),
            }
        }
        // And we should start two media tasks.

        let one_media_task = test_builder.expect_task();
        assert!(one_media_task.is_started());
        let two_media_task = match exec.run_until_stalled(&mut test_builder.next_task()) {
            Poll::Ready(Some(task)) => task,
            x => panic!("Expected another ready task but {x:?}"),
        };
        assert!(two_media_task.is_started());
    }

    #[fuchsia::test]
    fn permits_can_be_revoked_one_at_a_time() {
        let mut exec = fasync::TestExecutor::new();

        let (avdtp, remote) = setup_avdtp_peer();
        let (profile_proxy, _requests) =
            create_proxy_and_stream::<ProfileMarker>().expect("test proxy pair creation");
        let mut streams = Streams::new();
        let mut test_builder = TestMediaTaskBuilder::new();
        streams.insert(Stream::build(
            make_sbc_endpoint(1, avdtp::EndpointType::Sink),
            test_builder.builder(),
        ));
        let sbc_endpoint_id = 1_u8.try_into().unwrap();
        let remote_sbc_endpoint_id = 7_u8.try_into().unwrap();

        streams.insert(Stream::build(
            make_sbc_endpoint(2, avdtp::EndpointType::Sink),
            test_builder.builder(),
        ));
        let sbc2_endpoint_id = 2_u8.try_into().unwrap();
        let remote_sbc2_endpoint_id = 6_u8.try_into().unwrap();

        let permits = Permits::new(2);

        let peer = Peer::create(
            PeerId(1),
            avdtp,
            streams,
            Some(permits.clone()),
            profile_proxy,
            bt_metrics::MetricsLogger::default(),
        );
        let remote_peer = avdtp::Peer::new(remote);

        let one_media_task = start_sbc_stream(
            &mut exec,
            &mut test_builder,
            &peer,
            &remote_peer,
            &sbc_endpoint_id,
            &remote_sbc_endpoint_id,
        );
        let two_media_task = start_sbc_stream(
            &mut exec,
            &mut test_builder,
            &peer,
            &remote_peer,
            &sbc2_endpoint_id,
            &remote_sbc2_endpoint_id,
        );

        // Someone comes along and revokes one of our permits.
        let taken_permit = permits.take();

        let remote_endpoints: HashSet<_> =
            [&remote_sbc_endpoint_id, &remote_sbc2_endpoint_id].iter().cloned().collect();

        // We should send a suspend to the other end, for both of them.
        let mut remote_requests = remote_peer.take_request_stream();
        let suspended_id = match exec.run_until_stalled(&mut remote_requests.next()) {
            Poll::Ready(Some(Ok(avdtp::Request::Suspend { responder, stream_ids }))) => {
                assert!(stream_ids.len() == 1);
                assert!(remote_endpoints.contains(&stream_ids[0]));
                responder.send().expect("send response okay");
                stream_ids[0].clone()
            }
            x => panic!("Expected suspension and got {:?}", x),
        };

        // And the correct one of the media tasks should be stopped.
        if suspended_id == remote_sbc_endpoint_id {
            assert!(!one_media_task.is_started());
            assert!(two_media_task.is_started());
        } else {
            assert!(one_media_task.is_started());
            assert!(!two_media_task.is_started());
        }

        // After the permits are available again, we send a start, and start the media stream.
        drop(taken_permit);

        match exec.run_singlethreaded(&mut remote_requests.next()) {
            Some(Ok(avdtp::Request::Start { responder, stream_ids })) => {
                assert_eq!(stream_ids, &[suspended_id]);
                responder.send().expect("send response okay");
            }
            x => panic!("Expected start and got {:?}", x),
        }
        // And we should start another media task.
        let media_task = match exec.run_until_stalled(&mut test_builder.next_task()) {
            Poll::Ready(Some(task)) => task,
            x => panic!("Expected media task to start: {x:?}"),
        };
        assert!(media_task.is_started());
    }

    /// Test that the version check method correctly differentiates between newer
    /// and older A2DP versions.
    #[fuchsia::test]
    fn version_check() {
        let p1: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 1,
            minor_version: 3,
        };
        assert_eq!(true, a2dp_version_check(p1));

        let p1: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 2,
            minor_version: 10,
        };
        assert_eq!(true, a2dp_version_check(p1));

        let p1: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 1,
            minor_version: 0,
        };
        assert_eq!(false, a2dp_version_check(p1));

        let p1: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 0,
            minor_version: 9,
        };
        assert_eq!(false, a2dp_version_check(p1));

        let p1: ProfileDescriptor = ProfileDescriptor {
            profile_id: ServiceClassProfileIdentifier::AdvancedAudioDistribution,
            major_version: 2,
            minor_version: 2,
        };
        assert_eq!(true, a2dp_version_check(p1));
    }
}
