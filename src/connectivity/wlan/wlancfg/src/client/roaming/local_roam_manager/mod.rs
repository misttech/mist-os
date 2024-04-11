// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{
            connection_selection::{bss_selection, ConnectionSelectionRequester},
            roaming::{lib::*, roam_monitor},
            types,
        },
        telemetry::{TelemetryEvent, TelemetrySender},
    },
    anyhow::{format_err, Error},
    fuchsia_async as fasync,
    futures::{
        channel::mpsc, future::BoxFuture, select, stream::FuturesUnordered, FutureExt, StreamExt,
    },
    tracing::{info, warn},
};

const MIN_RSSI_IMPROVEMENT_TO_ROAM: f64 = 3.0;
const MIN_SNR_IMPROVEMENT_TO_ROAM: f64 = 3.0;

/// Local Roam Manager is implemented as a trait so that it can be stubbed out in unit tests.
pub trait LocalRoamManagerApi: Send + Sync {
    fn get_roam_monitor(
        &mut self,
        quality_data: bss_selection::BssQualityData,
        currently_fulfilled_connection: types::ConnectSelection,
        roam_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Box<dyn roam_monitor::RoamMonitorApi>;
}

/// Holds long lasting channels for metrics and roam scans, and creates roam monitors for each
/// new connection.
pub struct LocalRoamManager {
    /// Channel to send requests for roam searches so LocalRoamManagerService can serve
    /// connection selection scans.
    roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
    telemetry_sender: TelemetrySender,
}

impl LocalRoamManager {
    pub fn new(
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        telemetry_sender: TelemetrySender,
    ) -> Self {
        Self { roam_search_sender, telemetry_sender }
    }
}

impl LocalRoamManagerApi for LocalRoamManager {
    // Create a RoamMonitor object that can request roam scans, initiate roams, and record metrics
    // for a connection.
    fn get_roam_monitor(
        &mut self,
        quality_data: bss_selection::BssQualityData,
        currently_fulfilled_connection: types::ConnectSelection,
        roam_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Box<dyn roam_monitor::RoamMonitorApi> {
        let connection_data = ConnectionData::new(
            currently_fulfilled_connection.clone(),
            quality_data,
            fasync::Time::now(),
        );
        Box::new(roam_monitor::RoamMonitor::new(
            self.roam_search_sender.clone(),
            roam_sender,
            connection_data,
            self.telemetry_sender.clone(),
        ))
    }
}

/// Handles roam futures for scans, since FuturesUnordered are not Send + Sync.
/// State machine's connected state sends updates to RoamMonitors, which may send scan requests
/// to LocalRoamManagerService.
pub struct LocalRoamManagerService {
    roam_futures: FuturesUnordered<
        BoxFuture<'static, Result<(types::ScannedCandidate, RoamSearchRequest), Error>>,
    >,
    connection_selection_requester: ConnectionSelectionRequester,
    // Receive requests for roam searches.
    roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
    telemetry_sender: TelemetrySender,
}

impl LocalRoamManagerService {
    pub fn new(
        roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
        telemetry_sender: TelemetrySender,
        connection_selection_requester: ConnectionSelectionRequester,
    ) -> Self {
        Self {
            roam_futures: FuturesUnordered::new(),
            connection_selection_requester,
            roam_search_receiver,
            telemetry_sender,
        }
    }

    // process any futures that complete for roam scnas.
    pub async fn serve(mut self) {
        // watch futures for roam scans
        loop {
            select! {
                req  = self.roam_search_receiver.select_next_some() => {
                    info!("Performing scan to find proactive local roaming candidates.");
                    let roam_fut = get_roaming_connection_selection_future(
                        self.connection_selection_requester.clone(),
                        req,
                    );
                    self.roam_futures.push(roam_fut.boxed());
                    self.telemetry_sender.send(TelemetryEvent::RoamingScan);
                }
                roam_fut_response = self.roam_futures.select_next_some() => {
                    match roam_fut_response {
                        Ok((candidate, request)) => {
                            if is_roam_worthwhile(&request, &candidate) {
                                info!("Roam would be requested to candidate: {:?}. Roaming is not enabled.", candidate.to_string_without_pii());
                                self.telemetry_sender.send(TelemetryEvent::WouldRoamConnect);
                            }
                        },
                        Err(e) => {
                            warn!("An error occured during the roam scan: {:?}", e);
                        }
                    }
                }
            }
        }
    }
}

async fn get_roaming_connection_selection_future(
    mut connection_selection_requester: ConnectionSelectionRequester,
    request: RoamSearchRequest,
) -> Result<(types::ScannedCandidate, RoamSearchRequest), Error> {
    match connection_selection_requester
        .do_roam_selection(
            request.connection_data.currently_fulfilled_connection.target.network.clone(),
            request.connection_data.currently_fulfilled_connection.target.credential.clone(),
        )
        .await?
    {
        Some(candidate) => Ok((candidate, request)),
        None => Err(format_err!("No roam candidates found.")),
    }
}

/// A roam is worthwhile if the selected BSS looks to be a significant improvement over the current
/// BSS.
fn is_roam_worthwhile(
    request: &RoamSearchRequest,
    roam_candidate: &types::ScannedCandidate,
) -> bool {
    if roam_candidate.is_same_bss_security_and_credential(
        &request.connection_data.currently_fulfilled_connection.target,
    ) {
        info!("Roam search selected currently connected BSS.");
        return false;
    }
    // Candidate RSSI or SNR must be significantly better in order to trigger a roam.
    let current_rssi = request.connection_data.quality_data.signal_data.ewma_rssi.get();
    let current_snr = request.connection_data.quality_data.signal_data.ewma_snr.get();
    info!(
        "Roam candidate BSS - RSSI: {:?}, SNR: {:?}. Current BSS - RSSI: {:?}, SNR: {:?}.",
        roam_candidate.bss.signal.rssi_dbm,
        roam_candidate.bss.signal.snr_db,
        current_rssi,
        current_snr
    );
    if (roam_candidate.bss.signal.rssi_dbm as f64) < current_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM
        && (roam_candidate.bss.signal.snr_db as f64) < current_snr + MIN_SNR_IMPROVEMENT_TO_ROAM
    {
        info!(
            "Selected roam candidate ({:?}) is not enough of an improvement. Ignoring.",
            roam_candidate.to_string_without_pii()
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            client::connection_selection::{
                ConnectionSelectionRequest, EWMA_SMOOTHING_FACTOR, EWMA_VELOCITY_SMOOTHING_FACTOR,
            },
            config_management::network_config::PastConnectionList,
            util::{
                pseudo_energy::SignalData,
                testing::{
                    generate_connect_selection, generate_random_bss,
                    generate_random_bss_quality_data, generate_random_channel,
                    generate_random_scanned_candidate,
                },
            },
        },
        fuchsia_async::TestExecutor,
        futures::task::Poll,
        std::pin::pin,
        wlan_common::assert_variant,
    };

    struct RoamManagerServiceTestValues {
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        /// This is needed for roam search requests; normally it is how the RoamManagerService
        /// would tell the state machine roaming decisions.
        roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
        roam_manager_service: LocalRoamManagerService,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        currently_fulfilled_connection: types::ConnectSelection,
        connection_selection_request_receiver: mpsc::Receiver<ConnectionSelectionRequest>,
    }

    fn roam_service_test_setup() -> RoamManagerServiceTestValues {
        let currently_fulfilled_connection = generate_connect_selection();
        let (roam_search_sender, roam_search_receiver) = mpsc::unbounded();
        let (roam_req_sender, _roam_req_receiver) = mpsc::unbounded();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let (connection_selection_request_sender, connection_selection_request_receiver) =
            mpsc::channel(5);

        let connection_selection_requester =
            ConnectionSelectionRequester::new(connection_selection_request_sender);

        let roam_manager_service = LocalRoamManagerService::new(
            roam_search_receiver,
            telemetry_sender,
            connection_selection_requester,
        );

        RoamManagerServiceTestValues {
            roam_search_sender,
            roam_req_sender,
            roam_manager_service,
            telemetry_receiver,
            currently_fulfilled_connection,
            connection_selection_request_receiver,
        }
    }

    // Test that when LocalRoamManagerService gets a roam scan request, it initiates bss selection.
    #[fuchsia::test]
    fn test_roam_manager_service_initiates_network_selection() {
        let mut exec = TestExecutor::new();
        let mut test_values = roam_service_test_setup();

        let serve_fut = test_values.roam_manager_service.serve();
        let mut serve_fut = pin!(serve_fut);
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        let bss_quality_data = bss_selection::BssQualityData::new(
            SignalData::new(-80, 10, EWMA_SMOOTHING_FACTOR, EWMA_VELOCITY_SMOOTHING_FACTOR),
            generate_random_channel(),
            PastConnectionList::default(),
        );

        let connection_data = ConnectionData::new(
            test_values.currently_fulfilled_connection.clone(),
            bss_quality_data,
            fuchsia_async::Time::now(),
        );

        // Send a request for the LocalRoamManagerService to initiate bss selection.
        let req = RoamSearchRequest::new(connection_data, test_values.roam_req_sender);
        test_values.roam_search_sender.unbounded_send(req).expect("Failed to send roam search req");

        // Progress the RoamManager future to process the request for a roam search.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Verify that a metric is logged for the roam scan
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::RoamingScan))
        );

        // Verify that connection selection receives request
        assert_variant!(
            test_values.connection_selection_request_receiver.try_next(),
            Ok(Some(request)) => {
                assert_variant!(request, ConnectionSelectionRequest::RoamSelection {..});
            }
        );
    }

    // Test that roam manager would send connect request, if it were enabled.
    #[fuchsia::test]
    fn test_roam_manager_service_would_initiate_roam() {
        let mut exec = TestExecutor::new();
        let mut test_values = roam_service_test_setup();

        // Set up fake scan results of a better candidate BSS.
        let init_rssi = -80;
        let init_snr = 10;
        let selected_roam_candidate = types::ScannedCandidate {
            bss: types::Bss {
                signal: types::Signal {
                    rssi_dbm: init_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM as i8 + 1,
                    snr_db: init_snr + MIN_SNR_IMPROVEMENT_TO_ROAM as i8 + 1,
                },
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };

        // Start roam manager service
        let serve_fut = test_values.roam_manager_service.serve();
        let mut serve_fut = pin!(serve_fut);
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Initialize current connection data.
        let signal_data = SignalData::new(
            init_rssi,
            init_snr,
            EWMA_SMOOTHING_FACTOR,
            EWMA_VELOCITY_SMOOTHING_FACTOR,
        );
        let bss_quality_data = bss_selection::BssQualityData::new(
            signal_data,
            generate_random_channel(),
            PastConnectionList::default(),
        );
        let connection_data = ConnectionData::new(
            test_values.currently_fulfilled_connection.clone(),
            bss_quality_data,
            fuchsia_async::Time::now(),
        );

        // Send a roam search request to local roam manager service
        let req = RoamSearchRequest::new(connection_data, test_values.roam_req_sender.clone());
        test_values.roam_search_sender.unbounded_send(req).expect("Failed to send roam search req");

        // Progress the RoamManager future to process the request for a roam search.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Verify that a metric is logged for the roam scan
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::RoamingScan))
        );

        // Verify that connection selection receives request
        assert_variant!(
            test_values.connection_selection_request_receiver.try_next(),
            Ok(Some(request)) => {
                assert_variant!(request, ConnectionSelectionRequest::RoamSelection {network_id, credential, responder} => {
                    assert_eq!(network_id, test_values.currently_fulfilled_connection.target.network);
                    assert_eq!(credential, test_values.currently_fulfilled_connection.target.credential);
                    // Respond with barely better candidate.
                    responder.send(Some(selected_roam_candidate)).expect("failed to send connection selection");
                });
            }
        );

        // Progress the RoamManager future to process the roam search response.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // A metric will be logged when a roaming connect request is sent to state machine
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::WouldRoamConnect))
        );
        // Roaming would be requested via state machine, if it were enabled.
    }

    #[fuchsia::test]
    fn test_roam_manager_service_doesnt_roam_for_barely_better_rssi() {
        let mut exec = TestExecutor::new();
        let mut test_values = roam_service_test_setup();

        // Set up fake roam candidate with barely better rssi, less than the min improvement
        // threshold.
        let init_rssi = -80;
        let init_snr = 10;
        let selected_roam_candidate = types::ScannedCandidate {
            bss: types::Bss {
                signal: types::Signal {
                    rssi_dbm: init_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM as i8 - 1,
                    snr_db: init_snr + MIN_SNR_IMPROVEMENT_TO_ROAM as i8 - 1,
                },
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        };

        // Start roam manager service
        let serve_fut = test_values.roam_manager_service.serve();
        let mut serve_fut = pin!(serve_fut);
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Initialize current connection data.
        let signal_data = SignalData::new(
            init_rssi,
            init_snr,
            EWMA_SMOOTHING_FACTOR,
            EWMA_VELOCITY_SMOOTHING_FACTOR,
        );
        let bss_quality_data = bss_selection::BssQualityData::new(
            signal_data,
            generate_random_channel(),
            PastConnectionList::default(),
        );
        let connection_data = ConnectionData::new(
            test_values.currently_fulfilled_connection.clone(),
            bss_quality_data,
            fuchsia_async::Time::now(),
        );

        // Send a roam search request to local roam manager service
        let req = RoamSearchRequest::new(connection_data, test_values.roam_req_sender.clone());
        test_values.roam_search_sender.unbounded_send(req).expect("Failed to send roam search req");

        // Progress the RoamManager future to process the request for a roam search.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Verify that a metric is logged for the roam scan
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::RoamingScan))
        );

        // Verify that connection selection receives request
        assert_variant!(
            test_values.connection_selection_request_receiver.try_next(),
            Ok(Some(request)) => {
                assert_variant!(request, ConnectionSelectionRequest::RoamSelection { network_id, credential, responder } => {
                    assert_eq!(network_id, test_values.currently_fulfilled_connection.target.network);
                    assert_eq!(credential, test_values.currently_fulfilled_connection.target.credential);
                    // Respond with barely better candidate.
                    responder.send(Some(selected_roam_candidate)).expect("failed to send connection selection");
                });
            }
        );

        // Progress the RoamManager future to process the roam search response. No roam connect
        // metric will be sent, since the candidate BSS is not enough of an improvement.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);
    }

    #[fuchsia::test]
    fn is_roam_worthwhile_dedupes_current_connection() {
        let _exec = TestExecutor::new();
        let (sender, _) = mpsc::unbounded();
        let roam_search_request = RoamSearchRequest::new(
            ConnectionData::new(
                generate_connect_selection(),
                generate_random_bss_quality_data(),
                fasync::Time::now(),
            ),
            sender,
        );
        // Should evaluate to false if the selected roam candidate is the same BSS as the current
        // connection.
        assert!(!is_roam_worthwhile(
            &roam_search_request,
            &roam_search_request.connection_data.currently_fulfilled_connection.target,
        ))
    }
}
