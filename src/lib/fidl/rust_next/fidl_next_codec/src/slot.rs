// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::ptr::slice_from_raw_parts_mut;
use core::slice::from_raw_parts;

use munge::{Destructure, Move, Restructure};
use zerocopy::{FromBytes, IntoBytes};

/// An initialized but potentially invalid value.
///
/// The bytes of a `Slot` are always valid to read, but may not represent a
/// valid value of its type. For example, a `Slot<'_, bool>` may not be set to
/// 0 or 1.
#[repr(transparent)]
pub struct Slot<'buf, T: ?Sized> {
    ptr: *mut T,
    _phantom: PhantomData<&'buf mut [u8]>,
}

impl<'buf, T: ?Sized> Slot<'buf, T> {
    /// Returns a new `Slot` backed by the given `MaybeUninit`.
    pub fn new(backing: &'buf mut MaybeUninit<T>) -> Self
    where
        T: Sized,
    {
        unsafe {
            backing.as_mut_ptr().write_bytes(0, 1);
        }
        unsafe { Self::new_unchecked(backing.as_mut_ptr()) }
    }

    /// Creates a new slot from the given pointer.
    ///
    /// # Safety
    ///
    /// `ptr` must point to enough initialized bytes with the correct alignment
    /// to represent a `T`.
    pub unsafe fn new_unchecked(ptr: *mut T) -> Self {
        Self { ptr, _phantom: PhantomData }
    }

    /// Mutably reborrows the slot.
    pub fn as_mut(&mut self) -> Slot<'_, T> {
        Self { ptr: self.ptr, _phantom: PhantomData }
    }

    /// Returns a mutable pointer to the underlying potentially-invalid value.
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.ptr
    }

    /// Returns a pointer to the underlying potentially-invalid value.
    pub fn as_ptr(&self) -> *const T {
        self.ptr
    }

    /// Returns a reference to the contained value.
    ///
    /// # Safety
    ///
    /// The slot must contain a valid `T`.
    pub unsafe fn deref_unchecked(&self) -> &T {
        unsafe { &*self.as_ptr() }
    }

    /// Returns a mutable reference to the contained value.
    ///
    /// # Safety
    ///
    /// The slot must contain a valid `T`.
    pub unsafe fn deref_mut_unchecked(&mut self) -> &mut T {
        unsafe { &mut *self.as_mut_ptr() }
    }

    /// Writes the given value into the slot.
    pub fn write(&mut self, value: T)
    where
        T: IntoBytes + Sized,
    {
        unsafe {
            self.as_mut_ptr().write(value);
        }
    }
}

impl<T> Slot<'_, T> {
    /// Returns a slice of the underlying bytes.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { from_raw_parts(self.ptr.cast::<u8>(), size_of::<T>()) }
    }
}

impl<T, const N: usize> Slot<'_, [T; N]> {
    /// Returns a slot of the element at the given index.
    pub fn index(&mut self, index: usize) -> Slot<'_, T> {
        assert!(index < N, "attempted to index out-of-bounds");

        Slot { ptr: unsafe { self.as_mut_ptr().cast::<T>().add(index) }, _phantom: PhantomData }
    }
}

impl<T> Slot<'_, [T]> {
    /// Creates a new slice slot from the given pointer.
    ///
    /// # Safety
    ///
    /// `ptr` must point to enough initialized bytes with the correct alignment
    /// to represent a slice of `len` `T`s.
    pub unsafe fn new_slice_unchecked(ptr: *mut T, len: usize) -> Self {
        Self { ptr: slice_from_raw_parts_mut(ptr, len), _phantom: PhantomData }
    }

    /// Returns a slot of the element at the given index.
    pub fn index(&mut self, index: usize) -> Slot<'_, T> {
        assert!(index < self.ptr.len(), "attempted to index out-of-bounds");

        Slot { ptr: unsafe { self.as_mut_ptr().cast::<T>().add(index) }, _phantom: PhantomData }
    }
}

impl<T: FromBytes> Deref for Slot<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.as_ptr() }
    }
}

impl<T: FromBytes> DerefMut for Slot<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.as_mut_ptr() }
    }
}

impl<'buf, T> Iterator for Slot<'buf, [T]> {
    type Item = Slot<'buf, T>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ptr.len() == 0 {
            return None;
        }

        let result = Slot { ptr: self.ptr.cast::<T>(), _phantom: PhantomData };

        self.ptr =
            slice_from_raw_parts_mut(unsafe { self.ptr.cast::<T>().add(1) }, self.ptr.len() - 1);

        Some(result)
    }
}

unsafe impl<T> Destructure for Slot<'_, T> {
    type Underlying = T;
    type Destructuring = Move;

    fn underlying(&mut self) -> *mut Self::Underlying {
        self.as_mut_ptr()
    }
}

unsafe impl<'buf, T, U: 'buf> Restructure<U> for Slot<'buf, T> {
    type Restructured = Slot<'buf, U>;

    unsafe fn restructure(&self, ptr: *mut U) -> Self::Restructured {
        Slot { ptr, _phantom: PhantomData }
    }
}
