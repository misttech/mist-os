// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_camel_case_types)]

use crate::{signals::SigSet, uapi::sigset_t};
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

/// The type used by the kernel for the time in seconds in the stat struct.
pub type stat_time_t = i64;

#[derive(Default, Debug, Copy, Clone, AsBytes, FromZeros, FromBytes, NoCell)]
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

impl From<sigset_t> for SigSet {
    fn from(value: sigset_t) -> Self {
        SigSet(value.sig[0])
    }
}

impl From<SigSet> for sigset_t {
    fn from(val: SigSet) -> Self {
        sigset_t { sig: [val.0] }
    }
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeros, NoCell)]
pub struct user_regs_struct {
    pub regs: [u64; 31usize],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

#[repr(C)]
#[repr(align(16))]
#[derive(Debug, Default, Copy, Clone, AsBytes, FromBytes, FromZeros, NoCell)]
pub struct user_fpsimd_struct {
    pub vregs: [crate::__uint128_t; 32usize],
    pub fpsr: u32,
    pub fpcr: u32,
    pub _padding_0: [u8; 8usize],
}
