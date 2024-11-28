// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::util::cobalt_logger::log_cobalt_1dot1_batch;
use derivative::Derivative;
use fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload};
use fuchsia_inspect::Node as InspectNode;
use fuchsia_inspect_auto_persist::{self as auto_persist, AutoPersist};
use fuchsia_inspect_contrib::id_enum::{inspect_record_id_enum, IdEnum};
use fuchsia_inspect_contrib::nodes::{BoundedListNode, LruCacheNode};
use fuchsia_inspect_contrib::{inspect_insert, inspect_log};
use fuchsia_inspect_derive::Unit;
use fuchsia_sync::Mutex;
use std::sync::Arc;
use strum_macros::{Display, EnumIter};
use windowed_stats::experimental::clock::Timed;
use windowed_stats::experimental::series::interpolation::{Constant, LastSample};
use windowed_stats::experimental::series::statistic::Union;
use windowed_stats::experimental::series::{SamplingProfile, TimeMatrix};
use windowed_stats::experimental::serve::{
    InspectedTimeMatrix, InspectedTimeMatrixMetadata, TimeMatrixClient,
};
use wlan_common::bss::BssDescription;
use wlan_common::channel::Channel;
use {
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    wlan_legacy_metrics_registry as metrics,
};

const INSPECT_CONNECT_EVENTS_LIMIT: usize = 10;
const INSPECT_DISCONNECT_EVENTS_LIMIT: usize = 10;
const INSPECT_CONNECTED_NETWORKS_ID_LIMIT: usize = 16;
const INSPECT_DISCONNECT_SOURCES_ID_LIMIT: usize = 32;

#[derive(Debug, Display, EnumIter)]
enum ConnectionState {
    Idle(IdleState),
    Connected(ConnectedState),
    Disconnected(DisconnectedState),
}

impl IdEnum for ConnectionState {
    type Id = u8;
    fn to_id(&self) -> Self::Id {
        match self {
            Self::Idle(_) => 0,
            Self::Disconnected(_) => 1,
            Self::Connected(_) => 2,
        }
    }
}

#[derive(Debug, Default)]
struct IdleState {}

#[derive(Debug, Default)]
struct ConnectedState {}

#[derive(Debug, Default)]
struct DisconnectedState {}

#[derive(Derivative, Unit)]
#[derivative(PartialEq, Eq, Hash)]
struct InspectConnectedNetwork {
    bssid: String,
    ssid: String,
    protection: String,
    ht_cap: Option<Vec<u8>>,
    vht_cap: Option<Vec<u8>>,
    #[derivative(PartialEq = "ignore")]
    #[derivative(Hash = "ignore")]
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

#[derive(PartialEq, Unit, Hash)]
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

#[derive(PartialEq, Eq, Unit, Hash)]
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
    pub connected_duration: zx::MonotonicDuration,
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
    time_series_stats: ConnectDisconnectTimeSeries,
}

impl ConnectDisconnectLogger {
    pub fn new(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: &InspectNode,
        inspect_metadata_node: &InspectNode,
        inspect_metadata_path: &str,
        persistence_req_sender: auto_persist::PersistenceReqSender,
        time_matrix_client: &TimeMatrixClient,
    ) -> Self {
        Self::new_helper(
            cobalt_1dot1_proxy,
            inspect_node,
            inspect_metadata_node,
            persistence_req_sender,
            ConnectDisconnectTimeSeries::new(&time_matrix_client, inspect_metadata_path),
        )
    }

    fn new_helper(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: &InspectNode,
        inspect_metadata_node: &InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
        time_series_stats: ConnectDisconnectTimeSeries,
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
                persistence_req_sender,
            )),
            inspect_metadata_node: Mutex::new(InspectMetadataNode::new(inspect_metadata_node)),
            time_series_stats,
        };
        this.log_connection_state();
        this
    }

    fn update_connection_state(&self, state: ConnectionState) {
        *self.connection_state.lock() = state;
        self.log_connection_state();
    }

    fn log_connection_state(&self) {
        let wlan_connectivity_state_id = self.connection_state.lock().to_id() as u64;
        self.time_series_stats.log_wlan_connectivity_state(1 << wlan_connectivity_state_id);
    }

    pub fn is_connected(&self) -> bool {
        match &*self.connection_state.lock() {
            ConnectionState::Connected(_) => true,
            _ => false,
        }
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
                inspect_metadata_node.connected_networks.insert(connected_network) as u64;

            self.time_series_stats.log_connected_networks(1 << connected_network_id);

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
            inspect_metadata_node.connected_networks.insert(connected_network) as u64;
        let disconnect_source = InspectDisconnectSource::from(&info.disconnect_source);
        let disconnect_source_id =
            inspect_metadata_node.disconnect_sources.insert(disconnect_source) as u64;
        inspect_log!(self.disconnect_events_node.lock().get_mut(), {
            connected_duration: info.connected_duration.into_nanos(),
            disconnect_source_id: disconnect_source_id,
            network_id: connected_network_id,
            rssi_dbm: info.current_rssi_dbm,
            snr_db: info.current_snr_db,
            channel: format!("{}", info.current_channel),
        });

        self.time_series_stats.log_disconnected_networks(1 << connected_network_id);
        self.time_series_stats.log_disconnect_sources(1 << disconnect_source_id);
    }
}

struct InspectMetadataNode {
    connected_networks: LruCacheNode<InspectConnectedNetwork>,
    disconnect_sources: LruCacheNode<InspectDisconnectSource>,
}

impl InspectMetadataNode {
    const WLAN_CONNECTIVITY_STATES: &'static str = "wlan_connectivity_states";
    const CONNECTED_NETWORKS: &'static str = "connected_networks";
    const DISCONNECT_SOURCES: &'static str = "disconnect_sources";

    fn new(inspect_node: &InspectNode) -> Self {
        inspect_record_id_enum::<ConnectionState>(inspect_node, "wlan_connectivity_states");
        let connected_networks = inspect_node.create_child("connected_networks");
        let disconnect_sources = inspect_node.create_child("disconnect_sources");
        Self {
            connected_networks: LruCacheNode::new(
                connected_networks,
                INSPECT_CONNECTED_NETWORKS_ID_LIMIT,
            ),
            disconnect_sources: LruCacheNode::new(
                disconnect_sources,
                INSPECT_DISCONNECT_SOURCES_ID_LIMIT,
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct ConnectDisconnectTimeSeries {
    wlan_connectivity_states: InspectedTimeMatrix<u64>,
    connected_networks: InspectedTimeMatrix<u64>,
    disconnected_networks: InspectedTimeMatrix<u64>,
    disconnect_sources: InspectedTimeMatrix<u64>,
}

impl ConnectDisconnectTimeSeries {
    pub fn new(manager: &TimeMatrixClient, inspect_metadata_path: &str) -> Self {
        let wlan_connectivity_states = manager.inspect_time_matrix_with_metadata(
            "wlan_connectivity_states",
            TimeMatrix::<Union<u64>, LastSample>::new(
                SamplingProfile::highly_granular(),
                LastSample::or(0),
            ),
            InspectedTimeMatrixMetadata::default().with_bit_mapping(format!(
                "{inspect_metadata_path}/{}",
                InspectMetadataNode::WLAN_CONNECTIVITY_STATES
            )),
        );
        let connected_networks = manager.inspect_time_matrix_with_metadata(
            "connected_networks",
            TimeMatrix::<Union<u64>, Constant>::new(
                SamplingProfile::granular(),
                Constant::default(),
            ),
            InspectedTimeMatrixMetadata::default().with_bit_mapping(format!(
                "{inspect_metadata_path}/{}",
                InspectMetadataNode::CONNECTED_NETWORKS
            )),
        );
        let disconnected_networks = manager.inspect_time_matrix_with_metadata(
            "disconnected_networks",
            TimeMatrix::<Union<u64>, Constant>::new(
                SamplingProfile::granular(),
                Constant::default(),
            ),
            InspectedTimeMatrixMetadata::default()
                // `disconnected_networks` uses the same bit_mapping as `connected_networks`
                .with_bit_mapping(format!(
                    "{inspect_metadata_path}/{}",
                    InspectMetadataNode::CONNECTED_NETWORKS
                )),
        );
        let disconnect_sources = manager.inspect_time_matrix_with_metadata(
            "disconnect_sources",
            TimeMatrix::<Union<u64>, Constant>::new(
                SamplingProfile::granular(),
                Constant::default(),
            ),
            InspectedTimeMatrixMetadata::default().with_bit_mapping(format!(
                "{inspect_metadata_path}/{}",
                InspectMetadataNode::DISCONNECT_SOURCES
            )),
        );
        Self {
            wlan_connectivity_states,
            connected_networks,
            disconnected_networks,
            disconnect_sources,
        }
    }

    fn log_wlan_connectivity_state(&self, data: u64) {
        self.wlan_connectivity_states.fold_or_log_error(Timed::now(data));
    }
    fn log_connected_networks(&self, data: u64) {
        self.connected_networks.fold_or_log_error(Timed::now(data));
    }
    fn log_disconnected_networks(&self, data: u64) {
        self.disconnected_networks.fold_or_log_error(Timed::now(data));
    }
    fn log_disconnect_sources(&self, data: u64) {
        self.disconnect_sources.fold_or_log_error(Timed::now(data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use diagnostics_assertions::{
        assert_data_tree, AnyBoolProperty, AnyBytesProperty, AnyNumericProperty, AnyStringProperty,
    };

    use futures::task::Poll;
    use ieee80211_testutils::{BSSID_REGEX, SSID_REGEX};
    use rand::Rng;
    use std::pin::pin;
    use windowed_stats::experimental::serve::serve_time_matrix_inspection;
    use windowed_stats::experimental::testing::{MockTimeMatrix, TimeMatrixCall};
    use wlan_common::channel::{Cbw, Channel};
    use wlan_common::{fake_bss_description, random_bss_description};

    #[fuchsia::test]
    fn test_inspect_metadata_initialized() {
        let mut test_helper = setup_test();
        let time_series = MockConnectDisconnectTimeSeries::default();
        let _logger = ConnectDisconnectLogger::new_helper(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
            time_series.build(),
        );

        let data = test_helper.get_inspect_data_tree();
        assert_data_tree!(data, root: contains {
            test_stats: contains {
                metadata: contains {
                    wlan_connectivity_states: {
                        "0": "Idle",
                        "1": "Disconnected",
                        "2": "Connected",
                    }
                }
            }
        });

        assert_eq!(
            &time_series.wlan_connectivity_states.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(1 << 0))]
        );
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_inspect() {
        let mut test_helper = setup_test();
        let time_series = MockConnectDisconnectTimeSeries::default();
        let logger = ConnectDisconnectLogger::new_helper(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
            time_series.build(),
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
                    wlan_connectivity_states: {
                        "0": "Idle",
                        "1": "Disconnected",
                        "2": "Connected",
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
            &time_series.wlan_connectivity_states.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(1 << 0)), TimeMatrixCall::Fold(Timed::now(1 << 2)),]
        );
        assert_eq!(
            &time_series.connected_networks.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(1 << 0)),]
        );
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_cobalt() {
        let mut test_helper = setup_test();
        let (time_matrix_client, _fut) =
            serve_time_matrix_inspection(test_helper.inspect_node.create_child("time_series"));
        let logger = ConnectDisconnectLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            &test_helper.inspect_metadata_path,
            test_helper.persistence_sender.clone(),
            &time_matrix_client,
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
        let time_series = MockConnectDisconnectTimeSeries::default();
        let logger = ConnectDisconnectLogger::new_helper(
            test_helper.cobalt_1dot1_proxy.clone(),
            &test_helper.inspect_node,
            &test_helper.inspect_metadata_node,
            test_helper.persistence_sender.clone(),
            time_series.build(),
        );

        // Log the event
        let bss_description = fake_bss_description!(Open);
        let channel = bss_description.channel;
        let disconnect_info = DisconnectInfo {
            connected_duration: zx::MonotonicDuration::from_seconds(30),
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
                    wlan_connectivity_states: {
                        "0": "Idle",
                        "1": "Disconnected",
                        "2": "Connected",
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
                        connected_duration: zx::MonotonicDuration::from_seconds(30).into_nanos(),
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
            &time_series.wlan_connectivity_states.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(1 << 0)), TimeMatrixCall::Fold(Timed::now(1 << 1)),]
        );
        assert_eq!(
            &time_series.disconnected_networks.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(1 << 0)),]
        );
        assert_eq!(
            &time_series.disconnect_sources.drain_calls()[..],
            &[TimeMatrixCall::Fold(Timed::now(1 << 0)),]
        );
    }

    #[derive(Debug, Default)]
    struct MockConnectDisconnectTimeSeries {
        wlan_connectivity_states: MockTimeMatrix<u64>,
        connected_networks: MockTimeMatrix<u64>,
        disconnected_networks: MockTimeMatrix<u64>,
        disconnect_sources: MockTimeMatrix<u64>,
    }

    impl MockConnectDisconnectTimeSeries {
        fn build(&self) -> ConnectDisconnectTimeSeries {
            ConnectDisconnectTimeSeries {
                wlan_connectivity_states: self
                    .wlan_connectivity_states
                    .build_ref("wlan_connectivity_states"),
                connected_networks: self.connected_networks.build_ref("connected_networks"),
                disconnected_networks: self
                    .disconnected_networks
                    .build_ref("disconnected_networks"),
                disconnect_sources: self.disconnect_sources.build_ref("disconnect_sources"),
            }
        }
    }
}
