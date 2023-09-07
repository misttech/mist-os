// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::service::{ConnectedResult, WlanixService},
    crate::{
        build_nl80211_ack, build_nl80211_done, build_nl80211_message,
        nl80211::{Nl80211Attr, Nl80211Cmd},
        nl80211_message_resp, WifiState,
    },
    anyhow::{format_err, Context, Error},
    async_trait::async_trait,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_common_security as fidl_security,
    fidl_fuchsia_wlan_device_service as fidl_device_service,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
    fidl_fuchsia_wlan_wlanix as fidl_wlanix,
    fuchsia_async::{self as fasync, TimeoutExt},
    fuchsia_zircon as zx,
    futures::TryStreamExt,
    parking_lot::Mutex,
    std::{
        convert::{TryFrom, TryInto},
        sync::Arc,
    },
    tracing::{error, info},
    wlan_common::{bss::BssDescription, channel::Channel},
};

#[derive(Default, Clone)]
struct LastScanResults {
    iface_id: u16,
    results: Vec<fidl_sme::ScanResult>,
}

pub(crate) struct SmeService {
    monitor_svc: fidl_device_service::DeviceMonitorProxy,
    last_scan_results: Mutex<LastScanResults>,
}

impl SmeService {
    pub fn new() -> Result<Self, Error> {
        let monitor_svc = fuchsia_component::client::connect_to_protocol::<
            fidl_device_service::DeviceMonitorMarker,
        >()
        .context("failed to connect to device monitor")?;
        Ok(Self { monitor_svc, last_scan_results: Mutex::new(Default::default()) })
    }

    async fn passive_scan(&self, iface_id: u16) -> Result<(), Error> {
        let (sme_proxy, server) = create_proxy::<fidl_sme::ClientSmeMarker>()?;
        self.monitor_svc.get_client_sme(iface_id, server).await?.map_err(zx::Status::from_raw)?;
        let scan_request = fidl_sme::ScanRequest::Passive(fidl_sme::PassiveScanRequest);
        let scan_result_vmo = sme_proxy
            .scan(&scan_request)
            .await
            .context("Failed to request scan")?
            .map_err(|e| format_err!("Scan ended with error: {:?}", e))?;
        info!("Got scan results from SME.");
        *self.last_scan_results.lock() =
            LastScanResults { iface_id, results: wlan_common::scan::read_vmo(scan_result_vmo)? };
        Ok(())
    }
}

#[async_trait]
impl WlanixService for SmeService {
    async fn get_nl80211_interfaces(
        &self,
        responder: fidl_wlanix::Nl80211MessageResponder,
    ) -> Result<(), Error> {
        let mut resp = vec![];
        let ifaces = self.monitor_svc.list_ifaces().await?;
        for iface_id in ifaces {
            let iface_info = self
                .monitor_svc
                .query_iface(iface_id)
                .await?
                .map_err(zx::Status::from_raw)
                .context("Could not query iface info")?;
            resp.push(build_nl80211_message(
                Nl80211Cmd::NewInterface,
                vec![
                    Nl80211Attr::IfaceIndex(iface_id.into()),
                    // TODO(fxbug.dev/128604): Populate this with the real iface name assigned by netcfg.
                    Nl80211Attr::IfaceName(crate::IFACE_NAME.to_string()),
                    Nl80211Attr::Mac(iface_info.sta_addr),
                ],
            ));
        }
        resp.push(build_nl80211_done());
        responder.send(Ok(nl80211_message_resp(resp))).context("Failed to send scan results")
    }

    async fn trigger_nl80211_scan(
        &self,
        req_attrs: Vec<Nl80211Attr>,
        responder: fidl_wlanix::Nl80211MessageResponder,
        state: Arc<Mutex<WifiState>>,
    ) -> Result<(), Error> {
        let iface_id = req_attrs
            .iter()
            .filter_map(|attr| match attr {
                Nl80211Attr::IfaceIndex(idx) => Some(idx),
                _ => None,
            })
            .next()
            .ok_or(format_err!("No iface id specified in NL80211 scan request"))?;

        responder
            .send(Ok(nl80211_message_resp(vec![build_nl80211_ack()])))
            .context("Failed to ack TriggerScan")?;

        match self.passive_scan((*iface_id).try_into()?).await {
            Ok(()) => info!("Passive scan completed successfully"),
            Err(e) => error!("Failed to run passive scan: {:?}", e),
        }

        if let Some(proxy) = state.lock().scan_multicast_proxy.as_ref() {
            proxy
                .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                    message: Some(build_nl80211_message(
                        Nl80211Cmd::NewScanResults,
                        vec![Nl80211Attr::IfaceIndex(*iface_id)],
                    )),
                    ..Default::default()
                })
                .context("Failed to send NewScanResults")?;
        }
        Ok(())
    }

    fn get_nl80211_scan(
        &self,
        responder: fidl_wlanix::Nl80211MessageResponder,
    ) -> Result<(), Error> {
        let locked_results = self.last_scan_results.lock().clone();
        info!("Processing {} scan results", locked_results.results.len());
        let mut resp = vec![];
        for result in locked_results.results {
            resp.push(build_nl80211_message(
                Nl80211Cmd::NewScanResults,
                vec![
                    Nl80211Attr::IfaceIndex(locked_results.iface_id.into()),
                    convert_scan_result(result),
                ],
            ));
        }
        resp.push(build_nl80211_done());
        responder.send(Ok(nl80211_message_resp(resp))).context("Failed to send scan results")
    }

    async fn connect_to_network(&self, ssid: &[u8]) -> Result<ConnectedResult, Error> {
        let last_scan_results = self.last_scan_results.lock().clone();
        let selected_bss_description = last_scan_results
            .results
            .iter()
            .filter_map(|r| {
                // TODO(fxbug.dev/128604): handle the case when there are multiple BSS candidates
                BssDescription::try_from(r.bss_description.clone())
                    .ok()
                    .filter(|bss_description| bss_description.ssid == *ssid)
            })
            .next();

        let bss_description = match selected_bss_description {
            Some(bss_description) => bss_description,
            None => {
                return Err(format_err!("Requested network not found"));
            }
        };

        info!("Selected BSS to connect to");
        let iface_id = last_scan_results.iface_id;
        let (sme_proxy, server) = create_proxy::<fidl_sme::ClientSmeMarker>()?;
        self.monitor_svc.get_client_sme(iface_id, server).await?.map_err(zx::Status::from_raw)?;
        let (connect_txn, remote) = create_proxy()?;
        let bssid = bss_description.bssid;
        let connect_req = fidl_sme::ConnectRequest {
            ssid: bss_description.ssid.clone().into(),
            bss_description: bss_description.into(),
            multiple_bss_candidates: false,
            authentication: fidl_security::Authentication {
                protocol: fidl_security::Protocol::Open,
                credentials: None,
            },
            deprecated_scan_type: fidl_common::ScanType::Passive,
        };
        sme_proxy.connect(&connect_req, Some(remote))?;

        info!("Waiting for connect result from SME");
        let stream = connect_txn.take_event_stream();
        let sme_result = wait_for_connect_result(stream)
            .on_timeout(zx::Duration::from_seconds(30), || {
                Err(format_err!("Timed out waiting for connect result from SME."))
            })
            .await?;

        info!("Received connect result from SME: {:?}", sme_result);
        if sme_result.code == fidl_ieee80211::StatusCode::Success {
            Ok(ConnectedResult { ssid: ssid.to_vec(), bssid })
        } else {
            Err(format_err!("Connect failed with status code: {:?}", sme_result.code))
        }
    }
}

/// Wait until stream returns an OnConnectResult event or None. Ignore other event types.
async fn wait_for_connect_result(
    mut stream: fidl_sme::ConnectTransactionEventStream,
) -> Result<fidl_sme::ConnectResult, Error> {
    loop {
        let stream_fut = stream.try_next();
        match stream_fut
            .await
            .map_err(|e| format_err!("Failed to receive connect result from sme: {:?}", e))?
        {
            Some(fidl_sme::ConnectTransactionEvent::OnConnectResult { result }) => {
                return Ok(result)
            }
            Some(other) => {
                info!(
                    "Expected ConnectTransactionEvent::OnConnectResult, got {}. Ignoring.",
                    connect_txn_event_name(&other)
                );
            }
            None => {
                return Err(format_err!(
                    "Server closed the ConnectTransaction channel before sending a response"
                ));
            }
        };
    }
}

fn connect_txn_event_name(event: &fidl_sme::ConnectTransactionEvent) -> &'static str {
    match event {
        fidl_sme::ConnectTransactionEvent::OnConnectResult { .. } => "OnConnectResult",
        fidl_sme::ConnectTransactionEvent::OnDisconnect { .. } => "OnDisconnect",
        fidl_sme::ConnectTransactionEvent::OnSignalReport { .. } => "OnSignalReport",
        fidl_sme::ConnectTransactionEvent::OnChannelSwitched { .. } => "OnChannelSwitched",
    }
}

fn convert_scan_result(result: fidl_sme::ScanResult) -> Nl80211Attr {
    use crate::nl80211::{ChainSignalAttr, Nl80211BssAttr};
    let channel = Channel::try_from(result.bss_description.channel).unwrap();
    let center_freq = channel.get_center_freq().expect("Failed to get center freq").into();
    Nl80211Attr::Bss(vec![
        Nl80211BssAttr::Bssid(result.bss_description.bssid),
        Nl80211BssAttr::Frequency(center_freq),
        Nl80211BssAttr::InformationElement(result.bss_description.ies),
        Nl80211BssAttr::LastSeenBoottime(fasync::Time::now().into_nanos() as u64),
        Nl80211BssAttr::SignalMbm(result.bss_description.rssi_dbm as i32 * 100),
        Nl80211BssAttr::Capability(result.bss_description.capability_info),
        Nl80211BssAttr::Status(0),
        // TODO(fxbug.dev/128604): Determine whether we should provide real chain signals.
        Nl80211BssAttr::ChainSignal(vec![ChainSignalAttr {
            id: 0,
            rssi: result.bss_description.rssi_dbm.into(),
        }]),
    ])
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::Nl80211,
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_wlan_internal as fidl_internal,
        futures::{task::Poll, StreamExt},
        netlink_packet_core::{NetlinkDeserializable, NetlinkHeader},
        netlink_packet_generic::GenlMessage,
        wlan_common::assert_variant,
    };

    const FAKE_MAC_ADDR: [u8; 6] = [1, 2, 3, 4, 5, 6];

    fn fake_scan_result() -> fidl_sme::ScanResult {
        fidl_sme::ScanResult {
            compatibility: None,
            timestamp_nanos: 1000,
            bss_description: fidl_internal::BssDescription {
                bssid: [1, 2, 3, 4, 5, 6],
                bss_type: fidl_common::BssType::Infrastructure,
                beacon_period: 100,
                capability_info: 123,
                ies: vec![1, 2, 3, 2, 1],
                channel: fidl_common::WlanChannel {
                    primary: 1,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                rssi_dbm: -40,
                snr_db: -50,
            },
        }
    }

    #[test]
    fn test_convert_scan_result() {
        let exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(12345));

        let converted = convert_scan_result(fake_scan_result().clone());

        let attrs = assert_variant!(converted, Nl80211Attr::Bss(attrs) => attrs);
        use crate::nl80211::{ChainSignalAttr, Nl80211BssAttr};
        assert!(attrs.contains(&Nl80211BssAttr::Bssid([1, 2, 3, 4, 5, 6])));
        assert!(attrs.contains(&Nl80211BssAttr::InformationElement(vec![1, 2, 3, 2, 1])));
        assert!(attrs.contains(&Nl80211BssAttr::Frequency(2412)));
        assert!(attrs.contains(&Nl80211BssAttr::LastSeenBoottime(12345)));
        assert!(attrs.contains(&Nl80211BssAttr::SignalMbm(-4000)));
        assert!(attrs.contains(&Nl80211BssAttr::Capability(123)));
        assert!(attrs
            .contains(&Nl80211BssAttr::ChainSignal(vec![ChainSignalAttr { id: 0, rssi: -40 }])));
    }

    fn fake_nl80211_responder(
        exec: &mut fasync::TestExecutor,
    ) -> (
        <fidl_wlanix::Nl80211Proxy as fidl_wlanix::Nl80211ProxyInterface>::MessageResponseFut,
        fidl_wlanix::Nl80211MessageResponder,
    ) {
        let (proxy, mut stream) = create_proxy_and_stream::<fidl_wlanix::Nl80211Marker>()
            .expect("Failed to create wlanix service");
        let message_fut = proxy.message(Default::default());
        let nl80211_responder = assert_variant!(
            exec.run_until_stalled(&mut stream.select_next_some()),
            Poll::Ready(Ok(fidl_wlanix::Nl80211Request::Message { responder, .. })) => responder);
        (message_fut, nl80211_responder)
    }

    // This will only work if the message is a parseable nl80211 message. Some
    // attributes are currently write only in our NL80211 implementation.
    fn expect_nl80211_message(message: &fidl_wlanix::Nl80211Message) -> GenlMessage<Nl80211> {
        assert_eq!(message.message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
        GenlMessage::deserialize(
            &NetlinkHeader::default(),
            &message.payload.as_ref().expect("Message should always have a payload")[..],
        )
        .expect("Failed to deserialize genetlink message")
    }

    #[test]
    fn test_get_nl80211_interfaces() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (monitor_svc, mut monitor_stream) =
            create_proxy_and_stream::<fidl_device_service::DeviceMonitorMarker>()
                .expect("Failed to create device monitor service");
        let service = SmeService { monitor_svc, last_scan_results: Mutex::new(Default::default()) };

        // Query interfaces.
        let (mut message_fut, responder) = fake_nl80211_responder(&mut exec);
        let mut fut = service.get_nl80211_interfaces(responder);

        // First query device monitor for the list of ifaces.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::ListIfaces { responder })) => responder);
        responder.send(&[1]).expect("Failed to respond to ListIfaces");

        // Second query device monitor for more info on each iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let responder = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::QueryIface { iface_id: 1, responder })) => responder);
        responder
            .send(Ok(&fidl_device_service::QueryIfaceResponse {
                role: fidl_common::WlanMacRole::Client,
                id: 1,
                phy_id: 1,
                phy_assigned_id: 1,
                sta_addr: FAKE_MAC_ADDR.clone(),
            }))
            .expect("Failed to respond to QueryIfaceResponse");

        // The iface should be sent in response.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));
        let responses = assert_variant!(
            exec.run_until_stalled(&mut message_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse { responses: Some(responses), ..}))) => responses);
        assert_eq!(responses.len(), 2);
        let message = expect_nl80211_message(&responses[0]);
        assert_eq!(message.payload.cmd, Nl80211Cmd::NewInterface);
        assert!(message.payload.attrs.iter().any(|attr| *attr == Nl80211Attr::IfaceIndex(1)));
        assert!(message.payload.attrs.iter().any(|attr| *attr == Nl80211Attr::Mac(FAKE_MAC_ADDR)));
        assert_eq!(responses[1].message_type, Some(fidl_wlanix::Nl80211MessageType::Done));
    }

    #[test]
    fn test_trigger_nl80211_scan() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (monitor_svc, mut monitor_stream) =
            create_proxy_and_stream::<fidl_device_service::DeviceMonitorMarker>()
                .expect("Failed to create device monitor service");
        let service = SmeService { monitor_svc, last_scan_results: Mutex::new(Default::default()) };

        // Get a fake nl80211 responder.
        let (mut message_fut, responder) = fake_nl80211_responder(&mut exec);

        // Start the scan attempt.
        let state = Arc::new(Mutex::new(WifiState::default()));
        let mut fut =
            service.trigger_nl80211_scan(vec![Nl80211Attr::IfaceIndex(1)], responder, state);

        // SmeServer should ack the request and then connect to the iface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        assert_variant!(exec.run_until_stalled(&mut message_fut), Poll::Ready(Ok(_)));

        let (sme_server, responder) = assert_variant!(
            exec.run_until_stalled(&mut monitor_stream.select_next_some()),
            Poll::Ready(Ok(fidl_device_service::DeviceMonitorRequest::GetClientSme { iface_id: 1, sme_server, responder })) => (sme_server, responder));
        let mut sme_stream = sme_server.into_stream().expect("Failed to get SME stream");
        responder.send(Ok(())).expect("Failed to respond to GetClientSme");

        // We should not yet have returned any scan results.
        assert!(service.last_scan_results.lock().results.is_empty());

        // We should initiate an SME scan on the selected interface.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Pending);
        let scan_responder = assert_variant!(
            exec.run_until_stalled(&mut sme_stream.select_next_some()),
            Poll::Ready(Ok(fidl_sme::ClientSmeRequest::Scan { responder, ..})) => responder
        );
        let scan_results_vmo = wlan_common::scan::write_vmo(vec![fake_scan_result()])
            .expect("Failed to write scan results VMO");
        scan_responder.send(Ok(scan_results_vmo)).expect("Failed to send scan response");

        // Scan results are delivered and everything is cleaned up.
        assert_variant!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(())));
        let last_results = service.last_scan_results.lock().clone();
        assert_eq!(last_results.results.len(), 1);
        assert_eq!(last_results.iface_id, 1);
    }

    #[test]
    fn test_get_nl80211_scan() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(fasync::Time::from_nanos(0));

        let (monitor_svc, _monitor_stream) =
            create_proxy_and_stream::<fidl_device_service::DeviceMonitorMarker>()
                .expect("Failed to create device monitor service");
        let service = SmeService {
            monitor_svc,
            last_scan_results: Mutex::new(LastScanResults {
                iface_id: 1,
                results: vec![fake_scan_result()],
            }),
        };

        // Get a fake nl80211 responder.
        let (mut message_fut, responder) = fake_nl80211_responder(&mut exec);

        // Request scan results
        service.get_nl80211_scan(responder).expect("Failed to get scan results");

        // Scan results should be sent through the responder.
        let responses = assert_variant!(
            exec.run_until_stalled(&mut message_fut),
            Poll::Ready(Ok(Ok(fidl_wlanix::Nl80211MessageResponse { responses: Some(responses), ..}))) => responses);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].message_type, Some(fidl_wlanix::Nl80211MessageType::Message));
        assert_eq!(responses[1].message_type, Some(fidl_wlanix::Nl80211MessageType::Done));
    }
}
