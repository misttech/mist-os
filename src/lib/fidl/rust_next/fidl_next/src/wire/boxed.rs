// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::ptr::NonNull;

use munge::munge;

use crate::{
    decode, encode, Decode, Decoder, DecoderExt as _, Encodable, Encode, Slot, TakeFrom,
    WirePointer,
};

/// A boxed (optional) FIDL value.
#[repr(C)]
pub struct WireBox<'buf, T> {
    ptr: WirePointer<'buf, T>,
}

// SAFETY: `WireBox` doesn't add any restrictions on sending across thread boundaries, and so is
// `Send` if `T` is `Send`.
unsafe impl<T: Send> Send for WireBox<'_, T> {}

// SAFETY: `WireBox` doesn't add any interior mutability, so it is `Sync` if `T` is `Sync`.
unsafe impl<T: Sync> Sync for WireBox<'_, T> {}

impl<T> Drop for WireBox<'_, T> {
    fn drop(&mut self) {
        if self.is_some() {
            unsafe {
                self.ptr.as_ptr().drop_in_place();
            }
        }
    }
}

impl<T> WireBox<'_, T> {
    /// Encodes that a value is present in a slot.
    pub fn encode_present(slot: Slot<'_, Self>) {
        munge!(let Self { ptr } = slot);
        WirePointer::encode_present(ptr);
    }

    /// Encodes that a value is absent in a slot.
    pub fn encode_absent(slot: Slot<'_, Self>) {
        munge!(let Self { ptr } = slot);
        WirePointer::encode_absent(ptr);
    }

    /// Returns whether the value is present.
    pub fn is_some(&self) -> bool {
        !self.ptr.as_ptr().is_null()
    }

    /// Returns whether the value is absent.
    pub fn is_none(&self) -> bool {
        !self.is_some()
    }

    /// Returns a reference to the boxed value, if any.
    pub fn as_ref(&self) -> Option<&T> {
        NonNull::new(self.ptr.as_ptr()).map(|ptr| unsafe { ptr.as_ref() })
    }

    /// Returns a mutable reference to the boxed value, if any.
    pub fn as_mut(&mut self) -> Option<&mut T> {
        NonNull::new(self.ptr.as_ptr()).map(|mut ptr| unsafe { ptr.as_mut() })
    }
}

impl<T> Default for WireBox<'_, T> {
    fn default() -> Self {
        Self { ptr: WirePointer::null() }
    }
}

impl<T: fmt::Debug> fmt::Debug for WireBox<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<'buf, D: Decoder<'buf> + ?Sized, T: Decode<D>> Decode<D> for WireBox<'buf, T> {
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), decode::DecodeError> {
        munge!(let Self { mut ptr } = slot);

        if WirePointer::is_encoded_present(ptr.as_mut())? {
            let value = decoder.decode_next::<T>()?;
            WirePointer::set_decoded(ptr, value);
        }

        Ok(())
    }
}

impl<T: encode::EncodableOption> Encodable for Option<T> {
    type Encoded<'buf> = T::EncodedOption<'buf>;
}

impl<E: ?Sized, T: encode::EncodeOption<E>> Encode<E> for Option<T> {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), encode::EncodeError> {
        T::encode_option(self.as_mut(), encoder, slot)
    }
}

impl<T: TakeFrom<WT>, WT> TakeFrom<WireBox<'_, WT>> for Option<T> {
    fn take_from(from: &mut WireBox<'_, WT>) -> Self {
        from.as_mut().map(|value| T::take_from(value))
    }
}
