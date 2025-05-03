// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::util::cobalt_logger::log_cobalt_1dot1_batch;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload};
use {fuchsia_async as fasync, wlan_legacy_metrics_registry as metrics};

#[derive(Debug, PartialEq)]
pub enum ScanResult {
    Complete { num_results: usize },
    Failed,
    Cancelled,
}

pub struct ScanLogger {
    cobalt_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    scan_started_at: Option<fasync::BootInstant>,
}

impl ScanLogger {
    pub fn new(cobalt_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy) -> Self {
        Self { cobalt_proxy, scan_started_at: None }
    }

    pub async fn handle_scan_start(&mut self) {
        self.scan_started_at = Some(fasync::BootInstant::now());
        let metric_events = vec![MetricEvent {
            metric_id: metrics::SCAN_OCCURRENCE_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        }];
        log_cobalt_1dot1_batch!(self.cobalt_proxy, &metric_events, "handle_scan_start");
    }

    pub async fn handle_scan_result(&mut self, result: ScanResult) {
        let mut metric_events = vec![];
        let now = fasync::BootInstant::now();
        // Only log scan result metrics if there was a scan
        if let Some(scan_started_at) = self.scan_started_at.take() {
            match result {
                ScanResult::Complete { num_results } => {
                    let scan_duration = now - scan_started_at;
                    metric_events.push(MetricEvent {
                        metric_id: metrics::SCAN_FULFILLMENT_TIME_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::IntegerValue(scan_duration.into_millis()),
                    });
                    if num_results == 0 {
                        metric_events.push(MetricEvent {
                            metric_id: metrics::EMPTY_SCAN_RESULTS_METRIC_ID,
                            event_codes: vec![],
                            payload: MetricEventPayload::Count(1),
                        });
                    }
                }
                ScanResult::Failed => {
                    metric_events.push(MetricEvent {
                        metric_id: metrics::CLIENT_SCAN_FAILURE_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::Count(1),
                    });
                }
                ScanResult::Cancelled => {
                    metric_events.push(MetricEvent {
                        metric_id: metrics::ABORTED_SCAN_METRIC_ID,
                        event_codes: vec![],
                        payload: MetricEventPayload::Count(1),
                    });
                }
            }
        }

        if !metric_events.is_empty() {
            log_cobalt_1dot1_batch!(self.cobalt_proxy, &metric_events, "handle_scan_result");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{setup_test, TestHelper};
    use futures::task::Poll;
    use std::pin::pin;
    use test_case::test_case;

    fn run_handle_scan_start(test_helper: &mut TestHelper, scan_logger: &mut ScanLogger) {
        let mut test_fut = pin!(scan_logger.handle_scan_start());
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
    }

    fn run_handle_scan_result(
        test_helper: &mut TestHelper,
        scan_logger: &mut ScanLogger,
        scan_result: ScanResult,
    ) {
        let mut test_fut = pin!(scan_logger.handle_scan_result(scan_result));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
    }

    #[fuchsia::test]
    fn test_handle_scan_start() {
        let mut test_helper = setup_test();
        let mut scan_logger = ScanLogger::new(test_helper.cobalt_1dot1_proxy.clone());

        run_handle_scan_start(&mut test_helper, &mut scan_logger);

        let metrics = test_helper.get_logged_metrics(metrics::SCAN_OCCURRENCE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_handle_scan_result_complete() {
        let mut test_helper = setup_test();
        let mut scan_logger = ScanLogger::new(test_helper.cobalt_1dot1_proxy.clone());

        test_helper.exec.set_fake_time(fasync::MonotonicInstant::from_nanos(20_000_000));
        run_handle_scan_start(&mut test_helper, &mut scan_logger);

        test_helper.exec.set_fake_time(fasync::MonotonicInstant::from_nanos(100_000_000));
        let scan_result = ScanResult::Complete { num_results: 10 };
        run_handle_scan_result(&mut test_helper, &mut scan_logger, scan_result);

        let metrics = test_helper.get_logged_metrics(metrics::SCAN_FULFILLMENT_TIME_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::IntegerValue(80)); // 80ms
        let metrics = test_helper.get_logged_metrics(metrics::EMPTY_SCAN_RESULTS_METRIC_ID);
        assert!(metrics.is_empty());
    }

    #[fuchsia::test]
    fn test_handle_scan_result_empty() {
        let mut test_helper = setup_test();
        let mut scan_logger = ScanLogger::new(test_helper.cobalt_1dot1_proxy.clone());

        run_handle_scan_start(&mut test_helper, &mut scan_logger);

        let scan_result = ScanResult::Complete { num_results: 0 };
        run_handle_scan_result(&mut test_helper, &mut scan_logger, scan_result);

        let metrics = test_helper.get_logged_metrics(metrics::SCAN_FULFILLMENT_TIME_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        let metrics = test_helper.get_logged_metrics(metrics::EMPTY_SCAN_RESULTS_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_handle_scan_result_cancelled() {
        let mut test_helper = setup_test();
        let mut scan_logger = ScanLogger::new(test_helper.cobalt_1dot1_proxy.clone());

        run_handle_scan_start(&mut test_helper, &mut scan_logger);

        let scan_result = ScanResult::Cancelled;
        run_handle_scan_result(&mut test_helper, &mut scan_logger, scan_result);

        let metrics = test_helper.get_logged_metrics(metrics::ABORTED_SCAN_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_handle_scan_result_failure() {
        let mut test_helper = setup_test();
        let mut scan_logger = ScanLogger::new(test_helper.cobalt_1dot1_proxy.clone());

        run_handle_scan_start(&mut test_helper, &mut scan_logger);

        let scan_result = ScanResult::Failed;
        run_handle_scan_result(&mut test_helper, &mut scan_logger, scan_result);

        let metrics = test_helper.get_logged_metrics(metrics::CLIENT_SCAN_FAILURE_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }

    #[test_case(
        ScanResult::Complete { num_results: 10 },
        metrics::SCAN_FULFILLMENT_TIME_METRIC_ID;
        "scan complete"
    )]
    #[test_case(
        ScanResult::Failed,
        metrics::CLIENT_SCAN_FAILURE_METRIC_ID;
        "scan failed"
    )]
    #[test_case(
        ScanResult::Cancelled,
        metrics::ABORTED_SCAN_METRIC_ID;
        "scan cancelled"
    )]
    #[fuchsia::test(add_test_attr = false)]
    fn test_handle_scan_result_no_logging_to_cobalt_if_scan_not_started(
        scan_result: ScanResult,
        metric_id: u32,
    ) {
        let mut test_helper = setup_test();
        let mut scan_logger = ScanLogger::new(test_helper.cobalt_1dot1_proxy.clone());

        run_handle_scan_result(&mut test_helper, &mut scan_logger, scan_result);

        let metrics = test_helper.get_logged_metrics(metric_id);
        assert!(metrics.is_empty());
    }
}
