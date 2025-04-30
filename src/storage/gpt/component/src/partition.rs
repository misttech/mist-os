// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::gpt::GptPartition;
use anyhow::Error;
use block_client::{VmoId, WriteOptions};
use block_server::async_interface::{PassthroughSession, SessionManager};
use block_server::OffsetMap;
use fidl_fuchsia_hardware_block as fblock;

use fuchsia_sync::Mutex;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZero;
use std::sync::Arc;

/// PartitionBackend is an implementation of block_server's Interface which is backed by a windowed
/// view of the underlying GPT device.
pub struct PartitionBackend {
    partition: Arc<GptPartition>,
    vmo_keys_to_vmoids_map: Mutex<BTreeMap<usize, Arc<VmoId>>>,
}

impl block_server::async_interface::Interface for PartitionBackend {
    async fn open_session(
        &self,
        session_manager: Arc<SessionManager<Self>>,
        stream: fblock::SessionRequestStream,
        offset_map: OffsetMap,
        block_size: u32,
    ) -> Result<(), Error> {
        if offset_map.is_empty() {
            // For now, we don't support double-passthrough.  We could as needed for nested GPT.
            // If we support this, we can remove I/O and vmoid management from this struct.
            return session_manager.serve_session(stream, offset_map, block_size).await;
        }
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fblock::SessionMarker>();
        self.partition.open_passthrough_session(server_end);
        let passthrough = PassthroughSession::new(proxy);
        passthrough.serve(stream).await
    }

    async fn on_attach_vmo(&self, vmo: &zx::Vmo) -> Result<(), zx::Status> {
        let key = std::ptr::from_ref(vmo) as usize;
        let vmoid = self.partition.attach_vmo(vmo).await?;
        let old = self.vmo_keys_to_vmoids_map.lock().insert(key, Arc::new(vmoid));
        if let Some(vmoid) = old {
            // For now, leak the old vmoid.
            // XXX kludge -- addresses can be reused!  We need to manage vmoids ourself to properly
            // manage lifetimes, or possibly change the APIs to eliminate the need to do so.
            // TODO(https://fxbug.dev/339491886): Reconcile vmoid management.
            let _ = Arc::try_unwrap(vmoid)
                .map(|vmoid| vmoid.into_id())
                .expect("VMO removed while in use");
        }
        Ok(())
    }

    async fn get_info(&self) -> Result<Cow<'_, block_server::DeviceInfo>, zx::Status> {
        Ok(Cow::Owned(self.partition.get_info().await?))
    }

    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        trace_flow_id: Option<NonZero<u64>>,
    ) -> Result<(), zx::Status> {
        let vmoid = self.get_vmoid(vmo)?;
        self.partition
            .read(device_block_offset, block_count, vmoid.as_ref(), vmo_offset, trace_flow_id)
            .await
    }

    async fn write(
        &self,
        device_block_offset: u64,
        length: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        opts: WriteOptions,
        trace_flow_id: Option<NonZero<u64>>,
    ) -> Result<(), zx::Status> {
        let vmoid = self.get_vmoid(vmo)?;
        self.partition
            .write(device_block_offset, length, vmoid.as_ref(), vmo_offset, opts, trace_flow_id)
            .await
    }

    async fn flush(&self, trace_flow_id: Option<NonZero<u64>>) -> Result<(), zx::Status> {
        self.partition.flush(trace_flow_id).await
    }

    async fn trim(
        &self,
        device_block_offset: u64,
        block_count: u32,
        trace_flow_id: Option<NonZero<u64>>,
    ) -> Result<(), zx::Status> {
        self.partition.trim(device_block_offset, block_count, trace_flow_id).await
    }
}

impl PartitionBackend {
    pub fn new(partition: Arc<GptPartition>) -> Arc<Self> {
        Arc::new(Self { partition, vmo_keys_to_vmoids_map: Mutex::new(BTreeMap::new()) })
    }

    fn get_vmoid(&self, vmo: &zx::Vmo) -> Result<Arc<VmoId>, zx::Status> {
        let key = std::ptr::from_ref(vmo) as usize;
        self.vmo_keys_to_vmoids_map.lock().get(&key).map(Arc::clone).ok_or(zx::Status::NOT_FOUND)
    }
}

impl Drop for PartitionBackend {
    fn drop(&mut self) {
        for vmoid in std::mem::take(&mut *self.vmo_keys_to_vmoids_map.lock()).into_values() {
            // For now, leak the vmoids.
            // TODO(https://fxbug.dev/339491886): Reconcile vmoid management.
            let _ = Arc::try_unwrap(vmoid)
                .map(|vmoid| vmoid.into_id())
                .expect("VMO removed while in use");
        }
    }
}
