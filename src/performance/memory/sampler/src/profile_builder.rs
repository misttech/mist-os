// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use anyhow::Error;
use fidl_fuchsia_memory_sampler::ModuleMap;
use fuchsia_zircon::Vmo;
use prost::Message;

use crate::{crash_reporter::ProfileReport, pprof};

pub type StackTrace = Vec<u64>;

/// Represents an allocation for which no deallocation has been
/// reported.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveAllocation {
    pub size: u64,
    pub stack_trace: Rc<StackTrace>,
}

/// Aggregated counter of allocations for which a deallocation has
/// been recorded.
#[derive(Clone, Default, Debug)]
pub struct DeadAllocationCounter {
    pub total_size: u64,
    pub count: u64,
}

/// Aggregated counter of deallocations.
#[derive(Clone, Default, Debug)]
pub struct DeallocationCounter {
    pub total_size: u64,
    pub count: u64,
}

/// Accumulator for profiling information.
#[derive(Default, Debug)]
pub struct ProfileBuilder {
    process_name: String,
    module_map: Vec<ModuleMap>,
    /// Mapping from addresses to allocations; we assume there can
    /// only be one live allocation at a given address. Reallocations
    /// are recorded as a sequence of a deallocation and a new
    /// allocation.
    live_allocations: HashMap<u64, LiveAllocation>,
    /// Mapping from a stack trace to a counter of dead
    /// allocations. This representation aggregates allocations from
    /// the same call site to save memory.
    dead_allocations: HashMap<Rc<StackTrace>, DeadAllocationCounter>,
    /// Mapping from a stack trace to a counter of deallocations. This
    /// representation aggregates deallocations from the same call
    /// site to save memory.
    deallocations: HashMap<Rc<StackTrace>, DeallocationCounter>,
    /// Set used to hold references to recorded stack traces. This can
    /// be used to drastically reduce memory usage from allocations
    /// from an already known call site: additional allocations would
    /// store a reference, rather than the entire stack trace that
    /// could be fairly large.
    stack_traces: HashSet<Rc<StackTrace>>,
}

impl ProfileBuilder {
    /// Remove all stack traces that are no longer referenced by any
    /// recorded allocation.
    ///
    /// Note: this function can be used to reclaim memory after
    /// consuming allocations/deallocations (e.g. by producing a
    /// partial profile).
    fn prune_unreferenced_stack_traces(&mut self) {
        self.stack_traces.retain(|st| Rc::strong_count(st) > 1);
    }
    /// Add the given stack_trace to the cache, if needed, then return
    /// a reference to the cached value.
    fn cache_stack_trace(&mut self, stack_trace: StackTrace) -> Rc<StackTrace> {
        // Note: `Entry` on `HashSet` would save us from cloning the
        // stack trace here.
        if !self.stack_traces.contains(&stack_trace) {
            self.stack_traces.insert(Rc::new(stack_trace.clone()));
        };
        self.stack_traces.get(&stack_trace).unwrap().clone()
    }
    /// Register an allocation. It is considered live until a
    /// deallocation for the same address has been reported. Note that
    /// this assumes that allocations and deallocations at a given
    /// address are ordered.
    pub fn allocate(&mut self, address: u64, stack_trace: StackTrace, size: u64) {
        let stack_trace = self.cache_stack_trace(stack_trace);
        self.live_allocations.insert(address, LiveAllocation { size, stack_trace });
    }
    /// Register a deallocation, if the corresponding allocation has
    /// been registered before.
    pub fn deallocate(&mut self, address: u64, stack_trace: StackTrace) {
        self.live_allocations.remove(&address).map(|allocation| {
            {
                let dead_allocation =
                    self.dead_allocations.entry(allocation.stack_trace).or_default();
                dead_allocation.count += 1;
                dead_allocation.total_size += allocation.size;
            }
            {
                let stack_trace = self.cache_stack_trace(stack_trace);
                let deallocation = self.deallocations.entry(stack_trace).or_default();
                deallocation.count += 1;
                deallocation.total_size += allocation.size;
            }
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
    /// Returns the approximate amount of stack traces currently
    /// recorded in this instance.
    ///
    /// Note: Consuming past allocation events (e.g. by producing a
    /// partial profile) can be followed by a call to
    /// `self.prune_unereferenced_stack_traces` to reclaim space; this
    /// function can be used as an heuristic to estimate the amount of
    /// memory that can be reclaimed.
    pub fn get_approximate_reclaimable_stack_traces_count(&self) -> usize {
        self.dead_allocations.len() + self.deallocations.len()
    }
    /// Finalize the profile. Consumes this builder.
    pub fn build(self) -> Result<ProfileReport, Error> {
        let profile = pprof::build_profile(
            self.module_map.iter(),
            self.live_allocations.values(),
            self.dead_allocations,
            self.deallocations,
            &self.stack_traces,
        );

        let (vmo, size) = profile_to_vmo(&profile)?;
        Ok(ProfileReport::Final { process_name: self.process_name, profile: vmo, size })
    }
    /// Produce a partial profile from a process that is still
    /// live. Drop `dead_allocations` from `self`, and prune the
    /// cache.
    ///
    /// Note: this lets one produce regular running profiles from a
    /// long-lived process, while clearing from memory the state that
    /// will no longer be useful.
    pub fn build_partial_profile(&mut self, iteration: usize) -> Result<ProfileReport, Error> {
        let profile = {
            pprof::build_profile(
                self.module_map.iter(),
                self.live_allocations.values(),
                std::mem::replace(&mut self.dead_allocations, HashMap::new()),
                std::mem::replace(&mut self.deallocations, HashMap::new()),
                &self.stack_traces,
            )
        };
        self.prune_unreferenced_stack_traces();

        let (vmo, size) = profile_to_vmo(&profile)?;
        Ok(ProfileReport::Partial {
            process_name: self.process_name.clone(),
            profile: vmo,
            size,
            iteration,
        })
    }
}

// Serialize a profile to a VMO. On success, returns a tuple of a
// `Vmo` and the size of its content.
fn profile_to_vmo(profile: &pprof::pproto::Profile) -> Result<(Vmo, u64), Error> {
    let proto_profile = profile.encode_to_vec();
    let size = proto_profile.len() as u64;
    let vmo = Vmo::create(size)?;
    vmo.write(&proto_profile[..], 0)?;
    Ok((vmo, size))
}

#[cfg(test)]
mod test {
    use crate::profile_builder::{
        DeadAllocationCounter, DeallocationCounter, ModuleMap, ProfileBuilder,
    };
    use fidl_fuchsia_memory_sampler::ExecutableSegment;

    #[fuchsia::test]
    fn test_allocate() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let stack_trace = vec![];
        let size = 10;

        builder.allocate(address, stack_trace.clone(), size);

        let allocation =
            builder.live_allocations.get(&address).expect("Could not retrieve live allocation.");

        assert_eq!(size, allocation.size);
        assert_eq!(stack_trace, *(allocation.stack_trace));
    }

    #[fuchsia::test]
    fn test_deallocate_mismatch() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let stack_trace = vec![];

        builder.deallocate(address, stack_trace);

        let deallocations = builder.deallocations;

        assert!(deallocations.is_empty());
    }

    #[fuchsia::test]
    fn test_deallocate_match() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let allocation_stack_trace = vec![1, 2];
        let deallocation_stack_trace = vec![3, 4];
        let size = 10;

        builder.allocate(address, allocation_stack_trace.clone(), size);
        builder.deallocate(address, deallocation_stack_trace.clone());

        assert!(builder.live_allocations.is_empty());
        {
            let allocations = builder.dead_allocations;
            assert_eq!(1, allocations.values().len());
            {
                let (stack_trace, DeadAllocationCounter { count, total_size }) =
                    allocations.into_iter().next().unwrap();
                assert_eq!(size, total_size);
                assert_eq!(1, count);
                assert_eq!(allocation_stack_trace, *(stack_trace));
            }
        }

        {
            let deallocations = builder.deallocations;
            assert_eq!(1, deallocations.values().len());
            {
                let (stack_trace, DeallocationCounter { count, total_size }) =
                    deallocations.into_iter().next().unwrap();
                assert_eq!(size, total_size);
                assert_eq!(1, count);
                assert_eq!(deallocation_stack_trace, *(stack_trace));
            }
        }
    }

    #[fuchsia::test]
    fn test_build_partial_profile_prunes_stack_traces() {
        let mut builder = ProfileBuilder::default();
        let address = 0x1000;
        let allocation_stack_trace = vec![1, 2];
        let deallocation_stack_trace = vec![3, 4];
        let size = 10;
        let test_index = 42;

        builder.allocate(address, allocation_stack_trace.clone(), size);
        builder.deallocate(address, deallocation_stack_trace.clone());

        assert_ne!(0, builder.get_approximate_reclaimable_stack_traces_count());
        let _ = builder.build_partial_profile(test_index).unwrap();
        assert_eq!(0, builder.get_approximate_reclaimable_stack_traces_count());
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
