// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zx::sys::zx_handle_t;

/// Creates a trace provider service that enables traces created by a process
/// to be collected by the system trace manager.
///
/// Typically applications would call this method once, early in their main
/// function to enable them to be eligible to produce traces.
///
/// It is safe but unnecessary to call this function more than once.
pub fn trace_provider_create_with_fdio() {
    unsafe {
        sys::trace_provider_create_with_fdio_rust();
    }
}

pub fn trace_provider_create_with_service(to_service_h: zx_handle_t) {
    unsafe {
        sys::trace_provider_create_with_service_rust(to_service_h);
    }
}

/// Wait for trace provider initialization to acknowledge already-running traces before returning.
///
/// If the current thread is expected to initialize the provider then this should only be called
/// after doing so to avoid a deadlock.
pub fn trace_provider_wait_for_init() {
    unsafe {
        sys::trace_provider_wait_for_init();
    }
}

mod sys {
    // From librust-trace-provider.so
    extern "C" {
        // See the C++ documentation for these functions in trace_provider.cc
        pub(super) fn trace_provider_create_with_fdio_rust();
        pub(super) fn trace_provider_create_with_service_rust(to_service_h: zx::sys::zx_handle_t);
        pub(super) fn trace_provider_wait_for_init();
    }
}
