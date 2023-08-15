// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use starnix_sync::{InterruptibleEvent, WakeReason};
use std::{
    collections::HashMap,
    hash::Hash,
    ops::DerefMut,
    sync::{Arc, Weak},
};

use crate::{
    lock::Mutex, logging::impossible_error, mm::ProtectionFlags, signals::RunState, task::*,
    types::*,
};

/// A table of futexes.
///
/// Each 32-bit aligned address in an address space can potentially have an associated futex that
/// userspace can wait upon. This table is a sparse representation that has an actual WaitQueue
/// only for those addresses that have ever actually had a futex operation performed on them.
pub struct FutexTable<Key: FutexKey> {
    /// The futexes associated with each address in each VMO.
    ///
    /// This HashMap is populated on-demand when futexes are used.
    state: Mutex<HashMap<Key, Arc<FutexWaitQueue>>>,
}

impl<Key: FutexKey> Default for FutexTable<Key> {
    fn default() -> Self {
        Self { state: Default::default() }
    }
}

impl<Key: FutexKey> FutexTable<Key> {
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
        let (FutexOperand { vmo, offset }, key) = Key::get_operand_and_key(current_task, addr)?;

        let event = InterruptibleEvent::new();
        let guard = event.begin_wait();
        self.wait_queue_for_key(key).add(Arc::downgrade(&event), mask as u64);
        // TODO: This read should be atomic.
        let mut buf = [0u8; 4];
        vmo.read(&mut buf, offset).map_err(impossible_error)?;
        if u32::from_ne_bytes(buf) != value {
            return error!(EAGAIN);
        }
        // TODO(tbodt): Delete the wait queue from the hashmap when it becomes empty. Not doing
        // this is a memory leak.
        current_task.run_in_state(RunState::Event(event.clone()), move || {
            guard.block_until(deadline).map_err(|e| match e {
                WakeReason::Interrupted => errno!(EINTR),
                WakeReason::DeadlineExpired => errno!(ETIMEDOUT),
            })
        })
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
        let key = Key::get_key(task, addr)?;
        Ok(self.wait_queue_for_key(key).notify(mask as u64, count))
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
        let key = Key::get_key(current_task, addr)?;
        let new_key = Key::get_key(current_task, new_addr)?;
        let waiters = FutexWaitQueue::default();
        if let Some(old_waiters) = self.state.lock().remove(&key) {
            waiters.take_waiters(&old_waiters);
        }
        let woken = waiters.notify(FUTEX_BITSET_MATCH_ANY as u64, count);
        self.wait_queue_for_key(new_key).take_waiters(&waiters);
        Ok(woken)
    }

    /// Returns the WaitQueue for a given address.
    fn wait_queue_for_key(&self, key: Key) -> Arc<FutexWaitQueue> {
        let mut state = self.state.lock();
        let waiters = state.entry(key).or_default();
        Arc::clone(waiters)
    }
}

pub struct FutexOperand {
    vmo: Arc<zx::Vmo>,
    offset: u64,
}

pub trait FutexKey: Sized + Ord + Hash {
    fn get_key(task: &Task, addr: UserAddress) -> Result<Self, Errno>;

    fn get_operand_and_key(task: &Task, addr: UserAddress) -> Result<(FutexOperand, Self), Errno>;
}

#[derive(Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct PrivateFutexKey {
    addr: UserAddress,
}

impl FutexKey for PrivateFutexKey {
    fn get_key(_task: &Task, addr: UserAddress) -> Result<Self, Errno> {
        Ok(PrivateFutexKey { addr })
    }

    fn get_operand_and_key(task: &Task, addr: UserAddress) -> Result<(FutexOperand, Self), Errno> {
        let (vmo, offset) = task.mm.get_mapping_vmo(addr, ProtectionFlags::READ)?;
        let key = PrivateFutexKey { addr };
        Ok((FutexOperand { vmo, offset }, key))
    }
}

#[derive(Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct SharedFutexKey {
    // No chance of collisions since koids are never reused:
    // https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts#kernel_object_ids
    koid: zx::Koid,
    offset: u64,
}

impl FutexKey for SharedFutexKey {
    fn get_key(task: &Task, addr: UserAddress) -> Result<Self, Errno> {
        Self::get_operand_and_key(task, addr).map(|(_, key)| key)
    }

    fn get_operand_and_key(task: &Task, addr: UserAddress) -> Result<(FutexOperand, Self), Errno> {
        let (vmo, offset) = task.mm.get_mapping_vmo(addr, ProtectionFlags::READ)?;
        let koid = vmo.info().map_err(impossible_error)?.koid;
        Ok((FutexOperand { vmo, offset }, SharedFutexKey { koid, offset }))
    }
}

struct FutexWaiter {
    event: Weak<InterruptibleEvent>,
    mask: u64,
}

#[derive(Default)]
struct FutexWaitQueue {
    waiters: Mutex<Vec<FutexWaiter>>,
}

impl FutexWaitQueue {
    fn add(&self, event: Weak<InterruptibleEvent>, mask: u64) {
        let mut waiters = self.waiters.lock();
        waiters.push(FutexWaiter { event, mask });
    }

    fn notify(&self, mask: u64, limit: usize) -> usize {
        let mut woken = 0;
        // Using `retain` means we end up walking more than `limit` entries in this list, but that
        // lets us remove the stale waiters.
        self.waiters.lock().retain(|waiter| {
            if let Some(event) = waiter.event.upgrade() {
                if woken < limit && waiter.mask & mask != 0 {
                    event.notify();
                    woken += 1;
                    false
                } else {
                    true
                }
            } else {
                false
            }
        });
        woken
    }

    fn take_waiters(&self, other: &FutexWaitQueue) {
        let mut other_entries = std::mem::take(other.waiters.lock().deref_mut());
        self.waiters.lock().append(&mut other_entries);
    }
}
