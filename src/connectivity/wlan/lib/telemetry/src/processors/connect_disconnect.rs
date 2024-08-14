// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::inspect_time_series::TimeSeriesStats;
use crate::util::cobalt_logger::log_cobalt_1dot1_batch;
use crate::util::inspect_bounded_set::InspectBoundedSetNode;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload};
use fuchsia_inspect::Node as InspectNode;
use fuchsia_inspect_contrib::auto_persist::{
    AutoPersist, {self},
};
use fuchsia_inspect_contrib::nodes::BoundedListNode;
use fuchsia_inspect_contrib::{inspect_insert, inspect_log};
use fuchsia_inspect_derive::Unit;
use fuchsia_sync::Mutex;
use std::sync::Arc;
use tracing::{info, warn};
use windowed_stats::experimental::clock::{TimedSample, Timestamp};
use windowed_stats::experimental::series::interpolation::Constant;
use windowed_stats::experimental::series::statistic::Union;
use windowed_stats::experimental::series::{
    BitSet, RoundRobinSampler, Sampler, SamplingProfile, TimeMatrix,
};
use wlan_common::bss::BssDescription;
use wlan_common::channel::Channel;
use {
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_zircon as zx, wlan_legacy_metrics_registry as metrics,
};

const INSPECT_CONNECT_EVENTS_LIMIT: usize = 10;
const INSPECT_DISCONNECT_EVENTS_LIMIT: usize = 10;
const INSPECT_WLAN_CONNECTIVITY_STATES_ID_LIMIT: usize = 16;
const INSPECT_CONNECTED_NETWORKS_ID_LIMIT: usize = 16;
const INSPECT_DISCONNECT_SOURCES_ID_LIMIT: usize = 32;

const IDLE_STATE_NAME: &'static str = "Idle";
const DISCONNECTED_STATE_NAME: &'static str = "Disconnected";
const CONNECTED_STATE_NAME: &'static str = "Connected";
const CONNECTION_STATE_NAMES: [&'static str; 3] =
    [IDLE_STATE_NAME, DISCONNECTED_STATE_NAME, CONNECTED_STATE_NAME];

#[derive(Debug)]
enum ConnectionState {
    Idle(IdleState),
    Connected(ConnectedState),
    Disconnected(DisconnectedState),
}
#[derive(Debug)]
struct IdleState {}

#[derive(Debug)]
struct ConnectedState {}

#[derive(Debug)]
struct DisconnectedState {}

#[derive(PartialEq, Unit)]
struct InspectWlanConnectivityState {
    state_name: String,
}

#[derive(Unit)]
struct InspectConnectedNetwork {
    bssid: String,
    ssid: String,
    protection: String,
    ht_cap: Option<Vec<u8>>,
    vht_cap: Option<Vec<u8>>,
    wsc: Option<InspectNetworkWsc>,
    is_wmm_assoc: bool,
    wmm_param: Option<Vec<u8>>,
}

impl From<&BssDescription> for InspectConnectedNetwork {
    fn from(bss_description: &BssDescription) -> Self {
        Self {
            bssid: bss_description.bssid.to_string(),
            ssid: bss_description.ssid.to_string(),
            protection: format!("{:?}", bss_description.protection()),
            ht_cap: bss_description.raw_ht_cap().map(|cap| cap.bytes.into()),
            vht_cap: bss_description.raw_vht_cap().map(|cap| cap.bytes.into()),
            wsc: bss_description.probe_resp_wsc().as_ref().map(|wsc| InspectNetworkWsc::from(wsc)),
            is_wmm_assoc: bss_description.find_wmm_param().is_some(),
            wmm_param: bss_description.find_wmm_param().map(|bytes| bytes.into()),
        }
    }
}

#[derive(PartialEq, Unit)]
struct InspectNetworkWsc {
    device_name: String,
    manufacturer: String,
    model_name: String,
    model_number: String,
}

impl From<&wlan_common::ie::wsc::ProbeRespWsc> for InspectNetworkWsc {
    fn from(wsc: &wlan_common::ie::wsc::ProbeRespWsc) -> Self {
        Self {
            device_name: String::from_utf8_lossy(&wsc.device_name[..]).to_string(),
            manufacturer: String::from_utf8_lossy(&wsc.manufacturer[..]).to_string(),
            model_name: String::from_utf8_lossy(&wsc.model_name[..]).to_string(),
            model_number: String::from_utf8_lossy(&wsc.model_number[..]).to_string(),
        }
    }
}

#[derive(PartialEq, Unit)]
struct InspectDisconnectSource {
    source: String,
    reason: String,
    mlme_event_name: Option<String>,
}

impl From<&fidl_sme::DisconnectSource> for InspectDisconnectSource {
    fn from(disconnect_source: &fidl_sme::DisconnectSource) -> Self {
        match disconnect_source {
            fidl_sme::DisconnectSource::User(reason) => Self {
                source: "user".to_string(),
                reason: format!("{:?}", reason),
                mlme_event_name: None,
            },
            fidl_sme::DisconnectSource::Ap(cause) => Self {
                source: "ap".to_string(),
                reason: format!("{:?}", cause.reason_code),
                mlme_event_name: Some(format!("{:?}", cause.mlme_event_name)),
            },
            fidl_sme::DisconnectSource::Mlme(cause) => Self {
                source: "mlme".to_string(),
                reason: format!("{:?}", cause.reason_code),
                mlme_event_name: Some(format!("{:?}", cause.mlme_event_name)),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisconnectInfo {
    pub connected_duration: zx::Duration,
    pub is_sme_reconnecting: bool,
    pub disconnect_source: fidl_sme::DisconnectSource,
    pub original_bss_desc: Box<BssDescription>,
    pub current_rssi_dbm: i8,
    pub current_snr_db: i8,
    pub current_channel: Channel,
}

pub struct ConnectDisconnectLogger {
    connection_state: Arc<Mutex<ConnectionState>>,
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    connect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    disconnect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    inspect_metadata_node: Mutex<InspectMetadataNode>,
    time_series_stats: Arc<Mutex<dyn ConnectDisconnectTimeSeries>>,
}

impl ConnectDisconnectLogger {
    pub fn new(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: &InspectNode,
        inspect_metadata_node: &InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
    ) -> (Self, Arc<Mutex<dyn TimeSeriesStats>>) {
        let time_series_stats = Arc::new(Mutex::new(ConnectDisconnectTimeSeriesImpl::new()));
        let logger = Self::new_helper(
            cobalt_1dot1_proxy,
            inspect_node,
            inspect_metadata_node,
            persistence_req_sender,
            time_series_stats.clone(),
        );
        (logger, time_series_stats)
    }

    fn new_helper(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: &InspectNode,
        inspect_metadata_node: &InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
        time_series_stats: Arc<Mutex<dyn ConnectDisconnectTimeSeries>>,
    ) -> Self {
        let connect_events = inspect_node.create_child("connect_events");
        let disconnect_events = inspect_node.create_child("disconnect_events");
        let this = Self {
            cobalt_1dot1_proxy,
            connection_state: Arc::new(Mutex::new(ConnectionState::Idle(IdleState {}))),
            connect_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(connect_events, INSPECT_CONNECT_EVENTS_LIMIT),
                "wlan-connect-events",
                persistence_req_sender.clone(),
            )),
            disconnect_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(disconnect_events, INSPECT_DISCONNECT_EVENTS_LIMIT),
                "wlan-disconnect-events",
                persistence_req_sender.clone(),
            )),
            inspect_metadata_node: Mutex::new(InspectMetadataNode::new(inspect_metadata_node)),
            time_series_stats,
        };
        this.log_connection_state();
        this
    }

    fn inspect_wlan_connectivity_state(&self) -> InspectWlanConnectivityState {
        let state_name = match *self.connection_state.lock() {
            ConnectionState::Idle(_) => IDLE_STATE_NAME,
            ConnectionState::Disconnected(_) => DISCONNECTED_STATE_NAME,
            ConnectionState::Connected(_) => CONNECTED_STATE_NAME,
        };
        InspectWlanConnectivityState { state_name: state_name.to_string() }
    }

    fn update_connection_state(&self, state: ConnectionState) {
        *self.connection_state.lock() = state;
        self.log_connection_state();
    }

    fn log_connection_state(&self) {
        let wlan_connectivity_state = self.inspect_wlan_connectivity_state();
        let wlan_connectivity_state_id = self
            .inspect_metadata_node
            .lock()
            .wlan_connectivity_states
            .record_item(wlan_connectivity_state);
        self.time_series_stats.lock().log_wlan_connectivity_state(1 << wlan_connectivity_state_id);
    }

    pub fn handle_periodic_telemetry(&self) {
        self.log_connection_state();
    }

    pub async fn log_connect_attempt(
        &self,
        result: fidl_ieee80211::StatusCode,
        bss: &BssDescription,
    ) {
        let mut metric_events = vec![];
        metric_events.push(MetricEvent {
            metric_id: metrics::CONNECT_ATTEMPT_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
            event_codes: vec![result as u32],
            payload: MetricEventPayload::Count(1),
        });

        if result == fidl_ieee80211::StatusCode::Success {
            self.update_connection_state(ConnectionState::Connected(ConnectedState {}));

            let mut inspect_metadata_node = self.inspect_metadata_node.lock();
            let connected_network = InspectConnectedNetwork::from(bss);
            let connected_network_id =
                inspect_metadata_node.connected_networks.record_item(connected_network);

            let mut time_series_stats = self.time_series_stats.lock();
            time_series_stats.log_connected_networks(1 << connected_network_id);

            inspect_log!(self.connect_events_node.lock().get_mut(), {
                network_id: connected_network_id,
            });
        } else {
            self.update_connection_state(ConnectionState::Idle(IdleState {}));
        }

        log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_connect_attempt_cobalt_metrics",
        );
    }

    pub async fn log_disconnect(&self, info: &DisconnectInfo) {
        self.update_connection_state(ConnectionState::Disconnected(DisconnectedState {}));

        let mut inspect_metadata_node = self.inspect_metadata_node.lock();
        let connected_network = InspectConnectedNetwork::from(&*info.original_bss_desc);
        let connected_network_id =
            inspect_metadata_node.connected_networks.record_item(connected_network);
        let disconnect_source = InspectDisconnectSource::from(&info.disconnect_source);
        let disconnect_source_id =
            inspect_metadata_node.disconnect_sources.record_item(disconnect_source);
        inspect_log!(self.disconnect_events_node.lock().get_mut(), {
            connected_duration: info.connected_duration.into_nanos(),
            disconnect_source_id: disconnect_source_id,
            network_id: connected_network_id,
            rssi_dbm: info.current_rssi_dbm,
            snr_db: info.current_snr_db,
            channel: format!("{}", info.current_channel),
        });

        let mut time_series_stats = self.time_series_stats.lock();
        time_series_stats.log_disconnected_networks(1 << connected_network_id);
        time_series_stats.log_disconnect_sources(1 << disconnect_source_id);
    }
}

struct InspectMetadataNode {
    wlan_connectivity_states: InspectBoundedSetNode<InspectWlanConnectivityState>,
    connected_networks: InspectBoundedSetNode<InspectConnectedNetwork>,
    disconnect_sources: InspectBoundedSetNode<InspectDisconnectSource>,
}

impl InspectMetadataNode {
    fn new(inspect_node: &InspectNode) -> Self {
        let wlan_connectivity_states = inspect_node.create_child("wlan_connectivity_states");
        let connected_networks = inspect_node.create_child("connected_networks");
        let disconnect_sources = inspect_node.create_child("disconnect_sources");
        let mut this = Self {
            wlan_connectivity_states: InspectBoundedSetNode::new(
                wlan_connectivity_states,
                INSPECT_WLAN_CONNECTIVITY_STATES_ID_LIMIT,
            ),
            connected_networks: InspectBoundedSetNode::new_with_eq_fn(
                connected_networks,
                INSPECT_CONNECTED_NETWORKS_ID_LIMIT,
                |left, right| left.bssid == right.bssid,
            ),
            disconnect_sources: InspectBoundedSetNode::new(
                disconnect_sources,
                INSPECT_DISCONNECT_SOURCES_ID_LIMIT,
            ),
        };
        this.initialize();
        this
    }

    fn initialize(&mut self) {
        for state_name in CONNECTION_STATE_NAMES {
            let state = InspectWlanConnectivityState { state_name: state_name.to_string() };
            let _id = self.wlan_connectivity_states.record_item(state);
        }
    }
}

// Trait to help with unit testing
trait ConnectDisconnectTimeSeries: std::fmt::Debug + Send {
    fn log_wlan_connectivity_state(&mut self, data: u64);
    fn log_connected_networks(&mut self, data: u64);
    fn log_disconnected_networks(&mut self, data: u64);
    fn log_disconnect_sources(&mut self, data: u64);
}

#[derive(Debug)]
struct ConnectDisconnectTimeSeriesImpl {
    wlan_connectivity_states: TimeMatrix<Union<BitSet<u64>>, Constant>,
    connected_networks: TimeMatrix<Union<BitSet<u64>>, Constant>,
    disconnected_networks: TimeMatrix<Union<BitSet<u64>>, Constant>,
    disconnect_sources: TimeMatrix<Union<BitSet<u64>>, Constant>,
}

impl ConnectDisconnectTimeSeriesImpl {
    pub fn new() -> Self {
        Self {
            wlan_connectivity_states: TimeMatrix::new(
                SamplingProfile::Granular,
                Constant::default(),
            ),
            connected_networks: TimeMatrix::default(),
            disconnected_networks: TimeMatrix::default(),
            disconnect_sources: TimeMatrix::default(),
        }
    }
}

impl ConnectDisconnectTimeSeries for ConnectDisconnectTimeSeriesImpl {
    fn log_wlan_connectivity_state(&mut self, data: u64) {
        if let Err(e) = self.wlan_connectivity_states.fold(TimedSample::now(data)) {
            warn!("Failed logging wlan_connectivity_states sample: {:?}", e);
        };
    }

    fn log_connected_networks(&mut self, data: u64) {
        if let Err(e) = self.connected_networks.fold(TimedSample::now(data)) {
            warn!("Failed logging connected_networks sample: {:?}", e);
        };
    }

    fn log_disconnected_networks(&mut self, data: u64) {
        if let Err(e) = self.disconnected_networks.fold(TimedSample::now(data)) {
            warn!("Failed logging disconnected_networks sample: {:?}", e);
        };
    }

    fn log_disconnect_sources(&mut self, data: u64) {
        if let Err(e) = self.disconnect_sources.fold(TimedSample::now(data)) {
            warn!("Failed logging disconnect_sources sample: {:?}", e);
        };
    }
}

impl TimeSeriesStats for ConnectDisconnectTimeSeriesImpl {
    fn interpolate_data(&mut self) {
        let now = Timestamp::now();
        if let Err(e) = self.wlan_connectivity_states.interpolate(now) {
            warn!("Failed to interpolate wlan_connectivity_states: {:?}", e);
        }
        if let Err(e) = self.connected_networks.interpolate(now) {
            warn!("Failed to interpolate connected_networks: {:?}", e);
        }
        if let Err(e) = self.disconnected_networks.interpolate(now) {
            warn!("Failed to interpolate disconnected_networks: {:?}", e);
        }
        if let Err(e) = self.disconnect_sources.interpolate(now) {
            warn!("Failed to interpolate disconnect_sources: {:?}", e);
        }
    }

    fn log_inspect(&mut self, node: &InspectNode) {
        let now = Timestamp::now();
        match self.wlan_connectivity_states.interpolate_and_get_buffers(now) {
            Ok(bytes) => node.record_bytes("wlan_connectivity_states", bytes),
            Err(e) => node.record_string("wlan_connectivity_states_error", format!("{:?}", e)),
        }
        match self.connected_networks.interpolate_and_get_buffers(now) {
            Ok(bytes) => node.record_bytes("connected_networks", bytes),
            Err(e) => node.record_string("connected_networks_error", format!("{:?}", e)),
        }
        match self.disconnected_networks.interpolate_and_get_buffers(now) {
            Ok(bytes) => node.record_bytes("disconnected_networks", bytes),
            Err(e) => node.record_string("disconnected_networks_error", format!("{:?}", e)),
        }
        match self.disconnect_sources.interpolate_and_get_buffers(now) {
            Ok(bytes) => node.record_bytes("disconnect_sources", bytes),
            Err(e) => node.record_string("disconnect_sources_error", format!("{:?}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use diagnostics_assertions::{
        assert_data_tree, AnyBoolProperty, AnyBytesProperty, AnyNumericProperty, AnyStringProperty,
    };
    use fuchsia_zircon::DurationNum;
    use futures::task::Poll;
    use ieee80211_testutils::{BSSID_REGEX, SSID_REGEX};
    use rand::Rng;
    use std::pin::pin;
    use wlan_common::channel::{Cbw, Channel};
    use wlan_common::{fake_bss_description, random_bss_description};

    #[fuchsia::test]
    fn test_inspect_metadata_initialized() {
        let mut test_helper = setup_test();
        let time_series = Arc::new(Mutex::new(ConnectDisconnectTimeSeriesTestImpl::default()));
        let _logger = ConnectDisconnectLogger::new_helper(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
            time_series.clone(),
        );

        let data = test_helper.get_inspect_data_tree();
        assert_data_tree!(data, root: contains {
            test_stats: contains {
                metadata: contains {
                    wlan_connectivity_states: {
                        "0": {
                            "@time": AnyNumericProperty,
                            "data": { state_name: "Idle" },
                        },
                        "1": {
                            "@time": AnyNumericProperty,
                            "data": { state_name: "Disconnected" },
                        },
                        "2": {
                            "@time": AnyNumericProperty,
                            "data": { state_name: "Connected" },
                        },
                    }
                }
            }
        });

        assert_eq!(
            &time_series.lock().calls[..],
            &[ConnectDisconnectTimeSeriesCall::LogWlanConnectivityState(1 << 0)]
        );
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_inspect() {
        let mut test_helper = setup_test();
        let time_series = Arc::new(Mutex::new(ConnectDisconnectTimeSeriesTestImpl::default()));
        let logger = ConnectDisconnectLogger::new_helper(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
            time_series.clone(),
        );

        // Log the event
        let bss_description = random_bss_description!();
        let mut test_fut =
            pin!(logger.log_connect_attempt(fidl_ieee80211::StatusCode::Success, &bss_description));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        // Validate Inspect data
        let data = test_helper.get_inspect_data_tree();
        assert_data_tree!(data, root: contains {
            test_stats: contains {
                metadata: contains {
                    wlan_connectivity_states: contains {
                        "0": contains {
                            "data": { state_name: "Idle" },
                        },
                        "2": contains {
                            "data": { state_name: "Connected" },
                        }
                    },
                    connected_networks: contains {
                        "0": {
                            "@time": AnyNumericProperty,
                            "data": contains {
                                bssid: &*BSSID_REGEX,
                                ssid: &*SSID_REGEX,
                            }
                        }
                    },
                },
                connect_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        network_id: 0u64,
                    }
                }
            }
        });

        assert_eq!(
            &time_series.lock().calls[..],
            &[
                ConnectDisconnectTimeSeriesCall::LogWlanConnectivityState(1 << 0),
                ConnectDisconnectTimeSeriesCall::LogWlanConnectivityState(1 << 2),
                ConnectDisconnectTimeSeriesCall::LogConnectedNetworks(1 << 0),
            ]
        );
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_cobalt() {
        let mut test_helper = setup_test();
        let (logger, _time_series) = ConnectDisconnectLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
        );

        // Generate BSS Description
        let bss_description = random_bss_description!(Wpa2,
            channel: Channel::new(157, Cbw::Cbw40),
            bssid: [0x00, 0xf6, 0x20, 0x03, 0x04, 0x05],
        );

        // Log the event
        let mut test_fut =
            pin!(logger.log_connect_attempt(fidl_ieee80211::StatusCode::Success, &bss_description));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        // Validate Cobalt data
        let breakdowns_by_status_code = test_helper
            .get_logged_metrics(metrics::CONNECT_ATTEMPT_BREAKDOWN_BY_STATUS_CODE_METRIC_ID);
        assert_eq!(breakdowns_by_status_code.len(), 1);
        assert_eq!(
            breakdowns_by_status_code[0].event_codes,
            vec![fidl_ieee80211::StatusCode::Success as u32]
        );
        assert_eq!(breakdowns_by_status_code[0].payload, MetricEventPayload::Count(1));
    }

    #[fuchsia::test]
    fn test_log_disconnect_inspect() {
        let mut test_helper = setup_test();
        let time_series = Arc::new(Mutex::new(ConnectDisconnectTimeSeriesTestImpl::default()));
        let logger = ConnectDisconnectLogger::new_helper(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
            time_series.clone(),
        );

        // Log the event
        let bss_description = fake_bss_description!(Open);
        let channel = bss_description.channel;
        let disconnect_info = DisconnectInfo {
            connected_duration: 30.seconds(),
            is_sme_reconnecting: false,
            disconnect_source: fidl_sme::DisconnectSource::Ap(fidl_sme::DisconnectCause {
                mlme_event_name: fidl_sme::DisconnectMlmeEventName::DeauthenticateIndication,
                reason_code: fidl_ieee80211::ReasonCode::UnspecifiedReason,
            }),
            original_bss_desc: Box::new(bss_description),
            current_rssi_dbm: -30,
            current_snr_db: 25,
            current_channel: channel,
        };
        let mut test_fut = pin!(logger.log_disconnect(&disconnect_info));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        // Validate Inspect data
        let data = test_helper.get_inspect_data_tree();
        assert_data_tree!(data, root: contains {
            test_stats: contains {
                metadata: {
                    wlan_connectivity_states: contains {
                        "0": contains {
                            "data": { state_name: "Idle" },
                        },
                        "1": contains {
                            "data": { state_name: "Disconnected" },
                        }
                    },
                    connected_networks: {
                        "0": {
                            "@time": AnyNumericProperty,
                            "data": {
                                bssid: &*BSSID_REGEX,
                                ssid: &*SSID_REGEX,
                                ht_cap: AnyBytesProperty,
                                vht_cap: AnyBytesProperty,
                                protection: "Open",
                                is_wmm_assoc: AnyBoolProperty,
                                wmm_param: AnyBytesProperty,
                            }
                        }
                    },
                    disconnect_sources: {
                        "0": {
                            "@time": AnyNumericProperty,
                            "data": {
                                source: "ap",
                                reason: "UnspecifiedReason",
                                mlme_event_name: "DeauthenticateIndication",
                            }
                        }
                    },
                },
                disconnect_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        connected_duration: 30.seconds().into_nanos(),
                        disconnect_source_id: 0u64,
                        network_id: 0u64,
                        rssi_dbm: -30i64,
                        snr_db: 25i64,
                        channel: AnyStringProperty,
                    }
                }
            }
        });
        assert_eq!(
            &time_series.lock().calls[..],
            &[
                ConnectDisconnectTimeSeriesCall::LogWlanConnectivityState(1 << 0),
                ConnectDisconnectTimeSeriesCall::LogWlanConnectivityState(1 << 1),
                ConnectDisconnectTimeSeriesCall::LogDisconnectedNetworks(1 << 0),
                ConnectDisconnectTimeSeriesCall::LogDisconnectSources(1 << 0),
            ]
        );
    }

    #[derive(Debug, PartialEq)]
    enum ConnectDisconnectTimeSeriesCall {
        LogWlanConnectivityState(u64),
        LogConnectedNetworks(u64),
        LogDisconnectedNetworks(u64),
        LogDisconnectSources(u64),
    }

    #[derive(Debug, Default)]
    struct ConnectDisconnectTimeSeriesTestImpl {
        calls: Vec<ConnectDisconnectTimeSeriesCall>,
    }

    impl ConnectDisconnectTimeSeries for ConnectDisconnectTimeSeriesTestImpl {
        fn log_wlan_connectivity_state(&mut self, data: u64) {
            self.calls.push(ConnectDisconnectTimeSeriesCall::LogWlanConnectivityState(data));
        }
        fn log_connected_networks(&mut self, data: u64) {
            self.calls.push(ConnectDisconnectTimeSeriesCall::LogConnectedNetworks(data));
        }
        fn log_disconnected_networks(&mut self, data: u64) {
            self.calls.push(ConnectDisconnectTimeSeriesCall::LogDisconnectedNetworks(data));
        }
        fn log_disconnect_sources(&mut self, data: u64) {
            self.calls.push(ConnectDisconnectTimeSeriesCall::LogDisconnectSources(data));
        }
    }
}
