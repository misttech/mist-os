// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{self, Context, Error};
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_power_suspend as fsuspend;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use futures::prelude::*;
use std::{cell::RefCell, rc::Rc};

type NotifyFn = Box<dyn Fn(&fsuspend::SuspendStats, fsuspend::StatsWatchResponder) -> bool>;
type StatsHangingGet = HangingGet<fsuspend::SuspendStats, fsuspend::StatsWatchResponder, NotifyFn>;

enum IncomingRequest {
    Stats(fsuspend::StatsRequestStream),
}

/// SystemActivityGovernor runs the server for fuchsia.power.suspend FIDL APIs.
/// This type is not thread-safe.
pub struct SystemActivityGovernor {
    stats_hanging_get: RefCell<StatsHangingGet>,
}

impl SystemActivityGovernor {
    pub fn new() -> Self {
        let initial_stats = fsuspend::SuspendStats {
            success_count: Some(0),
            fail_count: Some(0),
            ..Default::default()
        };

        Self {
            stats_hanging_get: RefCell::new(HangingGet::new(
                initial_stats,
                Box::new(
                    |stats: &fsuspend::SuspendStats, res: fsuspend::StatsWatchResponder| -> bool {
                        if let Err(e) = res.send(stats) {
                            tracing::error!("Failed to send suspend stats to client: {e:?}");
                        }
                        true
                    },
                ),
            )),
        }
    }

    /// Runs a FIDL server to handle fuchsia.power.suspend API requests.
    pub async fn run(self) -> Result<(), Error> {
        let sag = Rc::new(self);
        // TODO(mbrunson): Register a set of power elements for exposing and
        //   managing the state of the system, e.g. on, suspend-to-idle,
        //   suspend-to-ram, etc.
        sag.run_suspend_fidl_server().await
    }

    async fn run_suspend_fidl_server(self: Rc<Self>) -> Result<(), Error> {
        let mut service_fs = ServiceFs::new_local();

        service_fs.dir("svc").add_fidl_service(IncomingRequest::Stats);
        service_fs
            .take_and_serve_directory_handle()
            .context("failed to serve outgoing namespace")?;

        let sag = self.clone();
        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let sag = sag.clone();
                async move {
                    match request {
                        IncomingRequest::Stats(stream) => {
                            fasync::Task::local(sag.handle_stats_request(stream)).detach()
                        }
                    }
                }
            })
            .await;
        Ok(())
    }

    async fn handle_stats_request(
        self: Rc<SystemActivityGovernor>,
        mut stream: fsuspend::StatsRequestStream,
    ) {
        let sub = self.stats_hanging_get.borrow_mut().new_subscriber();

        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsuspend::StatsRequest::Watch { responder } => {
                    if let Err(e) = sub.register(responder) {
                        tracing::error!("Failed to register for Watch call: {e:?}");
                    }
                }
                fsuspend::StatsRequest::_UnknownMethod { ordinal, .. } => {
                    tracing::warn!("Unknown StatsRequest ordinal: {ordinal}");
                }
            }
        }
    }
}
