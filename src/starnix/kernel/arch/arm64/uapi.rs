// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]

use zerocopy::{AsBytes, FromBytes, FromZeroes};

/// The type used by the kernel for the time in seconds in the stat struct.
pub type stat_time_t = i64;

#[derive(Default, Debug, Copy, Clone, AsBytes, FromZeroes, FromBytes)]
#[repr(packed)]
#[non_exhaustive]
pub struct epoll_event {
    pub events: u32,

    _padding: u32,

    pub data: u64,
}

impl epoll_event {
    pub fn new(events: u32, data: u64) -> Self {
        Self { events, data, _padding: 0 }
    }
}
