// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Describes how to apply a lock type to the implementing type.
///
/// An implementation of `LockFor<L>` for some `Self` means that `L` is a valid
/// lock level for `Self`, and defines how to access the state in `Self` that is
/// under the lock indicated by `L`.
pub trait LockFor<L> {
    /// The data produced by locking the state indicated by `L` in `Self`.
    type Data<'l>
    where
        Self: 'l;

    /// Locks `Self` for lock `L`.
    fn lock(&self) -> Self::Data<'_>;
}

/// Describes how to acquire reader and writer locks to the implementing type.
///
/// An implementation of `RwLockFor<L>` for some `Self` means that `L` is a
/// valid lock level for `T`, and defines how to access the state in `Self` that
/// is under the lock indicated by `L` in either read mode or write mode.
pub trait RwLockFor<L> {
    /// Data that is made accessible by acquiring a read lock.
    type ReadData<'l>
    where
        Self: 'l;

    /// Data that is made accessible by acquiring a write lock.
    type WriteData<'l>
    where
        Self: 'l;

    /// Acquires a read lock on the data in `Self` indicated by `L`.
    fn read_lock(&self) -> Self::ReadData<'_>;

    /// Acquires a write lock on the data in `Self` indicated by `L`.
    fn write_lock(&self) -> Self::WriteData<'_>;
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
    type Data<'l>
    where
        Self: 'l;

    /// How to access the state.
    fn access(&self) -> Self::Data<'_>;
}

#[cfg(test)]
mod example {
    //! Example implementations of the traits in this crate.

    use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

    use super::*;

    enum LockLevel {}

    impl<T> LockFor<LockLevel> for Mutex<T> {
        type Data<'l> = MutexGuard<'l, T> where Self: 'l;

        fn lock(&self) -> Self::Data<'_> {
            self.lock().unwrap()
        }
    }

    impl<T> RwLockFor<LockLevel> for RwLock<T> {
        type ReadData<'l> = RwLockReadGuard<'l, T> where Self: 'l;
        type WriteData<'l> = RwLockWriteGuard<'l, T> where Self: 'l;

        fn read_lock(&self) -> Self::ReadData<'_> {
            self.read().unwrap()
        }
        fn write_lock(&self) -> Self::WriteData<'_> {
            self.write().unwrap()
        }
    }

    struct CharWrapper(char);

    impl UnlockedAccess<char> for CharWrapper {
        type Data<'l> = &'l char where Self: 'l;

        fn access(&self) -> Self::Data<'_> {
            let Self(c) = self;
            c
        }
    }
}
