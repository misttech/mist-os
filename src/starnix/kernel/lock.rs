// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Use these crates so that we don't need to make the dependency conditional.
#[cfg(test)]
use core::marker::PhantomData;
use fuchsia_sync as _;
use lock_api as _;
use lock_sequence as _;
#[cfg(test)]
use lock_sequence::{lock::LockFor, relation::LockBefore, relation::LockLevel, Locked};
use parking_lot as _;

#[cfg(not(any(test, debug_assertions)))]
pub type Mutex<T> = fuchsia_sync::Mutex<T>;
#[cfg(not(any(test, debug_assertions)))]
pub type MutexGuard<'a, T> = fuchsia_sync::MutexGuard<'a, T>;
#[allow(unused)]
#[cfg(not(any(test, debug_assertions)))]
pub type MappedMutexGuard<'a, T> = fuchsia_sync::MappedMutexGuard<'a, T>;

#[cfg(not(any(test, debug_assertions)))]
pub type RwLock<T> = fuchsia_sync::RwLock<T>;
#[cfg(not(any(test, debug_assertions)))]
pub type RwLockReadGuard<'a, T> = fuchsia_sync::RwLockReadGuard<'a, T>;
#[cfg(not(any(test, debug_assertions)))]
pub type RwLockWriteGuard<'a, T> = fuchsia_sync::RwLockWriteGuard<'a, T>;

#[cfg(any(test, debug_assertions))]
type RawTracingMutex = tracing_mutex::lockapi::TracingWrapper<fuchsia_sync::RawSyncMutex>;
#[cfg(any(test, debug_assertions))]
pub type Mutex<T> = lock_api::Mutex<RawTracingMutex, T>;
#[cfg(any(test, debug_assertions))]
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawTracingMutex, T>;
#[allow(unused)]
#[cfg(any(test, debug_assertions))]
pub type MappedMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawTracingMutex, T>;

#[cfg(any(test, debug_assertions))]
type RawTracingRwLock = tracing_mutex::lockapi::TracingWrapper<fuchsia_sync::RawSyncRwLock>;
#[cfg(any(test, debug_assertions))]
pub type RwLock<T> = lock_api::RwLock<RawTracingRwLock, T>;
#[cfg(any(test, debug_assertions))]
pub type RwLockReadGuard<'a, T> = lock_api::RwLockReadGuard<'a, RawTracingRwLock, T>;
#[cfg(any(test, debug_assertions))]
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
#[cfg(test)]
#[derive(Debug, Default)]
pub struct OrderedMutex<T, L: LockLevel<Source = Self>> {
    mutex: Mutex<T>,
    _phantom: PhantomData<L>,
}

#[cfg(test)]
impl<T, L: LockLevel<Source = Self>> LockFor<L> for OrderedMutex<T, L> {
    type Data = T;
    type Guard<'a> = MutexGuard<'a, T> where T: 'a, L: 'a;
    fn lock(&self) -> Self::Guard<'_> {
        self.mutex.lock()
    }
}

#[cfg(test)]
impl<T, L: LockLevel<Source = Self>> OrderedMutex<T, L> {
    pub fn new(t: T) -> Self {
        Self { mutex: Mutex::new(t), _phantom: Default::default() }
    }

    pub fn lock<'a, P>(&'a self, locked: &'a mut Locked<'a, P>) -> <Self as LockFor<L>>::Guard<'a>
    where
        P: LockBefore<L> + LockLevel,
    {
        locked.lock(self)
    }

    pub fn lock_and<'a, P>(
        &'a self,
        locked: &'a mut Locked<'a, P>,
    ) -> (<Self as LockFor<L>>::Guard<'a>, Locked<'a, L>)
    where
        P: LockBefore<L> + LockLevel,
    {
        locked.lock_and(self)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use lock_sequence::Unlocked;

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
        //! A -> B -> C

        use crate::lock::OrderedMutex;
        use lock_sequence::{impl_lock_after, lock_level, relation::LockAfter, Unlocked};

        lock_level!(A, OrderedMutex<u8, A>);
        lock_level!(B, OrderedMutex<u16, B>);
        lock_level!(C, OrderedMutex<u32, C>);

        impl LockAfter<Unlocked> for A {}
        impl_lock_after!(A => B);
        impl_lock_after!(B => C);
    }

    use lock_levels::{A, B, C};

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
}
