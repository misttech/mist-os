// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Re-export `println` with a clearer name so we can divert logging in tests to
/// stdout.
pub use ::std::println as println_for_test;
