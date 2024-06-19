// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
use tracing::info;
use wlan_common::bss::BssDescription;
use wlan_common::channel::Channel;
use {
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_zircon as zx, wlan_legacy_metrics_registry as metrics,
};

const INSPECT_CONNECT_EVENTS_LIMIT: usize = 10;
const INSPECT_DISCONNECT_EVENTS_LIMIT: usize = 10;
const INSPECT_CONNECTED_NETWORKS_ID_LIMIT: usize = 16;
const INSPECT_DISCONNECT_SOURCES_ID_LIMIT: usize = 32;

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
    pub ap_state: ApState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApState {
    pub original: BssDescription,
    pub tracked: TrackedBss,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackedBss {
    pub signal: Signal,
    pub channel: Channel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    /// Calculated received signal strength for the beacon/probe response.
    pub rssi_dbm: i8,
    /// Signal to noise ratio for the beacon/probe response.
    pub snr_db: i8,
}

pub struct ConnectDisconnectLogger {
    connection_state: Arc<Mutex<ConnectionState>>,
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    _inspect_node: InspectNode,
    connect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    disconnect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    connected_networks_node: Mutex<InspectBoundedSetNode<InspectConnectedNetwork>>,
    disconnect_sources_node: Mutex<InspectBoundedSetNode<InspectDisconnectSource>>,
}

impl ConnectDisconnectLogger {
    pub fn new(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
    ) -> Self {
        let connect_events = inspect_node.create_child("connect_events");
        let disconnect_events = inspect_node.create_child("disconnect_events");
        let connected_networks = inspect_node.create_child("connected_networks");
        let disconnect_sources = inspect_node.create_child("disconnect_sources");
        Self {
            cobalt_1dot1_proxy,
            connection_state: Arc::new(Mutex::new(ConnectionState::Idle(IdleState {}))),
            _inspect_node: inspect_node,
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
            connected_networks_node: Mutex::new(InspectBoundedSetNode::new_with_eq_fn(
                connected_networks,
                INSPECT_CONNECTED_NETWORKS_ID_LIMIT,
                |left, right| left.bssid == right.bssid,
            )),
            disconnect_sources_node: Mutex::new(InspectBoundedSetNode::new(
                disconnect_sources,
                INSPECT_DISCONNECT_SOURCES_ID_LIMIT,
            )),
        }
    }

    pub async fn log_connect_attempt(&self, result: fidl_sme::ConnectResult, bss: &BssDescription) {
        let mut metric_events = vec![];
        metric_events.push(MetricEvent {
            metric_id: metrics::CONNECT_ATTEMPT_BREAKDOWN_BY_STATUS_CODE_METRIC_ID,
            event_codes: vec![result.code as u32],
            payload: MetricEventPayload::Count(1),
        });

        if result.code == fidl_ieee80211::StatusCode::Success {
            *self.connection_state.lock() = ConnectionState::Connected(ConnectedState {});
            inspect_log!(self.connect_events_node.lock().get_mut(), {
                network: {
                    bssid: bss.bssid.to_string(),
                    ssid: bss.ssid.to_string(),
                },
            });
        } else {
            *self.connection_state.lock() = ConnectionState::Idle(IdleState {});
        }

        log_cobalt_1dot1_batch!(
            self.cobalt_1dot1_proxy,
            &metric_events,
            "log_connect_attempt_cobalt_metrics",
        );
    }

    pub async fn log_disconnect(&self, info: &DisconnectInfo) {
        self.log_disconnect_inspect(info);
        *self.connection_state.lock() = ConnectionState::Disconnected(DisconnectedState {});
    }

    fn log_disconnect_inspect(&self, info: &DisconnectInfo) {
        let connected_network = InspectConnectedNetwork::from(&info.ap_state.original);
        let connected_network_id =
            self.connected_networks_node.lock().record_item(connected_network);
        let disconnect_source = InspectDisconnectSource::from(&info.disconnect_source);
        let disconnect_source_id =
            self.disconnect_sources_node.lock().record_item(disconnect_source);
        inspect_log!(self.disconnect_events_node.lock().get_mut(), {
            connected_duration: info.connected_duration.into_nanos(),
            disconnect_source_id: disconnect_source_id,
            network_id: connected_network_id,
            rssi_dbm: info.ap_state.tracked.signal.rssi_dbm,
            snr_db: info.ap_state.tracked.signal.snr_db,
            channel: format!("{}", info.ap_state.tracked.channel),
        });
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
    fn test_log_connect_attempt_inspect() {
        let mut test_helper = setup_test();
        let logger = ConnectDisconnectLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.create_inspect_node("test_stats"),
            test_helper.persistence_sender.clone(),
        );

        // Log the event
        let bss_description = random_bss_description!();
        let mut test_fut = pin!(logger.log_connect_attempt(
            fidl_sme::ConnectResult {
                code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
                is_credential_rejected: false,
                is_reconnect: false,
            },
            &bss_description
        ));
        assert_eq!(
            test_helper.run_until_stalled_drain_cobalt_events(&mut test_fut),
            Poll::Ready(())
        );

        // Validate Inspect data
        let data = test_helper.get_inspect_data_tree();
        assert_data_tree!(data, root: contains {
            test_stats: contains {
                connect_events: {
                    "0": {
                        "@time": AnyNumericProperty,
                        network: {
                            bssid: &*BSSID_REGEX,
                            ssid: &*SSID_REGEX,
                        }
                    }
                }
            }
        });
    }

    #[fuchsia::test]
    fn test_log_connect_attempt_cobalt() {
        let mut test_helper = setup_test();
        let logger = ConnectDisconnectLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.create_inspect_node("test_stats"),
            test_helper.persistence_sender.clone(),
        );

        // Generate BSS Description
        let bss_description = random_bss_description!(Wpa2,
            channel: Channel::new(157, Cbw::Cbw40),
            bssid: [0x00, 0xf6, 0x20, 0x03, 0x04, 0x05],
        );

        // Log the event
        let mut test_fut = pin!(logger.log_connect_attempt(
            fidl_sme::ConnectResult {
                code: fidl_fuchsia_wlan_ieee80211::StatusCode::Success,
                is_credential_rejected: false,
                is_reconnect: false,
            },
            &bss_description
        ));
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
        let logger = ConnectDisconnectLogger::new(
            test_helper.cobalt_1dot1_proxy.clone(),
            test_helper.create_inspect_node("test_stats"),
            test_helper.persistence_sender.clone(),
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
            ap_state: ApState {
                original: bss_description,
                tracked: TrackedBss { signal: Signal { rssi_dbm: -30, snr_db: 25 }, channel },
            },
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
    }
}
