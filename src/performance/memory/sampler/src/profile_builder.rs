// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use anyhow::Error;
use fidl_fuchsia_memory_sampler::{ModuleMap, StackTrace};
use fuchsia_zircon::Vmo;
use prost::Message;

use crate::pprof;

/// Represents an allocation for which no deallocation has been
/// reported.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveAllocation {
    pub size: i64,
    pub stack_trace: StackTrace,
}

/// Represents an allocation for which a deallocation has been
/// reported.
#[derive(Clone, Debug, PartialEq)]
pub struct DeadAllocation {
    pub size: i64,
    pub allocation_stack_trace: StackTrace,
    pub deallocation_stack_trace: StackTrace,
}

/// Accumulator for profiling information.
#[derive(Default, Debug)]
pub struct ProfileBuilder {
    process_name: String,
    module_map: Vec<ModuleMap>,
    live_allocations: HashMap<u64, LiveAllocation>,
    dead_allocations: Vec<DeadAllocation>,
}

impl ProfileBuilder {
    /// Register an allocation. It is considered live until a
    /// deallocation for the same address has been reported. Note that
    /// this assumes that allocations and deallocations at a given
    /// address are ordered.
    pub fn allocate(&mut self, address: u64, stack_trace: StackTrace, size: u64) {
        self.live_allocations.insert(address, LiveAllocation { size: size as i64, stack_trace });
    }
    /// Register a deallocation, if the corresponding allocation has
    /// been registered before.
    pub fn deallocate(&mut self, address: u64, stack_trace: StackTrace) {
        self.live_allocations.remove(&address).map(|allocation| {
            self.dead_allocations.push(DeadAllocation {
                deallocation_stack_trace: stack_trace,
                size: allocation.size,
                allocation_stack_trace: allocation.stack_trace,
            });
        });
    }
    /// Set the process information necessary to produce a profile.
    pub fn set_process_info(
        &mut self,
        process_name: Option<String>,
        module_map: impl Iterator<Item = ModuleMap>,
    ) {
        if let Some(process_name) = process_name {
            self.process_name = process_name;
        }
        self.module_map.extend(module_map);
    }
    /// Returns the total amount of dead allocations currently
    /// recorded in this instance.
    pub fn get_dead_allocations_count(&self) -> usize {
        self.dead_allocations.len()
    }
    /// Finalize the profile. Consumes this builder.
    pub fn build(self) -> (String, pprof::pproto::Profile) {
        (
            self.process_name,
            pprof::build_profile(
                self.module_map.iter(),
                self.live_allocations.values(),
                self.dead_allocations.iter(),
            ),
        )
    }
    /// Produce a partial profile from a process that is still
    /// live. Drop `dead_allocations` from `self`.
    ///
    /// Note: this lets one produce regular running profile from a
    /// long-lived process, while clearing from memory the state that
    /// will no longer be useful.
    pub fn build_partial_profile(&mut self) -> (String, pprof::pproto::Profile) {
        (
            self.process_name.clone(),
            pprof::build_profile(
                self.module_map.iter(),
                self.live_allocations.values(),
                std::mem::replace(&mut self.dead_allocations, vec![]).iter(),
            ),
        )
    }
}

// Serialize a profile to a VMO. On success, returns a tuple of a
// `Vmo` and the size of its content.
pub fn profile_to_vmo(profile: &pprof::pproto::Profile) -> Result<(Vmo, u64), Error> {
    let proto_profile = profile.encode_to_vec();
    let size = proto_profile.len() as u64;
    let vmo = Vmo::create(size)?;
    vmo.write(&proto_profile[..], 0)?;
    Ok((vmo, size as u64))
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use crate::profile_builder::{DeadAllocation, ModuleMap, ProfileBuilder, StackTrace};
    use fidl_fuchsia_memory_sampler::ExecutableSegment;

    #[fuchsia::test]
    fn test_allocate() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let stack_trace = StackTrace { stack_frames: Some(vec![]), ..Default::default() };
        let size = 10;

        builder.allocate(address, stack_trace.clone(), size);

        let allocation =
            builder.live_allocations.get(&address).expect("Could not retrieve live allocation.");

        assert_eq!(size, allocation.size as u64);
        assert_eq!(stack_trace, allocation.stack_trace);
    }

    #[fuchsia::test]
    fn test_deallocate_mismatch() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let stack_trace = StackTrace { stack_frames: Some(vec![]), ..Default::default() };

        builder.deallocate(address, stack_trace);

        let allocations = builder.dead_allocations;

        assert_eq!(Vec::<DeadAllocation>::new(), allocations);
    }

    #[fuchsia::test]
    fn test_deallocate_match() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let allocation_stack_trace =
            StackTrace { stack_frames: Some(vec![1, 2]), ..Default::default() };
        let deallocation_stack_trace =
            StackTrace { stack_frames: Some(vec![3, 4]), ..Default::default() };
        let size = 10;

        builder.allocate(address, allocation_stack_trace.clone(), size);
        builder.deallocate(address, deallocation_stack_trace.clone());

        assert_eq!(HashMap::new(), builder.live_allocations);
        let mut allocations = builder.dead_allocations;
        assert_eq!(1, allocations.len());
        let DeadAllocation {
            size: reported_size,
            allocation_stack_trace: reported_allocation_stack_trace,
            deallocation_stack_trace: reported_deallocation_stack_trace,
        } = allocations.remove(0);
        assert_eq!(size, reported_size as u64);
        assert_eq!(allocation_stack_trace, reported_allocation_stack_trace);
        assert_eq!(deallocation_stack_trace, reported_deallocation_stack_trace);
    }

    #[fuchsia::test]
    fn test_set_process_info() {
        let mut builder = ProfileBuilder::default();
        let process_name = "test_process".to_string();
        let module_map = vec![ModuleMap {
            build_id: Some(vec![1, 2, 3, 4]),
            executable_segments: Some(vec![ExecutableSegment {
                start_address: Some(0),
                size: Some(10),
                relative_address: Some(100),
                ..Default::default()
            }]),
            ..Default::default()
        }];

        builder.set_process_info(Some(process_name.clone()), module_map.clone().into_iter());

        assert_eq!(process_name, builder.process_name);
        assert_eq!(module_map, builder.module_map);
    }
}
