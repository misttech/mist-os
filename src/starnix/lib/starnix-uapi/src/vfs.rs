// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::PAGE_SIZE;
use linux_uapi as uapi;
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLLET: u32 = 1 << 31;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct FdEvents: u32 {
        const POLLIN = uapi::POLLIN;
        const POLLPRI = uapi::POLLPRI;
        const POLLOUT = uapi::POLLOUT;
        const POLLERR = uapi::POLLERR;
        const POLLHUP = uapi::POLLHUP;
        const POLLNVAL = uapi::POLLNVAL;
        const POLLRDNORM = uapi::POLLRDNORM;
        const POLLRDBAND = uapi::POLLRDBAND;
        const POLLWRNORM = uapi::POLLWRNORM;
        const POLLWRBAND = uapi::POLLWRBAND;
        const POLLMSG = uapi::POLLMSG;
        const POLLREMOVE = uapi::POLLREMOVE;
        const POLLRDHUP = uapi::POLLRDHUP;
        const EPOLLET = EPOLLET;
        const EPOLLONESHOT = EPOLLONESHOT;
    }
}

impl FdEvents {
    /// Build events from the given value, truncating any bits that do not correspond to an event.
    pub fn from_u64(value: u64) -> Self {
        Self::from_bits_truncate((value & (u32::MAX as u64)) as u32)
    }
}

pub fn default_statfs(magic: u32) -> uapi::statfs {
    uapi::statfs {
        f_type: magic as i64,
        f_bsize: *PAGE_SIZE as i64,
        f_blocks: 0,
        f_bfree: 0,
        f_bavail: 0,
        f_files: 0,
        f_ffree: 0,
        f_fsid: uapi::__kernel_fsid_t::default(),
        f_namelen: uapi::NAME_MAX as i64,
        f_frsize: *PAGE_SIZE as i64,
        f_flags: 0,
        f_spare: [0, 0, 0, 0],
    }
}

#[repr(C)]
#[derive(AsBytes, FromBytes, FromZeros, NoCell)]
pub struct EpollEvent(uapi::epoll_event);

impl EpollEvent {
    pub fn new(events: FdEvents, data: u64) -> Self {
        Self(uapi::epoll_event { events: events.bits(), data, ..Default::default() })
    }

    pub fn events(&self) -> FdEvents {
        FdEvents::from_bits_retain(self.0.events)
    }

    pub fn data(&self) -> u64 {
        self.0.data
    }
}
