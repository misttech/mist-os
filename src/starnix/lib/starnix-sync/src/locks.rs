// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Use these crates so that we don't need to make the dependencies conditional.
use {fuchsia_sync as _, lock_api as _, tracing_mutex as _};

use crate::{LockAfter, LockBefore, LockFor, Locked, RwLockFor, UninterruptibleLock};
use core::marker::PhantomData;
use std::{any, fmt};

#[cfg(not(debug_assertions))]
pub type Mutex<T> = fuchsia_sync::Mutex<T>;
#[cfg(not(debug_assertions))]
pub type MutexGuard<'a, T> = fuchsia_sync::MutexGuard<'a, T>;
#[allow(unused)]
#[cfg(not(debug_assertions))]
pub type MappedMutexGuard<'a, T> = fuchsia_sync::MappedMutexGuard<'a, T>;

#[cfg(not(debug_assertions))]
pub type RwLock<T> = fuchsia_sync::RwLock<T>;
#[cfg(not(debug_assertions))]
pub type RwLockReadGuard<'a, T> = fuchsia_sync::RwLockReadGuard<'a, T>;
#[cfg(not(debug_assertions))]
pub type RwLockWriteGuard<'a, T> = fuchsia_sync::RwLockWriteGuard<'a, T>;

#[cfg(debug_assertions)]
type RawTracingMutex = tracing_mutex::lockapi::TracingWrapper<fuchsia_sync::RawSyncMutex>;
#[cfg(debug_assertions)]
pub type Mutex<T> = lock_api::Mutex<RawTracingMutex, T>;
#[cfg(debug_assertions)]
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawTracingMutex, T>;
#[allow(unused)]
#[cfg(debug_assertions)]
pub type MappedMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawTracingMutex, T>;

#[cfg(debug_assertions)]
type RawTracingRwLock = tracing_mutex::lockapi::TracingWrapper<fuchsia_sync::RawSyncRwLock>;
#[cfg(debug_assertions)]
pub type RwLock<T> = lock_api::RwLock<RawTracingRwLock, T>;
#[cfg(debug_assertions)]
pub type RwLockReadGuard<'a, T> = lock_api::RwLockReadGuard<'a, RawTracingRwLock, T>;
#[cfg(debug_assertions)]
pub type RwLockWriteGuard<'a, T> = lock_api::RwLockWriteGuard<'a, RawTracingRwLock, T>;

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

/// A wrapper for mutex that requires a `Locked` context to acquire.
/// This context must be of a level that precedes `L` in the lock ordering graph
/// where `L` is a level associated with this mutex.
pub struct OrderedMutex<T, L: LockAfter<UninterruptibleLock>> {
    mutex: Mutex<T>,
    _phantom: PhantomData<L>,
}

impl<T: Default, L: LockAfter<UninterruptibleLock>> Default for OrderedMutex<T, L> {
    fn default() -> Self {
        Self { mutex: Default::default(), _phantom: Default::default() }
    }
}

impl<T: fmt::Debug, L: LockAfter<UninterruptibleLock>> fmt::Debug for OrderedMutex<T, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OrderedMutex({:?}, {})", self.mutex, any::type_name::<L>())
    }
}

impl<T, L: LockAfter<UninterruptibleLock>> LockFor<L> for OrderedMutex<T, L> {
    type Data = T;
    type Guard<'a> = MutexGuard<'a, T> where T: 'a, L: 'a;
    fn lock(&self) -> Self::Guard<'_> {
        self.mutex.lock()
    }
}

impl<T, L: LockAfter<UninterruptibleLock>> OrderedMutex<T, L> {
    pub const fn new(t: T) -> Self {
        Self { mutex: Mutex::new(t), _phantom: PhantomData }
    }

    pub fn lock<'a, P>(&'a self, locked: &'a mut Locked<'_, P>) -> <Self as LockFor<L>>::Guard<'a>
    where
        P: LockBefore<L>,
    {
        locked.lock(self)
    }

    pub fn lock_and<'a, P>(
        &'a self,
        locked: &'a mut Locked<'_, P>,
    ) -> (<Self as LockFor<L>>::Guard<'a>, Locked<'a, L>)
    where
        P: LockBefore<L>,
    {
        locked.lock_and(self)
    }
}

/// Lock two OrderedMutex of the same level in the consistent order. Returns both
/// guards and a new locked context.
pub fn lock_both<'a, T, L: LockAfter<UninterruptibleLock>, P>(
    locked: &'a mut Locked<'_, P>,
    m1: &'a OrderedMutex<T, L>,
    m2: &'a OrderedMutex<T, L>,
) -> (MutexGuard<'a, T>, MutexGuard<'a, T>, Locked<'a, L>)
where
    P: LockBefore<L>,
{
    locked.lock_both_and(m1, m2)
}

/// A wrapper for an RwLock that requires a `Locked` context to acquire.
/// This context must be of a level that precedes `L` in the lock ordering graph
/// where `L` is a level associated with this RwLock.
pub struct OrderedRwLock<T, L: LockAfter<UninterruptibleLock>> {
    rwlock: RwLock<T>,
    _phantom: PhantomData<L>,
}

impl<T: Default, L: LockAfter<UninterruptibleLock>> Default for OrderedRwLock<T, L> {
    fn default() -> Self {
        Self { rwlock: Default::default(), _phantom: Default::default() }
    }
}

impl<T: fmt::Debug, L: LockAfter<UninterruptibleLock>> fmt::Debug for OrderedRwLock<T, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OrderedRwLock({:?}, {})", self.rwlock, any::type_name::<L>())
    }
}

impl<T, L: LockAfter<UninterruptibleLock>> RwLockFor<L> for OrderedRwLock<T, L> {
    type Data = T;
    type ReadGuard<'a> = RwLockReadGuard<'a, T> where T: 'a, L: 'a;
    type WriteGuard<'a> = RwLockWriteGuard<'a, T> where T: 'a, L: 'a;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.rwlock.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.rwlock.write()
    }
}

impl<T, L: LockAfter<UninterruptibleLock>> OrderedRwLock<T, L> {
    pub const fn new(t: T) -> Self {
        Self { rwlock: RwLock::new(t), _phantom: PhantomData }
    }

    pub fn read<'a, P>(
        &'a self,
        locked: &'a mut Locked<'_, P>,
    ) -> <Self as RwLockFor<L>>::ReadGuard<'a>
    where
        P: LockBefore<L>,
    {
        locked.read_lock(self)
    }

    pub fn write<'a, P>(
        &'a self,
        locked: &'a mut Locked<'_, P>,
    ) -> <Self as RwLockFor<L>>::WriteGuard<'a>
    where
        P: LockBefore<L>,
    {
        locked.write_lock(self)
    }

    pub fn read_and<'a, P>(
        &'a self,
        locked: &'a mut Locked<'_, P>,
    ) -> (<Self as RwLockFor<L>>::ReadGuard<'a>, Locked<'a, L>)
    where
        P: LockBefore<L>,
    {
        locked.read_lock_and(self)
    }

    pub fn write_and<'a, P>(
        &'a self,
        locked: &'a mut Locked<'_, P>,
    ) -> (<Self as RwLockFor<L>>::WriteGuard<'a>, Locked<'a, L>)
    where
        P: LockBefore<L>,
    {
        locked.write_lock_and(self)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Unlocked;

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

    mod lock_levels {
        //! Lock ordering tree:
        //! Unlocked -> A -> B -> C
        //!          -> D -> E -> F
        use crate::{LockAfter, UninterruptibleLock, Unlocked};
        use lock_ordering_macro::lock_ordering;
        lock_ordering! {
            Unlocked => A,
            A => B,
            B => C,
            Unlocked => D,
            D => E,
            E => F,
        }

        impl LockAfter<UninterruptibleLock> for A {}
        impl LockAfter<UninterruptibleLock> for B {}
        impl LockAfter<UninterruptibleLock> for C {}
        impl LockAfter<UninterruptibleLock> for D {}
        impl LockAfter<UninterruptibleLock> for E {}
        impl LockAfter<UninterruptibleLock> for F {}
    }

    use lock_levels::{A, B, C, D, E, F};

    #[test]
    fn test_ordered_mutex() {
        let a: OrderedMutex<u8, A> = OrderedMutex::new(15);
        let _b: OrderedMutex<u16, B> = OrderedMutex::new(30);
        let c: OrderedMutex<u32, C> = OrderedMutex::new(45);

        let mut locked = Unlocked::new();

        let (a_data, mut next_locked) = a.lock_and(&mut locked);
        let c_data = c.lock(&mut next_locked);

        // This won't compile
        //let _b_data = _b.lock(&mut locked);
        //let _b_data = _b.lock(&mut next_locked);

        assert_eq!(&*a_data, &15);
        assert_eq!(&*c_data, &45);
    }
    #[test]
    fn test_ordered_rwlock() {
        let d: OrderedRwLock<u8, D> = OrderedRwLock::new(15);
        let _e: OrderedRwLock<u16, E> = OrderedRwLock::new(30);
        let f: OrderedRwLock<u32, F> = OrderedRwLock::new(45);

        let mut locked = Unlocked::new();
        {
            let (d_data, mut next_locked) = d.write_and(&mut locked);
            let f_data = f.read(&mut next_locked);

            // This won't compile
            //let _e_data = _e.read(&mut locked);
            //let _e_data = _e.read(&mut next_locked);

            assert_eq!(&*d_data, &15);
            assert_eq!(&*f_data, &45);
        }
        {
            let (d_data, mut next_locked) = d.read_and(&mut locked);
            let f_data = f.write(&mut next_locked);

            // This won't compile
            //let _e_data = _e.write(&mut locked);
            //let _e_data = _e.write(&mut next_locked);

            assert_eq!(&*d_data, &15);
            assert_eq!(&*f_data, &45);
        }
    }

    #[test]
    fn test_lock_both() {
        let a1: OrderedMutex<u8, A> = OrderedMutex::new(15);
        let a2: OrderedMutex<u8, A> = OrderedMutex::new(30);
        let mut locked = Unlocked::new();
        {
            let (a1_data, a2_data, _) = lock_both(&mut locked, &a1, &a2);
            assert_eq!(&*a1_data, &15);
            assert_eq!(&*a2_data, &30);
        }
        {
            let (a2_data, a1_data, _) = lock_both(&mut locked, &a2, &a1);
            assert_eq!(&*a1_data, &15);
            assert_eq!(&*a2_data, &30);
        }
    }
}
