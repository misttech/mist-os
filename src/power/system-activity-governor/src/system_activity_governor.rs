// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_power_broker as fbroker;
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system::{
    self as fsystem, APPLICATION_ACTIVITY_ACTIVE, APPLICATION_ACTIVITY_INACTIVE,
    EXECUTION_STATE_ACTIVE, EXECUTION_STATE_INACTIVE, EXECUTION_STATE_WAKE_HANDLING,
};
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

/// SystemActivityGovernor runs the server for fuchsia.power.suspend FIDL APIs.
pub struct SystemActivityGovernor {
    execution_state: PowerElementContext,
    application_activity: PowerElementContext,
    stats_hanging_get: RefCell<StatsHangingGet>,
}

impl SystemActivityGovernor {
    pub async fn new(topology: &fbroker::TopologyProxy) -> Result<Rc<Self>> {
        let initial_stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };

        let execution_state = PowerElementContext::builder(
            topology,
            "execution_state",
            &[EXECUTION_STATE_INACTIVE, EXECUTION_STATE_WAKE_HANDLING, EXECUTION_STATE_ACTIVE],
        )
        .build()
        .await?;

        let application_activity = PowerElementContext::builder(
            topology,
            "application_activity",
            &[APPLICATION_ACTIVITY_INACTIVE, APPLICATION_ACTIVITY_ACTIVE],
        )
        .dependencies(vec![fbroker::LevelDependency {
            dependency_type: fbroker::DependencyType::Active,
            dependent_level: APPLICATION_ACTIVITY_ACTIVE,
            requires_token: execution_state.active_dependency_token(),
            requires_level: EXECUTION_STATE_ACTIVE,
        }])
        .build()
        .await?;

        Ok(Rc::new(Self {
            execution_state,
            application_activity,
            stats_hanging_get: RefCell::new(HangingGet::new(
                initial_stats,
                Box::new(
                    |stats: &fsuspend::SuspendStats, res: fsuspend::StatsWatchResponder| -> bool {
                        if let Err(error) = res.send(stats) {
                            tracing::warn!(?error, "Failed to send suspend stats to client");
                        }
                        true
                    },
                ),
            )),
        }))
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend and fuchsia.power.system API requests.
    pub async fn run(self: Rc<Self>) -> Result<()> {
        self.run_execution_state();
        self.run_application_activity();
        self.run_fidl_server().await
    }

    fn run_execution_state(self: &Rc<Self>) {
        let stats_publisher = self.stats_hanging_get.borrow().new_publisher();
        let this = self.clone();

        fasync::Task::local(async move {
            Self::run_power_element(
                &this.execution_state,
                EXECUTION_STATE_INACTIVE,
                move |required_level| {
                    if required_level == EXECUTION_STATE_INACTIVE {
                        stats_publisher.update(|stats_opt: &mut Option<fsuspend::SuspendStats>| {
                            let stats = stats_opt.as_mut().expect("stats is uninitialized");
                            // TODO(mbrunson): Trigger suspend and check return value.
                            stats.success_count = stats.success_count.map(|c| c + 1);
                            stats.last_time_in_suspend = Some(0);
                            tracing::debug!(?stats, "suspend stats");
                            true
                        });
                    }
                },
            )
            .await;
        })
        .detach();
    }

    fn run_application_activity(self: &Rc<Self>) {
        let this = self.clone();

        fasync::Task::local(async move {
            Self::run_power_element(
                &this.application_activity,
                APPLICATION_ACTIVITY_INACTIVE,
                |_| {},
            )
            .await;
        })
        .detach();
    }

    async fn run_power_element<'a>(
        power_element: &'a PowerElementContext,
        initial_level: fbroker::PowerLevel,
        update_fn: impl Fn(fbroker::PowerLevel),
    ) {
        let element_name = power_element.name();
        let mut last_required_level = initial_level;

        loop {
            tracing::debug!(
                ?element_name,
                ?last_required_level,
                "run_power_element: waiting for new level"
            );
            match power_element.level_control.watch_required_level(last_required_level).await {
                Ok(Ok(required_level)) => {
                    tracing::debug!(
                        ?element_name,
                        ?required_level,
                        ?last_required_level,
                        "run_power_element: new level requested"
                    );

                    let res = power_element
                        .level_control
                        .update_current_power_level(required_level)
                        .await;
                    if let Err(error) = res {
                        tracing::warn!(
                            ?element_name,
                            ?error,
                            "run_power_element: update_current_power_level failed"
                        );
                    }

                    update_fn(required_level);
                    last_required_level = required_level;
                }
                error => {
                    tracing::warn!(
                        ?element_name,
                        ?error,
                        "run_power_element: watch_required_level failed"
                    )
                }
            }
        }
    }

    async fn run_fidl_server(self: &Rc<Self>) -> Result<()> {
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
                            passive_dependency_token: Some(
                                self.execution_state.passive_dependency_token(),
                            ),
                            ..Default::default()
                        }),
                        application_activity: Some(fsystem::ApplicationActivity {
                            passive_dependency_token: Some(
                                self.application_activity.passive_dependency_token(),
                            ),
                            active_dependency_token: Some(
                                self.application_activity.active_dependency_token(),
                            ),
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
                    tracing::warn!(?ordinal, "Unknown ActivityGovernorRequest method");
                }
            }
        }
    }

    async fn handle_stats_request(self: Rc<Self>, mut stream: fsuspend::StatsRequestStream) {
        let sub = self.stats_hanging_get.borrow_mut().new_subscriber();

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsuspend::StatsRequest::Watch { responder } => {
                    if let Err(error) = sub.register(responder) {
                        tracing::warn!(?error, "Failed to register for Watch call");
                    }
                }
                fsuspend::StatsRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!(?ordinal, "Unknown StatsRequest method");
                }
            }
        }
    }
}
