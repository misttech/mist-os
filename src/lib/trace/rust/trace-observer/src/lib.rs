// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Start a thread that calls the provided callback when trace state changes.
pub fn start_trace_observer(callback: extern "C" fn()) {
    unsafe { sys::start_trace_observer_rust(callback) }
}

mod sys {
    #[link(name = "rust-trace-observer")]
    extern "C" {
        pub fn start_trace_observer_rust(callback: unsafe extern "C" fn());
    }
}
