// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::gpt::GptPartition;
use block_client::{VmoId, WriteOptions};

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// PartitionBackend is an implementation of block_server's Interface which is backed by a windowed
/// view of the underlying GPT device.
pub struct PartitionBackend {
    partition: Arc<GptPartition>,
    vmo_keys_to_vmoids_map: Mutex<BTreeMap<usize, Arc<VmoId>>>,
}

impl block_server::async_interface::Interface for PartitionBackend {
    async fn on_attach_vmo(&self, vmo: &zx::Vmo) -> Result<(), zx::Status> {
        let key = std::ptr::from_ref(vmo) as usize;
        let vmoid = self.partition.attach_vmo(vmo).await?;
        let old = self.vmo_keys_to_vmoids_map.lock().unwrap().insert(key, Arc::new(vmoid));
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

    async fn get_info(&self) -> Result<Cow<'_, block_server::PartitionInfo>, zx::Status> {
        Ok(Cow::Owned(self.partition.get_info().await?))
    }

    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
    ) -> Result<(), zx::Status> {
        let vmoid = self.get_vmoid(vmo)?;
        self.partition.read(device_block_offset, block_count, vmoid.as_ref(), vmo_offset).await
    }

    async fn write(
        &self,
        device_block_offset: u64,
        length: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64, // *bytes* not blocks
        opts: WriteOptions,
    ) -> Result<(), zx::Status> {
        let vmoid = self.get_vmoid(vmo)?;
        self.partition.write(device_block_offset, length, vmoid.as_ref(), vmo_offset, opts).await
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        self.partition.flush().await
    }

    async fn trim(&self, device_block_offset: u64, block_count: u32) -> Result<(), zx::Status> {
        self.partition.trim(device_block_offset, block_count).await
    }
}

impl PartitionBackend {
    pub fn new(partition: Arc<GptPartition>) -> Arc<Self> {
        Arc::new(Self { partition, vmo_keys_to_vmoids_map: Mutex::new(BTreeMap::new()) })
    }

    fn get_vmoid(&self, vmo: &zx::Vmo) -> Result<Arc<VmoId>, zx::Status> {
        let key = std::ptr::from_ref(vmo) as usize;
        self.vmo_keys_to_vmoids_map
            .lock()
            .unwrap()
            .get(&key)
            .map(Arc::clone)
            .ok_or(zx::Status::NOT_FOUND)
    }
}

impl Drop for PartitionBackend {
    fn drop(&mut self) {
        for vmoid in std::mem::take(&mut *self.vmo_keys_to_vmoids_map.lock().unwrap()).into_values()
        {
            // For now, leak the vmoids.
            // TODO(https://fxbug.dev/339491886): Reconcile vmoid management.
            let _ = Arc::try_unwrap(vmoid)
                .map(|vmoid| vmoid.into_id())
                .expect("VMO removed while in use");
        }
    }
}
