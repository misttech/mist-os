// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Synchronized reference counting primitives.
//!
//! This module introduces a family of reference counted types that allows
//! "killing" the underlying data before all strongly-held references to the
//! data are dropped. This enables the following features:
//!   * Upgrading a weak reference to a strong reference succeeds iff at least
//!     one strong reference exists _and_ the data has not been "killed".
//!   * Allow waiting for all strongly-held references to be dropped after
//!     "killing" the data. (TODO: https://fxbug.dev/122388).

use core::{
    convert::AsRef,
    hash::{Hash, Hasher},
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};
use derivative::Derivative;

#[derive(Debug)]
struct Inner<T> {
    killed: AtomicBool,
    data: T,
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        let Inner { killed, data: _ } = self;

        // `Ordering::Acquire` because we want to synchronize with the
        // `Ordering::Release` write to `killed` so that all memory writes
        // before the reference was killed is visible here.
        assert!(killed.load(Ordering::Acquire), "Must be killed");
    }
}

/// A killable reference.
///
/// Note that only one `Killable` may be associated with data. This is
/// enforced by not implementing [`Clone`].
///
/// For now, this reference is no different than a [`Strong`] but later changes
/// will enable blocking the destruction of a killable reference until all
/// strongly held references are dropped.
// TODO(https://fxbug.dev/122388): Implement the blocking.
#[derive(Debug)]
pub struct Killable<T>(alloc::sync::Arc<Inner<T>>);

impl<T> Drop for Killable<T> {
    fn drop(&mut self) {
        let Self(arc) = self;
        let Inner { killed, data: _ } = arc.as_ref();

        // `Ordering::Release` because want to make sure that all memory writes
        // before dropping this `Killable` synchronizes with later attempts to
        // upgrade weak pointers and the `Drop::drop` impl of `Inner`.
        //
        // TODO(https://fxbug.dev/122388): Require explicit killing of the
        // reference and support blocking.
        assert_eq!(false, killed.swap(true, Ordering::Release), "Must not be killed yet")
    }
}

impl<T> AsRef<T> for Killable<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T> Deref for Killable<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let Self(arc) = self;
        let Inner { killed: _, data } = arc.deref();
        data
    }
}

impl<T> Killable<T> {
    /// Returns a new strongly-held reference.
    pub fn new(data: T) -> Killable<T> {
        Killable(alloc::sync::Arc::new(Inner { killed: AtomicBool::new(false), data }))
    }

    /// Clones a strongly-held reference.
    pub fn clone_strong(Self(arc): &Self) -> Strong<T> {
        Strong(arc.clone())
    }

    /// Returns a weak reference pointing to the same underlying data.
    pub fn downgrade(Self(arc): &Self) -> Weak<T> {
        Weak(alloc::sync::Arc::downgrade(arc))
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(Self(this): &Self, Strong(other): &Strong<T>) -> bool {
        alloc::sync::Arc::ptr_eq(this, other)
    }
}

/// A strongly-held reference.
///
/// Similar to an [`alloc::sync::Arc`] but holding a `Strong` acts as a
/// witness to the live-ness of the underlying data. That is, holding a
/// `Strong` implies that the underlying data has not yet been "killed".
///
/// Note that `Strong`'s implementation of [`Hash`] operates on the pointer
/// itself and not the underlying data.
#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""))]
pub struct Strong<T>(alloc::sync::Arc<Inner<T>>);

impl<T> AsRef<T> for Strong<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T> Deref for Strong<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let Self(arc) = self;
        let Inner { killed: _, data } = arc.deref();
        data
    }
}

impl<T> Hash for Strong<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self(this) = self;
        alloc::sync::Arc::as_ptr(this).hash(state)
    }
}
impl<T> Strong<T> {
    /// Returns a weak reference pointing to the same underlying data.
    pub fn downgrade(Self(arc): &Self) -> Weak<T> {
        Weak(alloc::sync::Arc::downgrade(arc))
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(Self(this): &Self, Self(other): &Self) -> bool {
        alloc::sync::Arc::ptr_eq(this, other)
    }
}

/// A weakly-held reference.
///
/// Similar to an [`alloc::sync::Weak`].
///
/// A `Weak` does not make any claim to the live-ness of the underlying data.
/// Holders of a `Weak` must attempt to upgrade to a [`Strong`] through
/// [`Weak::upgrade`] to access the underlying data.
///
/// Note that `Weak`'s implementation of [`Hash`] operates on the pointer
/// itself and not the underlying data.
#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""))]
pub struct Weak<T>(alloc::sync::Weak<Inner<T>>);

impl<T> Hash for Weak<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self(this) = self;
        this.as_ptr().hash(state)
    }
}

impl<T> Weak<T> {
    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(&self, Self(other): &Self) -> bool {
        let Self(this) = self;
        this.ptr_eq(other)
    }

    /// Attempts to upgrade to a [`Strong`].
    ///
    /// Returns `None` if the inner value has since been killed.
    pub fn upgrade(&self) -> Option<Strong<T>> {
        let Self(weak) = self;
        let arc = weak.upgrade()?;
        let Inner { killed, data: _ } = arc.deref();

        // `Ordering::Acquire` because we want to synchronize with with the
        // `Ordering::Release` write to `killed` so that all memory writes
        // before the reference was killed is visible here.
        (!killed.load(Ordering::Acquire)).then(|| Strong(arc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zombie_weak() {
        let killable = Killable::new(());
        let weak = {
            let strong = Killable::clone_strong(&killable);
            Strong::downgrade(&strong)
        };
        core::mem::drop(killable);

        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn rcs() {
        const INITIAL_VAL: u8 = 1;
        const NEW_VAL: u8 = 2;

        let killable = Killable::new(std::sync::Mutex::new(INITIAL_VAL));
        let strong = Killable::clone_strong(&killable);
        let weak = Strong::downgrade(&strong);

        *killable.lock().unwrap() = NEW_VAL;
        assert_eq!(*killable.deref().lock().unwrap(), NEW_VAL);
        assert_eq!(*strong.deref().lock().unwrap(), NEW_VAL);
        assert_eq!(*weak.upgrade().unwrap().deref().lock().unwrap(), NEW_VAL);
    }
}
