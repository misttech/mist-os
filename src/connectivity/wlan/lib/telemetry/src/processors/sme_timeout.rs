// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::util::cobalt_logger::log_cobalt_batch;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventLoggerProxy, MetricEventPayload};

use wlan_legacy_metrics_registry as metrics;

pub struct SmeTimeoutLogger {
    cobalt_proxy: MetricEventLoggerProxy,
}

impl SmeTimeoutLogger {
    pub fn new(cobalt_proxy: MetricEventLoggerProxy) -> Self {
        Self { cobalt_proxy }
    }

    pub async fn handle_sme_timeout_event(&self) {
        let metric_events = vec![MetricEvent {
            metric_id: metrics::SME_OPERATION_TIMEOUT_2_METRIC_ID,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        }];
        log_cobalt_batch!(self.cobalt_proxy, &metric_events, "handle_sme_timeout_event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{setup_test, TestHelper};
    use futures::task::Poll;
    use std::pin::pin;

    fn run_handle_sme_timeout_event(
        test_helper: &mut TestHelper,
        sme_timeout_logger: &SmeTimeoutLogger,
    ) {
        let mut test_fut = pin!(sme_timeout_logger.handle_sme_timeout_event());
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );
    }

    #[fuchsia::test]
    fn test_handle_sme_timeout_event() {
        let mut test_helper = setup_test();
        let sme_timeout_logger = SmeTimeoutLogger::new(test_helper.cobalt_proxy.clone());

        run_handle_sme_timeout_event(&mut test_helper, &sme_timeout_logger);

        let metrics = test_helper.get_logged_metrics(metrics::SME_OPERATION_TIMEOUT_2_METRIC_ID);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].payload, MetricEventPayload::Count(1));
    }
}
