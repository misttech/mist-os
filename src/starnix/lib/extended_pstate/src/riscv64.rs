// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use static_assertions as _;

#[derive(Default)]
pub struct State {}

impl State {
    #[inline(always)]
    pub(crate) fn save(&mut self) {
        // TODO(fxbug.dev/128554): implement riscv support.
        panic!("riscv is not fully implemented");
    }

    #[inline(always)]
    // Safety: See comment in lib.rs.
    pub(crate) unsafe fn restore(&self) {
        // TODO(fxbug.dev/128554): implement riscv support.
        panic!("riscv is not fully implemented");
    }

    pub fn reset(&mut self) {
        *self = Default::default();
    }
}
