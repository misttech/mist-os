// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ffi::c_void;

/// Type that can be unsafely constructed from a pointer and implements `Send`.
///
/// `SendPtr` only implement unsafe constructors, and each constructor requires the caller to
/// make a particular promise about the pointer being used to construct the `SendPtr`.
///
/// # SendPtr and Pointee Lifetime are Unrelated
///
/// Wrapping a pointer in a `SendPtr` sent the pointer to another thread will not suddenly ensure
/// the pointee will live as long as the `SendPtr`. The same lifetime precautions that apply to raw
/// pointers apply to `SendPtr` too. When a `SendPtr` is eventually unwrapped, the raw pointer must
/// either be checked for validity or asserted to be valid by some invariant. The validity of the
/// raw pointer will most commonly be asserted because of some invariant promised to be upheld by
/// the function or type that produced it.
#[derive(Debug, Eq, PartialEq)]
pub struct SendPtr<T>(T);

// Safety: Any `SendPtr<T>` is safe to send to another thread because to construct one, the
// caller makes a promise of some sort that makes the pointer contained in the `SendPtr<T>` is
// safe to send to another thread.
unsafe impl<T> Send for SendPtr<T> {}

impl SendPtr<*mut c_void> {
    /// Constructs a `SendPtr<*mut c_void>.
    ///
    /// Supposing the wrapped pointer points to a `c_void` its entire lifetime, Rust cannot read or
    /// mutate a `c_void` directly. Therefore, it's not possible to write safe code that mutates the
    /// pointee.
    ///
    /// # Safety
    ///
    /// The caller of this function **must not** assume wrapping `ptr` in a `SendPtr` makes
    /// `ptr` always safe to pass to other functions. `SendPtr` does not ensure the pointee of `ptr`
    /// lives longer than the `SendPtr` constructed here. `SendPtr` merely wraps `ptr` to implement
    /// `Send` after the caller promises to uphold an invariant about the pointer itself.
    ///
    /// This function is unsafe because its liable to be abused. The caller should not use this
    /// function to send a pointer to another thread when it would otherwise be unsafe to do so.
    ///
    /// By calling this function, the caller promises the pointee of `ptr` will always be a `c_void`
    /// and thus unable to be read or mutated by safe Rust code. The caller must still ensure the
    /// pointee lives longer than the wrapped `ptr`.
    pub unsafe fn from_always_void(ptr: *mut c_void) -> Self {
        Self(ptr)
    }
}

impl<T> SendPtr<T> {
    /// Acquires the underlying pointer.
    pub fn as_ptr(self) -> T {
        self.0
    }
}
