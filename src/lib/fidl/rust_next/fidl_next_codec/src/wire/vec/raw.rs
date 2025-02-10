// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ptr::slice_from_raw_parts_mut;

use munge::munge;

use crate::{u64_le, Decode, DecodeError, Decoder, DecoderExt, Owned, Slot, WirePointer};

#[repr(C)]
pub struct RawWireVector<T> {
    len: u64_le,
    ptr: WirePointer<T>,
}

// SAFETY: `RawWireVector` doesn't add any restrictions on sending across thread boundaries, and so
// is `Send` if `T` is `Send`.
unsafe impl<T: Send> Send for RawWireVector<T> {}

// SAFETY: `RawWireVector` doesn't add any interior mutability, so it is `Sync` if `T` is `Sync`.
unsafe impl<T: Sync> Sync for RawWireVector<T> {}

impl<T> Drop for RawWireVector<T> {
    fn drop(&mut self) {
        if !self.ptr.as_ptr().is_null() {
            unsafe {
                self.as_slice_ptr().drop_in_place();
            }
        }
    }
}

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
        slice_from_raw_parts_mut(self.as_ptr(), self.len().try_into().unwrap())
    }

    /// # Safety
    ///
    /// The elements of the wire vector must not need to be individually decoded, and must always be
    /// valid.
    pub unsafe fn decode_raw<D>(
        slot: Slot<'_, Self>,
        mut decoder: &mut D,
    ) -> Result<(), DecodeError>
    where
        D: Decoder + ?Sized,
        T: Decode<D>,
    {
        munge!(let Self { len, mut ptr } = slot);

        let len = len.to_native();
        if WirePointer::is_encoded_present(ptr.as_mut())? {
            let mut slice = decoder.take_slice_slot::<u8>(len as usize)?;
            WirePointer::set_decoded(ptr, slice.as_mut_ptr().cast());
        }

        Ok(())
    }
}

unsafe impl<D: Decoder + ?Sized, T: Decode<D>> Decode<D> for RawWireVector<T> {
    fn decode(slot: Slot<'_, Self>, mut decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { len, mut ptr } = slot);

        let len = len.to_native();
        if WirePointer::is_encoded_present(ptr.as_mut())? {
            let slice = decoder.decode_next_slice::<T>(len as usize)?;
            let slice = unsafe { Owned::new_unchecked(slice.into_raw().cast::<T>()) };
            WirePointer::set_decoded(ptr, slice.into_raw());
        }

        Ok(())
    }
}
