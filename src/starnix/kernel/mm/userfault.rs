// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{MemoryManager, PAGE_SIZE};
use bitflags::bitflags;
use range_map::RangeMap;
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
use std::ops::Range;
use std::sync::{Arc, Weak};

#[derive(Debug)]
pub struct UserFault {
    mm: Weak<MemoryManager>,
    state: OrderedMutex<UserFaultState, UserFaultInner>,
}

#[derive(Debug, Clone)]
struct UserFaultState {
    /// If initialized, contains features that this userfault was initialized with
    features: Option<UserFaultFeatures>,

    /// Pages that are currently registered with this userfault object, and whether they are
    /// already populated.
    userfault_pages: RangeMap<UserAddress, bool>,
}

impl UserFault {
    pub fn new(mm: Weak<MemoryManager>) -> Self {
        Self { mm, state: OrderedMutex::new(UserFaultState::new()) }
    }

    pub fn userfault_pages_insert<L>(
        &self,
        locked: &mut Locked<L>,
        range: Range<UserAddress>,
        value: bool,
    ) where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).userfault_pages.insert(range, value);
    }

    pub fn userfault_pages_remove<L>(&self, locked: &mut Locked<L>, range: Range<UserAddress>)
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).userfault_pages.remove(range);
    }

    pub fn get_first_populated_page_after<L>(
        &self,
        locked: &mut Locked<L>,
        addr: UserAddress,
    ) -> Option<UserAddress>
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).userfault_pages.get(addr).map(|(affected_range, is_populated)| {
            if *is_populated {
                addr
            } else {
                affected_range.end
            }
        })
    }

    pub fn is_initialized<L>(self: &Arc<Self>, locked: &mut Locked<L>) -> bool
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).features.is_some()
    }

    pub fn has_features<L>(
        self: &Arc<Self>,
        locked: &mut Locked<L>,
        features: UserFaultFeatures,
    ) -> bool
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).features.map(|f| f.contains(features)).unwrap_or(false)
    }

    pub fn initialize<L>(self: &Arc<Self>, locked: &mut Locked<L>, features: UserFaultFeatures)
    where
        L: LockBefore<UserFaultInner>,
    {
        self.state.lock(locked).features = Some(features);
    }

    pub fn op_register<L>(
        self: &Arc<Self>,
        locked: &mut Locked<L>,
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

        mm.register_with_uffd(locked, start, len as usize, self, mode)?;
        Ok(SupportedUserFaultIoctls::COPY | SupportedUserFaultIoctls::ZERO_PAGE)
    }

    pub fn op_unregister<L>(
        self: &Arc<Self>,
        locked: &mut Locked<L>,
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
        mm.unregister_range_from_uffd(locked, start, len as usize)
    }

    pub fn op_copy<L>(
        self: &Arc<Self>,
        locked: &mut Locked<L>,
        mm_source: &MemoryManager,
        source: UserAddress,
        dest: UserAddress,
        len: u64,
        _mode: FaultCopyMode,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if !self.is_initialized(locked) {
            return error!(EINVAL);
        }
        check_op_range(source, len)?;
        check_op_range(dest, len)?;
        let mm = self.mm.upgrade().ok_or_else(|| errno!(EINVAL))?;

        // If the copy happens inside the same process, do it inside this process' memory manager
        // so that the lock is held throughout the operation.
        if Arc::as_ptr(&mm) == mm_source as *const MemoryManager {
            mm.copy_from_uffd(locked, source, dest, len as usize, self)
        } else {
            let mut buf = vec![std::mem::MaybeUninit::uninit(); len as usize];
            let buf = mm_source.syscall_read_memory(source, &mut buf)?;
            mm.fill_from_uffd(locked, dest, buf, len as usize, self)
        }
    }

    pub fn op_zero<L>(
        self: &Arc<Self>,
        locked: &mut Locked<L>,
        start: UserAddress,
        len: u64,
        _mode: FaultZeroMode,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if !self.is_initialized(locked) {
            return error!(EINVAL);
        }
        check_op_range(start, len)?;
        let mm = self.mm.upgrade().ok_or_else(|| errno!(EINVAL))?;
        mm.zero_from_uffd(locked, start, len as usize, self)
    }

    pub fn cleanup<L>(self: &Arc<Self>, locked: &mut Locked<L>)
    where
        L: LockBefore<UserFaultInner>,
    {
        if let Some(mm) = self.mm.upgrade() {
            mm.unregister_all_from_uffd(locked, self);
        }
    }
}

impl UserFaultState {
    pub fn new() -> Self {
        Self { features: None, userfault_pages: RangeMap::default() }
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
