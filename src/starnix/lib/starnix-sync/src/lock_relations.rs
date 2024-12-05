// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Traits used to describe lock-ordering relationships in a way that does not
//! introduce cycles.
//!
//! This module defines the traits used to describe lock-ordering relationships,
//! [`LockBefore`] and [`LockAfter`]. They are reciprocals, like [`From`] and
//! [`Into`] in the standard library, and like `From` and `Into`, a blanket impl
//! is provided for `LockBefore` for implementations of `LockAfter`.
//!
//! It's recommended to use `lock_ordering!` macro to construct the lock ordering
//! graph instead of implementing `LockAfter<B> for A` on your own lock level
//! types `A` and `B`. This is because the macro implements `LockBefore` for all
//! the levels that a given level is reachable from, but also because it checks
//! the graph for the absence of cycles.

/// Marker trait that indicates that `Self` can be locked after `A`.
///
/// This should be implemented for lock types to specify that, in the lock
/// ordering graph, `A` comes before `Self`. So if `B: LockAfter<A>`, lock type
/// `B` can be acquired after `A` but `A` cannot be acquired after `B`.
///
/// Note, though, that it's preferred to use the [`lock_ordering`] macro
/// instead of writing trait impls directly to avoid the possibility of lock
/// ordering cycles.
pub trait LockAfter<A> {}

/// Marker trait that indicates that `Self` is an ancestor of `X`.
///
/// Functions and trait impls that want to apply lock ordering bounds should use
/// this instead of [`LockAfter`]. Types should prefer to implement `LockAfter`
/// instead of this trait. Like [`From`] and [`Into`], a blanket impl of
/// `LockBefore` is provided for all types that implement `LockAfter`
pub trait LockBefore<X> {}

impl<B: LockAfter<A>, A> LockBefore<B> for A {}

/// Marker trait that indicates that `Self` is `X` or an ancestor of `X`.
///
/// Functions and trait impls that want to apply lock ordering bounds should
/// prefer [`LockBefore`]. However, there are some situations where using
/// template to apply lock ordering bounds is impossible, so a fixed level
/// must be used instead. In that case, `LockEqualOrBefore` can be used as
/// a workaround to avoid restricting other methods to just the fixed level.
/// See the tests for the example.
/// Note: Any type representing a lock level must explicitly implement
/// `LockEqualOrBefore<X> for X` (or use a `lock_ordering` macro) for this to
/// work.
pub trait LockEqualOrBefore<X> {}

impl<B, A> LockEqualOrBefore<B> for A where A: LockBefore<B> {}

// Define a lock level that corresponds to some state that can be locked.
#[macro_export]
macro_rules! lock_level {
    ($A:ident) => {
        pub enum $A {}
        impl $crate::LockEqualOrBefore<$A> for $A {}
        static_assertions::const_assert_eq!(std::mem::size_of::<$crate::Locked<'static, $A>>(), 0);
    };
}

#[cfg(test)]
mod test {
    use crate::{LockBefore, LockEqualOrBefore, LockFor, Locked, Unlocked};
    use lock_ordering_macro::lock_ordering;
    use std::sync::{Mutex, MutexGuard};
    extern crate self as starnix_sync;

    lock_ordering! {
        Unlocked => A,
        A => B,
        B => C,
    }

    pub struct FakeLocked {
        a: Mutex<u32>,
        c: Mutex<char>,
    }

    impl LockFor<A> for FakeLocked {
        type Data = u32;
        type Guard<'l>
            = MutexGuard<'l, u32>
        where
            Self: 'l;
        fn lock(&self) -> Self::Guard<'_> {
            self.a.lock().unwrap()
        }
    }

    impl LockFor<C> for FakeLocked {
        type Data = char;
        type Guard<'l>
            = MutexGuard<'l, char>
        where
            Self: 'l;
        fn lock(&self) -> Self::Guard<'_> {
            self.c.lock().unwrap()
        }
    }

    #[test]
    fn lock_a_then_c() {
        const A_DATA: u32 = 123;
        const C_DATA: char = '4';
        let state = FakeLocked { a: A_DATA.into(), c: C_DATA.into() };

        let mut locked = unsafe { Unlocked::new() };

        let (a, mut locked) = locked.lock_and::<A, _>(&state);
        assert_eq!(*a, A_DATA);
        // Show that A: LockBefore<B> and B: LockBefore<C> => A: LockBefore<C>.
        // Otherwise this wouldn't compile:
        let c = locked.lock::<C, _>(&state);
        assert_eq!(*c, C_DATA);
    }

    fn access_c<L: LockBefore<C>>(locked: &mut Locked<'_, L>, state: &FakeLocked) -> char {
        let c = locked.lock::<C, _>(state);
        *c
    }

    // Uses a fixed level B
    fn fixed1(locked: &mut Locked<'_, B>, state: &FakeLocked) -> char {
        relaxed(locked, state)
    }

    // Uses a fixed level B
    fn fixed2(locked: &mut Locked<'_, B>, state: &FakeLocked) -> char {
        access_c(locked, state)
    }

    // `LockBefore<B>` cannot be used here because then it would be impossible
    // to use it from fixed1. `LockBefore<C>` can't be used because then
    // `cast_locked::<B>` wouldn't work (there could be other ancestors of `C`)
    fn relaxed<L>(locked: &mut Locked<'_, L>, state: &FakeLocked) -> char
    where
        L: LockEqualOrBefore<B>,
    {
        let mut locked = locked.cast_locked::<B>();
        fixed2(&mut locked, state)
    }

    #[test]
    fn lock_equal_or_before() {
        const A_DATA: u32 = 123;
        const C_DATA: char = '4';
        let state = FakeLocked { a: A_DATA.into(), c: C_DATA.into() };
        let mut locked = unsafe { Unlocked::new() };

        assert_eq!(relaxed(&mut locked, &state), C_DATA);
        let mut locked = locked.cast_locked::<A>();
        assert_eq!(relaxed(&mut locked, &state), C_DATA);
        let mut locked = locked.cast_locked::<B>();
        assert_eq!(relaxed(&mut locked, &state), C_DATA);
        assert_eq!(fixed1(&mut locked, &state), C_DATA);
        // This won't compile as C: LockEqualOrBefore<B> is not satisfied.
        // let mut locked = locked.cast_locked::<C>();
        // assert_eq!(relaxed(&mut locked, &state), C_DATA);
    }

    #[test]
    fn build_graph_test() {
        lock_ordering! {
            Unlocked => LevelA,
            Unlocked => LevelB,
            LevelB => LevelC,
            LevelA => LevelC
            LevelC => LevelD
            // This creates a cycle:
            // LevelD => LevelA,
        }

        #[derive(Default)]
        pub struct HoldsLocks {
            a: Mutex<u8>,
        }

        impl LockFor<LevelA> for HoldsLocks {
            type Data = u8;
            type Guard<'l>
                = std::sync::MutexGuard<'l, u8>
            where
                Self: 'l;
            fn lock(&self) -> Self::Guard<'_> {
                self.a.lock().unwrap()
            }
        }

        let state = HoldsLocks::default();
        // Create a new lock session with the "root" lock level.
        let mut locked = unsafe { Unlocked::new() };
        // Access locked state.
        let (_a, _locked_a) = locked.lock_and::<LevelA, _>(&state);
    }
}
