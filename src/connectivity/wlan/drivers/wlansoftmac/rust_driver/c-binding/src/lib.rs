// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! C bindings for wlansoftmac-rust crate.

// Explicitly declare usage for cbindgen.

use {
    fuchsia_zircon as zx,
    std::ffi::c_void,
    wlan_mlme::{
        buffer::BufferProvider,
        device::{
            completers::{StartStaCompleter, StopStaCompleter},
            DeviceInterface,
        },
    },
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
    // Safety: Cast *mut c_void to usize so that the constructed lambda will be Send. This is safe
    // since we don't expect to move StartStaCompleter to a thread in a different address space,
    // i.e., the value of the pointer will still be valid in the thread.
    let completer = completer as usize;
    Box::into_raw(Box::new(start_wlansoftmac(
        StartStaCompleter::new(move |status: Result<(), zx::Status>| {
            run_completer(completer as *mut c_void, zx::Status::from(status).into_raw());
        }),
        device,
        buf_provider,
        wlan_softmac_bridge_client_handle,
    )))
}

#[no_mangle]
pub extern "C" fn stop_sta(
    completer: *mut c_void,
    run_completer: extern "C" fn(completer: *mut c_void),
    softmac: &mut WlanSoftmacHandle,
) {
    // Safety: Cast *mut c_void to usize so that the constructed lambda will be Send. This is safe
    // since we don't expect to move StopStaCompleter to a thread in a different address space,
    // i.e., the value of the pointer will still be valid in the thread.
    let completer = completer as usize;
    softmac.stop(StopStaCompleter::new(Box::new(move || run_completer(completer as *mut c_void))));
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
