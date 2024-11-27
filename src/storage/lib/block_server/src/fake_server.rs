// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use block_server::async_interface::{Interface, SessionManager};
use block_server::{BlockServer, PartitionInfo, WriteOptions};
use fidl::endpoints::Proxy as _;
use std::borrow::Cow;
use std::sync::Arc;
use {
    fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_volume as fvolume,
    fuchsia_async as fasync,
};

pub const TYPE_GUID: [u8; 16] = [1; 16];
pub const INSTANCE_GUID: [u8; 16] = [2; 16];
pub const PARTITION_NAME: &str = "fake-server";

/// The Observer can silently discard writes, or fail them explicitly (zx::Status::IO is returned).
pub enum WriteAction {
    Write,
    Discard,
    Fail,
}

pub trait Observer: Send + Sync {
    fn read(
        &self,
        _device_block_offset: u64,
        _block_count: u32,
        _vmo: &Arc<zx::Vmo>,
        _vmo_offset: u64,
    ) {
    }

    fn write(
        &self,
        _device_block_offset: u64,
        _block_count: u32,
        _vmo: &Arc<zx::Vmo>,
        _vmo_offset: u64,
        _opts: WriteOptions,
    ) -> WriteAction {
        WriteAction::Write
    }

    fn flush(&self) {}

    fn trim(&self, _device_block_offset: u64, _block_count: u32) {}
}

pub struct FakeServer {
    server: BlockServer<SessionManager<Data>>,
}

pub struct FakeServerOptions<'a> {
    pub block_count: Option<u64>,
    pub block_size: u32,
    pub initial_content: Option<&'a [u8]>,
    pub vmo: Option<zx::Vmo>,
    pub observer: Option<Box<dyn Observer>>,
}

impl Default for FakeServerOptions<'_> {
    fn default() -> Self {
        FakeServerOptions {
            block_count: None,
            block_size: 512,
            initial_content: None,
            vmo: None,
            observer: None,
        }
    }
}

impl From<FakeServerOptions<'_>> for FakeServer {
    fn from(options: FakeServerOptions<'_>) -> Self {
        let (vmo, block_count) = if let Some(vmo) = options.vmo {
            let size = vmo.get_size().unwrap();
            debug_assert!(size % options.block_size as u64 == 0);
            let block_count = size / options.block_size as u64;
            if let Some(bc) = options.block_count {
                assert_eq!(block_count, bc);
            }
            (vmo, block_count)
        } else {
            let block_count = options.block_count.unwrap();
            (zx::Vmo::create(block_count * options.block_size as u64).unwrap(), block_count)
        };

        if let Some(initial_content) = options.initial_content {
            vmo.write(initial_content, 0).unwrap();
        }

        Self {
            server: BlockServer::new(
                options.block_size,
                Arc::new(Data {
                    block_size: options.block_size,
                    block_count: block_count,
                    data: vmo,
                    observer: options.observer,
                }),
            ),
        }
    }
}

impl FakeServer {
    pub fn new(block_count: u64, block_size: u32, initial_content: &[u8]) -> Self {
        FakeServerOptions {
            block_count: Some(block_count),
            block_size,
            initial_content: Some(initial_content),
            ..Default::default()
        }
        .into()
    }

    pub fn from_vmo(block_size: u32, vmo: zx::Vmo) -> Self {
        FakeServerOptions { block_size, vmo: Some(vmo), ..Default::default() }.into()
    }

    pub async fn serve(&self, requests: fvolume::VolumeRequestStream) -> Result<(), Error> {
        self.server.handle_requests(requests).await
    }

    pub fn volume_proxy(self: &Arc<Self>) -> fvolume::VolumeProxy {
        let (client, stream) = fidl::endpoints::create_proxy_and_stream::<fvolume::VolumeMarker>();
        let this = self.clone();
        fasync::Task::spawn(async move {
            let _ = this.serve(stream).await;
        })
        .detach();
        client
    }

    pub fn block_proxy(self: &Arc<Self>) -> fblock::BlockProxy {
        fblock::BlockProxy::from_channel(self.volume_proxy().into_channel().unwrap())
    }
}

struct Data {
    block_size: u32,
    block_count: u64,
    data: zx::Vmo,
    observer: Option<Box<dyn Observer>>,
}

impl Interface for Data {
    async fn get_info(&self) -> Result<Cow<'_, PartitionInfo>, zx::Status> {
        Ok(Cow::Owned(PartitionInfo {
            block_range: Some(0..self.block_count),
            type_guid: TYPE_GUID.clone(),
            instance_guid: INSTANCE_GUID.clone(),
            name: Some(PARTITION_NAME.to_string()),
            flags: 0u64,
        }))
    }

    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.read(device_block_offset, block_count, vmo, vmo_offset);
        }
        vmo.write(
            &self.data.read_to_vec(
                device_block_offset * self.block_size as u64,
                block_count as u64 * self.block_size as u64,
            )?,
            vmo_offset,
        )
    }

    async fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
        opts: WriteOptions,
    ) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            match observer.write(device_block_offset, block_count, vmo, vmo_offset, opts) {
                WriteAction::Write => {}
                WriteAction::Discard => return Ok(()),
                WriteAction::Fail => return Err(zx::Status::IO),
            }
        }
        self.data.write(
            &vmo.read_to_vec(vmo_offset, block_count as u64 * self.block_size as u64)?,
            device_block_offset * self.block_size as u64,
        )
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.flush();
        }
        Ok(())
    }

    async fn trim(&self, device_block_offset: u64, block_count: u32) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.trim(device_block_offset, block_count);
        }
        Ok(())
    }
}
