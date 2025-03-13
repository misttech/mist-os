// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::marker::PhantomData;
use core::mem::forget;
use core::ops::Deref;
use core::ptr::NonNull;

/// An owned value in borrowed backing memory.
///
/// While the owned value may be dropped, it does not provide a mutable reference to the contained
/// value.
pub struct Owned<'buf, T: ?Sized> {
    ptr: NonNull<T>,
    _phantom: PhantomData<&'buf mut [u8]>,
}

// SAFETY: `Owned` doesn't add any restrictions on sending across thread boundaries, and so is
// `Send` if `T` is `Send`.
unsafe impl<T: Send + ?Sized> Send for Owned<'_, T> {}

// SAFETY: `Owned` doesn't add any interior mutability, so it is `Sync` if `T` is `Sync`.
unsafe impl<T: Sync + ?Sized> Sync for Owned<'_, T> {}

impl<T: ?Sized> Drop for Owned<'_, T> {
    fn drop(&mut self) {
        unsafe {
            self.ptr.as_ptr().drop_in_place();
        }
    }
}

impl<T: ?Sized> Owned<'_, T> {
    /// Returns an `Owned` of the given pointer.
    ///
    /// # Safety
    ///
    /// `new_unchecked` takes ownership of the pointed-to value. It must point
    /// to a valid value that is not aliased.
    pub unsafe fn new_unchecked(ptr: *mut T) -> Self {
        Self { ptr: unsafe { NonNull::new_unchecked(ptr) }, _phantom: PhantomData }
    }

    /// Consumes the `Owned`, returning its pointer to the owned value.
    pub fn into_raw(self) -> *mut T {
        let result = self.ptr.as_ptr();
        forget(self);
        result
    }
}

impl<T: ?Sized> Deref for Owned<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: fmt::Debug + ?Sized> fmt::Debug for Owned<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}
