// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ops::{Deref, DerefMut};

/// Describes how to apply a lock type to the implementing type.
///
/// An implementation of `LockFor<L>` for some `Self` means that `Self` can
/// be used to unlock some state that is under the lock indicated by `L`.
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
/// An implementation of `RwLockFor<L>` for some `Self` means that `Self` can
/// be used to unlock some state that is under the lock indicated by `L`.
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

#[cfg(test)]
mod example {
    //! Example implementations of the traits in this crate.

    use std::sync::{Mutex, MutexGuard};

    use super::*;

    enum LockLevel {}

    impl<T> LockFor<LockLevel> for Mutex<T> {
        type Data = T;
        type Guard<'l>
            = MutexGuard<'l, T>
        where
            Self: 'l;

        fn lock(&self) -> Self::Guard<'_> {
            self.lock().unwrap()
        }
    }
}
