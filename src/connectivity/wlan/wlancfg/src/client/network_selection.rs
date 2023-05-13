// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        client::{
            scan::{self, ScanReason},
            state_machine::PeriodicConnectionStats,
            types::{self, Bss, InternalSavedNetworkData, SecurityType, SecurityTypeDetailed},
        },
        config_management::{
            network_config::AddAndGetRecent, select_authentication_method,
            select_subset_potentially_hidden_networks, Credential, FailureReason,
            SavedNetworksManagerApi,
        },
        telemetry::{self, TelemetryEvent, TelemetrySender},
    },
    fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload},
    fidl_fuchsia_wlan_internal as fidl_internal, fuchsia_async as fasync,
    fuchsia_inspect::{Node as InspectNode, StringReference},
    fuchsia_inspect_contrib::{
        auto_persist::{self, AutoPersist},
        inspect_insert, inspect_log,
        log::{InspectList, WriteInspect},
        nodes::BoundedListNode as InspectBoundedListNode,
    },
    fuchsia_zircon as zx,
    futures::lock::Mutex,
    std::{
        collections::{HashMap, HashSet},
        sync::Arc,
    },
    tracing::{debug, error, info, trace, warn},
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

/// Above or at this RSSI, we'll give 5G networks a preference
const RSSI_CUTOFF_5G_PREFERENCE: i16 = -64;
/// The score boost for 5G networks that we are giving preference to.
const RSSI_5G_PREFERENCE_BOOST: i16 = 20;
/// The amount to decrease the score by for each failed connection attempt.
const SCORE_PENALTY_FOR_RECENT_FAILURE: i16 = 5;
/// This penalty is much higher than for a general failure because we are not likely to succeed
/// on a retry.
const SCORE_PENALTY_FOR_RECENT_CREDENTIAL_REJECTED: i16 = 30;
/// The amount to decrease the score for each time we are connected for only a short amount
/// of time before disconncting. This amount is the same as the penalty for 4 failed connect
/// attempts to a BSS.
const SCORE_PENALTY_FOR_SHORT_CONNECTION: i16 = 20;
/// Threshold for what we consider a short time to be connected
const SHORT_CONNECT_DURATION: zx::Duration = zx::Duration::from_seconds(7 * 60);

const RECENT_DISCONNECT_WINDOW: zx::Duration = zx::Duration::from_seconds(60 * 15); // 15 minutes
const RECENT_FAILURE_WINDOW: zx::Duration = zx::Duration::from_seconds(60 * 5); // 5 minutes
const INSPECT_EVENT_LIMIT_FOR_NETWORK_SELECTIONS: usize = 10;

pub struct NetworkSelector {
    saved_network_manager: Arc<dyn SavedNetworksManagerApi>,
    scan_requester: Arc<dyn scan::ScanRequestApi>,
    last_scan_result_time: Arc<Mutex<zx::Time>>,
    hasher: WlanHasher,
    _inspect_node_root: Arc<Mutex<InspectNode>>,
    inspect_node_for_network_selections: Arc<Mutex<AutoPersist<InspectBoundedListNode>>>,
    telemetry_sender: TelemetrySender,
}

impl NetworkSelector {
    pub fn new(
        saved_network_manager: Arc<dyn SavedNetworksManagerApi>,
        scan_requester: Arc<dyn scan::ScanRequestApi>,
        hasher: WlanHasher,
        inspect_node: InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
        telemetry_sender: TelemetrySender,
    ) -> Self {
        let inspect_node_for_network_selection = InspectBoundedListNode::new(
            inspect_node.create_child("network_selection"),
            INSPECT_EVENT_LIMIT_FOR_NETWORK_SELECTIONS,
        );
        let inspect_node_for_network_selection = AutoPersist::new(
            inspect_node_for_network_selection,
            "wlancfg-network-selection",
            persistence_req_sender.clone(),
        );
        Self {
            saved_network_manager,
            scan_requester,
            last_scan_result_time: Arc::new(Mutex::new(zx::Time::ZERO)),
            hasher,
            _inspect_node_root: Arc::new(Mutex::new(inspect_node)),
            inspect_node_for_network_selections: Arc::new(Mutex::new(
                inspect_node_for_network_selection,
            )),
            telemetry_sender,
        }
    }

    /// Scans and compiles list of BSSs that appear in the scan and belong to currently saved
    /// networks.
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
                        ctx: "NetworkSelector::perform_scan",
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

    /// Network selection. Filters list of available networks to only those that are acceptable for
    /// immediate connection.
    fn select_networks(
        &self,
        available_networks: HashSet<types::NetworkIdentifier>,
        network: &Option<types::NetworkIdentifier>,
    ) -> HashSet<types::NetworkIdentifier> {
        match network {
            Some(ref network) => HashSet::from([network.clone()]),
            None => {
                // TODO(fxbug.dev/113030): Add network selection logic.
                // Currently, the connection selection is determined solely based on the BSS. All available
                // networks are allowed.
                available_networks
            }
        }
    }

    /// BSS selection. Selects the best from a list of candidates that are available for
    /// connection.
    async fn select_bss(
        &self,
        allowed_candidate_list: Vec<types::ScannedCandidate>,
    ) -> Option<types::ScannedCandidate> {
        if allowed_candidate_list.is_empty() {
            info!("No BSSs available to select from.");
        } else {
            info!(
                "Selecting from {} BSSs found for allowed networks",
                allowed_candidate_list.len()
            );
        }

        let mut inspect_node = self.inspect_node_for_network_selections.lock().await;

        let selected = allowed_candidate_list
            .iter()
            .inspect(|&candidate| {
                info!("{}", candidate.to_string_without_pii());
            })
            .filter(|&candidate| {
                // Filter out incompatible BSSs
                if !candidate.bss.is_compatible() {
                    trace!("BSS is incompatible, filtering: {}", candidate.to_string_without_pii());
                    return false;
                };
                true
            })
            .max_by_key(|&candidate| candidate.score());

        // Log the candidates into Inspect
        inspect_log!(
            inspect_node.get_mut(),
            candidates: InspectList(&allowed_candidate_list),
            selected?: selected
        );

        if let Some(candidate) = selected {
            info!("Selected BSS:");
            info!("{}", candidate.to_string_without_pii());
            Some(candidate.clone())
        } else {
            None
        }
    }

    /// Full connection selection. Scans to find available candidates, uses network selection (or
    /// optional provided network) to filter out networks, and then bss selection to select the best
    /// of the remaining candidates. If the candidate was discovered via a passive scan, augment the
    /// bss info with an active scan
    pub(crate) async fn find_and_select_scanned_candidate(
        &self,
        network: Option<types::NetworkIdentifier>,
    ) -> Option<types::ScannedCandidate> {
        // Scan for BSSs belonging to saved networks.
        let available_candidate_list =
            self.find_available_bss_candidate_list(network.clone()).await;

        // Network selection.
        let available_networks: HashSet<types::NetworkIdentifier> =
            available_candidate_list.iter().map(|candidate| candidate.network.clone()).collect();
        let selected_networks = self.select_networks(available_networks, &network);

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
        let selection = match self.select_bss(allowed_candidate_list).await {
            Some(mut candidate) => {
                if network.is_some() {
                    // Strip scan observation type, because the candidate was discovered via a
                    // directed active scan, so we cannot know if it is discoverable via a passive
                    // scan.
                    candidate.bss.observation = types::ScanObservation::Unknown;
                }
                // If it was observed passively, augment with active scan.
                match candidate.bss.observation {
                    types::ScanObservation::Passive => Some(
                        augment_bss_candidate_with_active_scan(
                            candidate.clone(),
                            candidate.bss.channel,
                            candidate.bss.bssid,
                            self.scan_requester.clone(),
                        )
                        .await,
                    ),
                    _ => Some(candidate),
                }
            }
            None => None,
        };

        selection
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

impl types::ScannedCandidate {
    pub fn score(&self) -> i16 {
        let mut score = self.bss.rssi as i16;
        let channel = self.bss.channel;

        // If the network is 5G and has a strong enough RSSI, give it a bonus
        if channel.is_5ghz() && score >= RSSI_CUTOFF_5G_PREFERENCE {
            score = score.saturating_add(RSSI_5G_PREFERENCE_BOOST);
        }

        // Penalize APs with recent failures to connect
        let matching_failures = self
            .saved_network_info
            .recent_failures
            .iter()
            .filter(|failure| failure.bssid == self.bss.bssid);
        for failure in matching_failures {
            // Count failures for rejected credentials higher since we probably won't succeed
            // another try with the same credentials.
            if failure.reason == FailureReason::CredentialRejected {
                score = score.saturating_sub(SCORE_PENALTY_FOR_RECENT_CREDENTIAL_REJECTED);
            } else {
                score = score.saturating_sub(SCORE_PENALTY_FOR_RECENT_FAILURE);
            }
        }
        // Penalize APs with recent short connections before disconnecting.
        let short_connection_score: i16 = self
            .recent_short_connections()
            .try_into()
            .unwrap_or_else(|_| i16::MAX)
            .saturating_mul(SCORE_PENALTY_FOR_SHORT_CONNECTION);

        return score.saturating_sub(short_connection_score);
    }

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
        let rssi = self.bss.channel;
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
            self.score(),
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
            score: self.score(),
            security_type_saved: self.saved_security_type_to_string(),
            security_type_scanned: format!("{}", wlan_common::bss::Protection::from(self.security_type_detailed)),
            channel: InspectWlanChan(&self.bss.channel.into()),
            compatible: self.bss.is_compatible(),
            recent_failure_count: self.recent_failure_count(),
            saved_network_has_ever_connected: self.saved_network_info.has_ever_connected,
        });
    }
}

/// If a BSS was discovered via a passive scan, we need to perform an active scan on it to discover
/// all the information potentially needed by the SME layer.
async fn augment_bss_candidate_with_active_scan(
    scanned_candidate: types::ScannedCandidate,
    channel: types::WlanChan,
    bssid: types::Bssid,
    scan_requester: Arc<dyn scan::ScanRequestApi>,
) -> types::ScannedCandidate {
    // This internal function encapsulates all the logic and has a Result<> return type, allowing us
    // to use the `?` operator inside it to reduce nesting.
    async fn get_enhanced_bss_description(
        scanned_candidate: &types::ScannedCandidate,
        channel: types::WlanChan,
        bssid: types::Bssid,
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
                vec![channel],
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
                        if bss.bssid == bssid {
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

    match get_enhanced_bss_description(&scanned_candidate, channel, bssid, scan_requester).await {
        Ok(new_bss_description) => {
            let updated_scanned_bss =
                Bss { bss_description: new_bss_description, ..scanned_candidate.bss.clone() };
            types::ScannedCandidate { bss: updated_scanned_bss, ..scanned_candidate }
        }
        Err(()) => scanned_candidate,
    }
}

/// Give a numerical score to the connection quality in order to decide whether to look for a new
/// network and to ultimately decide whether to switch to a new network or stay on the same one.
/// score should be between 0 and 1, where 0 is an unusable connection and 1 is a great connection.
pub fn score_connection_quality(_connection_stats: &PeriodicConnectionStats) -> f32 {
    // TODO(fxbug.dev/84551) Actually implement the connection quality scoring and the threshold
    // for a bad connection
    return 1.0;
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
        ctx: "network_selection::record_metrics_on_scan",
    });
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            config_management::{
                network_config::{PastConnectionData, PastConnectionsByBssid},
                ConnectFailure, FailureReason, SavedNetworksManager,
            },
            util::testing::{
                create_inspect_persistence_channel, create_wlan_hasher,
                fakes::{FakeSavedNetworksManager, FakeScanRequester},
                generate_channel, generate_random_bss, generate_random_bss_with_compatibility,
                generate_random_saved_network_data, generate_random_scan_result,
                generate_random_scanned_candidate, random_connection_data,
            },
        },
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
        fidl_fuchsia_wlan_sme as fidl_sme, fuchsia_async as fasync,
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
        network_selector: Arc<NetworkSelector>,
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
        let inspect_node = inspector.root().create_child("net_select_test");
        let (persistence_req_sender, _persistence_stream) = create_inspect_persistence_channel();
        let (telemetry_sender, telemetry_receiver) = mpsc::channel::<TelemetryEvent>(100);

        let network_selector = Arc::new(NetworkSelector::new(
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
            network_selector,
            real_saved_network_manager,
            saved_network_manager,
            scan_requester,
            inspector,
            telemetry_receiver,
        }
    }

    fn generate_candidate_for_scoring(
        rssi: i8,
        snr_db: i8,
        channel: types::WlanChan,
    ) -> types::ScannedCandidate {
        let bss = types::Bss {
            rssi,
            snr_db,
            channel: channel,
            bss_description: fidl_internal::BssDescription {
                rssi_dbm: rssi,
                snr_db,
                channel: channel.into(),
                ..random_fidl_bss_description!()
            }
            .into(),
            ..generate_random_bss_with_compatibility()
        };
        types::ScannedCandidate { bss, ..generate_random_scanned_candidate() }
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
            past_connections: PastConnectionsByBssid::new(),
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
                    past_connections: PastConnectionsByBssid::new(),
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

    #[test_case(types::Bss {
            rssi: -8,
            channel: generate_channel(1),
            ..generate_random_bss()
        },
        -8; "2.4GHz BSS score is RSSI")]
    #[test_case(types::Bss {
            rssi: -49,
            channel: generate_channel(36),
            ..generate_random_bss()
        },
        -29; "5GHz score is (RSSI + mod), when above threshold")]
    #[test_case(types::Bss {
            rssi: -71,
            channel: generate_channel(36),
            ..generate_random_bss()
        },
        -71; "5GHz score is RSSI, when below threshold")]
    #[fuchsia::test(add_test_attr = false)]
    fn scoring_test(bss: types::Bss, expected_score: i16) {
        let _exec = fasync::TestExecutor::new_with_fake_time();
        let scanned_candidate =
            types::ScannedCandidate { bss: bss.clone(), ..generate_random_scanned_candidate() };
        assert_eq!(scanned_candidate.score(), expected_score)
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_bss_prefers_less_short_connections() {
        let bss_worse =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        let short_uptime = zx::Duration::from_seconds(30);
        let okay_uptime = zx::Duration::from_minutes(100);
        // Record a short uptime for the worse network and a long enough uptime for the better one.
        let short_uptime_data = past_connection_with_bssid_uptime(bss_worse.bssid, short_uptime);
        let okay_uptime_data = past_connection_with_bssid_uptime(bss_better.bssid, okay_uptime);
        internal_data.past_connections.add(bss_worse.bssid, short_uptime_data);
        internal_data.past_connections.add(bss_better.bssid, okay_uptime_data);
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };

        // Check that the better BSS has a higher score than the worse BSS.
        assert!(bss_better.score() > bss_worse.score());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_bss_prefers_less_failures() {
        let bss_worse =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -60, channel: generate_channel(3), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Add many test failures for the worse BSS and one for the better BSS
        let mut failures = vec![connect_failure_with_bssid(bss_worse.bssid); 12];
        failures.push(connect_failure_with_bssid(bss_better.bssid));
        internal_data.recent_failures = failures;
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };
        // Check that the better BSS has a higher score than the worse BSS.
        assert!(bss_better.score() > bss_worse.score());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_bss_prefers_strong_5ghz_with_failures() {
        // Test test that if one network has a few network failures but is 5 Ghz instead of 2.4,
        // the 5 GHz network has a higher score.
        let bss_worse =
            types::Bss { rssi: -35, channel: generate_channel(3), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -35, channel: generate_channel(36), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Set the failure list to have 0 failures for the worse BSS and 4 failures for the
        // stronger BSS.
        internal_data.recent_failures = vec![connect_failure_with_bssid(bss_better.bssid); 2];
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };
        assert!(bss_better.score() > bss_worse.score());
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_score_credentials_rejected_worse() {
        // If two BSS are identical other than one failed to connect with wrong credentials and
        // the other failed with a few connect failurs, the one with wrong credentials has a lower
        // score.
        let bss_worse =
            types::Bss { rssi: -30, channel: generate_channel(44), ..generate_random_bss() };
        let bss_better =
            types::Bss { rssi: -30, channel: generate_channel(44), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Add many test failures for the worse BSS and one for the better BSS
        let mut failures = vec![connect_failure_with_bssid(bss_better.bssid); 4];
        failures.push(ConnectFailure {
            bssid: bss_worse.bssid,
            time: fasync::Time::now(),
            reason: FailureReason::CredentialRejected,
        });
        internal_data.recent_failures = failures;
        let shared_candidate_data = types::ScannedCandidate {
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };
        let bss_worse = types::ScannedCandidate { bss: bss_worse, ..shared_candidate_data.clone() };
        let bss_better =
            types::ScannedCandidate { bss: bss_better, ..shared_candidate_data.clone() };

        assert!(bss_better.score() > bss_worse.score());
    }

    #[fasync::run_singlethreaded(test)]
    async fn score_many_penalties_do_not_cause_panic() {
        let bss = types::Bss { rssi: -80, channel: generate_channel(1), ..generate_random_bss() };
        let mut internal_data = generate_random_saved_network_data();
        // Add 10 general failures and 10 rejected credentials failures
        internal_data.recent_failures = vec![connect_failure_with_bssid(bss.bssid); 10];
        for _ in 0..1200 {
            internal_data.recent_failures.push(ConnectFailure {
                bssid: bss.bssid,
                time: fasync::Time::now(),
                reason: FailureReason::CredentialRejected,
            });
        }
        let short_uptime = zx::Duration::from_seconds(30);
        let data = past_connection_with_bssid_uptime(bss.bssid, short_uptime);
        for _ in 0..10 {
            internal_data.past_connections.add(bss.bssid, data);
        }
        let scanned_candidate = types::ScannedCandidate {
            bss,
            saved_network_info: internal_data,
            ..generate_random_scanned_candidate()
        };

        assert_eq!(scanned_candidate.score(), i16::MIN);
    }

    #[fuchsia::test]
    fn select_bss_sorts_by_score() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));

        let mut candidates = vec![];

        candidates.push(generate_candidate_for_scoring(-30, 30, generate_channel(36)));
        candidates.push(generate_candidate_for_scoring(-30, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-20, 30, generate_channel(1)));

        // there's a network on 5G, it should get a boost and be selected
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[0].clone())
        );

        // make the 5GHz network into a 2.4GHz network
        let mut modified_network = candidates[0].clone();
        let modified_bss =
            types::Bss { channel: generate_channel(6), ..modified_network.bss.clone() };
        modified_network.bss = modified_bss;
        candidates[0] = modified_network;

        // all networks are 2.4GHz, strongest RSSI network returned
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[2].clone())
        );
    }

    #[fuchsia::test]
    fn select_bss_sorts_by_failure_count() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));

        let mut candidates = vec![];

        candidates.push(generate_candidate_for_scoring(-25, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-30, 30, generate_channel(1)));

        // stronger network returned
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[0].clone()),
        );

        // mark the stronger network as having some failures
        let num_failures = 4;
        candidates[0].saved_network_info.recent_failures =
            vec![connect_failure_with_bssid(candidates[0].bss.bssid); num_failures];

        // weaker network (with no failures) returned
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[1].clone())
        );

        // give them both the same number of failures
        candidates[1].saved_network_info.recent_failures =
            vec![connect_failure_with_bssid(candidates[1].bss.bssid); num_failures];

        // stronger network returned
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[0].clone())
        );
    }

    #[fuchsia::test]
    fn select_bss_ignore_incompatible() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));

        let mut candidates = vec![];

        // Add two BSSs, both compatible to start.
        candidates.push(generate_candidate_for_scoring(-14, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-90, 30, generate_channel(1)));

        // The stronger BSS is selected initially.
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[0].clone())
        );

        // Make the stronger BSS incompatible.
        candidates[0].bss.compatibility = None;

        // The weaker, but still compatible, BSS is selected.
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[1].clone())
        );

        // TODO(fxbug.dev/120520): After `select_bss` filters out incompatible BSSs, this None
        // compatibility should change to a Some, to test that logic.
        // Make both BSSs incompatible.
        candidates[1].bss.compatibility = None;

        // No BSS is selected.
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            None
        );
    }

    #[fuchsia::test]
    fn select_bss_logs_to_inspect() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));

        let mut candidates = vec![];
        candidates.push(generate_candidate_for_scoring(-50, 30, generate_channel(1)));
        candidates.push(generate_candidate_for_scoring(-60, 30, generate_channel(3)));
        candidates.push(generate_candidate_for_scoring(-20, 30, generate_channel(6)));

        // stronger network returned
        assert_eq!(
            exec.run_singlethreaded(test_values.network_selector.select_bss(candidates.clone())),
            Some(candidates[2].clone())
        );

        let fidl_channel = fidl_common::WlanChannel::from(candidates[2].bss.channel);
        assert_data_tree!(test_values.inspector, root: {
            net_select_test: {
                "network_selection": {
                    "0": {
                        "@time": inspect::testing::AnyProperty,
                        "candidates": {
                            "0": contains {
                                score: inspect::testing::AnyProperty,
                            },
                            "1": contains {
                                score: inspect::testing::AnyProperty,
                            },
                            "2": contains {
                                score: inspect::testing::AnyProperty,
                            },
                        },
                        "selected": {
                            ssid_hash: candidates[2].hasher.hash_ssid(&candidates[2].network.ssid),
                            bssid_hash: candidates[2].hasher.hash_mac_addr(&candidates[2].bss.bssid.0),
                            rssi: i64::from(candidates[2].bss.rssi),
                            score: i64::from(candidates[2].score()),
                            security_type_saved: candidates[2].saved_security_type_to_string(),
                            security_type_scanned: format!("{}", wlan_common::bss::Protection::from(candidates[2].security_type_detailed)),
                            channel: {
                                cbw: inspect::testing::AnyProperty,
                                primary: u64::from(fidl_channel.primary),
                                secondary80: u64::from(fidl_channel.secondary80),
                            },
                            compatible: candidates[2].bss.compatibility.is_some(),
                            recent_failure_count: candidates[2].recent_failure_count(),
                            saved_network_has_ever_connected: candidates[2].saved_network_info.has_ever_connected,
                        },
                    }
                }
            },
        });
    }

    #[fuchsia::test]
    fn select_bss_empty_list_logs_to_inspect() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));

        assert_eq!(exec.run_singlethreaded(test_values.network_selector.select_bss(vec![])), None);

        // Verify that an empty list of candidates is still logged to inspect.
        assert_data_tree!(test_values.inspector, root: {
            net_select_test: {
                "network_selection": {
                    "0": {
                        "@time": inspect::testing::AnyProperty,
                        "candidates": {},
                    }
                }
            },
        });
    }

    #[fuchsia::test]
    fn augment_bss_candidate_with_active_scan_doesnt_run_on_actively_found_networks() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let mut candidate = generate_random_scanned_candidate();
        candidate.bss.observation = types::ScanObservation::Active;

        let fut = augment_bss_candidate_with_active_scan(
            candidate.clone(),
            candidate.bss.channel,
            candidate.bss.bssid,
            test_values.scan_requester.clone(),
        );
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

        let fut = augment_bss_candidate_with_active_scan(
            passively_scanned_candidate.clone(),
            passively_scanned_candidate.bss.channel,
            passively_scanned_candidate.bss.bssid,
            test_values.scan_requester.clone(),
        );
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
        let network_selector = test_values.network_selector;

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
        let network_selection_fut =
            network_selector.find_available_bss_candidate_list(Some(test_id_1.clone()));
        pin_mut!(network_selection_fut);
        let results = exec.run_singlethreaded(&mut network_selection_fut);
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
        let network_selector = test_values.network_selector;

        // create identifiers
        let test_id_not_hidden = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("foo").unwrap(),
            security_type: types::SecurityType::Wpa3,
        };
        let test_id_maybe_hidden = types::NetworkIdentifier {
            ssid: types::Ssid::try_from("bar").unwrap(),
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

        // Set the hidden probability for test_id_not_hidden
        exec.run_singlethreaded(
            test_values.saved_network_manager.update_hidden_prob(test_id_not_hidden.clone(), 0.0),
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
            // Initial passive scan has non-hidden result
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
            // Non-hidden test case, only one scan, return both results
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
                generate_random_scan_result(),
                generate_random_scan_result(),
            ])));
        }

        // Run the scan(s)
        let network_selection_fut = network_selector.find_available_bss_candidate_list(None);
        pin_mut!(network_selection_fut);
        let results = exec.run_singlethreaded(&mut network_selection_fut);
        assert_eq!(results.len(), 2);

        // Check that the right scan request(s) were sent
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
        let network_selector = test_values.network_selector;
        let mut telemetry_receiver = test_values.telemetry_receiver;

        // Return an error on the scan
        exec.run_singlethreaded(
            test_values.scan_requester.add_scan_result(Err(types::ScanError::GeneralError)),
        );

        // Kick off network selection
        let network_selection_fut = network_selector.find_and_select_scanned_candidate(None);
        pin_mut!(network_selection_fut);
        // Check that nothing is returned
        assert_variant!(exec.run_until_stalled(&mut network_selection_fut), Poll::Ready(None));

        // Check that the right scan request was sent
        assert_eq!(
            *exec.run_singlethreaded(test_values.scan_requester.scan_requests.lock()),
            vec![(scan::ScanReason::NetworkSelection, vec![], vec![])]
        );

        // Check the network selections were logged
        assert_data_tree!(test_values.inspector, root: {
            net_select_test: {
                network_selection: {
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
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_end_to_end() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let network_selector = test_values.network_selector;
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
        let network_selection_fut = network_selector.find_and_select_scanned_candidate(None);
        pin_mut!(network_selection_fut);
        let results =
            exec.run_singlethreaded(&mut network_selection_fut).expect("no selected candidate");
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
            net_select_test: {
                network_selection: {
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
                ctx: "network_selection::record_metrics_on_scan",
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
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_with_network_end_to_end() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let network_selector = test_values.network_selector;
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
        let network_selection_fut =
            network_selector.find_and_select_scanned_candidate(Some(test_id_1.clone()));
        pin_mut!(network_selection_fut);
        let results =
            exec.run_singlethreaded(&mut network_selection_fut).expect("no selected candidate");
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
    }

    #[fuchsia::test]
    fn select_networks_selects_specified_network() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let network_selector = test_values.network_selector;

        let ssid = "foo";
        let all_networks = vec![
            types::NetworkIdentifier {
                ssid: ssid.try_into().unwrap(),
                security_type: types::SecurityType::Wpa3,
            },
            types::NetworkIdentifier {
                ssid: ssid.try_into().unwrap(),
                security_type: types::SecurityType::Wpa2,
            },
        ];
        let all_network_set = HashSet::from_iter(all_networks.clone());

        // Specifying a network filters to just that network
        let desired_network = all_networks[0].clone();
        let selected_network = network_selector
            .select_networks(all_network_set.clone(), &Some(desired_network.clone()));
        assert_eq!(selected_network, HashSet::from([desired_network]));

        // No specified network returns all networks
        assert_eq!(
            network_selector.select_networks(all_network_set.clone(), &None),
            all_network_set
        );
    }

    #[fuchsia::test]
    fn find_and_select_scanned_candidate_with_network_end_to_end_with_failure() {
        let mut exec = fasync::TestExecutor::new();
        let test_values = exec.run_singlethreaded(test_setup(true));
        let network_selector = test_values.network_selector;
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
        let network_selection_fut =
            network_selector.find_and_select_scanned_candidate(Some(test_id_1.clone()));
        pin_mut!(network_selection_fut);

        // Check that nothing is returned
        let results = exec.run_singlethreaded(&mut network_selection_fut);
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

    fn connect_failure_with_bssid(bssid: types::Bssid) -> ConnectFailure {
        ConnectFailure {
            reason: FailureReason::GeneralFailure,
            time: fasync::Time::INFINITE,
            bssid,
        }
    }

    fn past_connection_with_bssid_uptime(
        bssid: types::Bssid,
        uptime: zx::Duration,
    ) -> PastConnectionData {
        PastConnectionData {
            bssid,
            connection_uptime: uptime,
            disconnect_time: fasync::Time::INFINITE, // disconnect will always be considered recent
            ..random_connection_data()
        }
    }

    fn fake_successful_connect_result() -> fidl_sme::ConnectResult {
        fidl_sme::ConnectResult {
            code: fidl_ieee80211::StatusCode::Success,
            is_credential_rejected: false,
            is_reconnect: false,
        }
    }
}
