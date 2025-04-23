// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::{forget, ManuallyDrop};
use core::ops::Deref;
use core::ptr::NonNull;

/// A decoded value and the decoder which contains it.
pub struct Decoded<T: ?Sized, D> {
    ptr: NonNull<T>,
    decoder: ManuallyDrop<D>,
}

// SAFETY: `Decoded` doesn't add any restrictions on sending across thread boundaries, and so is
// `Send` if `T` and `D` are `Send`.
unsafe impl<T: Send + ?Sized, D: Send> Send for Decoded<T, D> {}

// SAFETY: `Decoded` doesn't add any interior mutability, so it is `Sync` if `T` and `D` are `Sync`.
unsafe impl<T: Sync + ?Sized, D: Sync> Sync for Decoded<T, D> {}

impl<T: ?Sized, D> Drop for Decoded<T, D> {
    fn drop(&mut self) {
        // SAFETY: `ptr` points to a `T` which is safe to drop as an invariant of `Decoded`. We will
        // only ever drop it once, since `drop` may only be called once.
        unsafe {
            self.ptr.as_ptr().drop_in_place();
        }
        // SAFETY: `decoder` is only ever dropped once, since `drop` may only be called once.
        unsafe {
            ManuallyDrop::drop(&mut self.decoder);
        }
    }
}

impl<T: ?Sized, D> Decoded<T, D> {
    /// Creates an owned value contained within a decoder.
    ///
    /// `Decoded` drops `ptr`, but does not free the backing memory. `decoder` should free the
    /// memory backing `ptr` when dropped.
    ///
    /// # Safety
    ///
    /// - `ptr` must be non-null, properly-aligned, and valid for reads and writes.
    /// - `ptr` must be valid for dropping.
    /// - `ptr` must remain valid until `decoder` is dropped.
    pub unsafe fn new_unchecked(ptr: *mut T, decoder: D) -> Self {
        Self { ptr: unsafe { NonNull::new_unchecked(ptr) }, decoder: ManuallyDrop::new(decoder) }
    }

    /// Returns the raw pointer and decoder used to create this `Decoded`.
    pub fn into_raw_parts(mut self) -> (*mut T, D) {
        let ptr = self.ptr.as_ptr();
        // SAFETY: We forget `self` immediately after taking `self.decoder`, so we won't double-drop
        // the decoder.
        let decoder = unsafe { ManuallyDrop::take(&mut self.decoder) };
        forget(self);
        (ptr, decoder)
    }
}

impl<T: ?Sized, B> Deref for Decoded<T, B> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `ptr` is non-null, properly-aligned, and valid for reads and writes as an
        // invariant of `Decoded`.
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: fmt::Debug + ?Sized, B: fmt::Debug> fmt::Debug for Decoded<T, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}
