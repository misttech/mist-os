// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

use bumpalo::Bump;
use diagnostics_message::ffi::{CPPArray, LogMessage};
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
///   that size to data must not "wrap around" the address space. See the safety
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

/// Memory-managed state to be free'd on the Rust side
/// when the log messages are destroyed.
pub struct ManagedState<'a> {
    allocator: Bump,
    message_array: Vec<*mut LogMessage<'a>>,
}

impl Drop for LogMessages<'_> {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: All pointers in message_array are assumed to be valid.
            // Other unsafe code in this file and in C++ ensures this invariant.

            // Free all managed state in the log messages.
            // The log messages themselves don't need to be explicitly free'd
            // as they are owned by the Bump allocator.
            let state = Box::from_raw(self.state);
            for msg in &state.message_array {
                std::ptr::drop_in_place(*msg);
            }
        }
    }
}

/// LogMessages struct containing log messages
/// It is created by calling fuchsia_decode_log_messages_to_struct,
/// and freed by calling fuchsia_free_log_messages.
/// Log messages contain embedded pointers to the bytes from
/// which they were created, so the memory referred to
/// by the LogMessages must not be modified or free'd until
/// the LogMessages are free'd.
#[repr(C)]
pub struct LogMessages<'a> {
    messages: CPPArray<*mut LogMessage<'a>>,
    state: *mut ManagedState<'a>,
}

/// # Safety
///
/// Same as for `std::slice::from_raw_parts`. Summarizing in terms of this API:
///
/// - `msg` must be valid for reads for `size`, and it must be properly aligned.
/// - `msg` must point to `size` consecutive u8 values.
/// - 'msg' must outlive the returned LogMessages struct, and must not be free'd
///   until fuchsia_free_log_messages has been called.
/// - The `size` of the slice must be no larger than `isize::MAX`, and adding
///   that size to data must not "wrap around" the address space. See the safety
///   documentation of pointer::offset.
/// If identity is provided, it must contain a valid moniker and URL.
///
/// The returned LogMessages may be free'd with fuchsia_free_log_messages(log_messages).
/// Free'ing the LogMessages struct does the following, in this order:
/// * Frees memory associated with each individual log message
/// * Frees the bump allocator itself (and everything allocated from it), as well as
/// the message array itself.
#[no_mangle]
pub unsafe extern "C" fn fuchsia_decode_log_messages_to_struct(
    msg: *const u8,
    size: usize,
    expect_extended_attribution: bool,
) -> LogMessages<'static> {
    let mut state = Box::new(ManagedState { allocator: Bump::new(), message_array: vec![] });
    let buf = std::slice::from_raw_parts(msg, size);
    let mut current_slice = buf.as_ref();
    loop {
        let (data, remaining) = message::ffi::ffi_from_extended_record(
            current_slice,
            // SAFETY: The returned LogMessage must NOT outlive the bump allocator.
            // This is ensured by the allocator living in the heap-allocated ManagedState
            // struct which frees the LogMessages first when dropped, before allowing the bump
            // allocator itself to be freed.
            &*(&state.allocator as *const Bump),
            None,
            expect_extended_attribution,
        )
        .unwrap();
        state.message_array.push(data as *mut LogMessage<'static>);
        if remaining.is_empty() {
            break;
        }
        current_slice = remaining;
    }

    LogMessages { messages: (&state.message_array).into(), state: Box::into_raw(state) }
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

/// # Safety
///
/// This should only be called with a pointer obtained through
/// `fuchsia_decode_log_messages_to_struct`.
#[no_mangle]
pub unsafe extern "C" fn fuchsia_free_log_messages(input: LogMessages<'_>) {
    drop(input);
}
