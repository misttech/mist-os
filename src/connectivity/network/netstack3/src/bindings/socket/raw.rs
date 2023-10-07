// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::ControlFlow;

use fidl::endpoints::{ProtocolMarker, RequestStream};
use fidl_fuchsia_posix as fposix;
use fidl_fuchsia_posix_socket as fposix_socket;
use fidl_fuchsia_posix_socket_raw as fpraw;
use fuchsia_zircon as zx;
use futures::StreamExt as _;
use netstack3_core::SyncCtx;
use tracing::error;
use zx::{HandleBased, Peered};

use crate::bindings::{BindingsNonSyncCtxImpl, Ctx};

use super::{
    worker::{self, CloseResponder, SocketWorker, SocketWorkerHandler, TaskSpawnerCollection},
    SocketWorkerProperties, ZXSIO_SIGNAL_OUTGOING,
};

#[derive(Debug)]
struct BindingData {
    /// The event to hand off for [`fpraw::SocketRequest::Describe`].
    peer_event: zx::EventPair,
}

impl BindingData {
    fn new(
        _sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        _non_sync_ctx: &mut BindingsNonSyncCtxImpl,
        _domain: fposix_socket::Domain,
        _proto: fpraw::ProtocolAssociation,
        SocketWorkerProperties {}: SocketWorkerProperties,
    ) -> Self {
        let (local_event, peer_event) = zx::EventPair::create();
        match local_event.signal_peer(zx::Signals::NONE, ZXSIO_SIGNAL_OUTGOING) {
            Ok(()) => (),
            Err(e) => error!("socket failed to signal peer: {:?}", e),
        };

        BindingData { peer_event }
    }
}

impl CloseResponder for fpraw::SocketCloseResponder {
    fn send(self, response: Result<(), i32>) -> Result<(), fidl::Error> {
        fpraw::SocketCloseResponder::send(self, response)
    }
}

impl SocketWorkerHandler for BindingData {
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

    fn close(
        self,
        _sync_ctx: &SyncCtx<BindingsNonSyncCtxImpl>,
        _non_sync_ctx: &mut BindingsNonSyncCtxImpl,
    ) {
    }
}

struct RequestHandler<'a> {
    ctx: &'a Ctx,
    data: &'a mut BindingData,
}

impl<'a> RequestHandler<'a> {
    fn describe(self) -> fpraw::SocketDescribeResponse {
        let Self { ctx: _ctx, data: BindingData { peer_event } } = self;
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
        match request {
            fpraw::SocketRequest::Clone2 { request, control_handle: _ } => {
                let channel = fidl::AsyncChannel::from_channel(request.into_channel())
                    .expect("failed to create async channel");
                let stream = fpraw::SocketRequestStream::from_channel(channel);
                return ControlFlow::Continue(Some(stream));
            }
            fpraw::SocketRequest::Describe { responder } => {
                responder
                    .send(self.describe())
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
            }
            fpraw::SocketRequest::Close { responder } => return ControlFlow::Break(responder),
            fpraw::SocketRequest::Query { responder } => responder
                .send(fpraw::SOCKET_PROTOCOL_NAME.as_bytes())
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetReuseAddress { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetReuseAddress { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetError { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetBroadcast { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetBroadcast { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetSendBuffer { value_bytes: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetSendBuffer { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetReceiveBuffer { value_bytes: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetReceiveBuffer { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetKeepAlive { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetKeepAlive { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetOutOfBandInline { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetOutOfBandInline { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetNoCheck { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetNoCheck { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetLinger { linger: _, length_secs: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetLinger { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetReusePort { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetReusePort { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetAcceptConn { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetBindToDevice { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetBindToDevice { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetTimestamp { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetTimestamp { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::Bind { addr: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::Connect { addr: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::Disconnect { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetSockName { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetPeerName { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::Shutdown { mode: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpTypeOfService { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpTypeOfService { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpTtl { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpTtl { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpPacketInfo { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpPacketInfo { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpReceiveTypeOfService { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpReceiveTypeOfService { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpReceiveTtl { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpReceiveTtl { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpMulticastInterface { iface: _, address: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"))
            }
            fpraw::SocketRequest::GetIpMulticastInterface { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpMulticastTtl { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpMulticastTtl { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpMulticastLoopback { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpMulticastLoopback { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::AddIpMembership { membership: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::DropIpMembership { membership: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpTransparent { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpTransparent { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpReceiveOriginalDestinationAddress {
                value: _,
                responder,
            } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpReceiveOriginalDestinationAddress { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::AddIpv6Membership { membership: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::DropIpv6Membership { membership: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6MulticastInterface { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6MulticastInterface { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6UnicastHops { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6UnicastHops { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6ReceiveHopLimit { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6ReceiveHopLimit { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6MulticastHops { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6MulticastHops { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6MulticastLoopback { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6MulticastLoopback { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6Only { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6Only { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6ReceiveTrafficClass { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6ReceiveTrafficClass { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6TrafficClass { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6TrafficClass { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6ReceivePacketInfo { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6ReceivePacketInfo { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetOriginalDestination { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::RecvMsg {
                want_addr: _,
                data_len: _,
                want_control: _,
                flags: _,
                responder,
            } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SendMsg { addr: _, data: _, control: _, flags: _, responder } => {
                responder
                    .send(Err(fposix::Errno::Eopnotsupp))
                    .unwrap_or_else(|e| error!("failed to respond: {e:?}"))
            }
            fpraw::SocketRequest::GetInfo { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpHeaderIncluded { value: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpHeaderIncluded { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIcmpv6Filter { filter: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIcmpv6Filter { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::SetIpv6Checksum { config: _, responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
            fpraw::SocketRequest::GetIpv6Checksum { responder } => responder
                .send(Err(fposix::Errno::Eopnotsupp))
                .unwrap_or_else(|e| error!("failed to respond: {e:?}")),
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
                fpraw::ProviderRequest::Socket { responder, domain, proto } => {
                    let (client, request_stream) = fidl::endpoints::create_request_stream()
                        .expect("failed to create a new request stream");

                    spawner.spawn(SocketWorker::serve_stream_with(
                        ctx.clone(),
                        move |sync_ctx, non_sync_ctx, properties| {
                            BindingData::new(sync_ctx, non_sync_ctx, domain, proto, properties)
                        },
                        SocketWorkerProperties {},
                        request_stream,
                        (),
                        spawner.clone(),
                    ));
                    responder
                        .send(Ok(client))
                        .unwrap_or_else(|e| error!("failed to respond: {e:?}"));
                }
            }
        })
        .collect::<()>()
        .await;
    wait_group
}
