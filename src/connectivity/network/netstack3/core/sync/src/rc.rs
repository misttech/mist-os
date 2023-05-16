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

use alloc::vec::Vec;
use core::{
    convert::AsRef,
    hash::{Hash, Hasher},
    num::NonZeroUsize,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

use derivative::Derivative;

mod named {
    //! Provides tracking of instances via human-readable names.
    //!
    //! Names are only tracked in debug builds. All operations and types
    //! are no-ops and empty unless the `rc-debug-names` feature is enabled.

    use core::marker::PhantomData;

    /// Records reference-counted names of instances.
    #[derive(Debug, Default)]
    pub(super) struct StrongNames {
        /// The names that were inserted and aren't known to be gone.
        ///
        /// This holds weak references to allow callers to drop without
        /// synchronizing. Invalid weak pointers are cleaned up periodically but
        /// are not logically present.
        #[cfg(feature = "rc-debug-names")]
        names: crate::Mutex<Vec<alloc::sync::Weak<str>>>,
    }

    impl StrongNames {
        #[cfg(feature = "rc-debug-names")]
        #[cfg(test)]
        pub(super) fn names(&self) -> &crate::Mutex<Vec<alloc::sync::Weak<str>>> {
            &self.names
        }
    }

    /// Instance of a name produced by [`Names`].
    ///
    /// This should be held as a field of the named instance. When it goes out of
    /// scope, the reference count in the creating `Names` is decremented.
    #[derive(Clone, Debug)]
    pub(super) struct StrongName(#[cfg(feature = "rc-debug-names")] alloc::sync::Arc<str>);

    impl StrongName {
        pub(crate) fn downgrade(&self) -> WeakName {
            // We need to be careful to create a new Arc that is not connected
            // to the original one, because otherwise the weak name would
            // keep the name in Names alive.
            WeakName(
                #[cfg(feature = "rc-debug-names")]
                {
                    let Self(name) = self;
                    let name: &str = &*name;
                    alloc::sync::Arc::from(name)
                },
            )
        }
    }

    #[cfg(test)]
    #[cfg(feature = "rc-debug-names")]
    impl AsRef<str> for super::named::StrongName {
        fn as_ref(&self) -> &str {
            let Self(name) = self;
            name
        }
    }

    /// A weak name that isn't tracked by the originating [`Names`] instance.
    #[derive(Clone, Debug)]
    pub(super) struct WeakName(
        /// The human-readable name.
        ///
        /// Use an `Arc<str>` instead of a `String` to allow clones of a
        /// `WeakName` instance to cheaply share the backing memory.
        #[cfg(feature = "rc-debug-names")]
        alloc::sync::Arc<str>,
    );

    impl WeakName {
        pub(super) fn new(name: &str) -> Self {
            #[cfg(not(feature = "rc-debug-names"))]
            {
                let _ = name;
            }
            WeakName(
                #[cfg(feature = "rc-debug-names")]
                {
                    alloc::sync::Arc::from(name)
                },
            )
        }
    }

    impl StrongNames {
        /// Creates a new [`Name`] from the given name.
        ///
        /// On debug builds, the returned `Name` is tied to an internal
        /// reference count that will be decremented when the `Name` and all of
        /// its clones are dropped. Until then, the provided name will be
        /// available via [`Names::with_names`].
        ///
        /// On non-debug builds, this is a no-op.
        pub(super) fn insert(&self, name: &str) -> StrongName {
            #[cfg(not(feature = "rc-debug-names"))]
            {
                let _ = name;
            }
            StrongName(
                #[cfg(feature = "rc-debug-names")]
                {
                    let Self { names } = self;
                    let mut names = names.lock();
                    let mut i = 0;
                    // Iterate over the names to find one that matches, while
                    // also opportunistically pruning dead names to prevent
                    // unbounded memory growth. We do this with indices instead
                    // of iterators so that we can use Vec::swap_remove to
                    // remove items in-place instead of allocating a new vector.
                    while i < names.len() {
                        let Some(existing_name) = names[i].upgrade() else {
                            let _: alloc::sync::Weak<str> = names.swap_remove(i);
                            continue;
                        };
                        if name == &*existing_name {
                            return StrongName(existing_name);
                        }
                        i += 1;
                    }
                    let arc_name = alloc::sync::Arc::from(name);
                    let weak = alloc::sync::Arc::downgrade(&arc_name);
                    names.push(weak);
                    arc_name
                },
            )
        }

        /// Creates a new [`Name`] from the given weak name.
        ///
        /// Like [`Names::insert`], but with an existing [`WeakName`] instead
        /// of a string name.
        pub(super) fn insert_weak(&self, weak: &WeakName) -> StrongName {
            #[cfg(feature = "rc-debug-names")]
            {
                let WeakName(name) = weak;
                self.insert(&**name)
            }
            #[cfg(not(feature = "rc-debug-names"))]
            {
                let _: &WeakName = weak;
                StrongName()
            }
        }

        /// Provides access to names known to be alive.
        ///
        /// On debug builds, `cb` is called with an iterator that yields the
        /// names that are still alive (corresponding to calls to
        /// [`Names::insert`] and [`Names::insert_weak`] whose `Name` objects
        /// and their clones haven't all been dropped).
        ///
        /// On non-debug builds, the provided iterator is always empty.
        pub(super) fn with_names<R>(&self, cb: impl FnOnce(Iter<'_>) -> R) -> R {
            cb(Iter(
                #[cfg(feature = "rc-debug-names")]
                {
                    (&*self.names.lock()).into_iter()
                },
                #[cfg(not(feature = "rc-debug-names"))]
                {
                    core::iter::empty()
                },
                PhantomData,
            ))
        }
    }

    pub(super) struct Iter<'a>(
        #[cfg(feature = "rc-debug-names")] core::slice::Iter<'a, alloc::sync::Weak<str>>,
        #[cfg(not(feature = "rc-debug-names"))] core::iter::Empty<()>,
        PhantomData<&'a ()>,
    );

    impl Iterator for Iter<'_> {
        type Item = StrongName;

        fn next(&mut self) -> Option<Self::Item> {
            let Self(iter, _marker) = self;
            iter.find_map(|item| {
                #[cfg(not(feature = "rc-debug-names"))]
                {
                    let _ = item;
                }
                Some(StrongName(
                    #[cfg(feature = "rc-debug-names")]
                    {
                        item.upgrade()?
                    },
                ))
            })
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            let Self(it, _marker) = self;
            let (_lower, upper) = it.size_hint();
            (0, upper)
        }
    }
}

#[derive(Debug)]
struct Inner<T> {
    marked_for_destruction: AtomicBool,
    strong_names: named::StrongNames,
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
        let Inner { marked_for_destruction, data: _, strong_names: _ } = self;
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
        let Inner { marked_for_destruction, data: _, strong_names } = inner.as_ref();

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

        // Make sure that this `Primary` is the last thing to hold a strong
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
            "extra_refs_on_drops={:?}, names={:?}",
            extra_refs_on_drops,
            strong_names.with_names(|strong_names| { strong_names.collect::<Vec<_>>() })
        );
    }
}

/// Like [`Clone`] but with a debug-only name attached to the new instance.
pub trait NamedClone {
    /// Creates a new instance of `self` with the provided name.
    fn named_clone(&self, name: &'static str) -> Self;
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
        let Inner { marked_for_destruction: _, data, strong_names: _ } = inner.deref();
        data
    }
}

const FIRST_STRONG_NAME: &str = "strong-from-primary";

impl<T> Primary<T> {
    /// Returns a new strongly-held reference.
    pub fn new(data: T) -> Primary<T> {
        Primary {
            extra_refs_on_drops: None,
            inner: alloc::sync::Arc::new(Inner {
                marked_for_destruction: AtomicBool::new(false),
                strong_names: named::StrongNames::default(),
                data,
            }),
        }
    }

    /// Clones a strongly-held reference.
    pub fn clone_strong(Self { extra_refs_on_drops: _, inner }: &Self) -> Strong<T> {
        let Inner { data: _, strong_names: holders, marked_for_destruction: _ } = &**inner;
        let name = holders.insert(FIRST_STRONG_NAME);
        Strong { inner: alloc::sync::Arc::clone(inner), name }
    }

    /// Returns a weak reference pointing to the same underlying data.
    pub fn downgrade(Self { extra_refs_on_drops: _, inner }: &Self, name: &'static str) -> Weak<T> {
        Weak(alloc::sync::Arc::downgrade(inner), named::WeakName::new(name))
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(
        Self { extra_refs_on_drops: _, inner: this }: &Self,
        Strong { inner: other, name: _ }: &Strong<T>,
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
                let Inner { marked_for_destruction, data, strong_names: holders } = &mut inner;

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
                    // Explicitly drop since we do not need these anymore.
                    core::ptr::drop_in_place(marked_for_destruction);
                    core::ptr::drop_in_place(holders);

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
pub struct Strong<T> {
    inner: alloc::sync::Arc<Inner<T>>,
    #[allow(dead_code)]
    name: named::StrongName,
}

impl<T> AsRef<T> for Strong<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T> Deref for Strong<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let Self { inner, name: _ } = self;
        let Inner { marked_for_destruction: _, data, strong_names: _ } = inner.deref();
        data
    }
}

impl<T> Hash for Strong<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self { inner, name: _ } = self;
        alloc::sync::Arc::as_ptr(inner).hash(state)
    }
}

impl<T> Clone for Strong<T> {
    fn clone(&self) -> Self {
        let Self { inner, name } = self;
        let Inner { data: _, marked_for_destruction: _, strong_names: _ } = &**inner;
        Self { inner: alloc::sync::Arc::clone(inner), name: name.clone() }
    }
}

impl<T> NamedClone for Strong<T> {
    fn named_clone(&self, name: &'static str) -> Self {
        let Self { inner, name: _ } = self;
        let Inner { data: _, marked_for_destruction: _, strong_names } = &**inner;
        let name = strong_names.insert(name);
        Self { inner: alloc::sync::Arc::clone(inner), name }
    }
}

impl<T> Strong<T> {
    /// Returns a weak reference pointing to the same underlying data.
    pub fn downgrade(Self { inner, name }: &Self) -> Weak<T> {
        let weak = name.downgrade();
        Weak(alloc::sync::Arc::downgrade(inner), weak)
    }

    /// Returns true if the inner value has since been marked for destruction.
    pub fn marked_for_destruction(Self { inner, name: _ }: &Self) -> bool {
        let Inner { marked_for_destruction, data: _, strong_names: _ } = inner.as_ref();
        // `Ordering::Acquire` because we want to synchronize with with the
        // `Ordering::Release` write to `marked_for_destruction` so that all
        // memory writes before the reference was marked for destruction is
        // visible here.
        marked_for_destruction.load(Ordering::Acquire)
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn weak_ptr_eq(Self { inner: this, name: _ }: &Self, Weak(other, _name): &Weak<T>) -> bool {
        core::ptr::eq(alloc::sync::Arc::as_ptr(this), other.as_ptr())
    }

    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(
        Self { inner: this, name: _ }: &Self,
        Self { inner: other, name: _ }: &Self,
    ) -> bool {
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
pub struct Weak<T>(alloc::sync::Weak<Inner<T>>, named::WeakName);

impl<T> Hash for Weak<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self(this, _name) = self;
        this.as_ptr().hash(state)
    }
}

impl<T> Clone for Weak<T> {
    fn clone(&self) -> Self {
        let Self(this, name) = self;
        Weak(this.clone(), name.clone())
    }
}

impl<T> Weak<T> {
    /// Returns true if the two pointers point to the same allocation.
    pub fn ptr_eq(&self, Self(other, _name): &Self) -> bool {
        let Self(this, _name) = self;
        this.ptr_eq(other)
    }

    /// Attempts to upgrade to a [`Strong`].
    ///
    /// Returns `None` if the inner value has since been marked for destruction.
    pub fn upgrade(&self) -> Option<Strong<T>> {
        let Self(weak, name) = self;
        let arc = weak.upgrade()?;
        let Inner { marked_for_destruction, data: _, strong_names: holders } = arc.deref();

        // `Ordering::Acquire` because we want to synchronize with with the
        // `Ordering::Release` write to `marked_for_destruction` so that all
        // memory writes before the reference was marked for destruction is
        // visible here.
        if !marked_for_destruction.load(Ordering::Acquire) {
            let name = holders.insert_weak(name);
            Some(Strong { inner: arc, name })
        } else {
            None
        }
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

    #[cfg(feature = "rc-debug-names")]
    #[test]
    fn strong_named() {
        use std::collections::HashSet;

        let primary = Primary::new(10);
        let strong = Primary::clone_strong(&primary);
        let Primary { extra_refs_on_drops: _, inner } = &primary;
        let Inner { data: _, marked_for_destruction: _, strong_names } = &**inner;

        let expect_names = |expected| {
            strong_names.with_names(|it| {
                let names: Vec<_> = it.collect();
                let names: HashSet<_> = names.iter().map(|name| name.as_ref()).collect();
                assert_eq!(names, expected);
            })
        };

        // There is just the one strong reference.
        expect_names(HashSet::from([FIRST_STRONG_NAME]));

        // Cloning it without providing a name doesn't create a new name.
        {
            let _other_strong = strong.clone();
            expect_names(HashSet::from([FIRST_STRONG_NAME]));
        }

        {
            const NAME: &str = "some name";
            let _named_strong = strong.named_clone(NAME);
            expect_names(HashSet::from([NAME, FIRST_STRONG_NAME]));
        }
        // After that goes out of scope, the name goes away.
        expect_names(HashSet::from([FIRST_STRONG_NAME]));

        drop(strong);
        expect_names(HashSet::from([]));
    }

    #[cfg(feature = "rc-debug-names")]
    #[test]
    #[should_panic(expected = "other strong")]
    fn strong_names_in_panic() {
        let primary = Primary::new(10);
        let first_strong = Primary::clone_strong(&primary);
        let _strong = first_strong.named_clone("other strong");
        drop(first_strong);
        drop(primary);
    }

    #[cfg(feature = "rc-debug-names")]
    #[test]
    fn name_tracking_memory_usage() {
        // Creating and deleting strong and weak references doesn't result in
        // the Names container becoming too large.
        let primary = Primary::new(10);

        const NUM_ITERATIONS: usize = 10_000;
        let first_strong = Primary::clone_strong(&primary);

        for _ in 0..NUM_ITERATIONS {
            let strong = first_strong.named_clone("other strong");
            let _weak = Strong::downgrade(&strong);
            drop(strong);
        }

        let Primary { extra_refs_on_drops: _, inner } = &primary;
        let Inner { data: _, marked_for_destruction: _, strong_names } = &**inner;

        let strong_names = &**strong_names.names().lock();
        assert_matches::assert_matches!(strong_names, [_first_strong, _other_strong]);
    }

    #[cfg(feature = "rc-debug-names")]
    #[test]
    fn names_insert_does_not_skip() {
        // Regression test to ensure that, while inserting a new name,
        // Names::insert doesn't skip any entries.
        let names = named::StrongNames::default();
        let [_a, b, _c] = ["a", "b", "c"].map(|name| names.insert(&name));
        names.with_names(|names| {
            let names = names.collect::<Vec<_>>();
            let name_strs = names.iter().map(AsRef::as_ref).collect::<Vec<_>>();
            assert_eq!(name_strs, &["a", "b", "c"]);
        });

        drop(b);

        let _another_c = names.insert("c");
        names.with_names(|names| {
            let names = names.collect::<Vec<_>>();
            let name_strs = names.iter().map(AsRef::as_ref).collect::<Vec<_>>();
            assert_eq!(name_strs, &["a", "c"]);
        });
    }
}
