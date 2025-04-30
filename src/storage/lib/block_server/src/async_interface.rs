// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    DecodeResult, DecodedRequest, DeviceInfo, IntoSessionManager, OffsetMap, Operation,
    SessionHelper, FIFO_MAX_REQUESTS,
};
use anyhow::Error;
use block_protocol::{BlockFifoRequest, BlockFifoResponse, WriteOptions};
use futures::future::Fuse;
use futures::stream::FuturesUnordered;
use futures::{select_biased, FutureExt, StreamExt};
use std::borrow::Cow;
use std::future::{poll_fn, Future};
use std::mem::MaybeUninit;
use std::num::NonZero;
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
        trace_flow_id: Option<NonZero<u64>>,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called for a request to write bytes.
    fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        opts: WriteOptions,
        trace_flow_id: Option<NonZero<u64>>,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to flush the device.
    fn flush(
        &self,
        trace_flow_id: Option<NonZero<u64>>,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to trim a region.
    fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
        trace_flow_id: Option<NonZero<u64>>,
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

pub struct SessionManager<I: ?Sized> {
    interface: Arc<I>,
}

impl<I: Interface + ?Sized> SessionManager<I> {
    pub fn new(interface: Arc<I>) -> Self {
        Self { interface }
    }

    /// Runs `stream` until completion.
    pub async fn serve_session(
        self: Arc<Self>,
        stream: fblock::SessionRequestStream,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> Result<(), Error> {
        let (helper, fifo) = SessionHelper::new(self.clone(), offset_map, block_size)?;
        let helper = Arc::new(helper);
        let interface = self.interface.clone();

        let mut stream = stream.fuse();

        let scope = fasync::Scope::new();
        let helper_clone = helper.clone();
        let mut fifo_task = scope
            .spawn(async move {
                if let Err(status) = run_fifo(fifo, interface, helper).await {
                    if status != zx::Status::PEER_CLOSED {
                        log::error!(status:?; "FIFO error");
                    }
                }
            })
            .fuse();

        // Make sure we detach VMOs when we go out of scope.
        scopeguard::defer! {
            for (_, vmo) in helper_clone.take_vmos() {
                self.interface.on_detach_vmo(&vmo);
            }
        }

        loop {
            futures::select! {
                maybe_req = stream.next() => {
                    if let Some(req) = maybe_req {
                        helper_clone.handle_request(req?).await?;
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

// A task loop for receiving and responding to FIFO requests.
async fn run_fifo<I: Interface + ?Sized>(
    fifo: zx::Fifo<BlockFifoRequest, BlockFifoResponse>,
    interface: Arc<I>,
    helper: Arc<SessionHelper<SessionManager<I>>>,
) -> Result<(), zx::Status> {
    // The FIFO has to be processed by a single task due to implementation constraints on
    // fuchsia_async::Fifo.  Thus, we use an event loop to drive the FIFO.  FIFO reads and writes
    // can happen in batch, and request processing is parallel.
    // The general flow is:
    //  - Read messages from the FIFO, write into `requests`.
    //  - Read `requests`, decode them, and spawn a task to process them in `active_requests`, which
    //    will eventually write them into `responses`.
    //  - Read `responses` and write out to the FIFO.
    let mut fifo = fasync::Fifo::from_fifo(fifo);
    let (mut reader, mut writer) = fifo.async_io();
    let mut requests = [MaybeUninit::<BlockFifoRequest>::uninit(); FIFO_MAX_REQUESTS];
    let mut active_requests = FuturesUnordered::new();
    let mut responses = vec![];

    loop {
        let new_requests = {
            // We provide some flow control by limiting how many in-flight requests we will allow.
            // Due to request splitting, there might be more active requests than there are request
            // slots.
            let pending_requests = active_requests.len() + responses.len();
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
                response = active_requests.select_next_some() => {
                    responses.extend(response);
                    0
                }
                count = receive_requests => {
                    count?
                }
            )
        };

        let process_request =
            async |interface: &Arc<I>,
                   helper: &Arc<SessionHelper<SessionManager<I>>>,
                   decoded_request: DecodedRequest| {
                let tracking = decoded_request.request_tracking;
                let status = process_fifo_request(interface.clone(), decoded_request).await.into();
                helper.finish_fifo_request(tracking, status)
            };
        // NB: It is very important that there are no `await`s for the rest of the loop body, as
        // otherwise active requests might become stalled.
        let mut i = 0;
        let mut in_split = false;
        while i < new_requests {
            let request = &mut requests[i];
            i += 1;
            match helper.decode_fifo_request(unsafe { request.assume_init_mut() }, in_split) {
                DecodeResult::Ok(decoded_request) => {
                    in_split = false;
                    if let Operation::CloseVmo = decoded_request.operation {
                        if let Some(vmo) = decoded_request.vmo {
                            interface.on_detach_vmo(vmo.as_ref());
                        }
                        responses.extend(
                            helper.finish_fifo_request(
                                decoded_request.request_tracking,
                                zx::Status::OK,
                            ),
                        );
                    } else {
                        active_requests.push(process_request(&interface, &helper, decoded_request));
                    }
                }
                DecodeResult::Split(decoded_request) => {
                    active_requests.push(process_request(&interface, &helper, decoded_request));
                    // Re-process the request
                    in_split = true;
                    i -= 1;
                }
                DecodeResult::InvalidRequest(tracking, status) => {
                    in_split = false;
                    responses.extend(helper.finish_fifo_request(tracking, status));
                }
                DecodeResult::IgnoreRequest => {
                    in_split = false;
                }
            }
        }
    }
}

impl<I: Interface + ?Sized> super::SessionManager for SessionManager<I> {
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
}

impl<I: Interface> IntoSessionManager for Arc<I> {
    type SM = SessionManager<I>;

    fn into_session_manager(self) -> Arc<Self::SM> {
        Arc::new(SessionManager { interface: self })
    }
}

/// Processes a fifo request.
async fn process_fifo_request<I: Interface + ?Sized>(
    interface: Arc<I>,
    r: DecodedRequest,
) -> Result<(), zx::Status> {
    let trace_flow_id = r.request_tracking.trace_flow_id;
    match r.operation {
        Operation::Read { device_block_offset, block_count, _unused, vmo_offset } => {
            interface
                .read(
                    device_block_offset,
                    block_count,
                    &r.vmo.as_ref().unwrap(),
                    vmo_offset,
                    trace_flow_id,
                )
                .await
        }
        Operation::Write { device_block_offset, block_count, options, vmo_offset } => {
            interface
                .write(
                    device_block_offset,
                    block_count,
                    &r.vmo.as_ref().unwrap(),
                    vmo_offset,
                    options,
                    trace_flow_id,
                )
                .await
        }
        Operation::Flush => interface.flush(trace_flow_id).await,
        Operation::Trim { device_block_offset, block_count } => {
            interface.trim(device_block_offset, block_count, trace_flow_id).await
        }
        Operation::CloseVmo => {
            // Handled in main request loop
            unreachable!()
        }
    }
}
