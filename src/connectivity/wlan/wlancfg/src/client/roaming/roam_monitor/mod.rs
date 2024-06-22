// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::connection_selection::ConnectionSelectionRequester;
use crate::client::roaming::lib::{RoamTriggerData, RoamingConnectionData};
use crate::client::types;
use crate::telemetry::{TelemetryEvent, TelemetrySender};
use anyhow::{format_err, Error};
use fidl_fuchsia_wlan_internal as fidl_internal;
use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::{select, FutureExt};
use std::any::Any;
use tracing::{error, info, warn};

pub mod stationary_monitor;

// Struct to expose methods for state machine to send roam data, regardless of roam profile.
pub struct RoamDataSender {
    sender: mpsc::Sender<RoamTriggerData>,
}
impl RoamDataSender {
    pub fn new(trigger_data_sender: mpsc::Sender<RoamTriggerData>) -> Self {
        Self { sender: trigger_data_sender }
    }
    pub fn send_signal_report_ind(
        &mut self,
        ind: fidl_internal::SignalReportIndication,
    ) -> Result<(), anyhow::Error> {
        Ok(self.sender.try_send(RoamTriggerData::SignalReportInd(ind))?)
    }
}

/// Trait for creating different roam monitors based on roaming profiles.
pub trait RoamMonitorApi: Send + Sync + Any {
    // Handles trigger data and evaluates current state. Returns true if roam
    // search is warranted. All roam monitors MUST handle all trigger data types, even if they take
    // no action.
    fn should_roam_search(&mut self, data: RoamTriggerData) -> Result<bool, anyhow::Error>;
    // Determines if the selected roam candidate is still relevant and provides enough potential
    // improvement to warrant a roam. Returns true if roam request should be sent to state machine.
    fn should_send_roam_request(
        &self,
        candidate: types::ScannedCandidate,
    ) -> Result<bool, anyhow::Error>;
    // Returns tracked roam data, or error if data is not tracked by roam monitor.
    fn get_roam_data(&self) -> Result<RoamingConnectionData, anyhow::Error>;
}

// Service loop that orchestrates interaction between state machine (incoming roam data and outgoing
// roam requests), roam monitor implementation, and roam manager (roam search requests and results).
pub async fn serve_roam_monitor(
    mut roam_monitor: Box<dyn RoamMonitorApi>,
    mut trigger_data_receiver: mpsc::Receiver<RoamTriggerData>,
    connection_selection_requester: ConnectionSelectionRequester,
    mut roam_sender: mpsc::Sender<types::ScannedCandidate>,
    telemetry_sender: TelemetrySender,
) -> Result<(), anyhow::Error> {
    // Queue of initialized roam searches.
    let mut roam_search_result_futs: FuturesUnordered<
        BoxFuture<'static, Result<types::ScannedCandidate, Error>>,
    > = FuturesUnordered::new();

    loop {
        select! {
            // Handle incoming trigger data, queueing a roam search request if necessary.
            trigger_data = trigger_data_receiver.next() => match trigger_data {
                Some(data) => {
                    if roam_monitor.should_roam_search(data).unwrap_or_else(|e| {
                        error!("error handling roam trigger data: {}", e);
                        false
                    }) {
                        if let Ok(roam_data) = roam_monitor.get_roam_data() {
                            telemetry_sender.send(TelemetryEvent::RoamingScan);
                            info!("Performing scan to find proactive local roaming candidates.");
                            let roam_search_fut = get_roaming_connection_selection_future(
                                connection_selection_requester.clone(),
                                roam_data
                            );
                            roam_search_result_futs.push(roam_search_fut.boxed());
                        } else {
                            error!("Unexpected error getting roam data");
                        }
                    }
                }
                _ => {}
            },
            // Handle the result of a completed roam search, sending recommentation to roam if
            // necessary.
            roam_search_result = roam_search_result_futs.select_next_some() => match roam_search_result {
                Ok(candidate) => {
                    if roam_monitor.should_send_roam_request(candidate.clone()).unwrap_or_else(|e| {
                            error!("Error validating selected roam candidate: {}", e);
                            false
                        }) {
                            info!("Requesting roam to candidate: {:?}", candidate.to_string_without_pii());
                            if roam_sender.try_send(candidate).is_err() {
                                warn!("Failed to send roam request, exiting monitor service loop.");
                                break
                            }

                    }
                }
                Err(e) => {
                    error!("Error occured during roam search: {:?}", e);
                }
            },
            complete => {
                warn!("Roam monitor channels dropped, exiting monitor service loop.");
                break
            }
        }
    }
    Ok(())
}

// Request a roam selection from the connection selection module, bundle the receiver into a future
// to be queued and that can also return the initiating request.
async fn get_roaming_connection_selection_future(
    mut connection_selection_requester: ConnectionSelectionRequester,
    roaming_connection_data: RoamingConnectionData,
) -> Result<types::ScannedCandidate, Error> {
    match connection_selection_requester
        .do_roam_selection(
            roaming_connection_data.currently_fulfilled_connection.target.network.clone(),
            roaming_connection_data.currently_fulfilled_connection.target.credential.clone(),
        )
        .await?
    {
        Some(candidate) => Ok(candidate),
        None => Err(format_err!("No roam candidates found.")),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::connection_selection::ConnectionSelectionRequest;
    use crate::telemetry::TelemetryEvent;
    use crate::util::testing::fakes::FakeRoamMonitor;
    use crate::util::testing::generate_random_scanned_candidate;
    use fidl_fuchsia_wlan_internal as fidl_internal;
    use fuchsia_async::TestExecutor;
    use futures::pin_mut;
    use futures::task::Poll;
    use test_case::test_case;
    use wlan_common::assert_variant;

    struct TestValues {
        trigger_data_sender: mpsc::Sender<RoamTriggerData>,
        trigger_data_receiver: mpsc::Receiver<RoamTriggerData>,
        roam_sender: mpsc::Sender<types::ScannedCandidate>,
        roam_receiver: mpsc::Receiver<types::ScannedCandidate>,
        connection_selection_requester: ConnectionSelectionRequester,
        connection_selection_request_receiver: mpsc::Receiver<ConnectionSelectionRequest>,
        telemetry_sender: TelemetrySender,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
    }

    fn setup_test() -> TestValues {
        let (trigger_data_sender, trigger_data_receiver) = mpsc::channel(100);
        let (roam_sender, roam_receiver) = mpsc::channel(100);
        let (connection_selection_request_sender, connection_selection_request_receiver) =
            mpsc::channel(5);
        let connection_selection_requester =
            ConnectionSelectionRequester::new(connection_selection_request_sender);
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        TestValues {
            trigger_data_sender,
            trigger_data_receiver,
            roam_sender,
            roam_receiver,
            connection_selection_requester,
            connection_selection_request_receiver,
            telemetry_sender,
            telemetry_receiver,
        }
    }

    #[fuchsia::test]
    fn test_roam_data_sender_send_signal_report_ind() {
        let _exec = TestExecutor::new();
        let (sender, mut receiver) = mpsc::channel(100);
        let mut roam_data_sender = RoamDataSender::new(sender);
        let ind = fidl_internal::SignalReportIndication { rssi_dbm: -60, snr_db: 30 };

        roam_data_sender.send_signal_report_ind(ind).expect("error sending signal report");

        // Verify that roam sender packages trigger data and sends to roam monitor receiver.
        assert_variant!(receiver.try_next(), Ok(Some(RoamTriggerData::SignalReportInd(data))) => {
            assert_eq!(ind, data);
        });
    }

    #[test_case(false; "should not queue roam search")]
    #[test_case(true; "should queue roam search")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_serve_loop_handles_trigger_data(response_to_should_roam_scan: bool) {
        let mut exec = TestExecutor::new();
        let mut test_values = setup_test();

        // Create a fake roam monitor. Set the should_roam_scan response, so we can verify that the
        // serve loop forwarded the data and that the correct action was taken.
        let mut roam_monitor = FakeRoamMonitor::new();
        roam_monitor.response_to_should_roam_scan = response_to_should_roam_scan;

        // Start a serve loop with the fake roam monitor
        let serve_fut = serve_roam_monitor(
            Box::new(roam_monitor),
            test_values.trigger_data_receiver,
            test_values.connection_selection_requester,
            // test_values.roam_search_sender,
            test_values.roam_sender,
            test_values.telemetry_sender,
        );
        pin_mut!(serve_fut);

        // Run loop forward
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send some trigger data to kick off the handling sequence. The actual values here are
        // irrelevant.
        test_values
            .trigger_data_sender
            .try_send(RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: -40,
                snr_db: 40,
            }))
            .expect("failed to send");

        // Run loop forward.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        if response_to_should_roam_scan {
            // Verify metric was sent for upcoming roam scan
            assert_variant!(
                test_values.telemetry_receiver.try_next(),
                Ok(Some(TelemetryEvent::RoamingScan))
            );
            // Verify that a roam search request was sent after monitor responded true.
            assert_variant!(
                test_values.connection_selection_request_receiver.try_next(),
                Ok(Some(_))
            );
        } else {
            // Verify that no roam search was triggered after monitor responded false.
            assert_variant!(test_values.connection_selection_request_receiver.try_next(), Err(_));
        }
    }

    #[test_case(false; "should not send roam request")]
    #[test_case(true; "should send roam request")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_serve_loop_handles_roam_search_results(response_to_should_send_roam_request: bool) {
        let mut exec = TestExecutor::new();
        let mut test_values = setup_test();

        // Create a fake roam monitor. Set should_roam_scan to true to ensure roam searches get
        // queued. Conditionally set the should_send_roam_request response.
        let mut roam_monitor = FakeRoamMonitor::new();
        roam_monitor.response_to_should_roam_scan = true;
        roam_monitor.response_to_should_send_roam_request = response_to_should_send_roam_request;

        // Start a serve loop with the fake roam monitor
        let serve_fut = serve_roam_monitor(
            Box::new(roam_monitor),
            test_values.trigger_data_receiver,
            // test_values.roam_search_sender,
            test_values.connection_selection_requester,
            test_values.roam_sender,
            test_values.telemetry_sender,
        );
        pin_mut!(serve_fut);

        // Run loop forward
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send some trigger data to kick off the handling sequence. The actual values here are
        // irrelevant.
        test_values
            .trigger_data_sender
            .try_send(RoamTriggerData::SignalReportInd(fidl_internal::SignalReportIndication {
                rssi_dbm: -40,
                snr_db: 40,
            }))
            .expect("failed to send");

        // Run loop forward
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Respond via the connection selection requester
        let candidate = generate_random_scanned_candidate();
        assert_variant!(test_values.connection_selection_request_receiver.try_next(), Ok(Some(ConnectionSelectionRequest::RoamSelection { responder, .. })) => {
            // Respond with a roam candidate
            responder.send(Some(candidate.clone())).expect("failed to send");
        });

        // Run loop forward
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        if response_to_should_send_roam_request {
            // Verify metric was sent for upcoming roam scan
            assert_variant!(
                test_values.telemetry_receiver.try_next(),
                Ok(Some(TelemetryEvent::RoamingScan))
            );
            // Verify that a roam request is sent if the should_send_roam_request method returns
            // true.
            assert_variant!(test_values.roam_receiver.try_next(), Ok(Some(roam_request)) => {
                assert_eq!(roam_request, candidate);
            });
        } else {
            // Veriify that no roam request is sent if the should_send_roam_request method returns
            // false.
            assert_variant!(test_values.roam_receiver.try_next(), Err(_));
        }
    }
}
