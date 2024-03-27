// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains forwarding stubs from the C entry points to Rust implementations.
//
// Exposed functions TEE_FooBar forwards to tee_impl::foo_bar()

#![allow(non_snake_case)]

use crate::tee_impl;

// This function returns a list of the C entry point that we want to expose from
// this program. They need to be referenced from main to ensure that the linker
// thinks that they are referenced and need to be included in the final binary.
pub fn exposed_c_entry_points() -> &'static [*const extern "C" fn()] {
    &[TEE_Panic as *const extern "C" fn()]
}

#[no_mangle]
pub extern "C" fn TEE_Panic(code: u32) {
    tee_impl::panic(code)
}
