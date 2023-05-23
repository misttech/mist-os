// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod context;
mod frame_writer;
mod infra_bss;
mod remote_client;

use {
    crate::{
        buffer::{BufferProvider, InBuf},
        device::DeviceOps,
        error::Error,
        logger,
    },
    banjo_fuchsia_wlan_softmac as banjo_wlan_softmac, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_minstrel as fidl_minstrel, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fuchsia_zircon as zx,
    ieee80211::{Bssid, MacAddr, Ssid},
    std::fmt,
    tracing::{debug, error, info, trace, warn},
    wlan_common::{
        mac::{self, CapabilityInfo},
        timer::{EventId, Timer},
        TimeUnit,
    },
    wlan_sme::{self},
    zerocopy::ByteSlice,
};

use context::*;
use infra_bss::*;
use remote_client::*;

#[derive(Debug)]
struct BufferedFrame {
    in_buf: InBuf,
    bytes_written: usize,
    tx_flags: u32,
}

/// Rejection reasons for why a frame was not proceessed.
#[derive(Debug)]
pub enum Rejection {
    /// The frame was for another BSS.
    OtherBss,

    /// For data frames: The To DS bit was false, or the From DS bit was true.
    /// For management frames: The To DS bit was set and the frame was not a QMF (QoS Management
    /// frame) management frame, or the reserved From DS bit was set.
    BadDsBits,

    /// For ethernet frames

    /// Frame is malformed (For example, a minimum Ethernet frame must contain a header(14 bytes).
    FrameMalformed,

    /// No source address was found.
    NoSrcAddr,

    /// No client with the given address was found.
    NoSuchClient(MacAddr),

    /// Some error specific to a client occurred.
    Client(MacAddr, ClientRejection),

    /// Some general error occurred.
    Error(anyhow::Error),
}

impl Rejection {
    fn log_level(&self) -> tracing::Level {
        match self {
            Self::NoSrcAddr | Self::FrameMalformed => tracing::Level::ERROR,
            Self::Client(_, e) => e.log_level(),
            _ => tracing::Level::TRACE,
        }
    }
    fn log(&self, msg: &str) {
        match self.log_level() {
            tracing::Level::TRACE => trace!("{}: {}", msg, self),
            tracing::Level::DEBUG => debug!("{}: {}", msg, self),
            tracing::Level::INFO => info!("{}: {}", msg, self),
            tracing::Level::WARN => warn!("{}: {}", msg, self),
            tracing::Level::ERROR => error!("{}: {}", msg, self),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(addr, e) => write!(f, "client {:02X?}: {:?}", addr, e),
            _ => fmt::Debug::fmt(self, f),
        }
    }
}

impl From<anyhow::Error> for Rejection {
    fn from(e: anyhow::Error) -> Rejection {
        Self::Error(e)
    }
}

#[derive(Debug)]
pub enum TimedEvent {
    /// Events that are destined for a client to handle.
    ClientEvent(MacAddr, ClientEvent),
}

pub struct Ap<D> {
    ctx: Context<D>,
    bss: Option<InfraBss>,
}

/// This trait adds an ok_or_bss_err for Option<&Bss> and Option<&mut Bss>, which returns an error
/// with ZX_ERR_BAD_STATE if the Option is uninhabited.
trait BssOptionExt<T: std::borrow::Borrow<InfraBss>> {
    fn ok_or_bss_err(self) -> Result<T, Error>;
}

impl<T: std::borrow::Borrow<InfraBss>> BssOptionExt<T> for Option<T> {
    fn ok_or_bss_err(self) -> Result<T, Error> {
        self.ok_or(Error::Status(format!("BSS not started"), zx::Status::BAD_STATE))
    }
}

impl<D: DeviceOps> crate::MlmeImpl for Ap<D> {
    type Config = Bssid;
    type Device = D;
    type TimerEvent = TimedEvent;

    fn new(
        config: Self::Config,
        device: D,
        buf_provider: BufferProvider,
        timer: Timer<TimedEvent>,
    ) -> Self {
        Self::new(device, buf_provider, timer, config)
    }
    fn handle_mlme_request(&mut self, req: wlan_sme::MlmeRequest) -> Result<(), anyhow::Error> {
        Self::handle_mlme_req(self, req).map_err(|e| e.into())
    }
    fn handle_mac_frame_rx(
        &mut self,
        frame: &[u8],
        rx_info: banjo_fuchsia_wlan_softmac::WlanRxInfo,
    ) {
        Self::handle_mac_frame_rx(self, frame, rx_info)
    }
    fn handle_eth_frame_tx(&mut self, bytes: &[u8]) -> Result<(), anyhow::Error> {
        Self::handle_eth_frame_tx(self, bytes);
        Ok(())
    }
    fn handle_scan_complete(&mut self, _status: zx::Status, _scan_id: u64) {
        warn!("Unexpected ScanComplete for AP MLME.");
        return;
    }
    fn handle_timeout(&mut self, event_id: EventId, event: TimedEvent) {
        Self::handle_timed_event(self, event_id, event)
    }
    fn access_device(&mut self) -> &mut Self::Device {
        &mut self.ctx.device
    }
}

impl<D> Ap<D> {
    pub fn new(
        device: D,
        buf_provider: BufferProvider,
        timer: Timer<TimedEvent>,
        bssid: Bssid,
    ) -> Self {
        logger::init();
        Self { ctx: Context::new(device, buf_provider, timer, bssid), bss: None }
    }

    fn handle_sme_list_minstrel_peers(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::MinstrelListResponse>,
    ) -> Result<(), Error> {
        // TODO(fxbug.dev/79543): Implement once Minstrel is in Rust.
        error!("ListMinstrelPeers is not supported.");
        let peers = fidl_minstrel::Peers { addrs: vec![] };
        let resp = fidl_mlme::MinstrelListResponse { peers };
        responder.respond(resp);
        Ok(())
    }

    fn handle_sme_get_minstrel_stats(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::MinstrelStatsResponse>,
        _addr: &[u8; 6],
    ) -> Result<(), Error> {
        // TODO(fxbug.dev/79543): Implement once Minstrel is in Rust.
        error!("GetMinstrelStats is not supported.");
        let resp = fidl_mlme::MinstrelStatsResponse { peer: None };
        responder.respond(resp);
        Ok(())
    }
}

impl<D: DeviceOps> Ap<D> {
    // Timer handler functions.
    pub fn handle_timed_event(&mut self, event_id: EventId, event: TimedEvent) {
        let bss = match self.bss.as_mut() {
            Some(bss) => bss,
            None => {
                error!("received timed event but BSS was not started yet");
                return;
            }
        };

        if let Err(e) = bss.handle_timed_event(&mut self.ctx, event_id, event) {
            error!("failed to handle timed event frame: {}", e)
        }
    }

    // MLME handler functions.

    /// Handles MLME-START.request (IEEE Std 802.11-2016, 6.3.11.2) from the SME.
    fn handle_mlme_start_req(&mut self, req: fidl_mlme::StartRequest) -> Result<(), Error> {
        if self.bss.is_some() {
            info!("MLME-START.request: BSS already started");
            self.ctx.send_mlme_start_conf(fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined)?;
            return Ok(());
        }

        if req.bss_type != fidl_common::BssType::Infrastructure {
            info!("MLME-START.request: BSS type {:?} not supported", req.bss_type);
            self.ctx.send_mlme_start_conf(fidl_mlme::StartResultCode::NotSupported)?;
            return Ok(());
        }

        self.bss.replace(InfraBss::new(
            &mut self.ctx,
            Ssid::from_bytes_unchecked(req.ssid),
            TimeUnit(req.beacon_period),
            req.dtim_period,
            CapabilityInfo(req.capability_info),
            req.rates,
            req.channel,
            req.rsne,
        )?);

        self.ctx.send_mlme_start_conf(fidl_mlme::StartResultCode::Success)?;

        info!("MLME-START.request: OK");
        Ok(())
    }

    /// Handles MLME-STOP.request (IEEE Std 802.11-2016, 6.3.12.2) from the SME.
    fn handle_mlme_stop_req(&mut self, _req: fidl_mlme::StopRequest) -> Result<(), Error> {
        match self.bss.take() {
            Some(bss) => match bss.stop(&mut self.ctx) {
                Ok(_) => self.ctx.send_mlme_stop_conf(fidl_mlme::StopResultCode::Success)?,
                Err(e) => {
                    self.ctx.send_mlme_stop_conf(fidl_mlme::StopResultCode::InternalError)?;
                    return Err(e);
                }
            },
            None => {
                info!("MLME-STOP.request: BSS not started");
                self.ctx.send_mlme_stop_conf(fidl_mlme::StopResultCode::BssAlreadyStopped)?;
            }
        }
        info!("MLME-STOP.request: OK");
        Ok(())
    }

    /// Handles MLME-SETKEYS.request (IEEE Std 802.11-2016, 6.3.19.1) from the SME.
    ///
    /// The MLME should set the keys on the PHY.
    pub fn handle_mlme_setkeys_req(&mut self, req: fidl_mlme::SetKeysRequest) -> Result<(), Error> {
        if let Some(bss) = self.bss.as_mut() {
            bss.handle_mlme_setkeys_req(&mut self.ctx, &req.keylist[..])
        } else {
            Err(Error::Status(format!("cannot set keys on unstarted BSS"), zx::Status::BAD_STATE))
        }
    }

    pub fn handle_mlme_query_device_info(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_mlme::DeviceInfo>,
    ) -> Result<(), Error> {
        let info = self.ctx.device.wlan_softmac_query_response().try_into()?;
        responder.respond(info);
        Ok(())
    }

    pub fn handle_mlme_query_discovery_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::DiscoverySupport>,
    ) -> Result<(), Error> {
        let ddk_support = self.ctx.device.discovery_support();
        let support = crate::ddk_converter::convert_ddk_discovery_support(ddk_support)?;
        responder.respond(support);
        Ok(())
    }

    pub fn handle_mlme_query_mac_sublayer_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::MacSublayerSupport>,
    ) -> Result<(), Error> {
        let ddk_support = self.ctx.device.mac_sublayer_support();
        let support = crate::ddk_converter::convert_ddk_mac_sublayer_support(ddk_support)?;
        responder.respond(support);
        Ok(())
    }

    pub fn handle_mlme_query_security_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::SecuritySupport>,
    ) -> Result<(), Error> {
        let ddk_support = self.ctx.device.security_support();
        let support = crate::ddk_converter::convert_ddk_security_support(ddk_support)?;
        responder.respond(support);
        Ok(())
    }

    pub fn handle_mlme_query_spectrum_management_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::SpectrumManagementSupport>,
    ) -> Result<(), Error> {
        let ddk_support = self.ctx.device.spectrum_management_support();
        let support = crate::ddk_converter::convert_ddk_spectrum_management_support(ddk_support)?;
        responder.respond(support);
        Ok(())
    }

    pub fn handle_mlme_req(&mut self, req: wlan_sme::MlmeRequest) -> Result<(), Error> {
        use wlan_sme::MlmeRequest as Req;
        match req {
            Req::Start(req) => self.handle_mlme_start_req(req),
            Req::Stop(req) => self.handle_mlme_stop_req(req),
            Req::SetKeys(req) => self.handle_mlme_setkeys_req(req),
            Req::QueryDeviceInfo(responder) => self.handle_mlme_query_device_info(responder),
            Req::QueryDiscoverySupport(responder) => {
                self.handle_mlme_query_discovery_support(responder)
            }
            Req::QueryMacSublayerSupport(responder) => {
                self.handle_mlme_query_mac_sublayer_support(responder)
            }
            Req::QuerySecuritySupport(responder) => {
                self.handle_mlme_query_security_support(responder)
            }
            Req::QuerySpectrumManagementSupport(responder) => {
                self.handle_mlme_query_spectrum_management_support(responder)
            }
            Req::ListMinstrelPeers(responder) => self.handle_sme_list_minstrel_peers(responder),
            Req::GetMinstrelStats(req, responder) => {
                self.handle_sme_get_minstrel_stats(responder, &req.peer_addr)
            }
            Req::AuthResponse(resp) => {
                // TODO(fxbug.dev/91118) - Added to help investigate hw-sim test. Remove later
                info!("Handling MLME auth resp. self.bss.is_some()?: {}", self.bss.is_some());
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_auth_resp(&mut self.ctx, resp)
            }
            Req::Deauthenticate(req) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_deauth_req(&mut self.ctx, req)
            }
            Req::AssocResponse(resp) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_assoc_resp(&mut self.ctx, resp)
            }
            Req::Disassociate(req) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_disassoc_req(&mut self.ctx, req)
            }
            Req::SetCtrlPort(req) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_set_controlled_port_req(req)
            }
            Req::Eapol(req) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_eapol_req(&mut self.ctx, req)
            }
            _ => Err(Error::Status(format!("not supported"), zx::Status::NOT_SUPPORTED)),
        }
        .map_err(|e| {
            error!("error handling MLME message: {}", e);
            e
        })
    }

    pub fn handle_eth_frame_tx(&mut self, frame: &[u8]) {
        let bss = match self.bss.as_mut() {
            Some(bss) => bss,
            None => {
                error!("received Ethernet frame but BSS was not started yet");
                return;
            }
        };

        let mac::EthernetFrame { hdr, body } =
            match mac::EthernetFrame::parse(frame).ok_or_else(|| Rejection::FrameMalformed) {
                Ok(eth_frame) => eth_frame,
                Err(e) => {
                    error!("failed to parse Ethernet frame: {}", e);
                    return;
                }
            };

        if let Err(e) = bss.handle_eth_frame(&mut self.ctx, *hdr, body) {
            e.log("failed to handle Ethernet frame")
        }
    }

    pub fn handle_mac_frame_rx<B: ByteSlice>(
        &mut self,
        bytes: B,
        rx_info: banjo_wlan_softmac::WlanRxInfo,
    ) {
        let bss = match self.bss.as_mut() {
            Some(bss) => bss,
            None => {
                error!("received WLAN frame but BSS was not started yet");
                return;
            }
        };

        // Rogue frames received from the wrong channel
        if rx_info.channel.primary != bss.channel {
            return;
        }

        let body_aligned = (rx_info.rx_flags
            & banjo_wlan_softmac::WlanRxInfoFlags::FRAME_BODY_PADDING_4)
            != banjo_wlan_softmac::WlanRxInfoFlags(0);

        let mac_frame = match mac::MacFrame::parse(bytes, body_aligned) {
            Some(mac_frame) => mac_frame,
            None => {
                error!("failed to parse MAC frame");
                return;
            }
        };

        if let Err(e) = match mac_frame {
            mac::MacFrame::Mgmt { mgmt_hdr, body, .. } => {
                bss.handle_mgmt_frame(&mut self.ctx, *mgmt_hdr, body)
            }
            mac::MacFrame::Data { fixed_fields, addr4, qos_ctrl, body, .. } => bss
                .handle_data_frame(
                    &mut self.ctx,
                    *fixed_fields,
                    addr4.map(|a| *a),
                    qos_ctrl.map(|x| x.get()),
                    body,
                ),
            mac::MacFrame::Ctrl { frame_ctrl, body } => {
                bss.handle_ctrl_frame(&mut self.ctx, frame_ctrl, body)
            }
            mac::MacFrame::Unsupported { frame_ctrl } => {
                error!("received unsupported MAC frame: frame_ctrl = {:?}", frame_ctrl);
                return;
            }
        } {
            e.log("failed to handle MAC frame")
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            buffer::FakeBufferProvider,
            device::{
                test_utils::fake_fidl_band_caps, FakeDevice, FakeDeviceConfig, FakeDeviceState,
            },
            test_utils::MockWlanRxInfo,
        },
        banjo_fuchsia_wlan_common as banjo_common, fidl_fuchsia_wlan_common as fidl_common,
        fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fuchsia_async as fasync,
        futures::task::Poll,
        std::{
            convert::TryFrom,
            sync::{Arc, Mutex},
        },
        wlan_common::{
            assert_variant, big_endian::BigEndianU16, test_utils::fake_frames::fake_wpa2_rsne,
            timer,
        },
        wlan_frame_writer::write_frame_with_dynamic_buf,
        wlan_sme::responder::Responder,
    };
    const CLIENT_ADDR: MacAddr = [4u8; 6];
    const BSSID: Bssid = Bssid([2u8; 6]);
    const CLIENT_ADDR2: MacAddr = [6u8; 6];

    fn make_eth_frame(
        dst_addr: MacAddr,
        src_addr: MacAddr,
        protocol_id: u16,
        body: &[u8],
    ) -> Vec<u8> {
        let (mut buf, bytes_written) = write_frame_with_dynamic_buf!(vec![], {
            headers: {
                mac::EthernetIIHdr: &mac::EthernetIIHdr {
                    da: dst_addr,
                    sa: src_addr,
                    ether_type: BigEndianU16::from_native(protocol_id),
                },
            },
            payload: body,
        })
        .expect("writing to vec always succeeds");
        buf.truncate(bytes_written);
        buf
    }

    fn make_ap(
        exec: &fasync::TestExecutor,
    ) -> (Ap<FakeDevice>, Arc<Mutex<FakeDeviceState>>, timer::TimeStream<TimedEvent>) {
        let (timer, time_stream) = timer::create_timer();
        let (fake_device, fake_device_state) = FakeDevice::new_with_config(
            &exec,
            FakeDeviceConfig {
                mac_role: banjo_common::WlanMacRole::AP,
                sta_addr: BSSID.0,
                ..Default::default()
            },
        );
        (
            Ap::new(fake_device, FakeBufferProvider::new(), timer, BSSID),
            fake_device_state,
            time_stream,
        )
    }

    #[test]
    fn ap_handle_eth_frame() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        let client = ap.bss.as_mut().unwrap().clients.get_mut(&CLIENT_ADDR).unwrap();
        client
            .handle_mlme_auth_resp(&mut ap.ctx, fidl_mlme::AuthenticateResultCode::Success)
            .expect("expected OK");
        client
            .handle_mlme_assoc_resp(
                &mut ap.ctx,
                false,
                1,
                mac::CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::Success,
                1,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10][..],
            )
            .expect("expected OK");
        fake_device_state.lock().unwrap().wlan_queue.clear();

        ap.handle_eth_frame_tx(&make_eth_frame(
            CLIENT_ADDR,
            CLIENT_ADDR2,
            0x1234,
            &[1, 2, 3, 4, 5][..],
        ));

        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b00001000, 0b00000010, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                6, 6, 6, 6, 6, 6, // addr3
                0x30, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x12, 0x34, // Protocol ID
                // Data
                1, 2, 3, 4, 5,
            ][..]
        );
    }

    #[test]
    fn ap_handle_eth_frame_no_such_client() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.handle_eth_frame_tx(&make_eth_frame(
            CLIENT_ADDR2,
            CLIENT_ADDR,
            0x1234,
            &[1, 2, 3, 4, 5][..],
        ));
    }

    fn mock_rx_info(ap: &Ap<FakeDevice>) -> banjo_wlan_softmac::WlanRxInfo {
        let channel = banjo_common::WlanChannel {
            primary: ap.bss.as_ref().unwrap().channel,
            cbw: banjo_common::ChannelBandwidth::CBW20,
            secondary80: 0,
        };
        MockWlanRxInfo::with_channel(channel).into()
    }

    #[test]
    fn ap_handle_mac_frame() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.handle_mac_frame_rx(
            &[
                // Mgmt header
                0b10110000, 0b00000000, // Frame Control
                0, 0, // Duration
                2, 2, 2, 2, 2, 2, // addr1
                4, 4, 4, 4, 4, 4, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Auth body
                0, 0, // Auth Algorithm Number
                1, 0, // Auth Txn Seq Number
                0, 0, // Status code
            ][..],
            mock_rx_info(&ap),
        );

        assert_eq!(ap.bss.as_mut().unwrap().clients.contains_key(&CLIENT_ADDR), true);

        let msg = fake_device_state
            .lock()
            .unwrap()
            .next_mlme_msg::<fidl_mlme::AuthenticateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::AuthenticateIndication {
                peer_sta_address: CLIENT_ADDR,
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            },
        );
    }

    #[test]
    fn ap_handle_mac_frame_ps_poll() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        let client = ap.bss.as_mut().unwrap().clients.get_mut(&CLIENT_ADDR).unwrap();
        client
            .handle_mlme_auth_resp(&mut ap.ctx, fidl_mlme::AuthenticateResultCode::Success)
            .expect("expected OK");
        client
            .handle_mlme_assoc_resp(
                &mut ap.ctx,
                false,
                1,
                mac::CapabilityInfo(0),
                fidl_mlme::AssociateResultCode::Success,
                1,
                &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10][..],
            )
            .expect("expected OK");
        fake_device_state.lock().unwrap().wlan_queue.clear();

        // Put the client into dozing.
        ap.handle_mac_frame_rx(
            &[
                0b01001000, 0b00010001, // Frame control.
                0, 0, // Duration.
                2, 2, 2, 2, 2, 2, // BSSID.
                4, 4, 4, 4, 4, 4, // MAC address.
                2, 2, 2, 2, 2, 2, // BSSID.
                0x10, 0, // Sequence control.
            ][..],
            mock_rx_info(&ap),
        );

        ap.handle_eth_frame_tx(&make_eth_frame(
            CLIENT_ADDR,
            CLIENT_ADDR2,
            0x1234,
            &[1, 2, 3, 4, 5][..],
        ));
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 0);

        // Send a PS-Poll.
        ap.handle_mac_frame_rx(
            &[
                // Ctrl header
                0b10100100, 0b00000000, // Frame Control
                0b00000001, 0b11000000, // Masked AID
                2, 2, 2, 2, 2, 2, // addr1
                4, 4, 4, 4, 4, 4, // addr2
            ][..],
            mock_rx_info(&ap),
        );

        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b00001000, 0b00000010, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                6, 6, 6, 6, 6, 6, // addr3
                0x30, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x12, 0x34, // Protocol ID
                // Data
                1, 2, 3, 4, 5,
            ][..]
        );
    }

    #[test]
    fn ap_handle_mac_frame_no_such_client() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.handle_mac_frame_rx(
            &[
                // Mgmt header
                0b10100000, 0b00000001, // Frame Control
                0, 0, // Duration
                2, 2, 2, 2, 2, 2, // addr1
                4, 4, 4, 4, 4, 4, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Disassoc header:
                8, 0, // reason code
            ][..],
            mock_rx_info(&ap),
        );

        assert_eq!(ap.bss.as_mut().unwrap().clients.contains_key(&CLIENT_ADDR), false);
    }

    #[test]
    fn ap_handle_mac_frame_bogus() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.handle_mac_frame_rx(
            &[0][..],
            banjo_wlan_softmac::WlanRxInfo {
                rx_flags: banjo_wlan_softmac::WlanRxInfoFlags(0),
                valid_fields: banjo_wlan_softmac::WlanRxInfoValid(0),
                phy: banjo_common::WlanPhyType::DSSS,
                data_rate: 0,
                channel: banjo_common::WlanChannel {
                    primary: 0,
                    cbw: banjo_common::ChannelBandwidth::CBW20,
                    secondary80: 0,
                },
                mcs: 0,
                rssi_dbm: 0,
                snr_dbh: 0,
            },
        );
    }

    #[test]
    fn ap_handle_mac_frame_wrong_channel_drop() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        let probe_req = [
            // Mgmt header
            0b01000000, 0b00000000, // Frame Control
            0, 0, // Duration
            2, 2, 2, 2, 2, 2, // addr1
            4, 4, 4, 4, 4, 4, // addr2
            2, 2, 2, 2, 2, 2, // addr3
            0x10, 0, // Sequence Control
            // SSID
            0, 7, 0x63, 0x6f, 0x6f, 0x6c, 0x6e, 0x65, 0x74, 0x0a,
        ];
        let rx_info_wrong_channel = banjo_wlan_softmac::WlanRxInfo {
            rx_flags: banjo_wlan_softmac::WlanRxInfoFlags(0),
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid(0),
            phy: banjo_common::WlanPhyType::DSSS,
            data_rate: 0,
            channel: banjo_common::WlanChannel {
                primary: 0,
                cbw: banjo_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            },
            mcs: 0,
            rssi_dbm: 0,
            snr_dbh: 0,
        };
        ap.handle_mac_frame_rx(&probe_req[..], rx_info_wrong_channel.clone());

        // Probe Request from the wrong channel should be dropped and no probe response sent.
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 0);

        // Frame from the same channel must be processed and a probe response sent.
        let rx_info_same_channel = banjo_wlan_softmac::WlanRxInfo {
            channel: banjo_common::WlanChannel {
                primary: 1,
                cbw: banjo_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            },
            ..rx_info_wrong_channel
        };
        fake_device_state.lock().unwrap().wlan_queue.clear();
        ap.handle_mac_frame_rx(&probe_req[..], rx_info_same_channel);
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
    }

    #[test]
    fn ap_handle_mlme_start_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.handle_mlme_start_req(fidl_mlme::StartRequest {
            ssid: Ssid::try_from("coolnet").unwrap().into(),
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 5,
            dtim_period: 1,
            channel: 2,
            capability_info: CapabilityInfo(0).raw(),
            rates: vec![0b11111000],
            country: fidl_mlme::Country { alpha2: *b"xx", suffix: fidl_mlme::COUNTRY_ENVIRON_ALL },
            mesh_id: vec![],
            rsne: None,
            phy: fidl_common::WlanPhyType::Erp,
            channel_bandwidth: fidl_common::ChannelBandwidth::Cbw20,
        })
        .expect("expected Ap::handle_mlme_start_request OK");

        assert!(ap.bss.is_some());
        assert_eq!(
            fake_device_state.lock().unwrap().wlan_channel,
            banjo_common::WlanChannel {
                primary: 2,
                // TODO(fxbug.dev/40917): Correctly support this.
                cbw: banjo_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            }
        );

        let msg = fake_device_state
            .lock()
            .unwrap()
            .next_mlme_msg::<fidl_mlme::StartConfirm>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::StartConfirm { result_code: fidl_mlme::StartResultCode::Success },
        );
    }

    #[test]
    fn ap_handle_mlme_start_req_already_started() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );

        ap.handle_mlme_start_req(fidl_mlme::StartRequest {
            ssid: Ssid::try_from("coolnet").unwrap().into(),
            bss_type: fidl_common::BssType::Infrastructure,
            beacon_period: 5,
            dtim_period: 1,
            channel: 2,
            capability_info: CapabilityInfo(0).raw(),
            rates: vec![],
            country: fidl_mlme::Country { alpha2: *b"xx", suffix: fidl_mlme::COUNTRY_ENVIRON_ALL },
            mesh_id: vec![],
            rsne: None,
            phy: fidl_common::WlanPhyType::Erp,
            channel_bandwidth: fidl_common::ChannelBandwidth::Cbw20,
        })
        .expect("expected Ap::handle_mlme_start_request OK");

        let msg = fake_device_state
            .lock()
            .unwrap()
            .next_mlme_msg::<fidl_mlme::StartConfirm>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::StartConfirm {
                result_code: fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined
            },
        );
    }

    #[test]
    fn ap_handle_mlme_stop_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );

        ap.handle_mlme_stop_req(fidl_mlme::StopRequest {
            ssid: Ssid::try_from("coolnet").unwrap().into(),
        })
        .expect("expected Ap::handle_mlme_stop_request OK");
        assert!(ap.bss.is_none());

        let msg = fake_device_state
            .lock()
            .unwrap()
            .next_mlme_msg::<fidl_mlme::StopConfirm>()
            .expect("expected MLME message");
        assert_eq!(msg, fidl_mlme::StopConfirm { result_code: fidl_mlme::StopResultCode::Success },);
    }

    #[test]
    fn ap_handle_mlme_stop_req_already_stopped() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);

        ap.handle_mlme_stop_req(fidl_mlme::StopRequest {
            ssid: Ssid::try_from("coolnet").unwrap().into(),
        })
        .expect("expected Ap::handle_mlme_stop_request OK");
        assert!(ap.bss.is_none());

        let msg = fake_device_state
            .lock()
            .unwrap()
            .next_mlme_msg::<fidl_mlme::StopConfirm>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::StopConfirm { result_code: fidl_mlme::StopResultCode::BssAlreadyStopped },
        );
    }

    #[test]
    fn ap_handle_mlme_setkeys_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                Some(fake_wpa2_rsne()),
            )
            .expect("expected InfraBss::new ok"),
        );

        ap.handle_mlme_setkeys_req(fidl_mlme::SetKeysRequest {
            keylist: vec![fidl_mlme::SetKeyDescriptor {
                cipher_suite_oui: [1, 2, 3],
                cipher_suite_type: 4,
                key_type: fidl_mlme::KeyType::Pairwise,
                address: [5; 6],
                key_id: 6,
                key: vec![1, 2, 3, 4, 5, 6, 7],
                rsc: 8,
            }],
        })
        .expect("expected Ap::handle_mlme_setkeys_req OK");
        assert_eq!(fake_device_state.lock().unwrap().keys.len(), 1);
        let key = &fake_device_state.lock().unwrap().keys[0];
        assert_eq!(key.protection, crate::key::Protection::RX_TX);
        assert_eq!(key.cipher_oui, [1, 2, 3]);
        assert_eq!(key.cipher_type, 4);
        assert_eq!(key.key_type, crate::key::KeyType::PAIRWISE);
        assert_eq!(key.peer_addr, [5; 6]);
        assert_eq!(key.key_idx, 6);
        assert_eq!(key.key[0..key.key_len as usize], [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(key.rsc, 8);
    }

    #[test]
    fn ap_handle_mlme_setkeys_req_no_bss() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        assert_variant!(
            ap.handle_mlme_setkeys_req(fidl_mlme::SetKeysRequest {
                keylist: vec![fidl_mlme::SetKeyDescriptor {
                    cipher_suite_oui: [1, 2, 3],
                    cipher_suite_type: 4,
                    key_type: fidl_mlme::KeyType::Pairwise,
                    address: [5; 6],
                    key_id: 6,
                    key: vec![1, 2, 3, 4, 5, 6, 7],
                    rsc: 8,
                }],
            })
            .expect_err("expected Ap::handle_mlme_setkeys_req error"),
            Error::Status(_, zx::Status::BAD_STATE)
        );
    }

    #[test]
    fn ap_handle_mlme_setkeys_req_bss_no_rsne() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );

        assert_variant!(
            ap.handle_mlme_setkeys_req(fidl_mlme::SetKeysRequest {
                keylist: vec![fidl_mlme::SetKeyDescriptor {
                    cipher_suite_oui: [1, 2, 3],
                    cipher_suite_type: 4,
                    key_type: fidl_mlme::KeyType::Pairwise,
                    address: [5; 6],
                    key_id: 6,
                    key: vec![1, 2, 3, 4, 5, 6, 7],
                    rsc: 8,
                }],
            })
            .expect_err("expected Ap::handle_mlme_setkeys_req error"),
            Error::Status(_, zx::Status::BAD_STATE)
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_auth_resp() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::AuthResponse(fidl_mlme::AuthenticateResponse {
            peer_sta_address: CLIENT_ADDR,
            result_code: fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
        }))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AuthenticateResp) ok");
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b10110000, 0, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Auth header:
                0, 0, // auth algorithm
                2, 0, // auth txn seq num
                76, 0, // status code
            ][..]
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_auth_resp_no_bss() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);

        assert_eq!(
            zx::Status::from(
                ap.handle_mlme_req(wlan_sme::MlmeRequest::AuthResponse(
                    fidl_mlme::AuthenticateResponse {
                        peer_sta_address: CLIENT_ADDR,
                        result_code: fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
                    }
                ))
                .expect_err(
                    "expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AuthenticateResp) error"
                )
            ),
            zx::Status::BAD_STATE
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_auth_resp_no_such_client() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );

        assert_eq!(
            zx::Status::from(
                ap.handle_mlme_req(wlan_sme::MlmeRequest::AuthResponse(
                    fidl_mlme::AuthenticateResponse {
                        peer_sta_address: CLIENT_ADDR,
                        result_code: fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
                    }
                ))
                .expect_err(
                    "expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AuthenticateResp) error"
                )
            ),
            zx::Status::NOT_FOUND
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_deauth_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::Deauthenticate(
            fidl_mlme::DeauthenticateRequest {
                peer_sta_address: CLIENT_ADDR,
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
            },
        ))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::DeauthenticateReq) ok");
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b11000000, 0, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Deauth header:
                3, 0, // reason code
            ][..]
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_assoc_resp() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::AssocResponse(fidl_mlme::AssociateResponse {
            peer_sta_address: CLIENT_ADDR,
            result_code: fidl_mlme::AssociateResultCode::Success,
            association_id: 1,
            capability_info: CapabilityInfo(0).raw(),
            rates: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        }))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AssociateResp) ok");
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b00010000, 0, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Association response header:
                0, 0, // Capabilities
                0, 0, // status code
                1, 0, // AID
                // IEs
                1, 8, 1, 2, 3, 4, 5, 6, 7, 8, // Rates
                50, 2, 9, 10, // Extended rates
                90, 3, 90, 0, 0, // BSS max idle period
            ][..]
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_disassoc_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::Disassociate(fidl_mlme::DisassociateRequest {
            peer_sta_address: CLIENT_ADDR,
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc,
        }))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::DisassociateReq) ok");
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Mgmt header
                0b10100000, 0, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                // Disassoc header:
                8, 0, // reason code
            ][..]
        );
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_set_controlled_port_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                Some(fake_wpa2_rsne()),
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::AssocResponse(fidl_mlme::AssociateResponse {
            peer_sta_address: CLIENT_ADDR,
            result_code: fidl_mlme::AssociateResultCode::Success,
            association_id: 1,
            capability_info: CapabilityInfo(0).raw(),
            rates: vec![1, 2, 3],
        }))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AssociateResp) ok");

        ap.handle_mlme_req(wlan_sme::MlmeRequest::SetCtrlPort(
            fidl_mlme::SetControlledPortRequest {
                peer_sta_address: CLIENT_ADDR,
                state: fidl_mlme::ControlledPortState::Open,
            },
        ))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::SetControlledPort) ok");
    }

    #[test]
    fn ap_handle_mlme_req_handle_mlme_eapol_req() {
        let exec = fasync::TestExecutor::new();
        let (mut ap, fake_device_state, _) = make_ap(&exec);
        ap.bss.replace(
            InfraBss::new(
                &mut ap.ctx,
                Ssid::try_from("coolnet").unwrap(),
                TimeUnit::DEFAULT_BEACON_INTERVAL,
                2,
                CapabilityInfo(0),
                vec![0b11111000],
                1,
                None,
            )
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(CLIENT_ADDR, RemoteClient::new(CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::Eapol(fidl_mlme::EapolRequest {
            dst_addr: CLIENT_ADDR,
            src_addr: BSSID.0,
            data: vec![1, 2, 3],
        }))
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::EapolReq) ok");
        assert_eq!(fake_device_state.lock().unwrap().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().unwrap().wlan_queue[0].0[..],
            &[
                // Header
                0b00001000, 0b00000010, // Frame Control
                0, 0, // Duration
                4, 4, 4, 4, 4, 4, // addr1
                2, 2, 2, 2, 2, 2, // addr2
                2, 2, 2, 2, 2, 2, // addr3
                0x10, 0, // Sequence Control
                0xAA, 0xAA, 0x03, // DSAP, SSAP, Control, OUI
                0, 0, 0, // OUI
                0x88, 0x8E, // EAPOL protocol ID
                // Data
                1, 2, 3,
            ][..]
        );
    }

    #[test]
    fn ap_mlme_respond_to_query_device_info() {
        let mut exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QueryDeviceInfo(responder)),
            Ok(())
        );
        let info = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(
            info,
            fidl_mlme::DeviceInfo {
                sta_addr: BSSID.0,
                role: fidl_common::WlanMacRole::Ap,
                bands: fake_fidl_band_caps(),
                softmac_hardware_capability: 0,
                qos_capable: false,
            }
        );
    }

    #[test]
    fn ap_mlme_respond_to_query_discovery_support() {
        let mut exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QueryDiscoverySupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.scan_offload.supported, true);
        assert_eq!(resp.probe_response_offload.supported, false);
    }

    #[test]
    fn ap_mlme_respond_to_query_mac_sublayer_support() {
        let mut exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QueryMacSublayerSupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.rate_selection_offload.supported, false);
        assert_eq!(resp.data_plane.data_plane_type, fidl_common::DataPlaneType::EthernetDevice);
        assert_eq!(resp.device.is_synthetic, true);
        assert_eq!(
            resp.device.mac_implementation_type,
            fidl_common::MacImplementationType::Softmac
        );
        assert_eq!(resp.device.tx_status_report_supported, true);
    }

    #[test]
    fn ap_mlme_respond_to_query_security_support() {
        let mut exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QuerySecuritySupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.mfp.supported, false);
        assert_eq!(resp.sae.driver_handler_supported, false);
        assert_eq!(resp.sae.sme_handler_supported, false);
    }

    #[test]
    fn ap_mlme_respond_to_query_spectrum_management_support() {
        let mut exec = fasync::TestExecutor::new();
        let (mut ap, _, _) = make_ap(&exec);

        let (responder, mut receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QuerySpectrumManagementSupport(responder)),
            Ok(())
        );
        let resp = assert_variant!(exec.run_until_stalled(&mut receiver), Poll::Ready(Ok(r)) => r);
        assert_eq!(resp.dfs.supported, true);
    }

    #[test]
    fn display_rejection() {
        assert_eq!(format!("{}", Rejection::BadDsBits), "BadDsBits");
    }
}
