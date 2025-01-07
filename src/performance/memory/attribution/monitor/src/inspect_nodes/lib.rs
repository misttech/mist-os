// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{anyhow, Result};
use fuchsia_inspect::{Inspector, LazyNode};
use futures::FutureExt;

use inspect_runtime::PublishedInspectController;
use log::debug;
use {fuchsia_inspect as _, inspect_runtime as _};

/// Hold the resource required to serve the inspect tree.
/// The FIDL service stops when this object is dropped.
pub struct ServiceTask {
    _kmem_stats: LazyNode,
    _kmem_stats_compression: LazyNode,
    _inspect_controller: PublishedInspectController,
}

/// Begins to serve the inspect tree, and returns an object holding the server's resources.
/// Dropping the `ServiceTask` stops the service.
pub fn start_service(
    kernel_stats: impl fidl_fuchsia_kernel::StatsProxyInterface + Clone + 'static,
) -> Result<ServiceTask> {
    debug!("Start serving inspect tree.");

    // This creates the root of an Inspect tree
    // The Inspector is a singleton that you can access from any scope
    let inspector = fuchsia_inspect::component::inspector();

    // This serves the Inspect tree, converting failures into fatal errors
    let inspect_controller =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default())
            .ok_or_else(|| anyhow!("Failed to serve server handling `fuchsia.inspect.Tree`"))?;

    // Lazy evaluation is unregistered when the `LazyNode` is dropped.
    let kmem_stats = {
        let kernel_stats = kernel_stats.clone();
        inspector.root().create_lazy_child("kmem_stats", move || {
            let kernel_stats = kernel_stats.clone();
            async move {
                let inspector = Inspector::default();
                let root = inspector.root();
                let mem_stats = kernel_stats.get_memory_stats().await?;
                mem_stats.total_bytes.map(|v| root.record_uint("total_bytes", v));
                mem_stats.free_bytes.map(|v| root.record_uint("free_bytes", v));
                mem_stats.free_loaned_bytes.map(|v| root.record_uint("free_loaned_bytes", v));
                mem_stats.wired_bytes.map(|v| root.record_uint("wired_bytes", v));
                mem_stats.total_heap_bytes.map(|v| root.record_uint("total_heap_bytes", v));
                mem_stats.free_heap_bytes.map(|v| root.record_uint("free_heap_bytes", v));
                mem_stats.vmo_bytes.map(|v| root.record_uint("vmo_bytes", v));
                mem_stats.mmu_overhead_bytes.map(|v| root.record_uint("mmu_overhead_bytes", v));
                mem_stats.ipc_bytes.map(|v| root.record_uint("ipc_bytes", v));
                mem_stats.cache_bytes.map(|v| root.record_uint("cache_bytes", v));
                mem_stats.slab_bytes.map(|v| root.record_uint("slab_bytes", v));
                mem_stats.zram_bytes.map(|v| root.record_uint("zram_bytes", v));
                mem_stats.other_bytes.map(|v| root.record_uint("other_bytes", v));
                mem_stats
                    .vmo_reclaim_total_bytes
                    .map(|v| root.record_uint("vmo_reclaim_total_bytes", v));
                mem_stats
                    .vmo_reclaim_newest_bytes
                    .map(|v| root.record_uint("vmo_reclaim_newest_bytes", v));
                mem_stats
                    .vmo_reclaim_oldest_bytes
                    .map(|v| root.record_uint("vmo_reclaim_oldest_bytes", v));
                mem_stats
                    .vmo_reclaim_disabled_bytes
                    .map(|v| root.record_uint("vmo_reclaim_disabled_bytes", v));
                mem_stats
                    .vmo_discardable_locked_bytes
                    .map(|v| root.record_uint("vmo_discardable_locked_bytes", v));
                mem_stats
                    .vmo_discardable_unlocked_bytes
                    .map(|v| root.record_uint("vmo_discardable_unlocked_bytes", v));
                Ok(inspector)
            }
            .boxed()
        })
    };

    let kmem_stats_compression =
        inspector.root().create_lazy_child("kmem_stats_compression", move || {
            let kernel_stats = kernel_stats.clone();
            async move {
                let inspector = Inspector::default();
                let cmp_stats = kernel_stats.get_memory_stats_compression().await?;
                cmp_stats
                    .uncompressed_storage_bytes
                    .map(|v| inspector.root().record_uint("uncompressed_storage_bytes", v));
                cmp_stats
                    .compressed_storage_bytes
                    .map(|v| inspector.root().record_uint("compressed_storage_bytes", v));
                cmp_stats
                    .compressed_fragmentation_bytes
                    .map(|v| inspector.root().record_uint("compressed_fragmentation_bytes", v));
                cmp_stats
                    .compression_time
                    .map(|v| inspector.root().record_int("compression_time", v));
                cmp_stats
                    .decompression_time
                    .map(|v| inspector.root().record_int("decompression_time", v));
                cmp_stats
                    .total_page_compression_attempts
                    .map(|v| inspector.root().record_uint("total_page_compression_attempts", v));
                cmp_stats
                    .failed_page_compression_attempts
                    .map(|v| inspector.root().record_uint("failed_page_compression_attempts", v));
                cmp_stats
                    .total_page_decompressions
                    .map(|v| inspector.root().record_uint("total_page_decompressions", v));
                cmp_stats
                    .compressed_page_evictions
                    .map(|v| inspector.root().record_uint("compressed_page_evictions", v));
                cmp_stats
                    .eager_page_compressions
                    .map(|v| inspector.root().record_uint("eager_page_compressions", v));
                cmp_stats
                    .memory_pressure_page_compressions
                    .map(|v| inspector.root().record_uint("memory_pressure_page_compressions", v));
                cmp_stats
                    .critical_memory_page_compressions
                    .map(|v| inspector.root().record_uint("critical_memory_page_compressions", v));
                cmp_stats
                    .pages_decompressed_unit_ns
                    .map(|v| inspector.root().record_uint("pages_decompressed_unit_ns", v));
                cmp_stats.pages_decompressed_within_log_time.map(|v| {
                    let node = inspector.root().create_child("pages_decompressed_within_log_time");
                    // Using constant strings saves allocations.
                    node.record_uint("0", v[0]);
                    node.record_uint("1", v[1]);
                    node.record_uint("2", v[2]);
                    node.record_uint("3", v[3]);
                    node.record_uint("4", v[4]);
                    node.record_uint("5", v[5]);
                    node.record_uint("6", v[6]);
                    node.record_uint("7", v[7]);
                });
                Ok(inspector)
            }
            .boxed()
        });
    Ok(ServiceTask {
        _kmem_stats: kmem_stats,
        _kmem_stats_compression: kmem_stats_compression,
        _inspect_controller: inspect_controller,
    })
}
