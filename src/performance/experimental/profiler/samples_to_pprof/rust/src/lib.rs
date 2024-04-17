// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::CString;

// Given a symbolized file output by ffx profiler start, convert it to pprof protobuf output format
pub fn convert(from: String, to: String) -> bool {
    unsafe {
        return sys::convert(
            // Strings do not have interior nul bytes so this is safe.
            CString::from_vec_unchecked(from.into()).as_c_str().as_ptr(),
            CString::from_vec_unchecked(to.into()).as_c_str().as_ptr(),
        ) == 0;
    }
}

mod sys {
    use std::ffi::c_char;
    use std::ffi::c_int;
    #[link(name = "rust_samples_to_pprof")]
    extern "C" {
        // See the C++ documentation for this function in samples_to_pprof_rust.cc
        pub(super) fn convert(from: *const c_char, to: *const c_char) -> c_int;
    }
}
