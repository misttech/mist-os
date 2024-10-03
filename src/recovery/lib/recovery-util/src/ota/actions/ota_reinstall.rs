// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ota::controller::SendEvent;
use crate::ota::state_machine::Event;
use fuchsia_async as fasync;
use fuchsia_async::Task;
use ota_lib::{OtaManager, OtaStatus};
use recovery_metrics_registry::cobalt_registry as metrics;
use std::sync::Arc;

/// Starts an OTA reinstall
pub struct OtaReinstallAction {}

impl OtaReinstallAction {
    /// Runs the OTA reinstall process in a background Task.
    ///
    /// The device is expected to have a network connection. If there is no network connection, the
    /// OTA will fail and the Task will send Event::Error.
    ///
    /// The caller is expected to make calls to OtaManager to complete the OTA process. Afterwards,
    /// the OTA task created by this method will send either Event::Progress(100) or Event:Error.
    pub fn run(event_sender: Box<dyn SendEvent>, ota_manager: Arc<dyn OtaManager>) {
        // This is split into two parts for testing, allowing a factory to be injected and to
        // allow the test to await completion of the task.
        Task::local(run_ota(event_sender, ota_manager)).detach();
    }
}

async fn run_ota(mut event_sender: Box<dyn SendEvent>, ota_manager: Arc<dyn OtaManager>) {
    event_sender.send(Event::Reinstall); // Might need to document this or fix naming to be explicit
    event_sender.send_recovery_stage_event(metrics::RecoveryEventMetricDimensionResult::OtaStarted);
    let start_time = fasync::MonotonicInstant::now();
    let res = ota_manager.start_and_wait_for_result().await;
    let end_time = fasync::MonotonicInstant::now();
    let elapsed_time = (end_time - start_time).into_seconds();

    let event = match res {
        Ok(_) => {
            println!("OtaReinstallAction: OTA Success!");
            event_sender
                .send_recovery_stage_event(metrics::RecoveryEventMetricDimensionResult::OtaSuccess);
            event_sender.send_ota_duration(elapsed_time);
            // TODO(b/253084947) start_and_wait_for_result should return actual OTA result
            // Currently returns OK(()) or Err(..)
            Event::OtaStatusReceived(OtaStatus::Succeeded)
        }
        Err(e) => {
            println!("OTA Error..... {:?}", e);
            event_sender
                .send_recovery_stage_event(metrics::RecoveryEventMetricDimensionResult::OtaFailed);
            Event::Error(e.to_string())
        }
    };

    event_sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::OtaReinstallAction;
    use crate::ota::controller::{MockSendEvent, SendEvent};
    use crate::ota::state_machine::{Event, OtaStatus};
    use anyhow::{format_err, Error};
    use async_trait::async_trait;
    use futures::channel::mpsc;
    use futures::StreamExt;
    use mockall::predicate::eq;
    use ota_lib::OtaManager;
    use recovery_metrics_registry::cobalt_registry as metrics;
    use std::sync::Arc;

    struct FakeOtaManager {
        ota_fails: bool,
    }

    impl FakeOtaManager {
        pub fn new() -> Self {
            Self { ota_fails: false }
        }
        pub fn new_failing() -> Self {
            Self { ota_fails: true }
        }
    }

    #[async_trait]
    impl OtaManager for FakeOtaManager {
        async fn start_and_wait_for_result(&self) -> Result<(), Error> {
            if self.ota_fails {
                Err(format_err!("ota failed"))
            } else {
                Ok(())
            }
        }
        async fn stop(&self) -> Result<(), Error> {
            Ok(())
        }
        async fn complete_ota(&self, _status: OtaStatus) {}
    }

    #[fuchsia::test]
    async fn ota_reinstall_sends_status_event_on_successful_ota() {
        let (mut tx, mut on_handle_event) = mpsc::channel::<Event>(10);
        let mut event_sender = MockSendEvent::new();
        event_sender
            .expect_send_recovery_stage_event()
            .with(eq(metrics::RecoveryEventMetricDimensionResult::OtaStarted))
            .times(1)
            .return_const(());
        event_sender
            .expect_send_recovery_stage_event()
            .with(eq(metrics::RecoveryEventMetricDimensionResult::OtaSuccess))
            .times(1)
            .return_const(());
        event_sender.expect_send_ota_duration().times(1).return_const(());
        event_sender.expect_send().times(2).returning(move |event| {
            tx.try_send(event).unwrap();
        });
        // event_sender.expect_send().with(eq(Event::OtaStatusReceived(OtaStatus::Succeeded))).once().return_once(move |event| {
        //     tx.send(event).unwrap();
        // });
        let event_sender: Box<dyn SendEvent> = Box::new(event_sender);

        let ota_manager = Arc::new(FakeOtaManager::new());
        OtaReinstallAction::run(event_sender, ota_manager);
        println!("checking for reinstall event");
        assert_eq!(Event::Reinstall, on_handle_event.next().await.unwrap());
        println!("checking for status event");
        assert_eq!(
            Event::OtaStatusReceived(OtaStatus::Succeeded),
            on_handle_event.next().await.unwrap()
        );
        println!("Done Done Done");
    }

    #[fuchsia::test]
    async fn ota_reinstall_sends_error_event_on_failed_ota() {
        let (mut tx, mut on_handle_event) = mpsc::channel::<Event>(10);
        let mut event_sender = MockSendEvent::new();
        event_sender
            .expect_send_recovery_stage_event()
            .with(eq(metrics::RecoveryEventMetricDimensionResult::OtaStarted))
            .times(1)
            .return_const(());
        event_sender
            .expect_send_recovery_stage_event()
            .with(eq(metrics::RecoveryEventMetricDimensionResult::OtaFailed))
            .times(1)
            .return_const(());
        event_sender.expect_send().times(2).returning(move |event| {
            println!("Sending something {:?}", &event);
            tx.try_send(event).unwrap();
        });
        let event_sender: Box<dyn SendEvent> = Box::new(event_sender);

        let ota_manager = Arc::new(FakeOtaManager::new_failing());
        OtaReinstallAction::run(event_sender, ota_manager);
        assert_eq!(Event::Reinstall, on_handle_event.next().await.unwrap());
        assert_eq!(Event::Error("ota failed".to_string()), on_handle_event.next().await.unwrap());
    }
}
