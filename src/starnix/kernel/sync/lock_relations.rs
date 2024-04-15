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
//! It's recommended that, instead of implementing `LockAfter<B> for A` on your
//! own lock level types `A` and `B`, you use the [`impl_lock_after`] macro
//! instead. Why? Because in addition to emitting an implementation of
//! `LockAfter`, it also provides a blanket implementation equivalent to this:
//!
//! ```no_run
//! impl <X> LockAfter<X> for B where A: LockAfter<X> {}
//! ```
//!
//! The blanket implementations are useful for inferring trait implementations
//! but have a more important purpose: they make it impossible to introduce
//! cycles in our lock ordering graph, and that's *the* key property we want to
//! uphold.
//!
//! To see how this happens, let's look at the trait implementations. Suppose
//! we have a lock ordering graph that looks like this:
//!
//! ```text
//! A -> B -> C
//! ```
//!
//! Assuming we are using `impl_lock_after`, that gives us the following trait
//! implementations:
//!
//! ```no_run
//! // Graph edges
//! impl LockAfter<A> for B {}
//! impl LockAfter<B> for C {}
//! // Blanket impls that get us transitivity
//! impl<X> LockAfter<X> for B where A: LockAfter<X> {} // 1
//! impl<X> LockAfter<X> for C where B: LockAfter<X> {} // 2
//! ```
//! Now suppose we added an edge `C -> A` (introducing a cycle, which should result
//! in a compilation error). That would give us these two impls:
//!
//! ```no_run
//! // New edge
//! impl LockAfter<C> for A {}
//! // New blanket impl
//! impl<X> LockAfter<X> for A where C: LockAfter<X> {}
//! ```
//!
//! The compiler will follow the blanket impls to produce implicit `LockAfter`
//! implementations like this:
//!
//!   1. Our added edge satisfies the `where` clause for blanket impl 1 with
//!      `X=C`, so the compiler infers an implicit `impl LockAfter<C> for B {}`.
//!   2. That satisfies the conditions for blanket impl 2 (`X=C`), so now we
//!      also have `impl LockAfter<C> for C {}`.
//!   3. This satisfies the condition for our new blanket impl with
//!      `X=C`; now the compiler adds an implicit `impl LockAfter<C> for A {}`.
//!
//! Depicted visually, the compiler combines specific and blanket impls like
//! this:
//!
//! ```text
//! ┌─────────────────────────┐┌──────────────────────────────────────────────────┐
//! │ impl LockAfter<C> for A ││ impl<X> LockAfter<X> for B where A: LockAfter<X> │
//! └┬────────────────────────┘└┬─────────────────────────────────────────────────┘
//! ┌▽──────────────────────────▽┐┌──────────────────────────────────────────────────┐
//! │ impl LockAfter<C> for B    ││ impl<X> LockAfter<X> for C where B: LockAfter<X> │
//! └┬───────────────────────────┘└┬─────────────────────────────────────────────────┘
//! ┌▽─────────────────────────────▽┐┌──────────────────────────────────────────────────┐                                                  │
//! │ impl LockAfter<C> for C       ││ impl<X> LockAfter<X> for A where C: LockAfter<X> │                                                                                            │
//! └┬──────────────────────────────┘└┬─────────────────────────────────────────────────┘
//! ┌▽────────────────────────────────▽┐
//! │ impl LockAfter<C> for A  (again) │
//! └──────────────────────────────────┘
//!
//! ```
//! The final implicit trait implementation has the exact same trait
//! (`LockAfter<C>`) and type (`A`) as the explicit implementation we added with
//! our graph edge, so the compiler detects the duplication and rejects our
//! code. This works not just with the graph above, but with any graph that
//! includes a cycle.

/// Marker trait that indicates that `Self` can be locked after `A`.
///
/// This should be implemented for lock types to specify that, in the lock
/// ordering graph, `A` comes before `Self`. So if `B: LockAfter<A>`, lock type
/// `B` can be acquired after `A` but `A` cannot be acquired after `B`.
///
/// Note, though, that it's preferred to use the [`impl_lock_after`] macro
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
/// `LockEqualOrBefore<X> for X` (or use a `lock_level` macro) for this to
/// work.
pub trait LockEqualOrBefore<X> {}

impl<B, A> LockEqualOrBefore<B> for A where A: LockBefore<B> {}

// Defines the order between two lock levels. Lock B comes after A if B
// can be acquired while A is locked.
#[macro_export]
macro_rules! impl_lock_after {
    (Unlocked => $A:ty) => {
        impl $crate::LockAfter<Unlocked> for $A {}
    };
    ($A:ty => $B:ty) => {
        impl $crate::LockAfter<$A> for $B {}
        impl<X: $crate::LockBefore<$A>> $crate::LockAfter<X> for $B {}
    };
}

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
    use std::sync::{Mutex, MutexGuard};

    lock_level!(A);
    lock_level!(B);
    lock_level!(C);

    impl_lock_after!(A => B);
    impl_lock_after!(B => C);

    impl_lock_after!(Unlocked => A);

    pub struct FakeLocked {
        a: Mutex<u32>,
        c: Mutex<char>,
    }

    impl LockFor<A> for FakeLocked {
        type Data = u32;
        type Guard<'l> = MutexGuard<'l, u32> where Self: 'l;
        fn lock(&self) -> Self::Guard<'_> {
            self.a.lock().unwrap()
        }
    }

    impl LockFor<C> for FakeLocked {
        type Data = char;
        type Guard<'l> = MutexGuard<'l, char> where Self: 'l;
        fn lock(&self) -> Self::Guard<'_> {
            self.c.lock().unwrap()
        }
    }

    #[test]
    fn lock_a_then_c() {
        const A_DATA: u32 = 123;
        const C_DATA: char = '4';
        let state = FakeLocked { a: A_DATA.into(), c: C_DATA.into() };

        let mut locked = Unlocked::new();

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
        let mut locked = Unlocked::new();

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
}
