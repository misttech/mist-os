// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_BT_AFFORDANCES_FFI_C_BINDINGS_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_BT_AFFORDANCES_FFI_C_BINDINGS_H_

#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <new>
#include <ostream>

struct LePeer {
  uint64_t id;
  bool connectable;
  char name[248];
};

/// `peer` is only valid for the duration of this callback.
using LeScanCallback = void (*)(void *context, const LePeer *peer);

extern "C" {

/// Stop serving Rust affordances.
///
/// Returns ZX_STATUS_INTERNAL if Rust affordances exited with an error (check logs).
zx_status_t stop_rust_affordances();

/// Populates `addr_byte_buff` with public address of active host.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
///
/// # Safety
///
/// The caller must ensure that `addr_byte_buff` points to a valid buffer of 6 bytes.
zx_status_t read_local_address(uint8_t *addr_byte_buff);

/// Get identifier of peer with given `address`.
///
/// Returns 0 on error.
///
/// # Safety
///
/// The caller must ensure that `address` points to a valid C string encoding a BD_ADDR as a string
/// of bytes in little-endian order.
uint64_t get_peer_id(const char *address);

/// Connect to peer with given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t connect_peer(uint64_t peer_id);

/// Disconnect all logical links (BR/EDR & LE) to peer with given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t disconnect_peer(uint64_t peer_id);

/// Initiate pairing with peer with given identifier.
///
/// `le_security_level` is only relevant for LE pairing. Specify 1 for Encrypted or 2 for
/// Authenticated. All other values are interpreted as unset, defaulting to Authenticated. See
/// fuchsia.bluetooth.sys/PairingOptions for details.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t pair(uint64_t peer_id, uint32_t le_security_level);

/// Remove all bonding information and disconnect peer with given identifier, if found.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t forget_peer(uint64_t peer_id);

/// Connect an L2CAP channel on a specific PSM to an already-connected peer. Calling this again will
/// result in the channel being closed after the new channel is opened.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t connect_l2cap_channel(uint64_t peer_id, uint16_t psm);

/// Start or revoke discoverability.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t set_discoverability(bool discoverable);

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
zx_status_t start_le_scan(void *context, LeScanCallback cb);

/// Stop an ongoing LE scan.
///
/// Returns ZX_STATUS_BAD_STATE if no scan was ongoing.
zx_status_t stop_le_scan();

/// Connect to an LE peer with the given identifier.
///
/// Returns ZX_STATUS_INTERNAL on error (check logs).
zx_status_t connect_le(uint64_t peer_id);

}  // extern "C"

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_BT_AFFORDANCES_FFI_C_BINDINGS_H_
