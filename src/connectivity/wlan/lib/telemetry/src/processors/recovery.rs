// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::util::cobalt_logger::log_cobalt_batch;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventLoggerProxy, MetricEventPayload};

use wlan_legacy_metrics_registry as metrics;

pub struct RecoveryLogger {
    cobalt_proxy: MetricEventLoggerProxy,
}

impl RecoveryLogger {
    pub fn new(cobalt_proxy: MetricEventLoggerProxy) -> Self {
        Self { cobalt_proxy }
    }

    pub async fn handle_recovery_event(&self) {
        let metric_events = vec![MetricEvent {
            metric_id: metrics::RECOVERY_OCCURRENCE_2_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        }];
        log_cobalt_batch!(self.cobalt_proxy, &metric_events, "handle_recovery_event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{setup_test, TestHelper};
    use futures::task::Poll;
    use std::pin::pin;

    fn run_handle_recovery_event(test_helper: &mut TestHelper, recovery_logger: &RecoveryLogger) {
        let mut test_fut = pin!(recovery_logger.handle_recovery_event());
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
    }

    #[fuchsia::test]
    fn test_handle_recovery_event() {
        let mut test_helper = setup_test();
        let recovery_logger = RecoveryLogger::new(test_helper.cobalt_proxy.clone());

        run_handle_recovery_event(&mut test_helper, &recovery_logger);

        let metrics = test_helper.get_logged_metrics(metrics::RECOVERY_OCCURRENCE_2_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }
}
