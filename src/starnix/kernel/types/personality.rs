// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bitflags::bitflags;

use crate::types::uapi;

bitflags! {
    #[derive(Default)]
    pub struct PersonalityFlags: u32 {
        const UNAME26 = uapi::UNAME26;
        const ADDR_NO_RANDOMIZE = uapi::ADDR_NO_RANDOMIZE;
        const FDPIC_FUNCPTRS = uapi::FDPIC_FUNCPTRS;
        const MMAP_PAGE_ZERO = uapi::MMAP_PAGE_ZERO;
        const ADDR_COMPAT_LAYOUT = uapi::ADDR_COMPAT_LAYOUT;
        const READ_IMPLIES_EXEC = uapi::READ_IMPLIES_EXEC;
        const ADDR_LIMIT_32BIT = uapi::ADDR_LIMIT_32BIT;
        const SHORT_INODE = uapi::SHORT_INODE;
        const WHOLE_SECONDS = uapi::WHOLE_SECONDS;
        const STICKY_TIMEOUTS = uapi::STICKY_TIMEOUTS;
        const ADDR_LIMIT_3GB = uapi::ADDR_LIMIT_3GB;
    }
}
