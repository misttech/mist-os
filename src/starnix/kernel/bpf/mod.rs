// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of (e)BPF.
//!
//! BPF stands for Berkeley Packet Filter and is an API introduced in BSD that allows filtering
//! network packets by running little programs in the kernel. eBPF stands for extended BFP and
//! is a Linux extension of BPF that allows hooking BPF programs into many different
//! non-networking-related contexts.

pub mod attachments;
pub mod fs;
pub mod program;
pub mod syscalls;

use crate::bpf::attachments::EbpfAttachments;
use crate::bpf::program::{ProgramHandle, ProgramId, WeakProgramHandle};
use crate::mm::memory::MemoryObject;
use crate::security;
use crate::task::{register_delayed_release, CurrentTask, CurrentTaskAndLocked};
use ebpf_api::PinnedMap;
use starnix_lifecycle::{ObjectReleaser, ReleaserAction};
use starnix_sync::{BpfMapStateLevel, EbpfStateLock, LockBefore, Locked, MutexGuard, OrderedMutex};
use starnix_types::ownership::{Releasable, ReleaseGuard};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use std::collections::BTreeMap;
use std::ops::{Bound, Deref};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};

type BpfMapId = u32;

/// Counter for map identifiers.
static MAP_IDS: AtomicU32 = AtomicU32::new(1);
fn new_map_id() -> BpfMapId {
    MAP_IDS.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Default)]
struct BpfMapState {
    memory_object: Option<Arc<MemoryObject>>,
    is_frozen: bool,
}

/// A BPF map and Starnix-specific metadata.
#[derive(Debug)]
pub struct BpfMap {
    id: BpfMapId,
    map: PinnedMap,

    /// The internal state of the map object.
    state: OrderedMutex<BpfMapState, BpfMapStateLevel>,

    /// The security state associated with this bpf Map.
    pub security_state: security::BpfMapState,

    /// Reference to the `EbpfState`. Used to remove `self` from the state on
    /// drop.
    ebpf_state: Weak<EbpfState>,
}

impl Deref for BpfMap {
    type Target = PinnedMap;
    fn deref(&self) -> &PinnedMap {
        &self.map
    }
}

impl BpfMap {
    pub fn new<L>(
        locked: &mut Locked<L>,
        current_task: &CurrentTask,
        map: PinnedMap,
        security_state: security::BpfMapState,
    ) -> BpfMapHandle
    where
        L: LockBefore<EbpfStateLock>,
    {
        let map = BpfMapHandle::new(
            Self {
                id: new_map_id(),
                map,
                state: Default::default(),
                security_state,
                ebpf_state: Arc::downgrade(&current_task.kernel().ebpf_state),
            }
            .into(),
        );
        current_task.kernel().ebpf_state.register_map(locked, &map);
        map
    }

    pub fn id(&self) -> BpfMapId {
        self.id
    }

    fn frozen<'a, L>(&'a self, locked: &'a mut Locked<L>) -> impl Deref<Target = bool> + 'a
    where
        L: LockBefore<BpfMapStateLevel>,
    {
        MutexGuard::map(self.state.lock(locked), |s| &mut s.is_frozen)
    }

    fn freeze<L>(&self, locked: &mut Locked<L>) -> Result<(), Errno>
    where
        L: LockBefore<BpfMapStateLevel>,
    {
        let mut state = self.state.lock(locked);
        if state.is_frozen {
            return Ok(());
        }
        if let Some(memory) = state.memory_object.take() {
            // The memory has been computed, check whether it is still in use.
            if let Err(memory) = Arc::try_unwrap(memory) {
                // There is other user of the memory. freeze must fail.
                state.memory_object = Some(memory);
                return error!(EBUSY);
            }
        }
        state.is_frozen = true;
        return Ok(());
    }

    fn get_memory<L, F>(
        &self,
        locked: &mut Locked<L>,
        factory: F,
    ) -> Result<Arc<MemoryObject>, Errno>
    where
        L: LockBefore<BpfMapStateLevel>,
        F: FnOnce() -> Result<Arc<MemoryObject>, Errno>,
    {
        let mut state = self.state.lock(locked);
        if state.is_frozen {
            return error!(EPERM);
        }
        if let Some(memory) = state.memory_object.as_ref() {
            return Ok(memory.clone());
        }
        let memory = factory()?;
        state.memory_object = Some(memory.clone());
        Ok(memory)
    }
}

impl Releasable for BpfMap {
    type Context<'a> = CurrentTaskAndLocked<'a>;

    fn release<'a>(self, (locked, _current_task): CurrentTaskAndLocked<'a>) {
        if let Some(state) = self.ebpf_state.upgrade() {
            state.unregister_map(locked, self.id);
        }
    }
}

pub enum BpfMapReleaserAction {}
impl ReleaserAction<BpfMap> for BpfMapReleaserAction {
    fn release(map: ReleaseGuard<BpfMap>) {
        register_delayed_release(map);
    }
}
pub type BpfMapReleaser = ObjectReleaser<BpfMap, BpfMapReleaserAction>;
pub type BpfMapHandle = Arc<BpfMapReleaser>;
pub type WeakBpfMapHandle = Weak<BpfMapReleaser>;

/// Stores global eBPF state.
#[derive(Default)]
pub struct EbpfState {
    pub attachments: EbpfAttachments,

    programs: OrderedMutex<BTreeMap<ProgramId, WeakProgramHandle>, EbpfStateLock>,
    maps: OrderedMutex<BTreeMap<BpfMapId, WeakBpfMapHandle>, EbpfStateLock>,
}

impl EbpfState {
    fn register_program<L>(&self, locked: &mut Locked<L>, program: &ProgramHandle)
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.programs.lock(locked).insert(program.id(), Arc::downgrade(program));
    }

    fn unregister_program<L>(&self, locked: &mut Locked<L>, id: ProgramId)
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.programs.lock(locked).remove(&id).expect("Missing eBPF program");
    }

    fn get_next_program_id<L>(
        &self,
        locked: &mut Locked<L>,
        start_id: ProgramId,
    ) -> Option<ProgramId>
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.programs
            .lock(locked)
            .range((Bound::Excluded(start_id), Bound::Unbounded))
            .next()
            .map(|(k, _)| *k)
    }

    fn get_program_by_id<L>(&self, locked: &mut Locked<L>, id: ProgramId) -> Option<ProgramHandle>
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.programs.lock(locked).get(&id).map(|p| p.upgrade()).flatten()
    }

    fn register_map<L>(&self, locked: &mut Locked<L>, map: &BpfMapHandle)
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.maps.lock(locked).insert(map.id(), Arc::downgrade(map));
    }

    fn unregister_map<L>(&self, locked: &mut Locked<L>, id: BpfMapId)
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.maps.lock(locked).remove(&id).expect("Missing eBPF map");
    }

    fn get_next_map_id<L>(&self, locked: &mut Locked<L>, start_id: BpfMapId) -> Option<BpfMapId>
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.maps
            .lock(locked)
            .range((Bound::Excluded(start_id), Bound::Unbounded))
            .next()
            .map(|(k, _)| *k)
    }

    fn get_map_by_id<L>(&self, locked: &mut Locked<L>, id: BpfMapId) -> Option<BpfMapHandle>
    where
        L: LockBefore<EbpfStateLock>,
    {
        self.maps.lock(locked).get(&id).map(|p| p.upgrade()).flatten()
    }
}
