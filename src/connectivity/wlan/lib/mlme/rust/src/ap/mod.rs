// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod context;
mod frame_writer;
mod infra_bss;
mod remote_client;

use crate::ddk_converter;
use crate::device::{self, DeviceOps};
use crate::error::Error;
use fdf::ArenaStaticBox;
use ieee80211::{Bssid, MacAddr, Ssid};
use log::{debug, error, info, trace, warn};
use std::fmt;
use wlan_common::mac::{self, CapabilityInfo};
use wlan_common::timer::{EventId, Timer};
use wlan_common::TimeUnit;
use zerocopy::SplitByteSlice;
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_minstrel as fidl_minstrel,
    fidl_fuchsia_wlan_mlme as fidl_mlme, fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_trace as trace, wlan_trace as wtrace,
};

use context::*;
use infra_bss::*;
use remote_client::*;

#[derive(Debug)]
struct BufferedFrame {
    buffer: ArenaStaticBox<[u8]>,
    tx_flags: fidl_softmac::WlanTxInfoFlags,
    async_id: trace::Id,
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
    fn log_level(&self) -> log::Level {
        match self {
            Self::NoSrcAddr | Self::FrameMalformed => log::Level::Error,
            Self::Client(_, e) => e.log_level(),
            _ => log::Level::Trace,
        }
    }
    fn log(&self, msg: &str) {
        match self.log_level() {
            log::Level::Trace => trace!("{}: {}", msg, self),
            log::Level::Debug => debug!("{}: {}", msg, self),
            log::Level::Info => info!("{}: {}", msg, self),
            log::Level::Warn => warn!("{}: {}", msg, self),
            log::Level::Error => error!("{}: {}", msg, self),
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

    async fn new(
        config: Self::Config,
        device: D,
        timer: Timer<TimedEvent>,
    ) -> Result<Self, anyhow::Error>
    where
        Self: Sized,
    {
        Ok(Self::new(device, timer, config))
    }
    async fn handle_mlme_request(
        &mut self,
        req: wlan_sme::MlmeRequest,
    ) -> Result<(), anyhow::Error> {
        Self::handle_mlme_req(self, req).await.map_err(|e| e.into())
    }
    async fn handle_mac_frame_rx(
        &mut self,
        frame: &[u8],
        rx_info: fidl_softmac::WlanRxInfo,
        async_id: trace::Id,
    ) {
        Self::handle_mac_frame_rx(self, frame, rx_info, async_id).await
    }
    fn handle_eth_frame_tx(
        &mut self,
        bytes: &[u8],
        async_id: trace::Id,
    ) -> Result<(), anyhow::Error> {
        Self::handle_eth_frame_tx(self, bytes, async_id);
        Ok(())
    }
    async fn handle_scan_complete(&mut self, _status: zx::Status, _scan_id: u64) {
        warn!("Unexpected ScanComplete for AP MLME.");
        return;
    }
    async fn handle_timeout(&mut self, event_id: EventId, event: TimedEvent) {
        Self::handle_timed_event(self, event_id, event).await
    }
    fn access_device(&mut self) -> &mut Self::Device {
        &mut self.ctx.device
    }
}

impl<D> Ap<D> {
    pub fn new(device: D, timer: Timer<TimedEvent>, bssid: Bssid) -> Self {
        Self { ctx: Context::new(device, timer, bssid), bss: None }
    }

    fn handle_sme_list_minstrel_peers(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::MinstrelListResponse>,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/42159791): Implement once Minstrel is in Rust.
        error!("ListMinstrelPeers is not supported.");
        let peers = fidl_minstrel::Peers { addrs: vec![] };
        let resp = fidl_mlme::MinstrelListResponse { peers };
        responder.respond(resp);
        Ok(())
    }

    fn handle_sme_get_minstrel_stats(
        &self,
        responder: wlan_sme::responder::Responder<fidl_mlme::MinstrelStatsResponse>,
        _addr: &MacAddr,
    ) -> Result<(), Error> {
        // TODO(https://fxbug.dev/42159791): Implement once Minstrel is in Rust.
        error!("GetMinstrelStats is not supported.");
        let resp = fidl_mlme::MinstrelStatsResponse { peer: None };
        responder.respond(resp);
        Ok(())
    }
}

impl<D: DeviceOps> Ap<D> {
    // Timer handler functions.
    pub async fn handle_timed_event(&mut self, event_id: EventId, event: TimedEvent) {
        let bss = match self.bss.as_mut() {
            Some(bss) => bss,
            None => {
                error!("received timed event but BSS was not started yet");
                return;
            }
        };

        if let Err(e) = bss.handle_timed_event(&mut self.ctx, event_id, event).await {
            error!("failed to handle timed event frame: {}", e)
        }
    }

    // MLME handler functions.

    /// Handles MLME-START.request (IEEE Std 802.11-2016, 6.3.11.2) from the SME.
    async fn handle_mlme_start_req(&mut self, req: fidl_mlme::StartRequest) -> Result<(), Error> {
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

        self.bss.replace(
            InfraBss::new(
                &mut self.ctx,
                Ssid::from_bytes_unchecked(req.ssid),
                TimeUnit(req.beacon_period),
                req.dtim_period,
                CapabilityInfo(req.capability_info),
                req.rates,
                req.channel,
                req.rsne,
            )
            .await?,
        );

        self.ctx.send_mlme_start_conf(fidl_mlme::StartResultCode::Success)?;

        info!("MLME-START.request: OK");
        Ok(())
    }

    /// Handles MLME-STOP.request (IEEE Std 802.11-2016, 6.3.12.2) from the SME.
    async fn handle_mlme_stop_req(&mut self, _req: fidl_mlme::StopRequest) -> Result<(), Error> {
        match self.bss.take() {
            Some(bss) => match bss.stop(&mut self.ctx).await {
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
    pub async fn handle_mlme_setkeys_req(
        &mut self,
        req: fidl_mlme::SetKeysRequest,
    ) -> Result<(), Error> {
        if let Some(bss) = self.bss.as_mut() {
            bss.handle_mlme_setkeys_req(&mut self.ctx, req.keylist).await
        } else {
            Err(Error::Status(format!("cannot set keys on unstarted BSS"), zx::Status::BAD_STATE))
        }
    }

    pub async fn handle_mlme_query_device_info(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_mlme::DeviceInfo>,
    ) -> Result<(), Error> {
        let info = ddk_converter::mlme_device_info_from_softmac(
            device::try_query(&mut self.ctx.device).await?,
        )?;
        responder.respond(info);
        Ok(())
    }

    pub async fn handle_mlme_query_discovery_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::DiscoverySupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_discovery_support(&mut self.ctx.device).await?;
        responder.respond(support);
        Ok(())
    }

    pub async fn handle_mlme_query_mac_sublayer_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::MacSublayerSupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_mac_sublayer_support(&mut self.ctx.device).await?;
        responder.respond(support);
        Ok(())
    }

    pub async fn handle_mlme_query_security_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::SecuritySupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_security_support(&mut self.ctx.device).await?;
        responder.respond(support);
        Ok(())
    }

    pub async fn handle_mlme_query_spectrum_management_support(
        &mut self,
        responder: wlan_sme::responder::Responder<fidl_common::SpectrumManagementSupport>,
    ) -> Result<(), Error> {
        let support = device::try_query_spectrum_management_support(&mut self.ctx.device).await?;
        responder.respond(support);
        Ok(())
    }

    pub async fn handle_mlme_req(&mut self, req: wlan_sme::MlmeRequest) -> Result<(), Error> {
        use wlan_sme::MlmeRequest as Req;
        match req {
            Req::Start(req) => self.handle_mlme_start_req(req).await,
            Req::Stop(req) => self.handle_mlme_stop_req(req).await,
            Req::SetKeys(req) => self.handle_mlme_setkeys_req(req).await,
            Req::QueryDeviceInfo(responder) => self.handle_mlme_query_device_info(responder).await,
            Req::QueryDiscoverySupport(responder) => {
                self.handle_mlme_query_discovery_support(responder).await
            }
            Req::QueryMacSublayerSupport(responder) => {
                self.handle_mlme_query_mac_sublayer_support(responder).await
            }
            Req::QuerySecuritySupport(responder) => {
                self.handle_mlme_query_security_support(responder).await
            }
            Req::QuerySpectrumManagementSupport(responder) => {
                self.handle_mlme_query_spectrum_management_support(responder).await
            }
            Req::ListMinstrelPeers(responder) => self.handle_sme_list_minstrel_peers(responder),
            Req::GetMinstrelStats(req, responder) => {
                self.handle_sme_get_minstrel_stats(responder, &req.peer_addr.into())
            }
            Req::AuthResponse(resp) => {
                // TODO(https://fxbug.dev/42172646) - Added to help investigate hw-sim test. Remove later
                info!("Handling MLME auth resp. self.bss.is_some()?: {}", self.bss.is_some());
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_auth_resp(&mut self.ctx, resp).await
            }
            Req::Deauthenticate(req) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_deauth_req(&mut self.ctx, req).await
            }
            Req::AssocResponse(resp) => {
                self.bss.as_mut().ok_or_bss_err()?.handle_mlme_assoc_resp(&mut self.ctx, resp).await
            }
            Req::Disassociate(req) => {
                self.bss
                    .as_mut()
                    .ok_or_bss_err()?
                    .handle_mlme_disassoc_req(&mut self.ctx, req)
                    .await
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

    pub fn handle_eth_frame_tx(&mut self, frame: &[u8], async_id: trace::Id) {
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

        if let Err(e) = bss.handle_eth_frame(&mut self.ctx, *hdr, body, async_id) {
            e.log("failed to handle Ethernet frame")
        }
    }

    pub async fn handle_mac_frame_rx<B: SplitByteSlice>(
        &mut self,
        bytes: B,
        rx_info: fidl_softmac::WlanRxInfo,
        async_id: trace::Id,
    ) {
        let bss = match self.bss.as_mut() {
            Some(bss) => bss,
            None => {
                error!("received WLAN frame but BSS was not started yet");
                wtrace::async_end_wlansoftmac_rx(async_id, "BSS not started");
                return;
            }
        };

        // Rogue frames received from the wrong channel
        if rx_info.channel.primary != bss.channel {
            wtrace::async_end_wlansoftmac_rx(async_id, "frame from wrong channel");
            return;
        }

        let body_aligned = (rx_info.rx_flags & fidl_softmac::WlanRxInfoFlags::FRAME_BODY_PADDING_4)
            != fidl_softmac::WlanRxInfoFlags::empty();

        let mac_frame = match mac::MacFrame::parse(bytes, body_aligned) {
            Some(mac_frame) => mac_frame,
            None => {
                error!("failed to parse MAC frame");
                wtrace::async_end_wlansoftmac_rx(async_id, "failed to parse frame");
                return;
            }
        };

        if let Err(e) = match mac_frame {
            mac::MacFrame::Mgmt(mgmt) => bss.handle_mgmt_frame(&mut self.ctx, mgmt).await,
            mac::MacFrame::Data(data_frame) => bss.handle_data_frame(&mut self.ctx, data_frame),
            mac::MacFrame::Ctrl(ctrl_frame) => bss.handle_ctrl_frame(&mut self.ctx, ctrl_frame),
            mac::MacFrame::Unsupported { frame_ctrl } => {
                error!("received unsupported MAC frame: frame_ctrl = {:?}", frame_ctrl);
                wtrace::async_end_wlansoftmac_rx(async_id, "received unsupported frame");
                return;
            }
        } {
            wtrace::async_end_wlansoftmac_rx(async_id, "failed to handle frame");
            e.log("failed to handle MAC frame")
        } else {
            wtrace::async_end_wlansoftmac_rx(async_id, "successfully handled frame");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{test_utils, FakeDevice, FakeDeviceConfig, FakeDeviceState, LinkStatus};
    use crate::test_utils::MockWlanRxInfo;
    use fuchsia_sync::Mutex;
    use ieee80211::MacAddrBytes;
    use lazy_static::lazy_static;
    use std::sync::Arc;
    use wlan_common::big_endian::BigEndianU16;
    use wlan_common::test_utils::fake_frames::fake_wpa2_rsne;
    use wlan_common::{assert_variant, timer};
    use wlan_frame_writer::write_frame_to_vec;
    use wlan_sme::responder::Responder;
    use {
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
        fidl_fuchsia_wlan_softmac as fidl_softmac,
    };

    lazy_static! {
        static ref CLIENT_ADDR: MacAddr = [4u8; 6].into();
        static ref BSSID: Bssid = [2u8; 6].into();
        static ref CLIENT_ADDR2: MacAddr = [6u8; 6].into();
    }

    fn make_eth_frame(
        dst_addr: MacAddr,
        src_addr: MacAddr,
        protocol_id: u16,
        body: &[u8],
    ) -> Vec<u8> {
        write_frame_to_vec!({
            headers: {
                mac::EthernetIIHdr: &mac::EthernetIIHdr {
                    da: dst_addr,
                    sa: src_addr,
                    ether_type: BigEndianU16::from_native(protocol_id),
                },
            },
            payload: body,
        })
        .unwrap() // This frame construction never fails
    }

    // TODO(https://fxbug.dev/327499461): This function is async to ensure MLME functions will
    // run in an async context and not call `wlan_common::timer::Timer::now` without an
    // executor.
    async fn make_ap(
    ) -> (Ap<FakeDevice>, Arc<Mutex<FakeDeviceState>>, timer::EventStream<TimedEvent>) {
        let (timer, time_stream) = timer::create_timer();
        let (fake_device, fake_device_state) = FakeDevice::new_with_config(
            FakeDeviceConfig::default()
                .with_mock_mac_role(fidl_common::WlanMacRole::Ap)
                .with_mock_sta_addr((*BSSID).to_array()),
        )
        .await;
        (Ap::new(fake_device, timer, *BSSID), fake_device_state, time_stream)
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_eth_frame() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        let client = ap.bss.as_mut().unwrap().clients.get_mut(&CLIENT_ADDR).unwrap();
        client
            .handle_mlme_auth_resp(&mut ap.ctx, fidl_mlme::AuthenticateResultCode::Success)
            .await
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
            .await
            .expect("expected OK");
        fake_device_state.lock().wlan_queue.clear();

        ap.handle_eth_frame_tx(
            &make_eth_frame(*CLIENT_ADDR, *CLIENT_ADDR2, 0x1234, &[1, 2, 3, 4, 5][..]),
            0.into(),
        );

        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_eth_frame_no_such_client() {
        let (mut ap, _, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.handle_eth_frame_tx(
            &make_eth_frame(*CLIENT_ADDR2, *CLIENT_ADDR, 0x1234, &[1, 2, 3, 4, 5][..]),
            0.into(),
        );
    }

    fn mock_rx_info(ap: &Ap<FakeDevice>) -> fidl_softmac::WlanRxInfo {
        let channel = fidl_common::WlanChannel {
            primary: ap.bss.as_ref().unwrap().channel,
            cbw: fidl_common::ChannelBandwidth::Cbw20,
            secondary80: 0,
        };
        MockWlanRxInfo::with_channel(channel).into()
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mac_frame() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
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
            0.into(),
        )
        .await;

        assert_eq!(ap.bss.as_mut().unwrap().clients.contains_key(&CLIENT_ADDR), true);

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::AuthenticateIndication>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::AuthenticateIndication {
                peer_sta_address: CLIENT_ADDR.to_array(),
                auth_type: fidl_mlme::AuthenticationTypes::OpenSystem,
            },
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mac_frame_ps_poll() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        let client = ap.bss.as_mut().unwrap().clients.get_mut(&CLIENT_ADDR).unwrap();
        client
            .handle_mlme_auth_resp(&mut ap.ctx, fidl_mlme::AuthenticateResultCode::Success)
            .await
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
            .await
            .expect("expected OK");
        fake_device_state.lock().wlan_queue.clear();

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
            0.into(),
        )
        .await;

        ap.handle_eth_frame_tx(
            &make_eth_frame(*CLIENT_ADDR, *CLIENT_ADDR2, 0x1234, &[1, 2, 3, 4, 5][..]),
            0.into(),
        );
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 0);

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
            0.into(),
        )
        .await;

        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mac_frame_no_such_client() {
        let (mut ap, _, _) = make_ap().await;
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
            .await
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
            0.into(),
        )
        .await;

        assert_eq!(ap.bss.as_mut().unwrap().clients.contains_key(&CLIENT_ADDR), false);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mac_frame_bogus() {
        let (mut ap, _, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.handle_mac_frame_rx(
            &[0][..],
            fidl_softmac::WlanRxInfo {
                rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
                valid_fields: fidl_softmac::WlanRxInfoValid::empty(),
                phy: fidl_common::WlanPhyType::Dsss,
                data_rate: 0,
                channel: fidl_common::WlanChannel {
                    primary: 0,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                mcs: 0,
                rssi_dbm: 0,
                snr_dbh: 0,
            },
            0.into(),
        )
        .await;
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mac_frame_wrong_channel_drop() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
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
        let rx_info_wrong_channel = fidl_softmac::WlanRxInfo {
            rx_flags: fidl_softmac::WlanRxInfoFlags::empty(),
            valid_fields: fidl_softmac::WlanRxInfoValid::empty(),
            phy: fidl_common::WlanPhyType::Dsss,
            data_rate: 0,
            channel: fidl_common::WlanChannel {
                primary: 0,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            mcs: 0,
            rssi_dbm: 0,
            snr_dbh: 0,
        };
        ap.handle_mac_frame_rx(&probe_req[..], rx_info_wrong_channel.clone(), 0.into()).await;

        // Probe Request from the wrong channel should be dropped and no probe response sent.
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 0);

        // Frame from the same channel must be processed and a probe response sent.
        let rx_info_same_channel = fidl_softmac::WlanRxInfo {
            channel: fidl_common::WlanChannel {
                primary: 1,
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            },
            ..rx_info_wrong_channel
        };
        fake_device_state.lock().wlan_queue.clear();
        ap.handle_mac_frame_rx(&probe_req[..], rx_info_same_channel, 0.into()).await;
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_start_req() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
        .await
        .expect("expected Ap::handle_mlme_start_request OK");

        assert!(ap.bss.is_some());
        assert_eq!(
            fake_device_state.lock().wlan_channel,
            fidl_common::WlanChannel {
                primary: 2,
                // TODO(https://fxbug.dev/42116942): Correctly support this.
                cbw: fidl_common::ChannelBandwidth::Cbw20,
                secondary80: 0,
            }
        );
        assert_eq!(fake_device_state.lock().link_status, LinkStatus::UP);

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::StartConfirm>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::StartConfirm { result_code: fidl_mlme::StartResultCode::Success },
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_start_req_already_started() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
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
        .await
        .expect("expected Ap::handle_mlme_start_request OK");

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::StartConfirm>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::StartConfirm {
                result_code: fidl_mlme::StartResultCode::BssAlreadyStartedOrJoined
            },
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_stop_req() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );

        ap.handle_mlme_stop_req(fidl_mlme::StopRequest {
            ssid: Ssid::try_from("coolnet").unwrap().into(),
        })
        .await
        .expect("expected Ap::handle_mlme_stop_request OK");
        assert!(ap.bss.is_none());
        assert_eq!(fake_device_state.lock().link_status, LinkStatus::DOWN);

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::StopConfirm>()
            .expect("expected MLME message");
        assert_eq!(msg, fidl_mlme::StopConfirm { result_code: fidl_mlme::StopResultCode::Success },);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_stop_req_already_stopped() {
        let (mut ap, fake_device_state, _) = make_ap().await;

        ap.handle_mlme_stop_req(fidl_mlme::StopRequest {
            ssid: Ssid::try_from("coolnet").unwrap().into(),
        })
        .await
        .expect("expected Ap::handle_mlme_stop_request OK");
        assert!(ap.bss.is_none());

        let msg = fake_device_state
            .lock()
            .next_mlme_msg::<fidl_mlme::StopConfirm>()
            .expect("expected MLME message");
        assert_eq!(
            msg,
            fidl_mlme::StopConfirm { result_code: fidl_mlme::StopResultCode::BssAlreadyStopped },
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_setkeys_req() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );

        ap.handle_mlme_setkeys_req(fidl_mlme::SetKeysRequest {
            keylist: vec![fidl_mlme::SetKeyDescriptor {
                cipher_suite_oui: [1, 2, 3],
                cipher_suite_type: fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(4),
                key_type: fidl_mlme::KeyType::Pairwise,
                address: [5; 6],
                key_id: 6,
                key: vec![1, 2, 3, 4, 5, 6, 7],
                rsc: 8,
            }],
        })
        .await
        .expect("expected Ap::handle_mlme_setkeys_req OK");
        assert_eq!(
            fake_device_state.lock().keys,
            vec![fidl_softmac::WlanKeyConfiguration {
                protection: Some(fidl_softmac::WlanProtection::RxTx),
                cipher_oui: Some([1, 2, 3]),
                cipher_type: Some(4),
                key_type: Some(fidl_common::WlanKeyType::Pairwise),
                peer_addr: Some([5; 6]),
                key_idx: Some(6),
                key: Some(vec![1, 2, 3, 4, 5, 6, 7]),
                rsc: Some(8),
                ..Default::default()
            }]
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_setkeys_req_no_bss() {
        let (mut ap, _, _) = make_ap().await;
        assert_variant!(
            ap.handle_mlme_setkeys_req(fidl_mlme::SetKeysRequest {
                keylist: vec![fidl_mlme::SetKeyDescriptor {
                    cipher_suite_oui: [1, 2, 3],
                    cipher_suite_type:
                        fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(4),
                    key_type: fidl_mlme::KeyType::Pairwise,
                    address: [5; 6],
                    key_id: 6,
                    key: vec![1, 2, 3, 4, 5, 6, 7],
                    rsc: 8,
                }],
            })
            .await
            .expect_err("expected Ap::handle_mlme_setkeys_req error"),
            Error::Status(_, zx::Status::BAD_STATE)
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_setkeys_req_bss_no_rsne() {
        let (mut ap, _, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );

        assert_variant!(
            ap.handle_mlme_setkeys_req(fidl_mlme::SetKeysRequest {
                keylist: vec![fidl_mlme::SetKeyDescriptor {
                    cipher_suite_oui: [1, 2, 3],
                    cipher_suite_type:
                        fidl_ieee80211::CipherSuiteType::from_primitive_allow_unknown(4),
                    key_type: fidl_mlme::KeyType::Pairwise,
                    address: [5; 6],
                    key_id: 6,
                    key: vec![1, 2, 3, 4, 5, 6, 7],
                    rsc: 8,
                }],
            })
            .await
            .expect_err("expected Ap::handle_mlme_setkeys_req error"),
            Error::Status(_, zx::Status::BAD_STATE)
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_auth_resp() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::AuthResponse(fidl_mlme::AuthenticateResponse {
            peer_sta_address: CLIENT_ADDR.to_array(),
            result_code: fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
        }))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AuthenticateResp) ok");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_auth_resp_no_bss() {
        let (mut ap, _, _) = make_ap().await;

        assert_eq!(
            zx::Status::from(
                ap.handle_mlme_req(wlan_sme::MlmeRequest::AuthResponse(
                    fidl_mlme::AuthenticateResponse {
                        peer_sta_address: CLIENT_ADDR.to_array(),
                        result_code: fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
                    }
                ))
                .await
                .expect_err(
                    "expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AuthenticateResp) error"
                )
            ),
            zx::Status::BAD_STATE
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_auth_resp_no_such_client() {
        let (mut ap, _, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );

        assert_eq!(
            zx::Status::from(
                ap.handle_mlme_req(wlan_sme::MlmeRequest::AuthResponse(
                    fidl_mlme::AuthenticateResponse {
                        peer_sta_address: CLIENT_ADDR.to_array(),
                        result_code: fidl_mlme::AuthenticateResultCode::AntiCloggingTokenRequired,
                    }
                ))
                .await
                .expect_err(
                    "expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AuthenticateResp) error"
                )
            ),
            zx::Status::NOT_FOUND
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_deauth_req() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::Deauthenticate(
            fidl_mlme::DeauthenticateRequest {
                peer_sta_address: CLIENT_ADDR.to_array(),
                reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDeauth,
            },
        ))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::DeauthenticateReq) ok");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_assoc_resp() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::AssocResponse(fidl_mlme::AssociateResponse {
            peer_sta_address: CLIENT_ADDR.to_array(),
            result_code: fidl_mlme::AssociateResultCode::Success,
            association_id: 1,
            capability_info: CapabilityInfo(0).raw(),
            rates: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        }))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AssociateResp) ok");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_disassoc_req() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::Disassociate(fidl_mlme::DisassociateRequest {
            peer_sta_address: CLIENT_ADDR.to_array(),
            reason_code: fidl_ieee80211::ReasonCode::LeavingNetworkDisassoc,
        }))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::DisassociateReq) ok");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_set_controlled_port_req() {
        let (mut ap, _, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::AssocResponse(fidl_mlme::AssociateResponse {
            peer_sta_address: CLIENT_ADDR.to_array(),
            result_code: fidl_mlme::AssociateResultCode::Success,
            association_id: 1,
            capability_info: CapabilityInfo(0).raw(),
            rates: vec![1, 2, 3],
        }))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::AssociateResp) ok");

        ap.handle_mlme_req(wlan_sme::MlmeRequest::SetCtrlPort(
            fidl_mlme::SetControlledPortRequest {
                peer_sta_address: CLIENT_ADDR.to_array(),
                state: fidl_mlme::ControlledPortState::Open,
            },
        ))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::SetControlledPort) ok");
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_handle_mlme_req_handle_mlme_eapol_req() {
        let (mut ap, fake_device_state, _) = make_ap().await;
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
            .await
            .expect("expected InfraBss::new ok"),
        );
        ap.bss.as_mut().unwrap().clients.insert(*CLIENT_ADDR, RemoteClient::new(*CLIENT_ADDR));

        ap.handle_mlme_req(wlan_sme::MlmeRequest::Eapol(fidl_mlme::EapolRequest {
            dst_addr: CLIENT_ADDR.to_array(),
            src_addr: BSSID.to_array(),
            data: vec![1, 2, 3],
        }))
        .await
        .expect("expected Ap::handle_mlme_msg(fidl_mlme::MlmeRequest::EapolReq) ok");
        assert_eq!(fake_device_state.lock().wlan_queue.len(), 1);
        assert_eq!(
            &fake_device_state.lock().wlan_queue[0].0[..],
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

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_mlme_respond_to_query_device_info() {
        let (mut ap, _, _) = make_ap().await;

        let (responder, receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QueryDeviceInfo(responder)).await,
            Ok(())
        );
        assert_eq!(
            receiver.await.unwrap(),
            fidl_mlme::DeviceInfo {
                sta_addr: BSSID.to_array(),
                role: fidl_common::WlanMacRole::Ap,
                bands: test_utils::fake_mlme_band_caps(),
                softmac_hardware_capability: 0,
                qos_capable: false,
            }
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_mlme_respond_to_query_discovery_support() {
        let (mut ap, _, _) = make_ap().await;

        let (responder, receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QueryDiscoverySupport(responder)).await,
            Ok(())
        );
        let resp = receiver.await.unwrap();
        assert_eq!(resp.scan_offload.supported, true);
        assert_eq!(resp.probe_response_offload.supported, false);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_mlme_respond_to_query_mac_sublayer_support() {
        let (mut ap, _, _) = make_ap().await;

        let (responder, receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QueryMacSublayerSupport(responder)).await,
            Ok(())
        );
        let resp = receiver.await.unwrap();
        assert_eq!(resp.rate_selection_offload.supported, false);
        assert_eq!(resp.data_plane.data_plane_type, fidl_common::DataPlaneType::EthernetDevice);
        assert_eq!(resp.device.is_synthetic, true);
        assert_eq!(
            resp.device.mac_implementation_type,
            fidl_common::MacImplementationType::Softmac
        );
        assert_eq!(resp.device.tx_status_report_supported, true);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_mlme_respond_to_query_security_support() {
        let (mut ap, _, _) = make_ap().await;

        let (responder, receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QuerySecuritySupport(responder)).await,
            Ok(())
        );
        let resp = receiver.await.unwrap();
        assert_eq!(resp.mfp.supported, false);
        assert_eq!(resp.sae.driver_handler_supported, false);
        assert_eq!(resp.sae.sme_handler_supported, false);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn ap_mlme_respond_to_query_spectrum_management_support() {
        let (mut ap, _, _) = make_ap().await;

        let (responder, receiver) = Responder::new();
        assert_variant!(
            ap.handle_mlme_req(wlan_sme::MlmeRequest::QuerySpectrumManagementSupport(responder))
                .await,
            Ok(())
        );
        assert_eq!(receiver.await.unwrap().dfs.supported, true);
    }

    #[test]
    fn display_rejection() {
        assert_eq!(format!("{}", Rejection::BadDsBits), "BadDsBits");
    }
}
