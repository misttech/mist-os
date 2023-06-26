// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{
            scan::{self, ScanReason},
            types::{self, Bss, InternalSavedNetworkData, SecurityType, SecurityTypeDetailed},
        },
        config_management::{
            select_authentication_method, select_subset_potentially_hidden_networks, Credential,
            SavedNetworksManagerApi,
        },
        telemetry::{self, TelemetryEvent, TelemetrySender},
    },
    fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload},
    fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync,
    fuchsia_inspect::{Node as InspectNode, StringReference},
    fuchsia_inspect_contrib::{
        auto_persist::{self, AutoPersist},
        inspect_insert,
        log::WriteInspect,
        nodes::BoundedListNode as InspectBoundedListNode,
    },
    fuchsia_zircon as zx,
    futures::lock::Mutex,
    std::collections::HashMap,
    std::{collections::HashSet, sync::Arc},
    tracing::{debug, error, info, warn},
    wlan_common::{
        self, hasher::WlanHasher, security::SecurityAuthenticator, sequestered::Sequestered,
    },
    wlan_inspect::wrappers::InspectWlanChan,
    wlan_metrics_registry::{
        SavedNetworkInScanResultMigratedMetricDimensionBssCount,
        SavedNetworkInScanResultWithActiveScanMigratedMetricDimensionActiveScanSsidsObserved as ActiveScanSsidsObserved,
        ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount,
        LAST_SCAN_AGE_WHEN_SCAN_REQUESTED_MIGRATED_METRIC_ID,
        SAVED_NETWORK_IN_SCAN_RESULT_MIGRATED_METRIC_ID,
        SAVED_NETWORK_IN_SCAN_RESULT_WITH_ACTIVE_SCAN_MIGRATED_METRIC_ID,
        SCAN_RESULTS_RECEIVED_MIGRATED_METRIC_ID,
    },
};

pub mod bss_selection;
pub mod network_selection;
pub mod scoring_functions;

/// Number of previous RSSI measurements to exponentially weigh into average.
/// TODO(fxbug.dev/84870): Tune smoothing factor.
pub(crate) const EWMA_SMOOTHING_FACTOR: usize = 10;
/// Number of previous RSSI velocities to exponentially weigh into the average. Keeping the number
/// small lets the number react quickly and have a magnitude similar to if it weren't smoothed as
/// an EWMA, but makes the EWMA less resistant to momentary outliers.
pub(crate) const EWMA_VELOCITY_SMOOTHING_FACTOR: usize = 3;

const INSPECT_EVENT_LIMIT_FOR_CONNECTION_SELECTIONS: usize = 10;

const RECENT_DISCONNECT_WINDOW: zx::Duration = zx::Duration::from_seconds(60 * 15);
const RECENT_FAILURE_WINDOW: zx::Duration = zx::Duration::from_seconds(60 * 5);
const SHORT_CONNECT_DURATION: zx::Duration = zx::Duration::from_seconds(7 * 60);

pub struct ConnectionSelector {
    saved_network_manager: Arc<dyn SavedNetworksManagerApi>,
    scan_requester: Arc<dyn scan::ScanRequestApi>,
    last_scan_result_time: Arc<Mutex<zx::Time>>,
    hasher: WlanHasher,
    _inspect_node_root: Arc<Mutex<InspectNode>>,
    inspect_node_for_connection_selection: Arc<Mutex<AutoPersist<InspectBoundedListNode>>>,
    telemetry_sender: TelemetrySender,
}

impl ConnectionSelector {
    pub fn new(
        saved_network_manager: Arc<dyn SavedNetworksManagerApi>,
        scan_requester: Arc<dyn scan::ScanRequestApi>,
        hasher: WlanHasher,
        inspect_node: InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
        telemetry_sender: TelemetrySender,
    ) -> Self {
        let inspect_node_for_connection_selection = InspectBoundedListNode::new(
            inspect_node.create_child("connection_selection"),
            INSPECT_EVENT_LIMIT_FOR_CONNECTION_SELECTIONS,
        );
        let inspect_node_for_connection_selection = AutoPersist::new(
            inspect_node_for_connection_selection,
            "wlancfg-network-selection",
            persistence_req_sender.clone(),
        );
        Self {
            saved_network_manager,
            scan_requester,
            last_scan_result_time: Arc::new(Mutex::new(zx::Time::ZERO)),
            hasher,
            _inspect_node_root: Arc::new(Mutex::new(inspect_node)),
            inspect_node_for_connection_selection: Arc::new(Mutex::new(
                inspect_node_for_connection_selection,
            )),
            telemetry_sender,
        }
    }

    /// Full connection selection. Scans to find available candidates, uses network selection (or
    /// optional provided network) to filter out networks, and then bss selection to select the best
    /// of the remaining candidates. If the candidate was discovered via a passive scan, augments the
    /// bss info with an active scan
    pub(crate) async fn find_and_select_scanned_candidate(
        &self,
        network: Option<types::NetworkIdentifier>,
        reason: types::ConnectReason,
    ) -> Option<types::ScannedCandidate> {
        // Scan for BSSs belonging to saved networks.
        let available_candidate_list =
            self.find_available_bss_candidate_list(network.clone()).await;

        // Network selection.
        let available_networks: HashSet<types::NetworkIdentifier> =
            available_candidate_list.iter().map(|candidate| candidate.network.clone()).collect();
        let selected_networks = network_selection::select_networks(available_networks, &network);

        // Send network selection metrics
        self.telemetry_sender.send(TelemetryEvent::NetworkSelectionDecision {
            network_selection_type: match network {
                Some(_) => telemetry::NetworkSelectionType::Directed,
                None => telemetry::NetworkSelectionType::Undirected,
            },
            num_candidates: (!available_candidate_list.is_empty())
                .then_some(available_candidate_list.len())
                .ok_or(()),
            selected_count: selected_networks.len(),
        });

        // Filter down to only BSSs in the selected networks.
        let allowed_candidate_list = available_candidate_list
            .iter()
            .filter(|candidate| selected_networks.contains(&candidate.network))
            .cloned()
            .collect();

        // BSS Selection.
        let selection = match bss_selection::select_bss(
            allowed_candidate_list,
            reason,
            self.inspect_node_for_connection_selection.clone(),
            self.telemetry_sender.clone(),
        )
        .await
        {
            Some(mut candidate) => {
                if network.is_some() {
                    // Strip scan observation type, because the candidate was discovered via a
                    // directed active scan, so we cannot know if it is discoverable via a passive
                    // scan.
                    candidate.bss.observation = types::ScanObservation::Unknown;
                }
                // If it was observed passively, augment with active scan.
                match candidate.bss.observation {
                    types::ScanObservation::Passive => {
                        Some(self.augment_bss_candidate_with_active_scan(candidate.clone()).await)
                    }
                    _ => Some(candidate),
                }
            }
            None => None,
        };

        selection
    }

    /// Requests scans and compiles list of BSSs that appear in scan results and belong to currently
    /// saved networks.
    async fn find_available_bss_candidate_list(
        &self,
        network: Option<types::NetworkIdentifier>,
    ) -> Vec<types::ScannedCandidate> {
        let scan_for_candidates = || async {
            if let Some(ref network) = network {
                self.scan_requester
                    .perform_scan(ScanReason::BssSelection, vec![network.ssid.clone()], vec![])
                    .await
            } else {
                let last_scan_result_time = *self.last_scan_result_time.lock().await;
                let scan_age = zx::Time::get_monotonic() - last_scan_result_time;
                if last_scan_result_time != zx::Time::ZERO {
                    info!("Scan results are {}s old, triggering a scan", scan_age.into_seconds());
                    self.telemetry_sender.send(TelemetryEvent::LogMetricEvents {
                        events: vec![MetricEvent {
                            metric_id: LAST_SCAN_AGE_WHEN_SCAN_REQUESTED_MIGRATED_METRIC_ID,
                            event_codes: vec![],
                            payload: MetricEventPayload::IntegerValue(scan_age.into_micros()),
                        }],
                        ctx: "ConnectionSelector::perform_scan",
                    });
                }
                let passive_scan_results = match self
                    .scan_requester
                    .perform_scan(ScanReason::NetworkSelection, vec![], vec![])
                    .await
                {
                    Ok(scan_results) => scan_results,
                    Err(e) => return Err(e),
                };
                let passive_scan_ssids: HashSet<types::Ssid> = HashSet::from_iter(
                    passive_scan_results.iter().map(|result| result.ssid.clone()),
                );
                let requested_active_scan_ssids: Vec<types::Ssid> =
                    select_subset_potentially_hidden_networks(
                        self.saved_network_manager.get_networks().await,
                    )
                    .drain(..)
                    .map(|id| id.ssid)
                    .filter(|ssid| !passive_scan_ssids.contains(ssid))
                    .collect();

                self.telemetry_sender.send(TelemetryEvent::ActiveScanRequested {
                    num_ssids_requested: requested_active_scan_ssids.len(),
                });

                if requested_active_scan_ssids.is_empty() {
                    Ok(passive_scan_results)
                } else {
                    self.scan_requester
                        .perform_scan(
                            ScanReason::NetworkSelection,
                            requested_active_scan_ssids,
                            vec![],
                        )
                        .await
                        .map(|mut scan_results| {
                            scan_results.extend(passive_scan_results);
                            scan_results
                        })
                }
            }
        };

        let scan_results = scan_for_candidates().await;
        let candidates = match scan_results {
            Err(e) => {
                warn!("Failed to get available BSSs, {:?}", e);
                vec![]
            }
            Ok(scan_results) => {
                let candidates = merge_saved_networks_and_scan_data(
                    &self.saved_network_manager,
                    scan_results,
                    &self.hasher,
                )
                .await;
                if network.is_none() {
                    *self.last_scan_result_time.lock().await = zx::Time::get_monotonic();
                    record_metrics_on_scan(candidates.clone(), &self.telemetry_sender);
                }
                candidates
            }
        };
        candidates
    }

    /// If a BSS was discovered via a passive scan, we need to perform an active scan on it to
    /// discover all the information potentially needed by the SME layer.
    async fn augment_bss_candidate_with_active_scan(
        &self,
        scanned_candidate: types::ScannedCandidate,
    ) -> types::ScannedCandidate {
        // This internal function encapsulates all the logic and has a Result<> return type,
        // allowing us to use the `?` operator inside it to reduce nesting.
        async fn get_enhanced_bss_description(
            scanned_candidate: &types::ScannedCandidate,
            scan_requester: Arc<dyn scan::ScanRequestApi>,
        ) -> Result<Sequestered<fidl_internal::BssDescription>, ()> {
            match scanned_candidate.bss.observation {
                types::ScanObservation::Passive => {
                    info!("Performing directed active scan on selected network")
                }
                types::ScanObservation::Active => {
                    debug!("Network already discovered via active scan.");
                    return Err(());
                }
                types::ScanObservation::Unknown => {
                    error!("Unexpected `Unknown` variant of network `observation`.");
                    return Err(());
                }
            }

            // Perform the scan
            let mut directed_scan_result = scan_requester
                .perform_scan(
                    ScanReason::BssSelectionAugmentation,
                    vec![scanned_candidate.network.ssid.clone()],
                    vec![scanned_candidate.bss.channel],
                )
                .await
                .map_err(|_| {
                    info!("Failed to perform active scan to augment BSS info.");
                })?;

            // Find the bss in the results
            let bss_description = directed_scan_result
                .drain(..)
                .find_map(|mut network| {
                    if network.ssid == scanned_candidate.network.ssid {
                        for bss in network.entries.drain(..) {
                            if bss.bssid == scanned_candidate.bss.bssid {
                                return Some(bss.bss_description);
                            }
                        }
                    }
                    None
                })
                .ok_or_else(|| {
                    info!("BSS info will lack active scan augmentation, proceeding anyway.");
                })?;

            Ok(bss_description)
        }

        match get_enhanced_bss_description(&scanned_candidate, self.scan_requester.clone()).await {
            Ok(new_bss_description) => {
                let updated_scanned_bss =
                    Bss { bss_description: new_bss_description, ..scanned_candidate.bss.clone() };
                types::ScannedCandidate { bss: updated_scanned_bss, ..scanned_candidate }
            }
            Err(()) => scanned_candidate,
        }
    }
}

impl types::ScannedCandidate {
    pub fn recent_failure_count(&self) -> u64 {
        self.saved_network_info
            .recent_failures
            .iter()
            .filter(|failure| failure.bssid == self.bss.bssid)
            .count()
            .try_into()
            .unwrap_or_else(|e| {
                error!("{}", e);
                u64::MAX
            })
    }
    pub fn recent_short_connections(&self) -> usize {
        self.saved_network_info
            .past_connections
            .get_list_for_bss(&self.bss.bssid)
            .get_recent(fasync::Time::now() - RECENT_DISCONNECT_WINDOW)
            .iter()
            .filter(|d| d.connection_uptime < SHORT_CONNECT_DURATION)
            .collect::<Vec<_>>()
            .len()
    }

    pub fn saved_security_type_to_string(&self) -> String {
        match self.network.security_type {
            SecurityType::None => "open",
            SecurityType::Wep => "WEP",
            SecurityType::Wpa => "WPA1",
            SecurityType::Wpa2 => "WPA2",
            SecurityType::Wpa3 => "WPA3",
        }
        .to_string()
    }

    pub fn scanned_security_type_to_string(&self) -> String {
        match self.security_type_detailed {
            SecurityTypeDetailed::Unknown => "unknown",
            SecurityTypeDetailed::Open => "open",
            SecurityTypeDetailed::Wep => "WEP",
            SecurityTypeDetailed::Wpa1 => "WPA1",
            SecurityTypeDetailed::Wpa1Wpa2PersonalTkipOnly => "WPA1/2Tk",
            SecurityTypeDetailed::Wpa2PersonalTkipOnly => "WPA2Tk",
            SecurityTypeDetailed::Wpa1Wpa2Personal => "WPA1/2",
            SecurityTypeDetailed::Wpa2Personal => "WPA2",
            SecurityTypeDetailed::Wpa2Wpa3Personal => "WPA2/3",
            SecurityTypeDetailed::Wpa3Personal => "WPA3",
            SecurityTypeDetailed::Wpa2Enterprise => "WPA2Ent",
            SecurityTypeDetailed::Wpa3Enterprise => "WPA3Ent",
        }
        .to_string()
    }

    pub fn to_string_without_pii(&self) -> String {
        let channel = self.bss.channel;
        let rssi = self.bss.rssi;
        let recent_failure_count = self.recent_failure_count();
        let recent_short_connection_count = self.recent_short_connections();

        format!(
            "{}({:4}), {}({:6}), {:>4}dBm, channel {:8}, score {:4}{}{}{}{}",
            self.hasher.hash_ssid(&self.network.ssid),
            self.saved_security_type_to_string(),
            self.hasher.hash_mac_addr(&self.bss.bssid.0),
            self.scanned_security_type_to_string(),
            rssi,
            channel,
            scoring_functions::score_bss_scanned_candidate(self.clone()),
            if !self.bss.is_compatible() { ", NOT compatible" } else { "" },
            if recent_failure_count > 0 {
                format!(", {} recent failures", recent_failure_count)
            } else {
                "".to_string()
            },
            if recent_short_connection_count > 0 {
                format!(", {} recent short disconnects", recent_short_connection_count)
            } else {
                "".to_string()
            },
            if !self.saved_network_info.has_ever_connected { ", never used yet" } else { "" },
        )
    }
}

impl WriteInspect for types::ScannedCandidate {
    fn write_inspect(&self, writer: &InspectNode, key: impl Into<StringReference>) {
        inspect_insert!(writer, var key: {
            ssid_hash: self.hasher.hash_ssid(&self.network.ssid),
            bssid_hash: self.hasher.hash_mac_addr(&self.bss.bssid.0),
            rssi: self.bss.rssi,
            score: scoring_functions::score_bss_scanned_candidate(self.clone()),
            security_type_saved: self.saved_security_type_to_string(),
            security_type_scanned: format!("{}", wlan_common::bss::Protection::from(self.security_type_detailed)),
            channel: InspectWlanChan(&self.bss.channel.into()),
            compatible: self.bss.is_compatible(),
            recent_failure_count: self.recent_failure_count(),
            saved_network_has_ever_connected: self.saved_network_info.has_ever_connected,
        });
    }
}

fn get_authenticator(bss: &Bss, credential: &Credential) -> Option<SecurityAuthenticator> {
    let mutual_security_protocols = match bss.compatibility.as_ref() {
        Some(compatibility) => compatibility.mutual_security_protocols().clone(),
        None => {
            error!("BSS ({:?}) lacks compatibility information", bss.bssid.clone());
            return None;
        }
    };

    match select_authentication_method(mutual_security_protocols.clone(), credential) {
        Some(authenticator) => Some(authenticator),
        None => {
            error!(
                "Failed to negotiate authentication for BSS ({:?}) with mutually supported
                security protocols: {:?}, and credential type: {:?}.",
                bss.bssid,
                mutual_security_protocols.clone(),
                credential.type_str()
            );
            None
        }
    }
}

/// Merge the saved networks and scan results into a vector of BSS candidates that correspond to a
/// saved network.
async fn merge_saved_networks_and_scan_data(
    saved_network_manager: &Arc<dyn SavedNetworksManagerApi>,
    mut scan_results: Vec<types::ScanResult>,
    hasher: &WlanHasher,
) -> Vec<types::ScannedCandidate> {
    let mut merged_networks = vec![];
    for mut scan_result in scan_results.drain(..) {
        for saved_config in saved_network_manager
            .lookup_compatible(&scan_result.ssid, scan_result.security_type_detailed)
            .await
        {
            let multiple_bss_candidates = scan_result.entries.len() > 1;
            for bss in scan_result.entries.drain(..) {
                let authenticator = match get_authenticator(&bss, &saved_config.credential) {
                    Some(authenticator) => authenticator,
                    None => {
                        error!("Failed to create authenticator for bss candidate {:?} (SSID: {:?}). Removing from candidates.", bss.bssid, hasher.hash_ssid(&saved_config.ssid));
                        continue;
                    }
                };
                let scanned_candidate = types::ScannedCandidate {
                    network: types::NetworkIdentifier {
                        ssid: saved_config.ssid.clone(),
                        security_type: saved_config.security_type,
                    },
                    security_type_detailed: scan_result.security_type_detailed,
                    credential: saved_config.credential.clone(),
                    network_has_multiple_bss: multiple_bss_candidates,
                    saved_network_info: InternalSavedNetworkData {
                        has_ever_connected: saved_config.has_ever_connected,
                        recent_failures: saved_config
                            .perf_stats
                            .connect_failures
                            .get_recent_for_network(fasync::Time::now() - RECENT_FAILURE_WINDOW),
                        past_connections: saved_config.perf_stats.past_connections.clone(),
                    },
                    bss,
                    hasher: hasher.clone(),
                    authenticator,
                };
                merged_networks.push(scanned_candidate)
            }
        }
    }
    merged_networks
}

fn record_metrics_on_scan(
    mut merged_networks: Vec<types::ScannedCandidate>,
    telemetry_sender: &TelemetrySender,
) {
    let mut metric_events = vec![];
    let mut merged_network_map: HashMap<types::NetworkIdentifier, Vec<types::ScannedCandidate>> =
        HashMap::new();
    for bss in merged_networks.drain(..) {
        merged_network_map.entry(bss.network.clone()).or_default().push(bss);
    }

    let num_saved_networks_observed = merged_network_map.len();
    let mut num_actively_scanned_networks = 0;
    for (_network_id, bsss) in merged_network_map {
        // Record how many BSSs are visible in the scan results for this saved network.
        let num_bss = match bsss.len() {
            0 => unreachable!(), // The ::Zero enum exists, but we shouldn't get a scan result with no BSS
            1 => SavedNetworkInScanResultMigratedMetricDimensionBssCount::One,
            2..=4 => SavedNetworkInScanResultMigratedMetricDimensionBssCount::TwoToFour,
            5..=10 => SavedNetworkInScanResultMigratedMetricDimensionBssCount::FiveToTen,
            11..=20 => SavedNetworkInScanResultMigratedMetricDimensionBssCount::ElevenToTwenty,
            21..=usize::MAX => {
                SavedNetworkInScanResultMigratedMetricDimensionBssCount::TwentyOneOrMore
            }
            _ => unreachable!(),
        };
        metric_events.push(MetricEvent {
            metric_id: SAVED_NETWORK_IN_SCAN_RESULT_MIGRATED_METRIC_ID,
            event_codes: vec![num_bss as u32],
            payload: MetricEventPayload::Count(1),
        });

        // Check if the network was found via active scan.
        if bsss.iter().any(|bss| matches!(bss.bss.observation, types::ScanObservation::Active)) {
            num_actively_scanned_networks += 1;
        };
    }

    let saved_network_count_metric = match num_saved_networks_observed {
        0 => ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::Zero,
        1 => ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::One,
        2..=4 => ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::TwoToFour,
        5..=20 => ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::FiveToTwenty,
        21..=40 => ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::TwentyOneToForty,
        41..=usize::MAX => {
            ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::FortyOneOrMore
        }
        _ => unreachable!(),
    };
    metric_events.push(MetricEvent {
        metric_id: SCAN_RESULTS_RECEIVED_MIGRATED_METRIC_ID,
        event_codes: vec![saved_network_count_metric as u32],
        payload: MetricEventPayload::Count(1),
    });

    let actively_scanned_networks_metrics = match num_actively_scanned_networks {
        0 => ActiveScanSsidsObserved::Zero,
        1 => ActiveScanSsidsObserved::One,
        2..=4 => ActiveScanSsidsObserved::TwoToFour,
        5..=10 => ActiveScanSsidsObserved::FiveToTen,
        11..=20 => ActiveScanSsidsObserved::ElevenToTwenty,
        21..=50 => ActiveScanSsidsObserved::TwentyOneToFifty,
        51..=100 => ActiveScanSsidsObserved::FiftyOneToOneHundred,
        101..=usize::MAX => ActiveScanSsidsObserved::OneHundredAndOneOrMore,
        _ => unreachable!(),
    };
    metric_events.push(MetricEvent {
        metric_id: SAVED_NETWORK_IN_SCAN_RESULT_WITH_ACTIVE_SCAN_MIGRATED_METRIC_ID,
        event_codes: vec![actively_scanned_networks_metrics as u32],
        payload: MetricEventPayload::Count(1),
    });

    telemetry_sender.send(TelemetryEvent::LogMetricEvents {
        events: metric_events,
        ctx: "connection_selection::record_metrics_on_scan",
    });
}
#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            config_management::{
                network_config::HistoricalListsByBssid, ConnectFailure, FailureReason,
                SavedNetworksManager,
            },
            util::testing::{
                create_inspect_persistence_channel, create_wlan_hasher,
                fakes::{FakeSavedNetworksManager, FakeScanRequester},
                generate_channel, generate_random_bss, generate_random_connect_reason,
                generate_random_scan_result, generate_random_scanned_candidate,
            },
        },
        fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
        fuchsia_async as fasync,
        fuchsia_inspect::{self as inspect, assert_data_tree},
        futures::{channel::mpsc, task::Poll},
        lazy_static::lazy_static,
        pin_utils::pin_mut,
        rand::Rng,
        std::{convert::TryFrom, sync::Arc},
        test_case::test_case,
        wlan_common::{
            assert_variant, random_fidl_bss_description, scan::Compatibility,
            security::SecurityDescriptor,
        },
    };

    lazy_static! {
        pub static ref TEST_PASSWORD: Credential = Credential::Password(b"password".to_vec());
    }

    struct TestValues {
        connection_selector: Arc<ConnectionSelector>,
        real_saved_network_manager: Arc<dyn SavedNetworksManagerApi>,
        saved_network_manager: Arc<FakeSavedNetworksManager>,
        scan_requester: Arc<FakeScanRequester>,
        inspector: inspect::Inspector,
        telemetry_receiver: mpsc::Receiver<TelemetryEvent>,
    }

    async fn test_setup(use_real_save_network_manager: bool) -> TestValues {
        let real_saved_network_manager =
            Arc::new(SavedNetworksManager::new_for_test().await.unwrap());
        let saved_network_manager = Arc::new(FakeSavedNetworksManager::new());
        let scan_requester = Arc::new(FakeScanRequester::new());
        let inspector = inspect::Inspector::default();
        let inspect_node = inspector.root().create_child("connection_selection_test");
        let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);

        let connection_selector = Arc::new(ConnectionSelector::new(
            if use_real_save_network_manager {
                real_saved_network_manager.clone()
            } else {
                saved_network_manager.clone()
            },
            scan_requester.clone(),
            create_wlan_hasher(),
            inspect_node,
            persistence_req_sender,
            TelemetrySender::new(telemetry_sender),
        ));

        TestValues {
            connection_selector,
            real_saved_network_manager,
            saved_network_manager,
            scan_requester,
            inspector,
            telemetry_receiver,
        }
    }

    fn fake_successful_connect_result() -> fidl_sme::ConnectResult {
        fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        }
    }

    #[fuchsia::test]
    async fn scan_results_merged_with_saved_networks() {
        let test_values = test_setup(true).await;

        // create some identifiers
        let test_ssid_1 = types::Ssid::try_from("foo").unwrap();
        let test_security_1 = types::SecurityTypeDetailed::Wpa3Personal;
        let test_id_1 = types::NetworkIdentifier {
            ssid: test_ssid_1.clone(),
            security_type: types::SecurityType::Wpa3,
        };
        let credential_1 = Credential::Password("foo_pass".as_bytes().to_vec());
        let test_ssid_2 = types::Ssid::try_from("bar").unwrap();
        let test_security_2 = types::SecurityTypeDetailed::Wpa1;
        let test_id_2 = types::NetworkIdentifier {
            ssid: test_ssid_2.clone(),
            security_type: types::SecurityType::Wpa,
        };
        let credential_2 = Credential::Password("bar_pass".as_bytes().to_vec());

        // insert the saved networks
        assert!(test_values
            .real_saved_network_manager
            .store(test_id_1.clone(), credential_1.clone())
            .await
            .unwrap()
            .is_none());

        assert!(test_values
            .real_saved_network_manager
            .store(test_id_2.clone(), credential_2.clone())
            .await
            .unwrap()
            .is_none());

        // build some scan results
        let mock_scan_results = vec![
            types::ScanResult {
                ssid: test_ssid_1.clone(),
                security_type_detailed: test_security_1,
                entries: vec![
                    types::Bss {
                        compatibility: Compatibility::expect_some([
                            SecurityDescriptor::WPA3_PERSONAL,
                        ]),
                        ..generate_random_bss()
                    },
                    types::Bss {
                        compatibility: Compatibility::expect_some([
                            SecurityDescriptor::WPA3_PERSONAL,
                        ]),
                        ..generate_random_bss()
                    },
                    types::Bss {
                        compatibility: Compatibility::expect_some([
                            SecurityDescriptor::WPA3_PERSONAL,
                        ]),
                        ..generate_random_bss()
                    },
                ],
                compatibility: types::Compatibility::Supported,
            },
            types::ScanResult {
                ssid: test_ssid_2.clone(),
                security_type_detailed: test_security_2,
                entries: vec![types::Bss {
                    compatibility: Compatibility::expect_some([SecurityDescriptor::WPA1]),
                    ..generate_random_bss()
                }],
                compatibility: types::Compatibility::DisallowedNotSupported,
            },
        ];

        let bssid_1 = mock_scan_results[0].entries[0].bssid;
        let bssid_2 = mock_scan_results[0].entries[1].bssid;

        // mark the first one as having connected
        test_values
            .real_saved_network_manager
            .record_connect_result(
                test_id_1.clone(),
                &credential_1.clone(),
                bssid_1,
                fake_successful_connect_result(),
                types::ScanObservation::Unknown,
            )
            .await;

        // mark the second one as having a failure
        test_values
            .real_saved_network_manager
            .record_connect_result(
                test_id_1.clone(),
                &credential_1.clone(),
                bssid_2,
                fidl_sme::ConnectResult {
                    code: fidl_ieee80211::StatusCode::RefusedReasonUnspecified,
                    is_credential_rejected: true,
                    ..fake_successful_connect_result()
                },
                types::ScanObservation::Unknown,
            )
            .await;

        // build our expected result
        let failure_time = test_values
            .real_saved_network_manager
            .lookup(&test_id_1.clone())
            .await
            .get(0)
            .expect("failed to get config")
            .perf_stats
            .connect_failures
            .get_recent_for_network(fasync::Time::now() - RECENT_FAILURE_WINDOW)
            .get(0)
            .expect("failed to get recent failure")
            .time;
        let recent_failures = vec![ConnectFailure {
            bssid: bssid_2,
            time: failure_time,
            reason: FailureReason::CredentialRejected,
        }];
        let expected_internal_data_1 = InternalSavedNetworkData {
            has_ever_connected: true,
            recent_failures: recent_failures.clone(),
            past_connections: HistoricalListsByBssid::new(),
        };
        let hasher = create_wlan_hasher();
        let wpa3_authenticator = select_authentication_method(
            HashSet::from([SecurityDescriptor::WPA3_PERSONAL]),
            &credential_1,
        )
        .unwrap();
        let open_authenticator =
            select_authentication_method(HashSet::from([SecurityDescriptor::WPA1]), &credential_2)
                .unwrap();
        let expected_results = vec![
            types::ScannedCandidate {
                network: test_id_1.clone(),
                credential: credential_1.clone(),
                network_has_multiple_bss: true,
                security_type_detailed: test_security_1,
                saved_network_info: expected_internal_data_1.clone(),
                bss: mock_scan_results[0].entries[0].clone(),
                authenticator: wpa3_authenticator.clone(),
                hasher: hasher.clone(),
            },
            types::ScannedCandidate {
                network: test_id_1.clone(),
                credential: credential_1.clone(),
                network_has_multiple_bss: true,
                security_type_detailed: test_security_1,
                saved_network_info: expected_internal_data_1.clone(),
                bss: mock_scan_results[0].entries[1].clone(),
                authenticator: wpa3_authenticator.clone(),
                hasher: hasher.clone(),
            },
            types::ScannedCandidate {
                network: test_id_1.clone(),
                credential: credential_1.clone(),
                network_has_multiple_bss: true,
                security_type_detailed: test_security_1,
                saved_network_info: expected_internal_data_1.clone(),
                bss: mock_scan_results[0].entries[2].clone(),
                authenticator: wpa3_authenticator.clone(),
                hasher: hasher.clone(),
            },
            types::ScannedCandidate {
                network: test_id_2.clone(),
                credential: credential_2.clone(),
                network_has_multiple_bss: false,
                security_type_detailed: test_security_2,
                saved_network_info: InternalSavedNetworkData {
                    has_ever_connected: false,
                    recent_failures: Vec::new(),
                    past_connections: HistoricalListsByBssid::new(),
                },
                bss: mock_scan_results[1].entries[0].clone(),
                authenticator: open_authenticator.clone(),
                hasher: hasher.clone(),
            },
        ];

        // validate the function works
        let results = merge_saved_networks_and_scan_data(
            &test_values.real_saved_network_manager,
            mock_scan_results,
            &hasher,
        )
        .await;

        assert_eq!(results, expected_results);
    }

    #[fuchsia::test]
    fn augment_bss_candidate_with_active_scan_doesnt_run_on_actively_found_networks() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let mut candidate = generate_random_scanned_candidate();
        candidate.bss.observation = types::ScanObservation::Active;

        let fut = test_values
            .connection_selector
            .augment_bss_candidate_with_active_scan(candidate.clone());
        pin_mut!(fut);

        // The connect_req comes out the other side with no change
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(res) => {
            assert_eq!(&res, &candidate);
        });
    }

    #[fuchsia::test]
    fn augment_bss_candidate_with_active_scan_runs_on_passively_found_networks() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));

        let mut passively_scanned_candidate = generate_random_scanned_candidate();
        passively_scanned_candidate.bss.observation = types::ScanObservation::Passive;

        let fut = test_values
            .connection_selector
            .augment_bss_candidate_with_active_scan(passively_scanned_candidate.clone());
        pin_mut!(fut);

        // Set the scan results
        let new_bss_desc = random_fidl_bss_description!();
        exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
            types::ScanResult {
                ssid: passively_scanned_candidate.network.ssid.clone(),
                security_type_detailed: passively_scanned_candidate.security_type_detailed,
                compatibility: types::Compatibility::Supported,
                entries: vec![types::Bss {
                    bssid: passively_scanned_candidate.bss.bssid,
                    compatibility: wlan_common::scan::Compatibility::expect_some([
                        wlan_common::security::SecurityDescriptor::WPA1,
                    ]),
                    bss_description: new_bss_desc.clone().into(),
                    ..generate_random_bss()
                }],
            },
        ])));

        let candidate = exec.run_singlethreaded(fut);
        // The connect_req comes out the other side with the new bss_description
        assert_eq!(
            &candidate,
            &types::ScannedCandidate {
                bss: types::Bss {
                    bss_description: new_bss_desc.into(),
                    ..passively_scanned_candidate.bss.clone()
                },
                ..passively_scanned_candidate.clone()
            },
        );

        // Check the right scan request was sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![(
                ScanReason::BssSelectionAugmentation,
                vec![passively_scanned_candidate.network.ssid.clone()],
                vec![passively_scanned_candidate.bss.channel]
            )]
        );
    }

    #[fuchsia::test]
    fn find_available_bss_list_with_network_specified() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(false));
        let connection_selector = test_values.connection_selector;

        // create identifiers
        let test_id_1 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };
        let credential_1 = Credential::Password("foo_pass".as_bytes().to_vec());

        // insert saved networks
        assert!(exec
            .run_singlethreaded(
                test_values.saved_network_manager.store(test_id_1.clone(), credential_1.clone()),
            )
            .unwrap()
            .is_none());

        // Prep the scan results
        let mutual_security_protocols_1 = [SecurityDescriptor::WPA3_PERSONAL];
        let bss_desc_1 = random_fidl_bss_description!();
        exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
            types::ScanResult {
                ssid: test_id_1.ssid.clone(),
                security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                compatibility: types::Compatibility::Supported,
                entries: vec![types::Bss {
                    compatibility: wlan_common::scan::Compatibility::expect_some(
                        mutual_security_protocols_1,
                    ),
                    bss_description: bss_desc_1.clone().into(),
                    ..generate_random_bss()
                }],
            },
            generate_random_scan_result(),
            generate_random_scan_result(),
        ])));

        // Run the scan, specifying the desired network
        let fut = connection_selector.find_available_bss_candidate_list(Some(test_id_1.clone()));
        pin_mut!(fut);
        let results = exec.run_singlethreaded(&mut fut);
        assert_eq!(results.len(), 1);

        // Check that the right scan request was sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![(ScanReason::BssSelection, vec![test_id_1.ssid.clone()], vec![])]
        );
    }

    #[test_case(true)]
    #[test_case(false)]
    #[fuchsia::test(add_test_attr = false)]
    fn find_available_bss_list_without_network_specified(hidden: bool) {
        let mut exec = fasync::TestExecutor::new();
        let mut test_values = exec.run_singlethreaded(test_setup(false));
        let connection_selector = test_values.connection_selector;

        // create identifiers
        let test_id_not_hidden = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };
        let test_id_maybe_hidden = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("bar").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };
        let test_id_hidden_but_seen = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("baz").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };
        let credential = Credential::Password("some_pass".as_bytes().to_vec());

        // insert saved networks
        assert!(exec
            .run_singlethreaded(
                test_values
                    .saved_network_manager
                    .store(test_id_not_hidden.clone(), credential.clone()),
            )
            .unwrap()
            .is_none());
        assert!(exec
            .run_singlethreaded(
                test_values
                    .saved_network_manager
                    .store(test_id_maybe_hidden.clone(), credential.clone()),
            )
            .unwrap()
            .is_none());
        assert!(exec
            .run_singlethreaded(
                test_values
                    .saved_network_manager
                    .store(test_id_hidden_but_seen.clone(), credential.clone()),
            )
            .unwrap()
            .is_none());

        // Set the hidden probability for test_id_not_hidden and test_id_hidden_but_seen
        exec.run_singlethreaded(
            test_values.saved_network_manager.update_hidden_prob(test_id_not_hidden.clone(), 0.0),
        );
        exec.run_singlethreaded(
            test_values
                .saved_network_manager
                .update_hidden_prob(test_id_hidden_but_seen.clone(), 1.0),
        );
        // Set the hidden probability for test_id_maybe_hidden based on test variant
        exec.run_singlethreaded(
            test_values
                .saved_network_manager
                .update_hidden_prob(test_id_maybe_hidden.clone(), if hidden { 1.0 } else { 0.0 }),
        );

        // Prep the scan results
        let mutual_security_protocols = [SecurityDescriptor::WPA3_PERSONAL];
        let bss_desc = random_fidl_bss_description!();
        if hidden {
            // Initial passive scan has non-hidden result and hidden-but-seen result
            exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
                types::ScanResult {
                    ssid: test_id_not_hidden.ssid.clone(),
                    security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(
                            mutual_security_protocols,
                        ),
                        bss_description: bss_desc.clone().into(),
                        ..generate_random_bss()
                    }],
                },
                types::ScanResult {
                    ssid: test_id_hidden_but_seen.ssid.clone(),
                    security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(
                            mutual_security_protocols,
                        ),
                        bss_description: bss_desc.clone().into(),
                        ..generate_random_bss()
                    }],
                },
                generate_random_scan_result(),
                generate_random_scan_result(),
            ])));
            // Next active scan has hidden result
            exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
                types::ScanResult {
                    ssid: test_id_maybe_hidden.ssid.clone(),
                    security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(
                            mutual_security_protocols,
                        ),
                        bss_description: bss_desc.clone().into(),
                        ..generate_random_bss()
                    }],
                },
                generate_random_scan_result(),
                generate_random_scan_result(),
            ])));
        } else {
            // Non-hidden test case, only one scan, return all results
            exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
                types::ScanResult {
                    ssid: test_id_not_hidden.ssid.clone(),
                    security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(
                            mutual_security_protocols,
                        ),
                        bss_description: bss_desc.clone().into(),
                        ..generate_random_bss()
                    }],
                },
                types::ScanResult {
                    ssid: test_id_maybe_hidden.ssid.clone(),
                    security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(
                            mutual_security_protocols,
                        ),
                        bss_description: bss_desc.clone().into(),
                        ..generate_random_bss()
                    }],
                },
                types::ScanResult {
                    ssid: test_id_hidden_but_seen.ssid.clone(),
                    security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                    compatibility: types::Compatibility::Supported,
                    entries: vec![types::Bss {
                        compatibility: wlan_common::scan::Compatibility::expect_some(
                            mutual_security_protocols,
                        ),
                        bss_description: bss_desc.clone().into(),
                        ..generate_random_bss()
                    }],
                },
                generate_random_scan_result(),
                generate_random_scan_result(),
            ])));
        }

        // Run the scan(s)
        let connection_selection_fut = connection_selector.find_available_bss_candidate_list(None);
        pin_mut!(connection_selection_fut);
        let results = exec.run_singlethreaded(&mut connection_selection_fut);
        assert_eq!(results.len(), 3);

        // Check that the right scan request(s) were sent. The hidden-but-seen SSID should never
        // be requested for the active scan.
        if hidden {
            assert_eq!(
                *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
                vec![
                    (ScanReason::NetworkSelection, vec![], vec![]),
                    (ScanReason::NetworkSelection, vec![test_id_maybe_hidden.ssid.clone()], vec![])
                ]
            )
        } else {
            assert_eq!(
                *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
                vec![(ScanReason::NetworkSelection, vec![], vec![])]
            )
        }

        // Check that the metrics were logged
        assert_variant!(
            test_values.telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ActiveScanRequested{num_ssids_requested})) => {
                if hidden {
                        assert_eq!(num_ssids_requested, 1);
                } else {
                        assert_eq!(num_ssids_requested, 0);
                }
        });
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_scan_error() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let connection_selector = test_values.connection_selector;
        let mut telemetry_receiver = test_values.telemetry_receiver;

        // Return an error on the scan
        exec.run_singlethreaded(
            test_values.scan_requester.add_scan_result(Err(types::ScanError::GeneralError)),
        );

        // Kick off network selection
        let connection_selection_fut = connection_selector
            .find_and_select_scanned_candidate(None, generate_random_connect_reason());
        pin_mut!(connection_selection_fut);
        // Check that nothing is returned
        assert_variant!(exec.run_until_stalled(&mut connection_selection_fut), Poll::Ready(None));

        // Check that the right scan request was sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![(scan::ScanReason::NetworkSelection, vec![], vec![])]
        );

        // Check the network selections were logged
        assert_data_tree!(test_values.inspector, root: {
            connection_selection_test: {
                connection_selection: {
                    "0": {
                        "@time": inspect::testing::AnyProperty,
                        "candidates": {},
                    },
                }
            },
        });

        // Verify TelemetryEvent for network selection was sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::NetworkSelectionDecision {
                network_selection_type: telemetry::NetworkSelectionType::Undirected,
                num_candidates: Err(()),
                selected_count: 0,
            });
        });
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { selected_candidate: None, .. }))
        );
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_end_to_end() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let connection_selector = test_values.connection_selector;
        let mut telemetry_receiver = test_values.telemetry_receiver;

        // create some identifiers
        let test_id_1 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };
        let credential_1 = Credential::Password("foo_pass".as_bytes().to_vec());
        let bssid_1 = types::Bssid([1, 1, 1, 1, 1, 1]);

        let test_id_2 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("bar").unwrap(),
            security_type: types::SecurityType::Wpa,
        };
        let credential_2 = Credential::Password("bar_pass".as_bytes().to_vec());
        let bssid_2 = types::Bssid([2, 2, 2, 2, 2, 2]);

        // insert some new saved networks
        assert!(exec
            .run_singlethreaded(
                test_values
                    .real_saved_network_manager
                    .store(test_id_1.clone(), credential_1.clone()),
            )
            .unwrap()
            .is_none());
        assert!(exec
            .run_singlethreaded(
                test_values
                    .real_saved_network_manager
                    .store(test_id_2.clone(), credential_2.clone()),
            )
            .unwrap()
            .is_none());

        // Mark them as having connected. Make connection passive so that no active scans are made.
        exec.run_singlethreaded(test_values.real_saved_network_manager.record_connect_result(
            test_id_1.clone(),
            &credential_1.clone(),
            bssid_1,
            fake_successful_connect_result(),
            types::ScanObservation::Passive,
        ));
        exec.run_singlethreaded(test_values.real_saved_network_manager.record_connect_result(
            test_id_2.clone(),
            &credential_2.clone(),
            bssid_2,
            fake_successful_connect_result(),
            types::ScanObservation::Passive,
        ));

        // Prep the scan results
        let mutual_security_protocols_1 = [SecurityDescriptor::WPA3_PERSONAL];
        let channel_1 = generate_channel(3);
        let mutual_security_protocols_2 = [SecurityDescriptor::WPA1];
        let channel_2 = generate_channel(50);
        let mock_passive_scan_results = vec![
            types::ScanResult {
                ssid: test_id_1.ssid.clone(),
                security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                compatibility: types::Compatibility::Supported,
                entries: vec![types::Bss {
                    compatibility: wlan_common::scan::Compatibility::expect_some(
                        mutual_security_protocols_1,
                    ),
                    bssid: bssid_1,
                    channel: channel_1,
                    rssi: 20, // much higher than other result
                    observation: types::ScanObservation::Passive,
                    ..generate_random_bss()
                }],
            },
            types::ScanResult {
                ssid: test_id_2.ssid.clone(),
                security_type_detailed: types::SecurityTypeDetailed::Wpa1,
                compatibility: types::Compatibility::Supported,
                entries: vec![types::Bss {
                    compatibility: wlan_common::scan::Compatibility::expect_some(
                        mutual_security_protocols_2,
                    ),
                    bssid: bssid_2,
                    channel: channel_2,
                    rssi: -100, // much lower than other result
                    observation: types::ScanObservation::Passive,
                    ..generate_random_bss()
                }],
            },
            generate_random_scan_result(),
            generate_random_scan_result(),
        ];

        // Initial passive scan
        exec.run_singlethreaded(
            test_values.scan_requester.add_scan_result(Ok(mock_passive_scan_results.clone())),
        );

        // An additional directed active scan should be made for the selected network
        let bss_desc1_active = random_fidl_bss_description!();
        let new_bss = types::Bss {
            compatibility: wlan_common::scan::Compatibility::expect_some(
                mutual_security_protocols_1,
            ),
            bssid: bssid_1,
            bss_description: bss_desc1_active.clone().into(),
            ..generate_random_bss()
        };
        exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
            types::ScanResult {
                ssid: test_id_1.ssid.clone(),
                security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                compatibility: types::Compatibility::Supported,
                entries: vec![new_bss.clone()],
            },
            generate_random_scan_result(),
        ])));

        // Check that we pick a network
        let connection_selection_fut = connection_selector
            .find_and_select_scanned_candidate(None, generate_random_connect_reason());
        pin_mut!(connection_selection_fut);
        let results =
            exec.run_singlethreaded(&mut connection_selection_fut).expect("no selected candidate");
        assert_eq!(&results.network, &test_id_1.clone());
        assert_eq!(
            &results.bss,
            &types::Bss {
                bss_description: bss_desc1_active.clone().into(),
                ..mock_passive_scan_results[0].entries[0].clone()
            }
        );

        // Check that the right scan requests were sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![
                // Initial passive scan
                (ScanReason::NetworkSelection, vec![], vec![]),
                // Directed active scan should be made for the selected network
                (
                    ScanReason::BssSelectionAugmentation,
                    vec![test_id_1.ssid.clone()],
                    vec![channel_1]
                )
            ]
        );

        // Check the network selections were logged
        assert_data_tree!(test_values.inspector, root: {
            connection_selection_test: {
                connection_selection: {
                    "0": {
                        "@time": inspect::testing::AnyProperty,
                        "candidates": {
                            "0": contains {
                                bssid_hash: inspect::testing::AnyProperty,
                                score: inspect::testing::AnyProperty,
                            },
                            "1": contains {
                                bssid_hash: inspect::testing::AnyProperty,
                                score: inspect::testing::AnyProperty,
                            },
                        },
                        "selected": contains {
                            bssid_hash: inspect::testing::AnyProperty,
                            score: inspect::testing::AnyProperty,
                        },
                    },
                }
            },
        });

        // Verify TelemetryEvents for network selection were sent
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::ActiveScanRequested { num_ssids_requested: 0 }))
        );
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::LogMetricEvents {
                ctx: "connection_selection::record_metrics_on_scan",
                ..
            }))
        );
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::NetworkSelectionDecision {
                network_selection_type: telemetry::NetworkSelectionType::Undirected,
                num_candidates: Ok(2),
                selected_count: 2,
            });
        });
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { .. }))
        );
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_with_network_end_to_end() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let connection_selector = test_values.connection_selector;
        let mut telemetry_receiver = test_values.telemetry_receiver;

        // create identifiers
        let test_id_1 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };

        // insert saved networks
        assert!(exec
            .run_singlethreaded(
                test_values
                    .real_saved_network_manager
                    .store(test_id_1.clone(), TEST_PASSWORD.clone()),
            )
            .unwrap()
            .is_none());

        // Prep the scan results
        let mutual_security_protocols_1 = [SecurityDescriptor::WPA3_PERSONAL];
        let bss_desc_1 = random_fidl_bss_description!();
        let scanned_bss = types::Bss {
            // This network is WPA3, but should still match against the desired WPA2 network
            compatibility: wlan_common::scan::Compatibility::expect_some(
                mutual_security_protocols_1,
            ),
            bss_description: bss_desc_1.clone().into(),
            // Observation should be unknown, since we didn't try a passive scan.
            observation: types::ScanObservation::Unknown,
            ..generate_random_bss()
        };
        exec.run_singlethreaded(test_values.scan_requester.add_scan_result(Ok(vec![
            types::ScanResult {
                ssid: test_id_1.ssid.clone(),
                // This network is WPA3, but should still match against the desired WPA2 network
                security_type_detailed: types::SecurityTypeDetailed::Wpa3Personal,
                compatibility: types::Compatibility::Supported,
                entries: vec![scanned_bss.clone()],
            },
            generate_random_scan_result(),
            generate_random_scan_result(),
        ])));

        // Run network selection
        let connection_selection_fut = connection_selector.find_and_select_scanned_candidate(
            Some(test_id_1.clone()),
            generate_random_connect_reason(),
        );
        pin_mut!(connection_selection_fut);
        let results =
            exec.run_singlethreaded(&mut connection_selection_fut).expect("no selected candidate");
        assert_eq!(&results.network, &test_id_1);
        assert_eq!(&results.security_type_detailed, &types::SecurityTypeDetailed::Wpa3Personal);
        assert_eq!(&results.credential, &TEST_PASSWORD.clone());
        assert_eq!(&results.bss, &scanned_bss);

        // Check that the right scan request was sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![(ScanReason::BssSelection, vec![test_id_1.ssid.clone()], vec![])]
        );
        // Verify that NetworkSelectionDecision telemetry event is sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::NetworkSelectionDecision {
                network_selection_type: telemetry::NetworkSelectionType::Directed,
                num_candidates: Ok(1),
                selected_count: 1,
            });
        });
        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { .. }))
        );
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_with_network_end_to_end_with_failure() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let connection_selector = test_values.connection_selector;
        let mut telemetry_receiver = test_values.telemetry_receiver;

        // create identifiers
        let test_id_1 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };

        // Return an error on the scan
        exec.run_singlethreaded(
            test_values.scan_requester.add_scan_result(Err(types::ScanError::GeneralError)),
        );

        // Kick off network selection
        let connection_selection_fut = connection_selector.find_and_select_scanned_candidate(
            Some(test_id_1.clone()),
            generate_random_connect_reason(),
        );
        pin_mut!(connection_selection_fut);

        // Check that nothing is returned
        let results = exec.run_singlethreaded(&mut connection_selection_fut);
        assert_eq!(results, None);

        // Check that the right scan request was sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![(ScanReason::BssSelection, vec![test_id_1.ssid.clone()], vec![])]
        );

        // Verify that NetworkSelectionDecision telemetry event is sent
        assert_variant!(telemetry_receiver.try_next(), Ok(Some(event)) => {
            assert_variant!(event, TelemetryEvent::NetworkSelectionDecision {
                network_selection_type: telemetry::NetworkSelectionType::Directed,
                num_candidates: Err(()),
                selected_count: 1,
            });
        });

        assert_variant!(
            telemetry_receiver.try_next(),
            Ok(Some(TelemetryEvent::BssSelectionResult { .. }))
        );
    }

    #[fuchsia::test]
    async fn recorded_metrics_on_scan() {
        let (telemetry_sender, mut telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);

        // create some identifiers
        let test_id_1 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap().clone(),
            security_type: types::SecurityType::Wpa3,
        };

        let test_id_2 = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("bar").unwrap().clone(),
            security_type: types::SecurityType::Wpa,
        };

        let mut mock_scan_results = vec![];

        mock_scan_results.push(types::ScannedCandidate {
            network: test_id_1.clone(),
            bss: types::Bss {
                observation: types::ScanObservation::Passive,
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        });
        mock_scan_results.push(types::ScannedCandidate {
            network: test_id_1.clone(),
            bss: types::Bss {
                observation: types::ScanObservation::Passive,
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        });
        mock_scan_results.push(types::ScannedCandidate {
            network: test_id_1.clone(),
            bss: types::Bss {
                observation: types::ScanObservation::Active,
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        });
        mock_scan_results.push(types::ScannedCandidate {
            network: test_id_2.clone(),
            bss: types::Bss {
                observation: types::ScanObservation::Passive,
                ..generate_random_bss()
            },
            ..generate_random_scanned_candidate()
        });

        record_metrics_on_scan(mock_scan_results, &telemetry_sender);

        let metric_events = assert_variant!(telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::LogMetricEvents { events, .. })) => events);
        assert_eq!(metric_events.len(), 4);

        // The order of the first two cobalt events is not deterministic
        // Three BSSs present for network 1 in scan results
        assert!(metric_events[..2].iter().any(|event| *event
            == MetricEvent {
                metric_id: SAVED_NETWORK_IN_SCAN_RESULT_MIGRATED_METRIC_ID,
                event_codes: vec![
                    SavedNetworkInScanResultMigratedMetricDimensionBssCount::TwoToFour as u32
                ],
                payload: MetricEventPayload::Count(1),
            }));

        // One BSS present for network 2 in scan results
        assert!(metric_events[..2].iter().any(|event| *event
            == MetricEvent {
                metric_id: SAVED_NETWORK_IN_SCAN_RESULT_MIGRATED_METRIC_ID,
                event_codes: vec![
                    SavedNetworkInScanResultMigratedMetricDimensionBssCount::One as u32
                ],
                payload: MetricEventPayload::Count(1),
            }));

        // Total of two saved networks in the scan results
        assert_eq!(
            metric_events[2],
            MetricEvent {
                metric_id: SCAN_RESULTS_RECEIVED_MIGRATED_METRIC_ID,
                event_codes: vec![
                    ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::TwoToFour as u32
                ],
                payload: MetricEventPayload::Count(1),
            }
        );

        // One saved networks that was discovered via active scan
        assert_eq!(
            metric_events[3],
            MetricEvent {
                metric_id: SAVED_NETWORK_IN_SCAN_RESULT_WITH_ACTIVE_SCAN_MIGRATED_METRIC_ID,
                event_codes: vec![ActiveScanSsidsObserved::One as u32],
                payload: MetricEventPayload::Count(1),
            }
        );
    }

    #[fuchsia::test]
    async fn recorded_metrics_on_scan_no_saved_networks() {
        let (telemetry_sender, mut telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);
        let telemetry_sender = TelemetrySender::new(telemetry_sender);

        let mock_scan_results = vec![];

        record_metrics_on_scan(mock_scan_results, &telemetry_sender);

        let metric_events = assert_variant!(telemetry_receiver.try_next(), Ok(Some(TelemetryEvent::LogMetricEvents { events, .. })) => events);
        assert_eq!(metric_events.len(), 2);

        // No saved networks in scan results
        assert_eq!(
            metric_events[0],
            MetricEvent {
                metric_id: SCAN_RESULTS_RECEIVED_MIGRATED_METRIC_ID,
                event_codes: vec![
                    ScanResultsReceivedMigratedMetricDimensionSavedNetworksCount::Zero as u32
                ],
                payload: MetricEventPayload::Count(1),
            }
        );

        // Also no saved networks that were discovered via active scan
        assert_eq!(
            metric_events[1],
            MetricEvent {
                metric_id: SAVED_NETWORK_IN_SCAN_RESULT_WITH_ACTIVE_SCAN_MIGRATED_METRIC_ID,
                event_codes: vec![ActiveScanSsidsObserved::Zero as u32],
                payload: MetricEventPayload::Count(1),
            }
        );
    }
}
