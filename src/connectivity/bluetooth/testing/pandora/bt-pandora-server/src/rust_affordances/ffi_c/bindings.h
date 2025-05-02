// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_RUST_AFFORDANCES_FFI_C_BINDINGS_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_RUST_AFFORDANCES_FFI_C_BINDINGS_H_

#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <new>
#include <ostream>

extern "C" {

/// Stop serving Rust affordances.
///
/// Returns ZX_STATUS_BAD_STATE if Rust affordances are not running.
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

}  // extern "C"

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TESTING_PANDORA_BT_PANDORA_SERVER_SRC_RUST_AFFORDANCES_FFI_C_BINDINGS_H_
