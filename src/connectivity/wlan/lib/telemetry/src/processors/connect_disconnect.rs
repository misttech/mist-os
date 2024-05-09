// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::util::cobalt_logger::log_cobalt_1dot1_batch,
    fidl_fuchsia_metrics::{MetricEvent, MetricEventPayload},
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_inspect::Node as InspectNode,
    fuchsia_inspect_contrib::{
        auto_persist::{
            AutoPersist, {self},
        },
        inspect_insert, inspect_log,
        nodes::BoundedListNode,
    },
    fuchsia_sync::Mutex,
    fuchsia_zircon as zx,
    std::sync::Arc,
    tracing::info,
    wlan_common::bss::BssDescription,
    wlan_legacy_metrics_registry as metrics,
};

const INSPECT_CONNECT_EVENTS_LIMIT: usize = 7;
const INSPECT_DISCONNECT_EVENTS_LIMIT: usize = 7;

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

#[derive(Clone, Debug, PartialEq)]
pub struct DisconnectInfo {
    pub connected_duration: zx::Duration,
    pub is_sme_reconnecting: bool,
    pub disconnect_source: fidl_sme::DisconnectSource,
}

pub struct ConnectDisconnectLogger {
    connection_state: Arc<Mutex<ConnectionState>>,
    cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
    _inspect_node: InspectNode,
    connect_events_node: Mutex<AutoPersist<BoundedListNode>>,
    _disconnect_events_node: Mutex<AutoPersist<BoundedListNode>>,
}

impl ConnectDisconnectLogger {
    pub fn new(
        cobalt_1dot1_proxy: fidl_fuchsia_metrics::MetricEventLoggerProxy,
        inspect_node: InspectNode,
        persistence_req_sender: auto_persist::PersistenceReqSender,
    ) -> Self {
        let connect_events = inspect_node.create_child("connect_events");
        let disconnect_events = inspect_node.create_child("disconnect_events");
        Self {
            cobalt_1dot1_proxy,
            connection_state: Arc::new(Mutex::new(ConnectionState::Idle(IdleState {}))),
            _inspect_node: inspect_node,
            connect_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(connect_events, INSPECT_CONNECT_EVENTS_LIMIT),
                "wlan-connect-events",
                persistence_req_sender.clone(),
            )),
            _disconnect_events_node: Mutex::new(AutoPersist::new(
                BoundedListNode::new(disconnect_events, INSPECT_DISCONNECT_EVENTS_LIMIT),
                "wlan-disconnect-events",
                persistence_req_sender.clone(),
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

    pub async fn log_disconnect(&self) {
        *self.connection_state.lock() = ConnectionState::Disconnected(DisconnectedState {});
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::testing::*,
        diagnostics_assertions::{assert_data_tree, AnyNumericProperty},
        futures::task::Poll,
        ieee80211_testutils::{BSSID_REGEX, SSID_REGEX},
        rand::Rng,
        std::pin::pin,
        wlan_common::{
            channel::{Cbw, Channel},
            random_bss_description,
        },
    };

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
}
