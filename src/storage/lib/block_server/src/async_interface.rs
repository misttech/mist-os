// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{DecodedRequest, IntoSessionManager, Operation, PartitionInfo, SessionHelper};
use anyhow::Error;
use block_protocol::{BlockFifoRequest, WriteOptions};
use fuchsia_async::{self as fasync, FifoReadable as _, FifoWritable as _};
use futures::stream::StreamExt as _;
use futures::FutureExt;
use std::borrow::Cow;
use std::future::Future;
use std::mem::MaybeUninit;
use std::sync::Arc;
use {fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_volume as fvolume};

pub trait Interface: Send + Sync + Unpin + 'static {
    /// Called whenever a VMO is attached, prior to the VMO's usage in any other methods.  Whilst
    /// the VMO is attached, `vmo` will keep the same address so it is safe to use the pointer
    /// value (as, say, a key into a HashMap).
    fn on_attach_vmo(&self, _vmo: &zx::Vmo) -> impl Future<Output = Result<(), zx::Status>> + Send {
        async { Ok(()) }
    }

    /// Called whenever a VMO is detached.
    fn on_detach_vmo(&self, _vmo: &zx::Vmo) {}

    /// Called to get partition information.
    fn get_info(&self) -> impl Future<Output = Result<Cow<'_, PartitionInfo>, zx::Status>> + Send;

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
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        opts: WriteOptions,
    ) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to flush the device.
    fn flush(&self) -> impl Future<Output = Result<(), zx::Status>> + Send;

    /// Called to trim a region.
    fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
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

pub struct SessionManager<I> {
    interface: Arc<I>,
}

impl<I: Interface> SessionManager<I> {
    pub fn new(interface: Arc<I>) -> Self {
        Self { interface }
    }
}

impl<I: Interface> super::SessionManager for SessionManager<I> {
    async fn on_attach_vmo(self: Arc<Self>, vmo: &Arc<zx::Vmo>) -> Result<(), zx::Status> {
        self.interface.on_attach_vmo(vmo).await
    }

    async fn open_session(
        self: Arc<Self>,
        stream: fblock::SessionRequestStream,
        block_size: u32,
    ) -> Result<(), Error> {
        let (helper, fifo) = SessionHelper::new(self.clone(), block_size)?;
        let helper = Arc::new(helper);
        let interface = self.interface.clone();

        let mut stream = stream.fuse();
        let fifo = Arc::new(fasync::Fifo::from_fifo(fifo));

        let scope = fasync::Scope::new();
        let scope_ref = scope.clone();
        let helper_clone = helper.clone();
        let mut fifo_task = scope
            .spawn(async move {
                let mut requests = [MaybeUninit::<BlockFifoRequest>::uninit(); 64];
                while let Ok(count) = fifo.read_entries(&mut requests[..]).await {
                    for request in &requests[..count] {
                        if let Some(decoded_request) =
                            helper.decode_fifo_request(unsafe { request.assume_init_ref() })
                        {
                            let interface = interface.clone();
                            let fifo = fifo.clone();
                            let helper = helper.clone();
                            scope_ref.spawn(async move {
                                let tracking = decoded_request.request_tracking;
                                let status =
                                    process_fifo_request(interface, decoded_request).await.into();
                                if let Some(response) = helper.finish_fifo_request(tracking, status)
                                {
                                    if let Err(_) = fifo.write_entries(&response).await {
                                        return;
                                    }
                                }
                            });
                        }
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

    async fn get_info(&self) -> Result<Cow<'_, PartitionInfo>, zx::Status> {
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
async fn process_fifo_request<I: Interface>(
    interface: Arc<I>,
    r: DecodedRequest,
) -> Result<(), zx::Status> {
    match r.operation? {
        Operation::Read { device_block_offset, block_count, _unused, vmo_offset } => {
            interface
                .read(device_block_offset, block_count, &r.vmo.as_ref().unwrap(), vmo_offset)
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
                )
                .await
        }
        Operation::Flush => interface.flush().await,
        Operation::Trim { device_block_offset, block_count } => {
            interface.trim(device_block_offset, block_count).await
        }
        Operation::CloseVmo => {
            if let Some(vmo) = &r.vmo {
                interface.on_detach_vmo(vmo);
            }
            Ok(())
        }
    }
}
