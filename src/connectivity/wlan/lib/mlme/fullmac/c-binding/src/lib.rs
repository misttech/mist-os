// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_wlan_fullmac as fidl_fullmac;
use log::error;
use std::ffi::c_void;
use wlan_ffi_transport::completers::Completer;
use wlan_fullmac_mlme::device::FullmacDevice;
use wlan_fullmac_mlme::{start_and_serve_on_separate_thread, FullmacMlmeHandle};

/// Starts and runs the FullMAC MLME and SME futures on a separate thread. MLME will call the given
/// `run_shutdown_completer` when it exits.
///
/// # Safety
///
/// This function is unsafe for the following reasons:
///
///   - This function cannot guarantee `fullmac_client_end_handle` is a valid handle.
///   - This function cannot guarantee `run_shutdown_completer` is thread-safe.
///   - This function cannot guarantee `shutdown_completer` points to a valid object when
///     `run_shutdown_completer` is called.
///
/// By calling this function, the caller promises the following:
///
///   - The `fullmac_client_end_handle` is a valid handle.
///   - The `run_shutdown_completer` function is thread-safe.
///   - The `shutdown_completer` pointer will point to a valid object at least until
///     `run_shutdown_completer` is called.
///
/// It is additionally the caller's responsibility to deallocate the returned FullmacMlmeHandle.
/// The caller promises that they will eventually call `delete_fullmac_mlme` on the returned
/// FullmacMlmeHandle.
#[no_mangle]
pub unsafe extern "C" fn start_fullmac_mlme(
    fullmac_client_end_handle: zx::sys::zx_handle_t,
    shutdown_completer: *mut c_void,
    run_shutdown_completer: unsafe extern "C" fn(
        shutdown_completer: *mut c_void,
        status: zx::sys::zx_status_t,
    ),
) -> *mut FullmacMlmeHandle {
    let fullmac_impl_sync_proxy = {
        // Safety: This is safe because the caller promises `fullmac_client_end_handle`
        // is a valid handle.
        let handle = unsafe { fidl::Handle::from_raw(fullmac_client_end_handle) };
        let channel = fidl::Channel::from(handle);
        fidl_fullmac::WlanFullmacImpl_SynchronousProxy::new(channel)
    };

    // Safety: The provided closure is safe to send to another thread because shutdown_completer
    // and run_shutdown_completer are thread-safe.
    let shutdown_completer = unsafe {
        Completer::new_unchecked(move |status| {
            // Safety: This is safe because the caller of this function promised
            // `run_shutdown_completer` is thread-safe and `shutdown_completer` is valid until
            // its called.
            run_shutdown_completer(shutdown_completer, status);
        })
    };

    let device = FullmacDevice::new(fullmac_impl_sync_proxy);
    match start_and_serve_on_separate_thread(device, shutdown_completer) {
        Ok(mlme) => Box::into_raw(Box::new(mlme)),
        Err(e) => {
            error!("Failed to start FullMAC MLME: {}", e);
            std::ptr::null_mut()
        }
    }
}

/// Request that the FullMAC MLME stops. This is non-blocking.
///
/// It is assumed that |mlme| is valid (i.e., the user has not called
/// |delete_fullmac_mlme_handle| yet.
///
/// This should be synchronized with calls to |start_fullmac_mlme| and
/// |delete_fullmac_mlme_handle|.
///
/// TODO(https://fxbug.dev/368323681): Consider replacing |stop_fullmac_mlme| and
/// |delete_fullmac_mlme| with an internal FIDL protocol.
#[no_mangle]
pub extern "C" fn stop_fullmac_mlme(mlme: &mut FullmacMlmeHandle) {
    mlme.request_stop();
}

/// Takes ownership of and deallocates the passed FullmacMlmeHandle.
///
/// If |mlme| is non-null, this will join the FullMAC MLME thread and block the calling thread
/// until FullMAC MLME has exited.
///
/// If the user has not called |stop_fullmac_mlme| before |delete_fullmac_mlme_handle|, this will
/// request that the FullMAC MLME thread stop before joining thread to avoid blocking forever.
///
/// TODO(https://fxbug.dev/368323681): Consider replacing |stop_fullmac_mlme| and
/// |delete_fullmac_mlme| with an internal FIDL protocol.
///
/// # Safety
///
/// This fn accepts a raw pointer that is held by the FFI caller as a handle to
/// the MLME. This API is fundamentally unsafe, and relies on the caller to
/// pass the correct pointer and make no further calls on it later.
#[no_mangle]
pub unsafe extern "C" fn delete_fullmac_mlme_handle(mlme: *mut FullmacMlmeHandle) {
    if !mlme.is_null() {
        let mlme = Box::from_raw(mlme);
        mlme.delete();
    }
}
