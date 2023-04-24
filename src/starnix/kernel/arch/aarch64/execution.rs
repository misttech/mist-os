// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;

/// Generates CFI directives so the unwinder will be redirected to unwind the stack provided in `state`.
#[macro_export]
macro_rules! generate_cfi_directives {
    ($state:expr) => {};
}

/// Generates directives to restore the CFI state.
#[macro_export]
macro_rules! restore_cfi_directives {
    () => {};
}

pub(crate) use generate_cfi_directives;
pub(crate) use restore_cfi_directives;

pub fn generate_interrupt_instructions(_current_task: &CurrentTask) -> Vec<u8> {
    const INTERRUPT_AND_JUMP: [u8; 8] = [
        0x00, 0x00, 0x20, 0xd4, // 0xd4200000 = brk 0 (the argument is ignored by us).
        // TODO(fxbug.dev/121659): Write this code. This issues an undefined instruction so it's
        // obvious something is not implemented.
        //
        // I believe ARM lacks a single indirect-from-memory jump instruction like x86 so this
        // will need some research on what registers we can clobber.
        0x00, 0x00, 0x00, 0x00, // udf = "Undefined instruction"
    ];
    INTERRUPT_AND_JUMP.to_vec()
}
