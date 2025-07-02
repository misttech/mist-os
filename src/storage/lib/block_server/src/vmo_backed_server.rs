// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use block_server::async_interface::{Interface, SessionManager};
use block_server::{BlockInfo, BlockServer, DeviceInfo, WriteOptions};
use fidl::endpoints::ServerEnd;
use fs_management::filesystem::BlockConnector;
use std::borrow::Cow;
use std::num::NonZero;
use std::sync::Arc;
use {fidl_fuchsia_hardware_block as fblock, fidl_fuchsia_hardware_block_volume as fvolume};

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

    fn barrier(&self) {}

    fn flush(&self) {}

    fn trim(&self, _device_block_offset: u64, _block_count: u32) {}
}

/// A local server backed by a VMO.
pub struct VmoBackedServer {
    server: BlockServer<SessionManager<Data>>,
}

/// The initial contents of the VMO.  This also determines the size of the block device.
pub enum InitialContents<'a> {
    /// An empty VMO will be created with capacity for this many *blocks*.
    FromCapacity(u64),
    /// A VMO is created with capacity for this many *blocks* and the buffer's contents copied into
    /// it.
    FromBufferAndCapactity(u64, &'a [u8]),
    /// A VMO is created which is exactly large enough for the initial contents (rounded up to block
    /// size), and the buffer's contents copied into it.
    FromBuffer(&'a [u8]),
    /// The provided VMO is used.  If its size is not block-aligned, the data will be truncated.
    FromVmo(zx::Vmo),
}

pub struct VmoBackedServerOptions<'a> {
    /// NB: `block_count` is ignored as that comes from `initial_contents`.
    pub info: DeviceInfo,
    pub block_size: u32,
    pub initial_contents: InitialContents<'a>,
    pub observer: Option<Box<dyn Observer>>,
}

impl Default for VmoBackedServerOptions<'_> {
    fn default() -> Self {
        VmoBackedServerOptions {
            info: DeviceInfo::Block(BlockInfo {
                device_flags: fblock::Flag::empty(),
                block_count: 0,
                max_transfer_blocks: None,
            }),
            block_size: 512,
            initial_contents: InitialContents::FromCapacity(0),
            observer: None,
        }
    }
}

impl VmoBackedServerOptions<'_> {
    pub fn build(self) -> Result<VmoBackedServer, Error> {
        let (data, block_count) = match self.initial_contents {
            InitialContents::FromCapacity(block_count) => {
                (zx::Vmo::create(block_count * self.block_size as u64)?, block_count)
            }
            InitialContents::FromBufferAndCapactity(block_count, buf) => {
                let needed =
                    buf.len()
                        .checked_next_multiple_of(self.block_size as usize)
                        .ok_or_else(|| anyhow!("Invalid buffer size"))? as u64
                        / self.block_size as u64;
                if needed > block_count {
                    return Err(anyhow!("Not enough capacity: {needed} vs {block_count}"));
                }
                let vmo = zx::Vmo::create(block_count * self.block_size as u64)?;
                vmo.write(buf, 0)?;
                (vmo, block_count)
            }
            InitialContents::FromBuffer(buf) => {
                let block_count =
                    buf.len()
                        .checked_next_multiple_of(self.block_size as usize)
                        .ok_or_else(|| anyhow!("Invalid buffer size"))? as u64
                        / self.block_size as u64;
                let vmo = zx::Vmo::create(block_count * self.block_size as u64)?;
                vmo.write(buf, 0)?;
                (vmo, block_count)
            }
            InitialContents::FromVmo(vmo) => {
                let size = vmo.get_size()?;
                let block_count = size / self.block_size as u64;
                (vmo, block_count)
            }
        };

        let info = match self.info {
            DeviceInfo::Block(mut info) => {
                info.block_count = block_count;
                DeviceInfo::Block(info)
            }
            DeviceInfo::Partition(mut info) => {
                info.block_range = Some(0..block_count);
                DeviceInfo::Partition(info)
            }
        };
        Ok(VmoBackedServer {
            server: BlockServer::new(
                self.block_size,
                Arc::new(Data { info, block_size: self.block_size, data, observer: self.observer }),
            ),
        })
    }
}

impl VmoBackedServer {
    /// Handles `requests`.  The future will resolve when the stream terminates.
    pub async fn serve(&self, requests: fvolume::VolumeRequestStream) -> Result<(), Error> {
        self.server.handle_requests(requests).await
    }
}

/// Implements `BlockConnector` to vend connections to a VmoBackedServer.
pub struct VmoBackedServerConnector {
    scope: fuchsia_async::Scope,
    server: Arc<VmoBackedServer>,
}

impl VmoBackedServerConnector {
    pub fn new(scope: fuchsia_async::Scope, server: Arc<VmoBackedServer>) -> Self {
        Self { scope, server }
    }
}

impl BlockConnector for VmoBackedServerConnector {
    fn connect_channel_to_volume(
        &self,
        server_end: ServerEnd<fvolume::VolumeMarker>,
    ) -> Result<(), Error> {
        let server = self.server.clone();
        let _ = self.scope.spawn(async move {
            let _ = server.serve(server_end.into_stream()).await;
        });
        Ok(())
    }
}

/// Extension trait for test-only functionality.  `unwrap` is used liberally in these functions, to
/// simplify their usage in tests.
pub trait VmoBackedServerTestingExt {
    fn new(block_count: u64, block_size: u32, initial_content: &[u8]) -> Self;
    fn from_vmo(block_size: u32, vmo: zx::Vmo) -> Self;
    fn volume_proxy(self: &Arc<Self>) -> fvolume::VolumeProxy;
    fn connect(self: &Arc<Self>, server: ServerEnd<fvolume::VolumeMarker>);
    fn block_proxy(self: &Arc<Self>) -> fblock::BlockProxy;
}

impl VmoBackedServerTestingExt for VmoBackedServer {
    fn new(block_count: u64, block_size: u32, initial_content: &[u8]) -> Self {
        VmoBackedServerOptions {
            block_size,
            initial_contents: InitialContents::FromBufferAndCapactity(block_count, initial_content),
            ..Default::default()
        }
        .build()
        .unwrap()
    }
    fn from_vmo(block_size: u32, vmo: zx::Vmo) -> Self {
        VmoBackedServerOptions {
            block_size,
            initial_contents: InitialContents::FromVmo(vmo),
            ..Default::default()
        }
        .build()
        .unwrap()
    }

    fn volume_proxy(self: &Arc<Self>) -> fvolume::VolumeProxy {
        let (client, server) = fidl::endpoints::create_endpoints();
        self.connect(server);
        client.into_proxy()
    }

    fn connect(self: &Arc<Self>, server: ServerEnd<fvolume::VolumeMarker>) {
        let this = self.clone();
        fuchsia_async::Task::spawn(async move {
            let _ = this.serve(server.into_stream()).await;
        })
        .detach();
    }

    fn block_proxy(self: &Arc<Self>) -> fblock::BlockProxy {
        use fidl::endpoints::Proxy as _;
        fblock::BlockProxy::from_channel(self.volume_proxy().into_channel().unwrap())
    }
}

struct Data {
    info: DeviceInfo,
    block_size: u32,
    data: zx::Vmo,
    observer: Option<Box<dyn Observer>>,
}

impl Interface for Data {
    async fn get_info(&self) -> Result<Cow<'_, DeviceInfo>, zx::Status> {
        Ok(Cow::Borrowed(&self.info))
    }

    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
        _trace_flow_id: Option<NonZero<u64>>,
    ) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.read(device_block_offset, block_count, vmo, vmo_offset);
        }
        if let Some(max) = self.info.max_transfer_blocks().as_ref() {
            // Requests should be split up by the core library
            assert!(block_count <= max.get());
        }
        if device_block_offset + block_count as u64 > self.info.block_count().unwrap() {
            Err(zx::Status::OUT_OF_RANGE)
        } else {
            vmo.write(
                &self.data.read_to_vec(
                    device_block_offset * self.block_size as u64,
                    block_count as u64 * self.block_size as u64,
                )?,
                vmo_offset,
            )
        }
    }

    async fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
        opts: WriteOptions,
        _trace_flow_id: Option<NonZero<u64>>,
    ) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            match observer.write(device_block_offset, block_count, vmo, vmo_offset, opts) {
                WriteAction::Write => {}
                WriteAction::Discard => return Ok(()),
                WriteAction::Fail => return Err(zx::Status::IO),
            }
        }
        if let Some(max) = self.info.max_transfer_blocks().as_ref() {
            // Requests should be split up by the core library
            assert!(block_count <= max.get());
        }
        if device_block_offset + block_count as u64 > self.info.block_count().unwrap() {
            Err(zx::Status::OUT_OF_RANGE)
        } else {
            self.data.write(
                &vmo.read_to_vec(vmo_offset, block_count as u64 * self.block_size as u64)?,
                device_block_offset * self.block_size as u64,
            )
        }
    }

    fn barrier(&self) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.barrier();
        }
        Ok(())
    }

    async fn flush(&self, _trace_flow_id: Option<NonZero<u64>>) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.flush();
        }
        Ok(())
    }

    async fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
        _trace_flow_id: Option<NonZero<u64>>,
    ) -> Result<(), zx::Status> {
        if let Some(observer) = self.observer.as_ref() {
            observer.trim(device_block_offset, block_count);
        }
        if device_block_offset + block_count as u64 > self.info.block_count().unwrap() {
            Err(zx::Status::OUT_OF_RANGE)
        } else {
            Ok(())
        }
    }
}
