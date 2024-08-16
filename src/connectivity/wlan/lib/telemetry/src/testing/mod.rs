// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use super::*;
use fidl::endpoints::create_proxy_and_stream;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventLoggerRequest, MetricEventPayload};
use fuchsia_async as fasync;
use fuchsia_inspect::reader::{
    DiagnosticsHierarchy, {self as reader},
};
use fuchsia_inspect::{Inspector, Node as InspectNode};
use futures::task::Poll;
use std::pin::pin;

trait CobaltExt {
    // Respond to MetricEventLoggerRequest and extract its MetricEvent
    fn respond_to_metric_req(
        self,
        result: Result<(), fidl_fuchsia_metrics::Error>,
    ) -> Vec<fidl_fuchsia_metrics::MetricEvent>;
}

impl CobaltExt for MetricEventLoggerRequest {
    fn respond_to_metric_req(
        self,
        result: Result<(), fidl_fuchsia_metrics::Error>,
    ) -> Vec<fidl_fuchsia_metrics::MetricEvent> {
        match self {
            Self::LogOccurrence { metric_id, count, event_codes, responder } => {
                assert!(responder.send(result).is_ok());
                vec![MetricEvent {
                    metric_id,
                    event_codes,
                    payload: MetricEventPayload::Count(count),
                }]
            }
            Self::LogInteger { metric_id, value, event_codes, responder } => {
                assert!(responder.send(result).is_ok());
                vec![MetricEvent {
                    metric_id,
                    event_codes,
                    payload: MetricEventPayload::IntegerValue(value),
                }]
            }
            Self::LogIntegerHistogram { metric_id, histogram, event_codes, responder } => {
                assert!(responder.send(result).is_ok());
                vec![MetricEvent {
                    metric_id,
                    event_codes,
                    payload: MetricEventPayload::Histogram(histogram),
                }]
            }
            Self::LogString { metric_id, string_value, event_codes, responder } => {
                assert!(responder.send(result).is_ok());
                vec![MetricEvent {
                    metric_id,
                    event_codes,
                    payload: MetricEventPayload::StringValue(string_value),
                }]
            }
            Self::LogMetricEvents { events, responder } => {
                assert!(responder.send(result).is_ok());
                events
            }
        }
    }
}

pub struct TestHelper {
    pub inspector: Inspector,
    pub inspect_node: InspectNode,
    pub inspect_metadata_node: InspectNode,

    pub cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    cobalt_1dot1_stream: fidl_fuchsia_metrics::MetricEventLoggerRequestStream,
    /// As requests to Cobalt are responded to via `self.drain_cobalt_events()`,
    /// their payloads are drained to this HashMap
    cobalt_events: Vec<MetricEvent>,

    pub persistence_sender: mpsc::Sender<String>,
    persistence_stream: mpsc::Receiver<String>,

    // Note: keep the executor field last in the struct so it gets dropped last.
    pub exec: fasync::TestExecutor,
}

impl TestHelper {
    /// Continually execute the future and respond to any incoming Cobalt request with Ok.
    /// Append each metric request payload into `self.cobalt_events`.
    pub fn run_until_stalled_drain_cobalt_events<F>(&mut self, test_fut: &mut F) -> Poll<F::Output>
    where
        F: Future + Unpin,
    {
        let mut made_progress = true;
        let mut result = Poll::Pending;
        while made_progress {
            result = self.exec.run_until_stalled(test_fut);
            made_progress = false;
            while let Poll::Ready(Some(Ok(req))) =
                self.exec.run_until_stalled(&mut self.cobalt_1dot1_stream.next())
            {
                self.cobalt_events.append(&mut req.respond_to_metric_req(Ok(())));
                made_progress = true;
            }
        }
        result
    }

    pub fn get_logged_metrics(&self, metric_id: u32) -> Vec<MetricEvent> {
        self.cobalt_events.iter().filter(|ev| ev.metric_id == metric_id).cloned().collect()
    }

    /// Empty the cobalt metrics can be stored so that future checks on cobalt metrics can
    /// ignore previous values.
    // TODO(339221340): remove these allows once the skeleton has a few uses
    #[allow(unused)]
    pub fn clear_cobalt_events(&mut self) {
        self.cobalt_events = Vec::new();
    }

    pub fn get_inspect_data_tree(&mut self) -> DiagnosticsHierarchy {
        let read_fut = reader::read(&self.inspector);
        let mut read_fut = pin!(read_fut);
        match self.exec.run_until_stalled(&mut read_fut) {
            Poll::Pending => {
                panic!("Unexpected pending state");
            }
            Poll::Ready(result) => {
                let hierarchy = result.expect("failed to get hierarchy");
                return hierarchy;
            }
        }
    }

    pub fn create_inspect_node(&mut self, name: &str) -> InspectNode {
        self.inspector.root().create_child(name)
    }

    // TODO(339221340): remove these allows once the skeleton has a few uses
    #[allow(unused)]
    pub fn get_persistence_reqs(&mut self) -> Vec<String> {
        let mut persistence_reqs = vec![];
        loop {
            match self.persistence_stream.try_next() {
                Ok(Some(tag)) => persistence_reqs.push(tag),
                _ => return persistence_reqs,
            }
        }
    }
}

pub fn setup_test() -> TestHelper {
    let exec = fasync::TestExecutor::new_with_fake_time();
    exec.set_fake_time(fasync::Time::from_nanos(0));

    let (cobalt_1dot1_proxy, cobalt_1dot1_stream) =
        create_proxy_and_stream::<fidl_fuchsia_metrics::MetricEventLoggerMarker>()
            .expect("failed to create MetricsEventLogger proxy");

    let inspector = Inspector::default();
    let inspect_node = inspector.root().create_child("test_stats");
    let inspect_metadata_node = inspect_node.create_child("metadata");

    const DEFAULT_BUFFER_SIZE: usize = 100; // arbitrary value
    let (persistence_sender, persistence_stream) = mpsc::channel(DEFAULT_BUFFER_SIZE);

    TestHelper {
        inspector,
        inspect_node,
        inspect_metadata_node,
        cobalt_1dot1_stream,
        cobalt_1dot1_proxy,
        cobalt_events: vec![],
        persistence_sender,
        persistence_stream,
        exec,
    }
}
