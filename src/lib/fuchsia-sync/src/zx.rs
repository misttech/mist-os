// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::sys;

// We declare our own versions of these functions because the automatically generated versions have
// less ergnomic types. specifically, the `current_value` parameter is a scalar in this version but
// an atomic type in the generated version. Also, the return values in this version are
// sys::zx_status_t instead of a raw integer.
extern "C" {
    pub fn zx_futex_wait(
        value_ptr: *const sys::zx_futex_t,
        current_value: i32,
        new_futex_owner: sys::zx_handle_t,
        deadline: sys::zx_time_t,
    ) -> sys::zx_status_t;

    pub fn zx_futex_wake(value_ptr: *const sys::zx_futex_t, wake_count: u32) -> sys::zx_status_t;
}
