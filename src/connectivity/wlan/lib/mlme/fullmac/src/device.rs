// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{convert::banjo_to_fidl, FullmacDriverEvent, FullmacDriverEventSink},
    banjo_fuchsia_wlan_common as banjo_wlan_common,
    banjo_fuchsia_wlan_fullmac as banjo_wlan_fullmac, fidl_fuchsia_wlan_mlme as fidl_mlme,
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
    fn start(&mut self, ifc: *const WlanFullmacIfcProtocol) -> Result<zx::Handle, zx::Status>;
    fn query_device_info(&mut self) -> banjo_wlan_fullmac::WlanFullmacQueryInfo;
    fn query_mac_sublayer_support(&mut self) -> banjo_wlan_common::MacSublayerSupport;
    fn query_security_support(&mut self) -> banjo_wlan_common::SecuritySupport;
    fn query_spectrum_management_support(&mut self)
        -> banjo_wlan_common::SpectrumManagementSupport;
    fn start_scan(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseStartScanRequest);
    fn connect(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseConnectRequest);
    fn reconnect(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseReconnectRequest);
    fn auth_resp(&mut self, resp: banjo_wlan_fullmac::WlanFullmacImplBaseAuthRespRequest);
    fn deauth(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseDeauthRequest);
    fn assoc_resp(&mut self, resp: banjo_wlan_fullmac::WlanFullmacImplBaseAssocRespRequest);
    fn disassoc(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseDisassocRequest);
    fn reset(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseResetRequest);
    fn start_bss(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseStartBssRequest);
    fn stop_bss(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseStopBssRequest);
    fn set_keys_req(
        &mut self,
        req: banjo_wlan_fullmac::WlanFullmacSetKeysReq,
    ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp;
    fn del_keys_req(&mut self, req: banjo_wlan_fullmac::WlanFullmacDelKeysReq);
    fn eapol_tx(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseEapolTxRequest);
    fn get_iface_counter_stats(&mut self) -> fidl_mlme::GetIfaceCounterStatsResponse;
    fn get_iface_histogram_stats(&mut self) -> fidl_mlme::GetIfaceHistogramStatsResponse;
    fn sae_handshake_resp(&mut self, resp: banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp);
    fn sae_frame_tx(&mut self, frame: banjo_wlan_fullmac::WlanFullmacSaeFrame);
    fn wmm_status_req(&mut self);
    fn set_link_state(&mut self, controlled_port_state: fidl_mlme::ControlledPortState);
}

/// A `FullmacDeviceInterface` allows transmitting frames and MLME messages.
#[repr(C)]
pub struct RawFullmacDeviceInterface {
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

    start_scan: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseStartScanRequest,
    ),
    connect: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseConnectRequest,
    ),
    reconnect: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseReconnectRequest,
    ),
    auth_resp: extern "C" fn(
        device: *mut c_void,
        resp: *mut banjo_wlan_fullmac::WlanFullmacImplBaseAuthRespRequest,
    ),
    deauth: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseDeauthRequest,
    ),
    assoc_resp: extern "C" fn(
        device: *mut c_void,
        resp: *mut banjo_wlan_fullmac::WlanFullmacImplBaseAssocRespRequest,
    ),
    disassoc: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseDisassocRequest,
    ),
    reset: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseResetRequest,
    ),
    start_bss: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseStartBssRequest,
    ),
    stop_bss: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseStopBssRequest,
    ),
    set_keys_req: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacSetKeysReq,
    ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp,
    del_keys_req:
        extern "C" fn(device: *mut c_void, req: *mut banjo_wlan_fullmac::WlanFullmacDelKeysReq),
    eapol_tx: extern "C" fn(
        device: *mut c_void,
        req: *mut banjo_wlan_fullmac::WlanFullmacImplBaseEapolTxRequest,
    ),
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

// Our device is used inside a separate worker thread, so we force Rust to allow this.
unsafe impl Send for FullmacDevice {}

pub struct FullmacDevice {
    raw_device: RawFullmacDeviceInterface,
}

impl FullmacDevice {
    pub fn new(raw_device: RawFullmacDeviceInterface) -> FullmacDevice {
        FullmacDevice { raw_device }
    }
}

impl DeviceOps for FullmacDevice {
    fn start(&mut self, ifc: *const WlanFullmacIfcProtocol) -> Result<zx::Handle, zx::Status> {
        let mut out_channel = 0;
        let status =
            (self.raw_device.start)(self.raw_device.device, ifc, &mut out_channel as *mut u32);
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

    fn query_device_info(&mut self) -> banjo_wlan_fullmac::WlanFullmacQueryInfo {
        (self.raw_device.query_device_info)(self.raw_device.device)
    }

    fn query_mac_sublayer_support(&mut self) -> banjo_wlan_common::MacSublayerSupport {
        (self.raw_device.query_mac_sublayer_support)(self.raw_device.device)
    }

    fn query_security_support(&mut self) -> banjo_wlan_common::SecuritySupport {
        (self.raw_device.query_security_support)(self.raw_device.device)
    }

    fn query_spectrum_management_support(
        &mut self,
    ) -> banjo_wlan_common::SpectrumManagementSupport {
        (self.raw_device.query_spectrum_management_support)(self.raw_device.device)
    }

    fn start_scan(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseStartScanRequest) {
        (self.raw_device.start_scan)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseStartScanRequest,
        )
    }
    fn connect(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseConnectRequest) {
        (self.raw_device.connect)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseConnectRequest,
        )
    }
    fn reconnect(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseReconnectRequest) {
        (self.raw_device.reconnect)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseReconnectRequest,
        )
    }
    fn auth_resp(&mut self, mut resp: banjo_wlan_fullmac::WlanFullmacImplBaseAuthRespRequest) {
        (self.raw_device.auth_resp)(
            self.raw_device.device,
            &mut resp as *mut banjo_wlan_fullmac::WlanFullmacImplBaseAuthRespRequest,
        )
    }
    fn deauth(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseDeauthRequest) {
        (self.raw_device.deauth)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseDeauthRequest,
        )
    }
    fn assoc_resp(&mut self, mut resp: banjo_wlan_fullmac::WlanFullmacImplBaseAssocRespRequest) {
        (self.raw_device.assoc_resp)(
            self.raw_device.device,
            &mut resp as *mut banjo_wlan_fullmac::WlanFullmacImplBaseAssocRespRequest,
        )
    }
    fn disassoc(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseDisassocRequest) {
        (self.raw_device.disassoc)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseDisassocRequest,
        )
    }
    fn reset(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseResetRequest) {
        (self.raw_device.reset)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseResetRequest,
        )
    }
    fn start_bss(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseStartBssRequest) {
        (self.raw_device.start_bss)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseStartBssRequest,
        )
    }
    fn stop_bss(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseStopBssRequest) {
        (self.raw_device.stop_bss)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseStopBssRequest,
        )
    }
    fn set_keys_req(
        &mut self,
        mut req: banjo_wlan_fullmac::WlanFullmacSetKeysReq,
    ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp {
        (self.raw_device.set_keys_req)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacSetKeysReq,
        )
    }
    fn del_keys_req(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacDelKeysReq) {
        (self.raw_device.del_keys_req)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacDelKeysReq,
        )
    }
    fn eapol_tx(&mut self, mut req: banjo_wlan_fullmac::WlanFullmacImplBaseEapolTxRequest) {
        (self.raw_device.eapol_tx)(
            self.raw_device.device,
            &mut req as *mut banjo_wlan_fullmac::WlanFullmacImplBaseEapolTxRequest,
        )
    }
    fn get_iface_counter_stats(&mut self) -> fidl_mlme::GetIfaceCounterStatsResponse {
        let mut out_status: i32 = 0;
        let stats = (self.raw_device.get_iface_counter_stats)(
            self.raw_device.device,
            &mut out_status as *mut i32,
        );
        if out_status == zx::sys::ZX_OK {
            fidl_mlme::GetIfaceCounterStatsResponse::Stats(
                banjo_to_fidl::convert_iface_counter_stats(stats),
            )
        } else {
            fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(out_status)
        }
    }
    fn get_iface_histogram_stats(&mut self) -> fidl_mlme::GetIfaceHistogramStatsResponse {
        let mut out_status: i32 = 0;
        let stats = (self.raw_device.get_iface_histogram_stats)(
            self.raw_device.device,
            &mut out_status as *mut i32,
        );
        if out_status == zx::sys::ZX_OK {
            fidl_mlme::GetIfaceHistogramStatsResponse::Stats(
                banjo_to_fidl::convert_iface_histogram_stats(stats),
            )
        } else {
            fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(out_status)
        }
    }
    fn sae_handshake_resp(&mut self, mut resp: banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp) {
        (self.raw_device.sae_handshake_resp)(
            self.raw_device.device,
            &mut resp as *mut banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp,
        )
    }
    fn sae_frame_tx(&mut self, mut frame: banjo_wlan_fullmac::WlanFullmacSaeFrame) {
        (self.raw_device.sae_frame_tx)(
            self.raw_device.device,
            &mut frame as *mut banjo_wlan_fullmac::WlanFullmacSaeFrame,
        )
    }
    fn wmm_status_req(&mut self) {
        (self.raw_device.wmm_status_req)(self.raw_device.device)
    }
    fn set_link_state(&mut self, controlled_port_state: fidl_mlme::ControlledPortState) {
        let online = match controlled_port_state {
            fidl_mlme::ControlledPortState::Open => true,
            fidl_mlme::ControlledPortState::Closed => false,
        };
        (self.raw_device.on_link_state_changed)(self.raw_device.device, online)
    }
}

#[cfg(test)]
pub mod test_utils {
    use {
        super::*,
        banjo_fuchsia_wlan_ieee80211 as banjo_wlan_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
        std::{
            slice,
            sync::{Arc, Mutex},
        },
    };

    #[derive(Debug)]
    pub enum DriverCall {
        StartScan {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseStartScanRequest,
            channels: Vec<u8>,
            ssids: Vec<banjo_wlan_ieee80211::CSsid>,
        },
        ConnectReq {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseConnectRequest,
            selected_bss_ies: Vec<u8>,
            sae_password: Vec<u8>,
            wep_key: Vec<u8>,
            security_ie: Vec<u8>,
        },
        ReconnectReq {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseReconnectRequest,
        },
        AuthResp {
            resp: banjo_wlan_fullmac::WlanFullmacImplBaseAuthRespRequest,
        },
        DeauthReq {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseDeauthRequest,
        },
        AssocResp {
            resp: banjo_wlan_fullmac::WlanFullmacImplBaseAssocRespRequest,
        },
        Disassoc {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseDisassocRequest,
        },
        Reset {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseResetRequest,
        },
        StartBss {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseStartBssRequest,
        },
        StopBss {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseStopBssRequest,
        },
        SetKeysReq {
            req: banjo_wlan_fullmac::WlanFullmacSetKeysReq,
            keys: Vec<Vec<u8>>,
        },
        DelKeysReq {
            req: banjo_wlan_fullmac::WlanFullmacDelKeysReq,
        },
        EapolTx {
            req: banjo_wlan_fullmac::WlanFullmacImplBaseEapolTxRequest,
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

    pub struct FakeFullmacDeviceMocks {
        pub captured_driver_calls: Vec<DriverCall>,
        pub start_fn_status_mock: Option<zx::sys::zx_status_t>,
        pub query_device_info_mock: banjo_wlan_fullmac::WlanFullmacQueryInfo,
        pub query_mac_sublayer_support_mock: banjo_wlan_common::MacSublayerSupport,
        pub set_keys_resp_mock: Option<banjo_wlan_fullmac::WlanFullmacSetKeysResp>,
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
                mocks: Arc::new(Mutex::new(FakeFullmacDeviceMocks {
                    captured_driver_calls: vec![],
                    start_fn_status_mock: None,
                    query_device_info_mock: banjo_wlan_fullmac::WlanFullmacQueryInfo {
                        sta_addr: [0u8; 6],
                        role: banjo_wlan_common::WlanMacRole::CLIENT,
                        band_cap_list: [dummy_band_cap(); 16],
                        band_cap_count: 0,
                    },
                    query_mac_sublayer_support_mock: banjo_wlan_common::MacSublayerSupport {
                        rate_selection_offload: banjo_wlan_common::RateSelectionOffloadExtension {
                            supported: false,
                        },
                        data_plane: banjo_wlan_common::DataPlaneExtension {
                            data_plane_type:
                                banjo_wlan_common::DataPlaneType::GENERIC_NETWORK_DEVICE,
                        },
                        device: banjo_wlan_common::DeviceExtension {
                            is_synthetic: true,
                            mac_implementation_type:
                                banjo_wlan_common::MacImplementationType::FULLMAC,
                            tx_status_report_supported: false,
                        },
                    },
                    set_keys_resp_mock: None,
                    get_iface_counter_stats_mock: None,
                    get_iface_histogram_stats_mock: None,
                })),
            }
        }
    }

    impl DeviceOps for FakeFullmacDevice {
        fn start(&mut self, _ifc: *const WlanFullmacIfcProtocol) -> Result<zx::Handle, zx::Status> {
            match self.mocks.lock().unwrap().start_fn_status_mock {
                Some(status) => Err(zx::Status::from_raw(status)),

                // Start can only be called once since this moves usme_bootstrap_server_end.
                None => Ok(self.usme_bootstrap_server_end.take().unwrap().into_channel().into()),
            }
        }

        fn query_device_info(&mut self) -> banjo_wlan_fullmac::WlanFullmacQueryInfo {
            self.mocks.lock().unwrap().query_device_info_mock
        }

        fn query_mac_sublayer_support(&mut self) -> banjo_wlan_common::MacSublayerSupport {
            self.mocks.lock().unwrap().query_mac_sublayer_support_mock
        }

        fn query_security_support(&mut self) -> banjo_wlan_common::SecuritySupport {
            banjo_wlan_common::SecuritySupport {
                sae: banjo_wlan_common::SaeFeature {
                    driver_handler_supported: false,
                    sme_handler_supported: true,
                },
                mfp: banjo_wlan_common::MfpFeature { supported: false },
            }
        }

        fn query_spectrum_management_support(
            &mut self,
        ) -> banjo_wlan_common::SpectrumManagementSupport {
            banjo_wlan_common::SpectrumManagementSupport {
                dfs: banjo_wlan_common::DfsFeature { supported: false },
            }
        }

        // Cannot mark fn unsafe because it has to match fn signature in FullDeviceInterface
        fn start_scan(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseStartScanRequest) {
            let channels =
                unsafe { slice::from_raw_parts(req.channels_list, req.channels_count) }.to_vec();
            let ssids = unsafe { slice::from_raw_parts(req.ssids_list, req.ssids_count) }.to_vec();
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::StartScan {
                req,
                channels,
                ssids,
            });
        }

        fn connect(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseConnectRequest) {
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

            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::ConnectReq {
                req,
                selected_bss_ies,
                sae_password,
                wep_key,
                security_ie,
            });
        }
        fn reconnect(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseReconnectRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::ReconnectReq { req });
        }
        fn auth_resp(&mut self, resp: banjo_wlan_fullmac::WlanFullmacImplBaseAuthRespRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::AuthResp { resp });
        }
        fn deauth(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseDeauthRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::DeauthReq { req });
        }
        fn assoc_resp(&mut self, resp: banjo_wlan_fullmac::WlanFullmacImplBaseAssocRespRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::AssocResp { resp });
        }
        fn disassoc(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseDisassocRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::Disassoc { req });
        }
        fn reset(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseResetRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::Reset { req });
        }
        fn start_bss(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseStartBssRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::StartBss { req });
        }
        fn stop_bss(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseStopBssRequest) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::StopBss { req });
        }
        fn set_keys_req(
            &mut self,
            req: banjo_wlan_fullmac::WlanFullmacSetKeysReq,
        ) -> banjo_wlan_fullmac::WlanFullmacSetKeysResp {
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
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::SetKeysReq { req, keys });
            match self.mocks.lock().unwrap().set_keys_resp_mock {
                Some(resp) => resp,
                None => {
                    banjo_wlan_fullmac::WlanFullmacSetKeysResp { num_keys, statuslist: [0i32; 4] }
                }
            }
        }
        fn del_keys_req(&mut self, req: banjo_wlan_fullmac::WlanFullmacDelKeysReq) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::DelKeysReq { req });
        }
        fn eapol_tx(&mut self, req: banjo_wlan_fullmac::WlanFullmacImplBaseEapolTxRequest) {
            let data = unsafe { slice::from_raw_parts(req.data_list, req.data_count) }.to_vec();
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::EapolTx { req, data });
        }
        fn get_iface_counter_stats(&mut self) -> fidl_mlme::GetIfaceCounterStatsResponse {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::GetIfaceCounterStats);
            self.mocks.lock().unwrap().get_iface_counter_stats_mock.clone().unwrap_or(
                fidl_mlme::GetIfaceCounterStatsResponse::ErrorStatus(zx::sys::ZX_ERR_NOT_SUPPORTED),
            )
        }
        fn get_iface_histogram_stats(&mut self) -> fidl_mlme::GetIfaceHistogramStatsResponse {
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::GetIfaceHistogramStats);
            self.mocks.lock().unwrap().get_iface_histogram_stats_mock.clone().unwrap_or(
                fidl_mlme::GetIfaceHistogramStatsResponse::ErrorStatus(
                    zx::sys::ZX_ERR_NOT_SUPPORTED,
                ),
            )
        }
        fn sae_handshake_resp(&mut self, resp: banjo_wlan_fullmac::WlanFullmacSaeHandshakeResp) {
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::SaeHandshakeResp { resp });
        }
        fn sae_frame_tx(&mut self, frame: banjo_wlan_fullmac::WlanFullmacSaeFrame) {
            let sae_fields =
                unsafe { slice::from_raw_parts(frame.sae_fields_list, frame.sae_fields_count) }
                    .to_vec();
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::SaeFrameTx { frame, sae_fields });
        }
        fn wmm_status_req(&mut self) {
            self.mocks.lock().unwrap().captured_driver_calls.push(DriverCall::WmmStatusReq);
        }
        fn set_link_state(&mut self, controlled_port_state: fidl_mlme::ControlledPortState) {
            let online = match controlled_port_state {
                fidl_mlme::ControlledPortState::Open => true,
                fidl_mlme::ControlledPortState::Closed => false,
            };
            self.mocks
                .lock()
                .unwrap()
                .captured_driver_calls
                .push(DriverCall::OnLinkStateChanged { online });
        }
    }
}
