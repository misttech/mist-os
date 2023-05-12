// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Synchronized reference counting primitives.
//!
//! This module introduces a family of reference counted types that allows
//! marking the underlying data for destruction before all strongly references
//! to the data are dropped. This enables the following features:
//!   * Upgrading a weak reference to a strong reference succeeds iff at least
//!     one strong reference exists _and_ the data has not been marked for
//!     destruction.
//!   * Allow waiting for all strongly-held references to be dropped after
//!     marking the data. (TODO: https://fxbug.dev/122388).

use core::{
    convert::AsRef,
    hash::{Hash, Hasher},
    num::NonZeroUsize,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};
use derivative::Derivative;

#[derive(Debug)]
struct Inner<T> {
    marked_for_destruction: AtomicBool,
    data: T,
}

impl<T> Inner<T> {
    fn pre_drop_check(marked_for_destruction: &AtomicBool) {
        // `Ordering::Acquire` because we want to synchronize with with the
        // `Ordering::Release` write to `marked_for_destruction` so that all
        // memory writes before the reference was marked for destruction is
        // visible here.
        assert!(marked_for_destruction.load(Ordering::Acquire), "Must be marked for destruction");
    }
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        let Inner { marked_for_destruction, data: _ } = self;
        Self::pre_drop_check(marked_for_destruction)
    }
}

/// A primary reference.
///
/// Note that only one `Primary` may be associated with data. This is
/// enforced by not implementing [`Clone`].
///
/// For now, this reference is no different than a [`Strong`] but later changes
/// will enable blocking the destruction of a primary reference until all
/// strongly held references are dropped.
// TODO(https://fxbug.dev/122388): Implement the blocking.
#[derive(Debug)]
pub struct Primary<T> {
    /// Used to let this `Primary`'s `Drop::drop` implementation know if
    /// there are any extra (inner) [`alloc::sync::Arc`]s held.
    extra_refs_on_drops: Option<NonZeroUsize>,
    inner: alloc::sync::Arc<Inner<T>>,
}

const ONE_NONZERO_USIZE: NonZeroUsize = nonzero_ext::nonzero!(1_usize);

impl<T> Drop for Primary<T> {
    fn drop(&mut self) {
        let Self { extra_refs_on_drops, inner } = self;
        let Inner { marked_for_destruction, data: _ } = inner.as_ref();

        // `Ordering::Release` because want to make sure that all memory writes
        // before dropping this `Primary` synchronizes with later attempts to
        // upgrade weak pointers and the `Drop::drop` impl of `Inner`.
        //
        // TODO(https://fxbug.dev/122388): Require explicit marking for
        // destruction of the reference and support blocking.
        let was_marked = marked_for_destruction.swap(true, Ordering::Release);

        // Make debugging easier: don't panic if a panic is already happening
        // since double-panics are annoying to debug.
        if std::thread::panicking() {
            return;
        }

        assert_eq!(was_marked, false, "Must not be marked for destruction yet");

        // Make sure that this `Killable` is the last thing to hold a strong
        // reference to the underlying data when it is being dropped.
        //
        // Note that this family of reference counted pointers is always used
        // under a single lock so we know two threads will not concurrently
        // attempt to handle this drop method and attempt to upgrade a weak
        // reference.
        //
        // TODO(https://fxbug.dev/122388): Require explicit killing of the
        // reference and support blocking instead of this panic here.
        assert_eq!(
            alloc::sync::Arc::strong_count(inner),
            1 + extra_refs_on_drops.map_or(0, NonZeroUsize::get),
            "extra_refs_on_drops={:?}",
            extra_refs_on_drops,
        );
    }
}

impl<T> AsRef<T> for Primary<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T> Deref for Primary<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let Self { extra_refs_on_drops: _, inner } = self;
        let Inner { marked_for_destruction: _, data } = inner.deref();
        data
    }
}

impl<T> Primary<T> {
    /// Returns a new strongly-held reference.
    pub fn new(data: T) -> Primary<T> {
        Primary {
            extra_refs_on_drops: None,
            inner: alloc::sync::Arc::new(Inner {
                marked_for_destruction: AtomicBool::new(false),
                data,
            }),
        }
    }

    /// Clones a strongly-held reference.
    pub fn clone_strong(Self { extra_refs_on_drops: _, inner }: &Self) -> Strong<T> {
        Strong(alloc::sync::Arc::clone(inner))
    }

    /// Returns a weak reference pointing to the same underlying data.
    pub fn downgrade(Self { extra_refs_on_drops: _, inner }: &Self) -> Weak<T> {
        Weak(alloc::sync::Arc::downgrade(inner))
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(
        Self { extra_refs_on_drops: _, inner: this }: &Self,
        Strong(other): &Strong<T>,
    ) -> bool {
        alloc::sync::Arc::ptr_eq(this, other)
    }

    /// Returns the inner value if no [`Strong`] references are held.
    ///
    /// # Panics
    ///
    /// Panics if [`Strong`] references are held when this function is called.
    pub fn unwrap(mut this: Self) -> T {
        let inner = {
            let Self { extra_refs_on_drops, inner } = &mut this;
            let inner = alloc::sync::Arc::clone(inner);
            // We are going to drop the `Primary` but while holding a reference
            // to the inner `alloc::sync::Arc`. Let this `Primary`'s `Drop::drop`
            // impl know that we have an extra reference.
            assert_eq!(core::mem::replace(extra_refs_on_drops, Some(ONE_NONZERO_USIZE)), None,);
            core::mem::drop(this);
            inner
        };

        match alloc::sync::Arc::try_unwrap(inner) {
            Ok(mut inner) => {
                // We cannot destructure `inner` by value since `Inner`
                // implements `Drop`. As a workaround, we ptr-read `data` and
                // `core::mem::forget` `inner` to prevent `inner` from being
                // destroyed which will result in `data` being destroyed as
                // well.
                let Inner { marked_for_destruction, data } = &mut inner;

                // Make sure that `inner` is in a valid state for destruction.
                //
                // Note that we do not actually destroy all of `inner` here; we
                // decompose it into its parts, keeping what we need & throwing
                // away what we don't. Regardless, we perform the same checks.
                Inner::<T>::pre_drop_check(marked_for_destruction);

                // Safe since we know the reference (`inner`) points to a valid
                // object and `inner` is forgotten below so the destructor for
                // `inner` (and its fields) will not be run as a result of
                // `inner` being dropped.
                let data = unsafe {
                    // Explicitly drop since we do not need this anymore.
                    core::ptr::drop_in_place(marked_for_destruction);

                    // Read the data to return to the caller.
                    core::ptr::read(data)
                };

                // Forget inner now to prevent its `Drop::drop` impl from being
                // run which will attempt to destroy `data` but still perform
                // pre-drop checks on `Inner`'s state.
                core::mem::forget(inner);

                // We now own `data` and its destructor will not run (until
                // dropped by the caller).
                data
            }
            Err(inner) => {
                // Unreachable because `Primary`'s drop impl would have panic-ed
                // if we had any [`Strong`]s held.
                unreachable!("still had strong refs: {}", alloc::sync::Arc::strong_count(&inner))
            }
        }
    }
}

/// A strongly-held reference.
///
/// Similar to an [`alloc::sync::Arc`] but holding a `Strong` acts as a
/// witness to the live-ness of the underlying data. That is, holding a
/// `Strong` implies that the underlying data has not yet been destroyed.
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
        let Inner { marked_for_destruction: _, data } = arc.deref();
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

    /// Returns true if the inner value has since been marked for destruction.
    pub fn marked_for_destruction(Self(arc): &Self) -> bool {
        let Inner { marked_for_destruction, data: _ } = arc.as_ref();
        // `Ordering::Acquire` because we want to synchronize with with the
        // `Ordering::Release` write to `marked_for_destruction` so that all
        // memory writes before the reference was marked for destruction is
        // visible here.
        marked_for_destruction.load(Ordering::Acquire)
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn weak_ptr_eq(Self(this): &Self, Weak(other): &Weak<T>) -> bool {
        core::ptr::eq(alloc::sync::Arc::as_ptr(this), other.as_ptr())
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
    /// Returns `None` if the inner value has since been marked for destruction.
    pub fn upgrade(&self) -> Option<Strong<T>> {
        let Self(weak) = self;
        let arc = weak.upgrade()?;
        let Inner { marked_for_destruction, data: _ } = arc.deref();

        // `Ordering::Acquire` because we want to synchronize with with the
        // `Ordering::Release` write to `marked_for_destruction` so that all
        // memory writes before the reference was marked for destruction is
        // visible here.
        (!marked_for_destruction.load(Ordering::Acquire)).then(|| Strong(arc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zombie_weak() {
        let primary = Primary::new(());
        let weak = {
            let strong = Primary::clone_strong(&primary);
            Strong::downgrade(&strong)
        };
        core::mem::drop(primary);

        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn rcs() {
        const INITIAL_VAL: u8 = 1;
        const NEW_VAL: u8 = 2;

        let primary = Primary::new(std::sync::Mutex::new(INITIAL_VAL));
        let strong = Primary::clone_strong(&primary);
        let weak = Strong::downgrade(&strong);

        *primary.lock().unwrap() = NEW_VAL;
        assert_eq!(*primary.deref().lock().unwrap(), NEW_VAL);
        assert_eq!(*strong.deref().lock().unwrap(), NEW_VAL);
        assert_eq!(*weak.upgrade().unwrap().deref().lock().unwrap(), NEW_VAL);
    }

    #[test]
    fn unwrap_primary_without_strong_held() {
        const VAL: u16 = 6;
        let primary = Primary::new(VAL);
        assert_eq!(Primary::unwrap(primary), VAL);
    }

    #[test]
    #[should_panic(expected = "extra_refs_on_drops=Some(1)")]
    fn unwrap_primary_with_strong_held() {
        let primary = Primary::new(8);
        let _strong: Strong<_> = Primary::clone_strong(&primary);
        let _: u16 = Primary::unwrap(primary);
    }

    #[test]
    #[should_panic(expected = "extra_refs_on_drops=None")]
    fn drop_primary_with_strong_held() {
        let primary = Primary::new(9);
        let _strong: Strong<_> = Primary::clone_strong(&primary);
        core::mem::drop(primary);
    }
}
