// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Use these crates so that we don't need to make the dependency conditional.
use lock_api as _;
use parking_lot as _;

#[cfg(not(any(test, debug_assertions)))]
pub type Mutex<T> = fuchsia_sync::Mutex<T>;
#[cfg(not(any(test, debug_assertions)))]
pub type MutexGuard<'a, T> = fuchsia_sync::MutexGuard<'a, T>;
#[allow(unused)]
#[cfg(not(any(test, debug_assertions)))]
pub type MappedMutexGuard<'a, T> = fuchsia_sync::MappedMutexGuard<'a, T>;

// TODO(fxbug.dev/88603): Switch RWLocks to fuchsia_sync as well once fuchsia_sync offers RWLocks.
#[cfg(not(any(test, debug_assertions)))]
pub type RwLock<T> = parking_lot::RwLock<T>;
#[cfg(not(any(test, debug_assertions)))]
pub type RwLockReadGuard<'a, T> = parking_lot::RwLockReadGuard<'a, T>;
#[cfg(not(any(test, debug_assertions)))]
pub type RwLockWriteGuard<'a, T> = parking_lot::RwLockWriteGuard<'a, T>;

#[cfg(any(test, debug_assertions))]
type RawTracingMutex = tracing_mutex::lockapi::TracingWrapper<fuchsia_sync::RawSyncMutex>;
#[cfg(any(test, debug_assertions))]
pub type Mutex<T> = lock_api::Mutex<RawTracingMutex, T>;
#[cfg(any(test, debug_assertions))]
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawTracingMutex, T>;
#[allow(unused)]
#[cfg(any(test, debug_assertions))]
pub type MappedMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawTracingMutex, T>;

// TODO(fxbug.dev/88603): Switch RWLocks to fuchsia_sync as well once fuchsia_sync offers RWLocks.
#[cfg(any(test, debug_assertions))]
pub type RwLock<T> = tracing_mutex::parkinglot::TracingRwLock<T>;
#[cfg(any(test, debug_assertions))]
pub type RwLockReadGuard<'a, T> = tracing_mutex::parkinglot::TracingRwLockReadGuard<'a, T>;
#[cfg(any(test, debug_assertions))]
pub type RwLockWriteGuard<'a, T> = tracing_mutex::parkinglot::TracingRwLockWriteGuard<'a, T>;

/// Lock `m1` and `m2` in a consistent order (using the memory address of m1 and m2 and returns the
/// associated guard. This ensure that `ordered_lock(m1, m2)` and `ordered_lock(m2, m1)` will not
/// deadlock.
pub fn ordered_lock<'a, T>(
    m1: &'a Mutex<T>,
    m2: &'a Mutex<T>,
) -> (MutexGuard<'a, T>, MutexGuard<'a, T>) {
    let ptr1: *const Mutex<T> = m1;
    let ptr2: *const Mutex<T> = m2;
    if ptr1 < ptr2 {
        let g1 = m1.lock();
        let g2 = m2.lock();
        (g1, g2)
    } else {
        let g2 = m2.lock();
        let g1 = m1.lock();
        (g1, g2)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    fn test_lock_ordering() {
        let l1 = Mutex::new(1);
        let l2 = Mutex::new(2);

        {
            let (g1, g2) = ordered_lock(&l1, &l2);
            assert_eq!(*g1, 1);
            assert_eq!(*g2, 2);
        }
        {
            let (g2, g1) = ordered_lock(&l2, &l1);
            assert_eq!(*g1, 1);
            assert_eq!(*g2, 2);
        }
    }
}
