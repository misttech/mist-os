// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::ControlFlow;

use fidl::{
    encoding::Decode as _,
    endpoints::{ProtocolMarker, RequestStream},
};
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket as fposix_socket;
use fidl_fuchsia_posix_socket_raw as fpraw;
use fuchsia_zircon as zx;
use futures::StreamExt as _;
use net_types::{
    ip::{Ip, Ipv4, Ipv6},
    SpecifiedAddr,
};
use netstack3_core::{
    device::{DeviceId, WeakDeviceId},
    ip::{
        RawIpSocketId, RawIpSocketProtocol, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes,
    },
    socket::StrictlyZonedAddr,
    sync::Mutex,
    IpExt,
};
use packet_formats::ip::IpPacket as _;
use tracing::error;
use zerocopy::ByteSlice;
use zx::{HandleBased, Peered};

use crate::bindings::{
    socket::{
        queue::{BodyLen, MessageQueue},
        IpSockAddrExt, SockAddr,
    },
    util::{AllowBindingIdFromWeak, RemoveResourceResultExt, TryFromFidl, TryIntoFidlWithContext},
    BindingsCtx, Ctx,
};

use super::{
    worker::{self, CloseResponder, SocketWorker, SocketWorkerHandler, TaskSpawnerCollection},
    SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING,
};

impl RawIpSocketsBindingsTypes for BindingsCtx {
    type RawIpSocketState<I: Ip> = SocketState<I>;
}

impl<I: IpExt> RawIpSocketsBindingsContext<I, DeviceId<Self>> for BindingsCtx {
    fn receive_packet<B: ByteSlice>(
        &self,
        socket: &RawIpSocketId<I, Self>,
        packet: &I::Packet<B>,
        device: &DeviceId<Self>,
    ) {
        socket.external_state().enqueue_rx_packet::<B>(packet, device.downgrade())
    }
}

/// The Core held state of a raw IP socket.
#[derive(Debug)]
pub struct SocketState<I: Ip> {
    /// The received IP packets for the socket.
    rx_queue: Mutex<MessageQueue<ReceivedIpPacket<I>>>,
}

impl<I: IpExt> SocketState<I> {
    fn new(event: zx::EventPair) -> Self {
        SocketState { rx_queue: Mutex::new(MessageQueue::new(event)) }
    }

    fn enqueue_rx_packet<B: ByteSlice>(
        &self,
        packet: &I::Packet<B>,
        device: WeakDeviceId<BindingsCtx>,
    ) {
        self.rx_queue.lock().receive(ReceivedIpPacket::new::<B>(packet, device))
    }

    fn dequeue_rx_packet(&self) -> Option<ReceivedIpPacket<I>> {
        self.rx_queue.lock().pop()
    }
}

/// A received IP Packet.
#[derive(Debug)]
struct ReceivedIpPacket<I: Ip> {
    /// The packet bytes, including the header and body.
    data: Vec<u8>,
    /// The source IP address of the packet.
    src_addr: I::Addr,
    /// The device on which the packet was received.
    device: WeakDeviceId<BindingsCtx>,
}

impl<I: IpExt> ReceivedIpPacket<I> {
    fn new<B: ByteSlice>(packet: &I::Packet<B>, device: WeakDeviceId<BindingsCtx>) -> Self {
        ReceivedIpPacket { src_addr: packet.src_ip(), data: packet.to_vec(), device }
    }
}

impl<I: Ip> BodyLen for ReceivedIpPacket<I> {
    fn body_len(&self) -> usize {
        let Self { data, src_addr: _, device: _ } = self;
        data.len()
    }
}

/// The worker held state of a raw IP socket.
#[derive(Debug)]
struct SocketWorkerState<I: IpExt> {
    /// The event to hand off for [`fpraw::SocketRequest::Describe`].
    peer_event: zx::EventPair,
    /// The identifier for the [`netstack3_core`] socket resource.
    id: RawIpSocketId<I, BindingsCtx>,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpExt> SocketWorkerState<I> {
    fn new(
        ctx: &mut Ctx,
        proto: RawIpSocketProtocol<I>,
        SocketWorkerProperties {}: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        match local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            Ok(()) => (),
            Err(e) => error!("socket failed to signal peer: {:?}", e),
        };

        let id = ctx.api().raw_ip_socket().create(proto, SocketState::new(local_event));
        SocketWorkerState { peer_event, id }
    }
}

impl CloseResponder for fpraw::SocketCloseResponder {
    fn send(self, response: Result<(), i32>) -> Result<(), fidl::Error> {
        fpraw::SocketCloseResponder::send(self, response)
    }
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
impl<I: IpExt + IpSockAddrExt> SocketWorkerHandler for SocketWorkerState<I> {
    type Request = fpraw::SocketRequest;

    type RequestStream = fpraw::SocketRequestStream;

    type CloseResponder = fpraw::SocketCloseResponder;

    type SetupArgs = ();

    type Spawner = ();

    fn handle_request(
        &mut self,
        ctx: &mut Ctx,
        request: Self::Request,
        _spawner: &TaskSpawnerCollection<Self::Spawner>,
    ) -> std::ops::ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request)
    }

    async fn close(self, ctx: &mut Ctx) {
        let SocketWorkerState { peer_event: _, id } = self;
        let weak = id.downgrade();
        let SocketState { rx_queue: _ } = ctx
            .api()
            .raw_ip_socket()
            .close(id)
            .map_deferred(|d| d.into_future("raw IP socket", &weak))
            .into_future()
            .await;
    }
}

/// On Error, logs the `Errno` with additional debugging context.
///
/// The provided result is passed through unchanged.
///
/// Implemented as a macro to avoid erasing the callsite information.
macro_rules! maybe_log_error {
    ($operation:expr, $result:expr) => {
        match $result {
            Ok(val) => Ok(val),
            Err(errno) => {
                crate::bindings::socket::log_errno!(
                    errno,
                    "raw IP socket failed to handle {}: {:?}",
                    $operation,
                    errno
                );
                Err(errno)
            }
        }
    };
}

struct RequestHandler<'a, I: IpExt> {
    ctx: &'a Ctx,
    data: &'a mut SocketWorkerState<I>,
}

impl<'a, I: IpExt + IpSockAddrExt> RequestHandler<'a, I> {
    fn describe(
        SocketWorkerState { peer_event, id: _ }: &SocketWorkerState<I>,
    ) -> fpraw::SocketDescribeResponse {
        let peer = peer_event
            .duplicate_handle(
                // The client only needs to be able to receive signals so don't
                // allow it to set signals.
                zx::Rights::BASIC,
            )
            .expect("failed to duplicate handle");
        fpraw::SocketDescribeResponse { event: Some(peer), ..Default::default() }
    }

    fn handle_request(
        self,
        request: fpraw::SocketRequest,
    ) -> std::ops::ControlFlow<fpraw::SocketCloseResponder, Option<fpraw::SocketRequestStream>>
    {
        let Self { ctx, data } = self;
        match request {
            fpraw::SocketRequest::Clone2 { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel());
                let stream = fpraw::SocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fpraw::SocketRequest::Describe { responder } => {
                responder
                    .send(Self::describe(data))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fpraw::SocketRequest::Close { responder } => return ControlFlow::Break(responder),
            fpraw::SocketRequest::Query { responder } => responder
                .send(fpraw::SOCKET_PROTOCOL_NAME.as_bytes())
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetReuseAddress { value: _, responder } => {
                respond_not_supported!("raw::SetReuseAddress", responder)
            }
            fpraw::SocketRequest::GetReuseAddress { responder } => {
                respond_not_supported!("raw::GetReuseAddress", responder)
            }
            fpraw::SocketRequest::GetError { responder } => {
                respond_not_supported!("raw::GetError", responder)
            }
            fpraw::SocketRequest::SetBroadcast { value: _, responder } => {
                respond_not_supported!("raw::SetBroadcast", responder)
            }
            fpraw::SocketRequest::GetBroadcast { responder } => {
                respond_not_supported!("raw::GetBroadcast", responder)
            }
            fpraw::SocketRequest::SetSendBuffer { value_bytes: _, responder } => {
                respond_not_supported!("raw::SetSendBuffer", responder)
            }
            fpraw::SocketRequest::GetSendBuffer { responder } => {
                respond_not_supported!("raw::GetSendBuffer", responder)
            }
            fpraw::SocketRequest::SetReceiveBuffer { value_bytes: _, responder } => {
                respond_not_supported!("raw::SetReceiveBuffer", responder)
            }
            fpraw::SocketRequest::GetReceiveBuffer { responder } => {
                respond_not_supported!("raw::GetReceiveBuffer", responder)
            }
            fpraw::SocketRequest::SetKeepAlive { value: _, responder } => {
                respond_not_supported!("raw::SetKeepAlive", responder)
            }
            fpraw::SocketRequest::GetKeepAlive { responder } => {
                respond_not_supported!("raw::GetKeepAlive", responder)
            }
            fpraw::SocketRequest::SetOutOfBandInline { value: _, responder } => {
                respond_not_supported!("raw::SetOutOfBandInline", responder)
            }
            fpraw::SocketRequest::GetOutOfBandInline { responder } => {
                respond_not_supported!("raw::GetOutOfBandInline", responder)
            }
            fpraw::SocketRequest::SetNoCheck { value: _, responder } => {
                respond_not_supported!("raw::SetNoCheck", responder)
            }
            fpraw::SocketRequest::GetNoCheck { responder } => {
                respond_not_supported!("raw::GetNoCheck", responder)
            }
            fpraw::SocketRequest::SetLinger { linger: _, length_secs: _, responder } => {
                respond_not_supported!("raw::SetLinger", responder)
            }
            fpraw::SocketRequest::GetLinger { responder } => {
                respond_not_supported!("raw::GetLinger", responder)
            }
            fpraw::SocketRequest::SetReusePort { value: _, responder } => {
                respond_not_supported!("raw::SetReusePort", responder)
            }
            fpraw::SocketRequest::GetReusePort { responder } => {
                respond_not_supported!("raw::GetReusePort", responder)
            }
            fpraw::SocketRequest::GetAcceptConn { responder } => {
                respond_not_supported!("raw::GetAcceptConn", responder)
            }
            fpraw::SocketRequest::SetBindToDevice { value: _, responder } => {
                respond_not_supported!("raw::SetBindToDevice", responder)
            }
            fpraw::SocketRequest::GetBindToDevice { responder } => {
                respond_not_supported!("raw::GetBindToDevice", responder)
            }
            fpraw::SocketRequest::SetBindToInterfaceIndex { value: _, responder } => {
                respond_not_supported!("raw::SetBindToInterfaceIndex", responder)
            }
            fpraw::SocketRequest::GetBindToInterfaceIndex { responder } => {
                respond_not_supported!("raw::GetBindToInterfaceIndex", responder)
            }
            fpraw::SocketRequest::SetTimestamp { value: _, responder } => {
                respond_not_supported!("raw::SetTimestamp", responder)
            }
            fpraw::SocketRequest::GetTimestamp { responder } => {
                respond_not_supported!("raw::GetTimestamp", responder)
            }
            fpraw::SocketRequest::Bind { addr: _, responder } => {
                respond_not_supported!("raw::Bind", responder)
            }
            fpraw::SocketRequest::Connect { addr: _, responder } => {
                respond_not_supported!("raw::Connect", responder)
            }
            fpraw::SocketRequest::Disconnect { responder } => {
                respond_not_supported!("raw::Disconnect", responder)
            }
            fpraw::SocketRequest::GetSockName { responder } => {
                respond_not_supported!("raw::GetSockName", responder)
            }
            fpraw::SocketRequest::GetPeerName { responder } => {
                respond_not_supported!("raw::GetPeerName", responder)
            }
            fpraw::SocketRequest::Shutdown { mode: _, responder } => {
                respond_not_supported!("raw::Shutdown", responder)
            }
            fpraw::SocketRequest::SetIpTypeOfService { value: _, responder } => {
                respond_not_supported!("raw::SetIpTypeOfService", responder)
            }
            fpraw::SocketRequest::GetIpTypeOfService { responder } => {
                respond_not_supported!("raw::GetIpTypeOfService", responder)
            }
            fpraw::SocketRequest::SetIpTtl { value: _, responder } => {
                respond_not_supported!("raw::SetIpTtl", responder)
            }
            fpraw::SocketRequest::GetIpTtl { responder } => {
                respond_not_supported!("raw::GetIpTtl", responder)
            }
            fpraw::SocketRequest::SetIpPacketInfo { value: _, responder } => {
                respond_not_supported!("raw::SetIpPacketInfo", responder)
            }
            fpraw::SocketRequest::GetIpPacketInfo { responder } => {
                respond_not_supported!("raw::GetIpPacketInfo", responder)
            }
            fpraw::SocketRequest::SetIpReceiveTypeOfService { value: _, responder } => {
                respond_not_supported!("raw::SetIpReceiveTypeOfService", responder)
            }
            fpraw::SocketRequest::GetIpReceiveTypeOfService { responder } => {
                respond_not_supported!("raw::GetIpReceiveTypeOfService", responder)
            }
            fpraw::SocketRequest::SetIpReceiveTtl { value: _, responder } => {
                respond_not_supported!("raw::SetIpReceiveTtl", responder)
            }
            fpraw::SocketRequest::GetIpReceiveTtl { responder } => {
                respond_not_supported!("raw::GetIpReceiveTtl", responder)
            }
            fpraw::SocketRequest::SetIpMulticastInterface { iface: _, address: _, responder } => {
                respond_not_supported!("raw::SetIpMulticastInterface", responder)
            }
            fpraw::SocketRequest::GetIpMulticastInterface { responder } => {
                respond_not_supported!("raw::GetIpMulticastInterface", responder)
            }
            fpraw::SocketRequest::SetIpMulticastTtl { value: _, responder } => {
                respond_not_supported!("raw::SetIpMulticastTtl", responder)
            }
            fpraw::SocketRequest::GetIpMulticastTtl { responder } => {
                respond_not_supported!("raw::GetIpMulticastTtl", responder)
            }
            fpraw::SocketRequest::SetIpMulticastLoopback { value: _, responder } => {
                respond_not_supported!("raw::SetIpMulticastLoopback", responder)
            }
            fpraw::SocketRequest::GetIpMulticastLoopback { responder } => {
                respond_not_supported!("raw::GetIpMulticastLoopback", responder)
            }
            fpraw::SocketRequest::AddIpMembership { membership: _, responder } => {
                respond_not_supported!("raw::AddIpMembership", responder)
            }
            fpraw::SocketRequest::DropIpMembership { membership: _, responder } => {
                respond_not_supported!("raw::DropIpMembership", responder)
            }
            fpraw::SocketRequest::SetIpTransparent { value: _, responder } => {
                respond_not_supported!("raw::SetIpTransparent", responder)
            }
            fpraw::SocketRequest::GetIpTransparent { responder } => {
                respond_not_supported!("raw::GetIpTransparent", responder)
            }
            fpraw::SocketRequest::SetIpReceiveOriginalDestinationAddress {
                value: _,
                responder,
            } => respond_not_supported!("raw::SetIpReceiveOriginalDestinationAddress", responder),
            fpraw::SocketRequest::GetIpReceiveOriginalDestinationAddress { responder } => {
                respond_not_supported!("raw::GetIpReceiveOriginalDestinationAddress", responder)
            }
            fpraw::SocketRequest::AddIpv6Membership { membership: _, responder } => {
                respond_not_supported!("raw::AddIpv6Membership", responder)
            }
            fpraw::SocketRequest::DropIpv6Membership { membership: _, responder } => {
                respond_not_supported!("raw::DropIpv6Membership", responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastInterface { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6MulticastInterface", responder)
            }
            fpraw::SocketRequest::GetIpv6MulticastInterface { responder } => {
                respond_not_supported!("raw::GetIpv6MulticastInterface", responder)
            }
            fpraw::SocketRequest::SetIpv6UnicastHops { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6UnicastHops", responder)
            }
            fpraw::SocketRequest::GetIpv6UnicastHops { responder } => {
                respond_not_supported!("raw::GetIpv6UnicastHops", responder)
            }
            fpraw::SocketRequest::SetIpv6ReceiveHopLimit { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6ReceiveHopLimit", responder)
            }
            fpraw::SocketRequest::GetIpv6ReceiveHopLimit { responder } => {
                respond_not_supported!("raw::GetIpv6ReceiveHopLimit", responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastHops { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6MulticastHops", responder)
            }
            fpraw::SocketRequest::GetIpv6MulticastHops { responder } => {
                respond_not_supported!("raw::GetIpv6MulticastHops", responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastLoopback { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6MulticastLoopback", responder)
            }
            fpraw::SocketRequest::GetIpv6MulticastLoopback { responder } => {
                respond_not_supported!("raw::GetIpv6MulticastLoopback", responder)
            }
            fpraw::SocketRequest::SetIpv6Only { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6Only", responder)
            }
            fpraw::SocketRequest::GetIpv6Only { responder } => {
                respond_not_supported!("raw::GetIpv6Only", responder)
            }
            fpraw::SocketRequest::SetIpv6ReceiveTrafficClass { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6ReceiveTrafficClass", responder)
            }
            fpraw::SocketRequest::GetIpv6ReceiveTrafficClass { responder } => {
                respond_not_supported!("raw::GetIpv6ReceiveTrafficClass", responder)
            }
            fpraw::SocketRequest::SetIpv6TrafficClass { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6TrafficClass", responder)
            }
            fpraw::SocketRequest::GetIpv6TrafficClass { responder } => {
                respond_not_supported!("raw::GetIpv6TrafficClass", responder)
            }
            fpraw::SocketRequest::SetIpv6ReceivePacketInfo { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6ReceivePacketInfo", responder)
            }
            fpraw::SocketRequest::GetIpv6ReceivePacketInfo { responder } => {
                respond_not_supported!("raw::GetIpv6ReceivePacketInfo", responder)
            }
            fpraw::SocketRequest::GetOriginalDestination { responder } => {
                respond_not_supported!("raw::GetOriginalDestination", responder)
            }
            fpraw::SocketRequest::RecvMsg {
                want_addr,
                data_len,
                want_control,
                flags,
                responder,
            } => {
                let result = maybe_log_error!(
                    "recvmsg",
                    handle_recvmsg(ctx, data, want_addr, data_len, want_control, flags)
                );
                responder
                    .send(result.as_ref().map(RecvMsgResponse::borrowed).map_err(|e| *e))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"))
            }
            fpraw::SocketRequest::SendMsg { addr: _, data: _, control: _, flags: _, responder } => {
                respond_not_supported!("raw::SendMsg", responder)
            }
            fpraw::SocketRequest::GetInfo { responder } => {
                respond_not_supported!("raw::GetInfo", responder)
            }
            fpraw::SocketRequest::SetIpHeaderIncluded { value: _, responder } => {
                respond_not_supported!("raw::SetIpHeaderIncluded", responder)
            }
            fpraw::SocketRequest::GetIpHeaderIncluded { responder } => {
                respond_not_supported!("raw::GetIpHeaderIncluded", responder)
            }
            fpraw::SocketRequest::SetIcmpv6Filter { filter: _, responder } => {
                respond_not_supported!("raw::SetIcmpv6Filter", responder)
            }
            fpraw::SocketRequest::GetIcmpv6Filter { responder } => {
                respond_not_supported!("raw::GetIcmpv6Filter", responder)
            }
            fpraw::SocketRequest::SetIpv6Checksum { config: _, responder } => {
                respond_not_supported!("raw::SetIpv6Checksum", responder)
            }
            fpraw::SocketRequest::GetIpv6Checksum { responder } => {
                respond_not_supported!("raw::GetIpv6Checksum", responder)
            }
            fpraw::SocketRequest::SetMark { domain: _, mark: _, responder } => {
                // TODO(https://fxbug.dev/337134565): Implement socket marks.
                respond_not_supported!("raw::SetMark", responder)
            }
            fpraw::SocketRequest::GetMark { domain: _, responder } => {
                // TODO(https://fxbug.dev/337134565): Implement socket marks.
                respond_not_supported!("raw::GetMark", responder)
            }
        }
        ControlFlow::Continue(None)
    }
}

pub(crate) async fn serve(
    ctx: Ctx,
    stream: fpraw::ProviderRequestStream,
) -> crate::bindings::util::TaskWaitGroup {
    let ctx = &ctx;
    let (wait_group, spawner) = crate::bindings::util::TaskWaitGroup::new();
    let spawner: worker::ProviderScopedSpawner<_> = spawner.into();
    stream
        .map(|req| {
            let req = match req {
                Ok(req) => req,
                Err(e) => {
                    if !e.is_closed() {
                        tracing::error!(
                            "{} request error {e:?}",
                            fpraw::ProviderMarker::DEBUG_NAME
                        );
                    }
                    return;
                }
            };
            match req {
                fpraw::ProviderRequest::Socket { responder, domain, proto } => responder
                    .send(maybe_log_error!(
                        "create",
                        handle_create_socket(ctx.clone(), domain, proto, &spawner)
                    ))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            }
        })
        .collect::<()>()
        .await;
    wait_group
}

/// Handler for a [`fpraw::ProviderRequest::Socket`] request.
fn handle_create_socket(
    ctx: Ctx,
    domain: fposix_socket::Domain,
    protocol: fpraw::ProtocolAssociation,
    spawner: &worker::ProviderScopedSpawner<crate::bindings::util::TaskWaitGroupSpawner>,
) -> Result<fidl::endpoints::ClientEnd<fpraw::SocketMarker>, fposix::Errno> {
    let (client, request_stream) =
        fidl::endpoints::create_request_stream().expect("failed to create a new request stream");
    match domain {
        fposix_socket::Domain::Ipv4 => {
            let protocol = RawIpSocketProtocol::<Ipv4>::try_from_fidl(protocol)?;
            spawner.spawn(SocketWorker::serve_stream_with(
                ctx,
                move |ctx, properties| SocketWorkerState::new(ctx, protocol, properties),
                SocketWorkerProperties {},
                request_stream,
                (),
                spawner.clone(),
            ))
        }
        fposix_socket::Domain::Ipv6 => {
            let protocol = RawIpSocketProtocol::<Ipv6>::try_from_fidl(protocol)?;
            spawner.spawn(SocketWorker::serve_stream_with(
                ctx,
                move |ctx, properties| SocketWorkerState::new(ctx, protocol, properties),
                SocketWorkerProperties {},
                request_stream,
                (),
                spawner.clone(),
            ))
        }
    }
    Ok(client)
}

/// The response for a successful call to `recvmsg`.
struct RecvMsgResponse {
    src_addr: Option<fnet::SocketAddress>,
    data: Vec<u8>,
    control: fposix_socket::NetworkSocketRecvControlData,
    truncated_bytes: u32,
}

impl RecvMsgResponse {
    /// Provide a form compatible with [`fpraw::SocketRecvMsgResponder`].
    fn borrowed(
        &self,
    ) -> (Option<&fnet::SocketAddress>, &[u8], &fposix_socket::NetworkSocketRecvControlData, u32)
    {
        let RecvMsgResponse { src_addr, data, control, truncated_bytes } = self;
        (src_addr.as_ref(), data.as_ref(), &control, *truncated_bytes)
    }
}

/// Handler for a [`fpraw::SocketRequest::RecvMsg`] request.
fn handle_recvmsg<I: IpExt + IpSockAddrExt>(
    ctx: &Ctx,
    socket: &SocketWorkerState<I>,
    want_addr: bool,
    max_len: u32,
    want_control: bool,
    flags: fposix_socket::RecvMsgFlags,
) -> Result<RecvMsgResponse, fposix::Errno> {
    // TODO(https://fxbug.dev/42175797): Support control data & flags.
    let _want_control = want_control;
    let _flags = flags;

    let ReceivedIpPacket { mut data, src_addr, device } =
        socket.id.external_state().dequeue_rx_packet().ok_or(fposix::Errno::Eagain)?;
    let data_len: usize = max_len.try_into().unwrap_or(usize::MAX);
    let truncated_bytes: u32 = data.len().saturating_sub(data_len).try_into().unwrap_or(u32::MAX);
    data.truncate(data_len);

    let src_addr = want_addr.then(|| {
        let src_addr = SpecifiedAddr::new(src_addr).map(|addr| {
            StrictlyZonedAddr::new_with_zone(addr, || {
                // Opt into infallible conversion to a `BindingId`. We don't
                // care if the device this packet was received on has since been
                // removed.
                AllowBindingIdFromWeak(device)
                    .try_into_fidl_with_ctx(ctx.bindings_ctx())
                    .unwrap_or_else(|never| match never {})
            })
            .into_inner()
        });
        // NB: raw IP sockets always set the source port to 0, since they
        // operate at the network layer.
        const RAW_IP_PORT_NUM: u16 = 0;
        I::SocketAddress::new(src_addr, RAW_IP_PORT_NUM).into_sock_addr()
    });

    Ok(RecvMsgResponse {
        src_addr,
        data,
        control: fposix_socket::NetworkSocketRecvControlData::new_empty(),
        truncated_bytes,
    })
}

impl<I: IpExt> TryFromFidl<fpraw::ProtocolAssociation> for RawIpSocketProtocol<I> {
    type Error = fposix::Errno;

    fn try_from_fidl(proto: fpraw::ProtocolAssociation) -> Result<Self, Self::Error> {
        match proto {
            fpraw::ProtocolAssociation::Unassociated(fpraw::Empty) => Ok(RawIpSocketProtocol::Raw),
            fpraw::ProtocolAssociation::Associated(val) => {
                let protocol = RawIpSocketProtocol::<I>::new(I::Proto::from(val));
                match &protocol {
                    RawIpSocketProtocol::Raw => Err(fposix::Errno::Einval),
                    RawIpSocketProtocol::Proto(_) => Ok(protocol),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ip_test_macro::ip_test;
    use packet_formats::ip::IpProto;
    use test_case::test_case;

    #[ip_test]
    #[test_case(
        fpraw::ProtocolAssociation::Unassociated(fpraw::Empty),
        Ok(RawIpSocketProtocol::Raw);
        "unassociated"
    )]
    #[test_case(
        fpraw::ProtocolAssociation::Associated(255),
        Err(fposix::Errno::Einval);
        "associated_with_iana_reserved_protocol"
    )]
    fn raw_protocol_from_fidl<I: Ip + IpExt>(
        fidl: fpraw::ProtocolAssociation,
        expected_result: Result<RawIpSocketProtocol<I>, fposix::Errno>,
    ) {
        assert_eq!(RawIpSocketProtocol::<I>::try_from_fidl(fidl), expected_result);
    }

    #[ip_test]
    #[test_case(fpraw::ProtocolAssociation::Associated(6), IpProto::Tcp)]
    #[test_case(fpraw::ProtocolAssociation::Associated(17), IpProto::Udp)]
    fn protocol_from_fidl<I: Ip + IpExt>(
        fidl: fpraw::ProtocolAssociation,
        expected_proto: IpProto,
    ) {
        assert_eq!(
            RawIpSocketProtocol::<I>::try_from_fidl(fidl)
                .expect("conversion should succeed")
                .proto(),
            expected_proto.into(),
        );
    }
}
