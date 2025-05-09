// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::mem::{forget, MaybeUninit};
use core::ptr::NonNull;

use munge::munge;

use crate::{
    Decode, DecodeError, Decoder, DecoderExt as _, Encodable, EncodableOption, Encode, EncodeError,
    EncodeOption, EncodeOptionRef, EncodeRef, FromWire, FromWireOption, Slot, Wire, WirePointer,
};

/// A boxed (optional) FIDL value.
#[repr(C)]
pub struct WireBox<'de, T> {
    ptr: WirePointer<'de, T>,
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

unsafe impl<T: Wire> Wire for WireBox<'static, T> {
    type Decoded<'de> = WireBox<'de, T::Decoded<'de>>;

    #[inline]
    fn zero_padding(_: &mut MaybeUninit<Self>) {
        // Wire boxes have no padding
    }
}

impl<T> WireBox<'_, T> {
    /// Encodes that a value is present in an output.
    pub fn encode_present(out: &mut MaybeUninit<Self>) {
        munge!(let Self { ptr } = out);
        WirePointer::encode_present(ptr);
    }

    /// Encodes that a value is absent in a slot.
    pub fn encode_absent(out: &mut MaybeUninit<Self>) {
        munge!(let Self { ptr } = out);
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

    /// Returns an `Owned` of the boxed value, if any.
    pub fn into_option(self) -> Option<T> {
        let ptr = self.ptr.as_ptr();
        forget(self);
        if ptr.is_null() {
            None
        } else {
            unsafe { Some(ptr.read()) }
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for WireBox<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<D: Decoder + ?Sized, T: Decode<D>> Decode<D> for WireBox<'static, T> {
    fn decode(slot: Slot<'_, Self>, mut decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { mut ptr } = slot);

        if WirePointer::is_encoded_present(ptr.as_mut())? {
            let mut value = decoder.take_slot::<T>()?;
            T::decode(value.as_mut(), decoder)?;
            WirePointer::set_decoded(ptr, value.as_mut_ptr());
        }

        Ok(())
    }
}

impl<T: EncodableOption> Encodable for Option<T> {
    type Encoded = T::EncodedOption;
}

unsafe impl<E: ?Sized, T: EncodeOption<E>> Encode<E> for Option<T> {
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        T::encode_option(self, encoder, out)
    }
}

unsafe impl<E: ?Sized, T: EncodeOptionRef<E>> EncodeRef<E> for Option<T> {
    fn encode_ref(
        &self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        T::encode_option_ref(self.as_ref(), encoder, out)
    }
}

impl<T: FromWire<W>, W> FromWireOption<WireBox<'_, W>> for T {
    fn from_wire_option(wire: WireBox<'_, W>) -> Option<Self> {
        wire.into_option().map(T::from_wire)
    }
}
