// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod binding_stubs;
pub mod mem;

pub fn panic(code: u32) {
    std::process::exit(code as i32)
}
