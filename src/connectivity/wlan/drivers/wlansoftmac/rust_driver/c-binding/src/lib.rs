// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! C bindings for wlansoftmac-rust crate.

use {
    diagnostics_log::PublishOptions,
    fidl_fuchsia_wlan_softmac as fidl_softmac,
    fuchsia_async::LocalExecutor,
    fuchsia_zircon as zx,
    std::{ffi::c_void, sync::Once},
    wlan_ffi_transport::{
        BufferProvider, EthernetRx, FfiBufferProvider, FfiEthernetRx, FfiWlanTx, WlanTx,
    },
    wlan_mlme::device::Device,
};

static LOGGER_ONCE: Once = Once::new();

/// Start and run a bridged wlansoftmac driver hosting an MLME server and an SME server.
///
/// The driver is "bridged" in the sense that it requires a bridge to a Fuchsia driver to
/// communicate with other Fuchsia drivers over the FDF transport. When initialization of the
/// bridged driver completes, `run_init_completer` will be called.
///
/// # Safety
///
/// There are two layers of safety documentation for this function. The first layer is for this
/// function itself, and the second is for the `run_init_completer` function.
///
/// ## For this function itself
///
/// This function is unsafe for the following reasons:
///
///   - This function cannot guarantee `run_init_completer` is thread-safe, i.e., that it's safe to
///     to call at any time from any thread.
///   - This function cannot guarantee `init_completer` points to a valid object when
///     `run_init_completer` is called.
///   - This function cannot guarantee `wlan_softmac_bridge_client_handle` is a valid handle.
///
/// By calling this function, the caller promises the following:
///
///   - The `run_init_completer` function is thread-safe.
///   - The `init_completer` pointer will point to a valid object at least until
///     `run_init_completer` is called.
///   - The `wlan_softmac_bridge_client_handle` is a valid handle.
///
/// ## For `run_init_completer`
///
/// The `run_init_completer` function is unsafe because it cannot guarantee the `init_completer`
/// argument will be the same `init_completer` passed to `start_and_run_bridged_wlansoftmac`, and
/// cannot guarantee it will be called exactly once.
///
/// The caller of `run_init_completer` must promise to pass the same `init_completer` from
/// `start_and_run_bridged_wlansoftmac` to `run_init_completer` and call `run_init_completer`
/// exactly once.
#[no_mangle]
pub unsafe extern "C" fn start_and_run_bridged_wlansoftmac(
    init_completer: *mut c_void,
    run_init_completer: unsafe extern "C" fn(init_completer: *mut c_void, status: zx::zx_status_t),
    ethernet_rx: FfiEthernetRx,
    wlan_tx: FfiWlanTx,
    buffer_provider: FfiBufferProvider,
    wlan_softmac_bridge_client_handle: zx::sys::zx_handle_t,
) -> zx::sys::zx_status_t {
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
        move |result: Result<(), zx::Status>| match result {
            Ok(()) => {
                // Safety: This is safe because the caller of this function promised
                // `run_init_completer` is thread-safe and `init_completer` is valid until
                // its called.
                unsafe {
                    run_init_completer(init_completer, zx::Status::OK.into_raw());
                }
            }
            Err(status) => {
                // Safety: This is safe because the caller of this function promised
                // `run_init_completer` is thread-safe and `init_completer` is valid until
                // its called.
                unsafe {
                    run_init_completer(init_completer, status.into_raw());
                }
            }
        },
        device,
        BufferProvider::new(buffer_provider),
    ));
    zx::Status::from(result).into_raw()
}
