// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{convert::banjo_to_fidl, FullmacDriverEvent, FullmacDriverEventSink},
    banjo_fuchsia_hardware_wlan_fullmac as banjo_wlan_fullmac,
    banjo_fuchsia_wlan_common as banjo_wlan_common, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fuchsia_zircon as zx,
    std::ffi::c_void,
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
    pub(crate) deauth_conf: extern "C" fn(
        ctx: &mut FullmacDriverEventSink,
        resp: *const banjo_wlan_fullmac::WlanFullmacDeauthConfirm,
    ),
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
extern "C" fn deauth_conf(
    ctx: &mut FullmacDriverEventSink,
    resp: *const banjo_wlan_fullmac::WlanFullmacDeauthConfirm,
) {
    let resp = banjo_to_fidl::convert_deauthenticate_confirm(unsafe { *resp });
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

// Our device is used inside a separate worker thread, so we force Rust to allow this.
unsafe impl Send for FullmacDeviceInterface {}

/// A `FullmacDeviceInterface` allows transmitting frames and MLME messages.
#[repr(C)]
pub struct FullmacDeviceInterface {
    device: *mut c_void,
    /// Start operations on the underlying device and return the SME channel.
    start: extern "C" fn(
        device: *mut c_void,
        ifc: *const WlanFullmacIfcProtocol,
        out_sme_channel: *mut zx::sys::zx_handle_t,
    ) -> zx::sys::zx_status_t,

    query_device_info:
        extern "C" fn(device: *mut c_void) -> banjo_wlan_fullmac::WlanFullmacQueryInfo,
    query_mac_sublayer_support:
        extern "C" fn(device: *mut c_void) -> banjo_wlan_common::MacSublayerSupport,
    query_security_support:
        extern "C" fn(device: *mut c_void) -> banjo_wlan_common::SecuritySupport,
    query_spectrum_management_support:
        extern "C" fn(device: *mut c_void) -> banjo_wlan_common::SpectrumManagementSupport,

    start_scan:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacScanReq),
    connect_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacConnectReq),
    reconnect_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacReconnectReq),
    auth_resp:
        extern "C" fn(device: *mut c_void, resp: *mut banjo_wlan_fullmac::WlanFullmacAuthResp),
    deauth_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacDeauthReq),
    assoc_resp:
        extern "C" fn(device: *mut c_void, resp: *mut banjo_wlan_fullmac::WlanFullmacAssocResp),
    disassoc_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacDisassocReq),
    reset_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacResetReq),
    start_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacStartReq),
    stop_req: extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacStopReq),
    set_keys_req: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacSetKeysReq,
    ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp,
    del_keys_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacDelKeysReq),
    eapol_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacEapolReq),
    get_iface_counter_stats: extern "C" fn(
        device: *mut c_void,
        out_status: *mut i32,
    ) -> banjo_wlan_fullmac::WlanFullmacIfaceCounterStats,
    get_iface_histogram_stats: extern "C" fn(
        device: *mut c_void,
        out_status: *mut i32,
    )
        -> banjo_wlan_fullmac::WlanFullmacIfaceHistogramStats,
    sae_handshake_resp: extern "C" fn(
        device: *mut c_void,
        resp: *mut banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp,
    ),
    sae_frame_tx:
        extern "C" fn(device: *mut c_void, frame: *mut banjo_wlan_fullmac::WlanFullmacSaeFrame),
    wmm_status_req: extern "C" fn(device: *mut c_void),

    on_link_state_changed: extern "C" fn(device: *mut c_void, online: bool),
}

impl FullmacDeviceInterface {
    pub fn start(&self, ifc: *const WlanFullmacIfcProtocol) -> Result<zx::Handle, zx::Status> {
        let mut out_channel = 0;
        let status = (self.start)(self.device, ifc, &mut out_channel as *mut u32);
        // Unsafe block required because we cannot pass a Rust handle over FFI. An invalid
        // handle violates the banjo API, and may be detected by the caller of this fn.
        zx::ok(status).and_then(|_| {
            let handle = unsafe { zx::Handle::from_raw(out_channel) };
            if handle.is_invalid() {
                Err(zx::Status::BAD_HANDLE)
            } else {
                Ok(handle)
            }
        })
    }

    pub fn query_device_info(&self) -> banjo_wlan_fullmac::WlanFullmacQueryInfo {
        (self.query_device_info)(self.device)
    }

    pub fn query_mac_sublayer_support(&self) -> banjo_wlan_common::MacSublayerSupport {
        (self.query_mac_sublayer_support)(self.device)
    }

    pub fn query_security_support(&self) -> banjo_wlan_common::SecuritySupport {
        (self.query_security_support)(self.device)
    }

    pub fn query_spectrum_management_support(
        &self,
    ) -> banjo_wlan_common::SpectrumManagementSupport {
        (self.query_spectrum_management_support)(self.device)
    }

    pub fn start_scan(&self, req: &mut banjo_wlan_fullmac::WlanFullmacScanReq) {
        (self.start_scan)(self.device, req as *mut banjo_wlan_fullmac::WlanFullmacScanReq)
    }
    pub fn connect_req(&self, req: &mut banjo_wlan_fullmac::WlanFullmacConnectReq) {
        (self.connect_req)(self.device, req as *mut banjo_wlan_fullmac::WlanFullmacConnectReq)
    }
    pub fn reconnect_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacReconnectReq) {
        (self.reconnect_req)(
            self.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacReconnectReq,
        )
    }
    pub fn auth_resp(&self, mut resp: banjo_wlan_fullmac::WlanFullmacAuthResp) {
        (self.auth_resp)(self.device, &mut resp as *mut banjo_wlan_fullmac::WlanFullmacAuthResp)
    }
    pub fn deauth_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacDeauthReq) {
        (self.deauth_req)(self.device, &mut req as *mut banjo_wlan_fullmac::WlanFullmacDeauthReq)
    }
    pub fn assoc_resp(&self, mut resp: banjo_wlan_fullmac::WlanFullmacAssocResp) {
        (self.assoc_resp)(self.device, &mut resp as *mut banjo_wlan_fullmac::WlanFullmacAssocResp)
    }
    pub fn disassoc_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacDisassocReq) {
        (self.disassoc_req)(
            self.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacDisassocReq,
        )
    }
    pub fn reset_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacResetReq) {
        (self.reset_req)(self.device, &mut req as *mut banjo_wlan_fullmac::WlanFullmacResetReq)
    }
    pub fn start_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacStartReq) {
        (self.start_req)(self.device, &mut req as *mut banjo_wlan_fullmac::WlanFullmacStartReq)
    }
    pub fn stop_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacStopReq) {
        (self.stop_req)(self.device, &mut req as *mut banjo_wlan_fullmac::WlanFullmacStopReq)
    }
    pub fn set_keys_req(
        &self,
        req: &mut banjo_wlan_fullmac::WlanFullmacSetKeysReq,
    ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp {
        (self.set_keys_req)(self.device, req as *mut banjo_wlan_fullmac::WlanFullmacSetKeysReq)
    }
    pub fn del_keys_req(&self, mut req: banjo_wlan_fullmac::WlanFullmacDelKeysReq) {
        (self.del_keys_req)(self.device, &mut req as *mut banjo_wlan_fullmac::WlanFullmacDelKeysReq)
    }
    pub fn eapol_req(&self, req: &mut banjo_wlan_fullmac::WlanFullmacEapolReq) {
        (self.eapol_req)(self.device, req as *mut banjo_wlan_fullmac::WlanFullmacEapolReq)
    }
    pub fn get_iface_counter_stats(&self) -> fidl_mlme::GetIfaceCounterStatsResponse {
        let mut out_status: i32 = 0;
        let stats = (self.get_iface_counter_stats)(self.device, &mut out_status as *mut i32);
        if out_status == zx::sys::ZX_OK {
            fidl_mlme::GetIfaceCounterStatsResponse::Stats(
                banjo_to_fidl::convert_iface_counter_stats(stats),
            )
        } else {
            fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(out_status)
        }
    }
    pub fn get_iface_histogram_stats(&self) -> fidl_mlme::GetIfaceHistogramStatsResponse {
        let mut out_status: i32 = 0;
        let stats = (self.get_iface_histogram_stats)(self.device, &mut out_status as *mut i32);
        if out_status == zx::sys::ZX_OK {
            fidl_mlme::GetIfaceHistogramStatsResponse::Stats(
                banjo_to_fidl::convert_iface_histogram_stats(stats),
            )
        } else {
            fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(out_status)
        }
    }
    pub fn sae_handshake_resp(&self, mut resp: banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp) {
        (self.sae_handshake_resp)(
            self.device,
            &mut resp as *mut banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp,
        )
    }
    pub fn sae_frame_tx(&self, frame: &mut banjo_wlan_fullmac::WlanFullmacSaeFrame) {
        (self.sae_frame_tx)(self.device, frame as *mut banjo_wlan_fullmac::WlanFullmacSaeFrame)
    }
    pub fn wmm_status_req(&self) {
        (self.wmm_status_req)(self.device)
    }
    pub fn set_link_state(&self, controlled_port_state: fidl_mlme::ControlledPortState) {
        let online = match controlled_port_state {
            fidl_mlme::ControlledPortState::Open => true,
            fidl_mlme::ControlledPortState::Closed => false,
        };
        (self.on_link_state_changed)(self.device, online)
    }
}

#[cfg(test)]
pub mod test_utils {
    use {
        super::*,
        banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
        fuchsia_zircon::AsHandleRef,
        std::{pin::Pin, slice},
    };

    #[derive(Debug)]
    pub enum DriverCall {
        StartScan {
            req: banjo_wlan_fullmac::WlanFullmacScanReq,
            channels: Vec<u8>,
            ssids: Vec<banjo_wlan_ieee80211::CSsid>,
        },
        ConnectReq {
            req: banjo_wlan_fullmac::WlanFullmacConnectReq,
            selected_bss_ies: Vec<u8>,
            sae_password: Vec<u8>,
            wep_key: Vec<u8>,
            security_ie: Vec<u8>,
        },
        ReconnectReq {
            req: banjo_wlan_fullmac::WlanFullmacReconnectReq,
        },
        AuthResp {
            resp: banjo_wlan_fullmac::WlanFullmacAuthResp,
        },
        DeauthReq {
            req: banjo_wlan_fullmac::WlanFullmacDeauthReq,
        },
        AssocResp {
            resp: banjo_wlan_fullmac::WlanFullmacAssocResp,
        },
        DisassocReq {
            req: banjo_wlan_fullmac::WlanFullmacDisassocReq,
        },
        ResetReq {
            req: banjo_wlan_fullmac::WlanFullmacResetReq,
        },
        StartReq {
            req: banjo_wlan_fullmac::WlanFullmacStartReq,
        },
        StopReq {
            req: banjo_wlan_fullmac::WlanFullmacStopReq,
        },
        SetKeysReq {
            req: banjo_wlan_fullmac::WlanFullmacSetKeysReq,
            keys: Vec<Vec<u8>>,
        },
        DelKeysReq {
            req: banjo_wlan_fullmac::WlanFullmacDelKeysReq,
        },
        EapolReq {
            req: banjo_wlan_fullmac::WlanFullmacEapolReq,
            data: Vec<u8>,
        },
        GetIfaceCounterStats,
        GetIfaceHistogramStats,
        SaeHandshakeResp {
            resp: banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp,
        },
        SaeFrameTx {
            frame: banjo_wlan_fullmac::WlanFullmacSaeFrame,
            sae_fields: Vec<u8>,
        },
        WmmStatusReq,
        OnLinkStateChanged {
            online: bool,
        },
    }

    pub struct FakeFullmacDevice {
        pub usme_bootstrap_client_end:
            Option<fidl::endpoints::ClientEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub usme_bootstrap_server_end:
            Option<fidl::endpoints::ServerEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub captured_driver_calls: Vec<DriverCall>,
        pub start_fn_status_mock: Option<zx::sys::zx_status_t>,
        pub query_device_info_mock: banjo_wlan_fullmac::WlanFullmacQueryInfo,
        pub query_mac_sublayer_support_mock: banjo_wlan_common::MacSublayerSupport,
        pub set_keys_resp_mock: Option<banjo_wlan_fullmac::WlanFullmacSetKeysResp>,
        pub get_iface_counter_stats_mock:
            Option<(i32, banjo_wlan_fullmac::WlanFullmacIfaceCounterStats)>,
        pub get_iface_histogram_stats_mock:
            Option<(i32, banjo_wlan_fullmac::WlanFullmacIfaceHistogramStats)>,
    }

    const fn dummy_band_cap() -> banjo_wlan_fullmac::WlanFullmacBandCapability {
        banjo_wlan_fullmac::WlanFullmacBandCapability {
            band: banjo_wlan_common::WlanBand::TWO_GHZ,
            basic_rate_count: 0,
            basic_rate_list: [0u8; 12],
            ht_supported: false,
            ht_caps: banjo_wlan_ieee80211::HtCapabilities { bytes: [0u8; 26] },
            vht_supported: false,
            vht_caps: banjo_wlan_ieee80211::VhtCapabilities { bytes: [0u8; 12] },
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
                captured_driver_calls: vec![],
                start_fn_status_mock: None,
                query_device_info_mock: banjo_wlan_fullmac::WlanFullmacQueryInfo {
                    sta_addr: [0u8; 6],
                    role: banjo_wlan_common::WlanMacRole::CLIENT,
                    features: 0,
                    band_cap_list: [dummy_band_cap(); 16],
                    band_cap_count: 0,
                },
                query_mac_sublayer_support_mock: banjo_wlan_common::MacSublayerSupport {
                    rate_selection_offload: banjo_wlan_common::RateSelectionOffloadExtension {
                        supported: false,
                    },
                    data_plane: banjo_wlan_common::DataPlaneExtension {
                        data_plane_type: banjo_wlan_common::DataPlaneType::GENERIC_NETWORK_DEVICE,
                    },
                    device: banjo_wlan_common::DeviceExtension {
                        is_synthetic: true,
                        mac_implementation_type: banjo_wlan_common::MacImplementationType::FULLMAC,
                        tx_status_report_supported: false,
                    },
                },
                set_keys_resp_mock: None,
                get_iface_counter_stats_mock: None,
                get_iface_histogram_stats_mock: None,
            }
        }

        pub fn as_device(self: Pin<&mut Self>) -> FullmacDeviceInterface {
            FullmacDeviceInterface {
                device: self.get_mut() as *mut Self as *mut c_void,
                start: Self::start,
                query_device_info: Self::query_device_info,
                query_mac_sublayer_support: Self::query_mac_sublayer_support,
                query_security_support: Self::query_security_support,
                query_spectrum_management_support: Self::query_spectrum_management_support,
                start_scan: Self::start_scan,
                connect_req: Self::connect_req,
                reconnect_req: Self::reconnect_req,
                auth_resp: Self::auth_resp,
                deauth_req: Self::deauth_req,
                assoc_resp: Self::assoc_resp,
                disassoc_req: Self::disassoc_req,
                reset_req: Self::reset_req,
                start_req: Self::start_req,
                stop_req: Self::stop_req,
                set_keys_req: Self::set_keys_req,
                del_keys_req: Self::del_keys_req,
                eapol_req: Self::eapol_req,
                get_iface_counter_stats: Self::get_iface_counter_stats,
                get_iface_histogram_stats: Self::get_iface_histogram_stats,
                sae_handshake_resp: Self::sae_handshake_resp,
                sae_frame_tx: Self::sae_frame_tx,
                wmm_status_req: Self::wmm_status_req,
                on_link_state_changed: Self::on_link_state_changed,
            }
        }

        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn start(
            device: *mut c_void,
            _ifc: *const WlanFullmacIfcProtocol,
            out_sme_channel: *mut zx::sys::zx_handle_t,
        ) -> zx::sys::zx_status_t {
            let device = unsafe { &mut *(device as *mut Self) };
            let usme_bootstrap_server_end_handle =
                device.usme_bootstrap_server_end.as_ref().unwrap().channel().raw_handle();
            unsafe {
                *out_sme_channel = usme_bootstrap_server_end_handle;
            }
            match device.start_fn_status_mock {
                Some(status) => status,
                None => zx::sys::ZX_OK,
            }
        }

        pub extern "C" fn query_device_info(
            device: *mut c_void,
        ) -> banjo_wlan_fullmac::WlanFullmacQueryInfo {
            let device = unsafe { &mut *(device as *mut Self) };
            device.query_device_info_mock
        }

        pub extern "C" fn query_mac_sublayer_support(
            device: *mut c_void,
        ) -> banjo_wlan_common::MacSublayerSupport {
            let device = unsafe { &mut *(device as *mut Self) };
            device.query_mac_sublayer_support_mock
        }

        pub extern "C" fn query_security_support(
            _device: *mut c_void,
        ) -> banjo_wlan_common::SecuritySupport {
            banjo_wlan_common::SecuritySupport {
                sae: banjo_wlan_common::SaeFeature {
                    driver_handler_supported: false,
                    sme_handler_supported: true,
                },
                mfp: banjo_wlan_common::MfpFeature { supported: false },
            }
        }

        pub extern "C" fn query_spectrum_management_support(
            _device: *mut c_void,
        ) -> banjo_wlan_common::SpectrumManagementSupport {
            banjo_wlan_common::SpectrumManagementSupport {
                dfs: banjo_wlan_common::DfsFeature { supported: false },
            }
        }

        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn start_scan(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacScanReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            let channels =
                unsafe { slice::from_raw_parts(req.channels_list, req.channels_count) }.to_vec();
            let ssids = unsafe { slice::from_raw_parts(req.ssids_list, req.ssids_count) }.to_vec();
            device.captured_driver_calls.push(DriverCall::StartScan { req, channels, ssids });
        }

        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn connect_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacConnectReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            let selected_bss_ies = unsafe {
                slice::from_raw_parts(req.selected_bss.ies_list, req.selected_bss.ies_count)
            }
            .to_vec();
            let sae_password =
                unsafe { slice::from_raw_parts(req.sae_password_list, req.sae_password_count) }
                    .to_vec();
            let wep_key =
                unsafe { slice::from_raw_parts(req.wep_key.key_list, req.wep_key.key_count) }
                    .to_vec();
            let security_ie =
                unsafe { slice::from_raw_parts(req.security_ie_list, req.security_ie_count) }
                    .to_vec();

            device.captured_driver_calls.push(DriverCall::ConnectReq {
                req,
                selected_bss_ies,
                sae_password,
                wep_key,
                security_ie,
            });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn reconnect_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacReconnectReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::ReconnectReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn auth_resp(
            device: *mut c_void,
            resp: *mut banjo_wlan_fullmac::WlanFullmacAuthResp,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let resp = unsafe { *resp };
            device.captured_driver_calls.push(DriverCall::AuthResp { resp });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn deauth_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacDeauthReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::DeauthReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn assoc_resp(
            device: *mut c_void,
            resp: *mut banjo_wlan_fullmac::WlanFullmacAssocResp,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let resp = unsafe { *resp };
            device.captured_driver_calls.push(DriverCall::AssocResp { resp });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn disassoc_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacDisassocReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::DisassocReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn reset_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacResetReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::ResetReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn start_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacStartReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::StartReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn stop_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacStopReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::StopReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn set_keys_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacSetKeysReq,
        ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            let num_keys = req.num_keys;
            let mut keys = vec![];
            for i in 0..req.num_keys as usize {
                keys.push(
                    unsafe {
                        slice::from_raw_parts(req.keylist[i].key_list, req.keylist[i].key_count)
                    }
                    .to_vec(),
                );
            }
            device.captured_driver_calls.push(DriverCall::SetKeysReq { req, keys });
            match device.set_keys_resp_mock {
                Some(resp) => resp,
                None => {
                    banjo_wlan_fullmac::WlanFullmacSetKeysResp { num_keys, statuslist: [0i32; 4] }
                }
            }
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn del_keys_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacDelKeysReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            device.captured_driver_calls.push(DriverCall::DelKeysReq { req });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn eapol_req(
            device: *mut c_void,
            req: *mut banjo_wlan_fullmac::WlanFullmacEapolReq,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let req = unsafe { *req };
            let data = unsafe { slice::from_raw_parts(req.data_list, req.data_count) }.to_vec();
            device.captured_driver_calls.push(DriverCall::EapolReq { req, data });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn get_iface_counter_stats(
            device: *mut c_void,
            out_status: *mut i32,
        ) -> banjo_wlan_fullmac::WlanFullmacIfaceCounterStats {
            let device = unsafe { &mut *(device as *mut Self) };
            device.captured_driver_calls.push(DriverCall::GetIfaceCounterStats);
            match &device.get_iface_counter_stats_mock {
                Some((status, stats)) => {
                    unsafe { *out_status = *status };
                    stats.clone()
                }
                None => {
                    unsafe { *out_status = zx::sys::ZX_ERR_NOT_SUPPORTED };
                    banjo_wlan_fullmac::WlanFullmacIfaceCounterStats {
                        rx_unicast_drop: 1,
                        rx_unicast_total: 2,
                        rx_multicast: 3,
                        tx_total: 4,
                        tx_drop: 5,
                    }
                }
            }
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn get_iface_histogram_stats(
            device: *mut c_void,
            out_status: *mut i32,
        ) -> banjo_wlan_fullmac::WlanFullmacIfaceHistogramStats {
            let device = unsafe { &mut *(device as *mut Self) };
            device.captured_driver_calls.push(DriverCall::GetIfaceHistogramStats);
            match &device.get_iface_histogram_stats_mock {
                Some((status, stats)) => {
                    unsafe { *out_status = *status };
                    stats.clone()
                }
                None => {
                    unsafe { *out_status = zx::sys::ZX_ERR_NOT_SUPPORTED };
                    banjo_wlan_fullmac::WlanFullmacIfaceHistogramStats {
                        noise_floor_histograms_list: std::ptr::null(),
                        noise_floor_histograms_count: 0,
                        rssi_histograms_list: std::ptr::null(),
                        rssi_histograms_count: 0,
                        rx_rate_index_histograms_list: std::ptr::null(),
                        rx_rate_index_histograms_count: 0,
                        snr_histograms_list: std::ptr::null(),
                        snr_histograms_count: 0,
                    }
                }
            }
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn sae_handshake_resp(
            device: *mut c_void,
            resp: *mut banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let resp = unsafe { *resp };
            device.captured_driver_calls.push(DriverCall::SaeHandshakeResp { resp });
        }
        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn sae_frame_tx(
            device: *mut c_void,
            frame: *mut banjo_wlan_fullmac::WlanFullmacSaeFrame,
        ) {
            let device = unsafe { &mut *(device as *mut Self) };
            let frame = unsafe { *frame };
            let sae_fields =
                unsafe { slice::from_raw_parts(frame.sae_fields_list, frame.sae_fields_count) }
                    .to_vec();
            device.captured_driver_calls.push(DriverCall::SaeFrameTx { frame, sae_fields });
        }
        pub extern "C" fn wmm_status_req(device: *mut c_void) {
            let device = unsafe { &mut *(device as *mut Self) };
            device.captured_driver_calls.push(DriverCall::WmmStatusReq);
        }
        pub extern "C" fn on_link_state_changed(device: *mut c_void, online: bool) {
            let device = unsafe { &mut *(device as *mut Self) };
            device.captured_driver_calls.push(DriverCall::OnLinkStateChanged { online });
        }
    }
}
