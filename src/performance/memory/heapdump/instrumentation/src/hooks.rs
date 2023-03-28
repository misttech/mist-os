// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use heapdump_vmo::stack_trace_compression;
use std::ffi::c_void;

use crate::recursion_guard::with_recursion_guard;
use crate::{PROFILER, THREAD_DATA};

const STACK_TRACE_MAXIMUM_DEPTH: usize = 64;
const STACK_TRACE_MAXIMUM_COMPRESSED_SIZE: usize =
    stack_trace_compression::max_compressed_size(STACK_TRACE_MAXIMUM_DEPTH);

extern "C" {
    fn __sanitizer_fast_backtrace(buffer: *mut u64, buffer_size: usize) -> usize;
}

#[no_mangle]
pub extern "C" fn __scudo_allocate_hook(ptr: *mut c_void, size: usize) {
    // Collect stack trace outside of the recursion guard to avoid including it in the stack trace.
    let mut stack_buf = [0; STACK_TRACE_MAXIMUM_DEPTH];
    let stack_len =
        unsafe { __sanitizer_fast_backtrace(stack_buf.as_mut_ptr(), STACK_TRACE_MAXIMUM_DEPTH) };
    let stack = &stack_buf[..stack_len];

    let profiler = &*PROFILER;
    THREAD_DATA.with(|thread_data| {
        with_recursion_guard(|| {
            // Compress the stack trace.
            let mut compressed_stack_buf = [0; STACK_TRACE_MAXIMUM_COMPRESSED_SIZE];
            let compressed_stack_len =
                stack_trace_compression::compress_into(stack, &mut compressed_stack_buf);
            let compressed_stack = &compressed_stack_buf[..compressed_stack_len];

            profiler.record_allocation(
                &mut thread_data.borrow_mut(),
                ptr as u64,
                size as u64,
                compressed_stack,
            );
        });
    });
}

#[no_mangle]
pub extern "C" fn __scudo_deallocate_hook(ptr: *mut c_void) {
    let profiler = &*PROFILER;
    THREAD_DATA.with(|thread_data| {
        with_recursion_guard(|| {
            if ptr != std::ptr::null_mut() {
                profiler.forget_allocation(&mut thread_data.borrow_mut(), ptr as u64);
            }
        });
    });
}
