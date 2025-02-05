// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{MemoryManager, PAGE_SIZE};
use bitflags::bitflags;
use starnix_logging::track_stub;
use starnix_sync::{LockBefore, Locked, OrderedMutex, UserFaultInner};
use starnix_uapi::errors::Errno;
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{
    errno, error, UFFDIO_CONTINUE_MODE_DONTWAKE, UFFDIO_COPY_MODE_DONTWAKE, UFFDIO_COPY_MODE_WP,
    UFFDIO_REGISTER_MODE_MINOR, UFFDIO_REGISTER_MODE_MISSING, UFFDIO_REGISTER_MODE_WP,
    UFFDIO_ZEROPAGE_MODE_DONTWAKE, UFFD_FEATURE_EVENT_FORK, UFFD_FEATURE_EVENT_REMAP,
    UFFD_FEATURE_EVENT_REMOVE, UFFD_FEATURE_EVENT_UNMAP, UFFD_FEATURE_MINOR_HUGETLBFS,
    UFFD_FEATURE_MINOR_SHMEM, UFFD_FEATURE_MISSING_HUGETLBFS, UFFD_FEATURE_MISSING_SHMEM,
    UFFD_FEATURE_SIGBUS, UFFD_FEATURE_THREAD_ID,
};
use std::sync::{Arc, Weak};

#[derive(Debug)]
pub struct UserFault {
    mm: Weak<MemoryManager>,
    state: OrderedMutex<UserFaultState, UserFaultInner>,
}

#[derive(Debug, Clone)]
struct UserFaultState {
    features: Option<UserFaultFeatures>,
}

impl UserFault {
    pub fn new(mm: Weak<MemoryManager>) -> Self {
        Self { mm, state: OrderedMutex::new(UserFaultState::new()) }
    }

    pub fn is_initialized<L>(self: &Arc<Self>, locked: &mut Locked<'_, L>) -> bool
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).features.is_some()
    }

    pub fn initialize<L>(self: &Arc<Self>, locked: &mut Locked<'_, L>, features: UserFaultFeatures)
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).features = Some(features);
    }

    pub fn op_register<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        start: UserAddress,
        len: u64,
        mode: FaultRegisterMode,
    ) -> Result<SupportedUserFaultIoctls, Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if !self.is_initialized(locked) {
            return error!(EINVAL);
        }
        check_op_range(start, len)?;
        let mm = self.mm.upgrade().ok_or_else(|| errno!(EINVAL))?;

        mm.register_with_uffd(start, len as usize, Arc::downgrade(self), mode)?;
        // TODO(https://fxbug.dev/297375964): list supported ioctls
        Ok(SupportedUserFaultIoctls::empty())
    }

    pub fn op_unregister<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        start: UserAddress,
        len: u64,
    ) -> Result<(), Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if !self.is_initialized(locked) {
            return error!(EINVAL);
        }
        check_op_range(start, len)?;
        let mm = self.mm.upgrade().ok_or_else(|| errno!(EINVAL))?;
        mm.unregister_range_from_uffd(start, len as usize)
    }

    pub fn op_copy<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        _mm_source: &MemoryManager,
        source: UserAddress,
        dest: UserAddress,
        len: u64,
        _mode: FaultCopyMode,
    ) -> Result<u64, Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if !self.is_initialized(locked) {
            return error!(EINVAL);
        }
        check_op_range(source, len)?;
        check_op_range(dest, len)?;
        track_stub!(TODO("https://fxbug.dev/297375964"), "basic uffd operations");
        error!(ENOTSUP)
    }

    pub fn op_zero<L>(
        self: &Arc<Self>,
        locked: &mut Locked<'_, L>,
        start: UserAddress,
        len: u64,
        _mode: FaultZeroMode,
    ) -> Result<u64, Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if !self.is_initialized(locked) {
            return error!(EINVAL);
        }
        check_op_range(start, len)?;
        track_stub!(TODO("https://fxbug.dev/297375964"), "basic uffd operations");
        error!(ENOTSUP)
    }

    pub fn cleanup(self: &Arc<Self>) {
        if let Some(mm) = self.mm.upgrade() {
            mm.unregister_all_from_uffd(Arc::downgrade(self));
        }
    }
}

impl UserFaultState {
    pub fn new() -> Self {
        Self { features: None }
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub struct UserFaultFeatures: u32 {
        const ALL_SUPPORTED = UFFD_FEATURE_SIGBUS;
        const EVENT_FORK = UFFD_FEATURE_EVENT_FORK;
        const EVENT_REMAP = UFFD_FEATURE_EVENT_REMAP;
        const EVENT_REMOVE = UFFD_FEATURE_EVENT_REMOVE;
        const EVENT_UNMAP = UFFD_FEATURE_EVENT_UNMAP;
        const MISSING_HUGETLBFS = UFFD_FEATURE_MISSING_HUGETLBFS;
        const MISSING_SHMEM = UFFD_FEATURE_MISSING_SHMEM;
        const SIBGUS = UFFD_FEATURE_SIGBUS;
        const THREAD_ID = UFFD_FEATURE_THREAD_ID;
        const MINOR_HUGETLBFS = UFFD_FEATURE_MINOR_HUGETLBFS;
        const MINOR_SHMEM = UFFD_FEATURE_MINOR_SHMEM;
    }

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub struct FaultRegisterMode: u32 {
        const MINOR = UFFDIO_REGISTER_MODE_MINOR;
        const MISSING = UFFDIO_REGISTER_MODE_MISSING;
        const WRITE_PROTECT = UFFDIO_REGISTER_MODE_WP;
    }

    pub struct FaultCopyMode: u32 {
        const DONT_WAKE = UFFDIO_COPY_MODE_DONTWAKE;
        const WRITE_PROTECT = UFFDIO_COPY_MODE_WP;
    }

    pub struct FaultZeroMode: u32 {
        const DONT_WAKE = UFFDIO_ZEROPAGE_MODE_DONTWAKE;
    }

    pub struct FaultContinueMode: u32 {
        const DONT_WAKE = UFFDIO_CONTINUE_MODE_DONTWAKE;
    }


    pub struct SupportedUserFaultIoctls: u64 {
        const COPY = 1 << starnix_uapi::_UFFDIO_COPY;
        const WAKE = 1 << starnix_uapi::_UFFDIO_WAKE;
        const WRITE_PROTECT = 1 << starnix_uapi::_UFFDIO_WRITEPROTECT;
        const ZERO_PAGE = 1 << starnix_uapi::_UFFDIO_ZEROPAGE;
        const CONTINUE = 1 << starnix_uapi::_UFFDIO_CONTINUE;
    }
}

fn check_op_range(addr: UserAddress, len: u64) -> Result<(), Errno> {
    if addr.is_aligned(*PAGE_SIZE) && len % *PAGE_SIZE == 0 && len > 0 {
        Ok(())
    } else {
        error!(EINVAL)
    }
}
