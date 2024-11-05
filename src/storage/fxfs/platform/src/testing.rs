// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/377364505) Remove and fix lints once compiler roll
// https://fxbug.dev/370540341 lands.
#[allow(dead_code)]
mod fuchsia;
use self::fuchsia::*;

pub use self::fuchsia::testing::*;
