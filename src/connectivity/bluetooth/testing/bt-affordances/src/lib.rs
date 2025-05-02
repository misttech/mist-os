// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_bluetooth::PeerId;
use fuchsia_bt_test_affordances::WorkThread;
use futures::executor::block_on;
use std::ffi::CStr;
use std::sync::LazyLock;
use zx::sys::zx_status_t;

static WORKER: LazyLock<WorkThread> = LazyLock::new(|| WorkThread::spawn());

/// Stop serving Rust affordances.
///
/// Returns ZX_STATUS_BAD_STATE if Rust affordances are not running.
/// Returns ZX_STATUS_INTERNAL if Rust affordances exited with an error (check logs).
#[no_mangle]
pub extern "C" fn stop_rust_affordances() -> zx_status_t {
    println!("Stopping Rust affordances");
    WORKER.join()
}

/// Populates `addr_byte_buff` with public address of active host.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
///
/// # Safety
///
/// The caller must ensure that `addr_byte_buff` points to a valid buffer of 6 bytes.
#[no_mangle]
pub extern "C" fn read_local_address(addr_byte_buff: *mut u8) -> zx_status_t {
    if let Err(err) = block_on(WORKER.read_local_address(addr_byte_buff)) {
        eprintln!("read_local_address encountered error: {}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Get identifier of peer with given `address`.
///
/// Returns 0 on error.
///
/// # Safety
///
/// The caller must ensure that `address` points to a valid C string encoding a BD_ADDR as a string
/// of bytes in little-endian order.
#[no_mangle]
pub unsafe extern "C" fn get_peer_id(address: *const core::ffi::c_char) -> u64 {
    let address = CStr::from_ptr(address);
    match block_on(WORKER.get_peer_id(address)) {
        Ok(peer_id) => peer_id.value,
        Err(err) => {
            eprintln!("connect_peer encountered error: {}", err);
            0
        }
    }
}

/// Connect to peer with given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn connect_peer(peer_id: u64) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(WORKER.connect_peer(peer_id)) {
        eprintln!("connect_peer encountered error: {}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Remove all bonding information and disconnect peer with given identifier, if found.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn forget_peer(peer_id: u64) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(WORKER.forget_peer(peer_id)) {
        eprintln!("forget_peer encountered error: {:?}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Connect an L2CAP channel on a specific PSM to an already-connected peer. Calling this again will
/// result in the channel being closed after the new channel is opened.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn connect_l2cap_channel(peer_id: u64, psm: u16) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(WORKER.connect_l2cap_channel(peer_id, psm)) {
        eprintln!("connect_l2cap_channel encountered error: {:?}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Start or revoke discoverability.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn set_discoverability(discoverable: bool) -> zx_status_t {
    if let Err(err) = block_on(WORKER.set_discoverability(discoverable)) {
        eprintln!("set_discoverability encountered error: {:?}", err);
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}
