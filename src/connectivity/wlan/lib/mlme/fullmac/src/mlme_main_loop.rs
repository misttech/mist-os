// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::convert::{fullmac_to_mlme, mlme_to_fullmac};
use crate::device::DeviceOps;
use crate::wlan_fullmac_impl_ifc_request_handler::serve_wlan_fullmac_impl_ifc_request_handler;
use crate::{DriverState, FullmacDriverEvent, FullmacDriverEventSink};
use anyhow::{bail, Context};
use futures::channel::{mpsc, oneshot};
use futures::{select, Future, StreamExt};
use log::{error, info};
use std::pin::Pin;
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fuchsia_async as fasync,
};

/// Creates a future that implements the MLME main loop.
///
/// The MLME main loop is responsible for:
/// - Converting and sending MLME requests from |mlme_request_stream| to the vendor driver via
///   |device|.
/// - Receiving requests from the vendor driver through |fullmac_ifc_request_stream|, and
///   pushing them onto |driver_event_stream|.
pub(crate) fn create_mlme_main_loop<D: DeviceOps>(
    device: D,
    mlme_request_stream: wlan_sme::MlmeStream,
    mlme_event_sink: wlan_sme::MlmeEventSink,
    driver_event_stream: mpsc::UnboundedReceiver<FullmacDriverEvent>,
    driver_event_sink: FullmacDriverEventSink,
    fullmac_ifc_request_stream: fidl_fullmac::WlanFullmacImplIfcRequestStream,
) -> Pin<Box<impl Future<Output = anyhow::Result<()>>>> {
    let main_loop = MlmeMainLoop {
        device,
        mlme_request_stream,
        mlme_event_sink,
        driver_event_stream,
        is_bss_protected: false,
        device_link_state: fidl_mlme::ControlledPortState::Closed,
    };

    Box::pin(main_loop.serve(fullmac_ifc_request_stream, driver_event_sink))
}

struct MlmeMainLoop<D: DeviceOps> {
    device: D,
    mlme_request_stream: wlan_sme::MlmeStream,
    mlme_event_sink: wlan_sme::MlmeEventSink,
    driver_event_stream: mpsc::UnboundedReceiver<FullmacDriverEvent>,
    is_bss_protected: bool,
    device_link_state: fidl_mlme::ControlledPortState,
}

impl<D: DeviceOps> MlmeMainLoop<D> {
    /// Serves the MLME main loop.
    ///
    /// This:
    /// - Spawns the background task that implements the WlanFullmacImplIfc server.
    /// - Handles SME -> Vendor Driver requests by servicing |self.mlme_request_stream|.
    /// - Handles Vendor Driver -> SME requests by servicing |self.driver_event_stream|.
    ///
    /// |self.driver_event_stream| is populated by the WlanFullmacImplIfc server task, except for
    /// the `Stop` event which is sent by `FullmacMlmeHandle::stop`.
    ///
    /// Returns success if it receives the `Stop` event on |self.driver_event_stream|, and returns
    /// an error in all other cases.
    async fn serve(
        mut self,
        fullmac_ifc_request_stream: fidl_fullmac::WlanFullmacImplIfcRequestStream,
        driver_event_sink: FullmacDriverEventSink,
    ) -> anyhow::Result<()> {
        let mac_role = self
            .device
            .query_device_info()?
            .role
            .context("Vendor driver query response missing MAC role")?;

        // The WlanFullmacImplIfc server is a background task so that a blocking call into the
        // vendor driver does not prevent MLME from handling incoming WlanFullmacImplIfc requests.
        let (ifc_server_stop_sender, mut ifc_server_stop_receiver) = oneshot::channel::<()>();
        let _fullmac_ifc_server_task = fasync::Task::spawn(async move {
            serve_wlan_fullmac_impl_ifc_request_handler(
                fullmac_ifc_request_stream,
                driver_event_sink,
            )
            .await;
            info!("WlanFullmacImplIfc server stopped");

            // This signals that the fullmac_ifc_server_task exited before the main loop. It's fine
            // if the main loop exits and drops the receiver before this task exits, so we ignore
            // the result of this send.
            let _ = ifc_server_stop_sender.send(());
        });

        loop {
            select! {
                mlme_request = self.mlme_request_stream.next() => match mlme_request {
                    Some(req) => {
                        if let Err(e) = self.handle_mlme_request(req) {
                            error!("Failed to handle MLME req: {}", e);
                        }
                    },
                    None => bail!("MLME request stream terminated unexpectedly."),
                },
                driver_event = self.driver_event_stream.next() => match driver_event {
                    Some(event) => {
                        match self.handle_driver_event(event, &mac_role) {
                            Ok(DriverState::Running) => {},
                            Ok(DriverState::Stopping) => return Ok(()),
                            Err(e) => error!("Failed to handle driver event: {}", e),
                        }
                    },
                    None => bail!("Driver event stream terminated unexpectedly."),
                },
                ifc_stop = ifc_server_stop_receiver => match ifc_stop {
                    Ok(()) => bail!("WlanFullmacImplIfc request stream terminated unexpectedly."),
                    Err(e) => bail!("WlanFullmacImplIfc server task dropped stop sender unexpectedly. {}", e),
                },
            }
        }
    }

    fn handle_mlme_request(&mut self, req: wlan_sme::MlmeRequest) -> anyhow::Result<()> {
        use wlan_sme::MlmeRequest::*;
        match req {
            Scan(req) => self.handle_mlme_scan_request(req)?,
            Connect(req) => {
                self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                self.is_bss_protected = !req.security_ie.is_empty();
                self.device.connect(mlme_to_fullmac::convert_connect_request(req))?;
            }
            Reconnect(req) => {
                self.device.reconnect(mlme_to_fullmac::convert_reconnect_request(req))?;
            }
            Roam(req) => {
                self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                self.device.roam(mlme_to_fullmac::convert_roam_request(req))?;
            }
            AuthResponse(resp) => {
                self.device.auth_resp(mlme_to_fullmac::convert_authenticate_response(resp))?;
            }
            Deauthenticate(req) => {
                self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                self.device.deauth(mlme_to_fullmac::convert_deauthenticate_request(req))?;
            }
            AssocResponse(resp) => {
                self.device.assoc_resp(mlme_to_fullmac::convert_associate_response(resp))?;
            }
            Disassociate(req) => {
                self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                self.device.disassoc(mlme_to_fullmac::convert_disassociate_request(req))?;
            }
            Start(req) => {
                self.device.start_bss(mlme_to_fullmac::convert_start_bss_request(req)?)?
            }
            Stop(req) => self.device.stop_bss(mlme_to_fullmac::convert_stop_bss_request(req)?)?,
            SetKeys(req) => self.handle_mlme_set_keys_request(req)?,
            Eapol(req) => self.device.eapol_tx(mlme_to_fullmac::convert_eapol_request(req))?,
            SetCtrlPort(req) => self.set_link_state(req.state)?,
            QueryDeviceInfo(responder) => {
                let device_info =
                    fullmac_to_mlme::convert_device_info(self.device.query_device_info()?)?;
                responder.respond(device_info);
            }
            QueryDiscoverySupport(..) => info!("QueryDiscoverySupport is unsupported"),
            QueryMacSublayerSupport(responder) => {
                responder.respond(self.device.query_mac_sublayer_support()?)
            }
            QuerySecuritySupport(responder) => {
                responder.respond(self.device.query_security_support()?)
            }
            QuerySpectrumManagementSupport(responder) => {
                responder.respond(self.device.query_spectrum_management_support()?)
            }
            GetIfaceCounterStats(responder) => {
                responder.respond(self.device.get_iface_counter_stats()?)
            }
            GetIfaceHistogramStats(responder) => {
                responder.respond(self.device.get_iface_histogram_stats()?)
            }
            GetMinstrelStats(_, _) => info!("GetMinstrelStats is unsupported"),
            ListMinstrelPeers(_) => info!("ListMinstrelPeers is unsupported"),
            SaeHandshakeResp(resp) => self
                .device
                .sae_handshake_resp(mlme_to_fullmac::convert_sae_handshake_response(resp))?,
            SaeFrameTx(frame) => {
                self.device.sae_frame_tx(mlme_to_fullmac::convert_sae_frame(frame))?
            }
            WmmStatusReq => self.device.wmm_status_req()?,
            FinalizeAssociation(..) => info!("FinalizeAssociation is unsupported"),
        };
        Ok(())
    }

    fn handle_mlme_scan_request(&self, req: fidl_mlme::ScanRequest) -> anyhow::Result<()> {
        if req.channel_list.is_empty() {
            let end = fidl_mlme::ScanEnd {
                txn_id: req.txn_id,
                code: fidl_mlme::ScanResultCode::InvalidArgs,
            };
            self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnScanEnd { end });
        } else {
            self.device.start_scan(mlme_to_fullmac::convert_scan_request(req)?)?;
        }
        Ok(())
    }

    fn handle_mlme_set_keys_request(&self, req: fidl_mlme::SetKeysRequest) -> anyhow::Result<()> {
        let fullmac_req = mlme_to_fullmac::convert_set_keys_request(&req)?;
        let fullmac_resp = self.device.set_keys(fullmac_req)?;
        let mlme_resp = fullmac_to_mlme::convert_set_keys_resp(fullmac_resp, &req)?;
        self.mlme_event_sink.send(fidl_mlme::MlmeEvent::SetKeysConf { conf: mlme_resp });
        Ok(())
    }

    fn handle_driver_event(
        &mut self,
        event: FullmacDriverEvent,
        mac_role: &fidl_common::WlanMacRole,
    ) -> anyhow::Result<DriverState> {
        match event {
            FullmacDriverEvent::Stop => return Ok(DriverState::Stopping),
            FullmacDriverEvent::OnScanResult { result } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnScanResult { result });
            }
            FullmacDriverEvent::OnScanEnd { end } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnScanEnd { end });
            }
            FullmacDriverEvent::ConnectConf { resp } => {
                // IEEE Std 802.11-2016, 9.4.2.57
                // If BSS is protected, we do not open the controlled port yet until
                // RSN association is established and the keys are installed, at which
                // point we would receive a request to open the controlled port from SME.
                if !self.is_bss_protected && resp.result_code == fidl_ieee80211::StatusCode::Success
                {
                    self.set_link_state(fidl_mlme::ControlledPortState::Open)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::ConnectConf { resp });
            }
            FullmacDriverEvent::RoamConf { conf } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    // Like for connect, SME will open the controlled port for a protected BSS.
                    if !self.is_bss_protected
                        && conf.status_code == fidl_ieee80211::StatusCode::Success
                    {
                        self.set_link_state(fidl_mlme::ControlledPortState::Open)?;
                    }
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::RoamConf { conf });
            }
            FullmacDriverEvent::RoamStartInd { ind } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::RoamStartInd { ind });
            }
            FullmacDriverEvent::RoamResultInd { ind } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    // Like for connect, SME will open the controlled port for a protected BSS.
                    if !self.is_bss_protected
                        && ind.status_code == fidl_ieee80211::StatusCode::Success
                    {
                        self.set_link_state(fidl_mlme::ControlledPortState::Open)?;
                    }
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::RoamResultInd { ind });
            }
            FullmacDriverEvent::AuthInd { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::AuthenticateInd { ind });
            }
            FullmacDriverEvent::DeauthConf { resp } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DeauthenticateConf { resp });
            }
            FullmacDriverEvent::DeauthInd { ind } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DeauthenticateInd { ind });
            }
            FullmacDriverEvent::AssocInd { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::AssociateInd { ind });
            }
            FullmacDriverEvent::DisassocConf { resp } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DisassociateConf { resp });
            }
            FullmacDriverEvent::DisassocInd { ind } => {
                if *mac_role == fidl_common::WlanMacRole::Client {
                    self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DisassociateInd { ind });
            }
            FullmacDriverEvent::StartConf { resp } => {
                if resp.result_code == fidl_mlme::StartResultCode::Success {
                    self.set_link_state(fidl_mlme::ControlledPortState::Open)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::StartConf { resp });
            }
            FullmacDriverEvent::StopConf { resp } => {
                if resp.result_code == fidl_mlme::StopResultCode::Success {
                    self.set_link_state(fidl_mlme::ControlledPortState::Closed)?;
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::StopConf { resp });
            }
            FullmacDriverEvent::EapolConf { resp } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::EapolConf { resp });
            }
            FullmacDriverEvent::OnChannelSwitch { resp } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnChannelSwitched { info: resp });
            }
            FullmacDriverEvent::SignalReport { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::SignalReport { ind });
            }
            FullmacDriverEvent::EapolInd { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::EapolInd { ind });
            }
            FullmacDriverEvent::OnPmkAvailable { info } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnPmkAvailable { info });
            }
            FullmacDriverEvent::SaeHandshakeInd { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnSaeHandshakeInd { ind });
            }
            FullmacDriverEvent::SaeFrameRx { frame } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnSaeFrameRx { frame });
            }
            FullmacDriverEvent::OnWmmStatusResp { status, resp } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnWmmStatusResp { status, resp });
            }
        }
        Ok(DriverState::Running)
    }

    fn set_link_state(
        &mut self,
        new_link_state: fidl_mlme::ControlledPortState,
    ) -> anyhow::Result<()> {
        // TODO(https://fxbug.dev/42128153): Let SME handle these changes.
        if new_link_state == self.device_link_state {
            return Ok(());
        }

        let req = fidl_fullmac::WlanFullmacImplOnLinkStateChangedRequest {
            online: Some(new_link_state == fidl_mlme::ControlledPortState::Open),
            ..Default::default()
        };

        self.device.on_link_state_changed(req)?;
        self.device_link_state = new_link_state;
        Ok(())
    }
}

#[cfg(test)]
mod handle_mlme_request_tests {
    use super::*;
    use crate::device::test_utils::{DriverCall, FakeFullmacDevice, FakeFullmacDeviceMocks};
    use std::sync::{Arc, Mutex};
    use test_case::test_case;
    use wlan_common::assert_variant;
    use wlan_common::sink::UnboundedSink;
    use {fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_stats as fidl_stats};

    #[test]
    fn test_scan_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Scan(fidl_mlme::ScanRequest {
            txn_id: 1,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![2],
            ssid_list: vec![vec![3u8; 4]],
            probe_delay: 5,
            min_channel_time: 6,
            max_channel_time: 7,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::StartScan { req })) => req);
        assert_eq!(driver_req.txn_id, Some(1));
        assert_eq!(driver_req.scan_type, Some(fidl_fullmac::WlanScanType::Passive));
        assert_eq!(driver_req.channels, Some(vec![2]));
        assert_eq!(driver_req.min_channel_time, Some(6));
        assert_eq!(driver_req.max_channel_time, Some(7));
        assert_eq!(driver_req.ssids, Some(vec![vec![3u8; 4]]));
    }

    #[test]
    fn test_scan_request_empty_ssid_list() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Scan(fidl_mlme::ScanRequest {
            txn_id: 1,
            scan_type: fidl_mlme::ScanTypes::Active,
            channel_list: vec![2],
            ssid_list: vec![],
            probe_delay: 5,
            min_channel_time: 6,
            max_channel_time: 7,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::StartScan { req })) => req);
        assert_eq!(driver_req.scan_type, Some(fidl_fullmac::WlanScanType::Active));
        assert!(driver_req.ssids.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_scan_request_empty_channel_list() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Scan(fidl_mlme::ScanRequest {
            txn_id: 1,
            scan_type: fidl_mlme::ScanTypes::Passive,
            channel_list: vec![],
            ssid_list: vec![vec![3u8; 4]],
            probe_delay: 5,
            min_channel_time: 6,
            max_channel_time: 7,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(h.driver_calls.try_next(), Err(_));
        let scan_end = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(fidl_mlme::MlmeEvent::OnScanEnd { end })) => end);
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn test_connect_request() {
        let mut h = TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        let fidl_req = wlan_sme::MlmeRequest::Connect(fidl_mlme::ConnectRequest {
            selected_bss: fidl_common::BssDescription {
                bssid: [100u8; 6],
                bss_type: fidl_common::BssType::Infrastructure,
                beacon_period: 101,
                capability_info: 102,
                ies: vec![103u8, 104, 105],
                channel: fidl_common::WlanChannel {
                    primary: 106,
                    cbw: fidl_common::ChannelBandwidth::Cbw40,
                    secondary80: 0,
                },
                rssi_dbm: 107,
                snr_db: 108,
            },
            connect_failure_timeout: 1u32,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![2u8, 3, 4],
            wep_key: Some(Box::new(fidl_mlme::SetKeyDescriptor {
                key: vec![5u8, 6],
                key_id: 7,
                key_type: fidl_mlme::KeyType::Group,
                address: [8u8; 6],
                rsc: 9,
                cipher_suite_oui: [10u8; 3],
                cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(
                    11,
                ),
            })),
            security_ie: vec![12u8, 13],
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert!(h.mlme.is_bss_protected);

        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
              assert_eq!(req.online, Some(false));
          }
        );
        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::ConnectReq { req })) => {
            let selected_bss = req.selected_bss.clone().unwrap();
            assert_eq!(selected_bss.bssid, [100u8; 6]);
            assert_eq!(selected_bss.bss_type, fidl_common::BssType::Infrastructure);
            assert_eq!(selected_bss.beacon_period, 101);
            assert_eq!(selected_bss.capability_info, 102);
            assert_eq!(selected_bss.ies, vec![103u8, 104, 105]);
            assert_eq!(
                selected_bss.channel,
                fidl_common::WlanChannel {
                    primary: 106,
                    cbw: fidl_common::ChannelBandwidth::Cbw40,
                    secondary80: 0,
                }
            );
            assert_eq!(selected_bss.rssi_dbm, 107);
            assert_eq!(selected_bss.snr_db, 108);

            assert_eq!(req.connect_failure_timeout, Some(1u32));
            assert_eq!(req.auth_type, Some(fidl_fullmac::WlanAuthType::OpenSystem));
            assert_eq!(req.sae_password, Some(vec![2u8, 3, 4]));

            let wep_key = req.wep_key.clone().unwrap();
            assert_eq!(wep_key.key, Some(vec![5u8, 6]));
            assert_eq!(wep_key.key_idx, Some(7));
            assert_eq!(wep_key.key_type, Some(fidl_common::WlanKeyType::Group));
            assert_eq!(wep_key.peer_addr, Some([8u8; 6]));
            assert_eq!(wep_key.rsc, Some(9));
            assert_eq!(wep_key.cipher_oui, Some([10u8; 3]));
            assert_eq!(wep_key.cipher_type, fidl_ieee80211::CipherSuiteType::from_primitive(11));

            assert_eq!(req.security_ie, Some(vec![12u8, 13]));
        });
    }

    #[test]
    fn test_reconnect_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Reconnect(fidl_mlme::ReconnectRequest {
            peer_sta_address: [1u8; 6],
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::ReconnectReq { req })) => req);
        assert_eq!(
            driver_req,
            fidl_fullmac::WlanFullmacImplReconnectRequest {
                peer_sta_address: Some([1u8; 6]),
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_authenticate_response() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::AuthResponse(fidl_mlme::AuthenticateResponse {
            peer_sta_address: [1u8; 6],
            result_code: fidl_mlme::AuthenticateResultCode::Success,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::AuthResp { resp })) => resp);
        assert_eq!(
            driver_req,
            fidl_fullmac::WlanFullmacImplAuthRespRequest {
                peer_sta_address: Some([1u8; 6]),
                result_code: Some(fidl_fullmac::WlanAuthResult::Success),
                ..Default::default()
            }
        );
    }

    #[test]
    fn test_deauthenticate_request() {
        let mut h = TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        let fidl_req = wlan_sme::MlmeRequest::Deauthenticate(fidl_mlme::DeauthenticateRequest {
            peer_sta_address: [1u8; 6],
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
              assert_eq!(req.online, Some(false));
          }
        );
        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::DeauthReq { req })) => req);
        assert_eq!(driver_req.peer_sta_address, Some([1u8; 6]));
        assert_eq!(driver_req.reason_code, Some(fidl_ieee80211::ReasonCode::LeavingNetworkDeauth));
    }

    #[test]
    fn test_associate_response() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::AssocResponse(fidl_mlme::AssociateResponse {
            peer_sta_address: [1u8; 6],
            result_code: fidl_mlme::AssociateResultCode::Success,
            association_id: 2,
            capability_info: 3,
            rates: vec![4, 5],
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::AssocResp { resp })) => resp);
        assert_eq!(driver_req.peer_sta_address, Some([1u8; 6]));
        assert_eq!(driver_req.result_code, Some(fidl_fullmac::WlanAssocResult::Success));
        assert_eq!(driver_req.association_id, Some(2));
    }

    #[test]
    fn test_disassociate_request() {
        let mut h = TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        let fidl_req = wlan_sme::MlmeRequest::Disassociate(fidl_mlme::DisassociateRequest {
            peer_sta_address: [1u8; 6],
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
              assert_eq!(req.online, Some(false));
          }
        );

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::Disassoc{ req })) => req);
        assert_eq!(driver_req.peer_sta_address, Some([1u8; 6]));
        assert_eq!(
            driver_req.reason_code,
            Some(fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc)
        );
    }

    #[test]
    fn test_start_request() {
        let mut h = TestHelper::set_up();
        const SSID_LEN: usize = 2;
        const RSNE_LEN: usize = 15;
        let fidl_req = wlan_sme::MlmeRequest::Start(fidl_mlme::StartRequest {
            ssid: vec![1u8; SSID_LEN],
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 3,
            dtim_period: 4,
            channel: 5,
            capability_info: 6,
            rates: vec![7, 8, 9],
            country: fidl_mlme::Country { alpha2: [10, 11], suffix: 12 },
            mesh_id: vec![13],
            rsne: Some(vec![14; RSNE_LEN]),
            phy: fidl_common::WlanPhyType::Vht,
            channel_bandwidth: fidl_common::ChannelBandwidth::Cbw80,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::StartBss { req })) => req);

        assert_eq!(driver_req.ssid, Some(vec![1u8; SSID_LEN]));
        assert_eq!(driver_req.bss_type, Some(fidl_common::BssType::Infrastructure));
        assert_eq!(driver_req.beacon_period, Some(3));
        assert_eq!(driver_req.dtim_period, Some(4));
        assert_eq!(driver_req.channel, Some(5));
        assert_ne!(driver_req.rsne, Some(vec![14 as u8, RSNE_LEN as u8]));
        assert_eq!(driver_req.vendor_ie, Some(vec![]));
    }

    #[test]
    fn test_stop_request() {
        let mut h = TestHelper::set_up();
        const SSID_LEN: usize = 2;
        let fidl_req =
            wlan_sme::MlmeRequest::Stop(fidl_mlme::StopRequest { ssid: vec![1u8; SSID_LEN] });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::StopBss { req })) => req);
        assert_eq!(driver_req.ssid, Some(vec![1u8; SSID_LEN]));
    }

    #[test]
    fn test_set_keys_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
            keylist: vec![fidl_mlme::SetKeyDescriptor {
                key: vec![5u8, 6],
                key_id: 7,
                key_type: fidl_mlme::KeyType::Group,
                address: [8u8; 6],
                rsc: 9,
                cipher_suite_oui: [10u8; 3],
                cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(
                    11,
                ),
            }],
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::SetKeys { req })) => req);
        assert_eq!(driver_req.keylist.as_ref().unwrap().len(), 1 as usize);
        let keylist = driver_req.keylist.as_ref().unwrap();
        assert_eq!(keylist[0].key_idx, Some(7));
        assert_eq!(keylist[0].key_type, Some(fidl_common::WlanKeyType::Group));
        assert_eq!(keylist[0].peer_addr, Some([8u8; 6]));
        assert_eq!(keylist[0].rsc, Some(9));
        assert_eq!(keylist[0].cipher_oui, Some([10u8; 3]));
        assert_eq!(
            keylist[0].cipher_type,
            Some(fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(11))
        );

        let conf = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(fidl_mlme::MlmeEvent::SetKeysConf { conf })) => conf);
        assert_eq!(
            conf,
            fidl_mlme::SetKeysConfirm {
                results: vec![fidl_mlme::SetKeyResult { key_id: 7, status: 0 }]
            }
        );
    }

    #[test]
    fn test_set_keys_request_partial_failure() {
        let mut h = TestHelper::set_up();
        const NUM_KEYS: usize = 3;
        h.fake_device.lock().unwrap().set_keys_resp_mock =
            Some(fidl_fullmac::WlanFullmacSetKeysResp { statuslist: [0i32, 1, 0].to_vec() });
        let mut keylist = vec![];
        let key = fidl_mlme::SetKeyDescriptor {
            key: vec![5u8, 6],
            key_id: 7,
            key_type: fidl_mlme::KeyType::Group,
            address: [8u8; 6],
            rsc: 9,
            cipher_suite_oui: [10u8; 3],
            cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(11),
        };
        for i in 0..NUM_KEYS {
            keylist.push(fidl_mlme::SetKeyDescriptor { key_id: i as u16, ..key.clone() });
        }
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest { keylist });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::SetKeys { req })) => req);
        assert_eq!(driver_req.keylist.as_ref().unwrap().len(), NUM_KEYS as usize);
        let keylist = driver_req.keylist.unwrap();
        for i in 0..NUM_KEYS {
            assert_eq!(keylist[i].key_idx, Some(i as u8));
        }

        let conf = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(fidl_mlme::MlmeEvent::SetKeysConf { conf })) => conf);
        assert_eq!(
            conf,
            fidl_mlme::SetKeysConfirm {
                results: vec![
                    fidl_mlme::SetKeyResult { key_id: 0, status: 0 },
                    fidl_mlme::SetKeyResult { key_id: 1, status: 1 },
                    fidl_mlme::SetKeyResult { key_id: 2, status: 0 },
                ]
            }
        );
    }

    #[test]
    fn test_set_keys_request_too_many_keys() {
        let mut h = TestHelper::set_up();
        let key = fidl_mlme::SetKeyDescriptor {
            key: vec![5u8, 6],
            key_id: 7,
            key_type: fidl_mlme::KeyType::Group,
            address: [8u8; 6],
            rsc: 9,
            cipher_suite_oui: [10u8; 3],
            cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(11),
        };
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
            keylist: vec![key.clone(); 5],
        });

        assert!(h.mlme.handle_mlme_request(fidl_req).is_err());

        // No SetKeys and SetKeysResp
        assert_variant!(h.driver_calls.try_next(), Err(_));
        assert_variant!(h.mlme_event_receiver.try_next(), Err(_));
    }

    #[test]
    fn test_set_keys_request_when_resp_has_different_num_keys() {
        let mut h = TestHelper::set_up();
        h.fake_device.lock().unwrap().set_keys_resp_mock =
            Some(fidl_fullmac::WlanFullmacSetKeysResp { statuslist: [0i32; 2].to_vec() });
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
            keylist: vec![fidl_mlme::SetKeyDescriptor {
                key: vec![5u8, 6],
                key_id: 7,
                key_type: fidl_mlme::KeyType::Group,
                address: [8u8; 6],
                rsc: 9,
                cipher_suite_oui: [10u8; 3],
                cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(
                    11,
                ),
            }],
        });

        // An error is expected when converting the response
        assert!(h.mlme.handle_mlme_request(fidl_req).is_err());

        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::SetKeys { .. })));
        // No SetKeysConf MLME event because the SetKeysResp from driver has different number of
        // keys.
        assert_variant!(h.mlme_event_receiver.try_next(), Err(_));
    }

    #[test]
    fn test_eapol_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Eapol(fidl_mlme::EapolRequest {
            src_addr: [1u8; 6],
            dst_addr: [2u8; 6],
            data: vec![3u8; 4],
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::EapolTx { req })) => req);
        assert_eq!(driver_req.src_addr, Some([1u8; 6]));
        assert_eq!(driver_req.dst_addr, Some([2u8; 6]));
        assert_eq!(driver_req.data, Some(vec![3u8; 4]));
    }

    #[test_case(fidl_mlme::ControlledPortState::Open, true; "online")]
    #[test_case(fidl_mlme::ControlledPortState::Closed, false; "offline")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_set_ctrl_port(
        controlled_port_state: fidl_mlme::ControlledPortState,
        expected_link_state: bool,
    ) {
        let mut h = match controlled_port_state {
            fidl_mlme::ControlledPortState::Open => TestHelper::set_up(),
            fidl_mlme::ControlledPortState::Closed => {
                TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open)
            }
        };
        let fidl_req = wlan_sme::MlmeRequest::SetCtrlPort(fidl_mlme::SetControlledPortRequest {
            peer_sta_address: [1u8; 6],
            state: controlled_port_state,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
            assert_eq!(req.online, Some(expected_link_state));
        });
    }

    #[test]
    fn test_get_iface_counter_stats() {
        let mut h = TestHelper::set_up();
        let mocked_stats = fidl_stats::IfaceCounterStats {
            rx_unicast_drop: 11,
            rx_unicast_total: 22,
            rx_multicast: 33,
            tx_total: 44,
            tx_drop: 55,
        };
        h.fake_device
            .lock()
            .unwrap()
            .get_iface_counter_stats_mock
            .replace(fidl_mlme::GetIfaceCounterStatsResponse::Stats(mocked_stats));
        let (stats_responder, mut stats_receiver) = wlan_sme::responder::Responder::new();
        let fidl_req = wlan_sme::MlmeRequest::GetIfaceCounterStats(stats_responder);

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::GetIfaceCounterStats)));
        let stats = assert_variant!(stats_receiver.try_recv(), Ok(Some(stats)) => stats);
        let stats =
            assert_variant!(stats, fidl_mlme::GetIfaceCounterStatsResponse::Stats(stats) => stats);
        assert_eq!(
            stats,
            fidl_stats::IfaceCounterStats {
                rx_unicast_drop: 11,
                rx_unicast_total: 22,
                rx_multicast: 33,
                tx_total: 44,
                tx_drop: 55,
            }
        );
    }

    #[test]
    fn test_get_iface_histogram_stats() {
        let mut h = TestHelper::set_up();

        let mocked_stats = fidl_stats::IfaceHistogramStats {
            noise_floor_histograms: Some(vec![fidl_stats::NoiseFloorHistogram {
                hist_scope: fidl_stats::HistScope::Station,
                antenna_id: None,
                noise_floor_samples: vec![fidl_stats::HistBucket {
                    bucket_index: 2,
                    num_samples: 3,
                }],
                invalid_samples: 4,
            }]),
            rssi_histograms: Some(vec![fidl_stats::RssiHistogram {
                hist_scope: fidl_stats::HistScope::PerAntenna,
                antenna_id: Some(Box::new(fidl_stats::AntennaId {
                    freq: fidl_stats::AntennaFreq::Antenna5G,
                    index: 5,
                })),
                rssi_samples: vec![fidl_stats::HistBucket { bucket_index: 6, num_samples: 7 }],
                invalid_samples: 8,
            }]),
            rx_rate_index_histograms: Some(vec![fidl_stats::RxRateIndexHistogram {
                hist_scope: fidl_stats::HistScope::Station,
                antenna_id: None,
                rx_rate_index_samples: vec![fidl_stats::HistBucket {
                    bucket_index: 10,
                    num_samples: 11,
                }],
                invalid_samples: 12,
            }]),
            snr_histograms: Some(vec![fidl_stats::SnrHistogram {
                hist_scope: fidl_stats::HistScope::PerAntenna,
                antenna_id: Some(Box::new(fidl_stats::AntennaId {
                    freq: fidl_stats::AntennaFreq::Antenna2G,
                    index: 13,
                })),
                snr_samples: vec![fidl_stats::HistBucket { bucket_index: 14, num_samples: 15 }],
                invalid_samples: 16,
            }]),
            ..Default::default()
        };

        h.fake_device
            .lock()
            .unwrap()
            .get_iface_histogram_stats_mock
            .replace(fidl_mlme::GetIfaceHistogramStatsResponse::Stats(mocked_stats.clone()));
        let (stats_responder, mut stats_receiver) = wlan_sme::responder::Responder::new();
        let fidl_req = wlan_sme::MlmeRequest::GetIfaceHistogramStats(stats_responder);

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::GetIfaceHistogramStats)));
        let stats = assert_variant!(stats_receiver.try_recv(), Ok(Some(stats)) => stats);
        let stats = assert_variant!(stats, fidl_mlme::GetIfaceHistogramStatsResponse::Stats(stats) => stats);
        assert_eq!(stats, mocked_stats);
    }

    #[test]
    fn test_sae_handshake_resp() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::SaeHandshakeResp(fidl_mlme::SaeHandshakeResponse {
            peer_sta_address: [1u8; 6],
            status_code: fidl_ieee80211::StatusCode::AntiCloggingTokenRequired,
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_req = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::SaeHandshakeResp { resp })) => resp);
        assert_eq!(driver_req.peer_sta_address.unwrap(), [1u8; 6]);
        assert_eq!(
            driver_req.status_code.unwrap(),
            fidl_ieee80211::StatusCode::AntiCloggingTokenRequired
        );
    }

    #[test]
    fn test_convert_sae_frame() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::SaeFrameTx(fidl_mlme::SaeFrame {
            peer_sta_address: [1u8; 6],
            status_code: fidl_ieee80211::StatusCode::Success,
            seq_num: 2,
            sae_fields: vec![3u8; 4],
        });

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        let driver_frame = assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::SaeFrameTx { frame })) => frame);
        assert_eq!(driver_frame.peer_sta_address.unwrap(), [1u8; 6]);
        assert_eq!(driver_frame.status_code.unwrap(), fidl_ieee80211::StatusCode::Success);
        assert_eq!(driver_frame.seq_num.unwrap(), 2);
        assert_eq!(driver_frame.sae_fields.unwrap(), vec![3u8; 4]);
    }

    #[test]
    fn test_wmm_status_req() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::WmmStatusReq;

        h.mlme.handle_mlme_request(fidl_req).unwrap();

        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::WmmStatusReq)));
    }

    pub struct TestHelper {
        fake_device: Arc<Mutex<FakeFullmacDeviceMocks>>,
        mlme: MlmeMainLoop<FakeFullmacDevice>,
        mlme_event_receiver: mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>,
        driver_calls: mpsc::UnboundedReceiver<DriverCall>,
        _mlme_request_sender: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        _driver_event_sender: mpsc::UnboundedSender<FullmacDriverEvent>,
    }

    impl TestHelper {
        pub fn set_up_with_link_state(device_link_state: fidl_mlme::ControlledPortState) -> Self {
            let (fake_device, driver_calls) = FakeFullmacDevice::new();
            let (mlme_request_sender, mlme_request_stream) = mpsc::unbounded();
            let (mlme_event_sender, mlme_event_receiver) = mpsc::unbounded();
            let mlme_event_sink = UnboundedSink::new(mlme_event_sender);

            let (driver_event_sender, driver_event_stream) = mpsc::unbounded();
            let mocks = fake_device.mocks.clone();

            let mlme = MlmeMainLoop {
                device: fake_device,
                mlme_request_stream,
                mlme_event_sink,
                driver_event_stream,
                is_bss_protected: false,
                device_link_state,
            };
            Self {
                fake_device: mocks,
                mlme,
                mlme_event_receiver,
                driver_calls,
                _mlme_request_sender: mlme_request_sender,
                _driver_event_sender: driver_event_sender,
            }
        }

        // By default, link state starts off closed
        pub fn set_up() -> Self {
            Self::set_up_with_link_state(fidl_mlme::ControlledPortState::Closed)
        }
    }
}
#[cfg(test)]
mod handle_driver_event_tests {
    use super::*;
    use crate::device::test_utils::{DriverCall, FakeFullmacDevice, FakeFullmacDeviceMocks};
    use futures::channel::mpsc;
    use futures::task::Poll;
    use futures::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use test_case::test_case;
    use wlan_common::sink::UnboundedSink;
    use wlan_common::{assert_variant, fake_fidl_bss_description};
    use {
        fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
        fidl_fuchsia_wlan_mlme as fidl_mlme, fuchsia_async as fasync,
    };

    fn create_bss_descriptions() -> fidl_common::BssDescription {
        fidl_common::BssDescription {
            bssid: [9u8; 6],
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 1,
            capability_info: 2,
            ies: vec![3, 4, 5],
            channel: fidl_common::WlanChannel {
                primary: 6,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            rssi_dbm: 7,
            snr_db: 8,
        }
    }

    fn create_connect_request(security_ie: Vec<u8>) -> wlan_sme::MlmeRequest {
        wlan_sme::MlmeRequest::Connect(fidl_mlme::ConnectRequest {
            selected_bss: fake_fidl_bss_description!(Wpa2),
            connect_failure_timeout: 1u32,
            auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            sae_password: vec![2u8, 3, 4],
            wep_key: None,
            security_ie,
        })
    }

    #[test]
    fn test_on_scan_result() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let bss = create_bss_descriptions();
        let scan_result = fidl_fullmac::WlanFullmacImplIfcOnScanResultRequest {
            txn_id: Some(42u64),
            timestamp_nanos: Some(1337i64),
            bss: Some(bss.clone()),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.on_scan_result(&scan_result)),
            Poll::Ready(Ok(())),
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let result =
            assert_variant!(event, fidl_mlme::MlmeEvent::OnScanResult { result } => result);
        assert_eq!(result, fidl_mlme::ScanResult { txn_id: 42u64, timestamp_nanos: 1337i64, bss });
    }

    #[test]
    fn test_on_scan_end() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let scan_end = fidl_fullmac::WlanFullmacImplIfcOnScanEndRequest {
            txn_id: Some(42u64),
            code: Some(fidl_fullmac::WlanScanResult::Success),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.on_scan_end(&scan_end)),
            Poll::Ready(Ok(())),
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let end = assert_variant!(event, fidl_mlme::MlmeEvent::OnScanEnd { end } => end);
        assert_eq!(
            end,
            fidl_mlme::ScanEnd { txn_id: 42u64, code: fidl_mlme::ScanResultCode::Success }
        );
    }

    #[test]
    fn test_connect_conf() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let connect_conf = fidl_fullmac::WlanFullmacImplIfcConnectConfRequest {
            peer_sta_address: Some([1u8; 6]),
            result_code: Some(fidl_ieee80211::StatusCode::Success),
            association_id: Some(2),
            association_ies: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.connect_conf(&connect_conf)),
            Poll::Ready(Ok(())),
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::ConnectConf { resp } => resp);
        assert_eq!(
            conf,
            fidl_mlme::ConnectConfirm {
                peer_sta_address: [1u8; 6],
                result_code: fidl_ieee80211::StatusCode::Success,
                association_id: 2,
                association_ies: vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
            }
        );
    }

    #[test_case(true, fidl_ieee80211::StatusCode::Success, false; "secure connect with success status is not online yet")]
    #[test_case(false, fidl_ieee80211::StatusCode::Success, true; "insecure connect with success status is online right away")]
    #[test_case(true, fidl_ieee80211::StatusCode::RefusedReasonUnspecified, false; "secure connect with failed status is not online")]
    #[test_case(false, fidl_ieee80211::StatusCode::RefusedReasonUnspecified, false; "insecure connect with failed status in not online")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_connect_req_connect_conf_link_state(
        secure_connect: bool,
        connect_result_code: fidl_ieee80211::StatusCode,
        expected_online: bool,
    ) {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let connect_req =
            create_connect_request(if secure_connect { vec![7u8, 8] } else { vec![] });
        h.mlme_request_sender
            .unbounded_send(connect_req)
            .expect("sending ConnectReq should succeed");
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let connect_conf = fidl_fullmac::WlanFullmacImplIfcConnectConfRequest {
            peer_sta_address: Some([1u8; 6]),
            result_code: Some(connect_result_code),
            association_id: Some(2),
            association_ies: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9]),
            ..Default::default()
        };

        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.connect_conf(&connect_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
              assert_eq!(req.online, Some(false));
          }
        );
        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::ConnectReq { .. })));
        if expected_online {
            assert_variant!(
                h.driver_calls.try_next(),
                Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
                  assert_eq!(req.online, Some(true));
              }
            );
        } else {
            assert_variant!(h.driver_calls.try_next(), Err(_));
        }
    }

    #[test]
    fn test_auth_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let auth_ind = fidl_fullmac::WlanFullmacImplIfcAuthIndRequest {
            peer_sta_address: Some([1u8; 6]),
            auth_type: Some(fidl_fullmac::WlanAuthType::OpenSystem),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.auth_ind(&auth_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::AuthenticateInd { ind } => ind);
        assert_eq!(
            ind,
            fidl_mlme::AuthenticateIndication {
                peer_sta_address: [1u8; 6],
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            }
        );
    }

    #[test_case(fidl_common::WlanMacRole::Client; "client")]
    #[test_case(fidl_common::WlanMacRole::Ap; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_deauth_conf(mac_role: fidl_common::WlanMacRole) {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        h.fake_device.lock().unwrap().query_device_info_mock.as_mut().unwrap().role =
            Some(mac_role);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let deauth_conf = fidl_fullmac::WlanFullmacImplIfcDeauthConfRequest {
            peer_sta_address: Some([1u8; 6]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.deauth_conf(&deauth_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        if mac_role == fidl_common::WlanMacRole::Client {
            assert_variant!(
                h.driver_calls.try_next(),
                Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
                  assert_eq!(req.online, Some(false));
              }
            );
        } else {
            assert_variant!(h.driver_calls.try_next(), Err(_));
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf =
            assert_variant!(event, fidl_mlme::MlmeEvent::DeauthenticateConf { resp } => resp);
        assert_eq!(conf, fidl_mlme::DeauthenticateConfirm { peer_sta_address: [1u8; 6] });
    }

    #[test_case(fidl_common::WlanMacRole::Client; "client")]
    #[test_case(fidl_common::WlanMacRole::Ap; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_deauth_ind(mac_role: fidl_common::WlanMacRole) {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        h.fake_device.lock().unwrap().query_device_info_mock.as_mut().unwrap().role =
            Some(mac_role);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let deauth_ind = fidl_fullmac::WlanFullmacImplIfcDeauthIndRequest {
            peer_sta_address: Some([1u8; 6]),
            reason_code: Some(fidl_ieee80211::ReasonCode::LeavingNetworkDeauth),
            locally_initiated: Some(true),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.deauth_ind(&deauth_ind)),
            Poll::Ready(Ok(())),
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        if mac_role == fidl_common::WlanMacRole::Client {
            assert_variant!(
                h.driver_calls.try_next(),
                Ok(Some(DriverCall::OnLinkStateChanged { req })) =>{
                  assert_eq!(req.online, Some(false));
              }
            );
        } else {
            assert_variant!(h.driver_calls.try_next(), Err(_));
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::DeauthenticateInd { ind } => ind);
        assert_eq!(
            ind,
            fidl_mlme::DeauthenticateIndication {
                peer_sta_address: [1u8; 6],
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: true,
            }
        );
    }

    #[test]
    fn test_assoc_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let assoc_ind = fidl_fullmac::WlanFullmacImplIfcAssocIndRequest {
            peer_sta_address: Some([1u8; 6]),
            listen_interval: Some(2),
            ssid: vec![3u8; 4].into(),
            rsne: Some(vec![5u8; 6]),
            vendor_ie: Some(vec![7u8; 8]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.assoc_ind(&assoc_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::AssociateInd { ind } => ind);
        assert_eq!(
            ind,
            fidl_mlme::AssociateIndication {
                peer_sta_address: [1u8; 6],
                capability_info: 0,
                listen_interval: 2,
                ssid: Some(vec![3u8; 4]),
                rates: vec![],
                rsne: Some(vec![5u8; 6]),
            }
        );
    }

    #[test_case(fidl_common::WlanMacRole::Client; "client")]
    #[test_case(fidl_common::WlanMacRole::Ap; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_disassoc_conf(mac_role: fidl_common::WlanMacRole) {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        h.fake_device.lock().unwrap().query_device_info_mock.as_mut().unwrap().role =
            Some(mac_role);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let disassoc_conf = fidl_fullmac::WlanFullmacImplIfcDisassocConfRequest {
            status: Some(1),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.disassoc_conf(&disassoc_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        if mac_role == fidl_common::WlanMacRole::Client {
            assert_variant!(
                h.driver_calls.try_next(),
                Ok(Some(DriverCall::OnLinkStateChanged { req })) =>{
                  assert_eq!(req.online, Some(false));
              }
            );
        } else {
            assert_variant!(h.driver_calls.try_next(), Err(_));
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::DisassociateConf { resp } => resp);
        assert_eq!(conf, fidl_mlme::DisassociateConfirm { status: 1 });
    }

    #[test_case(fidl_common::WlanMacRole::Client; "client")]
    #[test_case(fidl_common::WlanMacRole::Ap; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_disassoc_ind(mac_role: fidl_common::WlanMacRole) {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        h.fake_device.lock().unwrap().query_device_info_mock.as_mut().unwrap().role =
            Some(mac_role);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let disassoc_ind = fidl_fullmac::WlanFullmacImplIfcDisassocIndRequest {
            peer_sta_address: Some([1u8; 6]),
            reason_code: Some(fidl_ieee80211::ReasonCode::LeavingNetworkDeauth),
            locally_initiated: Some(true),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.disassoc_ind(&disassoc_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        if mac_role == fidl_common::WlanMacRole::Client {
            assert_variant!(
                h.driver_calls.try_next(),
                Ok(Some(DriverCall::OnLinkStateChanged { req })) =>{
                  assert_eq!(req.online, Some(false));
              }
            );
        } else {
            assert_variant!(h.driver_calls.try_next(), Err(_));
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::DisassociateInd { ind } => ind);
        assert_eq!(
            ind,
            fidl_mlme::DisassociateIndication {
                peer_sta_address: [1u8; 6],
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
                locally_initiated: true,
            }
        );
    }

    #[test_case(fidl_fullmac::StartResult::Success, true, fidl_mlme::StartResultCode::Success; "success start result")]
    #[test_case(fidl_fullmac::StartResult::BssAlreadyStartedOrJoined, false, fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined; "other start result")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_start_conf(
        start_result: fidl_fullmac::StartResult,
        expected_link_state_changed: bool,
        expected_fidl_result_code: fidl_mlme::StartResultCode,
    ) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let start_conf = fidl_fullmac::WlanFullmacImplIfcStartConfRequest {
            result_code: Some(start_result),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.start_conf(&start_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        if expected_link_state_changed {
            assert_variant!(
                h.driver_calls.try_next(),
                Ok(Some(DriverCall::OnLinkStateChanged { req }))=>{
                  assert_eq!(req.online, Some(true));
              }
            );
        } else {
            assert_variant!(h.driver_calls.try_next(), Err(_));
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::StartConf { resp } => resp);
        assert_eq!(conf, fidl_mlme::StartConfirm { result_code: expected_fidl_result_code });
    }

    #[test]
    fn test_stop_conf() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let stop_conf = fidl_fullmac::WlanFullmacImplIfcStopConfRequest {
            result_code: Some(fidl_fullmac::StopResult::Success),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.stop_conf(&stop_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::StopConf { resp } => resp);
        assert_eq!(
            conf,
            fidl_mlme::StopConfirm { result_code: fidl_mlme::StopResultCode::Success }
        );
    }

    #[test]
    fn test_eapol_conf() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let eapol_conf = fidl_fullmac::WlanFullmacImplIfcEapolConfRequest {
            result_code: Some(fidl_fullmac::EapolTxResult::Success),
            dst_addr: Some([1u8; 6]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.eapol_conf(&eapol_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::EapolConf { resp } => resp);
        assert_eq!(
            conf,
            fidl_mlme::EapolConfirm {
                result_code: fidl_mlme::EapolResultCode::Success,
                dst_addr: [1u8; 6],
            }
        );
    }

    #[test]
    fn test_on_channel_switch() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let channel_switch_info = fidl_fullmac::WlanFullmacChannelSwitchInfo { new_channel: 9 };
        assert_variant!(
            h.exec.run_until_stalled(
                &mut h.fullmac_ifc_proxy.on_channel_switch(&channel_switch_info)
            ),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let info = assert_variant!(event, fidl_mlme::MlmeEvent::OnChannelSwitched { info } => info);
        assert_eq!(info, fidl_internal::ChannelSwitchInfo { new_channel: 9 });
    }

    #[test]
    fn test_roam_start_ind() {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let selected_bss = create_bss_descriptions();
        let roam_start_ind = fidl_fullmac::WlanFullmacImplIfcRoamStartIndRequest {
            selected_bssid: Some(selected_bss.bssid.clone()),
            selected_bss: Some(selected_bss.clone()),
            original_association_maintained: Some(false),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.roam_start_ind(&roam_start_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        // Receipt of a roam start causes MLME to close the controlled port.
        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req }))=>{
              assert_eq!(req.online, Some(false));
          }
        );

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::RoamStartInd { ind } => ind);

        // SME is notified of the roam start.
        assert_eq!(
            ind,
            fidl_mlme::RoamStartIndication {
                selected_bssid: selected_bss.bssid,
                selected_bss,
                original_association_maintained: false,
            }
        );
    }

    #[test]
    fn test_roam_req() {
        let (mut h, mut test_fut) =
            TestHelper::set_up_with_link_state(fidl_mlme::ControlledPortState::Open);

        let selected_bss = create_bss_descriptions();
        let roam_req = wlan_sme::MlmeRequest::Roam(fidl_mlme::RoamRequest {
            selected_bss: selected_bss.clone(),
        });

        h.mlme_request_sender.unbounded_send(roam_req).expect("sending RoamReq should succeed");
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        // Receipt of a roam request causes MLME to close the controlled port.
        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
                assert_eq!(req.online, Some(false));
            }
        );
        assert_variant!(h.driver_calls.try_next(), Ok(Some(DriverCall::RoamReq { req })) => {
            assert_eq!(selected_bss, req.selected_bss.clone().unwrap());
        });
    }

    #[test]
    fn test_roam_result_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let selected_bss = create_bss_descriptions();
        let original_association_maintained = false;
        let target_bss_authenticated = true;
        let association_id = 42;
        let association_ies = Vec::new();

        let roam_result_ind = fidl_fullmac::WlanFullmacImplIfcRoamResultIndRequest {
            selected_bssid: Some(selected_bss.bssid.clone()),
            status_code: Some(fidl_ieee80211::StatusCode::Success),
            original_association_maintained: Some(original_association_maintained),
            target_bss_authenticated: Some(target_bss_authenticated),
            association_id: Some(association_id),
            association_ies: Some(association_ies.clone()),
            ..Default::default()
        };

        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.roam_result_ind(&roam_result_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        // Receipt of a roam result success causes MLME to open the controlled port on an open network.
        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req }))=>{
              assert_eq!(req.online, Some(true));
          }
        );

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::RoamResultInd { ind } => ind);

        // SME is notified of the roam result.
        assert_eq!(
            ind,
            fidl_mlme::RoamResultIndication {
                selected_bssid: selected_bss.bssid,
                status_code: fidl_ieee80211::StatusCode::Success,
                original_association_maintained,
                target_bss_authenticated,
                association_id,
                association_ies,
            }
        );
    }

    #[test]
    fn test_roam_conf() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let selected_bss = create_bss_descriptions();
        let original_association_maintained = false;
        let target_bss_authenticated = true;
        let association_id = 42;
        let association_ies = Vec::new();

        let roam_conf = fidl_fullmac::WlanFullmacImplIfcRoamConfRequest {
            selected_bssid: Some(selected_bss.bssid.clone()),
            status_code: Some(fidl_ieee80211::StatusCode::Success),
            original_association_maintained: Some(original_association_maintained),
            target_bss_authenticated: Some(target_bss_authenticated),
            association_id: Some(association_id),
            association_ies: Some(association_ies.clone()),
            ..Default::default()
        };

        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.roam_conf(&roam_conf)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        // Receipt of a roam result success causes MLME to open the controlled port on an open network.
        assert_variant!(
            h.driver_calls.try_next(),
            Ok(Some(DriverCall::OnLinkStateChanged { req })) => {
                assert_eq!(req.online, Some(true));
            }
        );

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::RoamConf { conf } => conf);

        // SME is notified of the roam result.
        assert_eq!(
            conf,
            fidl_mlme::RoamConfirm {
                selected_bssid: selected_bss.bssid,
                status_code: fidl_ieee80211::StatusCode::Success,
                original_association_maintained,
                target_bss_authenticated,
                association_id,
                association_ies,
            }
        );
    }

    #[test]
    fn test_signal_report() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let signal_report_ind =
            fidl_fullmac::WlanFullmacSignalReportIndication { rssi_dbm: 1, snr_db: 2 };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.signal_report(&signal_report_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::SignalReport { ind } => ind);
        assert_eq!(ind, fidl_internal::SignalReportIndication { rssi_dbm: 1, snr_db: 2 });
    }

    #[test]
    fn test_eapol_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let eapol_ind = fidl_fullmac::WlanFullmacImplIfcEapolIndRequest {
            src_addr: Some([1u8; 6]),
            dst_addr: Some([2u8; 6]),
            data: Some(vec![3u8; 4]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.eapol_ind(&eapol_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::EapolInd { ind } => ind);
        assert_eq!(
            ind,
            fidl_mlme::EapolIndication {
                src_addr: [1u8; 6],
                dst_addr: [2u8; 6],
                data: vec![3u8; 4],
            }
        );
    }

    #[test]
    fn test_on_pmk_available() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let pmk_info = fidl_fullmac::WlanFullmacImplIfcOnPmkAvailableRequest {
            pmk: Some(vec![1u8; 2]),
            pmkid: Some(vec![3u8; 4]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.on_pmk_available(&pmk_info)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let info = assert_variant!(event, fidl_mlme::MlmeEvent::OnPmkAvailable { info } => info);
        assert_eq!(info, fidl_mlme::PmkInfo { pmk: vec![1u8; 2], pmkid: vec![3u8; 4] });
    }

    #[test]
    fn test_sae_handshake_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let sae_handshake_ind = fidl_fullmac::WlanFullmacImplIfcSaeHandshakeIndRequest {
            peer_sta_address: Some([1u8; 6]),
            ..Default::default()
        };
        assert_variant!(
            h.exec
                .run_until_stalled(&mut h.fullmac_ifc_proxy.sae_handshake_ind(&sae_handshake_ind)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::OnSaeHandshakeInd { ind } => ind);
        assert_eq!(ind, fidl_mlme::SaeHandshakeIndication { peer_sta_address: [1u8; 6] });
    }

    #[test]
    fn test_sae_frame_rx() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let sae_frame = fidl_fullmac::SaeFrame {
            peer_sta_address: Some([1u8; 6]),
            status_code: Some(fidl_ieee80211::StatusCode::Success),
            seq_num: Some(2),
            sae_fields: Some(vec![3u8; 4]),
            ..Default::default()
        };
        assert_variant!(
            h.exec.run_until_stalled(&mut h.fullmac_ifc_proxy.sae_frame_rx(&sae_frame)),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let frame = assert_variant!(event, fidl_mlme::MlmeEvent::OnSaeFrameRx { frame } => frame);
        assert_eq!(
            frame,
            fidl_mlme::SaeFrame {
                peer_sta_address: [1u8; 6],
                status_code: fidl_ieee80211::StatusCode::Success,
                seq_num: 2,
                sae_fields: vec![3u8; 4],
            }
        );
    }

    #[test]
    fn test_on_wmm_status_resp() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let status = zx::sys::ZX_OK;
        let wmm_params = fidl_common::WlanWmmParameters {
            apsd: true,
            ac_be_params: fidl_common::WlanWmmAccessCategoryParameters {
                ecw_min: 1,
                ecw_max: 2,
                aifsn: 3,
                txop_limit: 4,
                acm: true,
            },
            ac_bk_params: fidl_common::WlanWmmAccessCategoryParameters {
                ecw_min: 5,
                ecw_max: 6,
                aifsn: 7,
                txop_limit: 8,
                acm: false,
            },
            ac_vi_params: fidl_common::WlanWmmAccessCategoryParameters {
                ecw_min: 9,
                ecw_max: 10,
                aifsn: 11,
                txop_limit: 12,
                acm: true,
            },
            ac_vo_params: fidl_common::WlanWmmAccessCategoryParameters {
                ecw_min: 13,
                ecw_max: 14,
                aifsn: 15,
                txop_limit: 16,
                acm: false,
            },
        };
        assert_variant!(
            h.exec.run_until_stalled(
                &mut h.fullmac_ifc_proxy.on_wmm_status_resp(status, &wmm_params)
            ),
            Poll::Ready(Ok(()))
        );
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let (status, resp) = assert_variant!(event, fidl_mlme::MlmeEvent::OnWmmStatusResp { status, resp } => (status, resp));
        assert_eq!(status, zx::sys::ZX_OK);
        assert_eq!(
            resp,
            fidl_internal::WmmStatusResponse {
                apsd: true,
                ac_be_params: fidl_internal::WmmAcParams {
                    ecw_min: 1,
                    ecw_max: 2,
                    aifsn: 3,
                    txop_limit: 4,
                    acm: true,
                },
                ac_bk_params: fidl_internal::WmmAcParams {
                    ecw_min: 5,
                    ecw_max: 6,
                    aifsn: 7,
                    txop_limit: 8,
                    acm: false,
                },
                ac_vi_params: fidl_internal::WmmAcParams {
                    ecw_min: 9,
                    ecw_max: 10,
                    aifsn: 11,
                    txop_limit: 12,
                    acm: true,
                },
                ac_vo_params: fidl_internal::WmmAcParams {
                    ecw_min: 13,
                    ecw_max: 14,
                    aifsn: 15,
                    txop_limit: 16,
                    acm: false,
                },
            }
        );
    }

    struct TestHelper {
        fake_device: Arc<Mutex<FakeFullmacDeviceMocks>>,
        mlme_request_sender: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        fullmac_ifc_proxy: fidl_fullmac::WlanFullmacImplIfcProxy,
        mlme_event_receiver: mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>,
        driver_calls: mpsc::UnboundedReceiver<DriverCall>,
        exec: fasync::TestExecutor,
    }

    impl TestHelper {
        // By default, MLME is set up closed
        pub fn set_up() -> (Self, Pin<Box<impl Future<Output = Result<(), anyhow::Error>>>>) {
            Self::set_up_with_link_state(fidl_mlme::ControlledPortState::Closed)
        }

        /// For tests that need to observe all OnLinkStateChanged calls, they can choose what link
        /// state the device starts with to ensure that none get filtered out by MLME.
        pub fn set_up_with_link_state(
            device_link_state: fidl_mlme::ControlledPortState,
        ) -> (Self, Pin<Box<impl Future<Output = Result<(), anyhow::Error>>>>) {
            let exec = fasync::TestExecutor::new();

            let (fake_device, driver_call_receiver) = FakeFullmacDevice::new();
            let (mlme_request_sender, mlme_request_stream) = mpsc::unbounded();
            let (mlme_event_sender, mlme_event_receiver) = mpsc::unbounded();
            let mlme_event_sink = UnboundedSink::new(mlme_event_sender);

            let (driver_event_sender, driver_event_stream) = mpsc::unbounded();
            let driver_event_sink = FullmacDriverEventSink(UnboundedSink::new(driver_event_sender));

            let (fullmac_ifc_proxy, fullmac_ifc_request_stream) =
                fidl::endpoints::create_proxy_and_stream::<fidl_fullmac::WlanFullmacImplIfcMarker>(
                );

            let mocks = fake_device.mocks.clone();
            let main_loop = MlmeMainLoop {
                device: fake_device,
                mlme_request_stream,
                mlme_event_sink,
                driver_event_stream,
                is_bss_protected: false,
                device_link_state,
            };

            let test_fut = Box::pin(main_loop.serve(fullmac_ifc_request_stream, driver_event_sink));

            let test_helper = TestHelper {
                fake_device: mocks,
                mlme_request_sender,
                fullmac_ifc_proxy,
                mlme_event_receiver,
                driver_calls: driver_call_receiver,
                exec,
            };
            (test_helper, test_fut)
        }
    }
}
