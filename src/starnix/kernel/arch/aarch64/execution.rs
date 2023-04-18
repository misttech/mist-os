// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
