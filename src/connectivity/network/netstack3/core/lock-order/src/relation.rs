// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Marker trait that indicates that `Self` can be locked after `A`.
///
/// Lock types should implement this to specify that in the lock ordering
/// graph, `A` comes before `Self`. So if `B: LockAfter<A>`, lock type `B` can
/// be acquired after `A` but `A` cannot be acquired before `B`.
pub trait LockAfter<A> {}

/// Marker trait that indicates that `Self` is an ancestor of `X`.
///
/// Functions and trait impls that want to apply lock ordering bounds should use
/// this instead of [`LockAfter`]. Types should prefer to implement `LockAfter`
/// instead of this trait. Like [`From`] and [`Into`], a blanket impl of
/// `LockBefore` is provided for all types that implement `LockAfter`
pub trait LockBefore<X> {}

impl<B: LockAfter<A>, A> LockBefore<B> for A {}

#[macro_export]
macro_rules! impl_lock_after {
    ($A:ty => $B:ty) => {
        impl lock_order::relation::LockAfter<$A> for $B {}
        impl<X: lock_order::relation::LockBefore<$A>> lock_order::relation::LockAfter<X> for $B {}
    };
}

#[cfg(test)]
mod test {
    use crate::{lock::LockFor, relation::LockAfter, Locked, Unlocked};

    extern crate self as lock_order;

    enum A {}
    enum B {}
    enum C {}

    impl_lock_after!(A => B);
    impl_lock_after!(B => C);

    impl LockAfter<Unlocked> for A {}

    struct FakeLocked {
        a: u32,
        c: char,
    }

    impl LockFor<A> for FakeLocked {
        type Data<'l> = &'l u32 where Self: 'l;
        fn lock(&self) -> Self::Data<'_> {
            &self.a
        }
    }

    impl LockFor<C> for FakeLocked {
        type Data<'l> = &'l char where Self: 'l;
        fn lock(&self) -> Self::Data<'_> {
            &self.c
        }
    }

    #[test]
    fn lock_a_then_c() {
        let state = FakeLocked { a: 123, c: '4' };

        let mut locked = Locked::new(&state);

        let (a, mut locked): (_, Locked<'_, FakeLocked, A>) = locked.lock_and::<A>();
        assert_eq!(a, &123);
        // Show that A: LockBefore<B> and B: LockBefore<C> => A: LockBefore<C>.
        // Otherwise this wouldn't compile:
        let c = locked.lock::<C>();
        assert_eq!(c, &'4');
    }
}
