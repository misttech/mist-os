// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Fuchsia-native synchronization primitives.

mod mutex;
mod rwlock;
mod zx;

pub use mutex::*;
pub use rwlock::*;
