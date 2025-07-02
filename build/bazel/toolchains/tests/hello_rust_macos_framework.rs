// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// A trivial program to verify that linking against a system framework on Macos works.
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFLocaleGetSystem() -> *const ::std::os::raw::c_void;
}

fn main() {
    unsafe {
        let _ = CFLocaleGetSystem();
        println!("Success!");
    }
}
