// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    ActiveRequests, DecodedRequest, DeviceInfo, IntoSessionManager, OffsetMap, Operation,
    SessionHelper, TraceFlowId, FIFO_MAX_REQUESTS,
};
use anyhow::Error;
use block_protocol::{BlockFifoRequest, BlockFifoResponse, WriteOptions};
use futures::future::{Fuse, FusedFuture};
use futures::stream::FuturesUnordered;
use futures::{select_biased, FutureExt, StreamExt};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::mem::MaybeUninit;
use std::pin::pin;
use std::sync::Arc;
use std::task::{ready, Poll};
use {
    fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_volume as fvolume,
    fuchsia_async as fasync,
};

pub trait Interface: Send + Sync + Unpin + 'static {
    /// Runs `stream` to completion.
    ///
    /// Implementors can override this method if they want to create a passthrough session instead
    /// (and can use `[PassthroughSession]` below to do so).  See
    /// fuchsia.hardware.block.Block/OpenSessionWithOffsetMap.
    ///
    /// If the implementor uses a `[PassthroughSession]`, the following Interface methods
    /// will not be called, and can be stubbed out:
    ///   - on_attach_vmo
    ///   - on_detach_vmo
    ///   - read
    ///   - write
    ///   - flush
    ///   - trim
    fn open_session(
        &self,
        session_manager: Arc<SessionManager<Self>>,
        stream: fblock::SessionRequestStream,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> impl Future<Output = Result<(), Error>> + Send {
        // By default, serve the session rather than forwarding/proxying it.
        session_manager.serve_session(stream, offset_map, block_size)
    }

    /// Called whenever a VMO is attached, prior to the VMO's usage in any other methods.  Whilst
    /// the VMO is attached, `vmo` will keep the same address so it is safe to use the pointer
    /// value (as, say, a key into a HashMap).
    fn on_attach_vmo(&self, _vmo: &zx::Vmo) -> impl Future<Output = Result<(), zx::Status>> + Send {
        async { Ok(()) }
    }

    /// Called whenever a VMO is detached.
    fn on_detach_vmo(&self, _vmo: &zx::Vmo) {}

    /// Called to get block/partition information.
    fn get_info(&self) -> impl Future<Output = Result<Cow<'_, DeviceInfo>, zx::Status>> + Send;

    /// Called for a request to read bytes.
    fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        trace_flow_id: TraceFlowId,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called for a request to write bytes.
    fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        opts: WriteOptions,
        trace_flow_id: TraceFlowId,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to flush the device.
    fn flush(
        &self,
        trace_flow_id: TraceFlowId,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to trim a region.
    fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
        trace_flow_id: TraceFlowId,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to handle the GetVolumeInfo FIDL call.
    fn get_volume_info(
        &self,
    ) -> impl Future<Output = Result<(fvolume::VolumeManagerInfo, fvolume::VolumeInfo), zx::Status>> + Send
    {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Called to handle the QuerySlices FIDL call.
    fn query_slices(
        &self,
        _start_slices: &[u64],
    ) -> impl Future<Output = Result<Vec<fvolume::VsliceRange>, zx::Status>> + Send {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Called to handle the Extend FIDL call.
    fn extend(
        &self,
        _start_slice: u64,
        _slice_count: u64,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }

    /// Called to handle the Shrink FIDL call.
    fn shrink(
        &self,
        _start_slice: u64,
        _slice_count: u64,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send {
        async { Err(zx::Status::NOT_SUPPORTED) }
    }
}

/// A helper object to run a passthrough (proxy) session.
pub struct PassthroughSession(fblock::SessionProxy);

impl PassthroughSession {
    pub fn new(proxy: fblock::SessionProxy) -> Self {
        Self(proxy)
    }

    async fn handle_request(&self, request: fblock::SessionRequest) -> Result<(), Error> {
        match request {
            fblock::SessionRequest::GetFifo { responder } => {
                responder.send(self.0.get_fifo().await?)?;
            }
            fblock::SessionRequest::AttachVmo { vmo, responder } => {
                responder.send(self.0.attach_vmo(vmo).await?.as_ref().map_err(|s| *s))?;
            }
            fblock::SessionRequest::Close { responder } => {
                responder.send(self.0.close().await?)?;
            }
        }
        Ok(())
    }

    /// Runs `stream` until completion.
    pub async fn serve(&self, mut stream: fblock::SessionRequestStream) -> Result<(), Error> {
        while let Some(Ok(request)) = stream.next().await {
            if let Err(error) = self.handle_request(request).await {
                log::warn!(error:?; "FIDL error");
            }
        }
        Ok(())
    }
}

pub struct SessionManager<I: Interface + ?Sized> {
    interface: Arc<I>,
    active_requests: ActiveRequests<usize>,
}

impl<I: Interface + ?Sized> SessionManager<I> {
    pub fn new(interface: Arc<I>) -> Self {
        Self { interface, active_requests: ActiveRequests::default() }
    }

    /// Runs `stream` until completion.
    pub async fn serve_session(
        self: Arc<Self>,
        stream: fblock::SessionRequestStream,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> Result<(), Error> {
        let (helper, fifo) = SessionHelper::new(self.clone(), offset_map, block_size)?;
        let session = Session { helper: Arc::new(helper), interface: self.interface.clone() };
        let mut stream = stream.fuse();
        let scope = fasync::Scope::new();
        let helper = session.helper.clone();
        let mut fifo_task = scope
            .spawn(async move {
                if let Err(status) = session.run_fifo(fifo).await {
                    if status != zx::Status::PEER_CLOSED {
                        log::error!(status:?; "FIFO error");
                    }
                }
            })
            .fuse();

        // Make sure we detach VMOs when we go out of scope.
        scopeguard::defer! {
            for (_, vmo) in helper.take_vmos() {
                self.interface.on_detach_vmo(&vmo);
            }
        }

        loop {
            futures::select! {
                maybe_req = stream.next() => {
                    if let Some(req) = maybe_req {
                        helper.handle_request(req?).await?;
                    } else {
                        break;
                    }
                }
                _ = fifo_task => break,
            }
        }

        Ok(())
    }
}

struct Session<I: Interface + ?Sized> {
    interface: Arc<I>,
    helper: Arc<SessionHelper<SessionManager<I>>>,
}

impl<I: Interface + ?Sized> Session<I> {
    // A task loop for receiving and responding to FIFO requests.
    async fn run_fifo(
        &self,
        fifo: zx::Fifo<BlockFifoRequest, BlockFifoResponse>,
    ) -> Result<(), zx::Status> {
        scopeguard::defer! {
            self.helper.drop_active_requests(|session| *session == self as *const _ as usize);
        }

        // The FIFO has to be processed by a single task due to implementation constraints on
        // fuchsia_async::Fifo.  Thus, we use an event loop to drive the FIFO.  FIFO reads and
        // writes can happen in batch, and request processing is parallel.
        //
        // The general flow is:
        //  - Read messages from the FIFO, write into `requests`.
        //  - Read `requests`, decode them, and spawn a task to process them in `active_requests`,
        //    which will eventually write them into `responses`.
        //  - Read `responses` and write out to the FIFO.
        let mut fifo = fasync::Fifo::from_fifo(fifo);
        let (mut reader, mut writer) = fifo.async_io();
        let mut requests = [MaybeUninit::<BlockFifoRequest>::uninit(); FIFO_MAX_REQUESTS];
        let active_requests = &self.helper.session_manager.active_requests;
        let mut active_request_futures = FuturesUnordered::new();
        let mut responses = Vec::new();

        // We map requests using a single future `map_future`.  `pending_mappings` is used to queue
        // up requests that need to be mapped.  This will serialise how mappings occur which might
        // make updating mapping caches simpler.  If this proves to be a performance issue, we can
        // optimise it.
        let mut map_future = pin!(Fuse::terminated());
        let mut pending_mappings = VecDeque::new();

        loop {
            let new_requests = {
                // We provide some flow control by limiting how many in-flight requests we will
                // allow.
                let pending_requests = active_request_futures.len() + responses.len();
                let count = requests.len().saturating_sub(pending_requests);
                let mut receive_requests = pin!(if count == 0 {
                    Fuse::terminated()
                } else {
                    reader.read_entries(&mut requests[..count]).fuse()
                });
                let mut send_responses = pin!(if responses.is_empty() {
                    Fuse::terminated()
                } else {
                    poll_fn(|cx| -> Poll<Result<(), zx::Status>> {
                        match ready!(writer.try_write(cx, &responses[..])) {
                            Ok(written) => {
                                responses.drain(..written);
                                Poll::Ready(Ok(()))
                            }
                            Err(status) => Poll::Ready(Err(status)),
                        }
                    })
                    .fuse()
                });

                // Order is important here.  We want to prioritize sending results on the FIFO and
                // processing FIFO messages over receiving new ones, to provide flow control.
                select_biased!(
                    res = send_responses => {
                        res?;
                        0
                    },
                    response = active_request_futures.select_next_some() => {
                        responses.extend(response);
                        0
                    }
                    result = map_future => {
                        match result {
                            Ok((request, remainder)) => {
                                active_request_futures.push(self.process_fifo_request(request));
                                if let Some(remainder) = remainder {
                                    map_future.set(self.map_request(remainder).fuse());
                                }
                            }
                            Err(response) => responses.extend(response),
                        }
                        if map_future.is_terminated() {
                            if let Some(request) = pending_mappings.pop_front() {
                                map_future.set(self.map_request(request).fuse());
                            }
                        }
                        0
                    }
                    count = receive_requests => {
                        count?
                    }
                )
            };

            // NB: It is very important that there are no `await`s for the rest of the loop body, as
            // otherwise active requests might become stalled.
            for request in &mut requests[..new_requests] {
                match self.helper.decode_fifo_request(self as *const _ as usize, unsafe {
                    request.assume_init_mut()
                }) {
                    Ok(DecodedRequest {
                        operation: Operation::CloseVmo, vmo, request_id, ..
                    }) => {
                        if let Some(vmo) = vmo {
                            self.interface.on_detach_vmo(vmo.as_ref());
                        }
                        responses.extend(
                            active_requests
                                .complete_and_take_response(request_id, zx::Status::OK)
                                .map(|(_, response)| response),
                        );
                    }
                    Ok(request) => {
                        if map_future.is_terminated() {
                            map_future.set(self.map_request(request).fuse());
                        } else {
                            pending_mappings.push_back(request);
                        }
                    }
                    Err(None) => {}
                    Err(Some(response)) => responses.push(response),
                }
            }
        }
    }

    // This is currently async when it doesn't need to be to allow for upcoming changes.
    async fn map_request(
        &self,
        request: DecodedRequest,
    ) -> Result<(DecodedRequest, Option<DecodedRequest>), Option<BlockFifoResponse>> {
        self.helper.map_request(request)
    }

    /// Processes a fifo request.
    async fn process_fifo_request(
        &self,
        DecodedRequest { request_id, operation, vmo, trace_flow_id }: DecodedRequest,
    ) -> Option<BlockFifoResponse> {
        let result = match operation {
            Operation::Read { device_block_offset, block_count, _unused, vmo_offset } => {
                self.interface
                    .read(
                        device_block_offset,
                        block_count,
                        vmo.as_ref().unwrap(),
                        vmo_offset,
                        trace_flow_id,
                    )
                    .await
            }
            Operation::Write { device_block_offset, block_count, options, vmo_offset } => {
                self.interface
                    .write(
                        device_block_offset,
                        block_count,
                        vmo.as_ref().unwrap(),
                        vmo_offset,
                        options,
                        trace_flow_id,
                    )
                    .await
            }
            Operation::Flush => self.interface.flush(trace_flow_id).await,
            Operation::Trim { device_block_offset, block_count } => {
                self.interface.trim(device_block_offset, block_count, trace_flow_id).await
            }
            Operation::CloseVmo => {
                // Handled in main request loop
                unreachable!()
            }
        };
        self.helper
            .session_manager
            .active_requests
            .complete_and_take_response(request_id, result.into())
            .map(|(_, r)| r)
    }
}

impl<I: Interface + ?Sized> super::SessionManager for SessionManager<I> {
    // We don't need the session, we just need something unique to identify the session.
    type Session = usize;

    async fn on_attach_vmo(self: Arc<Self>, vmo: &Arc<zx::Vmo>) -> Result<(), zx::Status> {
        self.interface.on_attach_vmo(vmo).await
    }

    async fn open_session(
        self: Arc<Self>,
        stream: fblock::SessionRequestStream,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> Result<(), Error> {
        self.interface.clone().open_session(self, stream, offset_map, block_size).await
    }

    async fn get_info(&self) -> Result<Cow<'_, DeviceInfo>, zx::Status> {
        self.interface.get_info().await
    }

    async fn get_volume_info(
        &self,
    ) -> Result<(fvolume::VolumeManagerInfo, fvolume::VolumeInfo), zx::Status> {
        self.interface.get_volume_info().await
    }

    async fn query_slices(
        &self,
        start_slices: &[u64],
    ) -> Result<Vec<fvolume::VsliceRange>, zx::Status> {
        self.interface.query_slices(start_slices).await
    }

    async fn extend(&self, start_slice: u64, slice_count: u64) -> Result<(), zx::Status> {
        self.interface.extend(start_slice, slice_count).await
    }

    async fn shrink(&self, start_slice: u64, slice_count: u64) -> Result<(), zx::Status> {
        self.interface.shrink(start_slice, slice_count).await
    }

    fn active_requests(&self) -> &ActiveRequests<Self::Session> {
        return &self.active_requests;
    }
}

impl<I: Interface> IntoSessionManager for Arc<I> {
    type SM = SessionManager<I>;

    fn into_session_manager(self) -> Arc<Self::SM> {
        Arc::new(SessionManager { interface: self, active_requests: ActiveRequests::default() })
    }
}
