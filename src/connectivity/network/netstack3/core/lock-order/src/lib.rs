// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Tools for describing and enforcing lock acquisition order.
//!
//! Using code defines lock levels with types and then implements traits from
//! this crate, like [`relation::LockAfter`] to describe how those locks can
//! be acquired. A separate set of traits in [`lock`] implement locked access
//! for your type. A complete example:
//!
//! ```
//! use std::sync::Mutex;
//! use lock_order::{impl_lock_after, lock::LockFor, relation::LockAfter, Locked, Unlocked};
//!
//! #[derive(Default)]
//! struct HoldsLocks {
//!     a: Mutex<u8>,
//!     b: Mutex<u32>,
//! }
//!
//! enum LockA {}
//! enum LockB {}
//!
//! impl LockFor<LockA> for HoldsLocks {
//!     type Data = u8;
//!     type Guard<'l> = std::sync::MutexGuard<'l, u8>
//!         where Self: 'l;
//!     fn lock(&self) -> Self::Guard<'_> {
//!         self.a.lock().unwrap()
//!     }
//! }
//!
//! impl LockFor<LockB> for HoldsLocks {
//!     type Data = u32;
//!     type Guard<'l> = std::sync::MutexGuard<'l, u32>
//!         where Self: 'l;
//!     fn lock(&self) -> Self::Guard<'_> {
//!         self.b.lock().unwrap()
//!     }
//! }
//!
//! // LockA is the top of the lock hierarchy.
//! impl LockAfter<Unlocked> for LockA {}
//! // LockA can be acquired before LockB.
//! impl_lock_after!(LockA => LockB);
//!
//! // Accessing locked state looks like this:
//!
//! let state = HoldsLocks::default();
//! // Create a new lock session with the "root" lock level (empty tuple).
//! let mut locked = Locked::new(&state);
//! // Access locked state.
//! let (a, mut locked_a) = locked.lock_and::<LockA>();
//! let b = locked_a.lock::<LockB>();
//! ```
//!
//! The methods on [`Locked`] prevent out-of-order locking according to the
//! specified lock relationships.
//!
//! This won't compile because `LockB` does not implement `LockBefore<LockA>`:
//! ```compile_fail
//! # use std::sync::Mutex;
//! # use lock_order::{impl_lock_after, lock::LockFor, relation::LockAfter, Locked, Unlocked};
//! #
//! # #[derive(Default)]
//! # struct HoldsLocks {
//! #     a: Mutex<u8>,
//! #     b: Mutex<u32>,
//! # }
//! #
//! # enum LockA {}
//! # enum LockB {}
//! #
//! # impl LockFor<LockA> for HoldsLocks {
//! #     type Data = u8;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u8>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.a.lock().unwrap()
//! #     }
//! # }
//! #
//! # impl LockFor<LockB> for HoldsLocks {
//! #     type Data = u32;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u32>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.b.lock().unwrap()
//! #     }
//! # }
//! #
//! # // LockA is the top of the lock hierarchy.
//! # impl LockAfter<Unlocked> for LockA {}
//! # // LockA can be acquired before LockB.
//! # impl_lock_after!(LockA => LockB);
//! #
//! #
//! let state = HoldsLocks::default();
//! let mut locked = Locked::new(&state);
//!
//! // Locking B without A is fine, but locking A after B is not.
//! let (b, mut locked_b) = locked.lock_and::<LockB>();
//! // compile error: LockB does not implement LockBefore<LockA>
//! let a = locked_b.lock::<LockA>();
//! ```
//!
//! Even if the lock guard goes out of scope, the new `Locked` instance returned
//! by [Locked::lock_and] will prevent the original one from being used to
//! access state. This doesn't work:
//! ```compile_fail
//! # use std::sync::Mutex;
//! # use lock_order::{impl_lock_after, lock::LockFor, relation::LockAfter, Locked, Unlocked};
//! #
//! # #[derive(Default)]
//! # struct HoldsLocks {
//! #     a: Mutex<u8>,
//! #     b: Mutex<u32>,
//! # }
//! #
//! # enum LockA {}
//! # enum LockB {}
//! #
//! # impl LockFor<LockA> for HoldsLocks {
//! #     type Data = u8;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u8>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.a.lock().unwrap()
//! #     }
//! # }
//! #
//! # impl LockFor<LockB> for HoldsLocks {
//! #     type Data = u32;
//! #     type Guard<'l> = std::sync::MutexGuard<'l, u32>
//! #         where Self: 'l;
//! #     fn lock(&self) -> Self::Guard<'_> {
//! #         self.b.lock().unwrap()
//! #     }
//! # }
//! #
//! # // LockA is the top of the lock hierarchy.
//! # impl LockAfter<Unlocked> for LockA {}
//! # // LockA can be acquired before LockB.
//! # impl_lock_after!(LockA => LockB);
//! #
//! #
//! let state = HoldsLocks::default();
//! let mut locked = Locked::new(&state);
//!
//! let (b, mut locked_b) = locked.lock_and::<LockB>();
//! drop(b);
//! let b = locked_b.lock::<LockB>();
//! // Won't work; `locked` is mutably borrowed by `locked_b`.
//! let a = locked.lock::<LockA>();
//! ```
//!
//! The [`impl_lock_after`] macro provides implementations of `LockAfter` for
//! a pair of locks. The complete lock ordering tree can be spelled out by
//! calling `impl_lock_after` for each parent and child in the hierarchy. One
//! of the upsides to using `impl_lock_after` is that it also prevents
//! accidental lock ordering inversion. This won't compile:
//! ```compile_fail
//! enum LockA {}
//! enum LockB {}
//!
//! impl_lock_after(LockA => LockB);
//! impl_lock_after(LockB => LockA);
//! ```

#![cfg_attr(not(test), no_std)]

pub mod lock;
pub mod relation;

use core::marker::PhantomData;

use crate::{
    lock::{LockFor, RwLockFor, UnlockedAccess},
    relation::LockBefore,
};

/// Enforcement mechanism for lock ordering.
///
/// `Locked` won't allow locking that violates the described lock order. It
/// enforces this by allowing access to state so long as either
///   1. the state does not require a lock to access, or
///   2. the state does require a lock and that lock comes after the current
///      lock level in the global lock order.
///
/// In the locking case, acquiring a lock produces the new state and a new
/// `Locked` instance that mutably borrows from the original instance. This
/// means the original instance can't be used to acquire new locks until the
/// new instance leaves scope.
pub struct Locked<T, L>(T, PhantomData<L>);

/// "Highest" lock level
///
/// The lock level for the thing returned by `Locked::new`. Users of this crate
/// should implement `LockAfter<Unlocked>` for the root of any lock ordering
/// trees.
pub struct Unlocked;

impl<'a, T> Locked<&'a T, Unlocked> {
    /// Entry point for locked access.
    ///
    /// `Unlocked` is the "root" lock level and can be acquired before any lock.
    pub fn new(t: &'a T) -> Self {
        Self::new_locked(t)
    }
}

impl<'a, T, L> Locked<&'a T, L> {
    /// Entry point for locked access.
    ///
    /// Creates a new `Locked` that restricts locking to levels after `L`. This
    /// is safe because any acquirable locks must have a total ordering, and
    /// restricting the set of locks doesn't violate that ordering.
    pub fn new_locked(t: &'a T) -> Locked<&'a T, L> {
        Self(t, PhantomData)
    }

    /// Access some state that doesn't require locking.
    ///
    /// This allows access to state that doesn't require locking (and depends on
    /// [`UnlockedAccess`] to be implemented only in cases where that is true).
    pub fn unlocked_access<M>(&self) -> T::Guard<'a>
    where
        T: UnlockedAccess<M>,
    {
        let Self(t, PhantomData) = self;
        T::access(t)
    }
}

// It's important that the lifetime on `Locked` here be anonymous. That means
// that the lifetimes in the returned `Locked` objects below are inferred to
// be the lifetimes of the references to self (mutable or immutable).
impl<T, L> Locked<&T, L> {
    /// Acquire the given lock.
    ///
    /// This requires that `M` can be locked after `L`.
    pub fn lock<M>(&mut self) -> T::Guard<'_>
    where
        T: LockFor<M>,
        L: LockBefore<M>,
    {
        let (data, _): (_, Locked<&T, M>) = self.lock_and();
        data
    }

    /// Acquire the given lock and a new locked context.
    ///
    /// This requires that `M` can be locked after `L`.
    pub fn lock_and<M>(&mut self) -> (T::Guard<'_>, Locked<&T, M>)
    where
        T: LockFor<M>,
        L: LockBefore<M>,
    {
        let Self(t, PhantomData) = self;
        let data = T::lock(t);
        (data, Locked(t, PhantomData))
    }

    /// Attempt to acquire the given read lock.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn read_lock<M>(&mut self) -> T::ReadGuard<'_>
    where
        T: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let (data, _): (_, Locked<&T, M>) = self.read_lock_and();
        data
    }

    /// Attempt to acquire the given read lock and a new locked context.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn read_lock_and<M>(&mut self) -> (T::ReadGuard<'_>, Locked<&T, M>)
    where
        T: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let Self(t, PhantomData) = self;
        let data = T::read_lock(t);
        (data, Locked(t, PhantomData))
    }

    /// Attempt to acquire the given write lock.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn write_lock<M>(&mut self) -> T::WriteGuard<'_>
    where
        T: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let (data, _): (_, Locked<&T, M>) = self.write_lock_and();
        data
    }

    /// Attempt to acquire the given write lock.
    ///
    /// For accessing state via reader/writer locks. This requires that `M` can
    /// be locked after `L`.
    pub fn write_lock_and<M>(&mut self) -> (T::WriteGuard<'_>, Locked<&T, M>)
    where
        T: RwLockFor<M>,
        L: LockBefore<M>,
    {
        let Self(t, PhantomData) = self;
        let data = T::write_lock(t);
        (data, Locked(t, PhantomData))
    }

    /// Narrow the type on which locks can be acquired.
    ///
    /// This allows scoping down the state on which locks are acquired. This is
    /// safe because
    ///   1. the lock ordering does not take the type being locked into
    ///      account, so there's no danger of lock ordering being different for
    ///      `T` and `R`,
    ///   2. because the new `&R` references a part of the original `&S`, so it
    ///      can't allow access to a different locking domain, and
    ///   3. the returned `Locked` instance borrows `self` mutably so it can't
    ///      be used until the new instance is dropped.
    pub fn cast<R>(&mut self) -> Locked<&R, L>
    where
        T: AsRef<R>,
    {
        self.cast_with(AsRef::as_ref)
    }

    /// Narrow the type on which locks can be acquired.
    ///
    /// Like `cast`, but with a callable function instead of using `AsRef`. The
    /// same safety arguments apply.
    pub fn cast_with<R>(&mut self, f: impl FnOnce(&T) -> &R) -> Locked<&R, L> {
        let Self(t, PhantomData) = self;
        Locked(f(t), PhantomData)
    }

    /// Restrict locking as if a lock was acquired.
    ///
    /// Like `lock_and` but doesn't actually acquire the lock `M`. This is
    /// safe because any locks that could be acquired with the lock `M` held can
    /// also be acquired without `M` being held.
    pub fn cast_locked<M>(&mut self) -> Locked<&T, M>
    where
        L: LockBefore<M>,
    {
        let Self(t, _marker) = self;
        Locked(t, PhantomData)
    }

    /// Convenience function for accessing copyable state.
    ///
    /// This, combined with `cast` or `cast_with`, makes it easy to access
    /// non-locked state.
    pub fn copied(&self) -> T
    where
        T: Copy,
    {
        let Self(t, PhantomData) = self;
        **t
    }
}

#[cfg(test)]
mod test {
    use std::sync::{Mutex, MutexGuard};

    mod lock_levels {
        //! Lock ordering tree:
        //! A -> B -> {C, D}

        extern crate self as lock_order;

        use crate::{impl_lock_after, relation::LockAfter, Unlocked};

        pub enum A {}
        pub enum B {}
        pub enum C {}
        pub enum D {}

        impl LockAfter<Unlocked> for A {}
        impl_lock_after!(A => B);
        impl_lock_after!(B => C);
        impl_lock_after!(B => D);
    }

    use crate::{
        lock::{LockFor, UnlockedAccess},
        Locked,
    };
    use lock_levels::{A, B, C, D};

    /// Data type with multiple locked fields.
    #[derive(Default)]
    struct Data {
        a: Mutex<u8>,
        b: Mutex<u16>,
        c: Mutex<u64>,
        d: Mutex<u128>,
        u: usize,
    }

    impl LockFor<A> for Data {
        type Data = u8;
        type Guard<'l> = MutexGuard<'l, u8>;
        fn lock(&self) -> Self::Guard<'_> {
            self.a.lock().unwrap()
        }
    }

    impl LockFor<B> for Data {
        type Data = u16;
        type Guard<'l> = MutexGuard<'l, u16>;
        fn lock(&self) -> Self::Guard<'_> {
            self.b.lock().unwrap()
        }
    }

    impl LockFor<C> for Data {
        type Data = u64;
        type Guard<'l> = MutexGuard<'l, u64>;
        fn lock(&self) -> Self::Guard<'_> {
            self.c.lock().unwrap()
        }
    }

    impl LockFor<D> for Data {
        type Data = u128;
        type Guard<'l> = MutexGuard<'l, u128>;
        fn lock(&self) -> Self::Guard<'_> {
            self.d.lock().unwrap()
        }
    }

    #[derive(Debug)]
    struct NotPresent;

    enum UnlockedUsize {}

    impl UnlockedAccess<UnlockedUsize> for Data {
        type Data = usize;
        type Guard<'l> = &'l usize where Self: 'l;

        fn access(&self) -> Self::Guard<'_> {
            &self.u
        }
    }

    #[test]
    fn lock_a_then_c() {
        let data = Data::default();

        let mut w = Locked::new(&data);
        let (_a, mut wa) = w.lock_and::<A>();
        let (_c, _wc) = wa.lock_and::<C>();
        // This won't compile!
        // let _b = _wc.lock::<B>();
    }

    #[test]
    fn unlocked_access_does_not_prevent_locking() {
        let data = Data { a: Mutex::new(15), u: 34, ..Data::default() };

        let mut locked = Locked::new(&data);
        let u = locked.unlocked_access::<UnlockedUsize>();

        // Prove that `u` does not prevent locked state from being accessed.
        let a = locked.lock::<A>();
        assert_eq!(u, &34);
        assert_eq!(&*a, &15);
    }
}
