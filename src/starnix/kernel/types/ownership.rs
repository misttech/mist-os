// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This crates introduces a framework to handle explicit ownership.
//!
//! Explicit ownership is used for object that needs to be cleaned, but cannot use `Drop` because
//! the release operation requires a context. For example, when using the rust types to ensures the
//! locking order, taking a lock requires knowing what locks are already held at this point, and
//! uses an explicit object to represent this. If the object needs to take a lock during the
//! release operation, `Drop` cannot provide it.
//!
//! An object that uses explicit ownership uses the `Releasable` trait. The user must calls the
//! `release` method on it before it goes out of scope.
//!
//! A shared object that used explicit ownership used the `OwnedRef`/`WeakRef`/`TempRef`
//! containers.
//! The meaning are the following:
//! - Object that owns the shared object use `OwnedRef`. They are responsible to call `release`
//! before dropping the reference.
//! - Object that do not owned the shared object use `WeakRef`. This acts as a weak reference to
//! the object. They can convert it to a strong reference using the `upgrade` method. The returned
//! value is an `Option<TempRef>`. The `TempRef` allows access to the object. Because this doesn't
//! repsent ownership, the `TempRef` must not be kept, in particular, the user should not do any
//! blocking operation while having a `TempRef`.

// TODO(fxbug.dev/131097): Create a linter to ensure TempRef is not held while calling any blocking
// operation.

use core::hash::Hasher;
use fuchsia_zircon as zx;
use std::{
    hash::Hash,
    ops::Deref,
    sync::{
        atomic::{fence, AtomicUsize, Ordering},
        Arc, Weak,
    },
};

/// The base trait for explicit ownership. Any `Releasable` object must call `release` before
/// being dropped.
pub trait Releasable {
    type Context;

    // TODO(https://fxbug.dev/131095): This method should take `self` instead of `&self`.
    fn release(&self, c: &Self::Context);
}

/// An owning reference to a shared owned object. Each instance must call `release` before being
/// dropped.
/// `OwnedRef` will panic on Drop in debug builds if it has not been released.
#[must_use = "OwnedRef must be released"]
pub struct OwnedRef<T: Releasable> {
    /// The shared data.
    inner: Arc<RefInner<T>>,

    /// A guard that will ensure a panic on drop if the ref has not been released.
    drop_guard: DropGuard,
}

impl<T: Releasable> OwnedRef<T> {
    pub fn new(value: T) -> Self {
        Self { inner: RefInner::new(value), drop_guard: Default::default() }
    }

    /// Produce a `WeakRef` from a `OwnedRef`.
    pub fn downgrade(this: &Self) -> WeakRef<T> {
        WeakRef(Arc::downgrade(&this.inner))
    }

    /// Produce a `TempRef` from a `OwnedRef`. As an `OwnedRef` exists at the time of the creation,
    /// this cannot fail.
    pub fn temp(this: &Self) -> TempRef<'_, T> {
        TempRef::new(this.inner.clone())
    }
}

impl<T: Releasable + std::fmt::Debug> std::fmt::Debug for OwnedRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OwnedRef").field(&self.inner.value).finish()
    }
}

impl<T: Releasable> Clone for OwnedRef<T> {
    /// Clone the `OwnedRef`. Both the current and the new reference needs to be `release`d.
    fn clone(&self) -> Self {
        let previous_count = self.inner.owned_refs_count.fetch_add(1, Ordering::Relaxed);
        debug_assert!(previous_count > 0, "OwnedRef should not be used after being released.");
        Self { inner: Arc::clone(&self.inner), drop_guard: Default::default() }
    }
}

impl<T: Releasable> Releasable for OwnedRef<T> {
    type Context = T::Context;

    /// Release the `OwnedRef`. If this is the last instance, this method will block until all
    /// `TempRef` instances are dropped, and will release the underlying object.
    fn release(&self, c: &Self::Context) {
        self.drop_guard.disarm();
        let previous_count = self.inner.owned_refs_count.fetch_sub(1, Ordering::Release);
        if previous_count == 1 {
            fence(Ordering::Acquire);
            self.inner.wait_for_no_ref();
            self.inner.value.release(c);
        }
    }
}

impl<T: Releasable + Default> Default for OwnedRef<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Releasable> std::ops::Deref for OwnedRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.deref().value
    }
}

impl<T: Releasable> std::borrow::Borrow<T> for OwnedRef<T> {
    fn borrow(&self) -> &T {
        self.deref()
    }
}

impl<T: Releasable> std::convert::AsRef<T> for OwnedRef<T> {
    fn as_ref(&self) -> &T {
        self.deref()
    }
}

impl<T: Releasable + PartialEq> PartialEq for OwnedRef<T> {
    fn eq(&self, other: &OwnedRef<T>) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner) || **self == **other
    }
}

impl<T: Releasable + Eq> Eq for OwnedRef<T> {}

impl<T: Releasable + PartialOrd> PartialOrd for OwnedRef<T> {
    fn partial_cmp(&self, other: &OwnedRef<T>) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: Releasable + Ord> Ord for OwnedRef<T> {
    fn cmp(&self, other: &OwnedRef<T>) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: Releasable + Hash> Hash for OwnedRef<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

/// A weak reference to a shared owned object. The `upgrade` method try to build a `TempRef` from a
/// `WeakRef` and will fail if there is no `OwnedRef` left.
pub struct WeakRef<T>(Weak<RefInner<T>>);

impl<T> WeakRef<T> {
    pub fn new() -> Self {
        Self(Weak::new())
    }

    /// Try to upgrade the `WeakRef` into a `TempRef`. This will fail as soon as the last
    /// `OwnedRef` is released, even if some `TempRef` still exist at that time. The returned
    /// `TempRef` must be dropped as soon as possible. In particular, it must not be kept across
    /// blocking calls.
    pub fn upgrade(&self) -> Option<TempRef<'_, T>> {
        if let Some(value) = self.0.upgrade() {
            // As soon as the Arc has been upgraded, creates a TempRef to ensure the futex is woken
            // up in case `upgrade` and `release` are racing.
            let temp_ref = TempRef::new(value);
            // Only returns a valid `TempRef` if there are still some un-released `OwnedRef`. As
            // soon as `release` is called, no more `TempRef` can be acquire.
            if temp_ref.0.owned_refs_count.load(Ordering::Acquire) > 0 {
                return Some(temp_ref);
            }
        }
        None
    }
}

impl<T> Default for WeakRef<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for WeakRef<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Releasable> From<&OwnedRef<T>> for WeakRef<T> {
    fn from(owner: &OwnedRef<T>) -> Self {
        OwnedRef::downgrade(owner)
    }
}

/// A temporary reference to a shared owned object. This permits access to the shared object, but
/// will block any thread trying to release the last `OwnedRef`. As such, such reference must be
/// released as soon as possible. In particular, one must not do any blocking operation while
/// owning such a refeence.
// Until negative trait bound are implemented, using `*mut u8` to prevent transferring TempRef
// across threads.
pub struct TempRef<'a, T>(Arc<RefInner<T>>, std::marker::PhantomData<(&'a T, *mut u8)>);

impl<'a, T> Drop for TempRef<'a, T> {
    fn drop(&mut self) {
        self.0.dec_temp_ref();
    }
}

impl<'a, T> TempRef<'a, T> {
    /// Build a new TempRef. Ensures `temp_refs_count` is correctly updated.
    fn new(inner: Arc<RefInner<T>>) -> Self {
        inner.inc_temp_ref();
        Self(inner, Default::default())
    }

    /// This allows to change the lifetime annotation of a `TempRef` to static.
    ///
    /// # Safety
    ///
    /// As TempRef must be dropped as soon as possible, this provided the way to block the release
    /// of the related `OwnedRef`s and as such is considered unsafe. Any caller must ensure that
    /// the returned `TempRef` is not kept around while doing blocking calls.
    pub unsafe fn into_static(this: Self) -> TempRef<'static, T> {
        TempRef::new(this.0.clone())
    }
}

impl<'a, T: Releasable> From<&'a OwnedRef<T>> for TempRef<'a, T> {
    fn from(owner: &'a OwnedRef<T>) -> Self {
        OwnedRef::temp(owner)
    }
}

impl<'a, T> From<&TempRef<'a, T>> for WeakRef<T> {
    fn from(temp_ref: &TempRef<'a, T>) -> Self {
        Self(Arc::downgrade(&temp_ref.0))
    }
}

impl<'a, T> std::ops::Deref for TempRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.deref().value
    }
}

impl<'a, T> std::borrow::Borrow<T> for TempRef<'a, T> {
    fn borrow(&self) -> &T {
        &self.0.deref().value
    }
}

impl<'a, T> std::convert::AsRef<T> for TempRef<'a, T> {
    fn as_ref(&self) -> &T {
        &self.0.deref().value
    }
}

impl<'a, T: PartialEq> PartialEq for TempRef<'a, T> {
    fn eq(&self, other: &TempRef<'_, T>) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || **self == **other
    }
}

impl<'a, T: Eq> Eq for TempRef<'a, T> {}

impl<'a, T: PartialOrd> PartialOrd for TempRef<'a, T> {
    fn partial_cmp(&self, other: &TempRef<'_, T>) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<'a, T: Ord> Ord for TempRef<'a, T> {
    fn cmp(&self, other: &TempRef<'_, T>) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<'a, T: Hash> Hash for TempRef<'a, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

/// The internal data of `OwnedRef`/`WeakRef`/`TempRef`.
///
/// To ensure that `wait_for_no_ref` is correct, the following constraints must apply:
/// - Once `owned_refs_count` reaches 0, it must never increase again.
/// - The strong count of `Arc<Self>` must always be incremented before `temp_refs_count` is
///   incremented.
/// - Whenever a the strong count of `Arc<Self>` is incremented, `temp_ref_count` must be
///   increased.
/// This ensures that `wait_for_no_ref` will always be notified when it is waiting on the
/// `temp_refs_count` futex and the number of `TempRef` reaches 0.
struct RefInner<T> {
    /// The underlying value.
    value: T,
    /// The number of `OwnedRef` sharing this data.
    owned_refs_count: AtomicUsize,
    /// The number of `TempRef` sharing this data.
    // This is close to a duplicate of the Arc strong_count, and could be replaced by it if this
    // module reimplemented all of Arc/Weak. This can be changed without changing the API if this
    // becomes a performance issue.
    temp_refs_count: zx::sys::zx_futex_t,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Number of `TempRef` in the current thread. This is used to ensure there is no `TempRef`
    /// while doing a blocking operation.
    static TEMP_REF_LOCAL_COUNT: std::cell::RefCell<usize> = std::cell::RefCell::new(0);
}

/// Assert that no temp ref exist on the current thread. This is used before executing a blocking
/// operation to ensure it will not prevent a OwnedRef release.
pub fn debug_assert_no_local_temp_ref() {
    #[cfg(any(test, debug_assertions))]
    {
        TEMP_REF_LOCAL_COUNT.with(|count| {
            assert_eq!(*count.borrow(), 0, "Current threads owns {} TempRef", *count.borrow());
        });
    }
}

impl<T> RefInner<T> {
    fn new(value: T) -> Arc<Self> {
        Arc::new(Self { value, owned_refs_count: AtomicUsize::new(1), temp_refs_count: 0.into() })
    }

    /// Increase `temp_refs_count`. Must be called each time a new `TempRef` is built.
    fn inc_temp_ref(&self) {
        self.temp_refs_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(any(test, debug_assertions))]
        {
            TEMP_REF_LOCAL_COUNT.with(|count| {
                *count.borrow_mut() += 1;
            });
        }
    }

    /// Decrease `temp_refs_count`. Must be called each time a new `TempRef` is dropped.
    ///
    /// This will wake the futex on `temp_refs_count` when it reaches 0.
    fn dec_temp_ref(&self) {
        let previous_count = self.temp_refs_count.fetch_sub(1, Ordering::Release);
        if previous_count == 1 {
            fence(Ordering::Acquire);
            unsafe {
                // SAFETY: This is a ffi call to a zircon syscall.
                let result = zx::sys::zx_futex_wake(&self.temp_refs_count, 1);
                debug_assert_eq!(result, zx::sys::ZX_OK);
            }
        }
        #[cfg(any(test, debug_assertions))]
        {
            TEMP_REF_LOCAL_COUNT.with(|count| {
                *count.borrow_mut() -= 1;
            });
        }
    }

    /// Wait for this Arc to be the only left reference to the data. This should only be called on
    /// once the last `OwnedRef` has been released. This will wait for all existing `TempRef` to be
    /// dropped before returning.
    fn wait_for_no_ref(self: &Arc<Self>) {
        loop {
            // Ensure no more `OwnedRef` exists.
            debug_assert_eq!(self.owned_refs_count.load(Ordering::Acquire), 0);
            // If the strong count of the Arc is 1, there is no existing `TempRef`. None can be
            // created because `owned_refs_count` is 0. Return.
            if Arc::strong_count(self) == 1 {
                debug_assert_eq!(self.temp_refs_count.load(Ordering::Acquire), 0);
                return;
            }
            // Compute the current number of temp refs, and wait for it to drop to 0.
            let current_value = self.temp_refs_count.load(Ordering::Acquire);
            if current_value == 0 {
                // It is already 0, loop again.
                continue;
            }
            // Otherwise, wait on the futex that will be waken up when the number of temp_ref drops
            // to 0.
            unsafe {
                // SAFETY: This is a ffi call to a zircon syscall.
                let result = zx::sys::zx_futex_wait(
                    &self.temp_refs_count,
                    current_value.into(),
                    zx::sys::ZX_HANDLE_INVALID,
                    zx::Time::INFINITE.into_nanos(),
                );
                debug_assert!(
                    result == zx::sys::ZX_OK || result == zx::sys::ZX_ERR_BAD_STATE,
                    "Unexpected result: {result}"
                );
            }
        }
    }
}

#[derive(Default, Debug)]
pub struct DropGuard {
    #[cfg(any(test, debug_assertions))]
    released: std::sync::atomic::AtomicBool,
}

impl DropGuard {
    #[inline(always)]
    fn disarm(&self) {
        #[cfg(any(test, debug_assertions))]
        {
            self.released.store(true, Ordering::Release);
        }
    }
}

/// Macro that ensure the releasable is released with the given context if the body returns an
/// error.
macro_rules! release_on_error {
    ($releasable_name:ident, $context:expr, $body:block ) => {{
        #[allow(clippy::redundant_closure_call)]
        let result = { (|| $body)() };
        match result {
            Err(e) => {
                $releasable_name.release($context);
                return Err(e);
            }
            Ok(x) => x,
        }
    }};
}

pub(crate) use release_on_error;

#[cfg(any(test, debug_assertions))]
impl Drop for DropGuard {
    fn drop(&mut self) {
        assert!(*self.released.get_mut());
    }
}

#[cfg(test)]
mod test {
    use super::*;

    struct Data;

    impl Releasable for Data {
        type Context = ();
        fn release(&self, _: &()) {}
    }

    #[::fuchsia::test]
    #[should_panic]
    fn drop_without_release() {
        let _ = OwnedRef::new(Data {});
    }

    #[::fuchsia::test]
    fn test_creation_and_reference() {
        let value = OwnedRef::new(Data {});
        let reference = WeakRef::from(&value);
        reference.upgrade().expect("upgrade");
        value.release(&());
        assert!(reference.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn test_clone() {
        let value = OwnedRef::new(Data {});
        {
            let value2 = OwnedRef::clone(&value);
            value2.release(&());
        }
        #[allow(clippy::redundant_clone)]
        {
            let reference = WeakRef::from(&value);
            let _reference2 = reference.clone();
        }
        value.release(&());
    }

    #[::fuchsia::test]
    fn test_default() {
        let reference = WeakRef::<Data>::default();
        assert!(reference.upgrade().is_none());
    }

    #[::fuchsia::test]
    fn test_release_on_error() {
        fn release_on_error() -> Result<(), ()> {
            let value = OwnedRef::new(Data {});
            release_on_error!(value, &(), {
                if true {
                    return Err(());
                }
                Ok(())
            });
            Ok(())
        }
        assert_eq!(release_on_error(), Err(()));
    }

    #[::fuchsia::test]
    fn test_into_static() {
        let value = OwnedRef::new(Data {});
        let weak = WeakRef::from(&value);
        // SAFETY: This is safe, as static_ref remains on the stack.
        let static_ref = unsafe { TempRef::into_static(weak.upgrade().unwrap()) };
        // Check that weak can now be dropped.
        std::mem::drop(weak);
        // Drop static_ref
        std::mem::drop(static_ref);
        value.release(&());
    }

    #[::fuchsia::test]
    fn test_debug_assert_no_local_temp_ref() {
        debug_assert_no_local_temp_ref();
        let value = OwnedRef::new(Data {});
        debug_assert_no_local_temp_ref();
        let _temp_ref = OwnedRef::temp(&value);
        std::thread::spawn(|| {
            debug_assert_no_local_temp_ref();
        })
        .join()
        .expect("join");
        std::mem::drop(_temp_ref);
        debug_assert_no_local_temp_ref();
        value.release(&());
        debug_assert_no_local_temp_ref();
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_debug_assert_no_local_temp_ref_aborts() {
        let value = OwnedRef::new(Data {});
        {
            let _temp_ref = OwnedRef::temp(&value);
            debug_assert_no_local_temp_ref();
        }
        // This code should not be reached, but ensures the test will fail is
        // `debug_assert_no_local_temp_ref` fails to panic.
        value.release(&());
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_clone_released_owned_ref_abort_in_test() {
        let value = OwnedRef::new(Data {});
        value.release(&());
        let _ = OwnedRef::clone(&value);
    }
}
