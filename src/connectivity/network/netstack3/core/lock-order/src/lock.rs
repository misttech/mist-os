// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ops::{Deref, DerefMut};

/// Describes how to apply a lock type to the implementing type.
///
/// An implementation of `LockFor<L>` for some `Self` means that `L` is a valid
/// lock level for `Self`, and defines how to access the state in `Self` that is
/// under the lock indicated by `L`.
pub trait LockFor<L> {
    /// The data produced by locking the state indicated by `L` in `Self`.
    type Data;

    /// A guard providing read and write access to the data.
    type Guard<'l>: DerefMut<Target = Self::Data>
    where
        Self: 'l;

    /// Locks `Self` for lock `L`.
    fn lock(&self) -> Self::Guard<'_>;
}

/// Describes how to acquire reader and writer locks to the implementing type.
///
/// An implementation of `RwLockFor<L>` for some `Self` means that `L` is a
/// valid lock level for `T`, and defines how to access the state in `Self` that
/// is under the lock indicated by `L` in either read mode or write mode.
pub trait RwLockFor<L> {
    /// The data produced by locking the state indicated by `L` in `Self`.
    type Data;

    /// A guard providing read access to the data.
    type ReadGuard<'l>: Deref<Target = Self::Data>
    where
        Self: 'l;

    /// A guard providing write access to the data.
    type WriteGuard<'l>: DerefMut<Target = Self::Data>
    where
        Self: 'l;

    /// Acquires a read lock on the data in `Self` indicated by `L`.
    fn read_lock(&self) -> Self::ReadGuard<'_>;

    /// Acquires a write lock on the data in `Self` indicated by `L`.
    fn write_lock(&self) -> Self::WriteGuard<'_>;
}

/// Describes how to access state in `Self` that doesn't require locking.
///
/// `UnlockedAccess` allows access to some state in `Self` without acquiring
/// a lock. Unlike `Lock` and friends, the type parameter `A` in
/// `UnlockedAccess<A>` is used to provide a label for the state; it is
/// unrelated to the lock levels for `Self`.
///
/// In order for this crate to provide guarantees about lock ordering safety,
/// `UnlockedAccess` must only be implemented for accessing state that is
/// guaranteed to be accessible lock-free.
pub trait UnlockedAccess<A> {
    /// The type of state being accessed.
    type Data;

    /// A guard providing read access to the data.
    type Guard<'l>: Deref<Target = Self::Data>
    where
        Self: 'l;

    /// How to access the state.
    fn access(&self) -> Self::Guard<'_>;
}

#[cfg(test)]
mod example {
    //! Example implementations of the traits in this crate.

    use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

    use super::*;

    enum LockLevel {}

    impl<T> LockFor<LockLevel> for Mutex<T> {
        type Data = T;
        type Guard<'l> = MutexGuard<'l, T> where Self: 'l;

        fn lock(&self) -> Self::Guard<'_> {
            self.lock().unwrap()
        }
    }

    impl<T> RwLockFor<LockLevel> for RwLock<T> {
        type Data = T;
        type ReadGuard<'l> = RwLockReadGuard<'l, T> where Self: 'l;
        type WriteGuard<'l> = RwLockWriteGuard<'l, T> where Self: 'l;

        fn read_lock(&self) -> Self::ReadGuard<'_> {
            self.read().unwrap()
        }
        fn write_lock(&self) -> Self::WriteGuard<'_> {
            self.write().unwrap()
        }
    }

    struct CharWrapper(char);

    impl UnlockedAccess<char> for CharWrapper {
        type Data = char;
        type Guard<'l> = &'l char where Self: 'l;

        fn access(&self) -> Self::Guard<'_> {
            let Self(c) = self;
            c
        }
    }
}
