// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains forwarding stubs from the C entry points to Rust implementations.
//
// Exposed functions TEE_FooBar forwards to tee_impl::foo_bar()

#![allow(non_snake_case)]

use crate::tee_impl;

#[no_mangle]
extern "C" fn TEE_Panic(code: u32) {
    tee_impl::panic(code)
}
