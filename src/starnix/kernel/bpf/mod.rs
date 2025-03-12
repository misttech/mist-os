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

use crate::security::{self, BpfMapState};
use ebpf_api::PinnedMap;
use std::ops::Deref;

/// A BPF map and Starnix-specific metadata.
#[derive(Debug)]
pub struct BpfMap {
    pub map: PinnedMap,

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
    pub fn new(map: PinnedMap, security_state: BpfMapState) -> Self {
        Self { map, security_state }
    }
}
