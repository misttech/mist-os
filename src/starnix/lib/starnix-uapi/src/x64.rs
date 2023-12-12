// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]

use crate::{signals::SigSet, uapi::sigset_t};
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

/// The type used by the kernel for the time in seconds in the stat struct.
pub type stat_time_t = u64;

#[derive(Default, Debug, Copy, Clone, AsBytes, FromZeros, FromBytes, NoCell)]
#[repr(packed)]
#[non_exhaustive]
pub struct epoll_event {
    pub events: u32,

    pub data: u64,
}

impl epoll_event {
    pub fn new(events: u32, data: u64) -> Self {
        Self { events, data }
    }
}

impl From<sigset_t> for SigSet {
    fn from(value: sigset_t) -> Self {
        SigSet(value)
    }
}

impl From<SigSet> for sigset_t {
    fn from(val: SigSet) -> Self {
        val.0
    }
}
