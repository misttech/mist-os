// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::convert::{banjo_to_fidl, fullmac_to_mlme};
use super::{FullmacDriverEvent, FullmacDriverEventSink};
use anyhow::{format_err, Context};
use fidl::HandleBased;
use std::ffi::c_void;
use {
    banjo_fuchsia_wlan_common as banjo_wlan_common,
    banjo_fuchsia_wlan_fullmac as banjo_wlan_fullmac, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_fullmac as fidl_fullmac, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fuchsia_zircon as zx,
};

/// Hand-rolled Rust version of the banjo wlan_fullmac_ifc_protocol for communication from the
/// driver up.
/// Note that we copy the individual fns out of this struct into the equivalent generated struct
/// in C++. Thanks to cbindgen, this gives us a compile-time confirmation that our function
/// signatures are correct.
#[repr(C)]
pub struct WlanFullmacIfcProtocol {
    pub(crate) ops: *const WlanFullmacIfcProtocolOps,
    pub(crate) ctx: Box<FullmacDriverEventSink>,
}

#[repr(C)]
pub struct WlanFullmacIfcProtocolOps {
    pub(crate) on_scan_result: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        result: *const banjo_wlan_fullmac::WlanFullmacScanResult,
    ),
    pub(crate) on_scan_end: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        end: *const banjo_wlan_fullmac::WlanFullmacScanEnd,
    ),
    pub(crate) connect_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacConnectConfirm,
    ),
    pub(crate) roam_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacRoamConfirm,
    ),
    pub(crate) auth_ind: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacAuthInd,
    ),
    pub(crate) deauth_conf:
        extern "C" fn(ctx: &mut FullmacDriverEventSink, peer_sta_address: *const u8),
    pub(crate) deauth_ind: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacDeauthIndication,
    ),
    pub(crate) assoc_ind: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacAssocInd,
    ),
    pub(crate) disassoc_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacDisassocConfirm,
    ),
    pub(crate) disassoc_ind: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacDisassocIndication,
    ),
    pub(crate) start_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacStartConfirm,
    ),
    pub(crate) stop_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacStopConfirm,
    ),
    pub(crate) eapol_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacEapolConfirm,
    ),
    pub(crate) on_channel_switch: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacChannelSwitchInfo,
    ),

    // MLME extensions
    pub(crate) signal_report: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacSignalReportIndication,
    ),
    pub(crate) eapol_ind: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacEapolIndication,
    ),
    pub(crate) on_pmk_available: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        info: *const banjo_wlan_fullmac::WlanFullmacPmkInfo,
    ),
    pub(crate) sae_handshake_ind: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        ind: *const banjo_wlan_fullmac::WlanFullmacSaeHandshakeInd,
    ),
    pub(crate) sae_frame_rx: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        frame: *const banjo_wlan_fullmac::WlanFullmacSaeFrame,
    ),
    pub(crate) on_wmm_status_resp: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        status: zx::sys::zx_status_t,
        wmm_params: *const banjo_wlan_common::WlanWmmParameters,
    ),
}

#[no_mangle]
extern "C" fn on_scan_result(
    ctx: &mut FullmacDriverEventSink,
    result: *const banjo_wlan_fullmac::WlanFullmacScanResult,
) {
    let result = banjo_to_fidl::convert_scan_result(unsafe { *result });
    ctx.0.send(FullmacDriverEvent::OnScanResult { result });
}
#[no_mangle]
extern "C" fn on_scan_end(
    ctx: &mut FullmacDriverEventSink,
    end: *const banjo_wlan_fullmac::WlanFullmacScanEnd,
) {
    let end = banjo_to_fidl::convert_scan_end(unsafe { *end });
    ctx.0.send(FullmacDriverEvent::OnScanEnd { end });
}
#[no_mangle]
extern "C" fn connect_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacConnectConfirm,
) {
    let resp = banjo_to_fidl::convert_connect_confirm(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::ConnectConf { resp });
}
#[no_mangle]
extern "C" fn roam_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacRoamConfirm,
) {
    let resp = banjo_to_fidl::convert_roam_confirm(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::RoamConf { resp });
}
#[no_mangle]
extern "C" fn auth_ind(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacAuthInd,
) {
    let ind = banjo_to_fidl::convert_authenticate_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::AuthInd { ind });
}
#[no_mangle]
extern "C" fn deauth_conf(ctx: &mut FullmacDriverEventSink, peer_sta_address: *const u8) {
    let resp = banjo_to_fidl::convert_deauthenticate_confirm(peer_sta_address);
    ctx.0.send(FullmacDriverEvent::DeauthConf { resp });
}
#[no_mangle]
extern "C" fn deauth_ind(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacDeauthIndication,
) {
    let ind = banjo_to_fidl::convert_deauthenticate_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::DeauthInd { ind });
}
#[no_mangle]
extern "C" fn assoc_ind(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacAssocInd,
) {
    let ind = banjo_to_fidl::convert_associate_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::AssocInd { ind });
}
#[no_mangle]
extern "C" fn disassoc_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacDisassocConfirm,
) {
    let resp = banjo_to_fidl::convert_disassociate_confirm(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::DisassocConf { resp });
}
#[no_mangle]
extern "C" fn disassoc_ind(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacDisassocIndication,
) {
    let ind = banjo_to_fidl::convert_disassociate_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::DisassocInd { ind });
}
#[no_mangle]
extern "C" fn start_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacStartConfirm,
) {
    let resp = banjo_to_fidl::convert_start_confirm(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::StartConf { resp });
}
#[no_mangle]
extern "C" fn stop_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacStopConfirm,
) {
    let resp = banjo_to_fidl::convert_stop_confirm(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::StopConf { resp });
}
#[no_mangle]
extern "C" fn eapol_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacEapolConfirm,
) {
    let resp = banjo_to_fidl::convert_eapol_confirm(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::EapolConf { resp });
}
#[no_mangle]
extern "C" fn on_channel_switch(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacChannelSwitchInfo,
) {
    let resp = banjo_to_fidl::convert_channel_switch_info(unsafe { *resp });
    ctx.0.send(FullmacDriverEvent::OnChannelSwitch { resp });
}

// MLME extensions
#[no_mangle]
extern "C" fn signal_report(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacSignalReportIndication,
) {
    let ind = banjo_to_fidl::convert_signal_report_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::SignalReport { ind });
}
#[no_mangle]
extern "C" fn eapol_ind(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacEapolIndication,
) {
    let ind = banjo_to_fidl::convert_eapol_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::EapolInd { ind });
}
#[no_mangle]
extern "C" fn on_pmk_available(
    ctx: &mut FullmacDriverEventSink,
    info: *const banjo_wlan_fullmac::WlanFullmacPmkInfo,
) {
    let info = banjo_to_fidl::convert_pmk_info(unsafe { *info });
    ctx.0.send(FullmacDriverEvent::OnPmkAvailable { info });
}
#[no_mangle]
extern "C" fn sae_handshake_ind(
    ctx: &mut FullmacDriverEventSink,
    ind: *const banjo_wlan_fullmac::WlanFullmacSaeHandshakeInd,
) {
    let ind = banjo_to_fidl::convert_sae_handshake_indication(unsafe { *ind });
    ctx.0.send(FullmacDriverEvent::SaeHandshakeInd { ind });
}
#[no_mangle]
extern "C" fn sae_frame_rx(
    ctx: &mut FullmacDriverEventSink,
    frame: *const banjo_wlan_fullmac::WlanFullmacSaeFrame,
) {
    let frame = banjo_to_fidl::convert_sae_frame(unsafe { *frame });
    ctx.0.send(FullmacDriverEvent::SaeFrameRx { frame });
}
#[no_mangle]
extern "C" fn on_wmm_status_resp(
    ctx: &mut FullmacDriverEventSink,
    status: zx::sys::zx_status_t,
    wmm_params: *const banjo_wlan_common::WlanWmmParameters,
) {
    let resp = banjo_to_fidl::convert_wmm_params(unsafe { *wmm_params });
    ctx.0.send(FullmacDriverEvent::OnWmmStatusResp { status, resp });
}

const PROTOCOL_OPS: WlanFullmacIfcProtocolOps = WlanFullmacIfcProtocolOps {
    on_scan_result: on_scan_result,
    on_scan_end: on_scan_end,
    connect_conf: connect_conf,
    roam_conf: roam_conf,
    auth_ind: auth_ind,
    deauth_conf: deauth_conf,
    deauth_ind: deauth_ind,
    assoc_ind: assoc_ind,
    disassoc_conf: disassoc_conf,
    disassoc_ind: disassoc_ind,
    start_conf: start_conf,
    stop_conf: stop_conf,
    eapol_conf: eapol_conf,
    on_channel_switch: on_channel_switch,

    // MLME extensions
    signal_report: signal_report,
    eapol_ind: eapol_ind,
    on_pmk_available: on_pmk_available,
    sae_handshake_ind: sae_handshake_ind,
    sae_frame_rx: sae_frame_rx,
    on_wmm_status_resp: on_wmm_status_resp,
};

impl WlanFullmacIfcProtocol {
    pub(crate) fn new(sink: Box<FullmacDriverEventSink>) -> Self {
        // Const reference has 'static lifetime, so it's safe to pass down to the driver.
        let ops = &PROTOCOL_OPS;
        Self { ops, ctx: sink }
    }
}

/// This trait abstracts how Device accomplish operations. Test code
/// can then implement trait methods instead of mocking an underlying DeviceInterface
/// and FIDL proxy.
pub trait DeviceOps {
    fn start(&mut self, ifc: *const WlanFullmacIfcProtocol) -> Result<fidl::Channel, zx::Status>;
    fn query_device_info(&mut self) -> anyhow::Result<fidl_fullmac::WlanFullmacQueryInfo>;
    fn query_mac_sublayer_support(&mut self) -> anyhow::Result<fidl_common::MacSublayerSupport>;
    fn query_security_support(&mut self) -> anyhow::Result<fidl_common::SecuritySupport>;
    fn query_spectrum_management_support(
        &mut self,
    ) -> anyhow::Result<fidl_common::SpectrumManagementSupport>;
    fn start_scan(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplStartScanRequest,
    ) -> anyhow::Result<()>;
    fn connect(&mut self, req: fidl_fullmac::WlanFullmacImplConnectRequest) -> anyhow::Result<()>;
    fn reconnect(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplReconnectRequest,
    ) -> anyhow::Result<()>;
    fn auth_resp(
        &mut self,
        resp: fidl_fullmac::WlanFullmacImplAuthRespRequest,
    ) -> anyhow::Result<()>;
    fn deauth(&mut self, req: fidl_fullmac::WlanFullmacImplDeauthRequest) -> anyhow::Result<()>;
    fn assoc_resp(
        &mut self,
        resp: fidl_fullmac::WlanFullmacImplAssocRespRequest,
    ) -> anyhow::Result<()>;
    fn disassoc(&mut self, req: fidl_fullmac::WlanFullmacImplDisassocRequest)
        -> anyhow::Result<()>;
    fn reset(&mut self, req: fidl_fullmac::WlanFullmacImplResetRequest) -> anyhow::Result<()>;
    fn start_bss(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplStartBssRequest,
    ) -> anyhow::Result<()>;
    fn stop_bss(&mut self, req: fidl_fullmac::WlanFullmacImplStopBssRequest) -> anyhow::Result<()>;
    fn set_keys_req(
        &mut self,
        req: fidl_fullmac::WlanFullmacSetKeysReq,
    ) -> anyhow::Result<fidl_fullmac::WlanFullmacSetKeysResp>;
    fn del_keys_req(&mut self, req: fidl_fullmac::WlanFullmacDelKeysReq) -> anyhow::Result<()>;
    fn eapol_tx(&mut self, req: fidl_fullmac::WlanFullmacImplEapolTxRequest) -> anyhow::Result<()>;
    fn get_iface_counter_stats(
        &mut self,
    ) -> anyhow::Result<fidl_mlme::GetIfaceCounterStatsResponse>;
    fn get_iface_histogram_stats(
        &mut self,
    ) -> anyhow::Result<fidl_mlme::GetIfaceHistogramStatsResponse>;
    fn sae_handshake_resp(
        &mut self,
        resp: fidl_fullmac::WlanFullmacSaeHandshakeResp,
    ) -> anyhow::Result<()>;
    fn sae_frame_tx(&mut self, frame: fidl_fullmac::WlanFullmacSaeFrame) -> anyhow::Result<()>;
    fn wmm_status_req(&mut self) -> anyhow::Result<()>;
    fn on_link_state_changed(&mut self, online: bool) -> anyhow::Result<()>;
}

/// A `RawFullmacDeviceFfi` allows transmitting MLME messages.
#[repr(C)]
pub struct RawFullmacDeviceFfi {
    device: *mut c_void,
    /// Start operations on the underlying device and return the SME channel.
    start_fullmac_ifc_server: extern "C" fn(
        device: *mut c_void,
        ifc: *const WlanFullmacIfcProtocol,
        fullmac_ifc_server_end_handle: zx::sys::zx_handle_t,
    ) -> zx::sys::zx_status_t,
}

// Our device is used inside a separate worker thread, so we force Rust to allow this.
unsafe impl Send for FullmacDevice {}

pub struct FullmacDevice {
    raw_device: RawFullmacDeviceFfi,
    fullmac_impl_sync_proxy: fidl_fullmac::WlanFullmacImpl_SynchronousProxy,
}

impl FullmacDevice {
    pub fn new(
        raw_device: RawFullmacDeviceFfi,
        fullmac_impl_sync_proxy: fidl_fullmac::WlanFullmacImpl_SynchronousProxy,
    ) -> FullmacDevice {
        FullmacDevice { raw_device, fullmac_impl_sync_proxy }
    }
}

impl DeviceOps for FullmacDevice {
    fn start(&mut self, ifc: *const WlanFullmacIfcProtocol) -> Result<fidl::Channel, zx::Status> {
        let (fullmac_ifc_client_end, fullmac_ifc_server_end) = fidl::endpoints::create_endpoints();

        // Start the WlanFullmacImplIfc server in wlanif before calling WlanFullmacImpl::Start.
        // Note: wlanif takes ownership of the server end through this call.
        let status = (self.raw_device.start_fullmac_ifc_server)(
            self.raw_device.device,
            ifc,
            fullmac_ifc_server_end.into_channel().into_raw(),
        );
        zx::Status::ok(status)?;

        self.fullmac_impl_sync_proxy
            .start(fullmac_ifc_client_end, zx::Time::INFINITE)
            .map_err(|e| {
                tracing::error!("FIDL error on Start: {}", e);
                zx::Status::INTERNAL
            })?
            .map_err(|e| zx::Status::from_raw(e))
    }

    fn query_device_info(&mut self) -> anyhow::Result<fidl_fullmac::WlanFullmacQueryInfo> {
        self.fullmac_impl_sync_proxy
            .query(zx::Time::INFINITE)
            .context("FIDL error on QueryDeviceInfo")?
            .map_err(|e| format_err!("Driver returned error on QueryDeviceInfo: {}", e))
    }

    fn query_mac_sublayer_support(&mut self) -> anyhow::Result<fidl_common::MacSublayerSupport> {
        self.fullmac_impl_sync_proxy
            .query_mac_sublayer_support(zx::Time::INFINITE)
            .context("FIDL error on QueryMacSublayerSupport")?
            .map_err(|e| format_err!("Driver returned error on QueryMacSublayerSupport: {}", e))
    }

    fn query_security_support(&mut self) -> anyhow::Result<fidl_common::SecuritySupport> {
        self.fullmac_impl_sync_proxy
            .query_security_support(zx::Time::INFINITE)
            .context("FIDL error on QuerySecuritySupport")?
            .map_err(|e| format_err!("Driver returned error on QuerySecuritySupport: {}", e))
    }

    fn query_spectrum_management_support(
        &mut self,
    ) -> anyhow::Result<fidl_common::SpectrumManagementSupport> {
        self.fullmac_impl_sync_proxy
            .query_spectrum_management_support(zx::Time::INFINITE)
            .context("FIDL error on QuerySpectrumManagementSupport")?
            .map_err(|e| {
                format_err!("Driver returned error on QuerySpectrumManagementSupport: {}", e)
            })
    }

    fn start_scan(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplStartScanRequest,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .start_scan(&req, zx::Time::INFINITE)
            .context("FIDL error on StartScan")
    }
    fn connect(&mut self, req: fidl_fullmac::WlanFullmacImplConnectRequest) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .connect(&req, zx::Time::INFINITE)
            .context("FIDL error on Connect")
    }
    fn reconnect(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplReconnectRequest,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .reconnect(&req, zx::Time::INFINITE)
            .context("FIDL error on Reconnect")
    }
    fn auth_resp(
        &mut self,
        resp: fidl_fullmac::WlanFullmacImplAuthRespRequest,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .auth_resp(&resp, zx::Time::INFINITE)
            .context("FIDL error on AuthResp")
    }
    fn deauth(&mut self, req: fidl_fullmac::WlanFullmacImplDeauthRequest) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .deauth(&req, zx::Time::INFINITE)
            .context("FIDL error on Deauth")
    }
    fn assoc_resp(
        &mut self,
        resp: fidl_fullmac::WlanFullmacImplAssocRespRequest,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .assoc_resp(&resp, zx::Time::INFINITE)
            .context("FIDL error on AssocResp")
    }
    fn disassoc(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplDisassocRequest,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .disassoc(&req, zx::Time::INFINITE)
            .context("FIDL error on Disassoc")
    }
    fn reset(&mut self, req: fidl_fullmac::WlanFullmacImplResetRequest) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy.reset(&req, zx::Time::INFINITE).context("FIDL error on Reset")
    }
    fn start_bss(
        &mut self,
        req: fidl_fullmac::WlanFullmacImplStartBssRequest,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .start_bss(&req, zx::Time::INFINITE)
            .context("FIDL error on StartBss")
    }
    fn stop_bss(&mut self, req: fidl_fullmac::WlanFullmacImplStopBssRequest) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .stop_bss(&req, zx::Time::INFINITE)
            .context("FIDL error on StopBss")
    }
    fn set_keys_req(
        &mut self,
        req: fidl_fullmac::WlanFullmacSetKeysReq,
    ) -> anyhow::Result<fidl_fullmac::WlanFullmacSetKeysResp> {
        self.fullmac_impl_sync_proxy
            .set_keys_req(&req, zx::Time::INFINITE)
            .context("FIDL error on SetKeysReq")
    }
    fn del_keys_req(&mut self, req: fidl_fullmac::WlanFullmacDelKeysReq) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .del_keys_req(&req, zx::Time::INFINITE)
            .context("FIDL Error on DelKeysReq")
    }
    fn eapol_tx(&mut self, req: fidl_fullmac::WlanFullmacImplEapolTxRequest) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .eapol_tx(&req, zx::Time::INFINITE)
            .context("FIDL error on EapolTx")
    }
    fn get_iface_counter_stats(
        &mut self,
    ) -> anyhow::Result<fidl_mlme::GetIfaceCounterStatsResponse> {
        match self
            .fullmac_impl_sync_proxy
            .get_iface_counter_stats(zx::Time::INFINITE)
            .context("FIDL error on GetIfaceCounterStats")?
        {
            Ok(stats) => Ok(fidl_mlme::GetIfaceCounterStatsResponse::Stats(
                fullmac_to_mlme::convert_iface_counter_stats(stats),
            )),
            Err(e) => Ok(fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(e)),
        }
    }
    fn get_iface_histogram_stats(
        &mut self,
    ) -> anyhow::Result<fidl_mlme::GetIfaceHistogramStatsResponse> {
        match self
            .fullmac_impl_sync_proxy
            .get_iface_histogram_stats(zx::Time::INFINITE)
            .context("FIDL error on GetIfaceHistogramStats")?
        {
            Ok(stats) => Ok(fidl_mlme::GetIfaceHistogramStatsResponse::Stats(
                fullmac_to_mlme::convert_iface_histogram_stats(stats),
            )),
            Err(e) => Ok(fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(e)),
        }
    }
    fn sae_handshake_resp(
        &mut self,
        resp: fidl_fullmac::WlanFullmacSaeHandshakeResp,
    ) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .sae_handshake_resp(&resp, zx::Time::INFINITE)
            .context("FIDL error on SaeHandshakeResp")
    }
    fn sae_frame_tx(&mut self, frame: fidl_fullmac::WlanFullmacSaeFrame) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .sae_frame_tx(&frame, zx::Time::INFINITE)
            .context("FIDL error on SaeFrameTx")
    }
    fn wmm_status_req(&mut self) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .wmm_status_req(zx::Time::INFINITE)
            .context("FIDL error on WmmStatusReq")
    }
    fn on_link_state_changed(&mut self, online: bool) -> anyhow::Result<()> {
        self.fullmac_impl_sync_proxy
            .on_link_state_changed(online, zx::Time::INFINITE)
            .context("FIDL error on OnLinkStateChanged")
    }
}

#[cfg(test)]
pub mod test_utils {
    use super::*;
    use std::sync::{Arc, Mutex};
    use {fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme};

    #[derive(Debug)]
    pub enum DriverCall {
        StartScan { req: fidl_fullmac::WlanFullmacImplStartScanRequest },
        ConnectReq { req: fidl_fullmac::WlanFullmacImplConnectRequest },
        ReconnectReq { req: fidl_fullmac::WlanFullmacImplReconnectRequest },
        AuthResp { resp: fidl_fullmac::WlanFullmacImplAuthRespRequest },
        DeauthReq { req: fidl_fullmac::WlanFullmacImplDeauthRequest },
        AssocResp { resp: fidl_fullmac::WlanFullmacImplAssocRespRequest },
        Disassoc { req: fidl_fullmac::WlanFullmacImplDisassocRequest },
        Reset { req: fidl_fullmac::WlanFullmacImplResetRequest },
        StartBss { req: fidl_fullmac::WlanFullmacImplStartBssRequest },
        StopBss { req: fidl_fullmac::WlanFullmacImplStopBssRequest },
        SetKeysReq { req: fidl_fullmac::WlanFullmacSetKeysReq },
        DelKeysReq { req: fidl_fullmac::WlanFullmacDelKeysReq },
        EapolTx { req: fidl_fullmac::WlanFullmacImplEapolTxRequest },
        GetIfaceCounterStats,
        GetIfaceHistogramStats,
        SaeHandshakeResp { resp: fidl_fullmac::WlanFullmacSaeHandshakeResp },
        SaeFrameTx { frame: fidl_fullmac::WlanFullmacSaeFrame },
        WmmStatusReq,
        OnLinkStateChanged { online: bool },
    }

    pub struct FakeFullmacDeviceMocks {
        pub captured_driver_calls: Vec<DriverCall>,
        pub start_fn_status_mock: Option<zx::sys::zx_status_t>,

        // Note: anyhow::Error isn't cloneable, so the query mocks are all optionals to make this
        // easier to work with.
        //
        // If any of the query mocks are None, then an Err is returned from DeviceOps with an empty
        // error message.
        pub query_device_info_mock: Option<fidl_fullmac::WlanFullmacQueryInfo>,
        pub query_mac_sublayer_support_mock: Option<fidl_common::MacSublayerSupport>,
        pub query_security_support_mock: Option<fidl_common::SecuritySupport>,
        pub query_spectrum_management_support_mock: Option<fidl_common::SpectrumManagementSupport>,

        pub set_keys_resp_mock: Option<fidl_fullmac::WlanFullmacSetKeysResp>,
        pub get_iface_counter_stats_mock: Option<fidl_mlme::GetIfaceCounterStatsResponse>,
        pub get_iface_histogram_stats_mock: Option<fidl_mlme::GetIfaceHistogramStatsResponse>,
    }

    unsafe impl Send for FakeFullmacDevice {}
    pub struct FakeFullmacDevice {
        pub usme_bootstrap_client_end:
            Option<fidl::endpoints::ClientEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub usme_bootstrap_server_end:
            Option<fidl::endpoints::ServerEnd<fidl_sme::UsmeBootstrapMarker>>,

        // This is boxed because tests want a reference to this to check captured calls, but in
        // production we pass ownership of the DeviceOps to FullmacMlme. This avoids changing
        // ownership semantics for tests.
        pub mocks: Arc<Mutex<FakeFullmacDeviceMocks>>,
    }

    const fn dummy_band_cap() -> fidl_fullmac::WlanFullmacBandCapability {
        fidl_fullmac::WlanFullmacBandCapability {
            band: fidl_common::WlanBand::TwoGhz,
            basic_rate_count: 0,
            basic_rate_list: [0u8; 12],
            ht_supported: false,
            ht_caps: fidl_ieee80211::HtCapabilities { bytes: [0u8; 26] },
            vht_supported: false,
            vht_caps: fidl_ieee80211::VhtCapabilities { bytes: [0u8; 12] },
            operating_channel_count: 0,
            operating_channel_list: [0u8; 256],
        }
    }

    impl FakeFullmacDevice {
        pub fn new() -> Self {
            // Create a channel for SME requests, to be surfaced by start().
            let (usme_bootstrap_client_end, usme_bootstrap_server_end) =
                fidl::endpoints::create_endpoints::<fidl_sme::UsmeBootstrapMarker>();

            Self {
                usme_bootstrap_client_end: Some(usme_bootstrap_client_end),
                usme_bootstrap_server_end: Some(usme_bootstrap_server_end),
                mocks: Arc::new(Mutex::new(FakeFullmacDeviceMocks {
                    captured_driver_calls: vec![],
                    start_fn_status_mock: None,
                    query_device_info_mock: Some(fidl_fullmac::WlanFullmacQueryInfo {
                        sta_addr: [0u8; 6],
                        role: fidl_common::WlanMacRole::Client,
                        band_cap_list: std::array::from_fn(|_| dummy_band_cap()),
                        band_cap_count: 0,
                    }),
                    query_mac_sublayer_support_mock: Some(fidl_common::MacSublayerSupport {
                        rate_selection_offload: fidl_common::RateSelectionOffloadExtension {
                            supported: false,
                        },
                        data_plane: fidl_common::DataPlaneExtension {
                            data_plane_type: fidl_common::DataPlaneType::GenericNetworkDevice,
                        },
                        device: fidl_common::DeviceExtension {
                            is_synthetic: true,
                            mac_implementation_type: fidl_common::MacImplementationType::Fullmac,
                            tx_status_report_supported: false,
                        },
                    }),
                    query_security_support_mock: Some(fidl_common::SecuritySupport {
                        sae: fidl_common::SaeFeature {
                            driver_handler_supported: false,
                            sme_handler_supported: true,
                        },
                        mfp: fidl_common::MfpFeature { supported: false },
                    }),
                    query_spectrum_management_support_mock: Some(
                        fidl_common::SpectrumManagementSupport {
                            dfs: fidl_common::DfsFeature { supported: false },
                        },
                    ),
                    set_keys_resp_mock: None,
                    get_iface_counter_stats_mock: None,
                    get_iface_histogram_stats_mock: None,
                })),
            }
        }
    }

    impl DeviceOps for FakeFullmacDevice {
        fn start(
            &mut self,
            _ifc: *const WlanFullmacIfcProtocol,
        ) -> Result<fidl::Channel, zx::Status> {
            match self.mocks.lock().unwrap().start_fn_status_mock {
                Some(status) => Err(zx::Status::from_raw(status)),

                // Start can only be called once since this moves usme_bootstrap_server_end.
                None => Ok(self.usme_bootstrap_server_end.take().unwrap().into_channel()),
            }
        }

        fn query_device_info(&mut self) -> anyhow::Result<fidl_fullmac::WlanFullmacQueryInfo> {
            self.mocks.lock().unwrap().query_device_info_mock.clone().ok_or(format_err!(""))
        }

        fn query_mac_sublayer_support(
            &mut self,
        ) -> anyhow::Result<fidl_common::MacSublayerSupport> {
            self.mocks
                .lock()
                .unwrap()
                .query_mac_sublayer_support_mock
                .clone()
                .ok_or(format_err!(""))
        }

        fn query_security_support(&mut self) -> anyhow::Result<fidl_common::SecuritySupport> {
            self.mocks.lock().unwrap().query_security_support_mock.clone().ok_or(format_err!(""))
        }

        fn query_spectrum_management_support(
            &mut self,
        ) -> anyhow::Result<fidl_common::SpectrumManagementSupport> {
            self.mocks
                .lock()
                .unwrap()
                .query_spectrum_management_support_mock
                .clone()
                .ok_or(format_err!(""))
        }

        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        fn start_scan(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplStartScanRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::StartScan { req });
            Ok(())
        }

        fn connect(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplConnectRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::ConnectReq { req });
            Ok(())
        }
        fn reconnect(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplReconnectRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::ReconnectReq { req });
            Ok(())
        }
        fn auth_resp(
            &mut self,
            resp: fidl_fullmac::WlanFullmacImplAuthRespRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::AuthResp { resp });
            Ok(())
        }
        fn deauth(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplDeauthRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::DeauthReq { req });
            Ok(())
        }
        fn assoc_resp(
            &mut self,
            resp: fidl_fullmac::WlanFullmacImplAssocRespRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::AssocResp { resp });
            Ok(())
        }
        fn disassoc(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplDisassocRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::Disassoc { req });
            Ok(())
        }
        fn reset(&mut self, req: fidl_fullmac::WlanFullmacImplResetRequest) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::Reset { req });
            Ok(())
        }
        fn start_bss(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplStartBssRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::StartBss { req });
            Ok(())
        }
        fn stop_bss(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplStopBssRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::StopBss { req });
            Ok(())
        }
        fn set_keys_req(
            &mut self,
            req: fidl_fullmac::WlanFullmacSetKeysReq,
        ) -> anyhow::Result<fidl_fullmac::WlanFullmacSetKeysResp> {
            let num_keys = req.num_keys;
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::SetKeysReq { req });
            match self.mocks.lock().unwrap().set_keys_resp_mock {
                Some(resp) => Ok(resp),
                None => {
                    Ok(fidl_fullmac::WlanFullmacSetKeysResp { num_keys, statuslist: [0i32; 4] })
                }
            }
        }
        fn del_keys_req(&mut self, req: fidl_fullmac::WlanFullmacDelKeysReq) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::DelKeysReq { req });
            Ok(())
        }
        fn eapol_tx(
            &mut self,
            req: fidl_fullmac::WlanFullmacImplEapolTxRequest,
        ) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::EapolTx { req });
            Ok(())
        }
        fn get_iface_counter_stats(
            &mut self,
        ) -> anyhow::Result<fidl_mlme::GetIfaceCounterStatsResponse> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::GetIfaceCounterStats);
            Ok(self.mocks.lock().unwrap().get_iface_counter_stats_mock.clone().unwrap_or(
                fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(zx::sys::ZX_ERR_NOT_SUPPORTED),
            ))
        }
        fn get_iface_histogram_stats(
            &mut self,
        ) -> anyhow::Result<fidl_mlme::GetIfaceHistogramStatsResponse> {
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::GetIfaceHistogramStats);
            Ok(self.mocks.lock().unwrap().get_iface_histogram_stats_mock.clone().unwrap_or(
                fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(
                    zx::sys::ZX_ERR_NOT_SUPPORTED,
                ),
            ))
        }
        fn sae_handshake_resp(
            &mut self,
            resp: fidl_fullmac::WlanFullmacSaeHandshakeResp,
        ) -> anyhow::Result<()> {
            Ok(self
                .mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::SaeHandshakeResp { resp }))
        }
        fn sae_frame_tx(&mut self, frame: fidl_fullmac::WlanFullmacSaeFrame) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::SaeFrameTx { frame });
            Ok(())
        }
        fn wmm_status_req(&mut self) -> anyhow::Result<()> {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::WmmStatusReq);
            Ok(())
        }
        fn on_link_state_changed(&mut self, online: bool) -> anyhow::Result<()> {
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::OnLinkStateChanged { online });
            Ok(())
        }
    }
}
