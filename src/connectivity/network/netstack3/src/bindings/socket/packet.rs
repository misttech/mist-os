// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{num::NonZeroU16, ops::ControlFlow};

use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket as fpsocket;
use fidl_fuchsia_posix_socket_packet as fppacket;

use fidl::{endpoints::RequestStream as _, Peered as _};
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, HandleBased as _};
use futures::TryStreamExt as _;
use net_types::ethernet::Mac;
use netstack3_core::{
    device::{
        socket::{
            DeviceSocketTypes, EthernetFrame, Frame, NonSyncContext, Protocol, ReceivedFrame,
            SendDatagramError, SendDatagramParams, SendFrameError, SendFrameParams, SentFrame,
            SocketId, SocketInfo, TargetDevice,
        },
        DeviceId, FrameDestination, WeakDeviceId,
    },
    sync::Mutex,
    SyncCtx,
};
use packet::Buf;
use tracing::error;

use crate::bindings::{
    devices::BindingId,
    socket::{
        queue::{BodyLen, MessageQueue},
        worker::{self, CloseResponder, SocketWorker},
        IntoErrno, SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING,
    },
    util::{
        DeviceNotFoundError, IntoCore as _, IntoFidl as _, TryFromFidlWithContext,
        TryIntoCoreWithContext as _, TryIntoFidlWithContext,
    },
    BindingsNonSyncCtxImpl, Ctx, DeviceIdExt as _,
};

/// State held in the non-sync context for a single socket.
#[derive(Debug)]
pub(crate) struct SocketState {
    /// The received messages for the socket.
    queue: Mutex<MessageQueue<Message>>,
    kind: fppacket::Kind,
}

impl DeviceSocketTypes for BindingsNonSyncCtxImpl {
    type SocketState = SocketState;
}

impl NonSyncContext<DeviceId<Self>> for BindingsNonSyncCtxImpl {
    fn receive_frame(
        &self,
        state: &Self::SocketState,
        device: &DeviceId<Self>,
        frame: Frame<&[u8]>,
        raw: &[u8],
    ) {
        let SocketState { queue, kind } = state;

        let data = MessageData::new(&frame, device);

        let message = match kind {
            fppacket::Kind::Network => frame.into_body(),
            fppacket::Kind::Link => raw,
        };

        queue.lock().receive(IntoMessage(data, message));
    }
}

pub(crate) async fn serve(
    ctx: Ctx,
    stream: fppacket::ProviderRequestStream,
) -> Result<(), fidl::Error> {
    let ctx = &ctx;
    stream
        .try_for_each(|req| async {
            match req {
                fppacket::ProviderRequest::Socket { responder, kind } => {
                    let (client, request_stream) = fidl::endpoints::create_request_stream()
                        .unwrap_or_else(|e: fidl::Error| {
                            panic!("failed to create a new request stream: {e}")
                        });
                    fasync::Task::spawn(SocketWorker::serve_stream_with(
                        ctx.clone(),
                        move |sync_ctx, non_sync_ctx, properties| {
                            BindingData::new(sync_ctx, non_sync_ctx, kind, properties)
                        },
                        SocketWorkerProperties {},
                        request_stream,
                    ))
                    .detach();
                    responder
                        .send(Ok(client))
                        .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
            }
            Ok(())
        })
        .await
}

#[derive(Debug)]
struct BindingData {
    /// The event to hand off for [`fppacket::SocketRequest::Describe`].
    peer_event: zx::EventPair,
    /// The identifier for the [`netstack3_core`] socket resource.
    id: SocketId<BindingsNonSyncCtxImpl>,
}

struct IntoMessage<'a>(MessageData, &'a [u8]);

impl BodyLen for IntoMessage<'_> {
    fn body_len(&self) -> usize {
        let Self(_data, body) = self;
        body.len()
    }
}

impl From<IntoMessage<'_>> for Message {
    fn from(IntoMessage(data, body): IntoMessage<'_>) -> Self {
        Self { data, body: body.into() }
    }
}

#[derive(Debug)]
struct Message {
    data: MessageData,
    body: Vec<u8>,
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
}

impl MessageData {
    fn new(frame: &Frame<&[u8]>, device: &DeviceId<BindingsNonSyncCtxImpl>) -> Self {
        let (packet_type, info) = match frame {
            Frame::Sent(sent) => (
                fppacket::PacketType::Outgoing,
                match sent {
                    SentFrame::Ethernet(frame) => MessageDataInfo::new_ethernet(frame),
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
        };

        Self {
            packet_type,
            info,
            interface_id: device.external_state().static_common_info().binding_id.get(),
            interface_type: iface_type(device),
        }
    }
}

impl MessageDataInfo {
    fn new_ethernet(frame: &EthernetFrame<&[u8]>) -> Self {
        let EthernetFrame { src_mac, dst_mac: _, protocol, body: _ } = *frame;
        Self::Ethernet { src_mac, protocol: protocol.unwrap_or(0) }
    }
}

impl BodyLen for Message {
    fn body_len(&self) -> usize {
        self.body.len()
    }
}

impl BindingData {
    fn new(
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        _non_sync_ctx: &mut BindingsNonSyncCtxImpl,
        kind: fppacket::Kind,
        SocketWorkerProperties {}: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        match local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            Ok(()) => (),
            Err(e) => error!("socket failed to signal peer: {:?}", e),
        }

        let id = netstack3_core::device::socket::create(
            sync_ctx,
            SocketState { queue: Mutex::new(MessageQueue::new(local_event)), kind },
        );

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

    fn handle_request(
        &mut self,
        ctx: &Ctx,
        request: Self::Request,
    ) -> ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request)
    }

    fn close(
        self,
        sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        _non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    ) {
        let Self { peer_event: _, id } = self;
        netstack3_core::device::socket::remove(sync_ctx, id)
    }
}

struct RequestHandler<'a> {
    ctx: &'a Ctx,
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
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let device = match interface {
            fppacket::BoundInterfaceId::All(fppacket::Empty) => None,
            fppacket::BoundInterfaceId::Specified(id) => {
                let id = BindingId::new(id).ok_or(DeviceNotFoundError.into_errno())?;
                Some(id.try_into_core_with_ctx(non_sync_ctx).map_err(IntoErrno::into_errno)?)
            }
        };
        let device_selector = match device.as_ref() {
            Some(d) => TargetDevice::SpecificDevice(d),
            None => TargetDevice::AnyDevice,
        };

        match protocol {
            Some(protocol) => netstack3_core::device::socket::set_device_and_protocol(
                sync_ctx,
                id,
                device_selector,
                protocol,
            ),
            None => netstack3_core::device::socket::set_device(sync_ctx, id, device_selector),
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
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;

        let SocketInfo { device, protocol } =
            netstack3_core::device::socket::get_info(sync_ctx, id);
        let SocketState { queue: _, kind } = *id.socket_state();

        let interface = match device {
            TargetDevice::AnyDevice => fppacket::BoundInterface::All(fppacket::Empty),
            TargetDevice::SpecificDevice(d) => fppacket::BoundInterface::Specified(
                d.try_into_fidl_with_ctx(non_sync_ctx).map_err(IntoErrno::into_errno)?,
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

        let SocketState { queue, kind: _ } = id.socket_state();
        let mut queue = queue.lock();
        queue.pop().ok_or(fposix::EWOULDBLOCK)
    }

    fn set_receive_buffer(self, size: u64) {
        let Self { ctx: _, data: BindingData { peer_event: _, id } } = self;

        let SocketState { queue, kind: _ } = id.socket_state();
        let mut queue = queue.lock();
        queue.set_max_available_messages_size(size.try_into().unwrap_or(usize::MAX))
    }

    fn receive_buffer(self) -> u64 {
        let Self { ctx: _, data: BindingData { peer_event: _, id } } = self;

        let SocketState { queue, kind: _ } = id.socket_state();
        let queue = queue.lock();
        queue.max_available_messages_size().try_into().unwrap_or(u64::MAX)
    }

    fn send_msg(
        self,
        packet_info: Option<Box<fppacket::PacketInfo>>,
        data: Vec<u8>,
    ) -> Result<(), fposix::Errno> {
        let Self { ctx, data: BindingData { peer_event: _, id } } = self;
        let mut ctx = ctx.clone();
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
        let SocketState { kind, queue: _ } = *id.socket_state();

        let data = Buf::new(data, ..);
        match kind {
            fppacket::Kind::Network => {
                let params = packet_info.try_into_core_with_ctx(non_sync_ctx)?;
                netstack3_core::device::socket::send_datagram(
                    sync_ctx,
                    non_sync_ctx,
                    id,
                    params,
                    data,
                )
                .map_err(|(_, e): (Buf<Vec<u8>>, _)| e.into_errno())
            }
            fppacket::Kind::Link => {
                let params = packet_info
                    .try_into_core_with_ctx(non_sync_ctx)
                    .map_err(IntoErrno::into_errno)?;
                netstack3_core::device::socket::send_frame(sync_ctx, non_sync_ctx, id, params, data)
                    .map_err(|(_, e): (Buf<Vec<u8>>, _)| e.into_errno())
            }
        }
    }

    fn handle_request(
        self,
        request: fppacket::SocketRequest,
    ) -> ControlFlow<fppacket::SocketCloseResponder, Option<fppacket::SocketRequestStream>> {
        match request {
            fppacket::SocketRequest::Clone2 { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel())
                    .expect("failed to create async channel");
                let stream = fppacket::SocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fppacket::SocketRequest::Close { responder } => {
                return ControlFlow::Break(responder);
            }
            fppacket::SocketRequest::Query { responder } => {
                responder
                    .send(fppacket::SOCKET_PROTOCOL_NAME.as_bytes())
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fppacket::SocketRequest::SetReuseAddress { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetReuseAddress { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetError { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetBroadcast { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetBroadcast { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetSendBuffer { value_bytes: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetSendBuffer { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetReceiveBuffer { value_bytes, responder } => {
                responder
                    .send(Ok(self.set_receive_buffer(value_bytes)))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fppacket::SocketRequest::GetReceiveBuffer { responder } => {
                responder
                    .send(Ok(self.receive_buffer()))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fppacket::SocketRequest::SetKeepAlive { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetKeepAlive { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetOutOfBandInline { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetOutOfBandInline { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetNoCheck { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetNoCheck { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetLinger { linger: _, length_secs: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"))
            }
            fppacket::SocketRequest::GetLinger { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetReusePort { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetReusePort { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetAcceptConn { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetBindToDevice { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetBindToDevice { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::SetTimestamp { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetTimestamp { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::Describe { responder } => {
                responder
                    .send(self.describe())
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fppacket::SocketRequest::Bind { protocol, bound_interface_id, responder } => responder
                .send(self.bind(protocol, bound_interface_id))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::GetInfo { responder } => responder
                .send(match self.get_info() {
                    Ok((kind, ref protocol, ref iface)) => Ok((kind, protocol.as_ref(), iface)),
                    Err(e) => Err(e),
                })
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fppacket::SocketRequest::RecvMsg {
                want_packet_info,
                data_len,
                want_control,
                flags,
                responder,
            } => {
                let params = RecvMsgParams { want_packet_info, data_len, want_control, flags };
                responder
                    .send(match self.receive().map(|r| params.apply_to(r)) {
                        Ok((ref packet_info, ref data, ref control, truncated)) => {
                            Ok((packet_info.as_ref(), data.as_slice(), control, truncated))
                        }
                        Err(e) => Err(e),
                    })
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"))
            }
            fppacket::SocketRequest::SendMsg { packet_info, data, control, flags, responder } => {
                if ![
                    fppacket::SendControlData::default(),
                    fppacket::SendControlData {
                        socket: Some(fpsocket::SocketSendControlData::default()),
                        ..Default::default()
                    },
                ]
                .contains(&control)
                {
                    tracing::warn!("unsupported control data: {:?}", control);
                    responder
                        .send(Err(fposix::Errno::Eopnotsupp))
                        .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                } else if flags != fpsocket::SendMsgFlags::empty() {
                    tracing::warn!("unsupported control flags: {:?}", flags);
                    responder
                        .send(Err(fposix::Errno::Eopnotsupp))
                        .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                } else {
                    responder
                        .send(self.send_msg(packet_info, data))
                        .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
            }
        }
        ControlFlow::Continue(None)
    }
}

fn iface_type(device: &DeviceId<BindingsNonSyncCtxImpl>) -> fppacket::HardwareType {
    match device {
        DeviceId::Ethernet(_) => fppacket::HardwareType::Ethernet,
        DeviceId::Loopback(_) => fppacket::HardwareType::Loopback,
    }
}

impl TryIntoFidlWithContext<fppacket::InterfaceProperties>
    for WeakDeviceId<BindingsNonSyncCtxImpl>
{
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
        };
        let id = ctx.get_binding_id(device).get();
        Ok(fppacket::InterfaceProperties { addr, id, type_: iface_type })
    }
}

impl<D> TryFromFidlWithContext<Option<Box<fppacket::PacketInfo>>> for SendDatagramParams<D>
where
    D: TryFromFidlWithContext<BindingId, Error = DeviceNotFoundError>,
{
    type Error = fposix::Errno;

    fn try_from_fidl_with_ctx<C: crate::bindings::util::ConversionContext>(
        ctx: &C,
        packet_info: Option<Box<fppacket::PacketInfo>>,
    ) -> Result<Self, Self::Error> {
        packet_info.ok_or(fposix::Errno::Einval).and_then(|info| {
            let fppacket::PacketInfo { protocol, interface_id, addr } = *info;
            let device = BindingId::new(interface_id)
                .map(|id| id.try_into_core_with_ctx(ctx))
                .transpose()
                .map_err(IntoErrno::into_errno)?;
            let protocol = NonZeroU16::new(protocol);
            let dest_addr = match addr {
                fppacket::HardwareAddress::Eui48(mac) => Some(mac.into_core()),
                fppacket::HardwareAddress::None(fppacket::Empty) => None,
                fppacket::HardwareAddressUnknown!() => None,
            }
            .ok_or(fposix::Errno::Einval)?;
            Ok(Self { frame: SendFrameParams { device }, protocol, dest_addr })
        })
    }
}

impl<D> TryFromFidlWithContext<Option<Box<fppacket::PacketInfo>>> for SendFrameParams<D>
where
    D: TryFromFidlWithContext<BindingId, Error = DeviceNotFoundError>,
{
    type Error = DeviceNotFoundError;

    fn try_from_fidl_with_ctx<C: crate::bindings::util::ConversionContext>(
        ctx: &C,
        packet_info: Option<Box<fppacket::PacketInfo>>,
    ) -> Result<Self, Self::Error> {
        packet_info.map_or(Ok(SendFrameParams::default()), |info| {
            let fppacket::PacketInfo { protocol, interface_id, addr } = *info;
            // Ignore protocol and addr since the frame to send already includes
            // any link-layer headers.
            let _ = (protocol, addr);
            let device = BindingId::new(interface_id)
                .map(|id| id.try_into_core_with_ctx(ctx))
                .transpose()?;
            Ok(SendFrameParams { device })
        })
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
            };

            fppacket::RecvPacketInfo { packet_type, interface_type, packet_info }
        });

        // TODO(https://fxbug.dev/106735): Return control data and flags.
        let _ = (want_control, flags);
        let control = fppacket::RecvControlData::default();

        (packet_info, body, control, truncated.try_into().unwrap_or(u32::MAX))
    }
}

impl IntoErrno for SendDatagramError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            Self::Frame(f) => f.into_errno(),
            Self::NoProtocol => fposix::Errno::Einval,
        }
    }
}

impl IntoErrno for SendFrameError {
    fn into_errno(self) -> fposix::Errno {
        match self {
            SendFrameError::NoDevice => fposix::Errno::Einval,
            SendFrameError::SendFailed => fposix::Errno::Enobufs,
        }
    }
}
