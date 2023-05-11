// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log::{OnInterestChanged, TestRecord};
use fidl_fuchsia_diagnostics::Severity;
use fidl_fuchsia_validate_logs::{
    LogSinkPuppetRequest, LogSinkPuppetRequestStream, PuppetInfo, RecordSpec,
};
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use fuchsia_runtime as rt;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::prelude::*;
use tracing::*;

#[fuchsia::main]
async fn main() {
    tracing::info!("Puppet started.");
    tracing::dispatcher::get_default(|dispatcher| {
        let publisher: &diagnostics_log::Publisher = dispatcher.downcast_ref().unwrap();
        publisher.set_interest_listener(Listener::new());
    });

    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(|r: LogSinkPuppetRequestStream| r);
    fs.take_and_serve_directory_handle().unwrap();

    while let Some(incoming) = fs.next().await {
        Task::spawn(run_puppet(incoming)).detach();
    }
}

struct Listener {}

impl OnInterestChanged for Listener {
    fn on_changed(&self, severity: &Severity) {
        match severity {
            Severity::Trace => {
                trace!("Changed severity");
            }
            Severity::Debug => {
                debug!("Changed severity");
            }
            Severity::Info => {
                info!("Changed severity");
            }
            Severity::Warn => {
                warn!("Changed severity");
            }
            Severity::Error => {
                error!("Changed severity");
            }
            Severity::Fatal => {
                panic!("Changed severity");
            }
        }
    }
}

impl Listener {
    pub fn new() -> Listener {
        return Self {};
    }
}

async fn run_puppet(mut requests: LogSinkPuppetRequestStream) {
    while let Some(next) = requests.try_next().await.unwrap() {
        match next {
            LogSinkPuppetRequest::StopInterestListener { responder } => {
                // TODO (https://fxbug.dev/77781): Rust should support StopInterestListener.
                responder.send().unwrap();
            }
            LogSinkPuppetRequest::GetInfo { responder } => {
                let info = PuppetInfo {
                    tag: None,
                    pid: rt::process_self().get_koid().unwrap().raw_koid(),
                    tid: rt::thread_self().get_koid().unwrap().raw_koid(),
                };
                responder.send(&info).unwrap();
            }
            LogSinkPuppetRequest::EmitLog {
                responder,
                spec: RecordSpec { file, line, mut record },
            } => {
                // tracing 0.2 will let us to emit non-'static events directly, no downcasting
                tracing::dispatcher::get_default(|dispatcher| {
                    let publisher: &diagnostics_log::Publisher = dispatcher.downcast_ref().unwrap();
                    if record.timestamp == 0 {
                        record.timestamp = zx::Time::get_monotonic().into_nanos();
                    }
                    let test_record = TestRecord::from(&file, line, &record);
                    publisher.event_for_testing(test_record);
                });
                responder.send().unwrap();
            }
            LogSinkPuppetRequest::EmitPrintfLog { spec: _, responder } => {
                responder.send().unwrap();
            }
        }
    }
}
