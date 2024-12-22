// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU16;
use std::ops::ControlFlow;

use {
    fidl_fuchsia_posix as fposix, fidl_fuchsia_posix_socket as fpsocket,
    fidl_fuchsia_posix_socket_packet as fppacket,
};

use fidl::endpoints::{DiscoverableProtocolMarker as _, ProtocolMarker as _, RequestStream as _};
use fidl::Peered as _;
use futures::StreamExt as _;
use log::{error, info, warn};
use net_types::ethernet::Mac;
use net_types::ip::IpVersion;
use netstack3_core::device::{
    DeviceId, EthernetLinkDevice, LoopbackDevice, PureIpDevice, PureIpHeaderParams, WeakDeviceId,
};
use netstack3_core::device_socket::{
    DeviceSocketBindingsContext, DeviceSocketMetadata, DeviceSocketTypes, EthernetFrame,
    EthernetHeaderParams, Frame, FrameDestination, IpFrame, Protocol, ReceivedFrame,
    SendFrameErrorReason, SentFrame, SocketId, SocketInfo, TargetDevice,
};
use netstack3_core::sync::{Mutex, RwLock};
use packet::Buf;
use packet_formats::ethernet::EtherType;
use zx::{self as zx, HandleBased as _};

use crate::bindings::bpf::{SocketFilterProgram, SocketFilterResult};
use crate::bindings::devices::BindingId;
use crate::bindings::socket::queue::{BodyLen, MessageQueue};
use crate::bindings::socket::worker::{self, CloseResponder, SocketWorker};
use crate::bindings::socket::{IntoErrno, SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING};
use crate::bindings::util::{
    DeviceNotFoundError, IntoCore as _, IntoFidl as _, RemoveResourceResultExt as _,
    ResultExt as _, TryFromFidl, TryIntoCoreWithContext as _, TryIntoFidlWithContext,
};
use crate::bindings::{BindingsCtx, Ctx};

/// State held in the bindings context for a single socket.
#[derive(Debug)]
pub(crate) struct SocketState {
    /// The received messages for the socket.
    queue: Mutex<MessageQueue<Message>>,
    kind: fppacket::Kind,
    bpf_filter: RwLock<Option<SocketFilterProgram>>,
}

impl DeviceSocketTypes for BindingsCtx {
    type SocketState = SocketState;
}

impl DeviceSocketBindingsContext<DeviceId<Self>> for BindingsCtx {
    fn receive_frame(
        &self,
        state: &Self::SocketState,
        device: &DeviceId<Self>,
        frame: Frame<&[u8]>,
        raw: &[u8],
    ) {
        let SocketState { queue, kind, bpf_filter } = state;

        // Run BPF filter if any. The filter may request the packet
        // to be truncated or dropped.
        let truncated_size = match bpf_filter.read().as_ref() {
            Some(program) => match program.run(*kind, frame, raw) {
                SocketFilterResult::Reject => return,
                SocketFilterResult::Accept(size) => size,
            },
            None => usize::MAX,
        };

        let data = MessageData::new(&frame, device);

        // NB: Perform the expensive tasks before taking the message queue lock.
        let body = match kind {
            fppacket::Kind::Network => frame.into_body(),
            fppacket::Kind::Link => raw,
        };
        let truncated_size = body.len().min(truncated_size);
        let body = body[..truncated_size].to_vec();
        let message = Message { data, body };
        queue.lock().receive(message);
    }
}

pub(crate) async fn serve(
    ctx: Ctx,
    stream: fppacket::ProviderRequestStream,
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
                        error!("{} request error {e:?}", fppacket::ProviderMarker::DEBUG_NAME);
                    }
                    return;
                }
            };
            match req {
                fppacket::ProviderRequest::Socket { responder, kind } => {
                    let (client, request_stream) = fidl::endpoints::create_request_stream();
                    spawner.spawn(SocketWorker::serve_stream_with(
                        ctx.clone(),
                        move |ctx, properties| BindingData::new(ctx, kind, properties),
                        SocketWorkerProperties {},
                        request_stream,
                        (),
                        spawner.clone(),
                    ));
                    responder.send(Ok(client)).unwrap_or_log("failed to respond");
                }
            }
        })
        .collect::<()>()
        .await;

    wait_group
}

#[derive(Debug)]
struct BindingData {
    /// The event to hand off for [`fppacket::SocketRequest::Describe`].
    peer_event: zx::EventPair,
    /// The identifier for the [`netstack3_core`] socket resource.
    id: SocketId<BindingsCtx>,
}

#[derive(Debug)]
struct Message {
    data: MessageData,
    body: Vec<u8>,
}

impl BodyLen for Message {
    fn body_len(&self) -> usize {
        let Self { data: _, body } = self;
        body.len()
    }
}

#[derive(Debug)]
struct MessageData {
    info: MessageDataInfo,
    interface_type: fppacket::HardwareType,
    interface_id: u64,
    packet_type: fppacket::PacketType,
}

#[derive(Debug)]
enum MessageDataInfo {
    Ethernet { src_mac: Mac, protocol: u16 },
    Ip { ip_version: IpVersion },
}

impl MessageData {
    fn new(frame: &Frame<&[u8]>, device: &DeviceId<BindingsCtx>) -> Self {
        let (packet_type, info) = match frame {
            Frame::Sent(sent) => (
                fppacket::PacketType::Outgoing,
                match sent {
                    SentFrame::Ethernet(frame) => MessageDataInfo::new_ethernet(frame),
                    SentFrame::Ip(frame) => MessageDataInfo::new_ip(frame),
                },
            ),
            Frame::Received(ReceivedFrame::Ethernet { destination, frame }) => {
                let packet_type = match destination {
                    FrameDestination::Broadcast => fppacket::PacketType::Broadcast,
                    FrameDestination::Multicast => fppacket::PacketType::Multicast,
                    FrameDestination::Individual { local } => local
                        .then_some(fppacket::PacketType::Host)
                        .unwrap_or(fppacket::PacketType::OtherHost),
                };
                (packet_type, MessageDataInfo::new_ethernet(frame))
            }
            Frame::Received(ReceivedFrame::Ip(frame)) => {
                (fppacket::PacketType::Host, MessageDataInfo::new_ip(frame))
            }
        };

        Self {
            packet_type,
            info,
            interface_id: device.bindings_id().id.get(),
            interface_type: iface_type(device),
        }
    }
}

impl MessageDataInfo {
    fn new_ethernet(frame: &EthernetFrame<&[u8]>) -> Self {
        let EthernetFrame { src_mac, dst_mac: _, ethertype, body: _ } = *frame;
        Self::Ethernet { src_mac, protocol: ethertype.map_or(0, Into::into) }
    }

    fn new_ip(frame: &IpFrame<&[u8]>) -> Self {
        let IpFrame { ip_version, body: _ } = frame;
        Self::Ip { ip_version: *ip_version }
    }
}

impl BindingData {
    fn new(
        ctx: &mut Ctx,
        kind: fppacket::Kind,
        SocketWorkerProperties {}: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        match local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            Ok(()) => (),
            Err(e) => error!("socket failed to signal peer: {:?}", e),
        }

        let id = ctx.api().device_socket().create(SocketState {
            queue: Mutex::new(MessageQueue::new(local_event)),
            kind,
            bpf_filter: RwLock::new(None),
        });

        BindingData { peer_event, id }
    }
}

impl CloseResponder for fppacket::SocketCloseResponder {
    fn send(self, arg: Result<(), i32>) -> Result<(), fidl::Error> {
        fppacket::SocketCloseResponder::send(self, arg)
    }
}

impl worker::SocketWorkerHandler for BindingData {
    type Request = fppacket::SocketRequest;
    type RequestStream = fppacket::SocketRequestStream;
    type CloseResponder = fppacket::SocketCloseResponder;
    type SetupArgs = ();
    type Spawner = ();

    async fn handle_request(
        &mut self,
        ctx: &mut Ctx,
        request: Self::Request,
        _spawners: &worker::TaskSpawnerCollection<()>,
    ) -> ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request)
    }

    async fn close(self, ctx: &mut Ctx) {
        let Self { peer_event: _, id } = self;
        let weak = id.downgrade();
        let SocketState { .. } = ctx
            .api()
            .device_socket()
            .remove(id)
            .map_deferred(|d| d.into_future("packet socket", &weak))
            .into_future()
            .await;
    }
}

struct RequestHandler<'a> {
    ctx: &'a mut Ctx,
    data: &'a mut BindingData,
}

impl<'a> RequestHandler<'a> {
    fn describe(self) -> fppacket::SocketDescribeResponse {
        let Self { ctx: _, data: BindingData { peer_event, id: _ } } = self;
        let peer = peer_event
            .duplicate_handle(
                // The client only needs to be able to receive signals so don't
                // allow it to set signals.
                zx::Rights::BASIC,
            )
            .unwrap_or_else(|s: zx::Status| panic!("failed to duplicate handle: {s}"));
        fppacket::SocketDescribeResponse { event: Some(peer), ..Default::default() }
    }

    fn bind(
        self,
        protocol: Option<Box<fppacket::ProtocolAssociation>>,
        interface: fppacket::BoundInterfaceId,
    ) -> Result<(), fposix::Errno> {
        let protocol = protocol
            .map(|protocol| {
                Ok(match *protocol {
                    fppacket::ProtocolAssociation::All(fppacket::Empty) => Protocol::All,
                    fppacket::ProtocolAssociation::Specified(p) => {
                        Protocol::Specific(NonZeroU16::new(p).ok_or(fposix::Errno::Einval)?)
                    }
                })
            })
            .transpose()?;
        let Self { ctx, data: BindingData { peer_event: _, id } } = self;
        let device = match interface {
            fppacket::BoundInterfaceId::All(fppacket::Empty) => None,
            fppacket::BoundInterfaceId::Specified(id) => {
                let id = BindingId::new(id).ok_or_else(|| DeviceNotFoundError.into_errno())?;
                Some(id.try_into_core_with_ctx(ctx.bindings_ctx()).map_err(IntoErrno::into_errno)?)
            }
        };
        let device_selector = match device.as_ref() {
            Some(d) => TargetDevice::SpecificDevice(d),
            None => TargetDevice::AnyDevice,
        };

        match protocol {
            Some(protocol) => {
                ctx.api().device_socket().set_device_and_protocol(id, device_selector, protocol)
            }
            None => ctx.api().device_socket().set_device(id, device_selector),
        }
        Ok(())
    }

    fn get_info(
        self,
    ) -> Result<
        (fppacket::Kind, Option<fppacket::ProtocolAssociation>, fppacket::BoundInterface),
        fposix::Errno,
    > {
        let Self { ctx, data: BindingData { peer_event: _, id } } = self;

        let SocketInfo { device, protocol } = ctx.api().device_socket().get_info(id);
        let SocketState { kind, .. } = *id.socket_state();

        let interface = match device {
            TargetDevice::AnyDevice => fppacket::BoundInterface::All(fppacket::Empty),
            TargetDevice::SpecificDevice(d) => fppacket::BoundInterface::Specified(
                d.try_into_fidl_with_ctx(ctx.bindings_ctx()).map_err(IntoErrno::into_errno)?,
            ),
        };

        let protocol = protocol.map(|p| match p {
            Protocol::All => fppacket::ProtocolAssociation::All(fppacket::Empty),
            Protocol::Specific(p) => fppacket::ProtocolAssociation::Specified(p.get()),
        });

        Ok((kind, protocol, interface))
    }

    fn receive(self) -> Result<Message, fposix::Errno> {
        let Self { ctx: _, data: BindingData { peer_event: _, id } } = self;

        let SocketState { queue, .. } = id.socket_state();
        let mut queue = queue.lock();
        queue.pop().ok_or(fposix::EWOULDBLOCK)
    }

    fn set_receive_buffer(self, size: u64) {
        let Self { ctx: _, data: BindingData { peer_event: _, id } } = self;

        let SocketState { queue, .. } = id.socket_state();
        let mut queue = queue.lock();
        queue.set_max_available_messages_size(size.try_into().unwrap_or(usize::MAX))
    }

    fn receive_buffer(self) -> u64 {
        let Self { ctx: _, data: BindingData { peer_event: _, id } } = self;

        let SocketState { queue, .. } = id.socket_state();
        let queue = queue.lock();
        queue.max_available_messages_size().try_into().unwrap_or(u64::MAX)
    }

    fn send_msg(
        self,
        packet_info: Option<Box<fppacket::PacketInfo>>,
        data: Vec<u8>,
    ) -> Result<(), fposix::Errno> {
        let Self { ctx, data: BindingData { peer_event: _, id } } = self;
        let SocketState { kind, .. } = *id.socket_state();

        let data = Buf::new(data, ..);
        // NB: Packet sockets require the packet_info be provided to specify
        // the destination of the data.
        let packet_info = *packet_info.ok_or(fposix::Errno::Einval)?;
        let device = match BindingId::new(packet_info.interface_id) {
            None => Err(fposix::Errno::Enxio),
            Some(id) => {
                id.try_into_core_with_ctx(ctx.bindings_ctx()).map_err(IntoErrno::into_errno)
            }
        }?;

        let result = match device {
            DeviceId::Loopback(device_id) => {
                let metadata = match kind {
                    fppacket::Kind::Network => {
                        Some(EthernetHeaderParams::try_from_fidl(packet_info)?)
                    }
                    fppacket::Kind::Link => None,
                };
                ctx.api().device_socket().send_frame::<_, LoopbackDevice>(
                    id,
                    DeviceSocketMetadata { device_id, metadata },
                    data,
                )
            }
            DeviceId::Ethernet(device_id) => {
                let metadata = match kind {
                    fppacket::Kind::Network => {
                        Some(EthernetHeaderParams::try_from_fidl(packet_info)?)
                    }
                    fppacket::Kind::Link => None,
                };
                ctx.api().device_socket().send_frame::<_, EthernetLinkDevice>(
                    id,
                    DeviceSocketMetadata { device_id, metadata },
                    data,
                )
            }
            DeviceId::PureIp(device_id) => {
                // NB: The behavior of packet sockets over pure IP device is
                // agnostic to the socket kind.
                let metadata = PureIpHeaderParams::try_from_fidl(packet_info)?;
                ctx.api().device_socket().send_frame::<_, PureIpDevice>(
                    id,
                    DeviceSocketMetadata { device_id, metadata },
                    data,
                )
            }
        };
        result.map_err(|e| e.into_errno())
    }

    fn handle_request(
        self,
        request: fppacket::SocketRequest,
    ) -> ControlFlow<fppacket::SocketCloseResponder, Option<fppacket::SocketRequestStream>> {
        match request {
            fppacket::SocketRequest::Clone { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel());
                let stream = fppacket::SocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fppacket::SocketRequest::Close { responder } => {
                return ControlFlow::Break(responder);
            }
            fppacket::SocketRequest::Query { responder } => {
                responder
                    .send(fppacket::SocketMarker::PROTOCOL_NAME.as_bytes())
                    .unwrap_or_log("failed to respond");
            }
            fppacket::SocketRequest::SetReuseAddress { value: _, responder } => {
                respond_not_supported!("packet::SetReuseAddress", responder)
            }
            fppacket::SocketRequest::GetReuseAddress { responder } => {
                respond_not_supported!("packet::GetReuseAddress", responder)
            }
            fppacket::SocketRequest::GetError { responder } => {
                respond_not_supported!("packet::GetError", responder)
            }
            fppacket::SocketRequest::SetBroadcast { value: _, responder } => {
                respond_not_supported!("packet::SetBroadcast", responder)
            }
            fppacket::SocketRequest::GetBroadcast { responder } => {
                respond_not_supported!("packet::GetBroadcast", responder)
            }
            fppacket::SocketRequest::SetSendBuffer { value_bytes: _, responder } => {
                respond_not_supported!("packet::SetSendBuffer", responder)
            }
            fppacket::SocketRequest::GetSendBuffer { responder } => {
                respond_not_supported!("packet::GetSendBuffer", responder)
            }
            fppacket::SocketRequest::SetReceiveBuffer { value_bytes, responder } => {
                responder
                    .send(Ok(self.set_receive_buffer(value_bytes)))
                    .unwrap_or_log("failed to respond");
            }
            fppacket::SocketRequest::GetReceiveBuffer { responder } => {
                responder.send(Ok(self.receive_buffer())).unwrap_or_log("failed to respond");
            }
            fppacket::SocketRequest::SetKeepAlive { value: _, responder } => {
                respond_not_supported!("packet::SetKeepAlive", responder)
            }
            fppacket::SocketRequest::GetKeepAlive { responder } => {
                respond_not_supported!("packet::GetKeepAlive", responder)
            }
            fppacket::SocketRequest::SetOutOfBandInline { value: _, responder } => {
                respond_not_supported!("packet::SetOutOfBandInline", responder)
            }
            fppacket::SocketRequest::GetOutOfBandInline { responder } => {
                respond_not_supported!("packet::GetOutOfBandInline", responder)
            }
            fppacket::SocketRequest::SetNoCheck { value: _, responder } => {
                respond_not_supported!("packet::SetNoCheck", responder)
            }
            fppacket::SocketRequest::GetNoCheck { responder } => {
                respond_not_supported!("packet::GetNoCheck", responder)
            }
            fppacket::SocketRequest::SetLinger { linger: _, length_secs: _, responder } => {
                respond_not_supported!("packet::SetLinger", responder)
            }
            fppacket::SocketRequest::GetLinger { responder } => {
                respond_not_supported!("packet::GetLinger", responder)
            }
            fppacket::SocketRequest::SetReusePort { value: _, responder } => {
                respond_not_supported!("packet::SetReusePort", responder)
            }
            fppacket::SocketRequest::GetReusePort { responder } => {
                respond_not_supported!("packet::GetReusePort", responder)
            }
            fppacket::SocketRequest::GetAcceptConn { responder } => {
                respond_not_supported!("packet::GetAcceptConn", responder)
            }
            fppacket::SocketRequest::SetBindToDevice { value: _, responder } => {
                respond_not_supported!("packet::SetBindToDevice", responder)
            }
            fppacket::SocketRequest::GetBindToDevice { responder } => {
                respond_not_supported!("packet::GetBindToDevice", responder)
            }
            fppacket::SocketRequest::SetBindToInterfaceIndex { value: _, responder } => {
                respond_not_supported!("packet::SetBindToInterfaceIndex", responder)
            }
            fppacket::SocketRequest::GetBindToInterfaceIndex { responder } => {
                respond_not_supported!("packet::GetBindToInterfaceIndex", responder)
            }
            fppacket::SocketRequest::SetTimestamp { value: _, responder } => {
                respond_not_supported!("packet::SetTimestamp", responder)
            }
            fppacket::SocketRequest::GetTimestamp { responder } => {
                respond_not_supported!("packet::GetTimestamp", responder)
            }
            fppacket::SocketRequest::Describe { responder } => {
                responder.send(self.describe()).unwrap_or_log("failed to respond");
            }
            fppacket::SocketRequest::Bind { protocol, bound_interface_id, responder } => responder
                .send(self.bind(protocol, bound_interface_id).log_error("packet::Bind"))
                .unwrap_or_log("failed to respond"),
            fppacket::SocketRequest::GetInfo { responder } => responder
                .send(match self.get_info().log_error("packet::GetInfo") {
                    Ok((kind, ref protocol, ref iface)) => Ok((kind, protocol.as_ref(), iface)),
                    Err(e) => Err(e),
                })
                .unwrap_or_log("failed to respond"),
            fppacket::SocketRequest::RecvMsg {
                want_packet_info,
                data_len,
                want_control,
                flags,
                responder,
            } => {
                let params = RecvMsgParams { want_packet_info, data_len, want_control, flags };
                responder
                    .send(
                        match self
                            .receive()
                            .log_error("packet::RecvMsg")
                            .map(|r| params.apply_to(r))
                        {
                            Ok((ref packet_info, ref data, ref control, truncated)) => {
                                Ok((packet_info.as_ref(), data.as_slice(), control, truncated))
                            }
                            Err(e) => Err(e),
                        },
                    )
                    .unwrap_or_log("failed to respond")
            }
            fppacket::SocketRequest::SendMsg { packet_info, data, control, flags, responder } => {
                let result = if ![
                    fppacket::SendControlData::default(),
                    fppacket::SendControlData {
                        socket: Some(fpsocket::SocketSendControlData::default()),
                        ..Default::default()
                    },
                ]
                .contains(&control)
                {
                    warn!("unsupported control data: {:?}", control);
                    Err(fposix::Errno::Eopnotsupp)
                } else if flags != fpsocket::SendMsgFlags::empty() {
                    warn!("unsupported control flags: {:?}", flags);
                    Err(fposix::Errno::Eopnotsupp)
                } else {
                    self.send_msg(packet_info, data)
                };
                responder
                    .send(result.log_error("packet::SendMsg"))
                    .unwrap_or_log("failed to respond");
            }
            fppacket::SocketRequest::SetMark { domain: _, mark: _, responder } => {
                responder.send(Err(fposix::Errno::Eopnotsupp)).unwrap_or_log("failed to respond")
            }
            fppacket::SocketRequest::GetMark { domain: _, responder } => {
                responder.send(Err(fposix::Errno::Eopnotsupp)).unwrap_or_log("failed to respond")
            }
            fppacket::SocketRequest::AttachBpfFilterUnsafe { code, responder } => {
                info!("AttachBpfFilterUnsafe is called on a packet socket.");
                *self.data.id.socket_state().bpf_filter.write() =
                    Some(SocketFilterProgram::new(code));
                responder.send(Ok(())).unwrap_or_log("failed to respond");
            }
        }
        ControlFlow::Continue(None)
    }
}

fn iface_type(device: &DeviceId<BindingsCtx>) -> fppacket::HardwareType {
    match device {
        DeviceId::Ethernet(_) => fppacket::HardwareType::Ethernet,
        DeviceId::Loopback(_) => fppacket::HardwareType::Loopback,
        DeviceId::PureIp(_) => fppacket::HardwareType::NetworkOnly,
    }
}

impl TryIntoFidlWithContext<fppacket::InterfaceProperties> for WeakDeviceId<BindingsCtx> {
    type Error = DeviceNotFoundError;
    fn try_into_fidl_with_ctx<C: crate::bindings::util::ConversionContext>(
        self,
        ctx: &C,
    ) -> Result<fppacket::InterfaceProperties, Self::Error> {
        let device = self.upgrade().ok_or(DeviceNotFoundError)?;
        let iface_type = iface_type(&device);
        let addr = match &device {
            DeviceId::Ethernet(e) => {
                fppacket::HardwareAddress::Eui48(e.external_state().mac.into_fidl())
            }
            DeviceId::Loopback(_) => {
                // Pretend that the loopback interface has an all-zeroes MAC
                // address to match Linux behavior.
                fppacket::HardwareAddress::Eui48(fidl_fuchsia_net::MacAddress { octets: [0; 6] })
            }
            DeviceId::PureIp(_) => {
                // Pure IP devices don't support link-layer addressing.
                fppacket::HardwareAddress::None(fppacket::Empty)
            }
        };
        let id = ctx.get_binding_id(device).get();
        Ok(fppacket::InterfaceProperties { addr, id, type_: iface_type })
    }
}

impl TryFromFidl<fppacket::PacketInfo> for EthernetHeaderParams {
    type Error = fposix::Errno;

    fn try_from_fidl(packet_info: fppacket::PacketInfo) -> Result<Self, Self::Error> {
        let fppacket::PacketInfo { protocol, interface_id: _, addr } = packet_info;
        let protocol = NonZeroU16::new(protocol).ok_or(fposix::Errno::Einval)?;
        let dest_addr = match addr {
            fppacket::HardwareAddress::Eui48(mac) => Some(mac.into_core()),
            fppacket::HardwareAddress::None(fppacket::Empty) => None,
            fppacket::HardwareAddress::__SourceBreaking { .. } => None,
        }
        .ok_or(fposix::Errno::Einval)?;
        Ok(EthernetHeaderParams { dest_addr, protocol: protocol.get().into() })
    }
}

impl TryFromFidl<fppacket::PacketInfo> for PureIpHeaderParams {
    type Error = fposix::Errno;

    fn try_from_fidl(packet_info: fppacket::PacketInfo) -> Result<Self, Self::Error> {
        let fppacket::PacketInfo { protocol, interface_id: _, addr } = packet_info;
        match addr {
            fppacket::HardwareAddress::None(fppacket::Empty) => {}
            fppacket::HardwareAddress::Eui48(_)
            | fppacket::HardwareAddress::__SourceBreaking { .. } => {
                return Err(fposix::Errno::Einval)
            }
        }
        let ip_version = match protocol.into() {
            EtherType::Ipv4 => IpVersion::V4,
            EtherType::Ipv6 => IpVersion::V6,
            EtherType::Arp | EtherType::Other(_) => return Err(fposix::Errno::Einval),
        };
        Ok(PureIpHeaderParams { ip_version })
    }
}

struct RecvMsgParams {
    want_packet_info: bool,
    data_len: u32,
    want_control: bool,
    flags: fidl_fuchsia_posix_socket::RecvMsgFlags,
}

impl RecvMsgParams {
    fn apply_to(
        self,
        response: Message,
    ) -> (Option<fppacket::RecvPacketInfo>, Vec<u8>, fppacket::RecvControlData, u32) {
        let Self { want_packet_info, data_len, want_control, flags } = self;
        let data_len = data_len.try_into().unwrap_or(usize::MAX);

        let Message { data, mut body } = response;

        let truncated = body.len().saturating_sub(data_len);
        body.truncate(data_len);

        let packet_info = want_packet_info.then(|| {
            let MessageData { info, interface_type, interface_id, packet_type } = data;
            let packet_info = match info {
                MessageDataInfo::Ethernet { src_mac, protocol } => fppacket::PacketInfo {
                    protocol,
                    interface_id,
                    addr: fppacket::HardwareAddress::Eui48(src_mac.into_fidl()),
                },
                MessageDataInfo::Ip { ip_version } => {
                    let protocol = EtherType::from_ip_version(ip_version).into();
                    fppacket::PacketInfo {
                        protocol,
                        interface_id,
                        // IP Devices don't have hardware addresses.
                        addr: fppacket::HardwareAddress::None(fppacket::Empty),
                    }
                }
            };

            fppacket::RecvPacketInfo { packet_type, interface_type, packet_info }
        });

        // TODO(https://fxbug.dev/42058078): Return control data and flags.
        let _ = (want_control, flags);
        let control = fppacket::RecvControlData::default();

        (packet_info, body, control, truncated.try_into().unwrap_or(u32::MAX))
    }
}

impl IntoErrno for SendFrameErrorReason {
    fn into_errno(self) -> fposix::Errno {
        match self {
            SendFrameErrorReason::Alloc | SendFrameErrorReason::QueueFull => fposix::Errno::Enobufs,
            SendFrameErrorReason::SizeConstraintsViolation => fposix::Errno::Einval,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use net_declare::{fidl_mac, net_mac};
    use test_case::test_case;

    const IFACE: u64 = 1;
    const FIDL_PROTO: u16 = 1234;
    const PROTO: EtherType = EtherType::Other(1234);
    const FIDL_MAC: fppacket::HardwareAddress =
        fppacket::HardwareAddress::Eui48(fidl_mac!("00:11:22:33:44:55"));
    const MAC: net_types::ethernet::Mac = net_mac!("00:11:22:33:44:55");
    const EMPTY_HWADDR: fppacket::HardwareAddress =
        fppacket::HardwareAddress::None(fppacket::Empty);

    #[test_case(
        fppacket::PacketInfo{interface_id: IFACE, protocol: FIDL_PROTO, addr: FIDL_MAC},
        Ok(EthernetHeaderParams{dest_addr: MAC, protocol: PROTO});
        "success"
    )]
    #[test_case(
        fppacket::PacketInfo{interface_id: IFACE, protocol: 0, addr: FIDL_MAC},
        Err(fposix::Errno::Einval);
        "bad_protocol"
    )]
    #[test_case(
        fppacket::PacketInfo{
            interface_id: IFACE,
            protocol: FIDL_PROTO,
            addr: EMPTY_HWADDR,
        },
        Err(fposix::Errno::Einval);
        "missing_addr"
    )]
    fn ethernet_header_params_from_packet_info(
        packet_info: fppacket::PacketInfo,
        expected_params: Result<EthernetHeaderParams, fposix::Errno>,
    ) {
        assert_eq!(EthernetHeaderParams::try_from_fidl(packet_info), expected_params);
    }

    const IPV4_PROTO: u16 = 0x0800;
    const IPV6_PROTO: u16 = 0x86DD;
    #[test_case(
        fppacket::PacketInfo{interface_id: IFACE, protocol: IPV4_PROTO, addr: EMPTY_HWADDR},
        Ok(PureIpHeaderParams{ip_version: IpVersion::V4});
        "success_ipv4"
    )]
    #[test_case(
        fppacket::PacketInfo{interface_id: IFACE, protocol: IPV6_PROTO, addr: EMPTY_HWADDR},
        Ok(PureIpHeaderParams{ip_version: IpVersion::V6});
        "success_ipv6"
    )]
    #[test_case(
        fppacket::PacketInfo{interface_id: IFACE, protocol: FIDL_PROTO, addr: EMPTY_HWADDR},
        Err(fposix::Errno::Einval);
        "bad_protocol"
    )]
    #[test_case(
        fppacket::PacketInfo{interface_id: IFACE, protocol: IPV4_PROTO, addr: FIDL_MAC},
        Err(fposix::Errno::Einval);
        "bad_addr"
    )]
    fn pure_ip_header_params_from_packet_info(
        packet_info: fppacket::PacketInfo,
        expected_params: Result<PureIpHeaderParams, fposix::Errno>,
    ) {
        assert_eq!(PureIpHeaderParams::try_from_fidl(packet_info), expected_params);
    }
}
