// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::ops::{Deref, DerefMut};

// This is a wrapper around externally allocated pointers. In order to allow
// these to be passed to a separate thread but still maintain some more well-
// defined lifetime bounds, we use this wrapper type.
pub struct ExtBuffer<T> {
    inner: *mut T,
    size: usize,
}

impl<T> ExtBuffer<T> {
    pub fn new(ptr: *mut T, size: usize) -> Self {
        Self { inner: ptr, size }
    }
}

impl<T: Send + Sized> Deref for ExtBuffer<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.inner, self.size) }
    }
}

impl<T: Send + Sized> DerefMut for ExtBuffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.inner, self.size) }
    }
}

unsafe impl<T: Send> Send for ExtBuffer<T> {}
