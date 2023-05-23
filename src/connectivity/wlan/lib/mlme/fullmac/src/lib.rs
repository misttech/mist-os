// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod convert;
pub mod device;
mod logger;

use {
    crate::{convert::banjo_to_fidl, device::FullmacDeviceInterface},
    anyhow::bail,
    banjo_fuchsia_wlan_common as banjo_wlan_common, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_internal as fidl_internal,
    fidl_fuchsia_wlan_mlme as fidl_mlme, fidl_fuchsia_wlan_sme as fidl_sme,
    fuchsia_async as fasync,
    fuchsia_inspect::{self, Inspector},
    fuchsia_inspect_contrib::auto_persist,
    fuchsia_zircon as zx,
    futures::{
        self,
        channel::{mpsc, oneshot},
        select, StreamExt,
    },
    rand,
    tracing::{error, info, warn},
    wlan_common::{hasher::WlanHasher, sink::UnboundedSink},
    wlan_sme::{self, serve::create_sme},
};

#[derive(thiserror::Error, Debug)]
pub enum FullmacMlmeError {
    #[error("Unable to get startup result")]
    UnableToGetStartupResult,
    #[error("device.start failed: {0}")]
    DeviceStartFailed(zx::Status),
    #[error("Failed to get usme bootstrap stream: {0}")]
    FailedToGetUsmeBootstrapStream(fidl::Error),
    #[error("USME bootstrap stream failed: {0}")]
    UsmeBootstrapStreamFailed(fidl::Error),
    #[error("USME bootstrap stream terminated")]
    UsmeBootstrapStreamTerminated,
    #[error("Failed to duplicate inspect VMO")]
    FailedToDuplicateInspectVmo,
    #[error("Failed to respond to usme bootstrap request: {0}")]
    FailedToRespondToUsmeBootstrapRequest(fidl::Error),
    #[error("Failed to get generic SME stream: {0}")]
    FailedToGetGenericSmeStream(fidl::Error),
    #[error("Failed to convert DeviceInfo: {0}")]
    FailedToConvertDeviceInfo(anyhow::Error),
    #[error("Invalid MAC implementation type: {0:?}")]
    InvalidMacImplementationType(fidl_common::MacImplementationType),
    #[error("Failed to convert MAC sublayer support: {0}")]
    FailedToConvertMacSublayerSupport(anyhow::Error),
    #[error("Failed to create persistence proxy: {0}")]
    FailedToCreatePersistenceProxy(fidl::Error),
    #[error("Failed to create sme: {0}")]
    FailedToCreateSme(anyhow::Error),
}

#[derive(Debug)]
pub struct FullmacDriverEventSink(pub UnboundedSink<FullmacDriverEvent>);

#[derive(Debug)]
pub enum FullmacDriverEvent {
    Stop,
    OnScanResult { result: fidl_mlme::ScanResult },
    OnScanEnd { end: fidl_mlme::ScanEnd },
    ConnectConf { resp: fidl_mlme::ConnectConfirm },
    RoamConf { resp: fidl_mlme::RoamConfirm },
    AuthInd { ind: fidl_mlme::AuthenticateIndication },
    DeauthConf { resp: fidl_mlme::DeauthenticateConfirm },
    DeauthInd { ind: fidl_mlme::DeauthenticateIndication },
    AssocInd { ind: fidl_mlme::AssociateIndication },
    DisassocConf { resp: fidl_mlme::DisassociateConfirm },
    DisassocInd { ind: fidl_mlme::DisassociateIndication },
    StartConf { resp: fidl_mlme::StartConfirm },
    StopConf { resp: fidl_mlme::StopConfirm },
    EapolConf { resp: fidl_mlme::EapolConfirm },
    OnChannelSwitch { resp: fidl_internal::ChannelSwitchInfo },

    SignalReport { ind: fidl_internal::SignalReportIndication },
    EapolInd { ind: fidl_mlme::EapolIndication },
    OnPmkAvailable { info: fidl_mlme::PmkInfo },
    SaeHandshakeInd { ind: fidl_mlme::SaeHandshakeIndication },
    SaeFrameRx { frame: fidl_mlme::SaeFrame },
    OnWmmStatusResp { status: i32, resp: fidl_internal::WmmStatusResponse },
}

pub struct FullmacMlmeHandle {
    driver_event_sender: mpsc::UnboundedSender<FullmacDriverEvent>,
    mlme_loop_join_handle: Option<std::thread::JoinHandle<()>>,
}

impl FullmacMlmeHandle {
    pub fn stop(&mut self) {
        if let Err(e) = self.driver_event_sender.unbounded_send(FullmacDriverEvent::Stop) {
            error!("Cannot signal MLME event loop thread: {}", e);
        }
        match self.mlme_loop_join_handle.take() {
            Some(join_handle) => {
                if let Err(e) = join_handle.join() {
                    error!("MLME event loop thread panicked: {:?}", e);
                }
            }
            None => warn!("Called stop on already stopped MLME"),
        }
    }

    pub fn delete(mut self) {
        if self.mlme_loop_join_handle.is_some() {
            warn!("Called delete on FullmacMlmeHandle before calling stop.");
            self.stop()
        }
    }
}

const INSPECT_VMO_SIZE_BYTES: usize = 1000 * 1024;

pub struct FullmacMlme {
    device: FullmacDeviceInterface,
    mlme_request_stream: wlan_sme::MlmeStream,
    mlme_event_sink: wlan_sme::MlmeEventSink,
    driver_event_stream: mpsc::UnboundedReceiver<FullmacDriverEvent>,
    is_bss_protected: bool,
}

impl FullmacMlme {
    pub fn start(device: FullmacDeviceInterface) -> Result<FullmacMlmeHandle, anyhow::Error> {
        // Logger requires the executor to be initialized first.
        let mut executor = fasync::LocalExecutor::new();
        logger::init();

        let (driver_event_sender, driver_event_stream) = mpsc::unbounded();
        let driver_event_sender_clone = driver_event_sender.clone();
        let inspector = Inspector::new(
            fuchsia_inspect::InspectorConfig::default().size(INSPECT_VMO_SIZE_BYTES),
        );
        let inspect_usme_node = inspector.root().create_child("usme");

        let (startup_sender, startup_receiver) = oneshot::channel();
        let mlme_loop_join_handle = std::thread::spawn(move || {
            info!("Starting WLAN MLME main loop");
            let mut executor = fasync::LocalExecutor::new();
            let future = Self::main_loop_thread(
                device,
                driver_event_sender_clone,
                driver_event_stream,
                inspector,
                inspect_usme_node,
                startup_sender,
            );
            executor.run_singlethreaded(future);
        });

        let startup_result = executor.run_singlethreaded(startup_receiver);
        match startup_result.map_err(|_e| FullmacMlmeError::UnableToGetStartupResult) {
            Ok(Ok(())) => Ok(FullmacMlmeHandle {
                driver_event_sender,
                mlme_loop_join_handle: Some(mlme_loop_join_handle),
            }),
            Err(err) | Ok(Err(err)) => match mlme_loop_join_handle.join() {
                Ok(()) => bail!("Failed to start the MLME event loop: {:?}", err),
                Err(panic_err) => {
                    bail!("MLME event loop failed and then panicked: {}, {:?}", err, panic_err)
                }
            },
        }
    }

    async fn main_loop_thread(
        device: FullmacDeviceInterface,
        driver_event_sink: mpsc::UnboundedSender<FullmacDriverEvent>,
        driver_event_stream: mpsc::UnboundedReceiver<FullmacDriverEvent>,
        inspector: Inspector,
        inspect_usme_node: fuchsia_inspect::Node,
        startup_sender: oneshot::Sender<Result<(), FullmacMlmeError>>,
    ) {
        let driver_event_sink =
            Box::new(FullmacDriverEventSink(UnboundedSink::new(driver_event_sink)));
        let ifc = device::WlanFullmacIfcProtocol::new(driver_event_sink);
        let usme_bootstrap_protocol_handle = match device.start(&ifc) {
            Ok(handle) => handle,
            Err(e) => {
                // Failure to unwrap indicates a critical failure in the driver init thread.
                startup_sender.send(Err(FullmacMlmeError::DeviceStartFailed(e))).unwrap();
                return;
            }
        };
        let channel = zx::Channel::from(usme_bootstrap_protocol_handle);
        let server = fidl::endpoints::ServerEnd::<fidl_sme::UsmeBootstrapMarker>::new(channel);
        let (mut usme_bootstrap_stream, _usme_bootstrap_control_handle) =
            match server.into_stream_and_control_handle() {
                Ok(res) => res,
                Err(e) => {
                    // Failure to unwrap indicates a critical failure in the driver init thread.
                    startup_sender
                        .send(Err(FullmacMlmeError::FailedToGetUsmeBootstrapStream(e)))
                        .unwrap();
                    return;
                }
            };

        let (generic_sme_server, legacy_privacy_support, responder) = match usme_bootstrap_stream
            .next()
            .await
        {
            Some(Ok(fidl_sme::UsmeBootstrapRequest::Start {
                generic_sme_server,
                legacy_privacy_support,
                responder,
                ..
            })) => (generic_sme_server, legacy_privacy_support, responder),
            Some(Err(e)) => {
                startup_sender.send(Err(FullmacMlmeError::UsmeBootstrapStreamFailed(e))).unwrap();
                return;
            }
            None => {
                startup_sender.send(Err(FullmacMlmeError::UsmeBootstrapStreamTerminated)).unwrap();
                return;
            }
        };
        let inspect_vmo = match inspector.duplicate_vmo() {
            Some(vmo) => vmo,
            None => {
                startup_sender.send(Err(FullmacMlmeError::FailedToDuplicateInspectVmo)).unwrap();
                return;
            }
        };
        if let Err(e) = responder.send(inspect_vmo).into() {
            startup_sender
                .send(Err(FullmacMlmeError::FailedToRespondToUsmeBootstrapRequest(e)))
                .unwrap();
            return;
        }
        let generic_sme_stream = match generic_sme_server.into_stream() {
            Ok(stream) => stream,
            Err(e) => {
                startup_sender.send(Err(FullmacMlmeError::FailedToGetGenericSmeStream(e))).unwrap();
                return;
            }
        };

        // Create SME
        let cfg = wlan_sme::Config {
            wep_supported: legacy_privacy_support.wep_supported,
            wpa1_supported: legacy_privacy_support.wpa1_supported,
        };

        let (mlme_event_sender, mlme_event_receiver) = mpsc::unbounded();
        let mlme_event_sink = UnboundedSink::new(mlme_event_sender);

        let info = device.query_device_info();
        let device_info = match banjo_to_fidl::convert_device_info(info) {
            Ok(info) => info,
            Err(e) => {
                startup_sender.send(Err(FullmacMlmeError::FailedToConvertDeviceInfo(e))).unwrap();
                return;
            }
        };

        let support = device.query_mac_sublayer_support();
        let mac_sublayer_support = match banjo_to_fidl::convert_mac_sublayer_support(support) {
            Ok(s) => {
                if s.device.mac_implementation_type != fidl_common::MacImplementationType::Fullmac {
                    startup_sender
                        .send(Err(FullmacMlmeError::InvalidMacImplementationType(
                            s.device.mac_implementation_type,
                        )))
                        .unwrap();
                    return;
                }
                s
            }
            Err(e) => {
                startup_sender
                    .send(Err(FullmacMlmeError::FailedToConvertMacSublayerSupport(e)))
                    .unwrap();
                return;
            }
        };

        let support = device.query_security_support();
        let security_support = banjo_to_fidl::convert_security_support(support);

        let support = device.query_spectrum_management_support();
        let spectrum_management_support =
            banjo_to_fidl::convert_spectrum_management_support(support);

        // According to doc, `rand::random` uses ThreadRng, which is cryptographically secure:
        // https://docs.rs/rand/0.5.0/rand/rngs/struct.ThreadRng.html
        let wlan_hasher = WlanHasher::new(rand::random::<u64>().to_le_bytes());

        // TODO(fxbug.dev/113677): Get persistence working by adding the appropriate configs
        //                         in *.cml files
        let (persistence_proxy, _persistence_server_end) = match fidl::endpoints::create_proxy::<
            fidl_fuchsia_diagnostics_persist::DataPersistenceMarker,
        >() {
            Ok(r) => r,
            Err(e) => {
                startup_sender
                    .send(Err(FullmacMlmeError::FailedToCreatePersistenceProxy(e)))
                    .unwrap();
                return;
            }
        };
        let (persistence_req_sender, _persistence_req_forwarder_fut) =
            auto_persist::create_persistence_req_sender(persistence_proxy);

        let (mlme_request_stream, sme_fut) = match create_sme(
            cfg.into(),
            mlme_event_receiver,
            &device_info,
            mac_sublayer_support,
            security_support,
            spectrum_management_support,
            inspect_usme_node,
            wlan_hasher,
            persistence_req_sender,
            generic_sme_stream,
        ) {
            Ok((mlme_request_stream, sme_fut)) => (mlme_request_stream, sme_fut),
            Err(e) => {
                startup_sender.send(Err(FullmacMlmeError::FailedToCreateSme(e))).unwrap();
                return;
            }
        };

        let mlme = Self {
            device,
            mlme_request_stream,
            mlme_event_sink,
            driver_event_stream,
            is_bss_protected: false,
        };

        // Startup is complete. Signal the main thread to proceed.
        // Failure to unwrap indicates a critical failure in the driver init thread.
        startup_sender.send(Ok(())).unwrap();

        let mlme_main_loop_fut = mlme.run_main_loop();
        match futures::try_join!(mlme_main_loop_fut, sme_fut) {
            Ok(_) => info!("MLME and/or SME event loop exited gracefully"),
            Err(e) => error!("MLME and/or SME event loop exited with error: {:?}", e),
        }
    }

    pub async fn run_main_loop(mut self) -> Result<(), anyhow::Error> {
        let mac_role = self.device.query_device_info().role;
        loop {
            select! {
                mlme_request = self.mlme_request_stream.next() => match mlme_request {
                    Some(req) => self.handle_mlme_request(req),
                    None => bail!("MLME request stream terminated unexpectedly."),
                },
                driver_event = self.driver_event_stream.next() => match driver_event {
                    Some(event) => {
                        if let DriverState::Stopping = self.handle_driver_event(event, &mac_role) {
                            return Ok(())
                        }
                    },
                    None => bail!("Driver event stream terminated unexpectedly."),
                }
            }
        }
    }

    fn handle_mlme_request(&mut self, req: wlan_sme::MlmeRequest) {
        use convert::fidl_to_banjo::*;
        use wlan_sme::MlmeRequest::*;
        match req {
            Scan(req) => self.handle_mlme_scan_request(req),
            Connect(req) => {
                self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
                self.is_bss_protected = !req.security_ie.is_empty();
                self.device.connect_req(&mut convert_connect_request(&req))
            }
            Reconnect(req) => self.device.reconnect_req(convert_reconnect_request(&req)),
            AuthResponse(resp) => self.device.auth_resp(convert_authenticate_response(&resp)),
            Deauthenticate(req) => {
                self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
                self.device.deauth_req(convert_deauthenticate_request(&req))
            }
            AssocResponse(resp) => self.device.assoc_resp(convert_associate_response(&resp)),
            Disassociate(req) => self.device.disassoc_req(convert_disassociate_request(&req)),
            Reset(req) => self.device.reset_req(convert_reset_request(&req)),
            Start(req) => self.device.start_req(convert_start_request(&req)),
            Stop(req) => self.device.stop_req(convert_stop_request(&req)),
            SetKeys(req) => self.handle_mlme_set_keys_request(req),
            DeleteKeys(req) => self.device.del_keys_req(convert_delete_keys_request(&req)),
            Eapol(req) => self.device.eapol_req(&mut convert_eapol_request(&req)),
            SendMpOpenAction(..) => info!("SendMpOpenAction is unsupported"),
            SendMpConfirmAction(..) => info!("SendMpConfirmAction is unsupported"),
            MeshPeeringEstablished(..) => info!("MeshPeeringEstablished is unsupported"),
            SetCtrlPort(req) => self.device.set_link_state(req.state),
            QueryDeviceInfo(responder) => {
                let info = self.device.query_device_info();
                match banjo_to_fidl::convert_device_info(info) {
                    Ok(info) => responder.respond(info),
                    Err(e) => warn!("Failed to convert DeviceInfo: {}", e),
                }
            }
            QueryDiscoverySupport(..) => info!("QueryDiscoverySupport is unsupported"),
            QueryMacSublayerSupport(responder) => {
                let support = self.device.query_mac_sublayer_support();
                match banjo_to_fidl::convert_mac_sublayer_support(support) {
                    Ok(s) => responder.respond(s),
                    Err(e) => warn!("Failed to convert MAC sublayer support: {}", e),
                }
            }
            QuerySecuritySupport(responder) => {
                let support = self.device.query_security_support();
                responder.respond(banjo_to_fidl::convert_security_support(support));
            }
            QuerySpectrumManagementSupport(responder) => {
                let support = self.device.query_spectrum_management_support();
                responder.respond(banjo_to_fidl::convert_spectrum_management_support(support));
            }
            GetIfaceCounterStats(responder) => {
                responder.respond(self.device.get_iface_counter_stats())
            }
            GetIfaceHistogramStats(responder) => {
                responder.respond(self.device.get_iface_histogram_stats())
            }
            GetMinstrelStats(_, _) => info!("GetMinstrelStats is unsupported"),
            ListMinstrelPeers(_) => info!("ListMinstrelPeers is unsupported"),
            SaeHandshakeResp(resp) => {
                self.device.sae_handshake_resp(convert_sae_handshake_response(&resp))
            }
            SaeFrameTx(frame) => self.device.sae_frame_tx(&mut convert_sae_frame(&frame)),
            WmmStatusReq => self.device.wmm_status_req(),
            FinalizeAssociation(..) => info!("FinalizeAssociation is unsupported"),
        }
    }

    fn handle_mlme_scan_request(&self, req: fidl_mlme::ScanRequest) {
        use convert::fidl_to_banjo::*;

        if req.channel_list.is_empty() {
            let end = fidl_mlme::ScanEnd {
                txn_id: req.txn_id,
                code: fidl_mlme::ScanResultCode::InvalidArgs,
            };
            self.mlme_event_sink.send(fidl_mlme::MlmeEvent::OnScanEnd { end });
            return;
        }
        let ssid_list_copy: Vec<_> =
            req.ssid_list.iter().map(|ssid_bytes| convert_ssid(&ssid_bytes[..])).collect();
        self.device.start_scan(&mut convert_scan_request(&req, &ssid_list_copy[..]))
    }

    fn handle_mlme_set_keys_request(&self, req: fidl_mlme::SetKeysRequest) {
        use convert::fidl_to_banjo::*;

        let result = convert_set_keys_request(&req).and_then(|mut banjo_req| {
            let set_keys_resp = self.device.set_keys_req(&mut banjo_req);
            banjo_to_fidl::convert_set_keys_resp(set_keys_resp, &req)
        });
        match result {
            Ok(resp) => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::SetKeysConf { conf: resp });
            }
            Err(e) => {
                warn!("Failed to convert SetKeysResp: {}", e);
            }
        }
    }

    fn handle_driver_event(
        &self,
        event: FullmacDriverEvent,
        mac_role: &banjo_wlan_common::WlanMacRole,
    ) -> DriverState {
        match event {
            FullmacDriverEvent::Stop => return DriverState::Stopping,
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
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Open);
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::ConnectConf { resp });
            }
            FullmacDriverEvent::RoamConf { resp } => {
                // TODO(fxbug.dev/120899) Implement RoamConf handling in MLME.
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::RoamConf { resp });
            }
            FullmacDriverEvent::AuthInd { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::AuthenticateInd { ind });
            }
            FullmacDriverEvent::DeauthConf { resp } => {
                if *mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DeauthenticateConf { resp });
            }
            FullmacDriverEvent::DeauthInd { ind } => {
                if *mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DeauthenticateInd { ind });
            }
            FullmacDriverEvent::AssocInd { ind } => {
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::AssociateInd { ind });
            }
            FullmacDriverEvent::DisassocConf { resp } => {
                if *mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DisassociateConf { resp });
            }
            FullmacDriverEvent::DisassocInd { ind } => {
                if *mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::DisassociateInd { ind });
            }
            FullmacDriverEvent::StartConf { resp } => {
                if resp.result_code == fidl_mlme::StartResultCode::Success {
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Open);
                }
                self.mlme_event_sink.send(fidl_mlme::MlmeEvent::StartConf { resp });
            }
            FullmacDriverEvent::StopConf { resp } => {
                if resp.result_code == fidl_mlme::StopResultCode::Success {
                    self.device.set_link_state(fidl_mlme::ControlledPortState::Closed);
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
        DriverState::Running
    }
}

enum DriverState {
    Running,
    Stopping,
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::device::test_utils::FakeFullmacDevice,
        fuchsia_async as fasync,
        futures::{task::Poll, Future},
        std::pin::Pin,
        wlan_common::assert_variant,
    };

    #[test]
    fn test_main_loop_thread_happy_path() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(startup_result, Ok(()));

        let (client_sme_proxy, client_sme_server) =
            fidl::endpoints::create_proxy::<fidl_sme::ClientSmeMarker>()
                .expect("creating ClientSme proxy should succeed");
        let mut client_sme_response_fut = h.generic_sme_proxy.get_client_sme(client_sme_server);
        assert_variant!(h.exec.run_until_stalled(&mut client_sme_response_fut), Poll::Pending);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        assert_variant!(
            h.exec.run_until_stalled(&mut client_sme_response_fut),
            Poll::Ready(Ok(Ok(())))
        );

        let mut status_fut = client_sme_proxy.status();
        assert_variant!(h.exec.run_until_stalled(&mut status_fut), Poll::Pending);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);
        let status_result = assert_variant!(h.exec.run_until_stalled(&mut status_fut), Poll::Ready(result) => result);
        assert_variant!(status_result, Ok(fidl_sme::ClientStatusResponse::Idle(_)));
    }

    #[test]
    fn test_main_loop_thread_stops() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(startup_result, Ok(()));

        h.driver_event_sender
            .unbounded_send(FullmacDriverEvent::Stop)
            .expect("expect sending driver Stop event to succeed");
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));
    }

    #[test]
    fn test_main_loop_thread_fails_due_to_device_start() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.start_fn_status_mock = Some(zx::sys::ZX_ERR_BAD_STATE);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(startup_result, Err(FullmacMlmeError::DeviceStartFailed(_)))
    }

    #[test]
    fn test_main_loop_thread_fails_due_to_usme_bootstrap_terminated() {
        let bootstrap = false;
        let (mut h, mut test_fut) = TestHelper::set_up_with_usme_bootstrap(bootstrap);
        h.usme_bootstrap_proxy.take();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(startup_result, Err(FullmacMlmeError::UsmeBootstrapStreamTerminated));
    }

    #[test]
    fn test_main_loop_thread_fails_due_to_convert_device_info() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_device_info_mock.band_cap_count = 1;
        h.fake_device.query_device_info_mock.band_cap_list[0].band =
            banjo_wlan_common::WlanBand(255);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(startup_result, Err(FullmacMlmeError::FailedToConvertDeviceInfo(_)));
    }

    #[test]
    fn test_main_loop_thread_fails_due_to_wrong_mac_implementation_type() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_mac_sublayer_support_mock.device.mac_implementation_type =
            banjo_wlan_common::MacImplementationType::SOFTMAC;
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(startup_result, Err(FullmacMlmeError::InvalidMacImplementationType(_)));
    }

    #[test]
    fn test_main_loop_thread_fails_due_to_convert_mac_sublayer_support() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_mac_sublayer_support_mock.device.mac_implementation_type =
            banjo_wlan_common::MacImplementationType::FULLMAC;
        h.fake_device.query_mac_sublayer_support_mock.data_plane.data_plane_type =
            banjo_wlan_common::DataPlaneType(99);
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Ready(()));

        let startup_result =
            assert_variant!(h.startup_receiver.try_recv(), Ok(Some(result)) => result);
        assert_variant!(
            startup_result,
            Err(FullmacMlmeError::FailedToConvertMacSublayerSupport(_))
        );
    }

    struct TestHelper {
        fake_device: Pin<Box<FakeFullmacDevice>>,
        driver_event_sender: mpsc::UnboundedSender<FullmacDriverEvent>,
        usme_bootstrap_proxy: Option<fidl_sme::UsmeBootstrapProxy>,
        _usme_bootstrap_result: Option<fidl::client::QueryResponseFut<zx::Vmo>>,
        generic_sme_proxy: fidl_sme::GenericSmeProxy,
        startup_receiver: oneshot::Receiver<Result<(), FullmacMlmeError>>,
        exec: fasync::TestExecutor,
    }

    impl TestHelper {
        pub fn set_up() -> (Self, Pin<Box<impl Future<Output = ()>>>) {
            let bootstrap = true;
            Self::set_up_with_usme_bootstrap(bootstrap)
        }

        pub fn set_up_with_usme_bootstrap(
            bootstrap: bool,
        ) -> (Self, Pin<Box<impl Future<Output = ()>>>) {
            let exec = fasync::TestExecutor::new();

            let mut fake_device = Box::pin(FakeFullmacDevice::new());
            let usme_bootstrap_proxy = fake_device
                .usme_bootstrap_client_end
                .take()
                .unwrap()
                .into_proxy()
                .expect("converting into UsmeBootstrapProxy should succeed");

            let (generic_sme_proxy, generic_sme_server_end) =
                fidl::endpoints::create_proxy::<fidl_sme::GenericSmeMarker>()
                    .expect("creating GenericSmeProxy should succeed");
            let usme_bootstrap_result = if bootstrap {
                Some(usme_bootstrap_proxy.start(
                    generic_sme_server_end,
                    &fidl_sme::LegacyPrivacySupport { wep_supported: false, wpa1_supported: false },
                ))
            } else {
                None
            };

            let (driver_event_sender, driver_event_stream) = mpsc::unbounded();
            let inspector = Inspector::default();
            let inspect_usme_node = inspector.root().create_child("usme");
            let (startup_sender, startup_receiver) = oneshot::channel();

            let test_fut = Box::pin(FullmacMlme::main_loop_thread(
                fake_device.as_mut().as_device(),
                driver_event_sender.clone(),
                driver_event_stream,
                inspector,
                inspect_usme_node,
                startup_sender,
            ));

            let test_helper = TestHelper {
                fake_device,
                driver_event_sender,
                usme_bootstrap_proxy: Some(usme_bootstrap_proxy),
                _usme_bootstrap_result: usme_bootstrap_result,
                generic_sme_proxy,
                startup_receiver,
                exec,
            };
            (test_helper, test_fut)
        }
    }
}

#[cfg(test)]
mod handle_mlme_request_tests {
    use {
        super::*,
        crate::device::test_utils::{DriverCall, FakeFullmacDevice},
        banjo_fuchsia_hardware_wlan_fullmac as banjo_wlan_fullmac,
        banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211,
        fidl_fuchsia_wlan_stats as fidl_stats,
        std::pin::Pin,
        test_case::test_case,
        wlan_common::assert_variant,
    };

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

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let (driver_req, driver_req_channels, driver_req_ssids) = assert_variant!(driver_calls.next(), Some(DriverCall::StartScan { req, channels, ssids }) => (req, channels, ssids));
        assert_eq!(driver_req.txn_id, 1);
        assert_eq!(driver_req.scan_type, banjo_wlan_fullmac::WlanScanType::PASSIVE);
        assert_eq!(&driver_req_channels[..], &[2]);
        assert_eq!(driver_req.min_channel_time, 6);
        assert_eq!(driver_req.max_channel_time, 7);
        assert_eq!(driver_req_ssids.len(), 1);
        assert_eq!(driver_req_ssids[0].data[..driver_req_ssids[0].len as usize], [3u8; 4][..]);
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

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let (driver_req, driver_req_ssids) = assert_variant!(driver_calls.next(), Some(DriverCall::StartScan { req, ssids, .. }) => (req, ssids));
        assert_eq!(driver_req.scan_type, banjo_wlan_fullmac::WlanScanType::ACTIVE);
        assert!(driver_req_ssids.is_empty());
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

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(driver_calls.next(), None);
        let scan_end = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(fidl_mlme::MlmeEvent::OnScanEnd { end })) => end);
        assert_eq!(
            scan_end,
            fidl_mlme::ScanEnd { txn_id: 1, code: fidl_mlme::ScanResultCode::InvalidArgs }
        );
    }

    #[test]
    fn test_connect_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Connect(fidl_mlme::ConnectRequest {
            selected_bss: fidl_internal::BssDescription {
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
                cipher_suite_type: 11,
            })),
            security_ie: vec![12u8, 13],
        });

        h.mlme.handle_mlme_request(fidl_req);

        assert!(h.mlme.is_bss_protected);
        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(
            driver_calls.next(),
            Some(DriverCall::OnLinkStateChanged { online: false })
        );
        assert_variant!(driver_calls.next(), Some(DriverCall::ConnectReq { req, selected_bss_ies, sae_password, wep_key, security_ie }) => {
            assert_eq!(req.selected_bss.bssid, [100u8; 6]);
            assert_eq!(req.selected_bss.bss_type, banjo_wlan_common::BssType::INFRASTRUCTURE);
            assert_eq!(req.selected_bss.beacon_period, 101);
            assert_eq!(req.selected_bss.capability_info, 102);
            assert_eq!(*selected_bss_ies, vec![103u8, 104, 105]);
            assert_eq!(req.selected_bss.bss_type, banjo_wlan_common::BssType::INFRASTRUCTURE);
            assert_eq!(
                req.selected_bss.channel,
                banjo_wlan_common::WlanChannel {
                    primary: 106,
                    cbw: banjo_wlan_common::ChannelBandwidth::CBW40,
                    secondary80: 0,
                }
            );
            assert_eq!(req.selected_bss.rssi_dbm, 107);
            assert_eq!(req.selected_bss.snr_db, 108);

            assert_eq!(req.connect_failure_timeout, 1u32);
            assert_eq!(req.auth_type, banjo_wlan_fullmac::WlanAuthType::OPEN_SYSTEM);
            assert_eq!(*sae_password, vec![2u8, 3, 4]);

            assert_eq!(*wep_key, vec![5u8, 6]);
            assert_eq!(req.wep_key.key_id, 7);
            assert_eq!(req.wep_key.key_type, banjo_wlan_common::WlanKeyType::GROUP);
            assert_eq!(req.wep_key.address, [8u8; 6]);
            assert_eq!(req.wep_key.rsc, 9);
            assert_eq!(req.wep_key.cipher_suite_oui, [10u8; 3]);
            assert_eq!(req.wep_key.cipher_suite_type, 11);

            assert_eq!(*security_ie, vec![12u8, 13]);
        });
    }

    #[test]
    fn test_reconnect_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Reconnect(fidl_mlme::ReconnectRequest {
            peer_sta_address: [1u8; 6],
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::ReconnectReq { req }) => req);
        assert_eq!(
            *driver_req,
            banjo_wlan_fullmac::WlanFullmacReconnectReq { peer_sta_address: [1u8; 6] }
        );
    }

    #[test]
    fn test_authenticate_response() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::AuthResponse(fidl_mlme::AuthenticateResponse {
            peer_sta_address: [1u8; 6],
            result_code: fidl_mlme::AuthenticateResultCode::Success,
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::AuthResp { resp }) => resp);
        assert_eq!(
            *driver_req,
            banjo_wlan_fullmac::WlanFullmacAuthResp {
                peer_sta_address: [1u8; 6],
                result_code: banjo_wlan_fullmac::WlanAuthResult::SUCCESS,
            }
        );
    }

    #[test]
    fn test_deauthenticate_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Deauthenticate(fidl_mlme::DeauthenticateRequest {
            peer_sta_address: [1u8; 6],
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(
            driver_calls.next(),
            Some(DriverCall::OnLinkStateChanged { online: false })
        );
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::DeauthReq { req }) => req);
        assert_eq!(driver_req.peer_sta_address, [1u8; 6]);
        assert_eq!(
            driver_req.reason_code,
            banjo_wlan_ieee80211::ReasonCode::LEAVING_NETWORK_DEAUTH
        );
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

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::AssocResp { resp }) => resp);
        assert_eq!(driver_req.peer_sta_address, [1u8; 6]);
        assert_eq!(driver_req.result_code, banjo_wlan_fullmac::WlanAssocResult::SUCCESS);
        assert_eq!(driver_req.association_id, 2);
    }

    #[test]
    fn test_disassociate_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Disassociate(fidl_mlme::DisassociateRequest {
            peer_sta_address: [1u8; 6],
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc,
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::DisassocReq { req }) => req);
        assert_eq!(driver_req.peer_sta_address, [1u8; 6]);
        assert_eq!(
            driver_req.reason_code,
            banjo_wlan_ieee80211::ReasonCode::LEAVING_NETWORK_DISASSOC
        );
    }

    #[test]
    fn test_reset_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Reset(fidl_mlme::ResetRequest {
            sta_address: [1u8; 6],
            set_default_mib: true,
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::ResetReq { req }) => req);
        assert_eq!(
            *driver_req,
            banjo_wlan_fullmac::WlanFullmacResetReq {
                sta_address: [1u8; 6],
                set_default_mib: true,
            }
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

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::StartReq { req }) => req);
        assert_eq!(driver_req.ssid.len as usize, SSID_LEN);
        assert_eq!(driver_req.ssid.data[..SSID_LEN], [1u8; SSID_LEN][..]);
        assert_eq!(driver_req.bss_type, banjo_wlan_common::BssType::INFRASTRUCTURE);
        assert_eq!(driver_req.beacon_period, 3);
        assert_eq!(driver_req.dtim_period, 4);
        assert_eq!(driver_req.channel, 5);
        assert_eq!(driver_req.rsne_len as usize, RSNE_LEN);
        assert_eq!(driver_req.rsne[..RSNE_LEN], [14; RSNE_LEN][..]);
        assert_eq!(driver_req.vendor_ie, [0; 510]);
        assert_eq!(driver_req.vendor_ie_len, 0);
    }

    #[test]
    fn test_stop_request() {
        let mut h = TestHelper::set_up();
        const SSID_LEN: usize = 2;
        let fidl_req =
            wlan_sme::MlmeRequest::Stop(fidl_mlme::StopRequest { ssid: vec![1u8; SSID_LEN] });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::StopReq { req }) => req);
        assert_eq!(driver_req.ssid.len as usize, SSID_LEN);
        assert_eq!(driver_req.ssid.data[..SSID_LEN], [1u8; SSID_LEN][..]);
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
                cipher_suite_type: 11,
            }],
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let (driver_req, driver_req_keys) = assert_variant!(driver_calls.next(), Some(DriverCall::SetKeysReq { req, keys }) => (req, keys));
        assert_eq!(driver_req.num_keys, 1);
        assert_eq!(driver_req_keys[0], vec![5u8, 6]);
        assert_eq!(driver_req.keylist[0].key_id, 7);
        assert_eq!(driver_req.keylist[0].key_type, banjo_wlan_common::WlanKeyType::GROUP);
        assert_eq!(driver_req.keylist[0].address, [8u8; 6]);
        assert_eq!(driver_req.keylist[0].rsc, 9);
        assert_eq!(driver_req.keylist[0].cipher_suite_oui, [10u8; 3]);
        assert_eq!(driver_req.keylist[0].cipher_suite_type, 11);

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
        h.fake_device.set_keys_resp_mock = Some(banjo_wlan_fullmac::WlanFullmacSetKeysResp {
            num_keys: NUM_KEYS as u64,
            statuslist: [0i32, 1, 0, 0],
        });
        let mut keylist = vec![];
        let key = fidl_mlme::SetKeyDescriptor {
            key: vec![5u8, 6],
            key_id: 7,
            key_type: fidl_mlme::KeyType::Group,
            address: [8u8; 6],
            rsc: 9,
            cipher_suite_oui: [10u8; 3],
            cipher_suite_type: 11,
        };
        for i in 0..NUM_KEYS {
            keylist.push(fidl_mlme::SetKeyDescriptor { key_id: i as u16, ..key.clone() });
        }
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest { keylist });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let (driver_req, _driver_req_keys) = assert_variant!(driver_calls.next(), Some(DriverCall::SetKeysReq { req, keys }) => (req, keys));
        assert_eq!(driver_req.num_keys, NUM_KEYS as u64);
        for i in 0..NUM_KEYS {
            assert_eq!(driver_req.keylist[i].key_id, i as u16);
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
            cipher_suite_type: 11,
        };
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
            keylist: vec![key.clone(); 5],
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        // No SetKeysReq and SetKeysResp
        assert_variant!(driver_calls.next(), None);
        assert_variant!(h.mlme_event_receiver.try_next(), Err(_));
    }

    #[test]
    fn test_set_keys_request_when_resp_has_different_num_keys() {
        let mut h = TestHelper::set_up();
        h.fake_device.set_keys_resp_mock =
            Some(banjo_wlan_fullmac::WlanFullmacSetKeysResp { num_keys: 2, statuslist: [0i32; 4] });
        let fidl_req = wlan_sme::MlmeRequest::SetKeys(fidl_mlme::SetKeysRequest {
            keylist: vec![fidl_mlme::SetKeyDescriptor {
                key: vec![5u8, 6],
                key_id: 7,
                key_type: fidl_mlme::KeyType::Group,
                address: [8u8; 6],
                rsc: 9,
                cipher_suite_oui: [10u8; 3],
                cipher_suite_type: 11,
            }],
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(driver_calls.next(), Some(DriverCall::SetKeysReq { .. }));
        // No SetKeysConf MLME event because the SetKeysResp from driver has different number of
        // keys.
        assert_variant!(h.mlme_event_receiver.try_next(), Err(_));
    }

    #[test]
    fn test_delete_keys_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::DeleteKeys(fidl_mlme::DeleteKeysRequest {
            keylist: vec![fidl_mlme::DeleteKeyDescriptor {
                key_id: 1,
                key_type: fidl_mlme::KeyType::PeerKey,
                address: [2u8; 6],
            }],
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req =
            assert_variant!(driver_calls.next(), Some(DriverCall::DelKeysReq { req }) => req);
        assert_eq!(driver_req.num_keys, 1);
        assert_eq!(driver_req.keylist[0].key_id, 1);
        assert_eq!(driver_req.keylist[0].key_type, banjo_wlan_common::WlanKeyType::PEER);
        assert_eq!(driver_req.keylist[0].address, [2u8; 6]);
    }

    #[test]
    fn test_eapol_request() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::Eapol(fidl_mlme::EapolRequest {
            src_addr: [1u8; 6],
            dst_addr: [2u8; 6],
            data: vec![3u8; 4],
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let (driver_req, driver_req_data) = assert_variant!(driver_calls.next(), Some(DriverCall::EapolReq { req, data }) => (req, data));
        assert_eq!(driver_req.src_addr, [1u8; 6]);
        assert_eq!(driver_req.dst_addr, [2u8; 6]);
        assert_eq!(*driver_req_data, vec![3u8; 4]);
    }

    #[test_case(fidl_mlme::ControlledPortState::Open, true; "online")]
    #[test_case(fidl_mlme::ControlledPortState::Closed, false; "offline")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_set_ctrl_port(
        controlled_port_state: fidl_mlme::ControlledPortState,
        expected_link_state: bool,
    ) {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::SetCtrlPort(fidl_mlme::SetControlledPortRequest {
            peer_sta_address: [1u8; 6],
            state: controlled_port_state,
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(driver_calls.next(), Some(DriverCall::OnLinkStateChanged { online }) => {
            assert_eq!(*online, expected_link_state);
        });
    }

    #[test]
    fn test_get_iface_counter_stats() {
        let mut h = TestHelper::set_up();
        let mocked_stats = banjo_wlan_fullmac::WlanFullmacIfaceCounterStats {
            rx_unicast_drop: 11,
            rx_unicast_total: 22,
            rx_multicast: 33,
            tx_total: 44,
            tx_drop: 55,
        };
        h.fake_device.get_iface_counter_stats_mock.replace((zx::sys::ZX_OK, mocked_stats));
        let (stats_responder, mut stats_receiver) = wlan_sme::responder::Responder::new();
        let fidl_req = wlan_sme::MlmeRequest::GetIfaceCounterStats(stats_responder);

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(driver_calls.next(), Some(DriverCall::GetIfaceCounterStats));
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

        let noise_floor_samples =
            vec![banjo_wlan_fullmac::WlanFullmacHistBucket { bucket_index: 2, num_samples: 3 }];
        let noise_floor_histograms = vec![banjo_wlan_fullmac::WlanFullmacNoiseFloorHistogram {
            hist_scope: banjo_wlan_fullmac::WlanFullmacHistScope::STATION,
            antenna_id: banjo_wlan_fullmac::WlanFullmacAntennaId {
                freq: banjo_wlan_fullmac::WlanFullmacAntennaFreq::ANTENNA_2_G,
                index: 1,
            },
            noise_floor_samples_list: noise_floor_samples.as_ptr(),
            noise_floor_samples_count: noise_floor_samples.len(),
            invalid_samples: 4,
        }];

        let rssi_samples =
            vec![banjo_wlan_fullmac::WlanFullmacHistBucket { bucket_index: 6, num_samples: 7 }];
        let rssi_histograms = vec![banjo_wlan_fullmac::WlanFullmacRssiHistogram {
            hist_scope: banjo_wlan_fullmac::WlanFullmacHistScope::PER_ANTENNA,
            antenna_id: banjo_wlan_fullmac::WlanFullmacAntennaId {
                freq: banjo_wlan_fullmac::WlanFullmacAntennaFreq::ANTENNA_5_G,
                index: 5,
            },
            rssi_samples_list: rssi_samples.as_ptr(),
            rssi_samples_count: rssi_samples.len(),
            invalid_samples: 8,
        }];

        let rx_rate_index_samples =
            vec![banjo_wlan_fullmac::WlanFullmacHistBucket { bucket_index: 10, num_samples: 11 }];
        let rx_rate_index_histograms = vec![banjo_wlan_fullmac::WlanFullmacRxRateIndexHistogram {
            hist_scope: banjo_wlan_fullmac::WlanFullmacHistScope::STATION,
            antenna_id: banjo_wlan_fullmac::WlanFullmacAntennaId {
                freq: banjo_wlan_fullmac::WlanFullmacAntennaFreq::ANTENNA_5_G,
                index: 9,
            },
            rx_rate_index_samples_list: rx_rate_index_samples.as_ptr(),
            rx_rate_index_samples_count: rx_rate_index_samples.len(),
            invalid_samples: 12,
        }];

        let snr_samples =
            vec![banjo_wlan_fullmac::WlanFullmacHistBucket { bucket_index: 14, num_samples: 15 }];
        let snr_histograms = vec![banjo_wlan_fullmac::WlanFullmacSnrHistogram {
            hist_scope: banjo_wlan_fullmac::WlanFullmacHistScope::PER_ANTENNA,
            antenna_id: banjo_wlan_fullmac::WlanFullmacAntennaId {
                freq: banjo_wlan_fullmac::WlanFullmacAntennaFreq::ANTENNA_2_G,
                index: 13,
            },
            snr_samples_list: snr_samples.as_ptr(),
            snr_samples_count: snr_samples.len(),
            invalid_samples: 16,
        }];

        let mocked_stats = banjo_wlan_fullmac::WlanFullmacIfaceHistogramStats {
            noise_floor_histograms_list: noise_floor_histograms.as_ptr(),
            noise_floor_histograms_count: noise_floor_histograms.len(),
            rssi_histograms_list: rssi_histograms.as_ptr(),
            rssi_histograms_count: rssi_histograms.len(),
            rx_rate_index_histograms_list: rx_rate_index_histograms.as_ptr(),
            rx_rate_index_histograms_count: rx_rate_index_histograms.len(),
            snr_histograms_list: snr_histograms.as_ptr(),
            snr_histograms_count: snr_histograms.len(),
        };

        h.fake_device.get_iface_histogram_stats_mock.replace((zx::sys::ZX_OK, mocked_stats));
        let (stats_responder, mut stats_receiver) = wlan_sme::responder::Responder::new();
        let fidl_req = wlan_sme::MlmeRequest::GetIfaceHistogramStats(stats_responder);

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(driver_calls.next(), Some(DriverCall::GetIfaceHistogramStats));
        let stats = assert_variant!(stats_receiver.try_recv(), Ok(Some(stats)) => stats);
        let stats = assert_variant!(stats, fidl_mlme::GetIfaceHistogramStatsResponse::Stats(stats) => stats);
        assert_eq!(
            stats,
            fidl_stats::IfaceHistogramStats {
                noise_floor_histograms: vec![fidl_stats::NoiseFloorHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    noise_floor_samples: vec![fidl_stats::HistBucket {
                        bucket_index: 2,
                        num_samples: 3,
                    }],
                    invalid_samples: 4,
                }],
                rssi_histograms: vec![fidl_stats::RssiHistogram {
                    hist_scope: fidl_stats::HistScope::PerAntenna,
                    antenna_id: Some(Box::new(fidl_stats::AntennaId {
                        freq: fidl_stats::AntennaFreq::Antenna5G,
                        index: 5,
                    })),
                    rssi_samples: vec![fidl_stats::HistBucket { bucket_index: 6, num_samples: 7 }],
                    invalid_samples: 8,
                }],
                rx_rate_index_histograms: vec![fidl_stats::RxRateIndexHistogram {
                    hist_scope: fidl_stats::HistScope::Station,
                    antenna_id: None,
                    rx_rate_index_samples: vec![fidl_stats::HistBucket {
                        bucket_index: 10,
                        num_samples: 11,
                    }],
                    invalid_samples: 12,
                }],
                snr_histograms: vec![fidl_stats::SnrHistogram {
                    hist_scope: fidl_stats::HistScope::PerAntenna,
                    antenna_id: Some(Box::new(fidl_stats::AntennaId {
                        freq: fidl_stats::AntennaFreq::Antenna2G,
                        index: 13,
                    })),
                    snr_samples: vec![fidl_stats::HistBucket { bucket_index: 14, num_samples: 15 }],
                    invalid_samples: 16,
                }],
            }
        );
    }

    #[test]
    fn test_sae_handshake_resp() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::SaeHandshakeResp(fidl_mlme::SaeHandshakeResponse {
            peer_sta_address: [1u8; 6],
            status_code: fidl_ieee80211::StatusCode::AntiCloggingTokenRequired,
        });

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let driver_req = assert_variant!(driver_calls.next(), Some(DriverCall::SaeHandshakeResp { resp }) => resp);
        assert_eq!(driver_req.peer_sta_address, [1u8; 6]);
        assert_eq!(
            driver_req.status_code,
            banjo_wlan_ieee80211::StatusCode::ANTI_CLOGGING_TOKEN_REQUIRED
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

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        let (driver_frame, driver_frame_sae_fields) = assert_variant!(driver_calls.next(), Some(DriverCall::SaeFrameTx { frame, sae_fields }) => (frame, sae_fields));
        assert_eq!(driver_frame.peer_sta_address, [1u8; 6]);
        assert_eq!(driver_frame.status_code, banjo_wlan_ieee80211::StatusCode::SUCCESS);
        assert_eq!(driver_frame.seq_num, 2);
        assert_eq!(*driver_frame_sae_fields, vec![3u8; 4]);
    }

    #[test]
    fn test_wmm_status_req() {
        let mut h = TestHelper::set_up();
        let fidl_req = wlan_sme::MlmeRequest::WmmStatusReq;

        h.mlme.handle_mlme_request(fidl_req);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(driver_calls.next(), Some(DriverCall::WmmStatusReq));
    }

    pub struct TestHelper {
        fake_device: Pin<Box<FakeFullmacDevice>>,
        mlme: FullmacMlme,
        mlme_event_receiver: mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>,
        _mlme_request_sender: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        _driver_event_sender: mpsc::UnboundedSender<FullmacDriverEvent>,
    }

    impl TestHelper {
        pub fn set_up() -> Self {
            let mut fake_device = Box::pin(FakeFullmacDevice::new());
            let (mlme_request_sender, mlme_request_stream) = mpsc::unbounded();
            let (mlme_event_sender, mlme_event_receiver) = mpsc::unbounded();
            let mlme_event_sink = UnboundedSink::new(mlme_event_sender);

            let (driver_event_sender, driver_event_stream) = mpsc::unbounded();

            let mlme = FullmacMlme {
                device: fake_device.as_mut().as_device(),
                mlme_request_stream,
                mlme_event_sink,
                driver_event_stream,
                is_bss_protected: false,
            };
            Self {
                fake_device,
                mlme,
                mlme_event_receiver,
                _mlme_request_sender: mlme_request_sender,
                _driver_event_sender: driver_event_sender,
            }
        }
    }
}

#[cfg(test)]
mod handle_driver_event_tests {
    use crate::device::WlanFullmacIfcProtocol;

    use {
        super::*,
        crate::device::test_utils::{DriverCall, FakeFullmacDevice},
        banjo_fuchsia_hardware_wlan_fullmac as banjo_wlan_fullmac,
        banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211,
        banjo_fuchsia_wlan_internal as banjo_wlan_internal, fidl_fuchsia_wlan_mlme as fidl_mlme,
        fuchsia_async as fasync,
        futures::{task::Poll, Future},
        std::pin::Pin,
        test_case::test_case,
        wlan_common::{assert_variant, fake_fidl_bss_description},
    };

    fn create_bss_descriptions(
    ) -> (banjo_wlan_internal::BssDescription, fidl_internal::BssDescription, Vec<u8>) {
        let ies = vec![3, 4, 5];
        let banjo_bss = banjo_wlan_internal::BssDescription {
            bssid: [9u8; 6],
            bss_type: banjo_wlan_common::BssType::INFRASTRUCTURE,
            beacon_period: 1,
            capability_info: 2,
            ies_list: ies.as_ptr(),
            ies_count: ies.len(),
            channel: banjo_wlan_common::WlanChannel {
                primary: 6,
                cbw: banjo_wlan_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            },
            rssi_dbm: 7,
            snr_db: 8,
        };

        let fidl_bss = fidl_internal::BssDescription {
            bssid: [9u8; 6],
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 1,
            capability_info: 2,
            ies: ies.clone(),
            channel: fidl_common::WlanChannel {
                primary: 6,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            rssi_dbm: 7,
            snr_db: 8,
        };

        (banjo_bss, fidl_bss, ies)
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

        let (banjo_bss, expected_fidl_bss, _ies_buffer) = create_bss_descriptions();
        let scan_result = banjo_wlan_fullmac::WlanFullmacScanResult {
            txn_id: 42u64,
            timestamp_nanos: 1337i64,
            bss: banjo_bss,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).on_scan_result)(
                &mut h.driver_event_protocol.ctx,
                &scan_result,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let result =
            assert_variant!(event, fidl_mlme::MlmeEvent::OnScanResult { result } => result);
        assert_eq!(
            result,
            fidl_mlme::ScanResult {
                txn_id: 42u64,
                timestamp_nanos: 1337i64,
                bss: expected_fidl_bss,
            }
        );
    }

    #[test]
    fn test_on_scan_end() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let scan_end = banjo_wlan_fullmac::WlanFullmacScanEnd {
            txn_id: 42u64,
            code: banjo_wlan_fullmac::WlanScanResult::SUCCESS,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).on_scan_end)(
                &mut h.driver_event_protocol.ctx,
                &scan_end,
            );
        }
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

        let association_ies = vec![1, 2, 3, 4, 5, 6, 7, 8, 9];
        let connect_conf = banjo_wlan_fullmac::WlanFullmacConnectConfirm {
            peer_sta_address: [1u8; 6],
            result_code: banjo_wlan_ieee80211::StatusCode::SUCCESS,
            association_id: 2,
            association_ies_list: association_ies.as_ptr(),
            association_ies_count: association_ies.len(),
        };
        unsafe {
            ((*h.driver_event_protocol.ops).connect_conf)(
                &mut h.driver_event_protocol.ctx,
                &connect_conf,
            );
        }
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

    #[test_case(true, banjo_wlan_ieee80211::StatusCode::SUCCESS, false; "secure connect with success status is not online yet")]
    #[test_case(false, banjo_wlan_ieee80211::StatusCode::SUCCESS, true; "insecure connect with success status is online right away")]
    #[test_case(true, banjo_wlan_ieee80211::StatusCode::REFUSED_REASON_UNSPECIFIED, false; "secure connect with failed status is not online")]
    #[test_case(false, banjo_wlan_ieee80211::StatusCode::REFUSED_REASON_UNSPECIFIED, false; "insecure connect with failed status in not online")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_connect_req_connect_conf_link_state(
        secure_connect: bool,
        connect_result_code: banjo_wlan_ieee80211::StatusCode,
        expected_online: bool,
    ) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let connect_req =
            create_connect_request(if secure_connect { vec![7u8, 8] } else { vec![] });
        h.mlme_request_sender
            .unbounded_send(connect_req)
            .expect("sending ConnectReq should succeed");
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let association_ies = vec![1, 2, 3, 4, 5, 6, 7, 8, 9];
        let connect_conf = banjo_wlan_fullmac::WlanFullmacConnectConfirm {
            peer_sta_address: [1u8; 6],
            result_code: connect_result_code,
            association_id: 2,
            association_ies_list: association_ies.as_ptr(),
            association_ies_count: association_ies.len(),
        };
        unsafe {
            ((*h.driver_event_protocol.ops).connect_conf)(
                &mut h.driver_event_protocol.ctx,
                &connect_conf,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        assert_variant!(
            driver_calls.next(),
            Some(DriverCall::OnLinkStateChanged { online: false })
        );
        assert_variant!(driver_calls.next(), Some(DriverCall::ConnectReq { .. }));
        if expected_online {
            assert_variant!(
                driver_calls.next(),
                Some(DriverCall::OnLinkStateChanged { online: true })
            );
        } else {
            assert_variant!(driver_calls.next(), None);
        }
    }

    #[test]
    fn test_auth_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let auth_ind = banjo_wlan_fullmac::WlanFullmacAuthInd {
            peer_sta_address: [1u8; 6],
            auth_type: banjo_wlan_fullmac::WlanAuthType::OPEN_SYSTEM,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).auth_ind)(&mut h.driver_event_protocol.ctx, &auth_ind);
        }
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

    #[test_case(banjo_wlan_common::WlanMacRole::CLIENT; "client")]
    #[test_case(banjo_wlan_common::WlanMacRole::AP; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_deauth_conf(mac_role: banjo_wlan_common::WlanMacRole) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_device_info_mock.role = mac_role;
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let deauth_conf =
            banjo_wlan_fullmac::WlanFullmacDeauthConfirm { peer_sta_address: [1u8; 6] };
        unsafe {
            ((*h.driver_event_protocol.ops).deauth_conf)(
                &mut h.driver_event_protocol.ctx,
                &deauth_conf,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        if mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
            assert_variant!(
                driver_calls.next(),
                Some(DriverCall::OnLinkStateChanged { online: false })
            );
        } else {
            assert_variant!(driver_calls.next(), None);
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf =
            assert_variant!(event, fidl_mlme::MlmeEvent::DeauthenticateConf { resp } => resp);
        assert_eq!(conf, fidl_mlme::DeauthenticateConfirm { peer_sta_address: [1u8; 6] });
    }

    #[test_case(banjo_wlan_common::WlanMacRole::CLIENT; "client")]
    #[test_case(banjo_wlan_common::WlanMacRole::AP; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_deauth_ind(mac_role: banjo_wlan_common::WlanMacRole) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_device_info_mock.role = mac_role;
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let deauth_ind = banjo_wlan_fullmac::WlanFullmacDeauthIndication {
            peer_sta_address: [1u8; 6],
            reason_code: banjo_wlan_ieee80211::ReasonCode::LEAVING_NETWORK_DEAUTH,
            locally_initiated: true,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).deauth_ind)(
                &mut h.driver_event_protocol.ctx,
                &deauth_ind,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        if mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
            assert_variant!(
                driver_calls.next(),
                Some(DriverCall::OnLinkStateChanged { online: false })
            );
        } else {
            assert_variant!(driver_calls.next(), None);
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

        let assoc_ind = banjo_wlan_fullmac::WlanFullmacAssocInd {
            peer_sta_address: [1u8; 6],
            listen_interval: 2,
            ssid: banjo_wlan_ieee80211::CSsid { data: [3u8; 32], len: 4 },
            rsne: [5u8; 255],
            rsne_len: 6,
            vendor_ie: [7u8; 510],
            vendor_ie_len: 8,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).assoc_ind)(
                &mut h.driver_event_protocol.ctx,
                &assoc_ind,
            );
        }
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

    #[test_case(banjo_wlan_common::WlanMacRole::CLIENT; "client")]
    #[test_case(banjo_wlan_common::WlanMacRole::AP; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_disassoc_conf(mac_role: banjo_wlan_common::WlanMacRole) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_device_info_mock.role = mac_role;
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let disassoc_conf = banjo_wlan_fullmac::WlanFullmacDisassocConfirm { status: 1 };
        unsafe {
            ((*h.driver_event_protocol.ops).disassoc_conf)(
                &mut h.driver_event_protocol.ctx,
                &disassoc_conf,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        if mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
            assert_variant!(
                driver_calls.next(),
                Some(DriverCall::OnLinkStateChanged { online: false })
            );
        } else {
            assert_variant!(driver_calls.next(), None);
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::DisassociateConf { resp } => resp);
        assert_eq!(conf, fidl_mlme::DisassociateConfirm { status: 1 });
    }

    #[test_case(banjo_wlan_common::WlanMacRole::CLIENT; "client")]
    #[test_case(banjo_wlan_common::WlanMacRole::AP; "ap")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_disassoc_ind(mac_role: banjo_wlan_common::WlanMacRole) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        h.fake_device.query_device_info_mock.role = mac_role;
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let disassoc_ind = banjo_wlan_fullmac::WlanFullmacDisassocIndication {
            peer_sta_address: [1u8; 6],
            reason_code: banjo_wlan_ieee80211::ReasonCode::LEAVING_NETWORK_DEAUTH,
            locally_initiated: true,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).disassoc_ind)(
                &mut h.driver_event_protocol.ctx,
                &disassoc_ind,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        if mac_role == banjo_wlan_common::WlanMacRole::CLIENT {
            assert_variant!(
                driver_calls.next(),
                Some(DriverCall::OnLinkStateChanged { online: false })
            );
        } else {
            assert_variant!(driver_calls.next(), None);
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

    #[test_case(banjo_wlan_fullmac::WlanStartResult::SUCCESS, true, fidl_mlme::StartResultCode::Success; "success start result")]
    #[test_case(banjo_wlan_fullmac::WlanStartResult::BSS_ALREADY_STARTED_OR_JOINED, false, fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined; "other start result")]
    #[fuchsia::test(add_test_attr = false)]
    fn test_start_conf(
        start_result: banjo_wlan_fullmac::WlanStartResult,
        expected_link_state_changed: bool,
        expected_fidl_result_code: fidl_mlme::StartResultCode,
    ) {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let start_conf = banjo_wlan_fullmac::WlanFullmacStartConfirm { result_code: start_result };
        unsafe {
            ((*h.driver_event_protocol.ops).start_conf)(
                &mut h.driver_event_protocol.ctx,
                &start_conf,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let mut driver_calls = h.fake_device.captured_driver_calls.iter();
        if expected_link_state_changed {
            assert_variant!(
                driver_calls.next(),
                Some(DriverCall::OnLinkStateChanged { online: true })
            );
        } else {
            assert_variant!(driver_calls.next(), None);
        }

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let conf = assert_variant!(event, fidl_mlme::MlmeEvent::StartConf { resp } => resp);
        assert_eq!(conf, fidl_mlme::StartConfirm { result_code: expected_fidl_result_code });
    }

    #[test]
    fn test_stop_conf() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let stop_conf = banjo_wlan_fullmac::WlanFullmacStopConfirm {
            result_code: banjo_wlan_fullmac::WlanStopResult::SUCCESS,
        };
        unsafe {
            ((*h.driver_event_protocol.ops).stop_conf)(
                &mut h.driver_event_protocol.ctx,
                &stop_conf,
            );
        }
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

        let eapol_conf = banjo_wlan_fullmac::WlanFullmacEapolConfirm {
            result_code: banjo_wlan_fullmac::WlanEapolResult::SUCCESS,
            dst_addr: [1u8; 6],
        };
        unsafe {
            ((*h.driver_event_protocol.ops).eapol_conf)(
                &mut h.driver_event_protocol.ctx,
                &eapol_conf,
            );
        }
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

        let channel_switch_info =
            banjo_wlan_fullmac::WlanFullmacChannelSwitchInfo { new_channel: 9 };
        unsafe {
            ((*h.driver_event_protocol.ops).on_channel_switch)(
                &mut h.driver_event_protocol.ctx,
                &channel_switch_info,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let info = assert_variant!(event, fidl_mlme::MlmeEvent::OnChannelSwitched { info } => info);
        assert_eq!(info, fidl_internal::ChannelSwitchInfo { new_channel: 9 });
    }

    #[test]
    fn test_signal_report() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let signal_report_ind =
            banjo_wlan_fullmac::WlanFullmacSignalReportIndication { rssi_dbm: 1, snr_db: 2 };
        unsafe {
            ((*h.driver_event_protocol.ops).signal_report)(
                &mut h.driver_event_protocol.ctx,
                &signal_report_ind,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::SignalReport { ind } => ind);
        assert_eq!(ind, fidl_internal::SignalReportIndication { rssi_dbm: 1, snr_db: 2 });
    }

    #[test]
    fn test_eapol_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let data = vec![3u8; 4];
        let eapol_ind = banjo_wlan_fullmac::WlanFullmacEapolIndication {
            src_addr: [1u8; 6],
            dst_addr: [2u8; 6],
            data_list: data.as_ptr(),
            data_count: data.len(),
        };
        unsafe {
            ((*h.driver_event_protocol.ops).eapol_ind)(
                &mut h.driver_event_protocol.ctx,
                &eapol_ind,
            );
        }
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

        let pmk = vec![1u8; 2];
        let pmkid = vec![3u8; 4];
        let pmk_info = banjo_wlan_fullmac::WlanFullmacPmkInfo {
            pmk_list: pmk.as_ptr(),
            pmk_count: pmk.len(),
            pmkid_list: pmkid.as_ptr(),
            pmkid_count: pmkid.len(),
        };
        unsafe {
            ((*h.driver_event_protocol.ops).on_pmk_available)(
                &mut h.driver_event_protocol.ctx,
                &pmk_info,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let info = assert_variant!(event, fidl_mlme::MlmeEvent::OnPmkAvailable { info } => info);
        assert_eq!(info, fidl_mlme::PmkInfo { pmk: vec![1u8; 2], pmkid: vec![3u8; 4] });
    }

    #[test]
    fn test_sae_handshake_ind() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let sae_handshake_ind =
            banjo_wlan_fullmac::WlanFullmacSaeHandshakeInd { peer_sta_address: [1u8; 6] };
        unsafe {
            ((*h.driver_event_protocol.ops).sae_handshake_ind)(
                &mut h.driver_event_protocol.ctx,
                &sae_handshake_ind,
            );
        }
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let event = assert_variant!(h.mlme_event_receiver.try_next(), Ok(Some(ev)) => ev);
        let ind = assert_variant!(event, fidl_mlme::MlmeEvent::OnSaeHandshakeInd { ind } => ind);
        assert_eq!(ind, fidl_mlme::SaeHandshakeIndication { peer_sta_address: [1u8; 6] });
    }

    #[test]
    fn test_sae_frame_rx() {
        let (mut h, mut test_fut) = TestHelper::set_up();
        assert_variant!(h.exec.run_until_stalled(&mut test_fut), Poll::Pending);

        let sae_fields = vec![3u8; 4];
        let sae_frame = banjo_wlan_fullmac::WlanFullmacSaeFrame {
            peer_sta_address: [1u8; 6],
            status_code: banjo_wlan_ieee80211::StatusCode::SUCCESS,
            seq_num: 2,
            sae_fields_list: sae_fields.as_ptr(),
            sae_fields_count: sae_fields.len(),
        };
        unsafe {
            ((*h.driver_event_protocol.ops).sae_frame_rx)(
                &mut h.driver_event_protocol.ctx,
                &sae_frame,
            );
        }
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
        let wmm_params = banjo_wlan_common::WlanWmmParameters {
            apsd: true,
            ac_be_params: banjo_wlan_common::WlanWmmAccessCategoryParameters {
                ecw_min: 1,
                ecw_max: 2,
                aifsn: 3,
                txop_limit: 4,
                acm: true,
            },
            ac_bk_params: banjo_wlan_common::WlanWmmAccessCategoryParameters {
                ecw_min: 5,
                ecw_max: 6,
                aifsn: 7,
                txop_limit: 8,
                acm: false,
            },
            ac_vi_params: banjo_wlan_common::WlanWmmAccessCategoryParameters {
                ecw_min: 9,
                ecw_max: 10,
                aifsn: 11,
                txop_limit: 12,
                acm: true,
            },
            ac_vo_params: banjo_wlan_common::WlanWmmAccessCategoryParameters {
                ecw_min: 13,
                ecw_max: 14,
                aifsn: 15,
                txop_limit: 16,
                acm: false,
            },
        };
        unsafe {
            ((*h.driver_event_protocol.ops).on_wmm_status_resp)(
                &mut h.driver_event_protocol.ctx,
                status,
                &wmm_params,
            );
        }
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
        fake_device: Pin<Box<FakeFullmacDevice>>,
        mlme_request_sender: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        driver_event_protocol: WlanFullmacIfcProtocol,
        mlme_event_receiver: mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>,
        exec: fasync::TestExecutor,
    }

    impl TestHelper {
        pub fn set_up() -> (Self, Pin<Box<impl Future<Output = Result<(), anyhow::Error>>>>) {
            let exec = fasync::TestExecutor::new();

            let mut fake_device = Box::pin(FakeFullmacDevice::new());
            let (mlme_request_sender, mlme_request_stream) = mpsc::unbounded();
            let (mlme_event_sender, mlme_event_receiver) = mpsc::unbounded();
            let mlme_event_sink = UnboundedSink::new(mlme_event_sender);

            let (driver_event_sender, driver_event_stream) = mpsc::unbounded();
            let driver_event_sink =
                Box::new(FullmacDriverEventSink(UnboundedSink::new(driver_event_sender)));
            let ifc = device::WlanFullmacIfcProtocol::new(driver_event_sink);

            let mlme = FullmacMlme {
                device: fake_device.as_mut().as_device(),
                mlme_request_stream,
                mlme_event_sink,
                driver_event_stream,
                is_bss_protected: false,
            };
            let test_fut = Box::pin(mlme.run_main_loop());
            let test_helper = TestHelper {
                fake_device,
                mlme_request_sender,
                driver_event_protocol: ifc,
                mlme_event_receiver,
                exec,
            };
            (test_helper, test_fut)
        }
    }
}
