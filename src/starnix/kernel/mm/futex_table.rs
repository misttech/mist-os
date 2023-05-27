// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use std::collections::HashMap;
use std::sync::Arc;

use crate::lock::Mutex;
use crate::logging::impossible_error;
use crate::mm::ProtectionFlags;
use crate::task::*;
use crate::types::*;

/// A table of futexes.
///
/// Each 32-bit aligned address in an address space can potentially have an associated futex that
/// userspace can wait upon. This table is a sparse representation that has an actual WaitQueue
/// only for those addresses that have ever actually had a futex operation performed on them.
#[derive(Default)]
pub struct FutexTable {
    /// The futexes associated with each address in each VMO.
    ///
    /// This HashMap is populated on-demand when futexes are used.
    state: Mutex<HashMap<FutexKey, Arc<WaitQueue>>>,
}

#[derive(Eq, Hash, PartialEq)]
struct FutexKey {
    // No chance of collisions since koids are never reused:
    // https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts#kernel_object_ids
    koid: zx::Koid,
    offset: u64,
}

impl FutexTable {
    /// Wait on the futex at the given address.
    ///
    /// See FUTEX_WAIT.
    pub fn wait(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        value: u32,
        mask: u32,
        deadline: zx::Time,
    ) -> Result<(), Errno> {
        let (vmo, key) = self.get_vmo_and_key(current_task, addr)?;
        let offset = key.offset;

        let waiter = Waiter::new();
        self.get_waiters(key).wait_async_mask(&waiter, mask as u64);
        // TODO: This read should be atomic.
        let mut buf = [0u8; 4];
        vmo.read(&mut buf, offset).map_err(impossible_error)?;
        if u32::from_ne_bytes(buf) != value {
            return error!(EAGAIN);
        }
        // TODO(tbodt): Delete the wait queue from the hashmap when it becomes empty. Not doing
        // this is a memory leak.
        waiter.wait_until(current_task, deadline)
    }

    /// Wake the given number of waiters on futex at the given address. Returns the number of
    /// waiters actually woken.
    ///
    /// See FUTEX_WAKE.
    pub fn wake(
        &self,
        task: &Task,
        addr: UserAddress,
        count: usize,
        mask: u32,
    ) -> Result<usize, Errno> {
        let (_, key) = self.get_vmo_and_key(task, addr)?;
        Ok(self.get_waiters(key).notify_mask_count(mask as u64, count))
    }

    /// Requeue the waiters to another address.
    ///
    /// See FUTEX_REQUEUE
    pub fn requeue(
        &self,
        current_task: &CurrentTask,
        addr: UserAddress,
        count: usize,
        new_addr: UserAddress,
    ) -> Result<usize, Errno> {
        let (_, key) = self.get_vmo_and_key(current_task, addr)?;
        let (_, new_key) = self.get_vmo_and_key(current_task, new_addr)?;
        let waiters = WaitQueue::default();
        if let Some(old_waiters) = self.state.lock().remove(&key) {
            waiters.transfer(&old_waiters);
        }
        let woken = waiters.notify_mask_count(FUTEX_BITSET_MATCH_ANY as u64, count);
        self.get_waiters(new_key).transfer(&waiters);
        Ok(woken)
    }

    fn get_vmo_and_key(
        &self,
        task: &Task,
        addr: UserAddress,
    ) -> Result<(Arc<zx::Vmo>, FutexKey), Errno> {
        let (vmo, offset) = task.mm.get_mapping_vmo(addr, ProtectionFlags::READ)?;
        let koid = vmo.info().map_err(impossible_error)?.koid;
        Ok((vmo, FutexKey { koid, offset }))
    }

    /// Returns the WaitQueue for a given address.
    fn get_waiters(&self, key: FutexKey) -> Arc<WaitQueue> {
        let mut state = self.state.lock();
        let waiters = state.entry(key).or_default();
        Arc::clone(waiters)
    }
}
