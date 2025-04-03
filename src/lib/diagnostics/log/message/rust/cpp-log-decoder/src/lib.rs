// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

use diagnostics_message::{self as message, MonikerWithUrl};
use std::ffi::CString;
use std::os::raw::c_char;

/// # Safety
///
/// Same as for `std::slice::from_raw_parts`. Summarizing in terms of this API:
///
/// - `msg` must be valid for reads for `size`, and it must be properly aligned.
/// - `msg` must point to `size` consecutive u8 values.
/// - The `size` of the slice must be no larger than `isize::MAX`, and adding
///   that size to data must not “wrap around” the address space. See the safety
///   documentation of pointer::offset.
#[no_mangle]
pub unsafe extern "C" fn fuchsia_decode_log_message_to_json(
    msg: *const u8,
    size: usize,
) -> *mut c_char {
    let managed_ptr = std::slice::from_raw_parts(msg, size);
    let data = &message::from_structured(
        MonikerWithUrl { moniker: "test_moniker".try_into().unwrap(), url: "".into() },
        managed_ptr,
    )
    .unwrap();
    let item = serde_json::to_string(&data).unwrap();
    CString::new(format!("[{}]", item)).unwrap().into_raw()
}

/// # Safety
///
/// This should only be called with a pointer obtained through
/// `fuchsia_decode_log_message_to_json`.
#[no_mangle]
pub unsafe extern "C" fn fuchsia_free_decoded_log_message(msg: *mut c_char) {
    let str_to_free = CString::from_raw(msg);
    let _freer = str_to_free;
}
