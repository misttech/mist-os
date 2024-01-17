// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! C bindings for wlansoftmac-rust crate.

// Explicitly declare usage for cbindgen.

use {
    fuchsia_zircon as zx,
    std::ffi::c_void,
    tracing::error,
    wlan_mlme::{
        buffer::BufferProvider,
        device::{completers::StopStaCompleter, DeviceInterface},
    },
    wlan_span::CSpan,
    wlansoftmac_rust::{start_wlansoftmac, WlanSoftmacHandle},
};

/// Cast a *mut c_void to a usize. This is normally done to workaround the Send trait which *mut c_void does
/// not implement
///
/// # Safety
///
/// The caller must only cast the usize back into a *mut c_void in the same address space.  Otherwise, the
/// *mut c_void may not be valid.
unsafe fn ptr_as_usize(ptr: *mut c_void) -> usize {
    ptr as usize
}

/// Cast a usize to a *mut c_void. This is normally done to workaround the Send trait which *mut c_void does
/// not implement
///
/// # Safety
///
/// The caller must only cast a usize into a *mut c_void if its known the *mut c_void will be used in the
/// same address space where it originated.  Otherwise, the *mut c_void may not be valid.
unsafe fn usize_as_ptr(u: usize) -> *mut c_void {
    u as *mut c_void
}

/// FFI interface: Start the Rust portion of the wlansoftmac driver which will implement
/// an MLME server and an SME server.
///
/// # Safety
///
/// The caller of this function should provide raw pointers that will be valid in the address space
/// where the Rust portion of wlansoftmac will run.
#[no_mangle]
pub unsafe extern "C" fn start_sta(
    completer: *mut c_void,
    run_completer: extern "C" fn(
        completer: *mut c_void,
        status: zx::zx_status_t,
        wlan_softmac_handle: *mut WlanSoftmacHandle,
    ),
    device: DeviceInterface,
    buf_provider: BufferProvider,
    wlan_softmac_bridge_client_handle: zx::sys::zx_handle_t,
) {
    // SAFETY: Cast *mut c_void to usize so the constructed lambda will be Send. This is safe since
    // StartStaCompleter will not move to a thread in a different address space.
    let completer = unsafe { ptr_as_usize(completer) };
    start_wlansoftmac(
        move |result: Result<WlanSoftmacHandle, zx::Status>| match result {
            Ok(handle) => {
                run_completer(
                    unsafe { usize_as_ptr(completer) },
                    zx::Status::OK.into_raw(),
                    Box::into_raw(Box::new(handle)),
                );
            }
            Err(status) => {
                run_completer(completer as *mut c_void, status.into_raw(), std::ptr::null_mut());
            }
        },
        device,
        buf_provider,
        wlan_softmac_bridge_client_handle,
    )
}

/// FFI interface: Stop a WlanSoftmac via the WlanSoftmacHandle. Takes ownership and invalidates
/// the passed WlanSoftmacHandle.
///
/// # Safety
///
/// This function casts a raw pointer to a WlanSoftmacHandle. This API is fundamentally
/// unsafe, and relies on the caller passing ownership of the correct pointer.
#[no_mangle]
pub unsafe extern "C" fn stop_sta(
    completer: *mut c_void,
    run_completer: extern "C" fn(completer: *mut c_void),
    softmac: *mut WlanSoftmacHandle,
) {
    if softmac.is_null() {
        error!("Call to stop_sta() with NULL pointer!");
        return;
    }
    let softmac = Box::from_raw(softmac);
    // SAFETY: Cast *mut c_void to usize so the constructed lambda will be Send. This is safe since
    // StopStaCompleter will not move to a thread in a different address space.
    let completer = unsafe { ptr_as_usize(completer) };
    softmac.stop(StopStaCompleter::new(Box::new(move || {
        run_completer(unsafe { usize_as_ptr(completer) })
    })));
}

#[no_mangle]
pub extern "C" fn sta_queue_eth_frame_tx(
    softmac: &mut WlanSoftmacHandle,
    frame: CSpan<'_>,
) -> zx::zx_status_t {
    zx::Status::from(softmac.queue_eth_frame_tx(frame.into())).into_raw()
}
