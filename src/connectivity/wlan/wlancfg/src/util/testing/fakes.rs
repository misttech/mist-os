// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![cfg(test)]

use crate::client::roaming::lib::{RoamTriggerData, RoamTriggerDataOutcome};
use crate::client::roaming::roam_monitor::RoamMonitorApi;
use crate::client::{scan, types as client_types};
use crate::config_management::{
    Credential, NetworkConfig, NetworkConfigError, NetworkIdentifier, PastConnectionData,
    PastConnectionList, SavedNetworksManagerApi,
};
use anyhow::format_err;
use async_trait::async_trait;
use futures::channel::mpsc;
use futures::lock::Mutex;
use log::{info, warn};
use rand::Rng;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use {
    fidl_fuchsia_wlan_policy as fidl_policy, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_async as fasync,
};

pub struct FakeSavedNetworksManager {
    saved_networks: Mutex<HashMap<NetworkIdentifier, Vec<NetworkConfig>>>,
    connections_recorded: Mutex<Vec<ConnectionRecord>>,
    connect_results_recorded: Mutex<Vec<ConnectResultRecord>>,
    lookup_compatible_response: Mutex<LookupCompatibleResponse>,
    pub fail_all_stores: bool,
    // A type alias for this complex type would be needless indirection, so allow the complex type
    #[allow(clippy::type_complexity)]
    pub scan_result_records: Arc<
        Mutex<
            Vec<(
                Vec<client_types::Ssid>,
                HashMap<client_types::NetworkIdentifierDetailed, Vec<client_types::Bss>>,
            )>,
        >,
    >,
    pub past_connections_response: PastConnectionList,
    pub is_network_single_bss_resp: Mutex<Option<bool>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionRecord {
    pub id: NetworkIdentifier,
    pub credential: Credential,
    pub data: PastConnectionData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectResultRecord {
    pub id: NetworkIdentifier,
    pub credential: Credential,
    pub bssid: client_types::Bssid,
    pub connect_result: fidl_sme::ConnectResult,
    pub scan_type: client_types::ScanObservation,
}

/// Use a struct so that the option can be updated from None to Some to allow the response to be
/// set after FakeSavedNetworksManager is created. Use an optional response value rather than
/// defaulting to an empty vector so that if the response is not set, lookup_compatible will panic
/// for easier debugging.
struct LookupCompatibleResponse {
    inner: Option<Vec<NetworkConfig>>,
}

impl LookupCompatibleResponse {
    fn new() -> Self {
        LookupCompatibleResponse { inner: None }
    }
}

impl Default for FakeSavedNetworksManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeSavedNetworksManager {
    pub fn new() -> Self {
        Self {
            saved_networks: Mutex::new(HashMap::new()),
            connections_recorded: Mutex::new(vec![]),
            connect_results_recorded: Mutex::new(vec![]),
            fail_all_stores: false,
            lookup_compatible_response: Mutex::new(LookupCompatibleResponse::new()),
            scan_result_records: Arc::new(Mutex::new(vec![])),
            past_connections_response: PastConnectionList::default(),
            is_network_single_bss_resp: Mutex::new(None),
        }
    }

    /// Create FakeSavedNetworksManager, saving network configs with the specified
    /// network identifiers and credentials at init.
    pub fn new_with_saved_networks(network_configs: Vec<fidl_policy::NetworkConfig>) -> Self {
        let mut saved_networks = HashMap::<NetworkIdentifier, Vec<NetworkConfig>>::new();
        for config in network_configs.into_iter() {
            let id: NetworkIdentifier =
                config.id.expect("test config is missing network identifier").into();
            let credential = config
                .credential
                .expect("test config is missing credential")
                .try_into()
                .expect("test credential is not valid");
            let config = NetworkConfig::new(id.clone(), credential, false)
                .expect("provided config is not valid");

            saved_networks.entry(id).or_default().push(config);
        }

        Self {
            saved_networks: Mutex::new(saved_networks),
            connections_recorded: Mutex::new(vec![]),
            connect_results_recorded: Mutex::new(vec![]),
            fail_all_stores: false,
            lookup_compatible_response: Mutex::new(LookupCompatibleResponse::new()),
            scan_result_records: Arc::new(Mutex::new(vec![])),
            past_connections_response: PastConnectionList::default(),
            is_network_single_bss_resp: Mutex::new(None),
        }
    }

    /// Returns the past connections as they were recorded, rather than how they would have been
    /// stored.
    pub fn get_recorded_past_connections(&self) -> Vec<ConnectionRecord> {
        self.connections_recorded
            .try_lock()
            .expect("expect locking self.connections_recorded to succeed")
            .clone()
    }

    pub fn get_recorded_connect_reslts(&self) -> Vec<ConnectResultRecord> {
        self.connect_results_recorded
            .try_lock()
            .expect("expect locking self.connect_results_recorded to succeed")
            .clone()
    }

    /// Manually change the hidden network probabiltiy of a saved network.
    pub async fn update_hidden_prob(&self, id: NetworkIdentifier, hidden_prob: f32) {
        let mut saved_networks = self.saved_networks.lock().await;
        let networks = match saved_networks.get_mut(&id) {
            Some(networks) => networks,
            None => {
                info!("Failed to find network to update");
                return;
            }
        };
        for network in networks.iter_mut() {
            network.hidden_probability = hidden_prob;
        }
    }

    pub fn set_lookup_compatible_response(&self, response: Vec<NetworkConfig>) {
        self.lookup_compatible_response.try_lock().expect("failed to get lock").inner =
            Some(response);
    }

    pub fn set_is_single_bss_response(&self, resp: bool) {
        *self
            .is_network_single_bss_resp
            .try_lock()
            .expect("failed to get is_network_single_bss_resp lock") = Some(resp);
    }
}

#[async_trait(?Send)]
impl SavedNetworksManagerApi for FakeSavedNetworksManager {
    async fn remove(
        &self,
        network_id: NetworkIdentifier,
        credential: Credential,
    ) -> Result<bool, NetworkConfigError> {
        let mut saved_networks = self.saved_networks.lock().await;
        if let Some(network_configs) = saved_networks.get_mut(&network_id) {
            let original_len = network_configs.len();
            network_configs.retain(|cfg| cfg.credential != credential);
            if original_len != network_configs.len() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn known_network_count(&self) -> usize {
        unimplemented!()
    }

    async fn lookup(&self, id: &NetworkIdentifier) -> Vec<NetworkConfig> {
        self.saved_networks.lock().await.get(id).cloned().unwrap_or_default()
    }

    async fn lookup_compatible(
        &self,
        ssid: &client_types::Ssid,
        _scan_security: client_types::SecurityTypeDetailed,
    ) -> Vec<NetworkConfig> {
        let predetermined_response = self.lookup_compatible_response.lock().await.inner.clone();
        match predetermined_response {
            Some(resp) => resp,
            None => {
                warn!("FakeSavedNetworksManager lookup_compatible response is not set, returning all networks with matching SSID");
                self.saved_networks
                    .lock()
                    .await
                    .iter()
                    .filter_map(
                        |(id, config)| if id.ssid == *ssid { Some(config.clone()) } else { None },
                    )
                    .flatten()
                    .collect()
            }
        }
    }

    /// Note that the configs-per-NetworkIdentifier limit is set to 1 in
    /// this mock struct. If a NetworkIdentifier is already stored, writing
    /// a config to it will evict the previously store one.
    async fn store(
        &self,
        network_id: NetworkIdentifier,
        credential: Credential,
    ) -> Result<Option<NetworkConfig>, NetworkConfigError> {
        if self.fail_all_stores {
            return Err(NetworkConfigError::FileWriteError);
        }
        let config = NetworkConfig::new(network_id.clone(), credential, false)?;
        return Ok(self
            .saved_networks
            .lock()
            .await
            .insert(network_id, vec![config])
            .and_then(|mut v| v.pop()));
    }

    async fn record_connect_result(
        &self,
        id: NetworkIdentifier,
        credential: &Credential,
        bssid: client_types::Bssid,
        connect_result: fidl_sme::ConnectResult,
        scan_type: client_types::ScanObservation,
    ) {
        self.connect_results_recorded.try_lock().expect("failed to record connect result").push(
            ConnectResultRecord {
                id: id.clone(),
                credential: credential.clone(),
                bssid,
                connect_result,
                scan_type,
            },
        );
    }

    async fn record_disconnect(
        &self,
        id: &NetworkIdentifier,
        credential: &Credential,
        data: PastConnectionData,
    ) {
        let mut connections_recorded = self.connections_recorded.lock().await;
        connections_recorded.push(ConnectionRecord {
            id: id.clone(),
            credential: credential.clone(),
            data,
        });
    }

    async fn record_periodic_metrics(&self) {}

    async fn record_scan_result(
        &self,
        target_ssids: Vec<client_types::Ssid>,
        results: &HashMap<client_types::NetworkIdentifierDetailed, Vec<client_types::Bss>>,
    ) {
        let mut records = self.scan_result_records.lock().await;
        records.push((target_ssids, results.clone()));
    }

    /// This is currently used for roaming scan decisions. If it is not set in the
    /// FakeSavedNetworksManager, an error will returned to avoid tests failing down the line
    /// because of the value returned here.
    async fn is_network_single_bss(
        &self,
        _id: &NetworkIdentifier,
        _credential: &Credential,
    ) -> Result<bool, anyhow::Error> {
        return self
            .is_network_single_bss_resp
            .try_lock()
            .expect("failed to get is_network_single_bss_resp lock")
            .ok_or_else(|| {
                format_err!(
                    "is_network_single_bss response is not set for FakeSavedNetworksManager"
                )
            });
    }

    async fn get_networks(&self) -> Vec<NetworkConfig> {
        self.saved_networks.lock().await.values().flat_map(|cfgs| cfgs.clone()).collect()
    }

    async fn get_past_connections(
        &self,
        _id: &NetworkIdentifier,
        _credential: &Credential,
        _bssid: &client_types::Bssid,
    ) -> PastConnectionList {
        self.past_connections_response.clone()
    }
}

pub fn create_inspect_persistence_channel() -> (mpsc::Sender<String>, mpsc::Receiver<String>) {
    const DEFAULT_BUFFER_SIZE: usize = 100; // arbitrary value
    mpsc::channel(DEFAULT_BUFFER_SIZE)
}

/// Create past connection data with all random values. Tests can set the values they care about.
pub fn random_connection_data() -> PastConnectionData {
    let mut rng = rand::thread_rng();
    let connect_time = fasync::MonotonicInstant::from_nanos(rng.gen::<u16>().into());
    let time_to_connect = zx::MonotonicDuration::from_seconds(rng.gen_range::<i64, _>(5..10));
    let uptime = zx::MonotonicDuration::from_seconds(rng.gen_range::<i64, _>(5..1000));
    let disconnect_time = connect_time + time_to_connect + uptime;
    PastConnectionData::new(
        client_types::Bssid::from(rng.gen::<[u8; 6]>()),
        disconnect_time,
        uptime,
        client_types::DisconnectReason::DisconnectDetectedFromSme,
        client_types::Signal { rssi_dbm: rng.gen_range(-90..-20), snr_db: rng.gen_range(10..50) },
        rng.gen::<u8>().into(),
    )
}

#[derive(Clone)]
pub struct FakeScanRequester {
    // A type alias for this complex type would be needless indirection, so allow the complex type
    #[allow(clippy::type_complexity)]
    pub scan_results:
        Arc<Mutex<VecDeque<Result<Vec<client_types::ScanResult>, client_types::ScanError>>>>,
    #[allow(clippy::type_complexity)]
    pub scan_requests: Arc<
        Mutex<VecDeque<(scan::ScanReason, Vec<client_types::Ssid>, Vec<client_types::WlanChan>)>>,
    >,
}

impl Default for FakeScanRequester {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeScanRequester {
    pub fn new() -> Self {
        FakeScanRequester {
            scan_results: Arc::new(Mutex::new(VecDeque::new())),
            scan_requests: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
    pub async fn add_scan_result(
        &self,
        res: Result<Vec<client_types::ScanResult>, client_types::ScanError>,
    ) {
        self.scan_results.lock().await.push_back(res);
    }
    pub async fn verify_scan_request(
        &self,
        mut expected: (scan::ScanReason, Vec<client_types::Ssid>, Vec<client_types::WlanChan>),
    ) {
        let mut actual = self.scan_requests.lock().await.pop_front().unwrap();
        // Sort SSIDs and channels
        actual.1.sort();
        actual.2.sort();
        expected.1.sort();
        expected.2.sort();
        assert_eq!(actual.0, expected.0);
        assert_eq!(actual.1, expected.1);
        assert_eq!(actual.2, expected.2);
    }
}

#[async_trait(?Send)]
impl scan::ScanRequestApi for FakeScanRequester {
    async fn perform_scan(
        &self,
        scan_reason: scan::ScanReason,
        ssids: Vec<client_types::Ssid>,
        channels: Vec<client_types::WlanChan>,
    ) -> Result<Vec<client_types::ScanResult>, client_types::ScanError> {
        self.scan_requests.lock().await.push_back((scan_reason, ssids, channels));
        self.scan_results.lock().await.pop_front().unwrap()
    }
}

#[derive(Clone)]
pub struct FakeRoamMonitor {
    pub trigger_data_queue: VecDeque<RoamTriggerData>,
    pub response_to_should_roam_scan: RoamTriggerDataOutcome,
    pub response_to_should_send_roam_request: bool,
}

impl Default for FakeRoamMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRoamMonitor {
    pub fn new() -> Self {
        Self {
            trigger_data_queue: VecDeque::new(),
            response_to_should_roam_scan: RoamTriggerDataOutcome::Noop,
            response_to_should_send_roam_request: false,
        }
    }
}

#[async_trait(?Send)]
impl RoamMonitorApi for FakeRoamMonitor {
    async fn handle_roam_trigger_data(
        &mut self,
        data: RoamTriggerData,
    ) -> Result<RoamTriggerDataOutcome, anyhow::Error> {
        self.trigger_data_queue.push_back(data);
        Ok(self.response_to_should_roam_scan.clone())
    }
    fn should_send_roam_request(
        &self,
        _candidate: client_types::ScannedCandidate,
    ) -> Result<bool, anyhow::Error> {
        Ok(self.response_to_should_send_roam_request)
    }
}
