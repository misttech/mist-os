// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use attribution_processing::digest::{BucketDefinition, Digest};
use attribution_processing::AttributionDataProvider;
use cobalt_client::traits::{AsEventCode, AsEventCodes};
use fuchsia_async::{Interval, Task};
use futures::stream::{StreamExt, TryStreamExt};
use futures::{try_join, TryFutureExt};
use memory_metrics_registry::cobalt_registry;
use std::sync::Arc;

use cobalt_registry::MemoryLeakMigratedMetricDimensionTimeSinceBoot as TimeSinceBoot;
use {fidl_fuchsia_kernel as fkernel, fidl_fuchsia_metrics as fmetrics};

fn error_from_metrics_error(error: fmetrics::Error) -> anyhow::Error {
    anyhow!("{:?}", error)
}

/// Sorted list mapping durations to the largest event that is lower.
const UPTIME_LEVEL_INDEX: &[(zx::BootDuration, TimeSinceBoot)] = &[
    (zx::BootDuration::from_minutes(1), TimeSinceBoot::Up),
    (zx::BootDuration::from_minutes(30), TimeSinceBoot::UpOneMinute),
    (zx::BootDuration::from_hours(1), TimeSinceBoot::UpThirtyMinutes),
    (zx::BootDuration::from_hours(6), TimeSinceBoot::UpOneHour),
    (zx::BootDuration::from_hours(12), TimeSinceBoot::UpSixHours),
    (zx::BootDuration::from_hours(24), TimeSinceBoot::UpTwelveHours),
    (zx::BootDuration::from_hours(48), TimeSinceBoot::UpOneDay),
    (zx::BootDuration::from_hours(72), TimeSinceBoot::UpTwoDays),
    (zx::BootDuration::from_hours(144), TimeSinceBoot::UpThreeDays),
];

fn bucket_name_to_dimension(
    name: &str,
) -> Option<cobalt_registry::MemoryMigratedMetricDimensionBucket> {
    match name {
        "TotalBytes" => Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::TotalBytes),
        "Free" => Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::Free),
        "Kernel" => Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::Kernel),
        "Orphaned" => Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::Orphaned),
        "Undigested" => Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::Undigested),
        "[Addl]PagerTotal" => {
            Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::__Addl_PagerTotal)
        }
        "[Addl]PagerNewest" => {
            Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::__Addl_PagerNewest)
        }
        "[Addl]PagerOldest" => {
            Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::__Addl_PagerOldest)
        }
        "[Addl]DiscardableLocked" => {
            Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::__Addl_DiscardableLocked)
        }
        "[Addl]DiscardableUnlocked" => {
            Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::__Addl_DiscardableUnlocked)
        }
        "[Addl]ZramCompressedBytes" => {
            Some(cobalt_registry::MemoryMigratedMetricDimensionBucket::__Addl_ZramCompressedBytes)
        }
        _ => None,
    }
}

/// Convert an instant to the code corresponding to the largest uptime that is smaller.
fn get_uptime_event_code(capture_time: zx::BootInstant) -> TimeSinceBoot {
    let uptime = zx::Duration::from_nanos(capture_time.into_nanos());
    UPTIME_LEVEL_INDEX
        .into_iter()
        .find(|&&(time, _)| uptime < time)
        .map(|(_, code)| *code)
        .unwrap_or(TimeSinceBoot::UpSixDays)
}

fn kmem_events(kmem_stats: &fkernel::MemoryStats) -> impl Iterator<Item = fmetrics::MetricEvent> {
    use cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown as Breakdown;
    let make_event = |code: Breakdown, value| {
        Some(fmetrics::MetricEvent {
            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
            event_codes: vec![code.as_event_code()],
            payload: fmetrics::MetricEventPayload::IntegerValue(value? as i64),
        })
    };
    vec![
        make_event(Breakdown::TotalBytes, kmem_stats.total_bytes),
        make_event(
            Breakdown::UsedBytes,
            (|| Some((kmem_stats.total_bytes? as i64 - kmem_stats.free_bytes? as i64) as u64))(),
        ),
        make_event(Breakdown::FreeBytes, kmem_stats.free_bytes),
        make_event(Breakdown::VmoBytes, kmem_stats.vmo_bytes),
        make_event(Breakdown::KernelFreeHeapBytes, kmem_stats.free_heap_bytes),
        make_event(Breakdown::MmuBytes, kmem_stats.mmu_overhead_bytes),
        make_event(Breakdown::IpcBytes, kmem_stats.ipc_bytes),
        make_event(Breakdown::KernelTotalHeapBytes, kmem_stats.total_heap_bytes),
        make_event(Breakdown::WiredBytes, kmem_stats.wired_bytes),
        make_event(Breakdown::OtherBytes, kmem_stats.other_bytes),
    ]
    .into_iter()
    .flatten()
}

fn kmem_events_with_uptime(
    kmem_stats: &fkernel::MemoryStats,
    capture_time: zx::BootInstant,
) -> impl Iterator<Item = fmetrics::MetricEvent> {
    use cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown as Breakdown;
    let make_event = |code: Breakdown, value| {
        Some(fmetrics::MetricEvent {
            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                general_breakdown: code,
                time_since_boot: get_uptime_event_code(capture_time),
            }
            .as_event_codes(),
            payload: fmetrics::MetricEventPayload::IntegerValue(value? as i64),
        })
    };
    vec![
        make_event(Breakdown::TotalBytes, kmem_stats.total_bytes),
        make_event(
            Breakdown::UsedBytes,
            (|| Some((kmem_stats.total_bytes? as i64 - kmem_stats.free_bytes? as i64) as u64))(),
        ),
        make_event(Breakdown::FreeBytes, kmem_stats.free_bytes),
        make_event(Breakdown::VmoBytes, kmem_stats.vmo_bytes),
        make_event(Breakdown::KernelFreeHeapBytes, kmem_stats.free_heap_bytes),
        make_event(Breakdown::MmuBytes, kmem_stats.mmu_overhead_bytes),
        make_event(Breakdown::IpcBytes, kmem_stats.ipc_bytes),
        make_event(Breakdown::KernelTotalHeapBytes, kmem_stats.total_heap_bytes),
        make_event(Breakdown::WiredBytes, kmem_stats.wired_bytes),
        make_event(Breakdown::OtherBytes, kmem_stats.other_bytes),
    ]
    .into_iter()
    .flatten()
}

fn digest_events(digest: Digest) -> impl Iterator<Item = fmetrics::MetricEvent> {
    digest.buckets.into_iter().filter_map(|bucket| {
        Some(fmetrics::MetricEvent {
            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
            event_codes: vec![bucket_name_to_dimension(&bucket.name)?.as_event_code()],
            payload: fmetrics::MetricEventPayload::IntegerValue(bucket.size as i64),
        })
    })
}

/// Provided a connection to a MetricEventLoggerFactory, request a MetricEventLogger appropriately
/// configured to log memory metrics.
pub async fn create_metric_event_logger(
    factory: fmetrics::MetricEventLoggerFactoryProxy,
) -> Result<fmetrics::MetricEventLoggerProxy> {
    let project_spec = fmetrics::ProjectSpec {
        customer_id: Some(cobalt_registry::CUSTOMER_ID),
        project_id: Some(cobalt_registry::PROJECT_ID),
        ..Default::default()
    };
    let (metric_event_logger, server_end) = fidl::endpoints::create_proxy();
    factory
        .create_metric_event_logger(&project_spec, server_end)
        .await
        .map_err(anyhow::Error::from)?
        .map_err(error_from_metrics_error)?;
    Ok(metric_event_logger)
}

/// Periodically upload metrics related to memory usage.
pub fn collect_metrics(
    attribution_data_service: Arc<impl AttributionDataProvider + 'static>,
    kernel_stats_proxy: fkernel::StatsProxy,
    metric_event_logger: fmetrics::MetricEventLoggerProxy,
    bucket_definitions: Vec<BucketDefinition>,
) -> Task<Result<()>> {
    Task::spawn(async move {
        Interval::new(zx::Duration::from_minutes(5))
            .map(Ok::<(), anyhow::Error>)
            .try_fold(
                (attribution_data_service, kernel_stats_proxy, metric_event_logger),
                |(attribution_data_service, kernel_stats_proxy, metric_event_logger), _| async {
                    let timestamp = zx::BootInstant::get();
                    let (kmem_stats, kmem_stats_compression) = try_join!(
                        kernel_stats_proxy.get_memory_stats().map_err(anyhow::Error::from),
                        kernel_stats_proxy
                            .get_memory_stats_compression()
                            .map_err(anyhow::Error::from)
                    )?;
                    let digest = Digest::compute(
                        attribution_data_service.clone(),
                        &kmem_stats,
                        &kmem_stats_compression,
                        &bucket_definitions,
                    )?;

                    let events = kmem_events(&kmem_stats)
                        .chain(kmem_events_with_uptime(&kmem_stats, timestamp))
                        .chain(digest_events(digest));
                    metric_event_logger
                        .log_metric_events(&events.collect::<Vec<fmetrics::MetricEvent>>())
                        .await
                        .map_err(anyhow::Error::from)?
                        .map_err(error_from_metrics_error)?;
                    Ok((attribution_data_service, kernel_stats_proxy, metric_event_logger))
                },
            )
            .await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use attribution_processing::testing::FakeAttributionDataProvider;
    use attribution_processing::{
        Attribution, AttributionData, Principal, PrincipalDescription, PrincipalIdentifier,
        PrincipalType, Resource, ResourceReference, ZXName,
    };
    use futures::task::Poll;
    use std::time::Duration;
    use {fidl_fuchsia_memory_attribution_plugin as fplugin, fuchsia_async as fasync};

    fn get_attribution_data_provider() -> Arc<impl AttributionDataProvider + 'static> {
        let attribution_data = AttributionData {
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
        };
        Arc::new(FakeAttributionDataProvider { attribution_data })
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
    fn test_periodic_metrics_collection() -> anyhow::Result<()> {
        // Setup executor.
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Setup mock data providers.
        let data_provider = get_attribution_data_provider();
        let (stats_provider, stats_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::StatsMarker>();
        fasync::Task::spawn(async move {
            serve_kernel_stats(stats_request_stream).await.unwrap();
        })
        .detach();
        let bucket_definitions = vec![];

        // Setup test proxy to observe emitted events from the service.
        let (metric_event_logger, metric_event_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fmetrics::MetricEventLoggerMarker>();

        // Service under test.
        let mut metrics_collector =
            collect_metrics(data_provider, stats_provider, metric_event_logger, bucket_definitions);

        // Give the service the opportunity to run.
        assert!(
            exec.run_until_stalled(&mut metrics_collector).is_pending(),
            "Metrics collection service returned unexpectedly early"
        );

        // Ensure no metrics has been uploaded yet.
        let mut metric_event_request_future = metric_event_request_stream.into_future();
        assert!(
            exec.run_until_stalled(&mut metric_event_request_future).is_pending(),
            "Metrics collection service returned unexpectedly early"
        );

        // Fake the passage of time, so that collect_metrics may do a capture.
        assert!(
            exec.run_until_stalled(&mut std::pin::pin!(fasync::TestExecutor::advance_to(
                exec.now() + Duration::from_secs(5 * 60).into()
            )))
            .is_ready(),
            "Failed to advance time"
        );
        let uptime = get_uptime_event_code(zx::BootInstant::get());

        // Ensure we have one and only one event ready for consumption.
        let Poll::Ready((event, metric_event_request_stream)) =
            exec.run_until_stalled(&mut metric_event_request_future)
        else {
            panic!("Failed to receive metrics")
        };
        let event = event.ok_or_else(|| anyhow!("Metrics stream unexpectedly closed"))??;
        match event {
            fmetrics::MetricEventLoggerRequest::LogMetricEvents { events, .. } => {
                // Kernel metrics
                assert_eq!(
                    &events[0..10],
                    vec![
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::TotalBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(1)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::UsedBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(-1)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::FreeBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(2)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::VmoBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(6)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::KernelFreeHeapBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(5)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::MmuBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(7)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::IpcBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(8)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::KernelTotalHeapBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(4)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::WiredBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(3)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_GENERAL_BREAKDOWN_MIGRATED_METRIC_ID,
                            event_codes: vec![cobalt_registry::MemoryGeneralBreakdownMigratedMetricDimensionGeneralBreakdown::OtherBytes.as_event_code()],
                            payload: fmetrics::MetricEventPayload::IntegerValue(9)
                        },]);
                // Kernel metrics with uptime
                assert_eq!(
                    &events[10..20],
                    vec![
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::TotalBytes, time_since_boot: uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(1)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::UsedBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(-1)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::FreeBytes, time_since_boot:uptime}.as_event_codes(),                            payload: fmetrics::MetricEventPayload::IntegerValue(2)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::VmoBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(6)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::KernelFreeHeapBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(5)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::MmuBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(7)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::IpcBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(8)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::KernelTotalHeapBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(4)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::WiredBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(3)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_LEAK_MIGRATED_METRIC_ID,
                            event_codes: cobalt_registry::MemoryLeakMigratedEventCodes {
                                general_breakdown: cobalt_registry::MemoryLeakMigratedMetricDimensionGeneralBreakdown::OtherBytes, time_since_boot:uptime}.as_event_codes(),
                            payload: fmetrics::MetricEventPayload::IntegerValue(9)
                        },
                    ]
                );
                // Digest metrics
                assert_eq!(
                    &events[20..],
                    vec![
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![19],
                            payload: fmetrics::MetricEventPayload::IntegerValue(1024)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![18],
                            payload: fmetrics::MetricEventPayload::IntegerValue(6)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![17],
                            payload: fmetrics::MetricEventPayload::IntegerValue(31)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![16],
                            payload: fmetrics::MetricEventPayload::IntegerValue(2)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![32],
                            payload: fmetrics::MetricEventPayload::IntegerValue(14)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![33],
                            payload: fmetrics::MetricEventPayload::IntegerValue(15)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![34],
                            payload: fmetrics::MetricEventPayload::IntegerValue(16)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![35],
                            payload: fmetrics::MetricEventPayload::IntegerValue(18)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![36],
                            payload: fmetrics::MetricEventPayload::IntegerValue(19)
                        },
                        fmetrics::MetricEvent {
                            metric_id: cobalt_registry::MEMORY_MIGRATED_METRIC_ID,
                            event_codes: vec![76],
                            payload: fmetrics::MetricEventPayload::IntegerValue(21)
                        }
                    ]
                )
            }
            _ => panic!("Unexpected metric event"),
        }

        assert!(exec
            .run_until_stalled(&mut metric_event_request_stream.into_future())
            .is_pending());
        Ok(())
    }
}
