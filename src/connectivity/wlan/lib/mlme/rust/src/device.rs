// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{banjo_buffer_to_slice, banjo_list_to_slice, buffer::OutBuf, common::mac::WlanGi, key},
    banjo_fuchsia_hardware_wlan_associnfo as banjo_wlan_associnfo,
    banjo_fuchsia_wlan_common::{self as banjo_common, WlanTxStatus},
    banjo_fuchsia_wlan_ieee80211 as banjo_ieee80211,
    banjo_fuchsia_wlan_internal::JoinBssRequest,
    banjo_fuchsia_wlan_softmac::{
        self as banjo_wlan_softmac, WlanRxPacket, WlanSoftmacQueryResponse, WlanTxPacket,
    },
    fidl_fuchsia_wlan_mlme as fidl_mlme, fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_zircon as zx,
    futures::channel::mpsc,
    ieee80211::MacAddr,
    std::{ffi::c_void, sync::Arc},
    tracing::error,
    wlan_common::{mac::FrameControl, tx_vector, TimeUnit},
};

#[cfg(test)]
pub use test_utils::*;

#[derive(Debug, PartialEq)]
pub struct LinkStatus(u32);
impl LinkStatus {
    pub const DOWN: Self = Self(0);
    pub const UP: Self = Self(1);
}

impl From<fidl_mlme::ControlledPortState> for LinkStatus {
    fn from(state: fidl_mlme::ControlledPortState) -> Self {
        match state {
            fidl_mlme::ControlledPortState::Open => Self::UP,
            fidl_mlme::ControlledPortState::Closed => Self::DOWN,
        }
    }
}

pub struct Device {
    raw_device: DeviceInterface,
    wlan_softmac_bridge_proxy: fidl_softmac::WlanSoftmacBridgeSynchronousProxy,
    minstrel: Option<crate::MinstrelWrapper>,
    event_receiver: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
    event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
}

unsafe impl Send for Device {}

impl Device {
    pub fn new(
        raw_device: DeviceInterface,
        wlan_softmac_bridge_proxy: fidl_softmac::WlanSoftmacBridgeSynchronousProxy,
    ) -> Device {
        let (event_sink, event_receiver) = mpsc::unbounded();
        Device {
            raw_device,
            wlan_softmac_bridge_proxy,
            minstrel: None,
            event_receiver: Some(event_receiver),
            event_sink,
        }
    }
}

const REQUIRED_WLAN_HEADER_LEN: usize = 10;
const PEER_ADDR_OFFSET: usize = 4;

/// This trait abstracts how Device accomplish operations. Test code
/// can then implement trait methods instead of mocking an underlying DeviceInterface
/// and FIDL proxy.
pub trait DeviceOps {
    fn start(&mut self, ifc: *const WlanSoftmacIfcProtocol<'_>) -> Result<zx::Handle, zx::Status>;
    fn wlan_softmac_query_response(&mut self) -> QueryResponse;
    fn discovery_support(&mut self) -> banjo_common::DiscoverySupport;
    fn mac_sublayer_support(&mut self) -> banjo_common::MacSublayerSupport;
    fn security_support(&mut self) -> banjo_common::SecuritySupport;
    fn spectrum_management_support(&mut self) -> banjo_common::SpectrumManagementSupport;
    fn deliver_eth_frame(&mut self, slice: &[u8]) -> Result<(), zx::Status>;
    fn send_wlan_frame(&mut self, buf: OutBuf, tx_flags: u32) -> Result<(), zx::Status>;

    fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status>;
    fn set_ethernet_up(&mut self) -> Result<(), zx::Status> {
        self.set_ethernet_status(LinkStatus::UP)
    }
    fn set_ethernet_down(&mut self) -> Result<(), zx::Status> {
        self.set_ethernet_status(LinkStatus::DOWN)
    }

    fn set_channel(&mut self, channel: banjo_common::WlanChannel) -> Result<(), zx::Status>;
    fn channel(&mut self) -> banjo_common::WlanChannel;
    fn set_key(&mut self, key: key::KeyConfig) -> Result<(), zx::Status>;
    fn start_passive_scan(&mut self, passive_scan_args: PassiveScanArgs)
        -> Result<u64, zx::Status>;
    fn start_active_scan(&mut self, active_scan_args: ActiveScanArgs) -> Result<u64, zx::Status>;
    fn cancel_scan(&mut self, scan_id: u64) -> Result<(), zx::Status>;
    fn join_bss(&mut self, cfg: JoinBssRequest) -> Result<(), zx::Status>;
    fn enable_beaconing(
        &mut self,
        buf: OutBuf,
        tim_ele_offset: usize,
        beacon_interval: TimeUnit,
    ) -> Result<(), zx::Status>;
    fn disable_beaconing(&mut self) -> Result<(), zx::Status>;
    fn notify_association_complete(
        &mut self,
        assoc_cfg: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status>;
    fn clear_association(&mut self, addr: &MacAddr) -> Result<(), zx::Status>;
    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>;
    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error>;
    fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper);
    fn minstrel(&mut self) -> Option<crate::MinstrelWrapper>;
    fn tx_vector_idx(
        &mut self,
        frame_control: &FrameControl,
        peer_addr: &[u8; 6],
        flags: u32,
    ) -> tx_vector::TxVecIdx {
        self.minstrel()
            .as_ref()
            .and_then(|minstrel| {
                minstrel.lock().get_tx_vector_idx(frame_control, &peer_addr, flags)
            })
            .unwrap_or_else(|| {
                // We either don't have minstrel, or minstrel failed to generate a tx vector.
                // Use a reasonable default value instead.
                // Note: This is only effective if the underlying device meets both criteria below:
                // 1. Does not support tx status report.
                // 2. Honors our instruction on tx_vector to use.
                // TODO(fxbug.dev/28893): Choose an optimal MCS for management frames
                // TODO(fxbug.dev/43456): Log stats about minstrel usage vs default tx vector.
                let mcs_idx = if frame_control.is_data() { 7 } else { 3 };
                tx_vector::TxVector::new(
                    banjo_common::WlanPhyType::ERP,
                    WlanGi::G_800NS,
                    banjo_common::ChannelBandwidth::CBW20,
                    mcs_idx,
                )
                .unwrap()
                .to_idx()
            })
    }
}

impl DeviceOps for Device {
    fn start(&mut self, ifc: *const WlanSoftmacIfcProtocol<'_>) -> Result<zx::Handle, zx::Status> {
        let mut out_channel = 0;
        let status =
            (self.raw_device.start)(self.raw_device.device, ifc, &mut out_channel as *mut u32);
        // Unsafe block required because we cannot pass a Rust handle over FFI. An invalid
        // handle violates the banjo API, and may be detected by the caller of this fn.
        zx::ok(status).map(|_| unsafe { zx::Handle::from_raw(out_channel) })
    }

    fn wlan_softmac_query_response(&mut self) -> QueryResponse {
        (self.raw_device.get_wlan_softmac_query_response)(self.raw_device.device).into()
    }

    fn discovery_support(&mut self) -> banjo_common::DiscoverySupport {
        (self.raw_device.get_discovery_support)(self.raw_device.device)
    }

    fn mac_sublayer_support(&mut self) -> banjo_common::MacSublayerSupport {
        (self.raw_device.get_mac_sublayer_support)(self.raw_device.device)
    }

    fn security_support(&mut self) -> banjo_common::SecuritySupport {
        (self.raw_device.get_security_support)(self.raw_device.device)
    }

    fn spectrum_management_support(&mut self) -> banjo_common::SpectrumManagementSupport {
        (self.raw_device.get_spectrum_management_support)(self.raw_device.device)
    }

    fn deliver_eth_frame(&mut self, slice: &[u8]) -> Result<(), zx::Status> {
        let status = (self.raw_device.deliver_eth_frame)(
            self.raw_device.device,
            slice.as_ptr(),
            slice.len(),
        );
        zx::ok(status)
    }

    fn send_wlan_frame(&mut self, buf: OutBuf, mut tx_flags: u32) -> Result<(), zx::Status> {
        if buf.as_slice().len() < REQUIRED_WLAN_HEADER_LEN {
            return Err(zx::Status::BUFFER_TOO_SMALL);
        }
        // Unwrap is safe since the byte slice is always the same size.
        let frame_control =
            zerocopy::LayoutVerified::<&[u8], FrameControl>::new(&buf.as_slice()[0..=1])
                .unwrap()
                .into_ref();
        if frame_control.protected() {
            tx_flags |= banjo_wlan_softmac::WlanTxInfoFlags::PROTECTED.0;
        }
        let mut peer_addr = [0u8; 6];
        peer_addr.copy_from_slice(&buf.as_slice()[PEER_ADDR_OFFSET..PEER_ADDR_OFFSET + 6]);
        let tx_vector_idx = self.tx_vector_idx(frame_control, &peer_addr, tx_flags);

        let tx_info = wlan_common::tx_vector::TxVector::from_idx(tx_vector_idx)
            .to_banjo_tx_info(tx_flags, self.minstrel.is_some());
        zx::ok((self.raw_device.queue_tx)(self.raw_device.device, 0, buf, tx_info))
    }

    fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.set_ethernet_status)(self.raw_device.device, status.0))
    }

    fn set_channel(&mut self, channel: banjo_common::WlanChannel) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.set_wlan_channel)(self.raw_device.device, channel))
    }

    fn set_key(&mut self, key: key::KeyConfig) -> Result<(), zx::Status> {
        let mut banjo_key = banjo_wlan_softmac::WlanKeyConfiguration {
            protection: match key.protection {
                key::Protection::NONE => banjo_wlan_softmac::WlanProtection::NONE,
                key::Protection::RX => banjo_wlan_softmac::WlanProtection::RX,
                key::Protection::TX => banjo_wlan_softmac::WlanProtection::TX,
                key::Protection::RX_TX => banjo_wlan_softmac::WlanProtection::RX_TX,
                _ => return Err(zx::Status::INVALID_ARGS),
            },
            cipher_oui: key.cipher_oui,
            cipher_type: key.cipher_type,
            key_type: match key.key_type {
                key::KeyType::PAIRWISE => banjo_wlan_associnfo::WlanKeyType::PAIRWISE,
                key::KeyType::GROUP => banjo_wlan_associnfo::WlanKeyType::GROUP,
                key::KeyType::IGTK => banjo_wlan_associnfo::WlanKeyType::IGTK,
                key::KeyType::PEER => banjo_wlan_associnfo::WlanKeyType::PEER,
                _ => return Err(zx::Status::INVALID_ARGS),
            },
            peer_addr: key.peer_addr,
            key_idx: key.key_idx,
            key_list: key.key.as_ptr(),
            key_count: usize::from(key.key_len),
            rsc: key.rsc,
        };
        zx::ok((self.raw_device.set_key)(self.raw_device.device, &mut banjo_key))
    }

    fn start_passive_scan(
        &mut self,
        passive_scan_args: PassiveScanArgs,
    ) -> Result<u64, zx::Status> {
        let mut out_scan_id = 0;
        let passive_scan_request = StartPassiveScanRequest::from(passive_scan_args);
        let status = (self.raw_device.start_passive_scan)(
            self.raw_device.device,
            passive_scan_request.to_banjo_ptr(),
            &mut out_scan_id as *mut u64,
        );
        zx::ok(status).map(|()| out_scan_id)
    }

    fn start_active_scan(&mut self, active_scan_args: ActiveScanArgs) -> Result<u64, zx::Status> {
        let mut out_scan_id = 0;
        let active_scan_request = StartActiveScanRequest::from(active_scan_args);
        let status = (self.raw_device.start_active_scan)(
            self.raw_device.device,
            active_scan_request.to_banjo_ptr(),
            &mut out_scan_id as *mut u64,
        );
        zx::ok(status).map(|()| out_scan_id)
    }

    fn cancel_scan(&mut self, scan_id: u64) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.cancel_scan)(self.raw_device.device, scan_id))
    }

    fn channel(&mut self) -> banjo_common::WlanChannel {
        (self.raw_device.get_wlan_channel)(self.raw_device.device)
    }

    fn join_bss(&mut self, mut cfg: JoinBssRequest) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.join_bss)(self.raw_device.device, &mut cfg))
    }

    fn enable_beaconing(
        &mut self,
        buf: OutBuf,
        tim_ele_offset: usize,
        beacon_interval: TimeUnit,
    ) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.enable_beaconing)(
            self.raw_device.device,
            buf,
            tim_ele_offset,
            beacon_interval.0,
        ))
    }

    fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.disable_beaconing)(self.raw_device.device))
    }

    fn clear_association(&mut self, addr: &MacAddr) -> Result<(), zx::Status> {
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().remove_peer(addr);
        }
        zx::ok((self.raw_device.clear_association)(self.raw_device.device, addr))
    }

    fn notify_association_complete(
        &mut self,
        assoc_cfg: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().add_peer(&assoc_cfg)?;
        }
        self.wlan_softmac_bridge_proxy
            .notify_association_complete(&assoc_cfg, zx::Time::INFINITE)
            .map_err(|fidl_error| {
                error!("FIDL error during ConfigureAssoc: {:?}", fidl_error);
                zx::Status::INTERNAL
            })?
            .map_err(|e| zx::Status::from_raw(e))
    }

    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
        self.event_receiver.take()
    }

    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
        self.event_sink.unbounded_send(event).map_err(|e| e.into())
    }

    fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper) {
        self.minstrel.replace(minstrel);
    }

    fn minstrel(&mut self) -> Option<crate::MinstrelWrapper> {
        self.minstrel.as_ref().map(Arc::clone)
    }
}

/// Hand-rolled Rust version of the banjo wlan_softmac_ifc_protocol for communication from the driver up.
/// Note that we copy the individual fns out of this struct into the equivalent generated struct
/// in C++. Thanks to cbindgen, this gives us a compile-time confirmation that our function
/// signatures are correct.
#[repr(C)]
pub struct WlanSoftmacIfcProtocol<'a> {
    ops: *const WlanSoftmacIfcProtocolOps,
    ctx: &'a mut crate::DriverEventSink,
}

#[repr(C)]
pub struct WlanSoftmacIfcProtocolOps {
    recv: extern "C" fn(ctx: &mut crate::DriverEventSink, packet: *const WlanRxPacket),
    complete_tx: extern "C" fn(
        ctx: &'static mut crate::DriverEventSink,
        packet: *const WlanTxPacket,
        status: i32,
    ),
    report_tx_status:
        extern "C" fn(ctx: &mut crate::DriverEventSink, tx_status: *const WlanTxStatus),
    scan_complete: extern "C" fn(ctx: &mut crate::DriverEventSink, status: i32, scan_id: u64),
}

#[no_mangle]
extern "C" fn handle_recv(ctx: &mut crate::DriverEventSink, packet: *const WlanRxPacket) {
    // TODO(fxbug.dev/29063): C++ uses a buffer allocator for this, determine if we need one.
    let bytes =
        unsafe { std::slice::from_raw_parts((*packet).mac_frame_buffer, (*packet).mac_frame_size) }
            .into();
    let rx_info = unsafe { (*packet).info };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::MacFrameRx { bytes, rx_info });
}
#[no_mangle]
extern "C" fn handle_complete_tx(
    _ctx: &mut crate::DriverEventSink,
    _packet: *const WlanTxPacket,
    _status: i32,
) {
    // TODO(fxbug.dev/85924): Implement this to support asynchronous packet delivery.
}
#[no_mangle]
extern "C" fn handle_report_tx_status(
    ctx: &mut crate::DriverEventSink,
    tx_status: *const WlanTxStatus,
) {
    if tx_status.is_null() {
        return;
    }
    let tx_status = unsafe { *tx_status };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::TxStatusReport { tx_status });
}
#[no_mangle]
extern "C" fn handle_scan_complete(ctx: &mut crate::DriverEventSink, status: i32, scan_id: u64) {
    let _ = ctx.0.unbounded_send(crate::DriverEvent::ScanComplete {
        status: zx::Status::from_raw(status),
        scan_id,
    });
}

const PROTOCOL_OPS: WlanSoftmacIfcProtocolOps = WlanSoftmacIfcProtocolOps {
    recv: handle_recv,
    complete_tx: handle_complete_tx,
    report_tx_status: handle_report_tx_status,
    scan_complete: handle_scan_complete,
};

impl<'a> WlanSoftmacIfcProtocol<'a> {
    pub fn new(sink: &'a mut crate::DriverEventSink) -> Self {
        // Const reference has 'static lifetime, so it's safe to pass down to the driver.
        let ops = &PROTOCOL_OPS;
        Self { ops, ctx: sink }
    }
}

#[cfg_attr(test, derive(Clone, Debug, PartialEq))]
pub struct PassiveScanArgs {
    pub channels: Vec<u8>,
    pub min_channel_time: zx::sys::zx_duration_t,
    pub max_channel_time: zx::sys::zx_duration_t,
    pub min_home_time: zx::sys::zx_duration_t,
}

impl From<banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest> for PassiveScanArgs {
    fn from(banjo_args: banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest) -> PassiveScanArgs {
        PassiveScanArgs {
            channels: banjo_list_to_slice!(banjo_args, channels).to_vec(),
            min_channel_time: banjo_args.min_channel_time,
            max_channel_time: banjo_args.max_channel_time,
            min_home_time: banjo_args.min_home_time,
        }
    }
}

#[cfg_attr(test, derive(Clone, Debug, PartialEq))]
pub struct ActiveScanArgs {
    pub min_channel_time: zx::sys::zx_duration_t,
    pub max_channel_time: zx::sys::zx_duration_t,
    pub min_home_time: zx::sys::zx_duration_t,
    pub min_probes_per_channel: u8,
    pub max_probes_per_channel: u8,
    pub ssids_list: Vec<banjo_ieee80211::CSsid>,
    pub mac_header: Vec<u8>,
    pub channels: Vec<u8>,
    pub ies: Vec<u8>,
}

impl From<banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest> for ActiveScanArgs {
    fn from(banjo_args: banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest) -> ActiveScanArgs {
        ActiveScanArgs {
            min_channel_time: banjo_args.min_channel_time,
            max_channel_time: banjo_args.max_channel_time,
            min_home_time: banjo_args.min_home_time,
            min_probes_per_channel: banjo_args.min_probes_per_channel,
            max_probes_per_channel: banjo_args.max_probes_per_channel,
            ssids_list: banjo_list_to_slice!(banjo_args, ssids).to_vec(),
            channels: banjo_list_to_slice!(banjo_args, channels).to_vec(),
            mac_header: banjo_buffer_to_slice!(banjo_args, mac_header).to_vec(),
            ies: banjo_buffer_to_slice!(banjo_args, ies).to_vec(),
        }
    }
}

mod convert {
    use {
        super::*, crate::ddk_converter::convert_ddk_band_cap, anyhow::format_err,
        fidl_fuchsia_wlan_common as fidl_common,
    };

    pub struct QueryResponse {
        inner: banjo_wlan_softmac::WlanSoftmacQueryResponse,
        supported_phys: Vec<banjo_common::WlanPhyType>,
        band_caps: Vec<banjo_wlan_softmac::WlanSoftmacBandCapability>,
    }

    unsafe impl Send for QueryResponse {}
    impl Clone for QueryResponse {
        fn clone(&self) -> Self {
            (*self.as_banjo()).into()
        }
    }

    impl QueryResponse {
        pub fn sta_addr(&self) -> MacAddr {
            self.inner.sta_addr
        }

        pub fn mac_role(&self) -> banjo_common::WlanMacRole {
            self.inner.mac_role
        }

        pub fn supported_phys(&self) -> &Vec<banjo_common::WlanPhyType> {
            &self.supported_phys
        }

        pub fn hardware_capability(&self) -> u32 {
            self.inner.hardware_capability
        }

        pub fn band_caps(&self) -> &Vec<banjo_wlan_softmac::WlanSoftmacBandCapability> {
            &self.band_caps
        }

        pub fn as_banjo(&self) -> &banjo_wlan_softmac::WlanSoftmacQueryResponse {
            &self.inner
        }

        pub(crate) fn fake() -> Self {
            let mut supported_phys = vec![
                banjo_common::WlanPhyType::DSSS,
                banjo_common::WlanPhyType::HR,
                banjo_common::WlanPhyType::OFDM,
                banjo_common::WlanPhyType::ERP,
                banjo_common::WlanPhyType::HT,
                banjo_common::WlanPhyType::VHT,
            ];
            let mut band_caps = test_utils::fake_band_caps();
            Self {
                inner: banjo_wlan_softmac::WlanSoftmacQueryResponse {
                    sta_addr: [7u8; 6],
                    mac_role: banjo_common::WlanMacRole::CLIENT,
                    supported_phys_list: supported_phys.as_mut_ptr(),
                    supported_phys_count: supported_phys.len(),
                    hardware_capability: 0,
                    band_caps_list: band_caps.as_mut_ptr(),
                    band_caps_count: band_caps.len(),
                },
                supported_phys,
                band_caps,
            }
        }

        pub(crate) fn with_mac_role(mut self, mac_role: banjo_common::WlanMacRole) -> Self {
            self.inner.mac_role = mac_role;
            self
        }

        pub(crate) fn with_sta_addr(mut self, sta_addr: MacAddr) -> Self {
            self.inner.sta_addr = sta_addr;
            self
        }
    }

    impl From<banjo_wlan_softmac::WlanSoftmacQueryResponse> for QueryResponse {
        fn from(mut query_response: banjo_wlan_softmac::WlanSoftmacQueryResponse) -> Self {
            let mut supported_phys = banjo_list_to_slice!(query_response, supported_phys).to_vec();
            query_response.supported_phys_list = supported_phys.as_mut_ptr();
            let mut band_caps = banjo_list_to_slice!(query_response, band_caps).to_vec();
            query_response.band_caps_list = band_caps.as_mut_ptr();

            Self { inner: query_response, supported_phys, band_caps }
        }
    }

    impl TryFrom<QueryResponse> for fidl_mlme::DeviceInfo {
        type Error = anyhow::Error;

        fn try_from(query_response: QueryResponse) -> Result<fidl_mlme::DeviceInfo, Self::Error> {
            (&query_response).try_into()
        }
    }

    impl TryFrom<&QueryResponse> for fidl_mlme::DeviceInfo {
        type Error = anyhow::Error;

        fn try_from(query_response: &QueryResponse) -> Result<fidl_mlme::DeviceInfo, Self::Error> {
            let mut bands = vec![];
            for band_cap in query_response.band_caps() {
                bands.push(convert_ddk_band_cap(&band_cap)?)
            }
            Ok(fidl_mlme::DeviceInfo {
                sta_addr: query_response.sta_addr(),
                role: fidl_common::WlanMacRole::from_primitive(query_response.mac_role().0).ok_or(
                    format_err!("Unknown WlanWlanMacRole: {}", query_response.mac_role().0),
                )?,
                bands,
                qos_capable: false,
                softmac_hardware_capability: query_response.hardware_capability(),
            })
        }
    }

    pub struct StartPassiveScanRequest {
        banjo_args: banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest,
        _args: PassiveScanArgs,
    }

    impl StartPassiveScanRequest {
        #[cfg(test)]
        pub fn as_banjo(&self) -> &banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest {
            &self.banjo_args
        }

        /// Returns a raw pointer to the `banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest`
        /// that `StartPassiveScanRequest` owns. The caller must ensure that the
        /// `StartPassiveScanRequest` outlives the pointer this function returns, or else it
        /// will end up pointing to garbage.
        pub fn to_banjo_ptr(
            &self,
        ) -> *const banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest {
            &self.banjo_args as *const banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest
        }
    }

    impl From<PassiveScanArgs> for StartPassiveScanRequest {
        fn from(passive_scan_args: PassiveScanArgs) -> StartPassiveScanRequest {
            StartPassiveScanRequest {
                banjo_args: banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest {
                    channels_list: passive_scan_args.channels.as_ptr(),
                    channels_count: passive_scan_args.channels.len(),
                    min_channel_time: passive_scan_args.min_channel_time,
                    max_channel_time: passive_scan_args.max_channel_time,
                    min_home_time: passive_scan_args.min_home_time,
                },
                _args: passive_scan_args,
            }
        }
    }

    pub struct StartActiveScanRequest {
        banjo_args: banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest,
        _args: ActiveScanArgs,
    }

    impl StartActiveScanRequest {
        #[cfg(test)]
        pub fn as_banjo(&self) -> &banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest {
            &self.banjo_args
        }

        /// Returns a raw pointer to the `banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest`
        /// that `StartActiveScanRequest` owns. The caller must ensure that the
        /// `StartActiveScanRequest` outlives the pointer this function returns, or else it
        /// will end up pointing to garbage.
        pub fn to_banjo_ptr(&self) -> *const banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest {
            &self.banjo_args as *const banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest
        }
    }

    impl From<ActiveScanArgs> for StartActiveScanRequest {
        fn from(active_scan_args: ActiveScanArgs) -> StartActiveScanRequest {
            StartActiveScanRequest {
                banjo_args: banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest {
                    min_channel_time: active_scan_args.min_channel_time,
                    max_channel_time: active_scan_args.max_channel_time,
                    min_home_time: active_scan_args.min_home_time,
                    min_probes_per_channel: active_scan_args.min_probes_per_channel,
                    max_probes_per_channel: active_scan_args.max_probes_per_channel,
                    channels_list: active_scan_args.channels.as_ptr(),
                    channels_count: active_scan_args.channels.len(),
                    ssids_list: active_scan_args.ssids_list.as_ptr(),
                    ssids_count: active_scan_args.ssids_list.len(),
                    mac_header_buffer: active_scan_args.mac_header.as_ptr(),
                    mac_header_size: active_scan_args.mac_header.len(),
                    ies_buffer: active_scan_args.ies.as_ptr(),
                    ies_size: active_scan_args.ies.len(),
                },
                _args: active_scan_args,
            }
        }
    }
}

// Wrappers to manage the lifetimes of pointers exposed by Banjo.
pub use convert::{QueryResponse, StartActiveScanRequest, StartPassiveScanRequest};

// Our device is used inside a separate worker thread, so we force Rust to allow this.
unsafe impl Send for DeviceInterface {}

/// A `Device` allows transmitting frames and MLME messages.
#[repr(C)]
pub struct DeviceInterface {
    device: *mut c_void,
    /// Start operations on the underlying device and return the SME channel.
    start: extern "C" fn(
        device: *mut c_void,
        ifc: *const WlanSoftmacIfcProtocol<'_>,
        out_sme_channel: *mut zx::sys::zx_handle_t,
    ) -> i32,
    /// Request to deliver an Ethernet II frame to Fuchsia's Netstack.
    deliver_eth_frame: extern "C" fn(device: *mut c_void, data: *const u8, len: usize) -> i32,
    /// Deliver a WLAN frame directly through the firmware.
    queue_tx: extern "C" fn(
        device: *mut c_void,
        options: u32,
        buf: OutBuf,
        tx_info: banjo_wlan_softmac::WlanTxInfo,
    ) -> i32,
    /// Reports the current status to the ethernet driver.
    set_ethernet_status: extern "C" fn(device: *mut c_void, status: u32) -> i32,
    /// Returns the currently set WLAN channel.
    get_wlan_channel: extern "C" fn(device: *mut c_void) -> banjo_common::WlanChannel,
    /// Request the PHY to change its channel. If successful, get_wlan_channel will return the
    /// chosen channel.
    set_wlan_channel: extern "C" fn(device: *mut c_void, channel: banjo_common::WlanChannel) -> i32,
    /// Set a key on the device.
    /// |key| is mutable because the underlying API does not take a const wlan_key_configuration_t.
    set_key: extern "C" fn(
        device: *mut c_void,
        key: *mut banjo_wlan_softmac::WlanKeyConfiguration,
    ) -> i32,
    /// Make passive scan request to the driver
    start_passive_scan: extern "C" fn(
        device: *mut c_void,
        passive_scan_args: *const banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest,
        out_scan_id: *mut u64,
    ) -> zx::sys::zx_status_t,
    /// Make active scan request to the driver
    start_active_scan: extern "C" fn(
        device: *mut c_void,
        active_scan_args: *const banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest,
        out_scan_id: *mut u64,
    ) -> zx::sys::zx_status_t,
    /// Cancel ongoing scan in the driver
    cancel_scan: extern "C" fn(device: *mut c_void, scan_id: u64) -> zx::sys::zx_status_t,
    /// Get information and capabilities of this WLAN interface
    get_wlan_softmac_query_response: extern "C" fn(device: *mut c_void) -> WlanSoftmacQueryResponse,
    /// Get discovery features supported by this WLAN interface
    get_discovery_support: extern "C" fn(device: *mut c_void) -> banjo_common::DiscoverySupport,
    /// Get MAC sublayer features supported by this WLAN interface
    get_mac_sublayer_support:
        extern "C" fn(device: *mut c_void) -> banjo_common::MacSublayerSupport,
    /// Get security features supported by this WLAN interface
    get_security_support: extern "C" fn(device: *mut c_void) -> banjo_common::SecuritySupport,
    /// Get spectrum management features supported by this WLAN interface
    get_spectrum_management_support:
        extern "C" fn(device: *mut c_void) -> banjo_common::SpectrumManagementSupport,
    /// Configure the device's BSS.
    /// |cfg| is mutable because the underlying API does not take a const join_bss_request_t.
    join_bss: extern "C" fn(device: *mut c_void, cfg: &mut JoinBssRequest) -> i32,
    /// Enable hardware offload of beaconing on the device.
    enable_beaconing: extern "C" fn(
        device: *mut c_void,
        buf: OutBuf,
        tim_ele_offset: usize,
        beacon_interval: u16,
    ) -> i32,
    /// Disable beaconing on the device.
    disable_beaconing: extern "C" fn(device: *mut c_void) -> i32,
    /// Clear the association context.
    clear_association: extern "C" fn(device: *mut c_void, addr: &[u8; 6]) -> i32,
}

pub mod test_utils {
    use {
        super::*,
        crate::{
            buffer::{BufferProvider, FakeBufferProvider},
            ddk_converter::convert_ddk_band_cap,
            error::Error,
            zeroed_array_from_prefix,
        },
        banjo_fuchsia_wlan_common as banjo_common, fidl_fuchsia_wlan_internal as fidl_internal,
        fidl_fuchsia_wlan_sme as fidl_sme, fuchsia_async as fasync,
        fuchsia_zircon::HandleBased,
        std::{
            collections::VecDeque,
            sync::{Arc, Mutex},
        },
        wlan_sme,
    };

    pub trait FromMlmeEvent {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self>
        where
            Self: std::marker::Sized;
    }

    impl FromMlmeEvent for fidl_mlme::AuthenticateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_authenticate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::AssociateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_associate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::ConnectConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_connect_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::StartConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_start_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::StopConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_stop_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::ScanResult {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_on_scan_result()
        }
    }

    impl FromMlmeEvent for fidl_mlme::ScanEnd {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_on_scan_end()
        }
    }

    impl FromMlmeEvent for fidl_mlme::EapolConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_eapol_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::EapolIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_eapol_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::DeauthenticateConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_deauthenticate_conf()
        }
    }

    impl FromMlmeEvent for fidl_mlme::DeauthenticateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_deauthenticate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::DisassociateIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_disassociate_ind()
        }
    }

    impl FromMlmeEvent for fidl_mlme::SetKeysConfirm {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_set_keys_conf()
        }
    }

    impl FromMlmeEvent for fidl_internal::SignalReportIndication {
        fn from_event(event: fidl_mlme::MlmeEvent) -> Option<Self> {
            event.into_signal_report()
        }
    }

    pub struct FakeDeviceConfig {
        pub mac_role: banjo_common::WlanMacRole,
        pub sta_addr: MacAddr,
        pub start_passive_scan_fails: bool,
        pub start_active_scan_fails: bool,
        pub send_wlan_frame_fails: bool,
    }

    impl Default for FakeDeviceConfig {
        fn default() -> Self {
            Self {
                mac_role: banjo_common::WlanMacRole::CLIENT,
                sta_addr: [7u8; 6],
                start_passive_scan_fails: false,
                start_active_scan_fails: false,
                send_wlan_frame_fails: false,
            }
        }
    }

    /// Wrapper struct that can share mutable access to the internal
    /// FakeDeviceState.
    #[derive(Clone)]
    pub struct FakeDevice {
        state: Arc<Mutex<FakeDeviceState>>,
    }

    pub struct FakeDeviceState {
        pub config: FakeDeviceConfig,
        pub minstrel: Option<crate::MinstrelWrapper>,
        pub eth_queue: Vec<Vec<u8>>,
        pub wlan_queue: Vec<(Vec<u8>, u32)>,
        pub mlme_event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
        pub mlme_event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
        pub mlme_request_sink: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        pub mlme_request_stream: Option<mpsc::UnboundedReceiver<wlan_sme::MlmeRequest>>,
        pub usme_bootstrap_client_end:
            Option<fidl::endpoints::ClientEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub usme_bootstrap_server_end:
            Option<fidl::endpoints::ServerEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub wlan_channel: banjo_common::WlanChannel,
        pub keys: Vec<key::KeyConfig>,
        pub next_scan_id: u64,
        pub captured_passive_scan_args: Option<PassiveScanArgs>,
        pub captured_active_scan_args: Option<ActiveScanArgs>,
        pub query_response: QueryResponse,
        pub discovery_support: banjo_common::DiscoverySupport,
        pub mac_sublayer_support: banjo_common::MacSublayerSupport,
        pub security_support: banjo_common::SecuritySupport,
        pub spectrum_management_support: banjo_common::SpectrumManagementSupport,
        pub join_bss_request: Option<JoinBssRequest>,
        pub beacon_config: Option<(Vec<u8>, usize, TimeUnit)>,
        pub link_status: LinkStatus,
        pub assocs: std::collections::HashMap<MacAddr, fidl_softmac::WlanAssociationConfig>,
        pub buffer_provider: BufferProvider,
        pub set_key_results: VecDeque<Result<(), zx::Status>>,
    }

    impl FakeDevice {
        pub fn new(executor: &fasync::TestExecutor) -> (FakeDevice, Arc<Mutex<FakeDeviceState>>) {
            Self::new_with_config(executor, FakeDeviceConfig::default())
        }
        pub fn new_with_config(
            _executor: &fasync::TestExecutor,
            config: FakeDeviceConfig,
        ) -> (FakeDevice, Arc<Mutex<FakeDeviceState>>) {
            let query_response =
                QueryResponse::fake().with_mac_role(config.mac_role).with_sta_addr(config.sta_addr);

            // Create a channel for SME requests, to be surfaced by start().
            let (usme_bootstrap_client_end, usme_bootstrap_server_end) =
                fidl::endpoints::create_endpoints::<fidl_sme::UsmeBootstrapMarker>();
            let (mlme_event_sink, mlme_event_stream) = mpsc::unbounded();
            let (mlme_request_sink, mlme_request_stream) = mpsc::unbounded();
            let state = Arc::new(Mutex::new(FakeDeviceState {
                config,
                minstrel: None,
                eth_queue: vec![],
                wlan_queue: vec![],
                mlme_event_sink,
                mlme_event_stream: Some(mlme_event_stream),
                mlme_request_sink,
                mlme_request_stream: Some(mlme_request_stream),
                usme_bootstrap_client_end: Some(usme_bootstrap_client_end),
                usme_bootstrap_server_end: Some(usme_bootstrap_server_end),
                wlan_channel: banjo_common::WlanChannel {
                    primary: 0,
                    cbw: banjo_common::ChannelBandwidth::CBW20,
                    secondary80: 0,
                },
                next_scan_id: 0,
                captured_passive_scan_args: None,
                captured_active_scan_args: None,
                query_response,
                discovery_support: fake_discovery_support(),
                mac_sublayer_support: fake_mac_sublayer_support(),
                security_support: fake_security_support(),
                spectrum_management_support: fake_spectrum_management_support(),
                keys: vec![],
                join_bss_request: None,
                beacon_config: None,
                link_status: LinkStatus::DOWN,
                assocs: std::collections::HashMap::new(),
                buffer_provider: FakeBufferProvider::new(),
                set_key_results: VecDeque::new(),
            }));
            (FakeDevice { state: state.clone() }, state)
        }

        pub fn state(&self) -> Arc<Mutex<FakeDeviceState>> {
            self.state.clone()
        }
    }

    impl FakeDeviceState {
        #[track_caller]
        pub fn next_mlme_msg<T: FromMlmeEvent>(&mut self) -> Result<T, Error> {
            self.mlme_event_stream
                .as_mut()
                .expect("no mlme event stream available")
                .try_next()
                .map_err(|e| anyhow::format_err!("Failed to read mlme event stream: {}", e))
                .and_then(|opt_next| opt_next.ok_or(anyhow::format_err!("No message available")))
                .and_then(|evt| {
                    T::from_event(evt).ok_or(anyhow::format_err!("Unexpected mlme event"))
                })
                .map_err(|e| e.into())
        }

        pub fn reset(&mut self) {
            self.eth_queue.clear();
        }
    }

    impl DeviceOps for FakeDevice {
        fn start(
            &mut self,
            _ifc: *const WlanSoftmacIfcProtocol<'_>,
        ) -> Result<zx::Handle, zx::Status> {
            let mut state = self.state.lock().unwrap();
            let usme_bootstrap_server_end_handle =
                state.usme_bootstrap_server_end.take().unwrap().into_channel().into_handle();
            // TODO(fxbug.dev/45464): Capture _ifc and provide a testing surface.
            Ok(usme_bootstrap_server_end_handle)
        }

        fn wlan_softmac_query_response(&mut self) -> QueryResponse {
            self.state.lock().unwrap().query_response.clone()
        }

        fn discovery_support(&mut self) -> banjo_common::DiscoverySupport {
            self.state.lock().unwrap().discovery_support
        }

        fn mac_sublayer_support(&mut self) -> banjo_common::MacSublayerSupport {
            self.state.lock().unwrap().mac_sublayer_support
        }

        fn security_support(&mut self) -> banjo_common::SecuritySupport {
            self.state.lock().unwrap().security_support
        }

        fn spectrum_management_support(&mut self) -> banjo_common::SpectrumManagementSupport {
            self.state.lock().unwrap().spectrum_management_support
        }

        fn deliver_eth_frame(&mut self, data: &[u8]) -> Result<(), zx::Status> {
            self.state.lock().unwrap().eth_queue.push(data.to_vec());
            Ok(())
        }

        fn send_wlan_frame(&mut self, buf: OutBuf, _tx_flags: u32) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            if state.config.send_wlan_frame_fails {
                buf.free();
                return Err(zx::Status::IO);
            }
            state.wlan_queue.push((buf.as_slice().to_vec(), 0));
            buf.free();
            Ok(())
        }

        fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
            self.state.lock().unwrap().link_status = status;
            Ok(())
        }

        fn set_channel(
            &mut self,
            wlan_channel: banjo_common::WlanChannel,
        ) -> Result<(), zx::Status> {
            self.state.lock().unwrap().wlan_channel = wlan_channel;
            Ok(())
        }

        fn channel(&mut self) -> banjo_common::WlanChannel {
            self.state.lock().unwrap().wlan_channel
        }

        fn set_key(&mut self, key: key::KeyConfig) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            state.keys.push(key.clone());
            state.set_key_results.pop_front().unwrap_or(Ok(()))
        }

        fn start_passive_scan(
            &mut self,
            passive_scan_args: PassiveScanArgs,
        ) -> Result<u64, zx::Status> {
            let mut state = self.state.lock().unwrap();
            if state.config.start_passive_scan_fails {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let scan_id = state.next_scan_id;
            state.next_scan_id += 1;
            state.captured_passive_scan_args.replace(passive_scan_args);
            Ok(scan_id)
        }

        fn start_active_scan(
            &mut self,
            active_scan_args: ActiveScanArgs,
        ) -> Result<u64, zx::Status> {
            let mut state = self.state.lock().unwrap();
            if state.config.start_active_scan_fails {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let scan_id = state.next_scan_id;
            state.next_scan_id += 1;
            state.captured_active_scan_args.replace(active_scan_args);
            Ok(scan_id)
        }

        fn cancel_scan(&mut self, _scan_id: u64) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }

        fn join_bss(&mut self, cfg: JoinBssRequest) -> Result<(), zx::Status> {
            self.state.lock().unwrap().join_bss_request.replace(cfg);
            Ok(())
        }

        fn enable_beaconing(
            &mut self,
            buf: OutBuf,
            tim_ele_offset: usize,
            beacon_interval: TimeUnit,
        ) -> Result<(), zx::Status> {
            self.state.lock().unwrap().beacon_config =
                Some((buf.as_slice().to_vec(), tim_ele_offset, beacon_interval));
            buf.free();
            Ok(())
        }

        fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
            self.state.lock().unwrap().beacon_config = None;
            Ok(())
        }

        fn notify_association_complete(
            &mut self,
            cfg: fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            if let Some(minstrel) = &state.minstrel {
                minstrel.lock().add_peer(&cfg)?
            }
            state.assocs.insert(cfg.bssid.unwrap(), cfg);
            Ok(())
        }

        fn clear_association(&mut self, addr: &MacAddr) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            if let Some(minstrel) = &state.minstrel {
                minstrel.lock().remove_peer(addr);
            }
            state.assocs.remove(addr);
            state.join_bss_request = None;
            Ok(())
        }

        fn take_mlme_event_stream(
            &mut self,
        ) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
            self.state.lock().unwrap().mlme_event_stream.take()
        }

        fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
            self.state.lock().unwrap().mlme_event_sink.unbounded_send(event).map_err(|e| e.into())
        }

        fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper) {
            self.state.lock().unwrap().minstrel.replace(minstrel);
        }

        fn minstrel(&mut self) -> Option<crate::MinstrelWrapper> {
            self.state.lock().unwrap().minstrel.as_ref().map(Arc::clone)
        }
    }

    pub fn fake_band_caps() -> Vec<banjo_wlan_softmac::WlanSoftmacBandCapability> {
        let basic_rate_list = zeroed_array_from_prefix!(
            [0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c],
            banjo_ieee80211::MAX_SUPPORTED_BASIC_RATES as usize
        );
        let basic_rate_count = basic_rate_list.len() as u8;

        vec![
            banjo_wlan_softmac::WlanSoftmacBandCapability {
                band: banjo_common::WlanBand::TWO_GHZ,
                basic_rate_list,
                basic_rate_count,
                operating_channel_list: zeroed_array_from_prefix!(
                    [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
                    banjo_ieee80211::MAX_UNIQUE_CHANNEL_NUMBERS as usize,
                ),
                operating_channel_count: 14,
                ht_supported: true,
                ht_caps: banjo_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                },
                vht_supported: false,
                vht_caps: banjo_ieee80211::VhtCapabilities { bytes: Default::default() },
            },
            banjo_wlan_softmac::WlanSoftmacBandCapability {
                band: banjo_common::WlanBand::FIVE_GHZ,
                basic_rate_list: zeroed_array_from_prefix!(
                    [0x02, 0x04, 0x0b, 0x16, 0x30, 0x60, 0x7e, 0x7f],
                    banjo_ieee80211::MAX_SUPPORTED_BASIC_RATES as usize
                ),
                basic_rate_count: 8,
                operating_channel_list: zeroed_array_from_prefix!(
                    [36, 40, 44, 48, 149, 153, 157, 161],
                    banjo_ieee80211::MAX_UNIQUE_CHANNEL_NUMBERS as usize,
                ),
                operating_channel_count: 8,
                ht_supported: true,
                ht_caps: banjo_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                },
                vht_supported: true,
                vht_caps: banjo_ieee80211::VhtCapabilities {
                    bytes: [0x32, 0x50, 0x80, 0x0f, 0xfe, 0xff, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00],
                },
            },
        ]
    }

    pub fn fake_fidl_band_caps() -> Vec<fidl_mlme::BandCapability> {
        fake_band_caps()
            .into_iter()
            .map(|band_cap| {
                convert_ddk_band_cap(&band_cap).expect("Failed to convert Banjo band capability")
            })
            .collect()
    }

    pub fn fake_discovery_support() -> banjo_common::DiscoverySupport {
        banjo_common::DiscoverySupport {
            scan_offload: banjo_common::ScanOffloadExtension {
                supported: true,
                scan_cancel_supported: false,
            },
            probe_response_offload: banjo_common::ProbeResponseOffloadExtension {
                supported: false,
            },
        }
    }

    pub fn fake_mac_sublayer_support() -> banjo_common::MacSublayerSupport {
        banjo_common::MacSublayerSupport {
            rate_selection_offload: banjo_common::RateSelectionOffloadExtension {
                supported: false,
            },
            data_plane: banjo_common::DataPlaneExtension {
                data_plane_type: banjo_common::DataPlaneType::ETHERNET_DEVICE,
            },
            device: banjo_common::DeviceExtension {
                is_synthetic: true,
                mac_implementation_type: banjo_common::MacImplementationType::SOFTMAC,
                tx_status_report_supported: true,
            },
        }
    }

    pub fn fake_security_support() -> banjo_common::SecuritySupport {
        banjo_common::SecuritySupport {
            mfp: banjo_common::MfpFeature { supported: false },
            sae: banjo_common::SaeFeature {
                driver_handler_supported: false,
                sme_handler_supported: false,
            },
        }
    }

    pub fn fake_spectrum_management_support() -> banjo_common::SpectrumManagementSupport {
        banjo_common::SpectrumManagementSupport {
            dfs: banjo_common::DfsFeature { supported: true },
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            banjo_list_to_slice,
            ddk_converter::{cssid_from_ssid_unchecked, test_utils::band_capability_eq},
            zeroed_array_from_prefix,
        },
        banjo_fuchsia_wlan_ieee80211::*,
        banjo_fuchsia_wlan_internal as banjo_internal, fidl_fuchsia_wlan_common as fidl_common,
        fuchsia_async as fasync,
        ieee80211::Ssid,
        std::convert::TryFrom,
        wlan_common::assert_variant,
    };

    fn make_deauth_confirm_msg() -> fidl_mlme::DeauthenticateConfirm {
        fidl_mlme::DeauthenticateConfirm { peer_sta_address: [1; 6] }
    }

    #[test]
    fn state_method_returns_correct_pointer() {
        let exec = fasync::TestExecutor::new();
        let (fake_device, fake_device_state) = FakeDevice::new(&exec);
        assert_eq!(Arc::as_ptr(&fake_device.state()), Arc::as_ptr(&fake_device_state));
    }

    #[test]
    fn fake_device_returns_expected_wlan_softmac_query_response() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let query_response = fake_device.wlan_softmac_query_response();
        assert_eq!(query_response.sta_addr(), [7u8; 6]);
        assert_eq!(query_response.mac_role(), banjo_common::WlanMacRole::CLIENT);
        assert_eq!(
            query_response.supported_phys(),
            &[
                banjo_common::WlanPhyType::DSSS,
                banjo_common::WlanPhyType::HR,
                banjo_common::WlanPhyType::OFDM,
                banjo_common::WlanPhyType::ERP,
                banjo_common::WlanPhyType::HT,
                banjo_common::WlanPhyType::VHT,
            ]
        );
        assert_eq!(query_response.hardware_capability(), 0);

        let band_caps = query_response.band_caps();
        let expected_band_caps = [
            banjo_wlan_softmac::WlanSoftmacBandCapability {
                band: banjo_common::WlanBand::TWO_GHZ,
                basic_rate_list: zeroed_array_from_prefix!(
                    [0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c],
                    banjo_ieee80211::MAX_SUPPORTED_BASIC_RATES as usize
                ),
                basic_rate_count: 12,
                operating_channel_list: zeroed_array_from_prefix!(
                    [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
                    banjo_ieee80211::MAX_UNIQUE_CHANNEL_NUMBERS as usize,
                ),
                operating_channel_count: 14,
                ht_supported: true,
                ht_caps: HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                },
                vht_supported: false,
                vht_caps: VhtCapabilities { bytes: Default::default() },
            },
            banjo_wlan_softmac::WlanSoftmacBandCapability {
                band: banjo_common::WlanBand::FIVE_GHZ,
                basic_rate_list: zeroed_array_from_prefix!(
                    [0x02, 0x04, 0x0b, 0x16, 0x30, 0x60, 0x7e, 0x7f],
                    banjo_ieee80211::MAX_SUPPORTED_BASIC_RATES as usize
                ),
                basic_rate_count: 8,
                operating_channel_list: zeroed_array_from_prefix!(
                    [36, 40, 44, 48, 149, 153, 157, 161],
                    banjo_ieee80211::MAX_UNIQUE_CHANNEL_NUMBERS as usize,
                ),
                operating_channel_count: 8,
                ht_supported: true,
                ht_caps: banjo_ieee80211::HtCapabilities {
                    bytes: [
                        0x63, 0x00, // HT capability info
                        0x17, // AMPDU params
                        0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                        0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                        0x01, 0x00, 0x00, 0x00, // Tx parameters
                        0x00, 0x00, // HT extended capabilities
                        0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                        0x00, // ASEL capabilities
                    ],
                },
                vht_supported: true,
                vht_caps: banjo_ieee80211::VhtCapabilities {
                    bytes: [0x32, 0x50, 0x80, 0x0f, 0xfe, 0xff, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00],
                },
            },
        ];
        band_caps.iter().zip(expected_band_caps).for_each(|(band_cap, expected_band_cap)| {
            assert!(band_capability_eq(&band_cap, &expected_band_cap));
        });
    }

    #[test]
    fn fake_device_returns_expected_discovery_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let discovery_support = fake_device.discovery_support();
        assert_eq!(
            discovery_support,
            banjo_common::DiscoverySupport {
                scan_offload: banjo_common::ScanOffloadExtension {
                    supported: true,
                    scan_cancel_supported: false,
                },
                probe_response_offload: banjo_common::ProbeResponseOffloadExtension {
                    supported: false,
                },
            }
        );
    }

    #[test]
    fn fake_device_returns_expected_mac_sublayer_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let mac_sublayer_support = fake_device.mac_sublayer_support();
        assert_eq!(
            mac_sublayer_support,
            banjo_common::MacSublayerSupport {
                rate_selection_offload: banjo_common::RateSelectionOffloadExtension {
                    supported: false,
                },
                data_plane: banjo_common::DataPlaneExtension {
                    data_plane_type: banjo_common::DataPlaneType::ETHERNET_DEVICE,
                },
                device: banjo_common::DeviceExtension {
                    is_synthetic: true,
                    mac_implementation_type: banjo_common::MacImplementationType::SOFTMAC,
                    tx_status_report_supported: true,
                },
            }
        );
    }

    #[test]
    fn fake_device_returns_expected_security_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let security_support = fake_device.security_support();
        assert_eq!(
            security_support,
            banjo_common::SecuritySupport {
                mfp: banjo_common::MfpFeature { supported: false },
                sae: banjo_common::SaeFeature {
                    driver_handler_supported: false,
                    sme_handler_supported: false,
                },
            }
        );
    }

    #[test]
    fn fake_device_returns_expected_spectrum_management_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let spectrum_management_support = fake_device.spectrum_management_support();
        assert_eq!(
            spectrum_management_support,
            banjo_common::SpectrumManagementSupport {
                dfs: banjo_common::DfsFeature { supported: true },
            }
        );
    }

    #[test]
    fn test_can_dynamically_change_fake_device_state() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        let query_response = fake_device.wlan_softmac_query_response();
        assert_eq!(query_response.mac_role(), banjo_common::WlanMacRole::CLIENT);

        fake_device_state.lock().unwrap().query_response =
            query_response.with_mac_role(banjo_common::WlanMacRole::AP);

        let query_response = fake_device.wlan_softmac_query_response();
        assert_eq!(query_response.mac_role(), banjo_common::WlanMacRole::AP);
    }

    #[test]
    fn send_mlme_message() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .send_mlme_event(fidl_mlme::MlmeEvent::DeauthenticateConf {
                resp: make_deauth_confirm_msg(),
            })
            .expect("error sending MLME message");

        // Read message from channel.
        let msg = fake_device_state
            .lock()
            .unwrap()
            .next_mlme_msg::<fidl_mlme::DeauthenticateConfirm>()
            .expect("error reading message from channel");
        assert_eq!(msg, make_deauth_confirm_msg());
    }

    #[test]
    fn send_mlme_message_peer_already_closed() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device_state.lock().unwrap().mlme_event_stream.take();

        fake_device
            .send_mlme_event(fidl_mlme::MlmeEvent::DeauthenticateConf {
                resp: make_deauth_confirm_msg(),
            })
            .expect_err("Mlme event should fail");
    }

    #[test]
    fn fake_device_deliver_eth_frame() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        assert_eq!(fake_device_state.lock().unwrap().eth_queue.len(), 0);
        let first_frame = [5; 32];
        let second_frame = [6; 32];
        assert_eq!(fake_device.deliver_eth_frame(&first_frame[..]), Ok(()));
        assert_eq!(fake_device.deliver_eth_frame(&second_frame[..]), Ok(()));
        assert_eq!(fake_device_state.lock().unwrap().eth_queue.len(), 2);
        assert_eq!(&fake_device_state.lock().unwrap().eth_queue[0], &first_frame);
        assert_eq!(&fake_device_state.lock().unwrap().eth_queue[1], &second_frame);
    }

    #[test]
    fn get_set_channel() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .set_channel(banjo_common::WlanChannel {
                primary: 2,
                cbw: banjo_common::ChannelBandwidth::CBW80P80,
                secondary80: 4,
            })
            .expect("set_channel failed?");
        // Check the internal state.
        assert_eq!(
            fake_device_state.lock().unwrap().wlan_channel,
            banjo_common::WlanChannel {
                primary: 2,
                cbw: banjo_common::ChannelBandwidth::CBW80P80,
                secondary80: 4
            }
        );
        // Check the external view of the internal state.
        assert_eq!(
            fake_device.channel(),
            banjo_common::WlanChannel {
                primary: 2,
                cbw: banjo_common::ChannelBandwidth::CBW80P80,
                secondary80: 4
            }
        );
    }

    #[test]
    fn set_key() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .set_key(key::KeyConfig {
                bssid: 1,
                protection: key::Protection::NONE,
                cipher_oui: [3, 4, 5],
                cipher_type: 6,
                key_type: key::KeyType::PAIRWISE,
                peer_addr: [8; 6],
                key_idx: 9,
                key_len: 10,
                key: [11; 32],
                rsc: 12,
            })
            .expect("error setting key");
        assert_eq!(fake_device_state.lock().unwrap().keys.len(), 1);
    }

    #[test]
    fn successful_conversion_of_passive_scan_args_to_banjo() {
        let args = StartPassiveScanRequest::from(PassiveScanArgs {
            channels: vec![1u8, 2, 3],
            min_channel_time: zx::Duration::from_millis(3).into_nanos(),
            max_channel_time: zx::Duration::from_millis(123).into_nanos(),
            min_home_time: 5,
        });
        let banjo_args = args.as_banjo();
        assert_eq!(banjo_list_to_slice!(banjo_args, channels), &[1u8, 2, 3]);
        assert_eq!(banjo_args.min_channel_time, zx::Duration::from_millis(3).into_nanos());
        assert_eq!(banjo_args.max_channel_time, zx::Duration::from_millis(123).into_nanos());
        assert_eq!(banjo_args.min_home_time, 5);
    }

    #[test]
    fn start_passive_scan() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);

        let result = fake_device.start_passive_scan(PassiveScanArgs {
            channels: vec![1u8, 2, 3],
            min_channel_time: zx::Duration::from_millis(0).into_nanos(),
            max_channel_time: zx::Duration::from_millis(200).into_nanos(),
            min_home_time: 0,
        });
        assert!(result.is_ok());

        assert_variant!(fake_device_state.lock().unwrap().captured_passive_scan_args, Some(ref passive_scan_args) => {
            assert_eq!(passive_scan_args.channels, vec![1, 2, 3]);
            assert_eq!(passive_scan_args.min_channel_time, 0);
            assert_eq!(passive_scan_args.max_channel_time, 200_000_000);
            assert_eq!(passive_scan_args.min_home_time, 0);
        }, "No passive scan argument available.");
    }

    #[test]
    fn successful_conversion_of_active_scan_args_to_banjo() {
        let args = StartActiveScanRequest::from(ActiveScanArgs {
            min_channel_time: zx::Duration::from_millis(2).into_nanos(),
            max_channel_time: zx::Duration::from_millis(301).into_nanos(),
            min_home_time: 6,
            min_probes_per_channel: 1,
            max_probes_per_channel: 3,
            channels: vec![4u8, 36, 149],
            ssids_list: vec![
                cssid_from_ssid_unchecked(&Ssid::try_from("foo").unwrap().into()),
                cssid_from_ssid_unchecked(&Ssid::try_from("bar").unwrap().into()),
            ],
            #[rustfmt::skip]
            mac_header: vec![
                0x40u8, 0x00, // Frame Control
                0x00, 0x00, // Duration
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                0x70, 0xdc, // Sequence Control
            ],
            #[rustfmt::skip]
            ies: vec![
                0x01u8, // Element ID for Supported Rates
                0x08, // Length
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 // Supported Rates
            ],
        });
        let banjo_args = args.as_banjo();
        assert_eq!(banjo_args.min_channel_time, zx::Duration::from_millis(2).into_nanos());
        assert_eq!(banjo_args.max_channel_time, zx::Duration::from_millis(301).into_nanos());
        assert_eq!(banjo_args.min_home_time, 6);
        assert_eq!(banjo_args.min_probes_per_channel, 1);
        assert_eq!(banjo_args.max_probes_per_channel, 3);
        assert_eq!(banjo_list_to_slice!(banjo_args, channels), &[4, 36, 149]);
        assert_eq!(
            banjo_list_to_slice!(banjo_args, ssids),
            &[
                cssid_from_ssid_unchecked(&Ssid::try_from("foo").unwrap().into()),
                cssid_from_ssid_unchecked(&Ssid::try_from("bar").unwrap().into()),
            ]
        );
        #[rustfmt::skip]
        assert_eq!(banjo_buffer_to_slice!(banjo_args, mac_header), &[
            0x40u8, 0x00, // Frame Control
            0x00, 0x00, // Duration
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
            0x70, 0xdc, // Sequence Control
        ]);
        #[rustfmt::skip]
        assert_eq!(banjo_buffer_to_slice!(banjo_args, ies), &[
            0x01u8, // Element ID for Supported Rates
            0x08, // Length
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 // Supported Rates
        ]);
    }

    #[test]
    fn start_active_scan() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);

        let result = fake_device.start_active_scan(ActiveScanArgs {
            min_channel_time: zx::Duration::from_millis(0).into_nanos(),
            max_channel_time: zx::Duration::from_millis(200).into_nanos(),
            min_home_time: 0,
            min_probes_per_channel: 1,
            max_probes_per_channel: 3,
            channels: vec![1u8, 2, 3],
            ssids_list: vec![
                cssid_from_ssid_unchecked(&Ssid::try_from("foo").unwrap().into()),
                cssid_from_ssid_unchecked(&Ssid::try_from("bar").unwrap().into()),
            ],
            #[rustfmt::skip]
                mac_header: vec![
                    0x40u8, 0x00, // Frame Control
                    0x00, 0x00, // Duration
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                    0x70, 0xdc, // Sequence Control
                ],
            #[rustfmt::skip]
                ies: vec![
                    0x01u8, // Element ID for Supported Rates
                    0x08, // Length
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 // Supported Rates
                ],
        });
        assert!(result.is_ok());
        assert_variant!(fake_device_state.lock().unwrap().captured_active_scan_args.as_ref(), Some(active_scan_args) => {
            assert_eq!(active_scan_args.min_channel_time, 0);
            assert_eq!(active_scan_args.max_channel_time, 200_000_000);
            assert_eq!(active_scan_args.min_home_time, 0);
            assert_eq!(active_scan_args.min_probes_per_channel, 1);
            assert_eq!(active_scan_args.max_probes_per_channel, 3);
            assert_eq!(active_scan_args.channels, vec![1, 2, 3]);
            assert_eq!(active_scan_args.ssids_list,
                       vec![
                           cssid_from_ssid_unchecked(&Ssid::try_from("foo").unwrap().into()),
                           cssid_from_ssid_unchecked(&Ssid::try_from("bar").unwrap().into()),
                       ]);
            assert_eq!(active_scan_args.mac_header, vec![
                0x40, 0x00, // Frame Control
                0x00, 0x00, // Duration
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                0x70, 0xdc, // Sequence Control
            ]);
            assert_eq!(active_scan_args.ies, vec![
                0x01u8, // Element ID for Supported Rates
                0x08, // Length
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 // Supported Rates
            ][..]);
        }, "No active scan argument available.");
    }

    #[test]
    fn join_bss() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .join_bss(JoinBssRequest {
                bssid: [1, 2, 3, 4, 5, 6],
                bss_type: banjo_internal::BssType::PERSONAL,
                remote: true,
                beacon_period: 100,
            })
            .expect("error configuring bss");
        assert!(fake_device_state.lock().unwrap().join_bss_request.is_some());
    }

    #[test]
    fn enable_disable_beaconing() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        let mut in_buf = fake_device_state
            .lock()
            .unwrap()
            .buffer_provider
            .get_buffer(4)
            .expect("error getting buffer");
        in_buf.as_mut_slice().copy_from_slice(&[1, 2, 3, 4][..]);

        fake_device
            .enable_beaconing(OutBuf::from(in_buf, 4), 1, TimeUnit(2))
            .expect("error enabling beaconing");
        assert_variant!(
        fake_device_state.lock().unwrap().beacon_config.as_ref(),
        Some((buf, tim_ele_offset, beacon_interval)) => {
            assert_eq!(&buf[..], &[1, 2, 3, 4][..]);
            assert_eq!(*tim_ele_offset, 1);
            assert_eq!(*beacon_interval, TimeUnit(2));
        });
        fake_device.disable_beaconing().expect("error disabling beaconing");
        assert_variant!(fake_device_state.lock().unwrap().beacon_config.as_ref(), None);
    }

    #[test]
    fn set_ethernet_status() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device.set_ethernet_up().expect("failed setting status");
        assert_eq!(fake_device_state.lock().unwrap().link_status, LinkStatus::UP);

        fake_device.set_ethernet_down().expect("failed setting status");
        assert_eq!(fake_device_state.lock().unwrap().link_status, LinkStatus::DOWN);
    }

    #[test]
    fn notify_association_complete() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .notify_association_complete(fidl_softmac::WlanAssociationConfig {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                aid: Some(1),
                listen_interval: Some(2),
                channel: Some(fidl_common::WlanChannel {
                    primary: 3,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                }),
                qos: Some(false),
                wmm_params: None,
                rates: None,
                capability_info: Some(0x0102),
                ht_cap: None,
                ht_op: None,
                vht_cap: None,
                vht_op: None,
                ..Default::default()
            })
            .expect("error configuring assoc");
        assert!(fake_device_state.lock().unwrap().assocs.contains_key(&[1, 2, 3, 4, 5, 6]));
    }

    #[test]
    fn clear_association() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .join_bss(JoinBssRequest {
                bssid: [1, 2, 3, 4, 5, 6],
                bss_type: banjo_internal::BssType::PERSONAL,
                remote: true,
                beacon_period: 100,
            })
            .expect("error configuring bss");

        let assoc_cfg = fidl_softmac::WlanAssociationConfig {
            bssid: Some([1, 2, 3, 4, 5, 6]),
            aid: Some(1),
            channel: Some(fidl_common::WlanChannel {
                primary: 149,
                cbw: fidl_common::ChannelBandwidth::Cbw40,
                secondary80: 42,
            }),
            ..Default::default()
        };

        assert!(fake_device_state.lock().unwrap().join_bss_request.is_some());
        fake_device.notify_association_complete(assoc_cfg).expect("error configuring assoc");
        assert_eq!(fake_device_state.lock().unwrap().assocs.len(), 1);
        fake_device.clear_association(&[1, 2, 3, 4, 5, 6]).expect("error clearing assoc");
        assert_eq!(fake_device_state.lock().unwrap().assocs.len(), 0);
        assert!(fake_device_state.lock().unwrap().join_bss_request.is_none());
    }
}
