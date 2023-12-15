// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_lock::OnceCell;
use async_trait::async_trait;
use fidl_fuchsia_process::HandleInfo;
use fuchsia_async as fasync;
use fuchsia_runtime::HandleType;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt},
};
use mapped_vmo::Mapping;
use runner::component::{ChannelEpitaph, Controllable};
use std::{ops::DerefMut, sync::Arc};
use tracing::warn;
use zx::Peered;

/// [`ColocatedProgram `] represents an instance of a program run by the
/// colocated runner. Its state is held in this struct and its behavior
/// is run in the `task`.
pub struct ColocatedProgram {
    task: Option<fasync::Task<()>>,
    filled: Option<oneshot::Receiver<Mapping>>,
    terminated: Arc<OnceCell<()>>,
}

impl ColocatedProgram {
    pub fn new(vmo_size: u64, numbered_handles: Vec<HandleInfo>) -> Result<Self, anyhow::Error> {
        let vmo = zx::Vmo::create(vmo_size)?;
        let vmo_size = vmo.get_size()?;
        let (filled_sender, filled) = oneshot::channel();
        let terminated = Arc::new(OnceCell::new());
        let fill_vmo_task =
            fasync::unblock(move || ColocatedProgram::fill_vmo(vmo, vmo_size, filled_sender));
        let terminated_clone = terminated.clone();
        let guard = scopeguard::guard((), move |()| {
            _ = terminated_clone.set_blocking(());
        });
        let task = async move {
            // We will notify others of termination when this guard is dropped,
            // which happens when this task is dropped.
            let _guard = guard;

            fill_vmo_task.await;

            // Signal to the outside world that the pages have been allocated.
            for info in numbered_handles.into_iter().filter(|info| {
                match fuchsia_runtime::HandleInfo::try_from(info.id) {
                    Ok(handle_info) => {
                        handle_info == fuchsia_runtime::HandleInfo::new(HandleType::User0, 0)
                    }
                    Err(_) => false,
                }
            }) {
                let handle = zx::EventPair::from(info.handle);
                match handle.signal_peer(zx::Signals::empty(), zx::Signals::USER_0) {
                    Ok(()) => {}
                    Err(status) => {
                        warn!("Failed to signal USER0 handle: {status}");
                    }
                }
            }

            // Sleep forever.
            std::future::pending().await
        };
        let task = fasync::Task::spawn(task);
        Ok(Self { task: Some(task), filled: Some(filled), terminated })
    }

    fn fill_vmo(vmo: zx::Vmo, vmo_size: u64, filled: oneshot::Sender<Mapping>) {
        // Map the VMO into the address space.
        let vmo_size = vmo_size as usize;
        let mut mapping = Mapping::create_from_vmo(
            &vmo,
            vmo_size,
            zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
        )
        .unwrap();
        let buffer = mapping.deref_mut();

        // Fill the VMO with randomized bytes, to cause pages to be physically allocated.
        // This approach will defeat page deduplication and page compression, for ease of
        // memory usage analysis. This program should more or less use `vmo_size` bytes.
        use rand::RngCore;
        let mut rng = rand::thread_rng();
        let mut offset: usize = 0;
        const BLOCK_SIZE: usize = 512;
        let mut bytes = vec![0u8; BLOCK_SIZE];
        loop {
            rng.fill_bytes(&mut bytes);
            buffer.write_at(offset, &bytes);
            offset += BLOCK_SIZE;
            if offset > vmo_size {
                break;
            }
        }
        buffer.release_writes();

        // Send the mapping to the program to be kept alive. This will keep those pages
        // committed.
        _ = filled.send(mapping);
    }

    /// Returns a future that will resolve when the program is terminated.
    pub fn wait_for_termination<'a>(&self) -> BoxFuture<'a, ChannelEpitaph> {
        let terminated = self.terminated.clone();
        async move {
            terminated.wait().await;
            ChannelEpitaph::ok()
        }
        .boxed()
    }
}

#[async_trait]
impl Controllable for ColocatedProgram {
    async fn kill(mut self) {
        warn!("Timed out stopping ColocatedProgram");
        self.stop().await
    }

    fn stop<'a>(&mut self) -> BoxFuture<'a, ()> {
        let task = self.task.take();
        let filled = self.filled.take();
        async {
            if let Some(filled) = filled {
                _ = filled.await;
            }
            if let Some(task) = task {
                _ = task.cancel();
            }
        }
        .boxed()
    }
}
