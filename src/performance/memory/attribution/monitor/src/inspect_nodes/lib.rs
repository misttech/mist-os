// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{anyhow, Error, Result};
use attribution_processing::digest::{BucketDefinition, Digest};
use attribution_processing::AttributionDataProvider;
use fpressure::WatcherRequest;
use fuchsia_async::{MonotonicDuration, Task, WakeupTime};
use fuchsia_inspect::{ArrayProperty, Inspector, Node};
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use futures::{select, try_join, FutureExt, StreamExt, TryFutureExt, TryStreamExt};
use inspect_runtime::PublishedInspectController;
use log::debug;
use memory_monitor2_config::Config;
use stalls::StallProvider;
use std::sync::Arc;
use {
    fidl_fuchsia_kernel as fkernel, fidl_fuchsia_memorypressure as fpressure, fuchsia_inspect as _,
    inspect_runtime as _,
};

/// Hold the resource required to serve the inspect tree.
/// The FIDL service stops when this object is dropped.
pub struct ServiceTask {
    _inspect_controller: PublishedInspectController,
    _periodic_digest: Task<Result<(), anyhow::Error>>,
}

/// Begins to serve the inspect tree, and returns an object holding the server's resources.
/// Dropping the `ServiceTask` stops the service.
pub fn start_service(
    attribution_data_service: Arc<impl AttributionDataProvider>,
    kernel_stats_proxy: fkernel::StatsProxy,
    stall_provider: Arc<impl StallProvider>,
    memory_monitor2_config: Config,
    memorypressure_proxy: fpressure::ProviderProxy,
    bucket_definitions: Vec<BucketDefinition>,
) -> Result<ServiceTask> {
    debug!("Start serving inspect tree.");

    // This creates the root of an Inspect tree
    // The Inspector is a singleton that you can access from any scope
    let inspector = fuchsia_inspect::component::inspector();

    // This serves the Inspect tree, converting failures into fatal errors
    let inspect_controller =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default())
            .ok_or_else(|| anyhow!("Failed to serve server handling `fuchsia.inspect.Tree`"))?;

    build_inspect_tree(
        kernel_stats_proxy.clone(),
        stall_provider,
        inspector,
    );
    let digest_service = digest_service(
        memory_monitor2_config,
        attribution_data_service,
        kernel_stats_proxy,
        memorypressure_proxy,
        bucket_definitions,
        inspector.root().create_child("logger"),
    )?;
    Ok(ServiceTask { _inspect_controller: inspect_controller, _periodic_digest: digest_service })
}

fn build_inspect_tree(
    kernel_stats_proxy: fkernel::StatsProxy,
    stall_provider: Arc<impl StallProvider>,
    inspector: &Inspector,
) {
    // Lazy evaluation is unregistered when the `LazyNode` is dropped.
    {
        let kernel_stats_proxy = kernel_stats_proxy.clone();
        inspector.root().record_lazy_child("kmem_stats", move || {
            let kernel_stats_proxy = kernel_stats_proxy.clone();
            async move {
                let inspector = Inspector::default();
                let root = inspector.root();
                let mem_stats = kernel_stats_proxy.get_memory_stats().await?;
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

    {
        inspector.root().record_lazy_child("kmem_stats_compression", move || {
            let kernel_stats_proxy = kernel_stats_proxy.clone();
            async move {
                let inspector = Inspector::default();
                let cmp_stats = kernel_stats_proxy.get_memory_stats_compression().await?;
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
                    let array =
                        inspector.root().create_uint_array("pages_decompressed_within_log_time", 8);
                    // Using constant strings saves allocations.
                    array.set(0, v[0]);
                    array.set(1, v[1]);
                    array.set(2, v[2]);
                    array.set(3, v[3]);
                    array.set(4, v[4]);
                    array.set(5, v[5]);
                    array.set(6, v[6]);
                    array.set(7, v[7]);
                    inspector.root().record(array);
                });
                Ok(inspector)
            }
            .boxed()
        });
    }

    {
        inspector.root().record_lazy_child("stalls", move || {
            let stall_info = stall_provider.get_stall_info().unwrap();
            let stall_rate_opt = stall_provider.get_stall_rate();
            async move {
                let inspector = Inspector::default();
                inspector.root().record_int("current_some", stall_info.stall_time_some);
                inspector.root().record_int("current_full", stall_info.stall_time_full);
                if let Some(stall_rate) = stall_rate_opt {
                    inspector
                        .root()
                        .record_int("rate_interval_s", stall_rate.interval.into_seconds());
                    inspector.root().record_int("rate_some", stall_rate.rate_some);
                    inspector.root().record_int("rate_full", stall_rate.rate_full);
                }
                Ok(inspector)
            }
            .boxed()
        });
    }
}

fn digest_service(
    memory_monitor2_config: Config,
    attribution_data_service: Arc<impl AttributionDataProvider + 'static>,
    kernel_stats_proxy: fkernel::StatsProxy,
    memorypressure_proxy: fpressure::ProviderProxy,
    bucket_definitions: Vec<BucketDefinition>,
    digest_node: Node,
) -> Result<Task<Result<(), Error>>> {
    // Initialize pressure monitoring.
    let (watcher, watcher_stream) =
        fidl::endpoints::create_request_stream::<fpressure::WatcherMarker>();
    memorypressure_proxy.register_watcher(watcher)?;

    Ok(fuchsia_async::Task::spawn(async move {
        let mut buckets_list_node =
            // Keep up to 100 measurements.
            BoundedListNode::new(digest_node.create_child("measurements"), 100);
        let buckets_names = std::cell::OnceCell::new();
        let attribution_data_service = attribution_data_service;
        let pressure_stream = watcher_stream.map_err(anyhow::Error::from);

        // Get the initial, baseline pressure level.
        let (request, mut pressure_stream) = pressure_stream.into_future().await;
        let WatcherRequest::OnLevelChanged { level, responder } = request.ok_or_else(|| {
            anyhow::Error::msg(
                "Unexpectedly exhausted pressure stream before receiving baseline pressure level",
            )
        })??;
        responder.send()?;
        let mut current_level = level;
        let new_timer = |level| {
            MonotonicDuration::from_seconds(match level {
                fpressure::Level::Normal => memory_monitor2_config.normal_capture_delay_s,
                fpressure::Level::Warning => memory_monitor2_config.warning_capture_delay_s,
                fpressure::Level::Critical => memory_monitor2_config.critical_capture_delay_s,
            } as i64)
            .into_timer()
            .boxed()
            .fuse()
        };
        let mut timer = new_timer(current_level);
        loop {
            // Wait for either a pressure change or the timer corresponding to the current level. In
            // either case, reset the timer.
            let () = select! {
                // When we receive a pressure change, update the current level, and if necessary do
                // a capture.
                pressure = pressure_stream.next() =>
                    match pressure.ok_or_else(|| anyhow::Error::msg("Unexpectedly exhausted pressure stream"))?? {
                        WatcherRequest::OnLevelChanged{level, responder} => {
                            responder.send()?;
                            if level == current_level { continue; }
                            current_level = level;
                            timer = new_timer(level);
                            if !memory_monitor2_config.capture_on_pressure_change { continue; }
                        },
                    },
                // If instead we reached the deadline, do a capture anyway. The deadline depends on
                // the current pressure level and the configuration.
                _ = timer => {timer = new_timer(current_level);}
            };

            // Retrieve (concurrently) the data necessary to perform the aggregation.
            let (attribution_data, kmem_stats, kmem_stats_compression) = try_join!(
                attribution_data_service.get_attribution_data(),
                kernel_stats_proxy.get_memory_stats().map_err(anyhow::Error::from),
                kernel_stats_proxy.get_memory_stats_compression().map_err(anyhow::Error::from)
            )?;

            // Compute the aggregation.
            let Digest { buckets } = Digest::new(
                &attribution_data,
                kmem_stats,
                kmem_stats_compression,
                &bucket_definitions,
            );

            // Initialize the inspect property containing the buckets names, if necessary.
            let _ = buckets_names.get_or_init(|| {
                // Create inspect node to store buckets related information.
                let buckets_names = digest_node.create_string_array("buckets", buckets.len());
                for (i, attribution_processing::digest::Bucket { name, .. }) in
                    buckets.iter().enumerate()
                {
                    buckets_names.set(i, name);
                }
                buckets_names
            });

            // Add an entry for the current aggregation.
            buckets_list_node.add_entry(|n| {
                n.record_int("timestamp", zx::BootInstant::get().into_nanos());
                let ia = n.create_uint_array("bucket_sizes", buckets.len());
                for (i, b) in buckets.iter().enumerate() {
                    ia.set(i, b.size as u64);
                }
                n.record(ia);
            });
        }
    }))
}

#[cfg(test)]
mod tests {
    use attribution_processing::{
        Attribution, AttributionData, Principal, PrincipalDescription, PrincipalIdentifier,
        PrincipalType, Resource, ResourceReference, ZXName,
    };
    use diagnostics_assertions::{assert_data_tree, NonZeroIntProperty};
    use fuchsia_async::TestExecutor;
    use futures::future::BoxFuture;
    use futures::task::Poll;
    use futures::TryStreamExt;
    use {
        fidl_fuchsia_memory_attribution_plugin as fplugin,
        fidl_fuchsia_memorypressure as fpressure, fuchsia_async as fasync,
    };

    use super::*;
    use std::time::Duration;

    struct FakeAttributionDataProvider {}

    impl AttributionDataProvider for FakeAttributionDataProvider {
        fn get_attribution_data(&self) -> BoxFuture<'_, Result<AttributionData, anyhow::Error>> {
            async {
                Ok(AttributionData {
                    principals_vec: vec![Principal {
                        identifier: PrincipalIdentifier(1),
                        description: PrincipalDescription::Component("principal".to_owned()),
                        principal_type: PrincipalType::Runnable,
                        parent: None,
                    }],
                    resources_vec: vec![Resource {
                        koid: 10,
                        name_index: 0,
                        resource_type: fplugin::ResourceType::Vmo(fplugin::Vmo {
                            parent: None,
                            private_committed_bytes: Some(1024),
                            private_populated_bytes: Some(2048),
                            scaled_committed_bytes: Some(1024),
                            scaled_populated_bytes: Some(2048),
                            total_committed_bytes: Some(1024),
                            total_populated_bytes: Some(2048),
                            ..Default::default()
                        }),
                    }],
                    resource_names: vec![ZXName::from_string_lossy("resource")],
                    attributions: vec![Attribution {
                        source: PrincipalIdentifier(1),
                        subject: PrincipalIdentifier(1),
                        resources: vec![ResourceReference::KernelObject(10)],
                    }],
                })
            }
            .boxed()
        }
    }

    async fn serve_kernel_stats(
        mut request_stream: fkernel::StatsRequestStream,
    ) -> Result<(), fidl::Error> {
        while let Some(request) = request_stream.try_next().await? {
            match request {
                fkernel::StatsRequest::GetMemoryStats { responder } => {
                    responder
                        .send(&fkernel::MemoryStats {
                            total_bytes: Some(1),
                            free_bytes: Some(2),
                            wired_bytes: Some(3),
                            total_heap_bytes: Some(4),
                            free_heap_bytes: Some(5),
                            vmo_bytes: Some(6),
                            mmu_overhead_bytes: Some(7),
                            ipc_bytes: Some(8),
                            other_bytes: Some(9),
                            free_loaned_bytes: Some(10),
                            cache_bytes: Some(11),
                            slab_bytes: Some(12),
                            zram_bytes: Some(13),
                            vmo_reclaim_total_bytes: Some(14),
                            vmo_reclaim_newest_bytes: Some(15),
                            vmo_reclaim_oldest_bytes: Some(16),
                            vmo_reclaim_disabled_bytes: Some(17),
                            vmo_discardable_locked_bytes: Some(18),
                            vmo_discardable_unlocked_bytes: Some(19),
                            ..Default::default()
                        })
                        .unwrap();
                }
                fkernel::StatsRequest::GetMemoryStatsExtended { responder: _ } => {
                    unimplemented!("Deprecated call, should not be used")
                }
                fkernel::StatsRequest::GetMemoryStatsCompression { responder } => {
                    responder
                        .send(&fkernel::MemoryStatsCompression {
                            uncompressed_storage_bytes: Some(20),
                            compressed_storage_bytes: Some(21),
                            compressed_fragmentation_bytes: Some(22),
                            compression_time: Some(23),
                            decompression_time: Some(24),
                            total_page_compression_attempts: Some(25),
                            failed_page_compression_attempts: Some(26),
                            total_page_decompressions: Some(27),
                            compressed_page_evictions: Some(28),
                            eager_page_compressions: Some(29),
                            memory_pressure_page_compressions: Some(30),
                            critical_memory_page_compressions: Some(31),
                            pages_decompressed_unit_ns: Some(32),
                            pages_decompressed_within_log_time: Some([
                                40, 41, 42, 43, 44, 45, 46, 47,
                            ]),
                            ..Default::default()
                        })
                        .unwrap();
                }
                fkernel::StatsRequest::GetCpuStats { responder: _ } => unimplemented!(),
                fkernel::StatsRequest::GetCpuLoad { duration: _, responder: _ } => unimplemented!(),
            }
        }
        Ok(())
    }

    #[test]
    fn test_build_inspect_tree() {
        let mut exec = fasync::TestExecutor::new();

        let (stats_provider, stats_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::StatsMarker>();

        fasync::Task::spawn(async move {
            serve_kernel_stats(stats_request_stream).await.unwrap();
        })
        .detach();

        let inspector = fuchsia_inspect::Inspector::default();

        struct FakeStallProvider {}
        impl StallProvider for FakeStallProvider {
            fn get_stall_info(&self) -> Result<zx::MemoryStall, anyhow::Error> {
                Ok(zx::MemoryStall { stall_time_some: 10, stall_time_full: 20 })
            }

            fn get_stall_rate(&self) -> Option<stalls::MemoryStallRate> {
                Some(stalls::MemoryStallRate {
                    interval: fasync::MonotonicDuration::from_seconds(60),
                    rate_some: 1,
                    rate_full: 2,
                })
            }
        }

        build_inspect_tree(
            stats_provider,
            Arc::new(FakeStallProvider {}),
            &inspector,
        );

        let output = exec
            .run_singlethreaded(fuchsia_inspect::reader::read(&inspector))
            .expect("got hierarchy");

        assert_data_tree!(output, root: {
            kmem_stats: {
                total_bytes: 1u64,
                free_bytes: 2u64,
                wired_bytes: 3u64,
                total_heap_bytes: 4u64,
                free_heap_bytes: 5u64,
                vmo_bytes: 6u64,
                mmu_overhead_bytes: 7u64,
                ipc_bytes: 8u64,
                other_bytes: 9u64,
                free_loaned_bytes: 10u64,
                cache_bytes: 11u64,
                slab_bytes: 12u64,
                zram_bytes: 13u64,
                vmo_reclaim_total_bytes: 14u64,
                vmo_reclaim_newest_bytes: 15u64,
                vmo_reclaim_oldest_bytes: 16u64,
                vmo_reclaim_disabled_bytes: 17u64,
                vmo_discardable_locked_bytes: 18u64,
                vmo_discardable_unlocked_bytes: 19u64
            },
            kmem_stats_compression: {
                uncompressed_storage_bytes: 20u64,
                compressed_storage_bytes: 21u64,
                compressed_fragmentation_bytes: 22u64,
                compression_time: 23i64,
                decompression_time: 24i64,
                total_page_compression_attempts: 25u64,
                failed_page_compression_attempts: 26u64,
                total_page_decompressions: 27u64,
                compressed_page_evictions: 28u64,
                eager_page_compressions: 29u64,
                memory_pressure_page_compressions: 30u64,
                critical_memory_page_compressions: 31u64,
                pages_decompressed_unit_ns: 32u64,
                pages_decompressed_within_log_time: vec![
                    40u64, 41u64, 42u64, 43u64, 44u64, 45u64, 46u64, 47u64,
                ]
            },
            stalls: {
                current_some: 10i64,
                current_full: 20i64,
                rate_some: 1i64,
                rate_full: 2i64,
                rate_interval_s: 60i64
            }
        });
    }

    #[test]
    fn test_digest_service_capture_on_pressure_change_and_wait() -> anyhow::Result<()> {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let data_provider = Arc::new(FakeAttributionDataProvider {});
        let (stats_provider, stats_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::StatsMarker>();

        fasync::Task::spawn(async move {
            serve_kernel_stats(stats_request_stream).await.unwrap();
        })
        .detach();

        let inspector = fuchsia_inspect::Inspector::default();
        let (pressure_provider, pressure_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpressure::ProviderMarker>();
        let mut digest_service = std::pin::pin!(digest_service(
            Config {
                capture_on_pressure_change: true,
                imminent_oom_capture_delay_s: 10,
                critical_capture_delay_s: 10,
                warning_capture_delay_s: 10,
                normal_capture_delay_s: 10,
            },
            data_provider,
            stats_provider,
            pressure_provider,
            vec![],
            inspector.root().create_child("logger"),
        )?);
        // Expects digst_service to register a watcher, answers with
        // an initial pressure level, then returns the watcher for
        // further signaling. Panics if this whole transaction is not
        // immediately ready.
        let Poll::Ready(watcher) = exec
            .run_until_stalled(
                &mut pressure_request_stream
                    .then(|request| async {
                        let fpressure::ProviderRequest::RegisterWatcher { watcher, .. } =
                            request.expect("digest_service failed to register a watcher");
                        let watcher = watcher.into_proxy();
                        watcher.on_level_changed(fpressure::Level::Normal).await.expect(
                            "digest_service failed to acknowledge the initial pressure level",
                        );
                        watcher
                    })
                    .boxed()
                    .into_future(),
            )
            .map(|(watcher, _)| {
                watcher.ok_or_else(|| anyhow::Error::msg("failed to register watcher"))
            })?
        else {
            panic!("digest_service failed to register a watcher");
        };
        // Send a pressure signal, to trigger a capture.
        assert!(exec
            .run_until_stalled(&mut watcher.on_level_changed(fpressure::Level::Warning))?
            .is_ready());
        // Ensure that digest_service has an opportunity to react to the pressure signal.
        let _ = exec.run_until_stalled(&mut digest_service);

        // Fake the passage of time, so that digest_service may do another capture.
        assert!(exec
            .run_until_stalled(&mut std::pin::pin!(TestExecutor::advance_to(
                exec.now() + Duration::from_secs(10).into()
            )))
            .is_ready());
        // Ensure that digest_service has an opportunity to react to the passage of time.
        let _ = exec.run_until_stalled(&mut digest_service)?;

        // This should resolve immediately because the inspect hierarchy has been populated by now.
        let Poll::Ready(output) = exec
            .run_until_stalled(&mut fuchsia_inspect::reader::read(&inspector).boxed())
            .map(|r| r.expect("got hierarchy"))
        else {
            panic!("Couldn't retrieve inspect output");
        };

        assert_data_tree!(output, root: {
            logger: {
                buckets: vec![
                    "Undigested",
                    "Orphaned",
                    "Kernel",
                    "Free",
                    "[Addl]PagerTotal",
                    "[Addl]PagerNewest",
                    "[Addl]PagerOldest",
                    "[Addl]DiscardableLocked",
                    "[Addl]DiscardableUnlocked",
                    "[Addl]ZramCompressedBytes",
                ],
                measurements: {
                    // Corresponds to the capture on pressure change
                    "0": {
                        timestamp: NonZeroIntProperty,
                        bucket_sizes: vec![
                            1024u64, // Undigested: matches the single unmatched VMO
                            6u64,    // Orphaned: vmo_bytes reported by the kernel but not covered by any bucket
                            31u64,   // Kernel: 3 wired + 4 heap + 7 mmu + 8 IPC + 9 other = 31
                            2u64,    // Free
                            14u64,   // [Addl]PagerTotal
                            15u64,   // [Addl]PagerNewest
                            16u64,   // [Addl]PagerOldest
                            18u64,   // [Addl]DiscardableLocked
                            19u64,   // [Addl]DiscardableUnlocked
                            21u64,   // [Addl]ZramCompressedBytes
                        ],
                    },
                    // Corresponds to the capture after the passage of time
                    "1": {
                        timestamp: NonZeroIntProperty,
                        bucket_sizes: vec![
                            1024u64, // Undigested: matches the single unmatched VMO
                            6u64,    // Orphaned: vmo_bytes reported by the kernel but not covered by any bucket
                            31u64,   // Kernel: 3 wired + 4 heap + 7 mmu + 8 IPC + 9 other = 31
                            2u64,    // Free
                            14u64,   // [Addl]PagerTotal
                            15u64,   // [Addl]PagerNewest
                            16u64,   // [Addl]PagerOldest
                            18u64,   // [Addl]DiscardableLocked
                            19u64,   // [Addl]DiscardableUnlocked
                            21u64,   // [Addl]ZramCompressedBytes
                        ],
                    },
                },
            },
        });
        Ok(())
    }

    #[test]
    fn test_digest_service_wait() -> anyhow::Result<()> {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let data_provider = Arc::new(FakeAttributionDataProvider {});
        let (stats_provider, stats_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::StatsMarker>();

        fasync::Task::spawn(async move {
            serve_kernel_stats(stats_request_stream).await.unwrap();
        })
        .detach();
        let (pressure_provider, pressure_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpressure::ProviderMarker>();
        let inspector = fuchsia_inspect::Inspector::default();
        let mut digest_service = std::pin::pin!(digest_service(
            Config {
                capture_on_pressure_change: false,
                imminent_oom_capture_delay_s: 10,
                critical_capture_delay_s: 10,
                warning_capture_delay_s: 10,
                normal_capture_delay_s: 10,
            },
            data_provider,
            stats_provider,
            pressure_provider,
            vec![],
            inspector.root().create_child("logger"),
        )?);
        // digest_service registers a watcher; make sure we answer.  Also, make sure not to drop the
        // proxy nor the pressure stream; early termination would get reported to digest_service,
        // which then prematurely interrupts it, before the timers have a chance to run.
        let Poll::Ready((_watcher, _pressure_stream)) = exec
            .run_until_stalled(
                &mut std::pin::pin!(pressure_request_stream.then(|request| async {
                    let fpressure::ProviderRequest::RegisterWatcher { watcher, .. } =
                        request.map_err(anyhow::Error::from)?;
                    let watcher_proxy = watcher.into_proxy();
                    let _ = watcher_proxy.on_level_changed(fpressure::Level::Normal).await?;
                    Ok::<fpressure::WatcherProxy, anyhow::Error>(watcher_proxy)
                }))
                .into_future(),
            )
            .map(|(watcher, pressure_stream)| {
                (
                    watcher.ok_or_else(|| {
                        anyhow::Error::msg("Pressure stream unexpectedly exhausted")
                    }),
                    pressure_stream,
                )
            })
        else {
            panic!("Failed to register the watcher");
        };

        // Give digest_service the opportunity to setup its timers.
        let _ = exec.run_until_stalled(&mut digest_service)?;
        // Fake the passage of time, so that digest_service may do another capture.
        assert!(exec
            .run_until_stalled(&mut std::pin::pin!(TestExecutor::advance_to(
                exec.now() + Duration::from_secs(15).into()
            )))
            .is_ready());
        // Ensure that digest_service has an opportunity to react to the passage of time.
        assert!(exec.run_until_stalled(&mut digest_service).is_pending());
        // This should resolve immediately because the inspect hierarchy has been populated by now.
        let Poll::Ready(output) = exec
            .run_until_stalled(&mut fuchsia_inspect::reader::read(&inspector).boxed())
            .map(|r| r.expect("got hierarchy"))
        else {
            panic!("Couldn't retrieve inspect output");
        };

        assert_data_tree!(output, root: {
            logger: {
                buckets: vec![
                    "Undigested",
                    "Orphaned",
                    "Kernel",
                    "Free",
                    "[Addl]PagerTotal",
                    "[Addl]PagerNewest",
                    "[Addl]PagerOldest",
                    "[Addl]DiscardableLocked",
                    "[Addl]DiscardableUnlocked",
                    "[Addl]ZramCompressedBytes",
                ],
                measurements: {
                    // Corresponds to the capture after the passage of time
                    "0": {
                        timestamp: NonZeroIntProperty,
                        bucket_sizes: vec![
                            1024u64, // Undigested: matches the single unmatched VMO
                            6u64,    // Orphaned: vmo_bytes reported by the kernel but not covered by any bucket
                            31u64,   // Kernel: 3 wired + 4 heap + 7 mmu + 8 IPC + 9 other = 31
                            2u64,    // Free
                            14u64,   // [Addl]PagerTotal
                            15u64,   // [Addl]PagerNewest
                            16u64,   // [Addl]PagerOldest
                            18u64,   // [Addl]DiscardableLocked
                            19u64,   // [Addl]DiscardableUnlocked
                            21u64,   // [Addl]ZramCompressedBytes
                        ],
                    },
                },
            },
        });
        Ok(())
    }

    #[test]
    fn test_digest_service_no_capture_on_pressure_change() -> anyhow::Result<()> {
        let mut exec = fasync::TestExecutor::new();
        let data_provider = Arc::new(FakeAttributionDataProvider {});
        let (stats_provider, stats_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::StatsMarker>();

        fasync::Task::spawn(async move {
            serve_kernel_stats(stats_request_stream).await.unwrap();
        })
        .detach();

        let inspector = fuchsia_inspect::Inspector::default();
        let (pressure_provider, pressure_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpressure::ProviderMarker>();
        let mut serve_pressure_stream = pressure_request_stream
            .then(|request| async {
                let fpressure::ProviderRequest::RegisterWatcher { watcher, .. } =
                    request.map_err(anyhow::Error::from)?;
                let watcher_proxy = watcher.into_proxy();
                let _ = watcher_proxy.on_level_changed(fpressure::Level::Normal).await?;
                Ok::<fpressure::WatcherProxy, anyhow::Error>(watcher_proxy)
            })
            .boxed();
        let mut digest_service = std::pin::pin!(digest_service(
            Config {
                capture_on_pressure_change: false,
                imminent_oom_capture_delay_s: 10,
                critical_capture_delay_s: 10,
                warning_capture_delay_s: 10,
                normal_capture_delay_s: 10,
            },
            data_provider,
            stats_provider,
            pressure_provider,
            vec![],
            inspector.root().create_child("logger"),
        )?);
        let watcher =
            exec.run_singlethreaded(serve_pressure_stream.next()).transpose()?.expect("watcher");
        let _ = exec.run_singlethreaded(watcher.on_level_changed(fpressure::Level::Warning))?;
        let _ = exec.run_until_stalled(&mut digest_service);
        let output = exec
            .run_singlethreaded(fuchsia_inspect::reader::read(&inspector))
            .expect("got hierarchy");

        assert_data_tree!(output, root: {
            logger: {
                measurements: {},
            },
        });
        Ok(())
    }

    #[test]
    fn test_digest_service_capture_on_pressure_change() -> anyhow::Result<()> {
        let mut exec = fasync::TestExecutor::new();
        let data_provider = Arc::new(FakeAttributionDataProvider {});
        let (stats_provider, stats_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::StatsMarker>();

        fasync::Task::spawn(async move {
            serve_kernel_stats(stats_request_stream).await.unwrap();
        })
        .detach();

        let inspector = fuchsia_inspect::Inspector::default();
        let (pressure_provider, pressure_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpressure::ProviderMarker>();
        let mut serve_pressure_stream = pressure_request_stream
            .then(|request| async {
                let fpressure::ProviderRequest::RegisterWatcher { watcher, .. } =
                    request.map_err(anyhow::Error::from)?;
                let watcher_proxy = watcher.into_proxy();
                let _ = watcher_proxy.on_level_changed(fpressure::Level::Normal).await?;
                Ok::<fpressure::WatcherProxy, anyhow::Error>(watcher_proxy)
            })
            .boxed();
        let mut digest_service = std::pin::pin!(digest_service(
            Config {
                capture_on_pressure_change: true,
                imminent_oom_capture_delay_s: 10,
                critical_capture_delay_s: 10,
                warning_capture_delay_s: 10,
                normal_capture_delay_s: 10,
            },
            data_provider,
            stats_provider,
            pressure_provider,
            vec![],
            inspector.root().create_child("logger"),
        )?);
        let watcher =
            exec.run_singlethreaded(serve_pressure_stream.next()).transpose()?.expect("watcher");
        let _ = exec.run_singlethreaded(watcher.on_level_changed(fpressure::Level::Warning))?;
        let _ = exec.run_until_stalled(&mut digest_service);
        let output = exec
            .run_singlethreaded(fuchsia_inspect::reader::read(&inspector))
            .expect("got hierarchy");

        assert_data_tree!(output, root: {
            logger: {
                buckets: vec![
                    "Undigested",
                    "Orphaned",
                    "Kernel",
                    "Free",
                    "[Addl]PagerTotal",
                    "[Addl]PagerNewest",
                    "[Addl]PagerOldest",
                    "[Addl]DiscardableLocked",
                    "[Addl]DiscardableUnlocked",
                    "[Addl]ZramCompressedBytes",
                ],
                measurements: {
                    "0": {
                        timestamp: NonZeroIntProperty,
                        bucket_sizes: vec![
                            1024u64, // Undigested: matches the single unmatched VMO
                            6u64,    // Orphaned: vmo_bytes reported by the kernel but not covered by any bucket
                            31u64,   // Kernel: 3 wired + 4 heap + 7 mmu + 8 IPC + 9 other = 31
                            2u64,    // Free
                            14u64,   // [Addl]PagerTotal
                            15u64,   // [Addl]PagerNewest
                            16u64,   // [Addl]PagerOldest
                            18u64,   // [Addl]DiscardableLocked
                            19u64,   // [Addl]DiscardableUnlocked
                            21u64,   // [Addl]ZramCompressedBytes
                        ],
                    },
                },
            },
        });
        Ok(())
    }
}
