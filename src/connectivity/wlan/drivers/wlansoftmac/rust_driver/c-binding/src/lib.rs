// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! C bindings for wlansoftmac-rust crate.

use diagnostics_log::PublishOptions;
use fuchsia_async::LocalExecutor;
use std::ffi::c_void;
use std::sync::Once;
use wlan_ffi_transport::{
    BufferProvider, EthernetRx, FfiBufferProvider, FfiEthernetRx, FfiWlanTx, WlanTx,
};
use wlan_mlme::completers::StartCompleter;
use wlan_mlme::device::Device;
use {fidl_fuchsia_wlan_softmac as fidl_softmac, fuchsia_zircon as zx};

static LOGGER_ONCE: Once = Once::new();

/// Start and run a bridged wlansoftmac driver hosting an MLME server and an SME server.
///
/// The driver is "bridged" in the sense that it requires a bridge to a Fuchsia driver to
/// communicate with other Fuchsia drivers over the FDF transport. After the bridged
/// driver starts successfully, `run_start_completer` will be called with `start_completer`.
/// If the bridged driver does not start successfully, this function will return a
/// non-`ZX_OK` status.
///
/// This function returns `ZX_OK` only if shutdown completes successfully. A successful
/// shutdown only occurs if the bridged driver receives a
/// `fuchsia.wlan.softmac/WlanSoftmacIfcBridge.StopBridgedDriver` message and the subsequent
/// teardown of the hosted server succeeds.
///
/// In all other scenarios, e.g., failure during startup, while running, or during shutdown,
/// this function will return a non-`ZX_OK` value.
///
/// # Safety
///
/// This function is unsafe for the following reasons:
///
///   - This function cannot guarantee `run_start_completer` is thread-safe.
///   - This function cannot guarantee `start_completer` points to a valid object when
///     `run_start_completer` is called.
///   - This function cannot guarantee `wlan_softmac_bridge_client_handle` is a valid handle.
///
/// By calling this function, the caller promises the following:
///
///   - The `run_start_completer` function is thread-safe.
///   - The `start_completer` pointer will point to a valid object at least until
///     `run_start_completer` is called.
///   - The `wlan_softmac_bridge_client_handle` is a valid handle.
#[no_mangle]
pub unsafe extern "C" fn start_and_run_bridged_wlansoftmac(
    start_completer: *mut c_void,
    run_start_completer: unsafe extern "C" fn(
        start_completer: *mut c_void,
        status: zx::zx_status_t,
    ),
    ethernet_rx: FfiEthernetRx,
    wlan_tx: FfiWlanTx,
    buffer_provider: FfiBufferProvider,
    wlan_softmac_bridge_client_handle: zx::sys::zx_handle_t,
) -> zx::sys::zx_status_t {
    let start_completer = StartCompleter::new(move |status| {
        // Safety: This is safe because the caller of this function promised
        // `run_start_completer` is thread-safe and `start_completer` is valid until
        // its called.
        unsafe {
            run_start_completer(start_completer, status);
        }
    });

    let mut executor = LocalExecutor::new();

    // The Fuchsia syslog must not be initialized from Rust more than once per process. In the case
    // of MLME, that means we can only call it once for both the client and ap modules. Ensure this
    // by using a shared `Once::call_once()`.
    LOGGER_ONCE.call_once(|| {
        // Initialize logging with a tag that can be used to filter for forwarding to console
        diagnostics_log::initialize_sync(
            PublishOptions::default()
                .tags(&["wlan"])
                .enable_metatag(diagnostics_log::Metatag::Target),
        );
    });

    let wlan_softmac_bridge_proxy = {
        // Safety: This is safe because the caller promises `wlan_softmac_bridge_client_handle`
        // is a valid handle.
        let handle = unsafe { fidl::Handle::from_raw(wlan_softmac_bridge_client_handle) };
        let channel = fidl::Channel::from(handle);
        let channel = fidl::AsyncChannel::from_channel(channel);
        fidl_softmac::WlanSoftmacBridgeProxy::new(channel)
    };
    let device =
        Device::new(wlan_softmac_bridge_proxy, EthernetRx::new(ethernet_rx), WlanTx::new(wlan_tx));

    let result = executor.run_singlethreaded(wlansoftmac_rust::start_and_serve(
        start_completer,
        device,
        BufferProvider::new(buffer_provider),
    ));
    zx::Status::from(result).into_raw()
}
