// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use tracing::error;
use wlan_fullmac_mlme::device::{FullmacDevice, RawFullmacDeviceFfi};
use wlan_fullmac_mlme::{FullmacMlme, FullmacMlmeHandle};
use {fidl_fuchsia_wlan_fullmac as fidl_fullmac, fuchsia_zircon as zx};

#[no_mangle]
pub extern "C" fn start_fullmac_mlme(
    raw_device: RawFullmacDeviceFfi,
    fullmac_client_end_handle: zx::sys::zx_handle_t,
) -> *mut FullmacMlmeHandle {
    let fullmac_impl_sync_proxy = {
        // Safety: This is safe because the caller promises `fullmac_client_end_handle`
        // is a valid handle.
        let handle = unsafe { fidl::Handle::from_raw(fullmac_client_end_handle) };
        let channel = fidl::Channel::from(handle);
        fidl_fullmac::WlanFullmacImpl_SynchronousProxy::new(channel)
    };
    let device = FullmacDevice::new(raw_device, fullmac_impl_sync_proxy);
    match FullmacMlme::start(device) {
        Ok(mlme) => Box::into_raw(Box::new(mlme)),
        Err(e) => {
            error!("Failed to start FullMAC MLME: {}", e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn stop_fullmac_mlme(mlme: &mut FullmacMlmeHandle) {
    mlme.stop();
}

/// FFI interface: Stop and delete a FullMAC MLME via the FullmacMlmeHandle. Takes ownership
/// and invalidates the passed FullmacMlmeHandle.
///
/// # Safety
///
/// This fn accepts a raw pointer that is held by the FFI caller as a handle to
/// the MLME. This API is fundamentally unsafe, and relies on the caller to
/// pass the correct pointer and make no further calls on it later.
#[no_mangle]
pub unsafe extern "C" fn delete_fullmac_mlme(mlme: *mut FullmacMlmeHandle) {
    if !mlme.is_null() {
        let mlme = Box::from_raw(mlme);
        mlme.delete();
    }
}
