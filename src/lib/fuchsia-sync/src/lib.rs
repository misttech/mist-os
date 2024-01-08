// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Fuchsia-native synchronization primitives.

#[cfg(target_os = "fuchsia")]
mod condvar;
#[cfg(target_os = "fuchsia")]
mod mutex;
#[cfg(target_os = "fuchsia")]
mod rwlock;

#[cfg(target_os = "fuchsia")]
pub use condvar::*;
#[cfg(target_os = "fuchsia")]
pub use mutex::*;
#[cfg(target_os = "fuchsia")]
pub use rwlock::*;

#[cfg(not(target_os = "fuchsia"))]
pub use parking_lot::{
    Condvar, MappedMutexGuard, MappedRwLockReadGuard, MappedRwLockWriteGuard, Mutex, MutexGuard,
    RawMutex as RawSyncMutex, RawRwLock as RawSyncRwLock, RwLock, RwLockReadGuard,
    RwLockWriteGuard,
};
