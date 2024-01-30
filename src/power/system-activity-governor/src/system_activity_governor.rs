// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{self, Context, Result};
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_power_broker as fbroker;
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::prelude::*;
use power_broker_client::PowerElementContext;
use std::{cell::RefCell, rc::Rc};

type NotifyFn = Box<dyn Fn(&fsuspend::SuspendStats, fsuspend::StatsWatchResponder) -> bool>;
type StatsHangingGet = HangingGet<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;

enum IncomingRequest {
    ActivityGovernor(fsystem::ActivityGovernorRequestStream),
    Stats(fsuspend::StatsRequestStream),
}

const EXECUTION_STATE_ACTIVE: fbroker::PowerLevel = 2;
const EXECUTION_STATE_WAKE_HANDLING: fbroker::PowerLevel = 1;
const EXECUTION_STATE_INACTIVE: fbroker::PowerLevel = 0;

/// SystemActivityGovernor runs the server for fuchsia.power.suspend FIDL APIs.
pub struct SystemActivityGovernor {
    execution_state: PowerElementContext,
    stats_hanging_get: RefCell<StatsHangingGet>,
}

impl SystemActivityGovernor {
    pub async fn new(topology: &fbroker::TopologyProxy) -> Result<Rc<Self>> {
        let initial_stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };

        let execution_state = PowerElementContext::new(
            topology,
            "execution_state",
            0,
            vec![EXECUTION_STATE_INACTIVE, EXECUTION_STATE_WAKE_HANDLING, EXECUTION_STATE_ACTIVE],
            Vec::new(),
            Vec::new(),
        )
        .await?;

        Ok(Rc::new(Self {
            execution_state,
            stats_hanging_get: RefCell::new(HangingGet::new(
                initial_stats,
                Box::new(
                    |stats: &fsuspend::SuspendStats, res: fsuspend::StatsWatchResponder| -> bool {
                        if let Err(e) = res.send(stats) {
                            tracing::warn!("Failed to send suspend stats to client: {e:?}");
                        }
                        true
                    },
                ),
            )),
        }))
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend and fuchsia.power.system API requests.
    pub async fn run(self: Rc<Self>) -> Result<()> {
        fasync::Task::local(self.clone().watch_execution_state()).detach();
        self.run_fidl_server().await
    }

    async fn watch_execution_state(self: Rc<Self>) {
        let stats_publisher = self.stats_hanging_get.borrow().new_publisher();
        let mut last_required_level = 0;
        loop {
            match self.execution_state.level_control.watch_required_level(last_required_level).await
            {
                Ok(Ok(required_level)) => {
                    tracing::debug!(
                        "watch_execution_state: required power level = {required_level:?}, last power level = {last_required_level:?}");

                    let res = self
                        .execution_state
                        .level_control
                        .update_current_power_level(required_level)
                        .await;
                    if let Err(e) = res {
                        tracing::warn!("update_current_power_level failed: {e:?}");
                    }

                    if required_level == 0 {
                        stats_publisher.update(|stats_opt: &mut Option<fsuspend::SuspendStats>| {
                            let stats = stats_opt.as_mut().expect("stats is uninitialized");
                            // TODO(mbrunson): Trigger suspend and check return value.
                            stats.success_count = stats.success_count.map(|c| c + 1);
                            stats.last_time_in_suspend = Some(0);
                            tracing::debug!("suspend stats: {stats:?}");
                            true
                        });
                    }
                    last_required_level = required_level;
                }
                e => {
                    tracing::warn!("watch_required_level for execution state failed: {e:?}")
                }
            }
        }
    }

    async fn run_fidl_server(self: Rc<Self>) -> Result<()> {
        let mut service_fs = ServiceFs::new_local();

        service_fs
            .dir("svc")
            .add_fidl_service(IncomingRequest::ActivityGovernor)
            .add_fidl_service(IncomingRequest::Stats);
        service_fs
            .take_and_serve_directory_handle()
            .context("failed to serve outgoing namespace")?;

        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let sag = self.clone();
                async move {
                    match request {
                        IncomingRequest::ActivityGovernor(stream) => {
                            fasync::Task::local(sag.handle_activity_governor_request(stream))
                                .detach()
                        }
                        IncomingRequest::Stats(stream) => {
                            fasync::Task::local(sag.handle_stats_request(stream)).detach()
                        }
                    }
                }
            })
            .await;
        Ok(())
    }

    async fn handle_activity_governor_request(
        self: Rc<Self>,
        mut stream: fsystem::ActivityGovernorRequestStream,
    ) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsystem::ActivityGovernorRequest::GetPowerElements { responder } => {
                    let _ = responder.send(fsystem::PowerElements {
                        execution_state: Some(fsystem::ExecutionState {
                            token: Some(self.execution_state.active_dependency_token()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                }
                fsystem::ActivityGovernorRequest::RegisterListener { responder, .. } => {
                    // TODO(mbrunson): Implement RegisterListener.
                    let _ = responder.send();
                }
                fsystem::ActivityGovernorRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!("Unknown ActivityGovernorRequest ordinal: {ordinal}");
                }
            }
        }
    }

    async fn handle_stats_request(self: Rc<Self>, mut stream: fsuspend::StatsRequestStream) {
        let sub = self.stats_hanging_get.borrow_mut().new_subscriber();

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsuspend::StatsRequest::Watch { responder } => {
                    if let Err(e) = sub.register(responder) {
                        tracing::warn!("Failed to register for Watch call: {e:?}");
                    }
                }
                fsuspend::StatsRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!("Unknown StatsRequest ordinal: {ordinal}");
                }
            }
        }
    }
}
