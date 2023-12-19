// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! C bindings for wlansoftmac-rust crate.

// Explicitly declare usage for cbindgen.

use {
    fuchsia_zircon as zx,
    std::ffi::c_void,
    tracing::error,
    wlan_mlme::{buffer::BufferProvider, device::DeviceInterface},
    wlan_span::CSpan,
    wlansoftmac_rust::{start_wlansoftmac, WlanSoftmacHandle},
};

#[no_mangle]
pub extern "C" fn start_sta(
    completer: *mut c_void,
    run_completer: extern "C" fn(completer: *mut c_void, status: zx::zx_status_t),
    device: DeviceInterface,
    buf_provider: BufferProvider,
    wlan_softmac_bridge_client_handle: zx::sys::zx_handle_t,
) -> *mut WlanSoftmacHandle {
    match start_wlansoftmac(device, buf_provider, wlan_softmac_bridge_client_handle) {
        Ok(handle) => {
            run_completer(completer, zx::Status::OK.into_raw());
            Box::into_raw(Box::new(handle))
        }
        Err(e) => {
            error!("Failed to start WLAN Softmac STA: {}", e);
            run_completer(completer, zx::Status::INTERNAL.into_raw());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn stop_sta(softmac: &mut WlanSoftmacHandle) {
    softmac.stop();
}

/// FFI interface: Stop and delete a WlanSoftmac via the WlanSoftmacHandle.
/// Takes ownership and invalidates the passed WlanSoftmacHandle.
///
/// # Safety
///
/// This fn accepts a raw pointer that is held by the FFI caller as a handle to
/// the Softmac. This API is fundamentally unsafe, and relies on the caller to
/// pass the correct pointer and make no further calls on it later.
#[no_mangle]
pub unsafe extern "C" fn delete_sta(softmac: *mut WlanSoftmacHandle) {
    if !softmac.is_null() {
        let softmac = Box::from_raw(softmac);
        softmac.delete();
    }
}

#[no_mangle]
pub extern "C" fn sta_queue_eth_frame_tx(
    softmac: &mut WlanSoftmacHandle,
    frame: CSpan<'_>,
) -> zx::zx_status_t {
    zx::Status::from(softmac.queue_eth_frame_tx(frame.into())).into_raw()
}
