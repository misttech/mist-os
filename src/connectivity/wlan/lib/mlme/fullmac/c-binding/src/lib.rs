// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    tracing::error,
    wlan_fullmac_mlme::{
        device::{FullmacDevice, RawFullmacDeviceInterface},
        FullmacMlme, FullmacMlmeHandle,
    },
};

#[no_mangle]
pub extern "C" fn start_fullmac_mlme(
    raw_device: RawFullmacDeviceInterface,
) -> *mut FullmacMlmeHandle {
    let device = FullmacDevice::new(raw_device);
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
