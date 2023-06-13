// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

/// Watch descriptor returned by inotify_add_watch(2).
///
/// See inotify(7) for details.
#[derive(
    Hash, PartialEq, Eq, PartialOrd, Ord, Debug, Copy, Clone, AsBytes, FromZeroes, FromBytes,
)]
#[repr(transparent)]
pub struct WdNumber(i32);

impl WdNumber {
    pub fn from_raw(n: i32) -> WdNumber {
        WdNumber(n)
    }

    pub fn raw(&self) -> i32 {
        self.0
    }
}

impl fmt::Display for WdNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wd({})", self.0)
    }
}
