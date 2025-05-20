// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use block_client::{BlockClient, RemoteBlockClient, VmoId};
use event_listener::Event;
use fidl_fuchsia_hardware_block::Flag;
use fuchsia_async as fasync;
use fuchsia_sync::Mutex;
use std::collections::HashMap;
use std::num::NonZero;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};

pub type Device = DeviceImpl<RemoteBlockClient>;

/// Wraps a `BlockClient` impl and manages registered VMO ids.
pub struct DeviceImpl<C: BlockClient + 'static> {
    client: C,
    vmo_ids: Mutex<HashMap<usize, Arc<VmoIdWrapperImpl<C>>>>,
    buffers: Buffers,
}

impl<C: BlockClient> DeviceImpl<C> {
    pub async fn new(client: C) -> Result<Self, zx::Status> {
        let buffers = Buffers::new(&client).await?;
        Ok(Self { client, vmo_ids: Mutex::default(), buffers })
    }

    pub fn block_flags(&self) -> Flag {
        self.client.block_flags()
    }

    pub fn max_transfer_blocks(&self) -> Option<NonZero<u32>> {
        self.client.max_transfer_blocks()
    }

    /// Ataches `vmo`.  NOTE: This assumes that the pointer &zx::Vmo will remain stable.
    pub async fn attach_vmo(self: &Arc<Self>, vmo: &zx::Vmo) -> Result<(), zx::Status> {
        let vmo_id = self.client.attach_vmo(vmo).await?;
        assert!(
            self.vmo_ids
                .lock()
                .insert(
                    vmo as *const _ as usize,
                    Arc::new(VmoIdWrapperImpl(Arc::downgrade(self), vmo_id))
                )
                .is_none(),
            "VMO already attached!"
        );
        Ok(())
    }

    /// Deteaches `vmo`.  NOTE: The pointer `&zx::Vmo` must match that used in `attach_vmo`.
    pub fn detach_vmo(&self, vmo: &zx::Vmo) {
        // This won't immediately detach because it might still be in-use, but as soon as all uses
        // finish, it will get detached.
        self.vmo_ids.lock().remove(&(vmo as *const _ as usize));
    }

    /// Returns the VMO ID registered the given vmo.
    ///
    /// # Panics
    ///
    /// Panics if we don't know about `vmo` i.e. `attach_vmo` above was not called.
    pub fn get_vmo_id(&self, vmo: &zx::Vmo) -> Arc<VmoIdWrapperImpl<C>> {
        self.vmo_ids.lock()[&(vmo as *const _ as usize)].clone()
    }

    /// Returns a buffer that can be used that is registered with the device.
    pub async fn get_buffer(&self) -> BufferGuard<'_, C> {
        loop {
            let listener = {
                let mut buffers = self.buffers.buffers.lock();
                if let Some(offset) = buffers.pop() {
                    return BufferGuard {
                        device: self,
                        vmo_offset: offset as u64,
                        vmo_id: &self.buffers.vmo_id,
                        // SAFETY: We mapped this range in `Buffers::new`.
                        buffer: unsafe {
                            std::slice::from_raw_parts_mut(
                                (self.buffers.addr + offset) as *mut u8,
                                BUFFER_SIZE,
                            )
                        },
                    };
                }
                self.buffers.event.listen()
            };
            listener.await
        }
    }
}

impl Deref for Device {
    type Target = RemoteBlockClient;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

pub struct VmoIdWrapperImpl<C: BlockClient + 'static>(Weak<DeviceImpl<C>>, VmoId);

impl<C: BlockClient + 'static> Drop for VmoIdWrapperImpl<C> {
    fn drop(&mut self) {
        // Turn it into an ID so that if the spawned task is dropped, an assertion doesn't fire.  It
        // will mean the ID is leaked, but it's most likely that the server is being shut down
        // anyway so it shouldn't matter.
        let vmo_id = self.1.take().into_id();
        if let Some(device) = self.0.upgrade() {
            fasync::Task::spawn(async move {
                let _ = device.client.detach_vmo(VmoId::new(vmo_id)).await;
            })
            .detach();
        }
    }
}

impl<C: BlockClient> Deref for VmoIdWrapperImpl<C> {
    type Target = VmoId;
    fn deref(&self) -> &Self::Target {
        &self.1
    }
}

pub const BUFFER_SIZE: usize = 1048576;
const BUFFER_COUNT: usize = 16;
const TOTAL_SIZE: usize = BUFFER_SIZE * BUFFER_COUNT;

struct Buffers {
    _vmo: zx::Vmo,
    vmo_id: VmoId,
    addr: usize,
    event: Event,
    buffers: Mutex<Vec<usize>>,
}

impl Buffers {
    async fn new(client: &impl BlockClient) -> Result<Self, zx::Status> {
        let vmo = zx::Vmo::create(TOTAL_SIZE as u64)?;
        let vmo_id = client.attach_vmo(&vmo).await?;
        let addr = fuchsia_runtime::vmar_root_self().map(
            0,
            &vmo,
            0,
            TOTAL_SIZE,
            zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )?;
        Ok(Self {
            _vmo: vmo,
            vmo_id,
            addr,
            event: Event::new(),
            buffers: Mutex::new((0..TOTAL_SIZE).into_iter().step_by(BUFFER_SIZE).collect()),
        })
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        // SAFETY: We mapped this address in `Buffer::new`.
        let _ = unsafe { fuchsia_runtime::vmar_root_self().unmap(self.addr, TOTAL_SIZE) };

        // If we're being dropped, then it means the client is about to be dropped, which means we
        // can just leak `vmo_id`.
        let _ = self.vmo_id.take().into_id();
    }
}

pub struct BufferGuard<'a, C: BlockClient + 'static> {
    device: &'a DeviceImpl<C>,
    vmo_offset: u64,
    vmo_id: &'a VmoId,
    buffer: &'a mut [u8],
}

impl<C: BlockClient> BufferGuard<'_, C> {
    pub fn vmo_id(&self) -> &VmoId {
        &self.vmo_id
    }

    pub fn vmo_offset(&self) -> u64 {
        self.vmo_offset
    }
}

impl<C: BlockClient> Drop for BufferGuard<'_, C> {
    fn drop(&mut self) {
        {
            let mut buffers = self.device.buffers.buffers.lock();
            buffers.push(self.buffer.as_ptr() as usize - self.device.buffers.addr);
        }
        self.device.buffers.event.notify_additional_relaxed(1);
    }
}

impl<C: BlockClient> Deref for BufferGuard<'_, C> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.buffer
    }
}

impl<C: BlockClient> DerefMut for BufferGuard<'_, C> {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceImpl, BUFFER_COUNT, BUFFER_SIZE};
    use assert_matches::assert_matches;
    use fake_block_client::FakeBlockClient;
    use fuchsia_async::{self as fasync, TestExecutor};
    use futures::future::select_all;
    use futures::FutureExt;
    use rand::Rng;
    use std::iter::repeat_with;
    use std::sync::Arc;
    use std::task::Poll;

    #[fuchsia::test(allow_stalls = false)]
    async fn test_get_buffer() {
        let fake_block_client = FakeBlockClient::new(1024, 1024);
        let device = Arc::new(DeviceImpl::new(fake_block_client).await.unwrap());
        let mut bufs = Vec::new();
        let mut offsets = Vec::new();

        fn check_no_overlap(offsets: &[u64], offset: u64) {
            for &o in offsets {
                assert!(offset >= o + BUFFER_SIZE as u64 || offset + BUFFER_SIZE as u64 <= o);
            }
        }

        for i in 0..BUFFER_COUNT {
            let mut buf = device.get_buffer().now_or_never().unwrap();
            check_no_overlap(&offsets, buf.vmo_offset());
            offsets.push(buf.vmo_offset());
            buf.fill(i as u8);
            bufs.push(buf);
        }

        // Check that all the buffers contain the expected fill.
        for (i, buf) in bufs.iter().enumerate() {
            assert_eq!(&**buf, &[i as u8; BUFFER_SIZE]);
        }

        // The next buffer we get should stall.
        let scope = fasync::Scope::new();
        let mut tasks: Vec<_> = repeat_with(|| {
            let device = device.clone();
            scope.compute(async move { device.get_buffer().await.vmo_offset() })
        })
        .take(2)
        .collect();

        assert!(TestExecutor::poll_until_stalled(select_all(&mut tasks)).await.is_pending());

        // Pop one buffer, and it should unblock the pending buffer.
        bufs.pop();
        offsets.pop();

        // Popping the buffer should have woken one of the tasks, but we don't know which one.
        // Randomly drop one of the tasks to test that dropping it still causes the other task to
        // wake up.
        let mut task = tasks.into_iter().skip(rand::thread_rng().gen_range(0..2)).next().unwrap();

        assert_matches!(
            TestExecutor::poll_until_stalled(&mut task).await,
            Poll::Ready(offset) => check_no_overlap(&offsets, offset)
        );
    }
}
