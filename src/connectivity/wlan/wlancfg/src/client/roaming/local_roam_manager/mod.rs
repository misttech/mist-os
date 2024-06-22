// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::connection_selection::ConnectionSelectionRequester;
use crate::client::roaming::lib::*;
use crate::client::roaming::roam_monitor;
use crate::client::types;
use crate::telemetry::TelemetrySender;
use anyhow::Error;
use futures::channel::mpsc;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{select, StreamExt};
use std::convert::Infallible;
use tracing::{debug, error};

// Create a roam monitor implementation.
fn create_roam_monitor(
    currently_fulfilled_connection: types::ConnectSelection,
    signal: types::Signal,
    telemetry_sender: TelemetrySender,
) -> Box<dyn roam_monitor::RoamMonitorApi> {
    Box::new(roam_monitor::stationary_monitor::StationaryMonitor::new(
        currently_fulfilled_connection,
        signal,
        telemetry_sender.clone(),
    ))
}

// Requests that can be made to roam manager service loop.
#[cfg_attr(test, derive(Debug))]
pub enum RoamServiceRequest {
    InitializeRoamMonitor {
        currently_fulfilled_connection: types::ConnectSelection,
        signal: types::Signal,
        roam_sender: mpsc::Sender<types::ScannedCandidate>,
        roam_trigger_data_receiver: mpsc::Receiver<RoamTriggerData>,
    },
}

// Allows callers to make roam service requests.
#[derive(Clone)]
pub struct RoamManager {
    sender: mpsc::Sender<RoamServiceRequest>,
}

impl RoamManager {
    pub fn new(sender: mpsc::Sender<RoamServiceRequest>) -> Self {
        Self { sender }
    }
    // Create a roam monitor and return handles for passing trigger data and roam requests .
    pub fn initialize_roam_monitor(
        &mut self,
        currently_fulfilled_connection: types::ConnectSelection,
        signal: types::Signal,
    ) -> (roam_monitor::RoamDataSender, mpsc::Receiver<types::ScannedCandidate>) {
        let (roam_sender, roam_receiver) = mpsc::channel(ROAMING_CHANNEL_BUFFER_SIZE);
        let (roam_trigger_data_sender, roam_trigger_data_receiver) =
            mpsc::channel(ROAMING_CHANNEL_BUFFER_SIZE);
        let _ = self
            .sender
            .try_send(RoamServiceRequest::InitializeRoamMonitor {
                currently_fulfilled_connection,
                signal,
                roam_sender,
                roam_trigger_data_receiver,
            })
            .inspect_err(|e| {
                error!("Failed to request roam monitoring: {}. Proceeding without roaming.", e)
            });
        (roam_monitor::RoamDataSender::new(roam_trigger_data_sender), roam_receiver)
    }
}

/// Service loop for handling roam manager requests.
pub async fn serve_local_roam_manager_requests(
    mut roam_service_request_receiver: mpsc::Receiver<RoamServiceRequest>,
    connection_selection_requester: ConnectionSelectionRequester,
    telemetry_sender: TelemetrySender,
) -> Result<Infallible, Error> {
    // Queue of created monitor futures.
    let mut monitor_futs: FuturesUnordered<BoxFuture<'static, Result<(), anyhow::Error>>> =
        FuturesUnordered::new();

    loop {
        select! {
            // Handle requests sent to roam manager service loop.
            req = roam_service_request_receiver.select_next_some() => {
                match req {
                    // Create and start a roam monitor future, passing in handles from caller. This
                    // ensures that new data is initialized for every new called (e.g. connected_state).
                    RoamServiceRequest::InitializeRoamMonitor { currently_fulfilled_connection, signal, roam_sender, roam_trigger_data_receiver }=> {
                        let monitor = create_roam_monitor(currently_fulfilled_connection, signal, telemetry_sender.clone());
                        let monitor_fut = roam_monitor::serve_roam_monitor(monitor, roam_trigger_data_receiver, connection_selection_requester.clone(), roam_sender.clone(), telemetry_sender.clone());
                        monitor_futs.push(Box::pin(monitor_fut));
                    }
                }
            }
            // Serves roam monitors. Futures return when when caller drops handle.
            _terminated_monitor = monitor_futs.select_next_some() => {
                debug!("Roam monitor future closed.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::TelemetryEvent;
    use crate::util::testing::{
        generate_connect_selection, generate_random_roaming_connection_data, generate_random_signal,
    };
    use fidl_fuchsia_wlan_internal::SignalReportIndication;
    use fuchsia_async::TestExecutor;
    use futures::task::Poll;
    use std::pin::pin;
    use wlan_common::assert_variant;

    struct RoamManagerServiceTestValues {
        telemetry_sender: TelemetrySender,
        connection_selection_requester: ConnectionSelectionRequester,
        roam_manager: RoamManager,
        roam_service_request_receiver: mpsc::Receiver<RoamServiceRequest>,
    }

    fn setup_test() -> RoamManagerServiceTestValues {
        let (telemetry_sender, _) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let (connection_selection_request_sender, _) = mpsc::channel(5);
        let connection_selection_requester =
            ConnectionSelectionRequester::new(connection_selection_request_sender);
        let (roam_service_request_sender, roam_service_request_receiver) = mpsc::channel(100);
        let roam_manager = RoamManager::new(roam_service_request_sender);
        RoamManagerServiceTestValues {
            telemetry_sender,
            connection_selection_requester,
            roam_manager,
            roam_service_request_receiver,
        }
    }

    #[fuchsia::test()]
    fn test_create_roam_monitor() {
        let _exec = TestExecutor::new();
        let (telemetry_sender, _) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let monitor = create_roam_monitor(
            generate_connect_selection(),
            generate_random_signal(),
            telemetry_sender.clone(),
        );
        let stationary_monitor: Box<dyn roam_monitor::RoamMonitorApi> =
            Box::new(roam_monitor::stationary_monitor::StationaryMonitor::new(
                generate_connect_selection(),
                generate_random_signal(),
                telemetry_sender.clone(),
            ));

        assert_eq!(stationary_monitor.type_id(), monitor.type_id());
    }

    #[fuchsia::test]
    fn test_roam_manager_handles_get_roam_monitor_request() {
        let mut exec = TestExecutor::new();
        let mut test_values = setup_test();
        let mut serve_fut = pin!(serve_local_roam_manager_requests(
            test_values.roam_service_request_receiver,
            test_values.connection_selection_requester.clone(),
            test_values.telemetry_sender.clone(),
        ));
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send a request to initialize a roam monitor
        let connection_data = generate_random_roaming_connection_data();
        let (mut roam_monitor_sender, _) = test_values.roam_manager.initialize_roam_monitor(
            connection_data.currently_fulfilled_connection.clone(),
            types::Signal {
                rssi_dbm: connection_data.signal_data.ewma_rssi.get() as i8,
                snr_db: connection_data.signal_data.ewma_snr.get() as i8,
            },
        );

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send some data via one of the roam monitor. Ensure that it doesn't error out, which means
        // a created roam monitor is holding the receiver end.
        roam_monitor_sender
            .send_signal_report_ind(SignalReportIndication { rssi_dbm: -60, snr_db: 30 })
            .expect("error sending data via roam monitor sender");
    }

    #[fuchsia::test]
    fn test_roam_manager_handles_terminated_roam_monitors() {
        let mut exec = TestExecutor::new();
        let mut test_values = setup_test();
        let mut serve_fut = pin!(serve_local_roam_manager_requests(
            test_values.roam_service_request_receiver,
            test_values.connection_selection_requester.clone(),
            test_values.telemetry_sender.clone(),
        ));
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send a request to initialize a roam monitor
        let connection_data = generate_random_roaming_connection_data();
        let (mut roam_monitor_sender, _) = test_values.roam_manager.initialize_roam_monitor(
            connection_data.currently_fulfilled_connection.clone(),
            types::Signal {
                rssi_dbm: connection_data.signal_data.ewma_rssi.get() as i8,
                snr_db: connection_data.signal_data.ewma_snr.get() as i8,
            },
        );

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send some trigger data over a channel. Ensure that it doesn't error out, which means
        // a created roam monitor is holding the receiver end.
        roam_monitor_sender
            .send_signal_report_ind(SignalReportIndication { rssi_dbm: -60, snr_db: 30 })
            .expect("error sending data via roam monitor sender");

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Drop the trigger data sender. This should cause the monitor to terminate?
        std::mem::drop(roam_monitor_sender);

        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
    }
}
