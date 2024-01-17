// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use heapdump_vmo::stack_trace_compression;
use std::ffi::c_void;

use crate::{with_profiler, PerThreadData, Profiler};

const STACK_TRACE_MAXIMUM_DEPTH: usize = 64;
const STACK_TRACE_MAXIMUM_COMPRESSED_SIZE: usize =
    stack_trace_compression::max_compressed_size(STACK_TRACE_MAXIMUM_DEPTH);

extern "C" {
    fn __sanitizer_fast_backtrace(buffer: *mut u64, buffer_size: usize) -> usize;
}

// Like `with_profiler`, but pass the current timestamp and the compressed call stack too.
//
// Note: This is function is `inline(always)` so that it doesn't appear in the stack trace.
#[inline(always)]
fn with_profiler_and_call_site(f: impl FnOnce(&Profiler, &mut PerThreadData, zx::Time, &[u8])) {
    // Collect the timestamp as early as possible.
    let timestamp = zx::Time::get_monotonic();

    // Collect stack trace outside of the recursion guard to avoid including it in the stack trace.
    let mut stack_buf = [0; STACK_TRACE_MAXIMUM_DEPTH];
    let stack_len =
        unsafe { __sanitizer_fast_backtrace(stack_buf.as_mut_ptr(), STACK_TRACE_MAXIMUM_DEPTH) };
    let stack = &stack_buf[..stack_len];

    with_profiler(|profiler, thread_data| {
        // Compress the stack trace.
        let mut compressed_stack_buf = [0; STACK_TRACE_MAXIMUM_COMPRESSED_SIZE];
        let compressed_stack_len =
            stack_trace_compression::compress_into(stack, &mut compressed_stack_buf);
        let compressed_stack = &compressed_stack_buf[..compressed_stack_len];

        f(profiler, thread_data, timestamp, compressed_stack)
    })
}

// Called by Scudo after new memory has been allocated by malloc/calloc/...
#[no_mangle]
pub extern "C" fn __scudo_allocate_hook(ptr: *mut c_void, size: usize) {
    with_profiler_and_call_site(|profiler, thread_data, timestamp, compressed_stack_trace| {
        profiler.record_allocation(
            thread_data,
            ptr as u64,
            size as u64,
            compressed_stack_trace,
            timestamp.into_nanos(),
        );
    });
}

// Called by Scudo before memory is deallocated by free.
#[no_mangle]
pub extern "C" fn __scudo_deallocate_hook(ptr: *mut c_void) {
    with_profiler(|profiler, thread_data| {
        if ptr != std::ptr::null_mut() {
            profiler.forget_allocation(thread_data, ptr as u64);
        }
    });
}

// Called by Scudo at the beginning of realloc.
#[no_mangle]
pub extern "C" fn __scudo_realloc_deallocate_hook(_old_ptr: *mut c_void) {
    // We don't do anything at this stage. All our work happens in __scudo_realloc_allocate_hook.
}

// Called by Scudo at the end of realloc.
#[no_mangle]
pub extern "C" fn __scudo_realloc_allocate_hook(
    old_ptr: *mut c_void,
    new_ptr: *mut c_void,
    size: usize,
) {
    with_profiler_and_call_site(|profiler, thread_data, timestamp, compressed_stack_trace| {
        // Has the memory block been reallocated in-place?
        if old_ptr == new_ptr {
            profiler.update_allocation(
                thread_data,
                old_ptr as u64,
                size as u64,
                compressed_stack_trace,
                timestamp.into_nanos(),
            );
        } else {
            profiler.record_allocation(
                thread_data,
                new_ptr as u64,
                size as u64,
                compressed_stack_trace,
                timestamp.into_nanos(),
            );
            profiler.forget_allocation(thread_data, old_ptr as u64);
        }
    });
}
