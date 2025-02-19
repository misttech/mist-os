// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ptr::slice_from_raw_parts_mut;

use munge::munge;

use crate::{u64_le, Slot, WirePointer};

#[repr(C)]
pub struct RawWireVector<T> {
    pub len: u64_le,
    pub ptr: WirePointer<T>,
}

// SAFETY: `RawWireVector` doesn't add any restrictions on sending across thread boundaries, and so
// is `Send` if `T` is `Send`.
unsafe impl<T: Send> Send for RawWireVector<T> {}

// SAFETY: `RawWireVector` doesn't add any interior mutability, so it is `Sync` if `T` is `Sync`.
unsafe impl<T: Sync> Sync for RawWireVector<T> {}

impl<T> RawWireVector<T> {
    pub fn encode_present(slot: Slot<'_, Self>, len: u64) {
        munge!(let Self { len: mut encoded_len, ptr } = slot);
        *encoded_len = u64_le::from_native(len);
        WirePointer::encode_present(ptr);
    }

    pub fn encode_absent(slot: Slot<'_, Self>) {
        munge!(let Self { mut len, ptr } = slot);
        *len = u64_le::from_native(0);
        WirePointer::encode_absent(ptr);
    }

    pub fn len(&self) -> u64 {
        self.len.to_native()
    }

    pub fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    pub fn as_slice_ptr(&self) -> *mut [T] {
        slice_from_raw_parts_mut(self.as_ptr(), self.len() as usize)
    }
}
