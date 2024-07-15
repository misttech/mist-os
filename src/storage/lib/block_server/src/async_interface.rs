// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{DecodedRequest, GroupOrRequest, IntoSessionManager, Operation, SessionHelper};
use anyhow::Error;
use block_protocol::{BlockFifoRequest, BlockFifoResponse};
use fuchsia_async::{self as fasync, FifoReadable as _, FifoWritable as _};
use futures::stream::{FuturesUnordered, StreamExt as _};
use futures::FutureExt;
use std::future::Future;
use std::mem::MaybeUninit;
use std::sync::Arc;
use {fidl_fuchsia_hardware_block as fblock, fuchsia_zircon as zx};

pub trait Interface: Send + Sync + Unpin + 'static {
    /// Called for a request to read bytes.
    fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called for a request to write bytes.
    fn write(
        &self,
        device_block_offset: u64,
        length: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to flush the device.
    fn flush(&self) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to trim a region.
    fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;
}

pub struct SessionManager<I> {
    interface: Arc<I>,
}

impl<I: Interface> super::SessionManager for SessionManager<I> {
    async fn open_session(
        self: Arc<Self>,
        stream: fblock::SessionRequestStream,
        block_size: u32,
    ) -> Result<(), Error> {
        let (helper, fifo) = SessionHelper::new(block_size)?;
        let interface = self.interface.clone();
        let mut inflight_requests = FuturesUnordered::new();
        let mut stream = stream.fuse();
        let mut requests = [MaybeUninit::<BlockFifoRequest>::uninit(); 64];
        let fifo = fasync::Fifo::from_fifo(fifo);

        loop {
            futures::select! {
                req = stream.next() => {
                    let Some(req) = req else { return Ok(()) };
                    helper.handle_request(req?)?;
                }
                result = fifo.read_entries(&mut requests[..]).fuse() => {
                    let count = result?;
                    for request in &requests[..count] {
                        if let Some(decoded_request) =
                            helper.decode_fifo_request(unsafe { request.assume_init_ref() })
                        {
                            let interface = interface.clone();
                            inflight_requests.push(async move {
                                let group_or_request = decoded_request.group_or_request;
                                let status =
                                    process_fifo_request(interface, decoded_request).await.into();
                                (group_or_request, status)
                            });
                        }
                    }
                }
                result = inflight_requests.next() => {
                    if let Some((group_or_request, status)) = result {
                        match group_or_request {
                            GroupOrRequest::Group(group_id) => {
                                if let Some(response) =
                                    helper.message_groups.complete(group_id, status)
                                {
                                    fifo.write_entries(&response).await?;
                                }
                            }
                            GroupOrRequest::Request(reqid) => {
                                fifo
                                    .write_entries(&BlockFifoResponse {
                                        status: status.into_raw(),
                                        reqid,
                                        ..Default::default()
                                    })
                                    .await?;
                            }
                        }
                    }
                }
            }
        }
    }
}

impl<I: Interface> IntoSessionManager for Arc<I> {
    type SM = SessionManager<I>;

    fn into_session_manager(self) -> Arc<Self::SM> {
        Arc::new(SessionManager { interface: self })
    }
}

/// Processes a fifo request.
async fn process_fifo_request<I: Interface>(
    interface: Arc<I>,
    r: DecodedRequest,
) -> Result<(), zx::Status> {
    match r.operation? {
        Operation::Read { device_block_offset, block_count, vmo_offset } => {
            interface
                .read(device_block_offset, block_count, &r.vmo.as_ref().unwrap(), vmo_offset)
                .await
        }
        Operation::Write { device_block_offset, block_count, vmo_offset } => {
            interface
                .write(device_block_offset, block_count, &r.vmo.as_ref().unwrap(), vmo_offset)
                .await
        }
        Operation::Flush => interface.flush().await,
        Operation::Trim { device_block_offset, block_count } => {
            interface.trim(device_block_offset, block_count).await
        }
        Operation::CloseVmo => Ok(()), // The caller did all that was required.
    }
}
