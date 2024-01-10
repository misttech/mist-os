// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{buffer::OutBuf, common::mac::WlanGi, error::Error},
    anyhow::format_err,
    banjo_fuchsia_wlan_common as banjo_common,
    banjo_fuchsia_wlan_softmac::{self as banjo_wlan_softmac, WlanRxPacket, WlanTxPacket},
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_mlme as fidl_mlme,
    fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_trace as trace, fuchsia_zircon as zx,
    futures::channel::mpsc,
    ieee80211::{MacAddr, MacAddrBytes},
    std::{ffi::c_void, fmt::Display, sync::Arc},
    tracing::error,
    wlan_common::{mac::FrameControl, tx_vector, TimeUnit},
};

pub mod completers {
    use {fuchsia_zircon as zx, tracing::error};

    pub struct StartStaCompleter<F>
    where
        F: FnOnce(Result<(), zx::Status>) + Send,
    {
        completer: Option<F>,
    }

    impl<F> StartStaCompleter<F>
    where
        F: FnOnce(Result<(), zx::Status>) + Send,
    {
        pub fn new(completer: F) -> Self {
            Self { completer: Some(completer) }
        }

        pub fn complete(mut self, result: Result<(), zx::Status>) {
            let completer = match self.completer.take() {
                None => {
                    error!("Failed to call completer because it is None.");
                    return;
                }
                Some(completer) => completer,
            };
            completer(result)
        }
    }

    impl<F> Drop for StartStaCompleter<F>
    where
        F: FnOnce(Result<(), zx::Status>) + Send,
    {
        fn drop(&mut self) {
            if let Some(completer) = self.completer.take() {
                error!(
                    "About to drop start_sta() completer without calling it!\n\
                     Calling start_sta() completer from drop() to mitigate potential deadlock."
                );
                completer(Err(zx::Status::BAD_STATE))
            }
        }
    }

    pub struct StopStaCompleter {
        // TODO(42075638): Using dynamic dispatch since otherwise we would need to plumb generics
        // everywhere MLME uses a DriverEventSink. Since we will remove DriverEventSink soon, this
        // is not worthwhile.
        completer: Option<Box<dyn FnOnce() + Send>>,
    }

    impl StopStaCompleter {
        pub fn new(completer: Box<dyn FnOnce() + Send>) -> Self {
            Self { completer: Some(completer) }
        }

        /// Safety: Must only be called if calling |completer| is thread-safe.
        pub fn complete(mut self) {
            let completer = match self.completer.take() {
                None => {
                    error!("Failed to call completer because it is None.");
                    return;
                }
                Some(completer) => completer,
            };
            completer()
        }
    }

    impl Drop for StopStaCompleter {
        fn drop(&mut self) {
            if let Some(completer) = self.completer.take() {
                error!(
                    "About to drop stop_sta() completer without calling it!\n\
                     Calling stop_sta() completer from drop() to mitigate potential deadlock."
                );
                completer()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use futures::channel::oneshot;

        #[test]
        fn start_sta_completer_sends_ok() {
            let (sender, mut receiver) = oneshot::channel::<Result<(), zx::Status>>();
            let start_sta_completer =
                StartStaCompleter::new(move |result: Result<(), zx::Status>| {
                    sender.send(result).expect("Failed to send result.");
                });
            start_sta_completer.complete(Ok(()));
            assert_eq!(Ok(Some(Ok(()))), receiver.try_recv());
        }

        #[test]
        fn start_sta_completer_sends_err() {
            let (sender, mut receiver) = oneshot::channel::<Result<(), zx::Status>>();
            let start_sta_completer =
                StartStaCompleter::new(move |result: Result<(), zx::Status>| {
                    sender.send(result).expect("Failed to send result.");
                });
            start_sta_completer.complete(Err(zx::Status::INTERNAL));
            assert_eq!(Ok(Some(Err(zx::Status::INTERNAL))), receiver.try_recv());
        }

        #[test]
        fn start_sta_completer_sends_err_on_drop() {
            let (sender, mut receiver) = oneshot::channel::<Result<(), zx::Status>>();
            let start_sta_completer =
                StartStaCompleter::new(move |result: Result<(), zx::Status>| {
                    sender.send(result).expect("Failed to send result.");
                });
            drop(start_sta_completer);
            assert_eq!(Ok(Some(Err(zx::Status::BAD_STATE))), receiver.try_recv());
        }

        #[test]
        fn stop_sta_completer_sends_value() {
            let (sender, mut receiver) = oneshot::channel();
            let stop_sta_completer = StopStaCompleter::new(Box::new(move || {
                sender.send(()).expect("Failed to send.");
            }));
            stop_sta_completer.complete();
            assert_eq!(Ok(Some(())), receiver.try_recv());
        }

        #[test]
        fn stop_sta_completer_sends_value_on_drop() {
            let (sender, mut receiver) = oneshot::channel();
            let stop_sta_completer = StopStaCompleter::new(Box::new(move || {
                sender.send(()).expect("Failed to send.");
            }));
            drop(stop_sta_completer);
            assert_eq!(Ok(Some(())), receiver.try_recv());
        }
    }
}

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

    fn flatten_and_log_error<T>(
        method_name: impl Display,
        result: Result<Result<T, zx::zx_status_t>, fidl::Error>,
    ) -> Result<T, zx::Status> {
        result
            .map_err(|fidl_error| {
                error!("FIDL error during {}: {:?}", method_name, fidl_error);
                zx::Status::INTERNAL
            })?
            .map_err(|status| {
                error!("{} failed: {:?}", method_name, status);
                zx::Status::from_raw(status)
            })
    }
}

const REQUIRED_WLAN_HEADER_LEN: usize = 10;
const PEER_ADDR_OFFSET: usize = 4;

/// This trait abstracts how Device accomplish operations. Test code
/// can then implement trait methods instead of mocking an underlying DeviceInterface
/// and FIDL proxy.
pub trait DeviceOps {
    fn start(&mut self, ifc: *const WlanSoftmacIfcProtocol<'_>) -> Result<zx::Handle, zx::Status>;
    fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status>;
    fn discovery_support(&mut self) -> Result<fidl_common::DiscoverySupport, zx::Status>;
    fn mac_sublayer_support(&mut self) -> Result<fidl_common::MacSublayerSupport, zx::Status>;
    fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status>;
    fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status>;
    fn deliver_eth_frame(&mut self, slice: &[u8]) -> Result<(), zx::Status>;
    fn send_wlan_frame(&mut self, buf: OutBuf, tx_flags: u32) -> Result<(), zx::Status>;

    fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status>;
    fn set_ethernet_up(&mut self) -> Result<(), zx::Status> {
        self.set_ethernet_status(LinkStatus::UP)
    }
    fn set_ethernet_down(&mut self) -> Result<(), zx::Status> {
        self.set_ethernet_status(LinkStatus::DOWN)
    }
    fn set_channel(&mut self, channel: fidl_common::WlanChannel) -> Result<(), zx::Status>;
    fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBridgeStartPassiveScanResponse, zx::Status>;
    fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBridgeStartActiveScanResponse, zx::Status>;
    fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeCancelScanRequest,
    ) -> Result<(), zx::Status>;
    fn join_bss(&mut self, request: &fidl_common::JoinBssRequest) -> Result<(), zx::Status>;
    fn enable_beaconing(
        &mut self,
        request: fidl_softmac::WlanSoftmacBridgeEnableBeaconingRequest,
    ) -> Result<(), zx::Status>;
    fn disable_beaconing(&mut self) -> Result<(), zx::Status>;
    fn install_key(
        &mut self,
        key_configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status>;
    fn notify_association_complete(
        &mut self,
        assoc_cfg: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status>;
    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeClearAssociationRequest,
    ) -> Result<(), zx::Status>;
    fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status>;
    fn take_mlme_event_stream(&mut self) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>;
    fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error>;
    fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper);
    fn minstrel(&mut self) -> Option<crate::MinstrelWrapper>;
    fn tx_vector_idx(
        &mut self,
        frame_control: &FrameControl,
        peer_addr: &MacAddr,
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
                // TODO(https://fxbug.dev/28893): Choose an optimal MCS for management frames
                // TODO(https://fxbug.dev/43456): Log stats about minstrel usage vs default tx vector.
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

pub fn try_query(
    device: &mut impl DeviceOps,
) -> Result<fidl_softmac::WlanSoftmacQueryResponse, Error> {
    device
        .wlan_softmac_query_response()
        .map_err(|status| Error::Status(String::from("Failed to query device."), status))
}

pub fn try_query_iface_mac(device: &mut impl DeviceOps) -> Result<MacAddr, Error> {
    try_query(device).and_then(|query_response| {
        query_response.sta_addr.map(From::from).ok_or_else(|| {
            Error::Internal(format_err!(
                "Required field not set in device query response: iface MAC"
            ))
        })
    })
}

pub fn try_query_discovery_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::DiscoverySupport, Error> {
    device.discovery_support().map_err(|status| {
        Error::Status(String::from("Failed to query discovery support for device."), status)
    })
}

pub fn try_query_mac_sublayer_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::MacSublayerSupport, Error> {
    device.mac_sublayer_support().map_err(|status| {
        Error::Status(String::from("Failed to query MAC sublayer support for device."), status)
    })
}

pub fn try_query_security_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::SecuritySupport, Error> {
    device.security_support().map_err(|status| {
        Error::Status(String::from("Failed to query security support for device."), status)
    })
}

pub fn try_query_spectrum_management_support(
    device: &mut impl DeviceOps,
) -> Result<fidl_common::SpectrumManagementSupport, Error> {
    device.spectrum_management_support().map_err(|status| {
        Error::Status(
            String::from("Failed to query spectrum management support for device."),
            status,
        )
    })
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

    fn wlan_softmac_query_response(
        &mut self,
    ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
        Self::flatten_and_log_error(
            "Query",
            self.wlan_softmac_bridge_proxy.query(zx::Time::INFINITE),
        )
    }

    fn discovery_support(&mut self) -> Result<fidl_common::DiscoverySupport, zx::Status> {
        Self::flatten_and_log_error(
            "QueryDiscoverySupport",
            self.wlan_softmac_bridge_proxy.query_discovery_support(zx::Time::INFINITE),
        )
    }

    fn mac_sublayer_support(&mut self) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
        Self::flatten_and_log_error(
            "QueryMacSublayerSupport",
            self.wlan_softmac_bridge_proxy.query_mac_sublayer_support(zx::Time::INFINITE),
        )
    }

    fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
        Self::flatten_and_log_error(
            "QuerySecuritySupport",
            self.wlan_softmac_bridge_proxy.query_security_support(zx::Time::INFINITE),
        )
    }

    fn spectrum_management_support(
        &mut self,
    ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
        Self::flatten_and_log_error(
            "QuerySpectrumManagementSupport",
            self.wlan_softmac_bridge_proxy.query_spectrum_management_support(zx::Time::INFINITE),
        )
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
            zerocopy::Ref::<&[u8], FrameControl>::new(&buf.as_slice()[0..=1]).unwrap().into_ref();
        if frame_control.protected() {
            tx_flags |= banjo_wlan_softmac::WlanTxInfoFlags::PROTECTED.0;
        }
        let peer_addr: MacAddr = {
            let mut peer_addr = [0u8; 6];
            peer_addr.copy_from_slice(&buf.as_slice()[PEER_ADDR_OFFSET..PEER_ADDR_OFFSET + 6]);
            peer_addr.into()
        };
        let tx_vector_idx = self.tx_vector_idx(frame_control, &peer_addr, tx_flags);

        let tx_info = wlan_common::tx_vector::TxVector::from_idx(tx_vector_idx)
            .to_banjo_tx_info(tx_flags, self.minstrel.is_some());
        zx::ok((self.raw_device.queue_tx)(self.raw_device.device, 0, buf, tx_info))
    }

    fn set_ethernet_status(&mut self, status: LinkStatus) -> Result<(), zx::Status> {
        zx::ok((self.raw_device.set_ethernet_status)(self.raw_device.device, status.0))
    }

    fn set_channel(&mut self, channel: fidl_common::WlanChannel) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .set_channel(
                &fidl_softmac::WlanSoftmacBridgeSetChannelRequest {
                    channel: Some(channel),
                    ..Default::default()
                },
                zx::Time::INFINITE,
            )
            .map_err(|error| {
                error!("SetChannel failed with FIDL error: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    fn start_passive_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeStartPassiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBridgeStartPassiveScanResponse, zx::Status> {
        Self::flatten_and_log_error(
            "StartPassiveScan",
            self.wlan_softmac_bridge_proxy.start_passive_scan(request, zx::Time::INFINITE),
        )
    }

    fn start_active_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
    ) -> Result<fidl_softmac::WlanSoftmacBridgeStartActiveScanResponse, zx::Status> {
        Self::flatten_and_log_error(
            "StartActiveScan",
            self.wlan_softmac_bridge_proxy.start_active_scan(request, zx::Time::INFINITE),
        )
    }

    fn cancel_scan(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeCancelScanRequest,
    ) -> Result<(), zx::Status> {
        Self::flatten_and_log_error(
            "CancelScan",
            self.wlan_softmac_bridge_proxy.cancel_scan(request, zx::Time::INFINITE),
        )
    }

    fn join_bss(&mut self, request: &fidl_common::JoinBssRequest) -> Result<(), zx::Status> {
        Self::flatten_and_log_error(
            "JoinBss",
            self.wlan_softmac_bridge_proxy.join_bss(request, zx::Time::INFINITE),
        )
    }

    fn enable_beaconing(
        &mut self,
        request: fidl_softmac::WlanSoftmacBridgeEnableBeaconingRequest,
    ) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .enable_beaconing(&request, zx::Time::INFINITE)
            .map_err(|error| {
                error!("FIDL error during EnableBeaconing: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .disable_beaconing(zx::Time::INFINITE)
            .map_err(|error| {
                error!("DisableBeaconing failed with FIDL error: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    fn install_key(
        &mut self,
        key_configuration: &fidl_softmac::WlanKeyConfiguration,
    ) -> Result<(), zx::Status> {
        self.wlan_softmac_bridge_proxy
            .install_key(&key_configuration, zx::Time::INFINITE)
            .map_err(|error| {
                error!("FIDL error during InstallKey: {:?}", error);
                zx::Status::INTERNAL
            })?
            .map_err(zx::Status::from_raw)
    }

    fn notify_association_complete(
        &mut self,
        assoc_cfg: fidl_softmac::WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().add_peer(&assoc_cfg)?;
        }
        Self::flatten_and_log_error(
            "NotifyAssociationComplete",
            self.wlan_softmac_bridge_proxy
                .notify_association_complete(&assoc_cfg, zx::Time::INFINITE),
        )
    }

    fn clear_association(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        let addr: MacAddr = request
            .peer_addr
            .ok_or_else(|| {
                error!("ClearAssociation called with no peer_addr field.");
                zx::Status::INVALID_ARGS
            })?
            .into();
        if let Some(minstrel) = &self.minstrel {
            minstrel.lock().remove_peer(&addr);
        }
        Self::flatten_and_log_error(
            "ClearAssociation",
            self.wlan_softmac_bridge_proxy.clear_association(request, zx::Time::INFINITE),
        )
    }

    fn update_wmm_parameters(
        &mut self,
        request: &fidl_softmac::WlanSoftmacBridgeUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Self::flatten_and_log_error(
            "UpdateWmmParameters",
            self.wlan_softmac_bridge_proxy.update_wmm_parameters(request, zx::Time::INFINITE),
        )
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
    report_tx_result: extern "C" fn(
        ctx: &mut crate::DriverEventSink,
        tx_result: *const banjo_common::WlanTxResult,
    ),
    scan_complete: extern "C" fn(ctx: &mut crate::DriverEventSink, status: i32, scan_id: u64),
}

#[no_mangle]
extern "C" fn handle_recv(ctx: &mut crate::DriverEventSink, packet: *const WlanRxPacket) {
    trace::duration!("wlan", "handle_recv");

    // TODO(https://fxbug.dev/29063): C++ uses a buffer allocator for this, determine if we need one.
    let bytes =
        unsafe { std::slice::from_raw_parts((*packet).mac_frame_buffer, (*packet).mac_frame_size) }
            .into();
    let rx_info = unsafe { (*packet).info };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::MacFrameRx { bytes, rx_info }).map_err(|e| {
        error!("Failed to receive frame: {:?}", e);
    });
}
#[no_mangle]
extern "C" fn handle_complete_tx(
    _ctx: &mut crate::DriverEventSink,
    _packet: *const WlanTxPacket,
    _status: i32,
) {
    // TODO(https://fxbug.dev/85924): Implement this to support asynchronous packet delivery.
}
#[no_mangle]
extern "C" fn handle_report_tx_result(
    ctx: &mut crate::DriverEventSink,
    tx_result_in: *const banjo_common::WlanTxResult,
) {
    if tx_result_in.is_null() {
        return;
    }
    let tx_result = unsafe { *tx_result_in };
    let _ = ctx.0.unbounded_send(crate::DriverEvent::TxResultReport { tx_result });
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
    report_tx_result: handle_report_tx_result,
    scan_complete: handle_scan_complete,
};

impl<'a> WlanSoftmacIfcProtocol<'a> {
    pub fn new(sink: &'a mut crate::DriverEventSink) -> Self {
        // Const reference has 'static lifetime, so it's safe to pass down to the driver.
        let ops = &PROTOCOL_OPS;
        Self { ops, ctx: sink }
    }
}

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
}

pub mod test_utils {
    use {
        super::*,
        crate::{
            buffer::{BufferProvider, FakeBufferProvider},
            ddk_converter,
            error::Error,
        },
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
        fidl_fuchsia_wlan_internal as fidl_internal, fidl_fuchsia_wlan_sme as fidl_sme,
        fuchsia_async as fasync,
        fuchsia_zircon::HandleBased,
        ieee80211::Bssid,
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
        pub mac_role: fidl_common::WlanMacRole,
        pub sta_addr: Bssid,
        pub start_passive_scan_fails: bool,
        pub start_active_scan_fails: bool,
        pub send_wlan_frame_fails: bool,
    }

    impl Default for FakeDeviceConfig {
        fn default() -> Self {
            Self {
                mac_role: fidl_common::WlanMacRole::Client,
                sta_addr: [7u8; 6].into(),
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
        mlme_event_sink: mpsc::UnboundedSender<fidl_mlme::MlmeEvent>,
    }

    pub struct FakeDeviceState {
        pub config: FakeDeviceConfig,
        pub minstrel: Option<crate::MinstrelWrapper>,
        pub eth_queue: Vec<Vec<u8>>,
        pub wlan_queue: Vec<(Vec<u8>, u32)>,
        pub mlme_event_stream: Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>>,
        pub mlme_request_sink: mpsc::UnboundedSender<wlan_sme::MlmeRequest>,
        pub mlme_request_stream: Option<mpsc::UnboundedReceiver<wlan_sme::MlmeRequest>>,
        pub usme_bootstrap_client_end:
            Option<fidl::endpoints::ClientEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub usme_bootstrap_server_end:
            Option<fidl::endpoints::ServerEnd<fidl_sme::UsmeBootstrapMarker>>,
        pub wlan_channel: fidl_common::WlanChannel,
        pub keys: Vec<fidl_softmac::WlanKeyConfiguration>,
        pub next_scan_id: u64,
        pub captured_passive_scan_request:
            Option<fidl_softmac::WlanSoftmacBridgeStartPassiveScanRequest>,
        pub captured_active_scan_request: Option<fidl_softmac::WlanSoftmacStartActiveScanRequest>,
        pub query_response: fidl_softmac::WlanSoftmacQueryResponse,
        pub discovery_support: fidl_common::DiscoverySupport,
        pub mac_sublayer_support: fidl_common::MacSublayerSupport,
        pub security_support: fidl_common::SecuritySupport,
        pub spectrum_management_support: fidl_common::SpectrumManagementSupport,
        pub join_bss_request: Option<fidl_common::JoinBssRequest>,
        pub beacon_config: Option<(Vec<u8>, usize, TimeUnit)>,
        pub link_status: LinkStatus,
        pub assocs: std::collections::HashMap<MacAddr, fidl_softmac::WlanAssociationConfig>,
        pub buffer_provider: BufferProvider,
        pub install_key_results: VecDeque<Result<(), zx::Status>>,
        pub captured_update_wmm_parameters_request:
            Option<fidl_softmac::WlanSoftmacBridgeUpdateWmmParametersRequest>,
    }

    impl FakeDevice {
        pub fn new(executor: &fasync::TestExecutor) -> (FakeDevice, Arc<Mutex<FakeDeviceState>>) {
            Self::new_with_config(executor, FakeDeviceConfig::default())
        }
        pub fn new_with_config(
            _executor: &fasync::TestExecutor,
            config: FakeDeviceConfig,
        ) -> (FakeDevice, Arc<Mutex<FakeDeviceState>>) {
            let query_response = fidl_softmac::WlanSoftmacQueryResponse {
                sta_addr: Some(config.sta_addr.to_array()),
                mac_role: Some(config.mac_role),
                supported_phys: Some(vec![
                    fidl_common::WlanPhyType::Dsss,
                    fidl_common::WlanPhyType::Hr,
                    fidl_common::WlanPhyType::Ofdm,
                    fidl_common::WlanPhyType::Erp,
                    fidl_common::WlanPhyType::Ht,
                    fidl_common::WlanPhyType::Vht,
                ]),
                hardware_capability: Some(0),
                band_caps: Some(fake_band_caps()),
                ..Default::default()
            };

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
                mlme_event_stream: Some(mlme_event_stream),
                mlme_request_sink,
                mlme_request_stream: Some(mlme_request_stream),
                usme_bootstrap_client_end: Some(usme_bootstrap_client_end),
                usme_bootstrap_server_end: Some(usme_bootstrap_server_end),
                wlan_channel: fidl_common::WlanChannel {
                    primary: 0,
                    cbw: fidl_common::ChannelBandwidth::Cbw20,
                    secondary80: 0,
                },
                next_scan_id: 0,
                captured_passive_scan_request: None,
                captured_active_scan_request: None,
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
                install_key_results: VecDeque::new(),
                captured_update_wmm_parameters_request: None,
            }));
            (FakeDevice { state: state.clone(), mlme_event_sink }, state)
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
            // TODO(https://fxbug.dev/45464): Capture _ifc and provide a testing surface.
            Ok(usme_bootstrap_server_end_handle)
        }

        fn wlan_softmac_query_response(
            &mut self,
        ) -> Result<fidl_softmac::WlanSoftmacQueryResponse, zx::Status> {
            Ok(self.state.lock().unwrap().query_response.clone())
        }

        fn discovery_support(&mut self) -> Result<fidl_common::DiscoverySupport, zx::Status> {
            Ok(self.state.lock().unwrap().discovery_support)
        }

        fn mac_sublayer_support(&mut self) -> Result<fidl_common::MacSublayerSupport, zx::Status> {
            Ok(self.state.lock().unwrap().mac_sublayer_support)
        }

        fn security_support(&mut self) -> Result<fidl_common::SecuritySupport, zx::Status> {
            Ok(self.state.lock().unwrap().security_support)
        }

        fn spectrum_management_support(
            &mut self,
        ) -> Result<fidl_common::SpectrumManagementSupport, zx::Status> {
            Ok(self.state.lock().unwrap().spectrum_management_support)
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
            wlan_channel: fidl_common::WlanChannel,
        ) -> Result<(), zx::Status> {
            self.state.lock().unwrap().wlan_channel = wlan_channel;
            Ok(())
        }

        fn start_passive_scan(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBridgeStartPassiveScanRequest,
        ) -> Result<fidl_softmac::WlanSoftmacBridgeStartPassiveScanResponse, zx::Status> {
            let mut state = self.state.lock().unwrap();
            if state.config.start_passive_scan_fails {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let scan_id = state.next_scan_id;
            state.next_scan_id += 1;
            state.captured_passive_scan_request.replace(request.clone());
            Ok(fidl_softmac::WlanSoftmacBridgeStartPassiveScanResponse {
                scan_id: Some(scan_id),
                ..Default::default()
            })
        }

        fn start_active_scan(
            &mut self,
            request: &fidl_softmac::WlanSoftmacStartActiveScanRequest,
        ) -> Result<fidl_softmac::WlanSoftmacBridgeStartActiveScanResponse, zx::Status> {
            let mut state = self.state.lock().unwrap();
            if state.config.start_active_scan_fails {
                return Err(zx::Status::NOT_SUPPORTED);
            }
            let scan_id = state.next_scan_id;
            state.next_scan_id += 1;
            state.captured_active_scan_request.replace(request.clone());
            Ok(fidl_softmac::WlanSoftmacBridgeStartActiveScanResponse {
                scan_id: Some(scan_id),
                ..Default::default()
            })
        }

        fn cancel_scan(
            &mut self,
            _request: &fidl_softmac::WlanSoftmacBridgeCancelScanRequest,
        ) -> Result<(), zx::Status> {
            Err(zx::Status::NOT_SUPPORTED)
        }

        fn join_bss(&mut self, request: &fidl_common::JoinBssRequest) -> Result<(), zx::Status> {
            self.state.lock().unwrap().join_bss_request.replace(request.clone());
            Ok(())
        }

        fn enable_beaconing(
            &mut self,
            request: fidl_softmac::WlanSoftmacBridgeEnableBeaconingRequest,
        ) -> Result<(), zx::Status> {
            match (request.packet_template, request.tim_ele_offset, request.beacon_interval) {
                (Some(packet_template), Some(tim_ele_offset), Some(beacon_interval)) => Ok({
                    self.state.lock().unwrap().beacon_config = Some((
                        packet_template.mac_frame.clone(),
                        usize::try_from(tim_ele_offset).map_err(|_| zx::Status::INTERNAL)?,
                        TimeUnit(beacon_interval),
                    ));
                }),
                _ => Err(zx::Status::INVALID_ARGS),
            }
        }

        fn disable_beaconing(&mut self) -> Result<(), zx::Status> {
            self.state.lock().unwrap().beacon_config = None;
            Ok(())
        }

        fn install_key(
            &mut self,
            key_configuration: &fidl_softmac::WlanKeyConfiguration,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            state.keys.push(key_configuration.clone());
            state.install_key_results.pop_front().unwrap_or(Ok(()))
        }

        fn notify_association_complete(
            &mut self,
            cfg: fidl_softmac::WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            if let Some(minstrel) = &state.minstrel {
                minstrel.lock().add_peer(&cfg)?
            }
            state.assocs.insert(cfg.bssid.unwrap().into(), cfg);
            Ok(())
        }

        fn clear_association(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBridgeClearAssociationRequest,
        ) -> Result<(), zx::Status> {
            let addr: MacAddr = request.peer_addr.unwrap().into();
            let mut state = self.state.lock().unwrap();
            if let Some(minstrel) = &state.minstrel {
                minstrel.lock().remove_peer(&addr);
            }
            state.assocs.remove(&addr);
            state.join_bss_request = None;
            Ok(())
        }

        fn update_wmm_parameters(
            &mut self,
            request: &fidl_softmac::WlanSoftmacBridgeUpdateWmmParametersRequest,
        ) -> Result<(), zx::Status> {
            let mut state = self.state.lock().unwrap();
            state.captured_update_wmm_parameters_request.replace(request.clone());
            Ok(())
        }

        fn take_mlme_event_stream(
            &mut self,
        ) -> Option<mpsc::UnboundedReceiver<fidl_mlme::MlmeEvent>> {
            self.state.lock().unwrap().mlme_event_stream.take()
        }

        fn send_mlme_event(&mut self, event: fidl_mlme::MlmeEvent) -> Result<(), anyhow::Error> {
            self.mlme_event_sink.unbounded_send(event).map_err(|e| e.into())
        }

        fn set_minstrel(&mut self, minstrel: crate::MinstrelWrapper) {
            self.state.lock().unwrap().minstrel.replace(minstrel);
        }

        fn minstrel(&mut self) -> Option<crate::MinstrelWrapper> {
            self.state.lock().unwrap().minstrel.as_ref().map(Arc::clone)
        }
    }

    pub fn fake_band_caps() -> Vec<fidl_softmac::WlanSoftmacBandCapability> {
        vec![
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::TwoGhz),
                basic_rates: Some(vec![
                    0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c,
                ]),
                operating_channels: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
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
                }),
                vht_supported: Some(false),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities { bytes: Default::default() }),
                ..Default::default()
            },
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x02, 0x04, 0x0b, 0x16, 0x30, 0x60, 0x7e, 0x7f]),
                operating_channels: Some(vec![36, 40, 44, 48, 149, 153, 157, 161]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
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
                }),
                vht_supported: Some(true),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities {
                    bytes: [0x32, 0x50, 0x80, 0x0f, 0xfe, 0xff, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00],
                }),
                ..Default::default()
            },
        ]
    }

    pub fn fake_mlme_band_caps() -> Vec<fidl_mlme::BandCapability> {
        fake_band_caps()
            .into_iter()
            .map(ddk_converter::mlme_band_cap_from_softmac)
            .collect::<Result<_, _>>()
            .expect("Failed to convert softmac driver band capabilities.")
    }

    pub fn fake_discovery_support() -> fidl_common::DiscoverySupport {
        fidl_common::DiscoverySupport {
            scan_offload: fidl_common::ScanOffloadExtension {
                supported: true,
                scan_cancel_supported: false,
            },
            probe_response_offload: fidl_common::ProbeResponseOffloadExtension { supported: false },
        }
    }

    pub fn fake_mac_sublayer_support() -> fidl_common::MacSublayerSupport {
        fidl_common::MacSublayerSupport {
            rate_selection_offload: fidl_common::RateSelectionOffloadExtension { supported: false },
            data_plane: fidl_common::DataPlaneExtension {
                data_plane_type: fidl_common::DataPlaneType::EthernetDevice,
            },
            device: fidl_common::DeviceExtension {
                is_synthetic: true,
                mac_implementation_type: fidl_common::MacImplementationType::Softmac,
                tx_status_report_supported: true,
            },
        }
    }

    pub fn fake_security_support() -> fidl_common::SecuritySupport {
        fidl_common::SecuritySupport {
            mfp: fidl_common::MfpFeature { supported: false },
            sae: fidl_common::SaeFeature {
                driver_handler_supported: false,
                sme_handler_supported: false,
            },
        }
    }

    pub fn fake_spectrum_management_support() -> fidl_common::SpectrumManagementSupport {
        fidl_common::SpectrumManagementSupport { dfs: fidl_common::DfsFeature { supported: true } }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{ddk_converter, WlanTxPacketExt as _},
        fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211,
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
        let query_response = fake_device.wlan_softmac_query_response().unwrap();
        assert_eq!(query_response.sta_addr, [7u8; 6].into());
        assert_eq!(query_response.mac_role, Some(fidl_common::WlanMacRole::Client));
        assert_eq!(
            query_response.supported_phys,
            Some(vec![
                fidl_common::WlanPhyType::Dsss,
                fidl_common::WlanPhyType::Hr,
                fidl_common::WlanPhyType::Ofdm,
                fidl_common::WlanPhyType::Erp,
                fidl_common::WlanPhyType::Ht,
                fidl_common::WlanPhyType::Vht,
            ]),
        );
        assert_eq!(query_response.hardware_capability, Some(0));

        let expected_band_caps = [
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::TwoGhz),
                basic_rates: Some(vec![
                    0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c,
                ]),
                operating_channels: Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
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
                }),
                vht_supported: Some(false),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities { bytes: Default::default() }),
                ..Default::default()
            },
            fidl_softmac::WlanSoftmacBandCapability {
                band: Some(fidl_common::WlanBand::FiveGhz),
                basic_rates: Some(vec![0x02, 0x04, 0x0b, 0x16, 0x30, 0x60, 0x7e, 0x7f]),
                operating_channels: Some(vec![36, 40, 44, 48, 149, 153, 157, 161]),
                ht_supported: Some(true),
                ht_caps: Some(fidl_ieee80211::HtCapabilities {
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
                }),
                vht_supported: Some(true),
                vht_caps: Some(fidl_ieee80211::VhtCapabilities {
                    bytes: [0x32, 0x50, 0x80, 0x0f, 0xfe, 0xff, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00],
                }),
                ..Default::default()
            },
        ];
        let actual_band_caps = query_response.band_caps.as_ref().unwrap();
        for (actual_band_cap, expected_band_cap) in actual_band_caps.iter().zip(&expected_band_caps)
        {
            assert_eq!(actual_band_cap, expected_band_cap);
        }
    }

    #[test]
    fn fake_device_returns_expected_discovery_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let discovery_support = fake_device.discovery_support().unwrap();
        assert_eq!(
            discovery_support,
            fidl_common::DiscoverySupport {
                scan_offload: fidl_common::ScanOffloadExtension {
                    supported: true,
                    scan_cancel_supported: false,
                },
                probe_response_offload: fidl_common::ProbeResponseOffloadExtension {
                    supported: false,
                },
            }
        );
    }

    #[test]
    fn fake_device_returns_expected_mac_sublayer_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let mac_sublayer_support = fake_device.mac_sublayer_support().unwrap();
        assert_eq!(
            mac_sublayer_support,
            fidl_common::MacSublayerSupport {
                rate_selection_offload: fidl_common::RateSelectionOffloadExtension {
                    supported: false,
                },
                data_plane: fidl_common::DataPlaneExtension {
                    data_plane_type: fidl_common::DataPlaneType::EthernetDevice,
                },
                device: fidl_common::DeviceExtension {
                    is_synthetic: true,
                    mac_implementation_type: fidl_common::MacImplementationType::Softmac,
                    tx_status_report_supported: true,
                },
            }
        );
    }

    #[test]
    fn fake_device_returns_expected_security_support() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, _) = FakeDevice::new(&exec);
        let security_support = fake_device.security_support().unwrap();
        assert_eq!(
            security_support,
            fidl_common::SecuritySupport {
                mfp: fidl_common::MfpFeature { supported: false },
                sae: fidl_common::SaeFeature {
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
        let spectrum_management_support = fake_device.spectrum_management_support().unwrap();
        assert_eq!(
            spectrum_management_support,
            fidl_common::SpectrumManagementSupport {
                dfs: fidl_common::DfsFeature { supported: true },
            }
        );
    }

    #[test]
    fn test_can_dynamically_change_fake_device_state() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        let query_response = fake_device.wlan_softmac_query_response().unwrap();
        assert_eq!(query_response.mac_role, Some(fidl_common::WlanMacRole::Client));

        fake_device_state.lock().unwrap().query_response = fidl_softmac::WlanSoftmacQueryResponse {
            mac_role: Some(fidl_common::WlanMacRole::Ap),
            ..query_response
        };

        let query_response = fake_device.wlan_softmac_query_response().unwrap();
        assert_eq!(query_response.mac_role, Some(fidl_common::WlanMacRole::Ap));
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
    fn set_channel() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .set_channel(fidl_common::WlanChannel {
                primary: 2,
                cbw: fidl_common::ChannelBandwidth::Cbw80P80,
                secondary80: 4,
            })
            .expect("set_channel failed?");
        // Check the internal state.
        assert_eq!(
            fake_device_state.lock().unwrap().wlan_channel,
            fidl_common::WlanChannel {
                primary: 2,
                cbw: fidl_common::ChannelBandwidth::Cbw80P80,
                secondary80: 4
            }
        );
    }

    #[test]
    fn install_key() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .install_key(&fidl_softmac::WlanKeyConfiguration {
                protection: Some(fidl_softmac::WlanProtection::None),
                cipher_oui: Some([3, 4, 5]),
                cipher_type: Some(6),
                key_type: Some(fidl_common::WlanKeyType::Pairwise),
                peer_addr: Some([8; 6]),
                key_idx: Some(9),
                key: Some(vec![11; 32]),
                rsc: Some(12),
                ..Default::default()
            })
            .expect("error setting key");
        assert_eq!(fake_device_state.lock().unwrap().keys.len(), 1);
    }

    #[test]
    fn start_passive_scan() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);

        let result = fake_device.start_passive_scan(
            &fidl_softmac::WlanSoftmacBridgeStartPassiveScanRequest {
                channels: Some(vec![1u8, 2, 3]),
                min_channel_time: Some(zx::Duration::from_millis(0).into_nanos()),
                max_channel_time: Some(zx::Duration::from_millis(200).into_nanos()),
                min_home_time: Some(0),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        assert_eq!(
            fake_device_state.lock().unwrap().captured_passive_scan_request,
            Some(fidl_softmac::WlanSoftmacBridgeStartPassiveScanRequest {
                channels: Some(vec![1, 2, 3]),
                min_channel_time: Some(0),
                max_channel_time: Some(200_000_000),
                min_home_time: Some(0),
                ..Default::default()
            }),
        );
    }

    #[test]
    fn start_active_scan() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);

        let result =
            fake_device.start_active_scan(&fidl_softmac::WlanSoftmacStartActiveScanRequest {
                channels: Some(vec![1u8, 2, 3]),
                ssids: Some(vec![
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("foo").unwrap().into(),
                    ),
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("bar").unwrap().into(),
                    ),
                ]),
                mac_header: Some(vec![
                    0x40u8, 0x00, // Frame Control
                    0x00, 0x00, // Duration
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                    0x70, 0xdc, // Sequence Control
                ]),
                ies: Some(vec![
                    0x01u8, // Element ID for Supported Rates
                    0x08,   // Length
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // Supported Rates
                ]),
                min_channel_time: Some(zx::Duration::from_millis(0).into_nanos()),
                max_channel_time: Some(zx::Duration::from_millis(200).into_nanos()),
                min_home_time: Some(0),
                min_probes_per_channel: Some(1),
                max_probes_per_channel: Some(3),
                ..Default::default()
            });
        assert!(result.is_ok());
        assert_eq!(
            fake_device_state.lock().unwrap().captured_active_scan_request,
            Some(fidl_softmac::WlanSoftmacStartActiveScanRequest {
                channels: Some(vec![1, 2, 3]),
                ssids: Some(vec![
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("foo").unwrap().into()
                    ),
                    ddk_converter::cssid_from_ssid_unchecked(
                        &Ssid::try_from("bar").unwrap().into()
                    ),
                ]),
                mac_header: Some(vec![
                    0x40, 0x00, // Frame Control
                    0x00, 0x00, // Duration
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 1
                    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, // Address 2
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Address 3
                    0x70, 0xdc, // Sequence Control
                ]),
                ies: Some(vec![
                    0x01, // Element ID for Supported Rates
                    0x08, // Length
                    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 // Supported Rates
                ]),
                min_channel_time: Some(0),
                max_channel_time: Some(200_000_000),
                min_home_time: Some(0),
                min_probes_per_channel: Some(1),
                max_probes_per_channel: Some(3),
                ..Default::default()
            }),
            "No active scan argument available."
        );
    }

    #[test]
    fn join_bss() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .join_bss(&fidl_common::JoinBssRequest {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                bss_type: Some(fidl_common::BssType::Personal),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
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
        let mac_frame = in_buf.as_slice().to_vec();

        fake_device
            .enable_beaconing(fidl_softmac::WlanSoftmacBridgeEnableBeaconingRequest {
                packet_template: Some(fidl_softmac::WlanTxPacket::template(mac_frame)),
                tim_ele_offset: Some(1),
                beacon_interval: Some(2),
                ..Default::default()
            })
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
        assert!(fake_device_state.lock().unwrap().assocs.contains_key(&[1, 2, 3, 4, 5, 6].into()));
    }

    #[test]
    fn clear_association() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);
        fake_device
            .join_bss(&fidl_common::JoinBssRequest {
                bssid: Some([1, 2, 3, 4, 5, 6]),
                bss_type: Some(fidl_common::BssType::Personal),
                remote: Some(true),
                beacon_period: Some(100),
                ..Default::default()
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
        fake_device
            .clear_association(&fidl_softmac::WlanSoftmacBridgeClearAssociationRequest {
                peer_addr: Some([1, 2, 3, 4, 5, 6]),
                ..Default::default()
            })
            .expect("error clearing assoc");
        assert_eq!(fake_device_state.lock().unwrap().assocs.len(), 0);
        assert!(fake_device_state.lock().unwrap().join_bss_request.is_none());
    }

    #[test]
    fn fake_device_captures_update_wmm_parameters_request() {
        let exec = fasync::TestExecutor::new();
        let (mut fake_device, fake_device_state) = FakeDevice::new(&exec);

        let request = fidl_softmac::WlanSoftmacBridgeUpdateWmmParametersRequest {
            ac: Some(fidl_ieee80211::WlanAccessCategory::Background),
            params: Some(fidl_common::WlanWmmParameters {
                apsd: true,
                ac_be_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 10,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
                ac_bk_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 11,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
                ac_vi_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 12,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
                ac_vo_params: fidl_common::WlanWmmAccessCategoryParameters {
                    ecw_min: 13,
                    ecw_max: 100,
                    aifsn: 1,
                    txop_limit: 5,
                    acm: true,
                },
            }),
            ..Default::default()
        };
        let result = fake_device.update_wmm_parameters(&request);
        assert!(result.is_ok());

        assert_eq!(
            fake_device_state.lock().unwrap().captured_update_wmm_parameters_request,
            Some(request),
        );
    }
}
