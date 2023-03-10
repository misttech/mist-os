// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::c_void;

use crate::{PROFILER, THREAD_DATA};

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
