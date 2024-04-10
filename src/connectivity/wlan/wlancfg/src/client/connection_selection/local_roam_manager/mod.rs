// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{
            connection_selection::{bss_selection, ConnectionSelector},
            types,
        },
        telemetry::{TelemetryEvent, TelemetrySender},
    },
    fuchsia_async as fasync,
    futures::{
        channel::mpsc, future::BoxFuture, select, stream::FuturesUnordered, FutureExt, StreamExt,
    },
    std::sync::Arc,
    tracing::{info, warn},
};

pub mod roam_monitor;

const MIN_RSSI_IMPROVEMENT_TO_ROAM: f64 = 3.0;
const MIN_SNR_IMPROVEMENT_TO_ROAM: f64 = 3.0;

/// Roam searches return None if there were no BSSs found in the scan.
type RoamSearchResult = Result<Option<(types::ScannedCandidate, RoamSearchRequest)>, anyhow::Error>;

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
    roam_futures: FuturesUnordered<BoxFuture<'static, RoamSearchResult>>,
    connection_selector: Arc<ConnectionSelector>,
    // Receive requests for roam searches.
    roam_search_receiver: mpsc::UnboundedReceiver<RoamSearchRequest>,
    telemetry_sender: TelemetrySender,
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
                    let roam_fut = get_roaming_connection_selection_future(
                        self.connection_selector.clone(),
                        req,
                    );
                    self.roam_futures.push(roam_fut.boxed());
                    self.telemetry_sender.send(TelemetryEvent::RoamingScan);
                }
                roam_fut_response = self.roam_futures.select_next_some() => {
                    match roam_fut_response {
                        Ok(Some((candidate, request))) => {
                            if is_roam_worthwhile(&request, &candidate) {
                                info!("Roam would be requested to candidate: {:?}. Roaming is not enabled.", candidate.to_string_without_pii());
                                self.telemetry_sender.send(TelemetryEvent::WouldRoamConnect);
                            }
                        },
                        Ok(None) => warn!("No roam candidates found in scan. This is unexpected, as at least the currently connected BSS should be found."),
                        Err(e) => warn!("An error occured during the roam scan: {:?}", e),
                    }
                }
            }
        }
    }
}

/// Data tracked about a connection to make decisions about whether to roam.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct ConnectionData {
    // Information about the current connection, from the time of initial connection.
    currently_fulfilled_connection: types::ConnectSelection,
    // Rolling data about the current quality of the connection
    quality_data: bss_selection::BssQualityData,
    // Roaming related metadata
    roam_decision_data: RoamDecisionData,
}

impl ConnectionData {
    pub fn new(
        currently_fulfilled_connection: types::ConnectSelection,
        quality_data: bss_selection::BssQualityData,
        connection_start_time: fasync::Time,
    ) -> Self {
        Self {
            currently_fulfilled_connection,
            roam_decision_data: RoamDecisionData::new(
                quality_data.signal_data.ewma_rssi.get(),
                connection_start_time,
            ),
            quality_data,
        }
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
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

#[cfg_attr(test, derive(Debug))]
pub struct RoamSearchRequest {
    connection_data: ConnectionData,
    /// This is used to tell the state machine to roam. The state machine should drop its end of
    /// the channel to ignore roam requests if the connection has already changed.
    _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
}

impl RoamSearchRequest {
    fn new(
        connection_data: ConnectionData,
        _roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
    ) -> Self {
        RoamSearchRequest { connection_data, _roam_req_sender }
    }
}

async fn get_roaming_connection_selection_future(
    connection_selector: Arc<ConnectionSelector>,
    request: RoamSearchRequest,
) -> RoamSearchResult {
    match connection_selector
        .find_and_select_roam_candidate(
            request.connection_data.currently_fulfilled_connection.target.network.clone(),
            &request.connection_data.currently_fulfilled_connection.target.credential,
        )
        .await?
    {
        Some(selected_candidate) => Ok(Some((selected_candidate, request))),
        _ => Ok(None),
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
                scan, EWMA_SMOOTHING_FACTOR, EWMA_VELOCITY_SMOOTHING_FACTOR,
            },
            config_management::network_config::{NetworkConfig, PastConnectionList},
            util::{
                pseudo_energy::SignalData,
                testing::{
                    create_inspect_persistence_channel,
                    fakes::{FakeSavedNetworksManager, FakeScanRequester},
                    generate_connect_selection, generate_random_bss,
                    generate_random_bss_quality_data, generate_random_bssid,
                    generate_random_channel,
                },
            },
        },
        fidl_fuchsia_wlan_internal as fidl_internal,
        fuchsia_async::TestExecutor,
        futures::task::Poll,
        ieee80211::MacAddrBytes,
        std::pin::pin,
        wlan_common::{assert_variant, random_fidl_bss_description, security::SecurityDescriptor},
    };

    struct RoamManagerServiceTestValues {
        roam_search_sender: mpsc::UnboundedSender<RoamSearchRequest>,
        /// This is needed for roam search requests; normally it is how the RoamManagerService
        /// would tell the state machine roaming decisions.
        roam_req_sender: mpsc::UnboundedSender<types::ScannedCandidate>,
        roam_manager_service: LocalRoamManagerService,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
        fake_scan_requester: Arc<FakeScanRequester>,
        currently_fulfilled_connection: types::ConnectSelection,
    }

    fn roam_service_test_setup() -> RoamManagerServiceTestValues {
        let currently_fulfilled_connection = generate_connect_selection();
        let config = NetworkConfig::new(
            currently_fulfilled_connection.target.network.clone(),
            currently_fulfilled_connection.target.credential.clone(),
            true,
        )
        .expect("failed to create config");
        let fake_saved_networks =
            Arc::new(FakeSavedNetworksManager::new_with_saved_networks(vec![config.into()]));

        let (roam_search_sender, roam_search_receiver) = mpsc::unbounded();
        let (roam_req_sender, _roam_req_receiver) = mpsc::unbounded();
        let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);
        let fake_scan_requester = Arc::new(FakeScanRequester::new());
        let selector = Arc::new(ConnectionSelector::new(
            fake_saved_networks.clone(),
            fake_scan_requester.clone(),
            fuchsia_inspect::Inspector::default().root().create_child("connection_selector"),
            persistence_req_sender,
            telemetry_sender.clone(),
        ));

        let roam_manager_service =
            LocalRoamManagerService::new(roam_search_receiver, telemetry_sender, selector);

        RoamManagerServiceTestValues {
            roam_search_sender,
            roam_req_sender,
            roam_manager_service,
            telemetry_receiver,
            fake_scan_requester,
            currently_fulfilled_connection,
        }
    }

    /// Generate scan results using the network identifier in test values, one for each
    /// provided BSSID.
    fn gen_scan_result_with_bssids(
        test_values: &RoamManagerServiceTestValues,
        bssids: Vec<types::Bssid>,
    ) -> Vec<types::ScanResult> {
        use rand::Rng;
        bssids
            .into_iter()
            .map(|bssid| {
                let bss_description = fidl_internal::BssDescription {
                    bssid: bssid.to_array(),
                    ..random_fidl_bss_description!()
                };
                types::ScanResult {
                    ssid: test_values.currently_fulfilled_connection.target.network.ssid.clone(),
                    // A WPA2 network is generated for the test values.
                    security_type_detailed: types::SecurityTypeDetailed::Wpa2Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(vec![
                            SecurityDescriptor::WPA2_PERSONAL,
                        ]),
                        bss_description: bss_description.into(),
                        ..generate_random_bss()
                    }],
                }
            })
            .collect()
    }

    fn gen_scan_result_with_signal(
        test_values: &RoamManagerServiceTestValues,
        bssid: types::Bssid,
        rssi: i8,
        snr: i8,
    ) -> types::ScanResult {
        use rand::Rng;
        types::ScanResult {
            ssid: test_values.currently_fulfilled_connection.target.network.ssid.clone(),
            // A WPA2 network is generated for the test values.
            security_type_detailed: types::SecurityTypeDetailed::Wpa2Personal,
            compatibility: types::Compatibility::Supported,
            entries: vec![types::Bss {
                bssid,
                compatibility: wlan_common::scan::Compatibility::expect_some(vec![
                    SecurityDescriptor::WPA2_PERSONAL,
                ]),
                bss_description: random_fidl_bss_description!().into(),
                signal: types::Signal { rssi_dbm: rssi, snr_db: snr },
                ..generate_random_bss()
            }],
        }
    }

    // Test that when LocalRoamManagerService gets a roam scan request, it initiates bss selection.
    #[fuchsia::test]
    fn test_roam_manager_service_initiates_network_selection() {
        let mut exec = TestExecutor::new();
        let mut test_values = roam_service_test_setup();
        // Add scan result to send back with another BSS to connect to.
        let other_bssid = generate_random_bssid();
        let scan_results = gen_scan_result_with_bssids(
            &test_values,
            vec![test_values.currently_fulfilled_connection.target.bss.bssid, other_bssid],
        );
        exec.run_singlethreaded(test_values.fake_scan_requester.add_scan_result(Ok(scan_results)));

        let serve_fut = test_values.roam_manager_service.serve();
        let mut serve_fut = pin!(serve_fut);
        assert_variant!(exec.run_until_stalled(&mut serve_fut), Poll::Pending);

        let init_rssi = -80;
        let init_snr = 10;
        let past_connections = PastConnectionList::default();
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

        // Check that a scan happens for bss selection. An undirected passive scan is used for
        // roaming so no SSIDs are in the scan request.
        let scan_reason = scan::ScanReason::RoamSearch;
        let ssids = vec![];
        let channels = vec![];
        let verify_scan_fut =
            test_values.fake_scan_requester.verify_scan_request((scan_reason, ssids, channels));
        let mut verify_scan_fut = pin!(verify_scan_fut);
        assert_variant!(&mut exec.run_until_stalled(&mut verify_scan_fut), Poll::Ready(()));

        //  A metric will be logged for BSS selection, ignore it.
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { .. }))
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
        let other_bssid = generate_random_bssid();
        let scan_result = gen_scan_result_with_signal(
            &test_values,
            other_bssid,
            init_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM as i8 + 1,
            init_snr + 10,
        );
        exec.run_singlethreaded(
            test_values.fake_scan_requester.add_scan_result(Ok(vec![scan_result])),
        );

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

        // Check that a scan happens for bss selection. An undirected passive scan is used for
        // roaming, so no SSIDs are in the scan request.
        let verify_scan_fut = test_values.fake_scan_requester.verify_scan_request((
            scan::ScanReason::RoamSearch,
            vec![],
            vec![],
        ));
        let mut verify_scan_fut = pin!(verify_scan_fut);
        assert_variant!(&mut exec.run_until_stalled(&mut verify_scan_fut), Poll::Ready(()));

        //  A metric will be logged for BSS selection, ignore it.
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { .. }))
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

        // Set up fake scan results of a barely better candidate BSS, less than the min improvement
        // threshold.
        let init_rssi = -80;
        let init_snr = 10;
        let other_bssid = generate_random_bssid();
        let scan_result = gen_scan_result_with_signal(
            &test_values,
            other_bssid,
            init_rssi + MIN_RSSI_IMPROVEMENT_TO_ROAM as i8 - 1,
            init_snr + MIN_SNR_IMPROVEMENT_TO_ROAM as i8 - 1,
        );
        exec.run_singlethreaded(
            test_values.fake_scan_requester.add_scan_result(Ok(vec![scan_result])),
        );

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

        // Check that a scan happens for bss selection. An undirected passive scan is used for
        // roaming, so no SSIDs are in the scan request.
        let verify_scan_fut = test_values.fake_scan_requester.verify_scan_request((
            scan::ScanReason::RoamSearch,
            vec![],
            vec![],
        ));
        let mut verify_scan_fut = pin!(verify_scan_fut);
        assert_variant!(&mut exec.run_until_stalled(&mut verify_scan_fut), Poll::Ready(()));

        //  A metric will be logged for BSS selection, ignore it.
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { .. }))
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
