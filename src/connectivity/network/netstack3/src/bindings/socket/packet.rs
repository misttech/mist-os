// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    num::NonZeroU16,
    ops::{ControlFlow, DerefMut},
    sync::Arc,
};

use assert_matches::assert_matches;
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket_packet as fppacket;

use fidl::{endpoints::RequestStream as _, Peered as _};
use fuchsia_async as fasync;
use fuchsia_zircon::{self as zx, HandleBased as _};
use futures::TryStreamExt as _;
use log::error;
use net_types::ethernet::Mac;
use netstack3_core::{
    data_structures::id_map::{EntryKey as _, IdMap},
    device::{
        socket::{Frame, NonSyncContext, Protocol, SocketId, SocketInfo, TargetDevice},
        DeviceId, FrameDestination, WeakDeviceId,
    },
    sync::Mutex,
    Ctx, SyncCtx,
};

use crate::bindings::{
    devices::BindingId,
    socket::{
        queue::{BodyLen, MessageQueue},
        worker::{self, CloseResponder, SocketWorker},
        IntoErrno, SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING,
    },
    util::{
        ConversionContext as _, DeviceNotFoundError, IntoFidl, TryIntoCoreWithContext as _,
        TryIntoFidlWithContext,
    },
    BindingsNonSyncCtxImpl, NetstackContext,
};

#[derive(Default)]
pub(crate) struct Sockets {
    /// State for all sockets, indexed by their [`SocketId`]
    sockets: IdMap<SocketState>,
}

/// State held in the non-sync context for a single socket.
#[derive(Debug)]
struct SocketState {
    /// The received messages for the socket.
    ///
    /// This is shared with the worker that handles requests for the socket.
    queue: Arc<Mutex<MessageQueue<Message>>>,
    kind: fppacket::Kind,
}

impl NonSyncContext<DeviceId<Self>> for BindingsNonSyncCtxImpl {
    fn receive_frame(
        &mut self,
        socket: SocketId,
        device: &DeviceId<Self>,
        frame: Frame<&[u8]>,
        raw: &[u8],
    ) {
        let Sockets { sockets } = &self.packet_sockets;
        let SocketState { queue, kind } =
            sockets.get(socket.get_key_index()).expect("invalid socket ID");

        let Frame::Ethernet { frame_dst, src_mac, dst_mac, protocol, body } = frame;
        let packet_type = match frame_dst {
            FrameDestination::Broadcast => fppacket::PacketType::Broadcast,
            FrameDestination::Multicast => fppacket::PacketType::Multicast,
            FrameDestination::Individual { local } => local
                .then_some(fppacket::PacketType::Host)
                .unwrap_or(fppacket::PacketType::OtherHost),
        };
        let message = match kind {
            fppacket::Kind::Network => body,
            fppacket::Kind::Link => raw,
        };

        let data = MessageData {
            dst_mac,
            src_mac,
            packet_type,
            protocol: protocol.unwrap_or(0),
            interface_type: iface_type(device),
            interface_id: self.get_binding_id(device.clone()).get(),
        };
        queue.lock().receive(IntoMessage(data, message))
    }
}

pub(crate) async fn serve(
    ctx: NetstackContext,
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
                    responder_send!(responder, &mut Ok(client));
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
    /// Shared queue of messages received for the socket.
    ///
    /// This is the "read" end of the queue; the non-sync context holds a
    /// reference to the same queue for delivering incoming messages.
    message_queue: Arc<Mutex<MessageQueue<Message>>>,
    state: State,
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
    src_mac: Mac,
    dst_mac: Mac,
    protocol: u16,
    interface_type: fppacket::HardwareType,
    interface_id: u64,
    packet_type: fppacket::PacketType,
}

impl BodyLen for Message {
    fn body_len(&self) -> usize {
        self.body.len()
    }
}

impl BindingData {
    fn new(
        sync_ctx: &mut SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
        kind: fppacket::Kind,
        SocketWorkerProperties {}: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        match local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            Ok(()) => (),
            Err(e) => error!("socket failed to signal peer: {:?}", e),
        }

        let message_queue = Arc::new(Mutex::new(MessageQueue::new(local_event)));
        let id = netstack3_core::device::socket::create(sync_ctx);

        let Sockets { sockets } = &mut non_sync_ctx.packet_sockets;
        assert_matches!(
            sockets.insert(id.get_key_index(), SocketState { queue: message_queue.clone(), kind }),
            None
        );

        BindingData { message_queue, peer_event, state: State { id, kind } }
    }
}

#[derive(Debug)]
struct State {
    id: SocketId,
    kind: fppacket::Kind,
}

impl CloseResponder for fppacket::SocketCloseResponder {
    fn send(self, arg: &mut fidl_fuchsia_unknown::CloseableCloseResult) -> Result<(), fidl::Error> {
        fppacket::SocketCloseResponder::send(self, arg)
    }
}

impl worker::SocketWorkerHandler for BindingData {
    type Request = fppacket::SocketRequest;
    type RequestStream = fppacket::SocketRequestStream;
    type CloseResponder = fppacket::SocketCloseResponder;

    async fn handle_request(
        &mut self,
        ctx: &NetstackContext,
        request: Self::Request,
    ) -> ControlFlow<Self::CloseResponder, Option<Self::RequestStream>> {
        RequestHandler { ctx, data: self }.handle_request(request).await
    }

    fn close(
        self,
        sync_ctx: &mut SyncCtx<BindingsNonSyncCtxImpl>,
        non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    ) {
        let Self { peer_event: _, state, message_queue: _ } = self;
        let State { id, kind: _ } = state;
        let Sockets { sockets } = &mut non_sync_ctx.packet_sockets;
        assert_matches!(sockets.remove(id.get_key_index()), Some(_));
        netstack3_core::device::socket::remove(sync_ctx, id)
    }
}

struct RequestHandler<'a> {
    ctx: &'a NetstackContext,
    data: &'a mut BindingData,
}

impl<'a> RequestHandler<'a> {
    fn describe(self) -> fppacket::SocketDescribeResponse {
        let Self { ctx: _, data: BindingData { peer_event, state: _, message_queue: _ } } = self;
        let peer = peer_event
            .duplicate_handle(
                // The client only needs to be able to receive signals so don't
                // allow it to set signals.
                zx::Rights::BASIC,
            )
            .unwrap_or_else(|s: zx::Status| panic!("failed to duplicate handle: {s}"));
        fppacket::SocketDescribeResponse {
            event: Some(peer),
            ..fppacket::SocketDescribeResponse::EMPTY
        }
    }

    async fn bind(
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
        let Self { ctx, data: BindingData { peer_event: _, message_queue: _, state } } = self;
        let mut guard = ctx.lock().await;
        let Ctx { sync_ctx, non_sync_ctx } = guard.deref_mut();
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

        let State { id, kind: _ } = state;
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

    async fn get_info(
        self,
    ) -> Result<
        (fppacket::Kind, Option<Box<fppacket::ProtocolAssociation>>, fppacket::BoundInterface),
        fposix::Errno,
    > {
        let Self {
            ctx,
            data: BindingData { peer_event: _, message_queue: _, state: State { id, kind } },
        } = self;
        let mut guard = ctx.lock().await;
        let Ctx { sync_ctx, non_sync_ctx } = guard.deref_mut();

        let SocketInfo { device, protocol } =
            netstack3_core::device::socket::get_info(sync_ctx, id);
        let interface = match device {
            TargetDevice::AnyDevice => fppacket::BoundInterface::All(fppacket::Empty),
            TargetDevice::SpecificDevice(d) => fppacket::BoundInterface::Specified(
                d.try_into_fidl_with_ctx(non_sync_ctx).map_err(IntoErrno::into_errno)?,
            ),
        };

        let protocol = protocol.map(|p| {
            Box::new(match p {
                Protocol::All => fppacket::ProtocolAssociation::All(fppacket::Empty),
                Protocol::Specific(p) => fppacket::ProtocolAssociation::Specified(p.get()),
            })
        });

        Ok((*kind, protocol, interface))
    }

    async fn receive(self) -> Result<Message, fposix::Errno> {
        let Self {
            ctx: _,
            data: BindingData { peer_event: _, message_queue, state: State { id: _, kind: _ } },
        } = self;

        let mut queue = message_queue.lock();
        queue.pop().ok_or(fposix::EWOULDBLOCK)
    }

    async fn set_receive_buffer(self, size: u64) {
        let Self {
            ctx: _,
            data: BindingData { peer_event: _, message_queue, state: State { id: _, kind: _ } },
        } = self;

        let mut queue = message_queue.lock();
        queue.set_max_available_messages_size(size.try_into().unwrap_or(usize::MAX))
    }

    async fn receive_buffer(self) -> u64 {
        let Self {
            ctx: _,
            data: BindingData { peer_event: _, message_queue, state: State { id: _, kind: _ } },
        } = self;

        let queue = message_queue.lock();
        queue.max_available_messages_size().try_into().unwrap_or(u64::MAX)
    }

    async fn handle_request(
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
                responder_send!(responder, fppacket::SOCKET_PROTOCOL_NAME.as_bytes());
            }
            fppacket::SocketRequest::SetReuseAddress { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetReuseAddress { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetError { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetBroadcast { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetBroadcast { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetSendBuffer { value_bytes: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetSendBuffer { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetReceiveBuffer { value_bytes, responder } => {
                responder_send!(responder, &mut Ok(self.set_receive_buffer(value_bytes).await));
            }
            fppacket::SocketRequest::GetReceiveBuffer { responder } => {
                responder_send!(responder, &mut Ok(self.receive_buffer().await));
            }
            fppacket::SocketRequest::SetKeepAlive { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetKeepAlive { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetOutOfBandInline { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetOutOfBandInline { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetNoCheck { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetNoCheck { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetLinger { linger: _, length_secs: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetLinger { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetReusePort { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetReusePort { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetAcceptConn { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetBindToDevice { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetBindToDevice { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::SetTimestamp { value: _, responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::GetTimestamp { responder } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
            }
            fppacket::SocketRequest::Describe { responder } => {
                responder_send!(responder, self.describe());
            }
            fppacket::SocketRequest::Bind { protocol, bound_interface_id, responder } => {
                responder_send!(responder, &mut self.bind(protocol, bound_interface_id).await)
            }
            fppacket::SocketRequest::GetInfo { responder } => {
                responder_send!(responder, &mut self.get_info().await);
            }
            fppacket::SocketRequest::RecvMsg {
                want_packet_info,
                data_len,
                want_control,
                flags,
                responder,
            } => {
                let params = RecvMsgParams { want_packet_info, data_len, want_control, flags };
                let response = self.receive().await;
                responder_send!(responder, &mut response.map(|r| params.apply_to(r)))
            }
            fppacket::SocketRequest::SendMsg {
                packet_info: _,
                data: _,
                control: _,
                flags: _,
                responder,
            } => {
                responder_send!(responder, &mut Err(fposix::Errno::Eopnotsupp))
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
    ) -> (Option<Box<fppacket::RecvPacketInfo>>, Vec<u8>, fppacket::RecvControlData, u32) {
        let Self { want_packet_info, data_len, want_control, flags } = self;
        let data_len = data_len.try_into().unwrap_or(usize::MAX);

        let Message { data, mut body } = response;
        let MessageData { src_mac, dst_mac, packet_type, interface_id, interface_type, protocol } =
            data;

        let truncated = body.len().saturating_sub(data_len);
        body.truncate(data_len);

        let packet_info = if want_packet_info {
            let info = fppacket::RecvPacketInfo {
                packet_type,
                interface_type,
                packet_info: fppacket::PacketInfo {
                    protocol,
                    interface_id,
                    addr: fppacket::HardwareAddress::Eui48(src_mac.into_fidl()),
                },
            };
            Some(Box::new(info))
        } else {
            None
        };

        // TODO(https://fxbug.dev/106735): Return control data and flags.
        let _ = (want_control, flags, dst_mac);
        let control = fppacket::RecvControlData::EMPTY;

        (packet_info, body, control, truncated.try_into().unwrap_or(u32::MAX))
    }
}
