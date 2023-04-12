// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{DoneSignaler, TestEvent, TestEventSender},
    anyhow::{bail, Context, Error},
    async_trait::async_trait,
    fcrash::FileReportResults,
    fidl_fuchsia_feedback as fcrash,
    fidl_server::*,
    std::sync::{Arc, RwLock},
    tracing::*,
};

const REPORT_PROGRAM_NAME: &str = "triage_detect";

/// FakeCrashReporter can be injected to capture Detect's crash report requests.
#[derive(Clone)]
pub struct FakeCrashReporter {
    event_sender: Arc<RwLock<TestEventSender>>,
    done_signaler: DoneSignaler,
}

fn evaluate_report(report: &fcrash::CrashReport) -> Result<String, Error> {
    let fcrash::CrashReport { program_name, crash_signature, is_fatal, .. } = report;
    if program_name != &Some(REPORT_PROGRAM_NAME.to_string()) {
        bail!(
            "Crash report program name should be {} but it was {:?}",
            REPORT_PROGRAM_NAME,
            program_name
        );
    }
    if is_fatal != &Some(false) {
        bail!("Crash report should not be fatal, but it was {:?}", is_fatal);
    }
    match crash_signature {
        Some(signature) => return Ok(signature.to_string()),
        None => bail!("Crash report crash signature was None"),
    }
}

impl FakeCrashReporter {
    pub fn new(event_sender: TestEventSender, done_signaler: DoneSignaler) -> Self {
        let event_sender = Arc::new(RwLock::new(event_sender));
        Self { event_sender, done_signaler }
    }

    async fn send_test_event(&self, event: Result<TestEvent, Error>) -> Result<(), Error> {
        self.event_sender
            .write()
            .expect("failed to acquire lock on event sender")
            .unbounded_send(event)?;
        Ok(())
    }
}

#[async_trait]
impl AsyncRequestHandler<fcrash::CrashReporterMarker> for FakeCrashReporter {
    async fn handle_request(&self, request: fcrash::CrashReporterRequest) -> Result<(), Error> {
        match request {
            fcrash::CrashReporterRequest::File { report, responder } => {
                match evaluate_report(&report) {
                    Ok(signature) => {
                        info!("Received crash report: {}", signature);
                        self.send_test_event(Ok(TestEvent::CrashReport(signature))).await?;
                        responder.send(&mut Ok(())).context("failed to send response to client")?;
                    }
                    Err(problem) => {
                        error!("Problem in report: {}", problem);
                        self.send_test_event(Err(problem)).await?;
                        self.done_signaler.signal_done().await;
                    }
                }
            }
            fcrash::CrashReporterRequest::FileReport { report, responder } => {
                match evaluate_report(&report) {
                    Ok(signature) => {
                        info!("Received crash report: {}", signature);
                        self.send_test_event(Ok(TestEvent::CrashReport(signature))).await?;
                        responder
                            .send(&mut Ok(FileReportResults::EMPTY))
                            .context("failed to send response to client")?;
                    }
                    Err(problem) => {
                        error!("Problem in report: {}", problem);
                        self.send_test_event(Err(problem)).await?;
                        self.done_signaler.signal_done().await;
                    }
                }
            }
        }
        Ok(())
    }
}
