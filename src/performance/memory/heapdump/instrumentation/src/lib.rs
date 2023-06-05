// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, sys::zx_handle_t, AsHandleRef};
use lazy_static::lazy_static;
use std::cell::RefCell;
use std::ffi::c_char;

mod allocations_table;
mod profiler;
mod recursion_guard;
mod resources_table;

// Do not include the hooks in the tests' executable, to avoid instrumenting the test framework.
#[cfg(not(test))]
mod hooks;

use crate::profiler::{PerThreadData, Profiler};
use crate::recursion_guard::with_recursion_guard;

// WARNING! Do not change this to use once_cell: once_cell uses parking_lot, which may allocate in
// the contended case.
lazy_static! {
    pub static ref PROFILER: Profiler = with_recursion_guard(Default::default);
}

thread_local! {
    pub static THREAD_DATA: RefCell<PerThreadData> = with_recursion_guard(Default::default);
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct heapdump_global_stats {
    pub total_allocated_bytes: u64,
    pub total_deallocated_bytes: u64,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct heapdump_thread_local_stats {
    pub total_allocated_bytes: u64,
    pub total_deallocated_bytes: u64,
}

/// # Safety
/// The caller must pass either a channel handle or an invalid handle.
#[no_mangle]
pub unsafe extern "C" fn heapdump_bind_with_channel(registry_channel: zx_handle_t) {
    let handle = zx::Handle::from_raw(registry_channel);
    if !handle.is_invalid() {
        assert_eq!(handle.basic_info().unwrap().object_type, zx::ObjectType::CHANNEL);
    }

    PROFILER.bind(handle.into());
}

/// # Safety
/// The caller must pass suitably-aligned and writable areas of memory to store the stats into.
#[no_mangle]
pub unsafe extern "C" fn heapdump_get_stats(
    global: *mut heapdump_global_stats,
    local: *mut heapdump_thread_local_stats,
) {
    let profiler = &*PROFILER;
    THREAD_DATA.with(|thread_data| {
        with_recursion_guard(|| {
            if global != std::ptr::null_mut() {
                *global = profiler.get_global_stats();
            }
            if local != std::ptr::null_mut() {
                *local = thread_data.borrow().get_local_stats();
            }
        });
    });
}

/// # Safety
/// The caller must pass a nul-terminated string whose length is not greater than ZX_MAX_NAME_LEN.
#[no_mangle]
pub unsafe extern "C" fn heapdump_take_named_snapshot(name: *const c_char) {
    let name_cstr = std::ffi::CStr::from_ptr(name);
    let name_str = name_cstr.to_str().expect("name contains invalid characters");
    assert!(name_str.len() <= zx::sys::ZX_MAX_NAME_LEN, "name is too long");

    PROFILER.publish_named_snapshot(name_str);
}
