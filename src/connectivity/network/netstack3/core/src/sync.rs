// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Useful synchronization primitives.

// TODO(https://fxbug.dev/110884): Support single-threaded variants of types
// exported from this module.

#[cfg(all(feature = "instrumented", not(loom)))]
use netstack3_sync_instrumented as netstack3_sync;

// Don't perform recursive lock checks when benchmarking so that the benchmark
// results are not affected by the extra bookkeeping.
#[cfg(all(not(feature = "instrumented"), not(loom)))]
use netstack3_sync_not_instrumented as netstack3_sync;

#[cfg(loom)]
use netstack3_sync_loom as netstack3_sync;

pub use netstack3_sync::{
    rc::{NamedClone, Primary as PrimaryRc, Strong as StrongRc, Weak as WeakRc},
    LockGuard, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
