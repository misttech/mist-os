// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;

mod fifo;

pub use fifo::*;

bitflags! {
    /// Options that may be used for writes.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct WriteOptions: u32 {
        const FORCE_ACCESS = 1;
        const PRE_BARRIER = 2;
    }
}
