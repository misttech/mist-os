// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The primary purpose of this bedrock module is that tests inside component manager
//! can poke into the bedrock state if needed, and we can ensure that only tests
//! can do so, by marking relevant methods with `#[cfg(test)]`. We also have other
//! bedrock libraries such as `sandbox`. It's likely we'll find the best
//! module/library layout over time.

pub mod program;
