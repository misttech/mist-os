// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use futures::channel::oneshot;
use starnix_sync::InterruptibleEvent;
use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    hash::Hash,
    sync::{Arc, Weak},
};

use crate::{lock::Mutex, logging::impossible_error, mm::ProtectionFlags, task::*, types::*};

/// A table of futexes.
///
/// Each 32-bit aligned address in an address space can potentially have an associated futex that
/// userspace can wait upon. This table is a sparse representation that has an actual WaitQueue
/// only for those addresses that have ever actually had a futex operation performed on them.
pub struct FutexTable<Key: FutexKey> {
    /// The futexes associated with each address in each VMO.
    ///
    /// This HashMap is populated on-demand when futexes are used.
    state: Mutex<FutexTableState<Key>>,
}

impl<Key: FutexKey> Default for FutexTable<Key> {
    fn default() -> Self {
        Self { state: Mutex::new(FutexTableState(Default::default())) }
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
        let mut state = self.state.lock();
        // As the state is locked, no wake can happen before the waiter is registered.
        // If the addr is remapped, we will read stale data, but we will not miss a futex wake.
        let (FutexOperand { vmo, offset }, key) = Key::get_operand_and_key(current_task, addr)?;
        Self::check_futex_value(&vmo, offset, value)?;

        let event = InterruptibleEvent::new();
        let guard = event.begin_wait();
        state.get_waiters_or_default(key).add(FutexWaiter {
            mask,
            notifiable: FutexNotifiable::new_internal(Arc::downgrade(&event)),
        });
        std::mem::drop(state);

        current_task.block_until(guard, deadline)
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
        self.state.lock().wake(key, count, mask)
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
        let mut state = self.state.lock();
        let mut old_waiters = if let Some(old_waiters) = state.remove(&key) {
            old_waiters
        } else {
            return Ok(0);
        };
        let woken = old_waiters.notify(FUTEX_BITSET_MATCH_ANY, count);
        if !old_waiters.is_empty() {
            state.get_waiters_or_default(new_key).transfer(old_waiters);
        }
        Ok(woken)
    }

    fn check_futex_value(vmo: &zx::Vmo, offset: u64, value: u32) -> Result<(), Errno> {
        // TODO: This read should be atomic.
        let mut buf = [0u8; 4];
        vmo.read(&mut buf, offset).map_err(impossible_error)?;
        if u32::from_ne_bytes(buf) != value {
            return error!(EAGAIN);
        }
        Ok(())
    }
}

impl FutexTable<SharedFutexKey> {
    /// Wait on the futex at the given offset in the vmo.
    ///
    /// See FUTEX_WAIT.
    pub fn external_wait(
        &self,
        vmo: zx::Vmo,
        offset: u64,
        value: u32,
        mask: u32,
    ) -> Result<oneshot::Receiver<()>, Errno> {
        let key = SharedFutexKey::new(&vmo, offset)?;
        let mut state = self.state.lock();
        // As the state is locked, no wake can happen before the waiter is registered.
        Self::check_futex_value(&vmo, offset, value)?;

        let (sender, receiver) = oneshot::channel::<()>();
        state
            .get_waiters_or_default(key)
            .add(FutexWaiter { mask, notifiable: FutexNotifiable::new_external(sender) });
        Ok(receiver)
    }

    /// Wake the given number of waiters on futex at the given offset in the vmo. Returns the
    /// number of waiters actually woken.
    ///
    /// See FUTEX_WAKE.
    pub fn external_wake(
        &self,
        vmo: zx::Vmo,
        offset: u64,
        count: usize,
        mask: u32,
    ) -> Result<usize, Errno> {
        self.state.lock().wake(SharedFutexKey::new(&vmo, offset)?, count, mask)
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
        if !addr.is_aligned(4) {
            return error!(EINVAL);
        }
        Ok(PrivateFutexKey { addr })
    }

    fn get_operand_and_key(task: &Task, addr: UserAddress) -> Result<(FutexOperand, Self), Errno> {
        if !addr.is_aligned(4) {
            return error!(EINVAL);
        }
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
        if !addr.is_aligned(4) {
            return error!(EINVAL);
        }
        let (vmo, offset) = task.mm.get_mapping_vmo(addr, ProtectionFlags::READ)?;
        let key = SharedFutexKey::new(&vmo, offset)?;
        Ok((FutexOperand { vmo, offset }, key))
    }
}

impl SharedFutexKey {
    fn new(vmo: &zx::Vmo, offset: u64) -> Result<Self, Errno> {
        let koid = vmo.info().map_err(impossible_error)?.koid;
        Ok(Self { koid, offset })
    }
}

struct FutexTableState<Key: FutexKey>(
    // TODO(tbodt): Delete the wait queue from the hashmap when it becomes empty. Not doing
    // this is a memory leak.
    HashMap<Key, FutexWaiters>,
);

impl<Key: FutexKey> std::ops::Deref for FutexTableState<Key> {
    type Target = HashMap<Key, FutexWaiters>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<Key: FutexKey> std::ops::DerefMut for FutexTableState<Key> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<Key: FutexKey> FutexTableState<Key> {
    /// Returns the FutexWaiters for a given address, creating an empty one if none is registered.
    fn get_waiters_or_default(&mut self, key: Key) -> &mut FutexWaiters {
        self.entry(key).or_default()
    }

    fn wake(&mut self, key: Key, count: usize, mask: u32) -> Result<usize, Errno> {
        let entry = self.entry(key);
        match entry {
            Entry::Vacant(_) => Ok(0),
            Entry::Occupied(mut entry) => {
                let count = entry.get_mut().notify(mask, count);
                if entry.get().is_empty() {
                    entry.remove();
                }
                Ok(count)
            }
        }
    }
}

/// Abstraction over a process waiting on a Futex that can be notified.
enum FutexNotifiable {
    /// An internal process waiting on a Futex.
    Internal(Weak<InterruptibleEvent>),
    /// An external process waiting on a Futex.
    // The sender needs to be an option so that one can send the notification while only holding a
    // mut reference on the ExternalWaiter.
    External(Option<oneshot::Sender<()>>),
}

impl FutexNotifiable {
    fn new_internal(event: Weak<InterruptibleEvent>) -> Self {
        Self::Internal(event)
    }

    fn new_external(sender: oneshot::Sender<()>) -> Self {
        Self::External(Some(sender))
    }

    /// Tries to notify the process. Returns `true` is the process have been notified. Returns
    /// `false` otherwise. This means the process is stale and will never be available again.
    fn notify(&mut self) -> bool {
        match self {
            Self::Internal(event) => {
                if let Some(event) = event.upgrade() {
                    event.notify();
                    true
                } else {
                    false
                }
            }
            Self::External(ref mut sender) => {
                if let Some(sender) = sender.take() {
                    sender.send(()).is_ok()
                } else {
                    false
                }
            }
        }
    }
}

struct FutexWaiter {
    mask: u32,
    notifiable: FutexNotifiable,
}

#[derive(Default)]
struct FutexWaiters(VecDeque<FutexWaiter>);

impl FutexWaiters {
    fn add(&mut self, waiter: FutexWaiter) {
        self.0.push_back(waiter);
    }

    fn notify(&mut self, mask: u32, count: usize) -> usize {
        let mut woken = 0;
        self.0.retain_mut(|waiter| {
            if woken == count || waiter.mask & mask == 0 {
                return true;
            }
            // The send will fail if the receiver is gone, which means nothing was actualling
            // waiting on the futex.
            if waiter.notifiable.notify() {
                woken += 1;
            }
            false
        });
        woken
    }

    fn transfer(&mut self, mut other: Self) {
        self.0.append(&mut other.0);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
