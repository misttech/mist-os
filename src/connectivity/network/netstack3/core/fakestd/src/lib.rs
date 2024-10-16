// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/62502): remove this module.

pub use ::core::{convert, fmt, option};
pub use ::std::error;
pub mod thread {
    pub use ::std::thread::panicking;
}

/// Re-export `println` with a clearer name so we can divert logging in tests to
/// stdout.
pub use ::std::println as println_for_test;
