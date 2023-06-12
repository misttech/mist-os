// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::{create_endpoints, ServerEnd};
use fidl::AsHandleRef;
use fidl_fuchsia_memory_heapdump_process as fheapdump_process;
use fuchsia_zircon as zx;
use std::sync::Mutex;

use crate::allocations_table::AllocationsTable;
use crate::resources_table::{ResourceKey, ResourcesTable};
use crate::{
    heapdump_global_stats as HeapdumpGlobalStats,
    heapdump_thread_local_stats as HeapdumpThreadLocalStats,
};

/// The global instrumentation state for the current process (singleton).
///
/// This is the root of all the instrumentation's data structures except for per-thread data, which
/// is stored separately.
pub struct Profiler {
    snapshot_sink: fheapdump_process::SnapshotSinkV1SynchronousProxy,
    inner: Mutex<ProfilerInner>,
}

#[derive(Default)]
struct ProfilerInner {
    allocations_table: AllocationsTable,
    resources_table: ResourcesTable,
    global_stats: HeapdumpGlobalStats,
    snapshot_sink_server: Option<ServerEnd<fheapdump_process::SnapshotSinkV1Marker>>,
}

/// Per-thread instrumentation data.
#[derive(Default)]
pub struct PerThreadData {
    local_stats: HeapdumpThreadLocalStats,

    /// Koid of the current thread (cached).
    cached_koid: Option<zx::Koid>,

    /// The resource key of the current thread and its name at the time it was generated.
    resource_key_and_name: Option<(ResourceKey, [u8; zx::sys::ZX_MAX_NAME_LEN])>,
}

impl Default for Profiler {
    fn default() -> Profiler {
        let (client, server) = create_endpoints();
        let proxy = fheapdump_process::SnapshotSinkV1SynchronousProxy::new(client.into_channel());

        let inner = ProfilerInner { snapshot_sink_server: Some(server), ..Default::default() };
        Profiler { snapshot_sink: proxy, inner: Mutex::new(inner) }
    }
}

impl Profiler {
    pub fn bind(&self, registry_channel: zx::Channel) {
        let process_dup = fuchsia_runtime::process_self()
            .duplicate(zx::Rights::SAME_RIGHTS)
            .expect("failed to duplicate process handle");

        let (snapshot_sink_server, allocations_table_dup, resources_table_dup) = {
            let mut inner = self.inner.lock().unwrap();
            (
                inner.snapshot_sink_server.take(),
                inner.allocations_table.share_vmo(),
                inner.resources_table.share_vmo(),
            )
        };
        let snapshot_sink_server = snapshot_sink_server.expect("bind called more than once");

        let registry_proxy = fheapdump_process::RegistrySynchronousProxy::new(registry_channel);

        // Ignore result.
        let _ = registry_proxy.register_v1(
            process_dup,
            allocations_table_dup,
            resources_table_dup,
            snapshot_sink_server,
        );
    }
}

impl Profiler {
    pub fn get_global_stats(&self) -> HeapdumpGlobalStats {
        self.inner.lock().unwrap().global_stats
    }

    pub fn record_allocation(
        &self,
        thread_data: &mut PerThreadData,
        address: u64,
        size: u64,
        compressed_stack_trace: &[u8],
        timestamp: i64,
    ) {
        let (thread_koid, thread_name) =
            get_current_thread_koid_and_name(&mut thread_data.cached_koid);

        let mut inner = self.inner.lock().unwrap();
        let thread_info_key = match thread_data.resource_key_and_name {
            Some((resource_key, old_name)) if old_name == thread_name => {
                // The previously generated resource key is still valid, because the name did not
                // change in the meantime.
                resource_key
            }
            _ => {
                // We need to generate a new resource key.
                let resource_key =
                    inner.resources_table.insert_thread_info(thread_koid, &thread_name);
                thread_data.resource_key_and_name = Some((resource_key, thread_name));
                resource_key
            }
        };
        let stack_trace_key = inner.resources_table.intern_stack_trace(compressed_stack_trace);
        inner.allocations_table.record_allocation(
            address,
            size,
            thread_info_key,
            stack_trace_key,
            timestamp,
        );

        inner.global_stats.total_allocated_bytes += size;
        thread_data.local_stats.total_allocated_bytes += size;
    }

    pub fn forget_allocation(&self, thread_data: &mut PerThreadData, address: u64) {
        let mut inner = self.inner.lock().unwrap();
        let size = inner.allocations_table.forget_allocation(address);

        inner.global_stats.total_deallocated_bytes += size;
        thread_data.local_stats.total_deallocated_bytes += size;
    }

    pub fn publish_named_snapshot(&self, name: &str) {
        let allocations_table_snapshot = {
            let inner = self.inner.lock().unwrap();
            inner.allocations_table.snapshot_vmo()
        };

        // Ignore outcome.
        let _ = self.snapshot_sink.store_named_snapshot(name, allocations_table_snapshot);
    }
}

impl PerThreadData {
    pub fn get_local_stats(&self) -> HeapdumpThreadLocalStats {
        self.local_stats
    }
}

fn get_current_thread_koid_and_name(
    koid_cache: &mut Option<zx::Koid>,
) -> (zx::Koid, [u8; zx::sys::ZX_MAX_NAME_LEN]) {
    struct NameProperty;
    unsafe impl zx::PropertyQuery for NameProperty {
        const PROPERTY: zx::Property = zx::Property::NAME;
        type PropTy = [u8; zx::sys::ZX_MAX_NAME_LEN];
    }

    // Obtain the koid and the name of the current thread. Unlike the koid, the thread name
    // cannot be cached as it can change between calls.
    let thread = fuchsia_runtime::thread_self();
    let koid = koid_cache
        .get_or_insert_with(|| thread.get_koid().expect("failed to get current thread's koid"));
    let name = zx::object_get_property::<NameProperty>(thread.as_handle_ref())
        .expect("failed to get current thread's name");

    (*koid, name)
}
