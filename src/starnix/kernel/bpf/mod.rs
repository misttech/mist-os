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

use crate::mm::memory::MemoryObject;
use crate::security::{self};
use ebpf_api::PinnedMap;
use starnix_sync::{BpfMapStateLevel, LockBefore, Locked, OrderedMutex};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Debug, Default)]
struct BpfMapState {
    memory_object: Option<Arc<MemoryObject>>,
    is_frozen: bool,
}

/// A BPF map and Starnix-specific metadata.
#[derive(Debug)]
pub struct BpfMap {
    pub map: PinnedMap,

    /// The internal state of the map object.
    state: OrderedMutex<BpfMapState, BpfMapStateLevel>,

    /// The security state associated with this bpf Map.
    pub security_state: security::BpfMapState,
}

impl Deref for BpfMap {
    type Target = PinnedMap;
    fn deref(&self) -> &PinnedMap {
        &self.map
    }
}

impl BpfMap {
    pub fn new(map: PinnedMap, security_state: security::BpfMapState) -> Self {
        Self { map, state: Default::default(), security_state }
    }

    fn is_frozen<L>(&self, locked: &mut Locked<L>) -> bool
    where
        L: LockBefore<BpfMapStateLevel>,
    {
        self.state.lock(locked).is_frozen
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
