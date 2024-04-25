// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use async_trait::async_trait;
use errors::ModelError;
use fuchsia_inspect as inspect;
use fuchsia_inspect::{IntExponentialHistogramProperty, IntLinearHistogramProperty};
use fuchsia_sync as fsync;
use fuchsia_zircon as zx;
use inspect::HistogramProperty;
use moniker::Moniker;

use crate::model::hooks::{Event, EventPayload, EventType, HasEventType, Hook, HooksRegistration};

const STARTED_DURATIONS: &str = "started_durations";
const STOPPED_DURATIONS: &str = "stopped_durations";
const HISTOGRAM: &str = "histogram";

/// [-inf, 4, 7, 10, 16, 28, 52, 100, 196, 388, 772, 1540, 3076, 6148, inf]
const STARTED_DURATIONS_HISTOGRAM_PARAMS: inspect::ExponentialHistogramParams<i64> =
    inspect::ExponentialHistogramParams {
        floor: 4,
        initial_step: 3,
        step_multiplier: 2,
        buckets: 12,
    };

/// [-inf, 10, 20, 30, 40, ..., 240, 250, inf]
const STOPPED_DURATIONS_HISTOGRAM_PARAMS: inspect::LinearHistogramParams<i64> =
    inspect::LinearHistogramParams { floor: 10, step_size: 10, buckets: 24 };

type StopTime = zx::Time;

/// [`DurationStats`] tracks:
///
/// - durations an escrowing component was executing (`started_durations/histogram/MONIKER`)
/// - durations an escrowing component stayed stopped in-between two executions
///   (`stopped_durations/histogram/MONIKER`)
///
/// The tracking begins the first time a component sends an escrow request. Subsequently,
/// started/stopped durations will be tracked regardless if that component keeps sending
/// escrow requests.
///
/// The duration is measured in ticks in the Zircon monotonic clock, hence does
/// not account into times the system is suspended.
pub struct DurationStats {
    // Keeps the inspect node alive.
    _node: inspect::Node,
    started_durations: ComponentHistograms<IntExponentialHistogramProperty>,
    stopped_durations: ComponentHistograms<IntLinearHistogramProperty>,
    // The set of components that have sent an escrow request at least once,
    // and their last stop time.
    escrowing_components: fsync::Mutex<HashMap<Moniker, StopTime>>,
}

impl DurationStats {
    /// Creates a new duration tracker. Data will be written to the given inspect node.
    pub fn new(node: inspect::Node) -> Self {
        let started = node.create_child(STARTED_DURATIONS);
        let histogram = started.create_child(HISTOGRAM);
        node.record(started);
        let started_durations = ComponentHistograms {
            node: histogram,
            properties: Default::default(),
            init: |node, name| {
                node.create_int_exponential_histogram(name, STARTED_DURATIONS_HISTOGRAM_PARAMS)
            },
        };

        let stopped = node.create_child(STOPPED_DURATIONS);
        let histogram = stopped.create_child(HISTOGRAM);
        node.record(stopped);
        let stopped_durations = ComponentHistograms {
            node: histogram,
            properties: Default::default(),
            init: |node, name| {
                node.create_int_linear_histogram(name, STOPPED_DURATIONS_HISTOGRAM_PARAMS)
            },
        };

        Self {
            _node: node,
            started_durations,
            stopped_durations,
            escrowing_components: Default::default(),
        }
    }

    /// Provides the hook events that are needed to work.
    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "DurationStats",
            vec![EventType::Started, EventType::Stopped],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    fn on_component_started(self: &Arc<Self>, moniker: &Moniker, start_time: zx::Time) {
        if let Some(stop_time) = self.escrowing_components.lock().get(moniker) {
            let duration = start_time - *stop_time;
            self.stopped_durations.record(moniker, duration.into_seconds());
        }
    }

    fn on_component_stopped(
        self: &Arc<Self>,
        moniker: &Moniker,
        stop_time: zx::Time,
        execution_duration: zx::Duration,
        requested_escrow: bool,
    ) {
        let mut escrowing_components = self.escrowing_components.lock();
        if requested_escrow {
            escrowing_components.insert(moniker.clone(), stop_time);
        }
        if !escrowing_components.contains_key(moniker) {
            return;
        }
        self.started_durations.record(moniker, execution_duration.into_seconds());
    }
}

/// Maintains a histogram under each moniker where there is data.
///
/// The histogram will be a child property created under `node`, and will be named using
/// the component's moniker.
struct ComponentHistograms<H: HistogramProperty<Type = i64>> {
    node: inspect::Node,
    properties: fsync::Mutex<HashMap<Moniker, H>>,
    init: fn(&inspect::Node, String) -> H,
}

impl<H: HistogramProperty<Type = i64>> ComponentHistograms<H> {
    fn record(&self, moniker: &Moniker, value: i64) {
        let mut properties = self.properties.lock();
        let histogram = properties
            .entry(moniker.clone())
            .or_insert_with(|| (self.init)(&self.node, moniker.to_string()));
        histogram.insert(value);
    }
}

#[async_trait]
impl Hook for DurationStats {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let target_moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
        match event.event_type() {
            EventType::Started => {
                if let EventPayload::Started { runtime, .. } = &event.payload {
                    self.on_component_started(target_moniker, runtime.start_time);
                }
            }
            EventType::Stopped => {
                if let EventPayload::Stopped {
                    stop_time,
                    execution_duration,
                    requested_escrow,
                    ..
                } = &event.payload
                {
                    self.on_component_stopped(
                        target_moniker,
                        *stop_time,
                        *execution_duration,
                        *requested_escrow,
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use super::*;
    use crate::model::{
        component::{testing::wait_until_event_get_timestamp, StartReason},
        events::registry::EventSubscription,
        start::Start,
        testing::test_helpers::{ActionsTest, ComponentInfo},
    };
    use cm_rust::{Availability, UseEventStreamDecl, UseSource};
    use cm_rust_testing::ComponentDeclBuilder;
    use cm_types::Name;
    use diagnostics_assertions::{assert_data_tree, HistogramAssertion};
    use fidl::endpoints::ServerEnd;
    use fidl_fuchsia_component_runner as fcrunner;
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use futures::{channel::mpsc, StreamExt};
    use inspect::{ExponentialHistogramParams, LinearHistogramParams};

    // If a component stops without escrow, we should not record anything into the histograms.
    #[fuchsia::test]
    async fn no_escrow_no_values() {
        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let test = ActionsTest::new("root", components, None).await;
        let root = test.model.root();

        let inspector = inspect::Inspector::default();
        let stats = Arc::new(DurationStats::new(inspector.root().create_child("escrow")));
        root.hooks.install(stats.hooks()).await;
        root.ensure_started(&StartReason::Debug).await.unwrap();
        root.stop().await.unwrap();

        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {},
                },
                stopped_durations: {
                    histogram: {},
                }
            }
        });
    }

    // Tests the escrow stats as a component transitions between some lifecycles:
    //
    // - Start -> no records
    // - Stop with escrow once -> one started record
    // - Start again -> one started and one stopped record
    // - Stop again without escrow -> one started and two stopped records
    //
    #[fuchsia::test(allow_stalls = false)]
    async fn escrow_progression() {
        let (out_dir_tx, mut out_dir_rx) = mpsc::channel(1);
        let out_dir_tx = fsync::Mutex::new(out_dir_tx);

        let components = vec![("root", ComponentDeclBuilder::new().build())];
        let url = "test:///root_resolved";
        let test = ActionsTest::new(components[0].0, components, None).await;
        test.runner.add_host_fn(
            url,
            Box::new(move |server_end: ServerEnd<fio::DirectoryMarker>| {
                out_dir_tx.lock().try_send(server_end).unwrap();
            }),
        );
        let root = test.model.root();
        let inspector = inspect::Inspector::default();
        let stats = Arc::new(DurationStats::new(inspector.root().create_child("escrow")));
        root.hooks.install(stats.hooks()).await;

        let mut event_source =
            test.builtin_environment.lock().await.event_source_factory.create_for_above_root();
        let mut event_stream = event_source
            .subscribe(
                vec![EventType::Started.into(), EventType::Stopped.into()]
                    .into_iter()
                    .map(|event: Name| {
                        EventSubscription::new(UseEventStreamDecl {
                            source_name: event,
                            source: UseSource::Parent,
                            scope: None,
                            target_path: "/svc/fuchsia.component.EventStream".parse().unwrap(),
                            filter: None,
                            availability: Availability::Required,
                        })
                    })
                    .collect(),
            )
            .await
            .expect("subscribe to event stream");

        // Start -> no records
        root.ensure_started(&StartReason::Debug).await.unwrap();
        test.runner.wait_for_url(url).await;
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let start_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Started).await;
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {},
                },
                stopped_durations: {
                    histogram: {},
                }
            }
        });

        let mut started_assertion = HistogramAssertion::exponential(ExponentialHistogramParams {
            floor: 4,
            initial_step: 3,
            step_multiplier: 2,
            buckets: 12,
        });
        let mut stopped_assertion = HistogramAssertion::linear(LinearHistogramParams {
            floor: 10,
            step_size: 10,
            buckets: 24,
        });

        // Stop with escrow once -> one started record
        let info = ComponentInfo::new(root.clone()).await;
        let outgoing_server_end = out_dir_rx.next().await.unwrap();
        test.runner.send_on_escrow(
            &info.channel_id,
            fcrunner::ComponentControllerOnEscrowRequest {
                outgoing_dir: Some(outgoing_server_end),
                ..Default::default()
            },
        );
        test.runner.reset_wait_for_url(url);
        test.runner.abort_controller(&info.channel_id);
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let stop_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Stopped).await;
        started_assertion.insert_values(vec![(stop_timestamp - start_timestamp).into_seconds()]);
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {
                        ".": started_assertion.clone(),
                    },
                },
                stopped_durations: {
                    histogram: {},
                }
            }
        });

        // Start again -> one started and one stopped record
        root.ensure_started(&StartReason::Debug).await.unwrap();
        test.runner.wait_for_url(url).await;
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let start_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Started).await;
        stopped_assertion.insert_values(vec![(start_timestamp - stop_timestamp).into_seconds()]);
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {
                        ".": started_assertion.clone(),
                    },
                },
                stopped_durations: {
                    histogram: {
                        ".": stopped_assertion.clone(),
                    },
                }
            }
        });

        // Stop again without escrow -> one started and two stopped records
        root.stop().await.unwrap();
        _ = fasync::TestExecutor::poll_until_stalled(future::pending::<()>()).await;
        let stop_timestamp =
            wait_until_event_get_timestamp(&mut event_stream, EventType::Stopped).await;
        started_assertion.insert_values(vec![(stop_timestamp - start_timestamp).into_seconds()]);
        assert_data_tree!(inspector, root: {
            escrow: {
                started_durations: {
                    histogram: {
                        ".": started_assertion,
                    },
                },
                stopped_durations: {
                    histogram: {
                        ".": stopped_assertion,
                    },
                }
            }
        });
    }
}
