// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{
            connection_selection::{bss_selection, ConnectionSelector},
            types,
        },
        config_management,
        telemetry::{TelemetryEvent, TelemetrySender},
        util::pseudo_energy::SignalData,
    },
    anyhow::format_err,
    fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::{
        channel::mpsc, future::BoxFuture, select, stream::FuturesUnordered, FutureExt, StreamExt,
    },
    std::sync::Arc,
    tracing::info,
};

/// If there isn't a change in reasons to roam or significant change in RSSI, wait a while between
/// scans. IF there isn't a change, it is unlikely that there would be a reason to roam now.
const TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE: zx::Duration = zx::Duration::from_minutes(15);
const MIN_TIME_BETWEEN_ROAM_SCANS: zx::Duration = zx::Duration::from_minutes(1);
const MIN_RSSI_CHANGE_TO_ROAM_SCAN: f64 = 5.0;

/// Local Roam Manager is implemented as a trait so that it can be stubbed out in unit tests.
pub trait LocalRoamManagerApi: Send + Sync {
    fn handle_connection_stats(
        &mut self,
        stats: fidl_internal::SignalReportIndication,
        responder: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Result<u8, anyhow::Error>;

    fn handle_connection_start(
        &mut self,

        quality_data: bss_selection::BssQualityData,
        connection_start_time: fasync::Time,
        network: types::NetworkIdentifier,
        credential: config_management::Credential,
    );

    fn get_signal_data(&self) -> Option<SignalData>;
}

/// Data tracked about a connection to make decisions about whether to roam.
struct ConnectionData {
    /// Identifier for the network. Used to find other APs in the same network, and to track
    /// whether the connection on an iface has changed.
    network: types::NetworkIdentifier,
    /// The credential used to connect, to make sure the network config used to roam matches
    credential: config_management::Credential,
    quality_data: bss_selection::BssQualityData,
    roam_decision_data: RoamDecisionData,
}

impl ConnectionData {
    fn new(
        network: types::NetworkIdentifier,
        credential: config_management::Credential,
        quality_data: bss_selection::BssQualityData,
        connection_start_time: fasync::Time,
    ) -> Self {
        Self {
            network,
            credential,
            roam_decision_data: RoamDecisionData::new(
                quality_data.signal_data.ewma_rssi.get(),
                connection_start_time,
            ),
            quality_data,
        }
    }
}

#[cfg_attr(test, derive(Debug))]
pub struct RoamSearchRequest {
    network: types::NetworkIdentifier,
    /// Credential is used to ensure that the correct network config data is used.
    credential: config_management::Credential,
    /// This is used to tell the state machine to roam. The state machine should drop its end of
    /// the channel to ignore roam requests if the connection has already changed.
    _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
}

impl RoamSearchRequest {
    pub fn new(
        network: types::NetworkIdentifier,
        credential: config_management::Credential,
        _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Self {
        RoamSearchRequest { network, credential, _roam_req_sender }
    }
}

pub struct LocalRoamManager {
    /// Channel to receive requests for roam searches so LocalRoamManagerService can serve
    /// connection selection scans.
    roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
    telemetry_sender: TelemetrySender,
    connection_data: Option<ConnectionData>,
}

/// Handles roam futures for scans, since FuturesUnordered are not Send + Sync.
/// State machine's connected state sends updates to RoamManager, which may send scan requests
/// to LocalRoamManagerService, which may send a roam request to the connected state through
/// the provided channel.
pub struct LocalRoamManagerService {
    roam_futures: FuturesUnordered<
        BoxFuture<'static, Result<Option<types::ScannedCandidate>, anyhow::Error>>,
    >,
    connection_selector: Arc<ConnectionSelector>,
    // Receive requests for roam searches.
    roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
    telemetry_sender: TelemetrySender,
}

struct RoamDecisionData {
    time_prev_roam_scan: fasync::Time,
    roam_reasons_prev_scan: Vec<bss_selection::RoamReason>,
    /// This is the EWMA value, hence why it is an f64
    rssi_prev_roam_scan: f64,
}

impl RoamDecisionData {
    fn new(rssi: f64, connection_start_time: fasync::Time) -> Self {
        Self {
            time_prev_roam_scan: connection_start_time,
            roam_reasons_prev_scan: vec![],
            rssi_prev_roam_scan: rssi,
        }
    }
}

impl LocalRoamManagerApi for LocalRoamManager {
    /// Handle periodic stats. Use the sender to respond with roam requests. Returns the connection
    /// quality score for metrics purposes.
    fn handle_connection_stats(
        &mut self,
        ind: fidl_internal::SignalReportIndication,
        responder: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Result<u8, anyhow::Error> {
        // The connection data should have been initialized at the start of the connection.
        let connection_data = if let Some(data) = self.connection_data.as_mut() {
            data
        } else {
            return Err(format_err!("Connection data was not initialized; it should be initialized at the start of the connection."));
        };

        connection_data
            .quality_data
            .signal_data
            .update_with_new_measurement(ind.rssi_dbm, ind.snr_db);

        // Send RSSI and RSSI velocity metrics
        self.telemetry_sender.send(TelemetryEvent::OnSignalReport {
            ind,
            rssi_velocity: connection_data.quality_data.signal_data.ewma_rssi_velocity.get().round()
                as i8,
        });

        // Evaluate current BSS, and determine if roaming future should be triggered. Do not
        // trigger a roaming scan if one is in progress.
        let (bss_score, roam_reasons) =
            bss_selection::evaluate_current_bss(&connection_data.quality_data);
        if !roam_reasons.is_empty() {
            let now = fasync::Time::now();
            if now
                < connection_data.roam_decision_data.time_prev_roam_scan
                    + MIN_TIME_BETWEEN_ROAM_SCANS
            {
                return Ok(bss_score);
            }
            // If there isn't a new reason to roam and the previous scan
            // happened recently, do not scan again.
            let is_scan_old = now
                > connection_data.roam_decision_data.time_prev_roam_scan
                    + TIME_BETWEEN_ROAM_SCANS_IF_NO_CHANGE;
            let has_new_reason = roam_reasons
                .iter()
                .any(|r| !connection_data.roam_decision_data.roam_reasons_prev_scan.contains(r));
            let rssi = connection_data.quality_data.signal_data.ewma_rssi.get();
            let is_rssi_different = (connection_data.roam_decision_data.rssi_prev_roam_scan - rssi)
                .abs()
                > MIN_RSSI_CHANGE_TO_ROAM_SCAN;
            if is_scan_old || has_new_reason || is_rssi_different {
                // Initiate roam scan.
                let req = RoamSearchRequest::new(
                    connection_data.network.clone(),
                    connection_data.credential.clone(),
                    responder,
                );
                let _ = self.roam_search_sender.unbounded_send(req);

                // Updated fields for tracking roam scan decisions
                connection_data.roam_decision_data.time_prev_roam_scan = fasync::Time::now();
                connection_data.roam_decision_data.roam_reasons_prev_scan = roam_reasons;
                connection_data.roam_decision_data.rssi_prev_roam_scan = rssi;
            }
        }
        return Ok(bss_score);
    }

    fn handle_connection_start(
        &mut self,
        quality_data: bss_selection::BssQualityData,
        connection_start_time: fasync::Time,
        network: types::NetworkIdentifier,
        credential: config_management::Credential,
    ) {
        let data = ConnectionData::new(network, credential, quality_data, connection_start_time);

        self.connection_data = Some(data);
    }

    // Return the signal data, or none if the connection to track was never initialized.
    fn get_signal_data(&self) -> Option<SignalData> {
        self.connection_data.as_ref().map(|data| data.quality_data.signal_data)
    }
}

impl LocalRoamManager {
    pub fn new(
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        telemetry_sender: TelemetrySender,
    ) -> Self {
        Self { roam_search_sender, telemetry_sender, connection_data: None }
    }
}

impl LocalRoamManagerService {
    pub fn new(
        roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
        telemetry_sender: TelemetrySender,
        connection_selector: Arc<ConnectionSelector>,
    ) -> Self {
        Self {
            roam_futures: FuturesUnordered::new(),
            connection_selector,
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

                    let roam_fut = get_connection_selection_future(
                        self.connection_selector.clone(),
                        req.network,
                        req.credential
                    );
                    self.roam_futures.push(roam_fut.boxed());
                    self.telemetry_sender.send(TelemetryEvent::RoamingScan);
                }
                _roam_candidate = self.roam_futures.select_next_some() => {
                    // Roam scan results will be handled here
                }
            }
        }
    }
}

async fn get_connection_selection_future(
    connection_selector: Arc<ConnectionSelector>,
    network: types::NetworkIdentifier,
    credential: config_management::Credential,
) -> Result<Option<types::ScannedCandidate>, anyhow::Error> {
    connection_selector.find_and_select_roam_candidate(network, credential).await
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            client::connection_selection::{
                scan, EWMA_SMOOTHING_FACTOR, EWMA_VELOCITY_SMOOTHING_FACTOR,
            },
            config_management::network_config::PastConnectionList,
            util::{
                pseudo_energy::SignalData,
                testing::{
                    create_inspect_persistence_channel,
                    fakes::{FakeSavedNetworksManager, FakeScanRequester},
                    generate_random_channel, generate_random_network_identifier,
                    generate_random_password,
                },
            },
        },
        fuchsia_async::TestExecutor,
        futures::task::Poll,
        pin_utils::pin_mut,
        test_util::{assert_gt, assert_lt},
        wlan_common::assert_variant,
    };

    struct RoamManagerTestValues {
        roam_manager: LocalRoamManager,
        roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
        roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        /// A randomly generated WPA2 network identifier and password to use in tests.
        network: types::NetworkIdentifier,
        credential: config_management::Credential,
    }

    fn roam_manager_test_setup() -> RoamManagerTestValues {
        let (roam_search_sender, roam_search_receiver) = mpsc::unbounded();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let roam_manager = LocalRoamManager::new(roam_search_sender, telemetry_sender);
        let (roam_req_sender, _roam_req_receiver) = mpsc::unbounded();
        let network = generate_random_network_identifier();
        let credential = generate_random_password();

        RoamManagerTestValues {
            roam_manager,
            roam_search_receiver,
            roam_req_sender,
            telemetry_receiver,
            network,
            credential,
        }
    }

    #[fuchsia::test]
    fn test_roam_manager_should_queue_scan() {
        // Test that if connection quality data comes in indicating that a roam should be
        // considered, the roam manager will send out a scan request.
        let mut _exec = TestExecutor::new();
        let mut test_values = roam_manager_test_setup();

        let init_rssi = -70;
        let init_snr = 20;
        let past_connections = PastConnectionList::default();
        let connect_start_time = fasync::Time::now() - fasync::Duration::from_hours(1);
        let bss_quality_data = bss_selection::BssQualityData::new(
            SignalData::new(
                init_rssi,
                init_snr,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            generate_random_channel(),
            past_connections,
        );
        let rssi_dbm = -80;
        let snr_db = 10;
        let signal_report = fidl_internal::SignalReportIndication { rssi_dbm, snr_db };
        // Tell the LocalRoamManager about the start of a connection so that it begins tracking
        // roaming data.
        test_values.roam_manager.handle_connection_start(
            bss_quality_data.clone(),
            connect_start_time,
            test_values.network.clone(),
            test_values.credential.clone(),
        );

        // Send some periodic stats to the RoamManager for a connection with poor signal.
        let _score = test_values
            .roam_manager
            .handle_connection_stats(signal_report, test_values.roam_req_sender)
            .expect("Failed to get connection stats");

        // Check that a scan request is sent to the Roam Manager Service.
        let received_roam_req = test_values.roam_search_receiver.try_next();
        assert_variant!(received_roam_req, Ok(Some(req)) => {
            assert_eq!(req.network, test_values.network);
            assert_eq!(req.credential, test_values.credential);
        });

        // Verify that a telemerty event is sent for the RSSI and RSSI velocity
        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, signal_report);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_lt!(rssi_velocity, 0);
        });
    }

    #[fuchsia::test]
    fn test_roam_manager_tracks_signal_velocity_and_sends_telemetry_events() {
        // LocalRoamManager should keep track of the RSSI velocity which will be used to evaluate
        // connection quality and be sent to telemetry.
        let mut _exec = TestExecutor::new();
        let mut test_values = roam_manager_test_setup();

        let init_rssi = -70;
        let init_snr = 20;
        let past_connections = PastConnectionList::default();
        let connect_start_time = fasync::Time::now() - fasync::Duration::from_hours(1);
        let bss_quality_data = bss_selection::BssQualityData::new(
            SignalData::new(
                init_rssi,
                init_snr,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            generate_random_channel(),
            past_connections,
        );
        // Tell the LocalRoamManager about the start of a connection so that it begins tracking
        // roaming data.
        test_values.roam_manager.handle_connection_start(
            bss_quality_data.clone(),
            connect_start_time,
            test_values.network.clone(),
            test_values.credential.clone(),
        );

        // Send some stats with RSSI and SNR getting worse to the LocalRoamManager.
        let rssi_dbm_1 = -80;
        let snr_db_1 = 10;
        let signal_report_1 =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_dbm_1, snr_db: snr_db_1 };
        let _score = test_values
            .roam_manager
            .handle_connection_stats(signal_report_1, test_values.roam_req_sender.clone())
            .expect("Failed to get connection stats");

        // Verify that a telemerty event is sent for the RSSI and RSSI velocity
        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, signal_report_1);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_lt!(rssi_velocity, 0);
        });

        // Send some stats with RSSI and SNR getting getting better and check that RSSI velocity
        // is positive.
        let rssi_dbm_2 = -60;
        let snr_db_2 = 30;
        let signal_report_2 =
            fidl_internal::SignalReportIndication { rssi_dbm: rssi_dbm_2, snr_db: snr_db_2 };
        let _score = test_values
            .roam_manager
            .handle_connection_stats(signal_report_2, test_values.roam_req_sender)
            .expect("Failed to get connection stats");

        assert_variant!(test_values.telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::OnSignalReport {ind, rssi_velocity})) => {
            assert_eq!(ind, signal_report_2);
            // verify that RSSI velocity is negative since the signal report RSSI is lower.
            assert_gt!(rssi_velocity, 0);
        });
    }

    #[fuchsia::test]
    fn test_roam_manager_should_not_roam_scan_frequently() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(10));
        let current_time = fasync::Time::after(fasync::Duration::from_hours(2));
        exec.set_fake_time(current_time);
        // Test that if the connection continues to be bad, the roam manager does not scan too
        // often.
        let mut test_values = roam_manager_test_setup();

        let init_rssi = -70;
        let init_snr = 20;
        let past_connections = PastConnectionList::default();
        let connect_start_time = fasync::Time::now() - fasync::Duration::from_hours(1);
        let bss_quality_data = bss_selection::BssQualityData::new(
            SignalData::new(
                init_rssi,
                init_snr,
                EWMA_SMOOTHING_FACTOR,
                EWMA_VELOCITY_SMOOTHING_FACTOR,
            ),
            generate_random_channel(),
            past_connections,
        );
        let signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: init_rssi, snr_db: init_snr };
        // Tell the LocalRoamManager about the start of a connection so that it begins tracking
        // roaming data.
        test_values.roam_manager.handle_connection_start(
            bss_quality_data.clone(),
            connect_start_time,
            test_values.network.clone(),
            test_values.credential.clone(),
        );

        // Send some periodic stats to the RoamManager for a connection with poor signal.
        let _score = test_values
            .roam_manager
            .handle_connection_stats(signal_report, test_values.roam_req_sender.clone())
            .expect("Failed to get connection stats");

        // Check that a scan request is sent to the Roam Manager Service.
        let received_roam_req = test_values.roam_search_receiver.try_next();
        assert_variant!(received_roam_req, Ok(Some(req)) => {
            assert_eq!(req.network, test_values.network);
            assert_eq!(req.credential, test_values.credential);
        });

        // Send stats with a worse RSSI and check that a roam scan is not initiated
        let init_rssi = -70;
        let init_snr = 20;
        let signal_report =
            fidl_internal::SignalReportIndication { rssi_dbm: init_rssi, snr_db: init_snr };

        let _score = test_values
            .roam_manager
            .handle_connection_stats(signal_report, test_values.roam_req_sender.clone())
            .expect("Failed to get connection stats");
    }

    struct RoamManagerServiceTestValues {
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        /// This is needed for roam search requests; normally it is how the RoamManagerService
        /// would tell the state machine roaming decisions.
        roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
        roam_manager_service: LocalRoamManagerService,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        fake_scan_requester: Arc<FakeScanRequester>,
        id: types::NetworkIdentifier,
        credential: config_management::Credential,
    }

    fn roam_service_test_setup() -> RoamManagerServiceTestValues {
        let (roam_search_sender, roam_search_receiver) = mpsc::unbounded();
        let (roam_req_sender, _roam_req_receiver) = mpsc::unbounded();
        let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let fake_saved_networks = Arc::new(FakeSavedNetworksManager::new());
        let fake_scan_requester = Arc::new(FakeScanRequester::new());
        let selector = Arc::new(ConnectionSelector::new(
            fake_saved_networks.clone(),
            fake_scan_requester.clone(),
            fuchsia_inspect::Inspector::default().root().create_child("connection_selector"),
            persistence_req_sender,
            telemetry_sender.clone(),
        ));
        let id = generate_random_network_identifier();
        let credential = generate_random_password();

        let roam_manager_service =
            LocalRoamManagerService::new(roam_search_receiver, telemetry_sender, selector);

        RoamManagerServiceTestValues {
            roam_search_sender,
            roam_req_sender,
            roam_manager_service,
            telemetry_receiver,
            fake_scan_requester,
            id,
            credential,
        }
    }

    // Test that when LocalRoamManagerService gets a roam scan request, it initiates bss selection.
    #[fuchsia::test]
    fn test_roam_manager_service_initiates_network_seleciton() {
        let mut exec = TestExecutor::new();
        let mut test_values = roam_service_test_setup();
        // Add scan result to send back; FakeScanRequester needs a value to send.
        exec.run_singlethreaded(test_values.fake_scan_requester.add_scan_result(Ok(vec![])));
        exec.run_singlethreaded(test_values.fake_scan_requester.add_scan_result(Ok(vec![])));

        let serve_fut = test_values.roam_manager_service.serve();
        pin_mut!(serve_fut);
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Send a request for the LocalRoamManagerService to initiate bss selection.
        let req = RoamSearchRequest::new(
            test_values.id.clone(),
            test_values.credential,
            test_values.roam_req_sender,
        );
        test_values.roam_search_sender.unbounded_send(req).expect("Failed to send roam search req");

        // Progress the RoamManager future to process the request for a roam search.
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        // Verify that a metric is logged for the roam scan
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::RoamingScan))
        );

        // Check that a scan happens for bss selection. An undirected passive scan is used for
        // roaming so no SSIDs are in the scan request.
        let scan_reason = scan::ScanReason::RoamSearch;
        let ssids = vec![];
        let channels = vec![];
        let verify_scan_fut =
            test_values.fake_scan_requester.verify_scan_request((scan_reason, ssids, channels));
        pin_mut!(verify_scan_fut);
        assert_variant!(&mut exec.run_until_stalled(&mut verify_scan_fut), Poll::Ready(()));
    }
}
