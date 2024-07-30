// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use block_server::async_interface::{Interface, SessionManager};
use block_server::{BlockServer, PartitionInfo};
use std::sync::Arc;
use {fidl_fuchsia_hardware_block_volume as fvolume, fuchsia_zircon as zx};

pub const TYPE_GUID: [u8; 16] = [1; 16];
pub const INSTANCE_GUID: [u8; 16] = [2; 16];
pub const PARTITION_NAME: &str = "fake-server";

pub struct FakeServer {
    server: BlockServer<SessionManager<Data>>,
}

impl FakeServer {
    pub fn new(block_count: u64, block_size: u32, initial_content: &[u8]) -> Self {
        let vmo = zx::Vmo::create(block_count as u64 * block_size as u64).unwrap();
        let data = Arc::new(Data { block_size, data: vmo });
        data.data.write(initial_content, 0).unwrap();
        Self {
            server: BlockServer::new(
                PartitionInfo {
                    block_count,
                    block_size,
                    type_guid: TYPE_GUID.clone(),
                    instance_guid: INSTANCE_GUID.clone(),
                    name: PARTITION_NAME.to_string(),
                },
                data,
            ),
        }
    }

    pub async fn serve(&self, requests: fvolume::VolumeRequestStream) -> Result<(), Error> {
        self.server.handle_requests(requests).await
    }
}

struct Data {
    block_size: u32,
    data: zx::Vmo,
}

impl Interface for Data {
    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), zx::Status> {
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
    ) -> Result<(), zx::Status> {
        self.data.write(
            &vmo.read_to_vec(vmo_offset, block_count as u64 * self.block_size as u64)?,
            device_block_offset * self.block_size as u64,
        )
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn trim(&self, _device_block_offset: u64, _block_count: u32) -> Result<(), zx::Status> {
        Ok(())
    }
}
