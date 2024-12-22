// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::num::NonZeroU8;
use std::ops::ControlFlow;

use fidl::endpoints::{DiscoverableProtocolMarker as _, ProtocolMarker, RequestStream};
use futures::StreamExt as _;
use log::error;
use net_types::ip::{Ip, IpInvariant, IpVersion, Ipv4, Ipv6};
use net_types::SpecifiedAddr;
use netstack3_core::ip::{
    IpSockCreateAndSendError, IpSockSendError, RawIpSocketIcmpFilter, RawIpSocketIcmpFilterError,
    RawIpSocketProtocol, RawIpSocketSendToError, RawIpSocketsBindingsContext,
    RawIpSocketsBindingsTypes,
};
use netstack3_core::socket::StrictlyZonedAddr;
use netstack3_core::sync::Mutex;
use netstack3_core::IpExt;
use packet::Buf;
use packet_formats::ip::IpPacket as _;
use zerocopy::SplitByteSlice;
use zx::{HandleBased, Peered};
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_posix as fposix,
    fidl_fuchsia_posix_socket as fposix_socket, fidl_fuchsia_posix_socket_raw as fpraw,
};

use crate::bindings::socket::queue::{BodyLen, MessageQueue};
use crate::bindings::socket::{IntoErrno, IpSockAddrExt, SockAddr};
use crate::bindings::util::{
    AllowBindingIdFromWeak, IntoCore, IntoFidl, IntoFidlWithContext as _, RemoveResourceResultExt,
    ResultExt as _, TryFromFidl, TryFromFidlWithContext,
};
use crate::bindings::{BindingsCtx, Ctx};

use super::worker::{
    self, CloseResponder, SocketWorker, SocketWorkerHandler, TaskSpawnerCollection,
};
use super::{SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING};

type DeviceId = netstack3_core::device::DeviceId<BindingsCtx>;
type WeakDeviceId = netstack3_core::device::WeakDeviceId<BindingsCtx>;
type RawIpSocketId<I> = netstack3_core::ip::RawIpSocketId<I, WeakDeviceId, BindingsCtx>;

impl RawIpSocketsBindingsTypes for BindingsCtx {
    type RawIpSocketState<I: Ip> = SocketState<I>;
}

impl<I: IpExt> RawIpSocketsBindingsContext<I, DeviceId> for BindingsCtx {
    fn receive_packet<B: SplitByteSlice>(
        &self,
        socket: &RawIpSocketId<I>,
        packet: &I::Packet<B>,
        device: &DeviceId,
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

    fn enqueue_rx_packet<B: SplitByteSlice>(&self, packet: &I::Packet<B>, device: WeakDeviceId) {
        // NB: Perform the expensive tasks before taking the message queue lock.
        let packet = ReceivedIpPacket::new::<B>(packet, device);
        self.rx_queue.lock().receive(packet);
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
    device: WeakDeviceId,
}

impl<I: IpExt> ReceivedIpPacket<I> {
    fn new<B: SplitByteSlice>(packet: &I::Packet<B>, device: WeakDeviceId) -> Self {
        // NB: Match Linux, and only provide the packet header for IPv4.
        let data = match I::VERSION {
            IpVersion::V4 => packet.to_vec(),
            IpVersion::V6 => packet.body().to_vec(),
        };
        ReceivedIpPacket { src_addr: packet.src_ip(), data, device }
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
    id: RawIpSocketId<I>,
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

    async fn handle_request(
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

struct RequestHandler<'a, I: IpExt> {
    ctx: &'a mut Ctx,
    data: &'a mut SocketWorkerState<I>,
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
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
            fpraw::SocketRequest::Clone { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel());
                let stream = fpraw::SocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fpraw::SocketRequest::Describe { responder } => {
                responder.send(Self::describe(data)).unwrap_or_log("failed to respond");
            }
            fpraw::SocketRequest::Close { responder } => return ControlFlow::Break(responder),
            fpraw::SocketRequest::Query { responder } => responder
                .send(fpraw::SocketMarker::PROTOCOL_NAME.as_bytes())
                .unwrap_or_log("failed to respond"),
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
            fpraw::SocketRequest::SetBindToDevice { value, responder } => responder
                .send(handle_set_device::<I>(ctx, data, value).log_error("raw::SetBindToDevice"))
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetBindToDevice { responder } => {
                let name = handle_get_device(ctx, data);
                responder.send(Ok(name.as_deref().unwrap_or(""))).unwrap_or_log("failed to respond")
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
            fpraw::SocketRequest::SetIpTtl { value, responder } => responder
                .send(
                    handle_set_hop_limit(ctx, data, HopLimitType::Unicast, IpVersion::V4, value)
                        .log_error("raw::SetIpTtl"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIpTtl { responder } => responder
                .send(
                    handle_get_hop_limit(ctx, data, HopLimitType::Unicast, IpVersion::V4)
                        .log_error("raw::GetIpTtl"),
                )
                .unwrap_or_log("failed to respond"),
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
            fpraw::SocketRequest::SetIpMulticastTtl { value, responder } => responder
                .send(
                    handle_set_hop_limit(ctx, data, HopLimitType::Multicast, IpVersion::V4, value)
                        .log_error("raw::SetIpMulticastTtl"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIpMulticastTtl { responder } => responder
                .send(
                    handle_get_hop_limit(ctx, data, HopLimitType::Multicast, IpVersion::V4)
                        .log_error("raw::GetIpMulticastTtl"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::SetIpMulticastLoopback { value, responder } => responder
                .send(
                    handle_set_multicast_loop(ctx, data, value, IpVersion::V4)
                        .log_error("raw::SetIpMulticastLoopback"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIpMulticastLoopback { responder } => responder
                .send(
                    handle_get_multicast_loop(ctx, data, IpVersion::V4)
                        .log_error("raw::GetIpMulticastLoopback"),
                )
                .unwrap_or_log("failed to respond"),
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
            fpraw::SocketRequest::SetIpv6UnicastHops { value, responder } => responder
                .send(
                    handle_set_hop_limit(ctx, data, HopLimitType::Unicast, IpVersion::V6, value)
                        .log_error("raw::SetIpv6UnicastHops"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIpv6UnicastHops { responder } => responder
                .send(
                    handle_get_hop_limit(ctx, data, HopLimitType::Unicast, IpVersion::V6)
                        .log_error("raw::GetIpv6UnicastHops"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::SetIpv6ReceiveHopLimit { value: _, responder } => {
                respond_not_supported!("raw::SetIpv6ReceiveHopLimit", responder)
            }
            fpraw::SocketRequest::GetIpv6ReceiveHopLimit { responder } => {
                respond_not_supported!("raw::GetIpv6ReceiveHopLimit", responder)
            }
            fpraw::SocketRequest::SetIpv6MulticastHops { value, responder } => responder
                .send(
                    handle_set_hop_limit(ctx, data, HopLimitType::Multicast, IpVersion::V6, value)
                        .log_error("raw::SetIpv6MulticastHops"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIpv6MulticastHops { responder } => responder
                .send(
                    handle_get_hop_limit(ctx, data, HopLimitType::Multicast, IpVersion::V6)
                        .log_error("raw::GetIpv6MulticastHops"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::SetIpv6MulticastLoopback { value, responder } => responder
                .send(
                    handle_set_multicast_loop(ctx, data, value, IpVersion::V6)
                        .log_error("raw::SetIpv6MulticastLoopback"),
                )
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIpv6MulticastLoopback { responder } => responder
                .send(
                    handle_get_multicast_loop(ctx, data, IpVersion::V6)
                        .log_error("raw::GetIpv6MulticastLoopback"),
                )
                .unwrap_or_log("failed to respond"),
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
                let result = handle_recvmsg(ctx, data, want_addr, data_len, want_control, flags)
                    .log_error("raw::RecvMsg");
                responder
                    .send(result.as_ref().map(RecvMsgResponse::borrowed).map_err(|e| *e))
                    .unwrap_or_log("failed to respond")
            }
            fpraw::SocketRequest::SendMsg { addr, data: msg, control, flags, responder } => {
                responder
                    .send(
                        handle_sendmsg(ctx, data, addr, msg, control, flags)
                            .log_error("raw::SendMsg"),
                    )
                    .unwrap_or_log("failed to respond")
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
            fpraw::SocketRequest::SetIcmpv6Filter { filter, responder } => responder
                .send(handle_set_icmpv6_filter(ctx, data, filter).log_error("raw::SetIcmpv6Filter"))
                .unwrap_or_log("failed to respond"),
            fpraw::SocketRequest::GetIcmpv6Filter { responder } => {
                let result = handle_get_icmpv6_filter(ctx, data).log_error("raw::GetIcmpv6Filter");
                responder.send(result.as_ref().map_err(|e| *e)).unwrap_or_log("failed to respond");
            }
            fpraw::SocketRequest::SetIpv6Checksum { config: _, responder } => {
                respond_not_supported!("raw::SetIpv6Checksum", responder)
            }
            fpraw::SocketRequest::GetIpv6Checksum { responder } => {
                respond_not_supported!("raw::GetIpv6Checksum", responder)
            }
            fpraw::SocketRequest::SetMark { domain, mark, responder } => {
                ctx.api().raw_ip_socket().set_mark(&data.id, domain.into_core(), mark.into_core());
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
            fpraw::SocketRequest::GetMark { domain, responder } => {
                let mark =
                    ctx.api().raw_ip_socket().get_mark(&data.id, domain.into_core()).into_fidl();
                responder.send(Ok(&mark)).unwrap_or_log("failed to respond");
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
                        error!("{} request error {e:?}", fpraw::ProviderMarker::DEBUG_NAME);
                    }
                    return;
                }
            };
            match req {
                fpraw::ProviderRequest::Socket { responder, domain, proto } => responder
                    .send(
                        handle_create_socket(ctx.clone(), domain, proto, &spawner)
                            .log_error("raw::create"),
                    )
                    .unwrap_or_log("failed to respond"),
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
    let (client, request_stream) = fidl::endpoints::create_request_stream();
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
                AllowBindingIdFromWeak(device).into_fidl_with_ctx(ctx.bindings_ctx())
            })
            .into_inner()
        });
        // NB: raw IP sockets always set the source port to 0, since they
        // operate at the network layer.
        const RAW_IP_PORT_NUM: u16 = 0;
        I::SocketAddress::new(src_addr, RAW_IP_PORT_NUM).into_sock_addr()
    });

    Ok(RecvMsgResponse { src_addr, data, control: Default::default(), truncated_bytes })
}

/// Handler for a [`fpraw::SocketRequest::SendMsg`] request.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_sendmsg<I: IpExt + IpSockAddrExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    addr: Option<Box<fnet::SocketAddress>>,
    data: Vec<u8>,
    control: fposix_socket::NetworkSocketSendControlData,
    flags: fposix_socket::SendMsgFlags,
) -> Result<(), fposix::Errno> {
    // TODO(https://fxbug.dev/42175797): Support control data & flags.
    let _control = control;
    let _flags = flags;

    let remote_addr = addr
        .map(|addr| {
            // Match Linux and return `Einval` when asked to send to an IPv4
            // addr on an IPv6 socket. This errno is unique to raw IP sockets.
            let addr = match (I::VERSION, *addr) {
                (IpVersion::V6, fnet::SocketAddress::Ipv4(_)) => Err(fposix::Errno::Einval),
                (_, _) => I::SocketAddress::from_sock_addr(*addr),
            }?;
            // NB: raw IP sockets ignore the port.
            let (remote_addr, _port) =
                TryFromFidlWithContext::try_from_fidl_with_ctx(ctx.bindings_ctx(), addr)
                    .map_err(IntoErrno::into_errno)?;
            Ok(remote_addr)
        })
        .transpose()?;
    let body = Buf::new(data, ..);
    match remote_addr {
        Some(remote_addr) => ctx
            .api()
            .raw_ip_socket()
            .send_to(&socket.id, remote_addr, body)
            .map_err(IntoErrno::into_errno),
        None => {
            // TODO(https://fxbug.dev/342577389): Support sending on connected
            // sockets.
            // TODO(https://fxbug.dev/339692009): Support sending when the
            // remote_ip is provided via IP_HDRINCL.
            Err(fposix::Errno::Edestaddrreq)
        }
    }
}

/// Handler for a [`fpraw::SocketRequest::SetBindToDevice`] request.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_set_device<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    device_name: String,
) -> Result<(), fposix::Errno> {
    let device = if device_name.is_empty() {
        None
    } else {
        Some(
            ctx.bindings_ctx()
                .devices
                .get_device_by_name(device_name.as_str())
                .ok_or(fposix::Errno::Enodev)?,
        )
    };
    let _old_dev: Option<WeakDeviceId> =
        ctx.api().raw_ip_socket().set_device(&socket.id, device.as_ref());
    Ok(())
}

/// Handler for a [`fpraw::SocketRequest::GetBindToDevice`] request.
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_get_device<I: IpExt>(ctx: &mut Ctx, socket: &SocketWorkerState<I>) -> Option<String> {
    let device = ctx.api().raw_ip_socket().get_device(&socket.id);
    device.map(|core_id| core_id.bindings_id().name.clone())
}

/// Handler for a [`fpraw::SocketRequest::SetIcmpv6Filter`] request.
fn handle_set_icmpv6_filter<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    filter: fpraw::Icmpv6Filter,
) -> Result<(), fposix::Errno> {
    I::map_ip_in(
        (IpInvariant(filter), &socket.id),
        |_| Err(fposix::Errno::Enoprotoopt),
        |(IpInvariant(filter), id)| {
            let _old_filter = ctx
                .api()
                .raw_ip_socket()
                .set_icmp_filter(id, Some(filter.into_core()))
                .map_err(IntoErrno::into_errno)?;
            Ok(())
        },
    )
}

/// Handler for a [`fpraw::SocketRequest::GetIcmpv6Filter`] request.
fn handle_get_icmpv6_filter<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
) -> Result<fpraw::Icmpv6Filter, fposix::Errno> {
    I::map_ip_in(
        (IpInvariant(ctx), &socket.id),
        |_| Err(fposix::Errno::Enoprotoopt),
        |(IpInvariant(ctx), id)| match ctx.api().raw_ip_socket().get_icmp_filter(id) {
            Err(e) => Err(e.into_errno()),
            Ok(filter) => Ok(filter.into_fidl()),
        },
    )
}

/// Handler for the following requests:
///  - [`fpraw::SocketRequest::SetIpMulticastLoop`]
///  - [`fpraw::SocketRequest::SetIpv6MulticastLoop`]
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_set_multicast_loop<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    value: bool,
    ip_version: IpVersion,
) -> Result<(), fposix::Errno> {
    if I::VERSION != ip_version {
        return Err(fposix::Errno::Enoprotoopt);
    }
    let _old_value: bool = ctx.api().raw_ip_socket().set_multicast_loop(&socket.id, value);
    Ok(())
}

/// Handler for the following requests:
///  - [`fpraw::SocketRequest::GetIpMulticastLoop`]
///  - [`fpraw::SocketRequest::GetIpv6MulticastLoop`]
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_get_multicast_loop<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    ip_version: IpVersion,
) -> Result<bool, fposix::Errno> {
    if I::VERSION != ip_version {
        return Err(fposix::Errno::Enoprotoopt);
    }

    Ok(ctx.api().raw_ip_socket().get_multicast_loop(&socket.id))
}

/// The type of hop limit to set/get.
enum HopLimitType {
    Unicast,
    Multicast,
}

/// Handler for the following requests:
///  - [`fpraw::SocketRequest::SetIpTtl`]
///  - [`fpraw::SocketRequest::SetIpMulticastTtl`]
///  - [`fpraw::SocketRequest::SetIpv6UnicastHops`]
///  - [`fpraw::SocketRequest::SetIpv6MulticastHops`]
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_set_hop_limit<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    hop_limit_type: HopLimitType,
    ip_version: IpVersion,
    value: fposix_socket::OptionalUint8,
) -> Result<(), fposix::Errno> {
    if I::VERSION != ip_version {
        return Err(fposix::Errno::Enoprotoopt);
    }
    let value: Option<u8> = value.into_core();
    let _old_value = match hop_limit_type {
        HopLimitType::Unicast => ctx
            .api()
            .raw_ip_socket()
            .set_unicast_hop_limit(&socket.id, value.and_then(NonZeroU8::new)),
        HopLimitType::Multicast => ctx
            .api()
            .raw_ip_socket()
            .set_multicast_hop_limit(&socket.id, value.and_then(NonZeroU8::new)),
    };
    Ok(())
}

/// Handler for the following requests:
///  - [`fpraw::SocketRequest::GetIpTtl`]
///  - [`fpraw::SocketRequest::GetIpMulticastTtl`]
///  - [`fpraw::SocketRequest::GetIpv6UnicastHops`]
///  - [`fpraw::SocketRequest::GetIpv6MulticastHops`]
#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
fn handle_get_hop_limit<I: IpExt>(
    ctx: &mut Ctx,
    socket: &SocketWorkerState<I>,
    hop_limit_type: HopLimitType,
    ip_version: IpVersion,
) -> Result<u8, fposix::Errno> {
    if I::VERSION != ip_version {
        return Err(fposix::Errno::Enoprotoopt);
    }
    match hop_limit_type {
        HopLimitType::Unicast => {
            Ok(ctx.api().raw_ip_socket().get_unicast_hop_limit(&socket.id).into())
        }
        HopLimitType::Multicast => {
            Ok(ctx.api().raw_ip_socket().get_multicast_hop_limit(&socket.id).into())
        }
    }
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

mod icmp_filter_conversion_constants {
    // NB: An ICMP filter is a 256 bit value where bit `n` determines if
    // ICMP messages with type `n` should be filtered. The FIDL types
    // represent this as 8 u32s, while the core types represent this as
    // 32 u8s.
    pub(super) const NUM_FIDL_ELEMENTS: usize = 8;
    pub(super) const NUM_CORE_ELEMENTS: usize = 32;
    pub(super) const CORE_FIDL_ELEMENT_RATIO: usize = NUM_CORE_ELEMENTS / NUM_FIDL_ELEMENTS;
}

impl IntoCore<RawIpSocketIcmpFilter<Ipv6>> for fpraw::Icmpv6Filter {
    fn into_core(self) -> RawIpSocketIcmpFilter<Ipv6> {
        use icmp_filter_conversion_constants::*;
        let mut core_filter = [0u8; NUM_CORE_ELEMENTS];
        // NB: Chunk the core_filter up so that each chunk has enough bytes
        // for a FIDL element.
        let filter_chunks = core_filter.chunks_exact_mut(CORE_FIDL_ELEMENT_RATIO);
        for (i, chunk) in filter_chunks.enumerate() {
            // NB: Use little endian byte order here, as returned by the
            // `RawIpSocketIcmpFilter` type.
            chunk.clone_from_slice(&self.blocked_types[i].to_le_bytes());
        }
        RawIpSocketIcmpFilter::new(core_filter)
    }
}

impl IntoFidl<fpraw::Icmpv6Filter> for Option<RawIpSocketIcmpFilter<Ipv6>> {
    fn into_fidl(self) -> fpraw::Icmpv6Filter {
        use icmp_filter_conversion_constants::*;
        let mut fidl_filter = fpraw::Icmpv6Filter { blocked_types: [0; NUM_FIDL_ELEMENTS] };
        if let Some(core_filter) = self {
            let core_filter: [u8; NUM_CORE_ELEMENTS] = core_filter.into_bytes();
            // NB: Chunk the core_filter up so that each chunk has enough bytes
            // for a FIDL element.
            let filter_chunks = core_filter.chunks_exact(CORE_FIDL_ELEMENT_RATIO);
            debug_assert!(filter_chunks.remainder().is_empty());
            for (i, chunk) in filter_chunks.enumerate() {
                // NB: The bytes from `core_filter` that correspond to the FIDL
                // element at index `i`.
                let bytes: [u8; CORE_FIDL_ELEMENT_RATIO] =
                    chunk.try_into().unwrap_or_else(|_: core::array::TryFromSliceError| {
                        unreachable!("the slice must have {CORE_FIDL_ELEMENT_RATIO} bytes")
                    });
                // NB: Use little endian byte order here, as returned by the
                // `RawIpSocketIcmpFilter` type.
                fidl_filter.blocked_types[i] = u32::from_le_bytes(bytes)
            }
        }
        fidl_filter
    }
}

impl IntoErrno for RawIpSocketSendToError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            RawIpSocketSendToError::ProtocolRaw => fposix::Errno::Einval,
            RawIpSocketSendToError::MappedRemoteIp => fposix::Errno::Enetunreach,
            RawIpSocketSendToError::InvalidBody => fposix::Errno::Einval,
            RawIpSocketSendToError::Ip(inner) => match inner {
                // MTU errors result in `Emsgsize` for sendto, but `Einval` for
                // send.
                IpSockCreateAndSendError::Send(IpSockSendError::Mtu) => fposix::Errno::Emsgsize,
                IpSockCreateAndSendError::Send(IpSockSendError::IllegalLoopbackAddress) => {
                    fposix::Errno::Einval
                }
                IpSockCreateAndSendError::Send(IpSockSendError::BroadcastNotAllowed) => {
                    fposix::Errno::Eacces
                }
                IpSockCreateAndSendError::Send(IpSockSendError::Unroutable(inner)) => {
                    inner.into_errno()
                }
                IpSockCreateAndSendError::Create(inner) => inner.into_errno(),
            },
            RawIpSocketSendToError::Zone(inner) => inner.into_errno(),
        }
    }
}

impl IntoErrno for RawIpSocketIcmpFilterError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            RawIpSocketIcmpFilterError::ProtocolNotIcmp => fposix::Errno::Eopnotsupp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ip_test_macro::ip_test;
    use packet_formats::ip::IpProto;
    use test_case::test_case;

    #[ip_test(I)]
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
    fn raw_protocol_from_fidl<I: IpExt>(
        fidl: fpraw::ProtocolAssociation,
        expected_result: Result<RawIpSocketProtocol<I>, fposix::Errno>,
    ) {
        assert_eq!(RawIpSocketProtocol::<I>::try_from_fidl(fidl), expected_result);
    }

    #[ip_test(I)]
    #[test_case(fpraw::ProtocolAssociation::Associated(6), IpProto::Tcp)]
    #[test_case(fpraw::ProtocolAssociation::Associated(17), IpProto::Udp)]
    fn protocol_from_fidl<I: IpExt>(fidl: fpraw::ProtocolAssociation, expected_proto: IpProto) {
        assert_eq!(
            RawIpSocketProtocol::<I>::try_from_fidl(fidl)
                .expect("conversion should succeed")
                .proto(),
            expected_proto.into(),
        );
    }

    #[test_case(
        [
            u32::from_le_bytes([0, 1, 2, 3]), u32::from_le_bytes([4, 5, 6, 7 ]),
            u32::from_le_bytes([8, 9, 10, 11]), u32::from_le_bytes([12, 13, 14, 15]),
            u32::from_le_bytes([16, 17, 18, 19]), u32::from_le_bytes([20, 21, 22, 23]),
            u32::from_le_bytes([24, 25, 26, 27]), u32::from_le_bytes([28, 29, 30, 31]),
        ],
        [
             0,  1,  2,  3,  4,  5,  6,  7,
             8,  9, 10, 11, 12, 13, 14, 15,
            16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31
        ]
    )]
    fn icmp_filter_to_from_fidl(fidl_raw: [u32; 8], core_raw: [u8; 32]) {
        let fidl = fpraw::Icmpv6Filter { blocked_types: fidl_raw };
        let core: RawIpSocketIcmpFilter<Ipv6> = fidl.into_core();
        assert_eq!(core.clone().into_bytes(), core_raw);
        assert_eq!(Some(core).into_fidl().blocked_types, fidl_raw);
    }

    #[test]
    fn icmp_filter_from_none() {
        let core_filter: Option<RawIpSocketIcmpFilter<Ipv6>> = None;
        assert_eq!(core_filter.into_fidl().blocked_types, [0; 8])
    }
}
