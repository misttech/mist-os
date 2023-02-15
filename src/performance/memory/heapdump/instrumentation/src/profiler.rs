// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Mutex;

use crate::allocations_table::AllocationsTable;
use crate::{
    heapdump_global_stats as HeapdumpGlobalStats,
    heapdump_thread_local_stats as HeapdumpThreadLocalStats,
};

/// The global instrumentation state for the current process (singleton).
///
/// This is the root of all the instrumentation's data structures except for per-thread data, which
/// is stored separately.
pub struct Profiler {
    inner: Mutex<ProfilerInner>,
}

struct ProfilerInner {
    allocations_table: AllocationsTable,
    global_stats: HeapdumpGlobalStats,
}

/// Per-thread instrumentation data.
#[derive(Default)]
pub struct PerThreadData {
    local_stats: HeapdumpThreadLocalStats,
}

impl Profiler {
    pub fn new() -> Profiler {
        let inner = ProfilerInner {
            allocations_table: AllocationsTable::new(),
            global_stats: Default::default(),
        };

        Profiler { inner: Mutex::new(inner) }
    }

    pub fn get_global_stats(&self) -> HeapdumpGlobalStats {
        self.inner.lock().unwrap().global_stats
    }

    pub fn record_allocation(&self, thread_data: &mut PerThreadData, address: u64, size: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.allocations_table.record_allocation(address, size);

        inner.global_stats.total_allocated_bytes += size;
        thread_data.local_stats.total_allocated_bytes += size;
    }

    pub fn forget_allocation(&self, thread_data: &mut PerThreadData, address: u64) {
        let mut inner = self.inner.lock().unwrap();
        let size = inner.allocations_table.forget_allocation(address);

        inner.global_stats.total_deallocated_bytes += size;
        thread_data.local_stats.total_deallocated_bytes += size;
    }
}

impl PerThreadData {
    pub fn get_local_stats(&self) -> HeapdumpThreadLocalStats {
        self.local_stats
    }
}
