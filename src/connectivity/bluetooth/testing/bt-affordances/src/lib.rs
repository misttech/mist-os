// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_bluetooth::{AddressType, PeerId};
use fidl_fuchsia_bluetooth_sys::{BondableMode, PairingOptions, PairingSecurityLevel};
use fuchsia_async::LocalExecutor;
use fuchsia_bt_test_affordances::WorkThread;
use fuchsia_sync::Mutex;
use futures::channel::mpsc::UnboundedReceiver;
use futures::channel::oneshot;
use futures::executor::block_on;
use futures::StreamExt;
use std::ffi::{c_void, CStr};
use std::sync::LazyLock;
use std::thread::JoinHandle;
use zx::sys::zx_status_t;

struct State {
    worker: WorkThread,
    // Sender used to notify shutdown.
    le_scan: Mutex<Option<(JoinHandle<()>, oneshot::Sender<()>)>>,
}

impl State {
    const fn init() -> LazyLock<Self> {
        LazyLock::new(|| Self { worker: WorkThread::spawn(), le_scan: Mutex::new(None) })
    }

    fn start_le_scan(
        &self,
        context: FfiPointer,
        cb: LeScanCallbackWrapper,
    ) -> Result<(), anyhow::Error> {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();

        let le_peer_receiver = block_on(self.worker.start_le_scan())?;
        *self.le_scan.lock() = Some((
            std::thread::spawn(move || {
                LocalExecutor::new().run_singlethreaded(Self::le_scan(
                    le_peer_receiver,
                    context,
                    cb,
                    shutdown_receiver,
                ));
            }),
            shutdown_sender,
        ));

        Ok(())
    }

    async fn le_scan(
        mut le_peer_receiver: UnboundedReceiver<Vec<fidl_fuchsia_bluetooth_le::Peer>>,
        context: FfiPointer,
        cb: LeScanCallbackWrapper,
        mut shutdown_receiver: oneshot::Receiver<()>,
    ) {
        loop {
            let le_peers = futures::select! {
                le_peers_result = le_peer_receiver.next() => le_peers_result,
                _ = shutdown_receiver => {
                    println!("Stopping LE scan.");
                    return;
                }
            };

            let Some(le_peers) = le_peers else {
                eprintln!("LE scan stream ended unexpectedly.");
                return;
            };

            for found_peer in le_peers {
                Self::process_le_peer(found_peer, context, &cb);
            }
        }
    }

    fn process_le_peer(
        found_peer: fidl_fuchsia_bluetooth_le::Peer,
        context: FfiPointer,
        cb: &LeScanCallbackWrapper,
    ) {
        let mut le_peer = LePeer {
            id: found_peer.id.unwrap().value,
            connectable: found_peer.connectable.unwrap(),
            name: [0; 248],
        };
        if let Some(name) = found_peer.name {
            // SAFETY: Core Spec v6.0 Vol 4, Part E, Section 6.23 specifies the maximum length of a
            // Bluetooth device name as 248 octets, so this copy cannot overflow our buffer. The src
            // & dst of this copy are nonoverlapping regions of memory allocated separately by the
            // client and this library, valid for the duration of this copy.
            unsafe {
                let bytes = name.as_bytes();
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr() as *const core::ffi::c_char,
                    le_peer.name.as_mut_ptr(),
                    bytes.len(),
                );
            }
        }
        cb.0(context.0, &le_peer);
    }
}

static STATE: LazyLock<State> = State::init();

// SAFETY: An `FfiPointer` shall only be dereferenced within one thread in this library to prevent
// concurrent access.
#[derive(Clone, Copy)]
struct FfiPointer(*mut c_void);
unsafe impl Send for FfiPointer {}
unsafe impl Sync for FfiPointer {}

/// Stop serving Rust affordances.
///
/// Returns ZX_STATUS_INTERNAL if Rust affordances exited with an error (check logs).
#[no_mangle]
pub extern "C" fn stop_rust_affordances() -> zx_status_t {
    println!("Stopping Rust affordances");
    if let Err(err) = STATE.worker.join() {
        eprintln!("stop_rust_affordances encountered error: {err}");
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
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
    if let Err(err) = block_on(STATE.worker.read_local_address(addr_byte_buff)) {
        eprintln!("read_local_address encountered error: {err}");
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
    match block_on(STATE.worker.get_peer_id(address)) {
        Ok(peer_id) => peer_id.value,
        Err(err) => {
            eprintln!("get_peer_id encountered error: {err}");
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

    if let Err(err) = block_on(STATE.worker.connect_peer(peer_id)) {
        eprintln!("connect_peer encountered error: {err}");
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Disconnect all logical links (BR/EDR & LE) to peer with given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn disconnect_peer(peer_id: u64) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(STATE.worker.disconnect_peer(peer_id)) {
        eprintln!("disconnect_peer encountered error: {err}");
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Initiate pairing with peer with given identifier.
///
/// `le_security_level` is only relevant for LE pairing. Specify 1 for Encrypted or 2 for
/// Authenticated. All other values are interpreted as unset, defaulting to Authenticated. See
/// fuchsia.bluetooth.sys/PairingOptions for details.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn pair(peer_id: u64, le_security_level: u32, bondable: bool) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    let mut options = PairingOptions::default();
    options.le_security_level = PairingSecurityLevel::from_primitive(le_security_level);
    options.bondable_mode =
        bondable.then_some(BondableMode::Bondable).or(Some(BondableMode::NonBondable));

    if let Err(err) = block_on(STATE.worker.pair(peer_id, options)) {
        eprintln!("pair encountered error: {err}");
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

    if let Err(err) = block_on(STATE.worker.forget_peer(peer_id)) {
        eprintln!("forget_peer encountered error: {err:?}");
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

    if let Err(err) = block_on(STATE.worker.connect_l2cap_channel(peer_id, psm)) {
        eprintln!("connect_l2cap_channel encountered error: {err:?}");
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Start or revoke discoverability.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn set_discoverability(discoverable: bool) -> zx_status_t {
    if let Err(err) = block_on(STATE.worker.set_discoverability(discoverable)) {
        eprintln!("set_discoverability encountered error: {err:?}");
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

#[repr(C)]
pub struct LePeer {
    pub id: u64,
    pub connectable: bool,
    pub name: [core::ffi::c_char; 248],
    // TODO(https://fxbug.dev/396500079): Support more fields as necessary to enable PTS tests.
}

/// `peer` is only valid for the duration of this callback.
type LeScanCallback = extern "C" fn(context: *mut c_void, peer: *const LePeer);
struct LeScanCallbackWrapper(LeScanCallback);
unsafe impl Send for LeScanCallbackWrapper {}
unsafe impl Sync for LeScanCallbackWrapper {}

/// Scan for all nearby LE peripherals and broadcasters.
///
/// The callback `cb` is invoked on every LE peer found or updated. The `context` provided to this
/// function is included in each invocation of `cb`.
///
/// Calling this while a scan is ongoing drops and overwrites the existing scan.
///
/// Returns ZX_STATUS_INTERNAL if scan was unable to start because of an error (check logs).
///
/// # Safety
///
/// The caller must ensure `context` and `cb` point to valid memory & a valid callback.
#[no_mangle]
pub extern "C" fn start_le_scan(context: *mut c_void, cb: LeScanCallback) -> zx_status_t {
    let ffi_ptr = FfiPointer(context);
    let cb_wrapper = LeScanCallbackWrapper(cb);

    if let Err(err) = STATE.start_le_scan(ffi_ptr, cb_wrapper) {
        eprintln!("start_le_scan encountered error: {err:?}");
        return zx::Status::INTERNAL.into_raw();
    }

    zx::Status::OK.into_raw()
}

/// Stop an ongoing LE scan.
///
/// Returns ZX_STATUS_BAD_STATE if no scan was ongoing.
#[no_mangle]
pub extern "C" fn stop_le_scan() -> zx_status_t {
    if STATE.le_scan.lock().is_none() {
        return zx::Status::BAD_STATE.into_raw();
    }

    // The le_scan thread join operation must be non-blocking. We spawn a new thread to handle the
    // shutdown logic to prevent deadlocks if called from the scan callback thread.
    let _ = std::thread::spawn(|| {
        if !block_on(STATE.worker.stop_le_scan()) {
            eprintln!("LE scan could not be stopped (likely stopped already).");
        }

        let le_scan = STATE.le_scan.lock().take().unwrap();
        le_scan.1.send(()).unwrap();
        le_scan.0.join().expect("Failed to join LE scan thread");
    });

    zx::Status::OK.into_raw()
}

/// Connect to an LE peer with the given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
#[no_mangle]
pub extern "C" fn connect_le(peer_id: u64) -> zx_status_t {
    let peer_id = PeerId { value: peer_id };

    if let Err(err) = block_on(STATE.worker.connect_le(peer_id)) {
        eprintln!("connect_le encountered error: {err:?}");
        return zx::Status::INTERNAL.into_raw();
    }
    zx::Status::OK.into_raw()
}

/// Start advertising as an LE peripheral, accept the first connection, and return the PeerId of
/// its initiator.
///
/// `address_type` is 1 for Public or 2 for Random. All other values are interpreted as unset, in
/// which case the address type will be Public or Random depending on if privacy is enabled in the
/// system. See fuchsia.bluetooth.le/AdvertisingParameters for details.
#[no_mangle]
pub extern "C" fn advertise_peripheral(connectable: bool, address_type: u8) -> u64 {
    match block_on(
        STATE.worker.advertise_peripheral(connectable, AddressType::from_primitive(address_type)),
    ) {
        Ok(peer_id) => peer_id.value,
        Err(err) => {
            eprintln!("start_advertising_peripheral encountered error: {err}");
            0
        }
    }
}
