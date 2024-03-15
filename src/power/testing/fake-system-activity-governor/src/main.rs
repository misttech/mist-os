// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{cell::RefCell, rc::Rc};

use anyhow::{Context, Result};
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_power_suspend as fsuspend;
use fidl_fuchsia_power_system as fsystem;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_zircon::Event;
use futures::{StreamExt, TryStreamExt};
use tracing::info;

type NotifyFn = Box<dyn Fn(&fsuspend::SuspendStats, fsuspend::StatsWatchResponder) -> bool>;
type StatsHangingGet = HangingGet<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;

enum IncomingRequest {
    ActivityGovernor(fsystem::ActivityGovernorRequestStream),
    Stats(fsuspend::StatsRequestStream),
}

struct SuspendStatsManager {
    /// The hanging get handler used to notify subscribers of changes to suspend stats.
    hanging_get: RefCell<StatsHangingGet>,
}

impl SuspendStatsManager {
    fn new() -> Self {
        let stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };
        let hanging_get = StatsHangingGet::new(
            stats,
            Box::new(
                |stats: &fsuspend::SuspendStats, res: fsuspend::StatsWatchResponder| -> bool {
                    res.send(stats).expect("Send suspend stats to client");
                    true
                },
            ),
        );
        Self { hanging_get: RefCell::new(hanging_get) }
    }
}

struct FakeSystemActivityGovernor {
    suspend_stats: SuspendStatsManager,
}

impl FakeSystemActivityGovernor {
    fn new() -> Rc<Self> {
        Rc::new(Self { suspend_stats: SuspendStatsManager::new() })
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend and fuchsia.power.system API requests.
    async fn run(self: &Rc<Self>) -> Result<()> {
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
                    responder
                        .send(fsystem::PowerElements {
                            application_activity: Some(fsystem::ApplicationActivity {
                                active_dependency_token: Some(Event::create()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                        .expect("");
                }
                fsystem::ActivityGovernorRequest::RegisterListener { responder, .. } => {
                    responder.send().expect("");
                }
                fsystem::ActivityGovernorRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }

    async fn handle_stats_request(self: Rc<Self>, mut stream: fsuspend::StatsRequestStream) {
        let sub = self.suspend_stats.hanging_get.borrow_mut().new_subscriber();

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsuspend::StatsRequest::Watch { responder } => {
                    sub.register(responder).expect("");
                }
                fsuspend::StatsRequest::_UnknownMethod { .. } => todo!(),
            }
        }
    }
}

#[fuchsia::main]
async fn main() -> Result<()> {
    info!("Start fake sag component");
    // Set up the SystemActivityGovernor.
    let sag = FakeSystemActivityGovernor::new();
    sag.run().await
}
