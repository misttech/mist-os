// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, sys::zx_handle_t, AsHandleRef};
use once_cell::sync::Lazy;
use std::cell::RefCell;
use std::ffi::c_void;

mod allocations_table;
mod profiler;

use crate::profiler::{PerThreadData, Profiler};

static PROFILER: Lazy<Profiler> = Lazy::new(Default::default);

thread_local! {
    static THREAD_DATA: RefCell<PerThreadData> = RefCell::new(Default::default());
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

#[no_mangle]
pub extern "C" fn __scudo_allocate_hook(ptr: *mut c_void, size: usize) {
    THREAD_DATA.with(|thread_data| {
        PROFILER.record_allocation(&mut thread_data.borrow_mut(), ptr as u64, size as u64)
    });
}

#[no_mangle]
pub extern "C" fn __scudo_deallocate_hook(ptr: *mut c_void) {
    if ptr != std::ptr::null_mut() {
        THREAD_DATA.with(|thread_data| {
            PROFILER.forget_allocation(&mut thread_data.borrow_mut(), ptr as u64);
        });
    }
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
    if global != std::ptr::null_mut() {
        *global = PROFILER.get_global_stats();
    }
    if local != std::ptr::null_mut() {
        THREAD_DATA.with(|thread_data| {
            *local = thread_data.borrow().get_local_stats();
        });
    }
}
