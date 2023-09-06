// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod sys {
    #[link(name = "rust-trace-observer")]
    extern "C" {
        pub fn start_trace_observer_rust(callback: unsafe extern "C" fn());
    }
}
