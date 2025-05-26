// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use cobalt_client::traits::AsEventCode;
use futures::StreamExt;
use memory_metrics_registry::cobalt_registry;
use stalls::StallProvider;
use zx::MonotonicInstant;
use {anyhow, fidl_fuchsia_metrics as fmetrics};

use crate::error_from_metrics_error;

/// Collect and publish to Cobalt memory stall increase rate, every hour.
pub async fn collect_stalls_forever(
    stalls_provider: Arc<impl StallProvider + 'static>,
    metric_event_logger: fmetrics::MetricEventLoggerProxy,
) -> Result<(), anyhow::Error> {
    let mut last_stall = zx::MemoryStall::default();

    // Wait for one hour after device start to get the first stall value. We don't use the one-hour
    // timer as we may have been started later than at boot exactly.
    fuchsia_async::Timer::new(MonotonicInstant::ZERO + zx::Duration::from_hours(1)).await;

    let mut timer = fuchsia_async::Interval::new(zx::Duration::from_hours(1));
    loop {
        let new_stall = stalls_provider.get_stall_info()?;

        let stall_some_event = fmetrics::MetricEvent {
            metric_id: cobalt_registry::MEMORY_STALLS_PER_HOUR_METRIC_ID,
            payload: fmetrics::MetricEventPayload::IntegerValue(
                new_stall.stall_time_some - last_stall.stall_time_some,
            ),
            event_codes: vec![cobalt_registry::MemoryMetricDimensionStallType::Some.as_event_code()],
        };
        let stall_full_event = fmetrics::MetricEvent {
            metric_id: cobalt_registry::MEMORY_STALLS_PER_HOUR_METRIC_ID,
            payload: fmetrics::MetricEventPayload::IntegerValue(
                new_stall.stall_time_full - last_stall.stall_time_full,
            ),
            event_codes: vec![cobalt_registry::MemoryMetricDimensionStallType::Full.as_event_code()],
        };

        last_stall = new_stall;

        let events = vec![stall_some_event, stall_full_event];
        metric_event_logger.log_metric_events(&events).await?.map_err(error_from_metrics_error)?;
        timer.next().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use fuchsia_async as fasync;
    use futures::task::Poll;
    use std::sync::atomic::{AtomicU32, Ordering};
    use zx::Duration;

    fn get_stall_provider() -> Arc<impl StallProvider + 'static> {
        struct FakeStallProvider {
            count: AtomicU32,
        }

        impl Default for FakeStallProvider {
            fn default() -> Self {
                Self { count: AtomicU32::new(1) }
            }
        }

        impl StallProvider for FakeStallProvider {
            fn get_stall_info(&self) -> Result<zx::MemoryStall, anyhow::Error> {
                let count = self.count.fetch_add(1, Ordering::Relaxed);
                let memory_stall = zx::MemoryStall {
                    stall_time_some: (count * 10) as i64,
                    stall_time_full: (count * 20) as i64,
                };
                Ok(memory_stall)
            }
        }

        Arc::new(FakeStallProvider::default())
    }

    #[test]
    fn test_periodic_stalls_collection() -> anyhow::Result<()> {
        // Setup executor.
        let mut exec = fasync::TestExecutor::new_with_fake_time();

        // Setup mock data providers.
        let data_provider = get_stall_provider();

        // Setup test proxy to observe emitted events from the service.
        let (metric_event_logger, metric_event_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fmetrics::MetricEventLoggerMarker>();

        // Set the time to shortly after boot
        exec.set_fake_time((zx::MonotonicInstant::ZERO + Duration::from_seconds(3 * 60)).into());

        // Service under test.
        let mut stalls_collector =
            fuchsia_async::Task::spawn(collect_stalls_forever(data_provider, metric_event_logger));

        // Give the service the opportunity to run.
        assert!(
            exec.run_until_stalled(&mut stalls_collector).is_pending(),
            "Stalls collection service returned unexpectedly early"
        );

        // Ensure no metrics has been uploaded yet.
        let mut metric_event_request_future = metric_event_request_stream.into_future();
        assert!(
            exec.run_until_stalled(&mut metric_event_request_future).is_pending(),
            "Stalls collection service returned unexpectedly early"
        );

        // Fake the passage of time, so that collect_metrics may do a capture.
        assert!(
            exec.run_until_stalled(&mut std::pin::pin!(fasync::TestExecutor::advance_to(
                exec.now() + Duration::from_seconds(60 * 60 + 10)
            )))
            .is_ready(),
            "Failed to advance time"
        );

        // Ensure we have one and only one event ready for consumption.
        let Poll::Ready((event, metric_event_request_stream)) =
            exec.run_until_stalled(&mut metric_event_request_future)
        else {
            panic!("Failed to receive metrics")
        };
        let event = event.ok_or_else(|| anyhow!("Metrics stream unexpectedly closed"))??;
        match event {
            fmetrics::MetricEventLoggerRequest::LogMetricEvents { events, responder, .. } => {
                assert_eq!(events.len(), 2);
                // Kernel metrics
                assert_eq!(
                    events[0],
                    fmetrics::MetricEvent {
                        metric_id: cobalt_registry::MEMORY_STALLS_PER_HOUR_METRIC_ID,
                        event_codes: vec![
                            cobalt_registry::MemoryMetricDimensionStallType::Some.as_event_code()
                        ],
                        payload: fmetrics::MetricEventPayload::IntegerValue(10)
                    }
                );
                assert_eq!(
                    events[1],
                    fmetrics::MetricEvent {
                        metric_id: cobalt_registry::MEMORY_STALLS_PER_HOUR_METRIC_ID,
                        event_codes: vec![
                            cobalt_registry::MemoryMetricDimensionStallType::Full.as_event_code()
                        ],
                        payload: fmetrics::MetricEventPayload::IntegerValue(20)
                    }
                );
                responder.send(Ok(()))?;
            }
            _ => panic!("Unexpected metric event"),
        }

        let mut metric_event_request_future = metric_event_request_stream.into_future();

        assert!(exec.run_until_stalled(&mut metric_event_request_future).is_pending());

        // Advance to the next hour
        assert!(
            exec.run_until_stalled(&mut std::pin::pin!(fasync::TestExecutor::advance_to(
                (zx::MonotonicInstant::ZERO + Duration::from_seconds(60 * 60 * 2 + 10)).into()
            )))
            .is_ready(),
            "Failed to advance time"
        );

        // Ensure we have one and only one event ready for consumption.
        let Poll::Ready((event, metric_event_request_stream)) =
            exec.run_until_stalled(&mut metric_event_request_future)
        else {
            panic!("Failed to receive metrics")
        };
        let event = event.ok_or_else(|| anyhow!("Metrics stream unexpectedly closed"))??;
        match event {
            fmetrics::MetricEventLoggerRequest::LogMetricEvents { events, responder, .. } => {
                assert_eq!(events.len(), 2);
                // Kernel metrics
                assert_eq!(
                    events[0],
                    fmetrics::MetricEvent {
                        metric_id: cobalt_registry::MEMORY_STALLS_PER_HOUR_METRIC_ID,
                        event_codes: vec![
                            cobalt_registry::MemoryMetricDimensionStallType::Some.as_event_code()
                        ],
                        payload: fmetrics::MetricEventPayload::IntegerValue(10)
                    }
                );
                assert_eq!(
                    events[1],
                    fmetrics::MetricEvent {
                        metric_id: cobalt_registry::MEMORY_STALLS_PER_HOUR_METRIC_ID,
                        event_codes: vec![
                            cobalt_registry::MemoryMetricDimensionStallType::Full.as_event_code()
                        ],
                        payload: fmetrics::MetricEventPayload::IntegerValue(20)
                    }
                );
                responder.send(Ok(()))?;
            }
            _ => panic!("Unexpected metric event"),
        }

        assert!(exec
            .run_until_stalled(&mut metric_event_request_stream.into_future())
            .is_pending());

        Ok(())
    }
}
