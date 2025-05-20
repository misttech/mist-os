// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;
use core::mem::{forget, needs_drop, MaybeUninit};
use core::ops::Deref;
use core::ptr::{copy_nonoverlapping, NonNull};
use core::{fmt, slice};

use munge::munge;

use super::raw::RawWireVector;
use crate::{
    Chunk, Decode, DecodeError, Decoder, DecoderExt as _, Encodable, Encode, EncodeError,
    EncodeRef, Encoder, EncoderExt as _, FromWire, FromWireRef, Slot, Wire, WirePointer,
};

/// A FIDL vector
#[repr(transparent)]
pub struct WireVector<'de, T> {
    pub(crate) raw: RawWireVector<'de, T>,
}

unsafe impl<T: Wire> Wire for WireVector<'static, T> {
    type Decoded<'de> = WireVector<'de, T::Decoded<'de>>;

    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        munge!(let Self { raw } = out);
        RawWireVector::<T>::zero_padding(raw);
    }
}

impl<T> Drop for WireVector<'_, T> {
    fn drop(&mut self) {
        if needs_drop::<T>() {
            unsafe {
                self.raw.as_slice_ptr().drop_in_place();
            }
        }
    }
}

impl<T> WireVector<'_, T> {
    /// Encodes that a vector is present in a slot.
    pub fn encode_present(out: &mut MaybeUninit<Self>, len: u64) {
        munge!(let Self { raw } = out);
        RawWireVector::encode_present(raw, len);
    }

    /// Returns the length of the vector in elements.
    pub fn len(&self) -> usize {
        self.raw.len() as usize
    }

    /// Returns whether the vector is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns a pointer to the elements of the vector.
    fn as_slice_ptr(&self) -> NonNull<[T]> {
        unsafe { NonNull::new_unchecked(self.raw.as_slice_ptr()) }
    }

    /// Returns a slice of the elements of the vector.
    pub fn as_slice(&self) -> &[T] {
        unsafe { self.as_slice_ptr().as_ref() }
    }

    /// Decodes a wire vector which contains raw data.
    ///
    /// # Safety
    ///
    /// The elements of the wire vector must not need to be individually decoded, and must always be
    /// valid.
    pub unsafe fn decode_raw<D>(
        mut slot: Slot<'_, Self>,
        mut decoder: &mut D,
    ) -> Result<(), DecodeError>
    where
        D: Decoder + ?Sized,
        T: Decode<D>,
    {
        munge!(let Self { raw: RawWireVector { len, mut ptr } } = slot.as_mut());

        if !WirePointer::is_encoded_present(ptr.as_mut())? {
            return Err(DecodeError::RequiredValueAbsent);
        }

        let mut slice = decoder.take_slice_slot::<T>(**len as usize)?;
        WirePointer::set_decoded(ptr, slice.as_mut_ptr().cast());

        Ok(())
    }
}

/// An iterator over the items of a `WireVector`.
pub struct IntoIter<'de, T> {
    current: *mut T,
    remaining: usize,
    _phantom: PhantomData<&'de mut [Chunk]>,
}

impl<T> Drop for IntoIter<'_, T> {
    fn drop(&mut self) {
        for i in 0..self.remaining {
            unsafe {
                self.current.add(i).drop_in_place();
            }
        }
    }
}

impl<T> Iterator for IntoIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            None
        } else {
            let result = unsafe { self.current.read() };
            self.current = unsafe { self.current.add(1) };
            self.remaining -= 1;
            Some(result)
        }
    }
}

impl<'de, T> IntoIterator for WireVector<'de, T> {
    type IntoIter = IntoIter<'de, T>;
    type Item = T;

    fn into_iter(self) -> Self::IntoIter {
        let current = self.raw.as_ptr();
        let remaining = self.len();
        forget(self);

        IntoIter { current, remaining, _phantom: PhantomData }
    }
}

impl<T> Deref for WireVector<'_, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: fmt::Debug> fmt::Debug for WireVector<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

unsafe impl<D: Decoder + ?Sized, T: Decode<D>> Decode<D> for WireVector<'static, T> {
    fn decode(mut slot: Slot<'_, Self>, mut decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { raw: RawWireVector { len, mut ptr } } = slot.as_mut());

        if !WirePointer::is_encoded_present(ptr.as_mut())? {
            return Err(DecodeError::RequiredValueAbsent);
        }

        let mut slice = decoder.take_slice_slot::<T>(**len as usize)?;
        for i in 0..**len as usize {
            T::decode(slice.index(i), decoder)?;
        }
        WirePointer::set_decoded(ptr, slice.as_mut_ptr().cast());

        Ok(())
    }
}

#[inline]
fn encode_to_vector<V, E, T>(
    value: V,
    encoder: &mut E,
    out: &mut MaybeUninit<WireVector<'_, T::Encoded>>,
) -> Result<(), EncodeError>
where
    V: AsRef<[T]> + IntoIterator,
    V::IntoIter: ExactSizeIterator,
    V::Item: Encode<E, Encoded = T::Encoded>,
    E: Encoder + ?Sized,
    T: Encode<E>,
{
    let len = value.as_ref().len();
    if T::COPY_OPTIMIZATION.is_enabled() {
        let slice = value.as_ref();
        // SAFETY: `T` has copy optimization enabled, which guarantees that it has no uninit bytes
        // and can be copied directly to the output instead of calling `encode`. This means that we
        // may cast `&[T]` to `&[u8]` and write those bytes.
        let bytes = unsafe { slice::from_raw_parts(slice.as_ptr().cast(), size_of_val(slice)) };
        encoder.write(bytes);
    } else {
        encoder.encode_next_iter(value.into_iter())?;
    }
    WireVector::encode_present(out, len as u64);
    Ok(())
}

impl<T: Encodable> Encodable for Vec<T> {
    type Encoded = WireVector<'static, T::Encoded>;
}

unsafe impl<E, T> Encode<E> for Vec<T>
where
    E: Encoder + ?Sized,
    T: Encode<E>,
{
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encode_to_vector(self, encoder, out)
    }
}

unsafe impl<E, T> EncodeRef<E> for Vec<T>
where
    E: Encoder + ?Sized,
    T: EncodeRef<E>,
{
    fn encode_ref(
        &self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encode_to_vector(self, encoder, out)
    }
}

impl<T: Encodable> Encodable for &[T] {
    type Encoded = WireVector<'static, T::Encoded>;
}

unsafe impl<E, T> Encode<E> for &[T]
where
    E: Encoder + ?Sized,
    T: EncodeRef<E>,
{
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encode_to_vector(self, encoder, out)
    }
}

impl<T: FromWire<W>, W> FromWire<WireVector<'_, W>> for Vec<T> {
    fn from_wire(wire: WireVector<'_, W>) -> Self {
        let mut result = Vec::<T>::with_capacity(wire.len());
        if T::COPY_OPTIMIZATION.is_enabled() {
            unsafe {
                copy_nonoverlapping(wire.as_ptr().cast(), result.as_mut_ptr(), wire.len());
            }
            unsafe {
                result.set_len(wire.len());
            }
            forget(wire);
        } else {
            for item in wire.into_iter() {
                result.push(T::from_wire(item));
            }
        }
        result
    }
}

impl<T: FromWireRef<W>, W> FromWireRef<WireVector<'_, W>> for Vec<T> {
    fn from_wire_ref(wire: &WireVector<'_, W>) -> Self {
        let mut result = Vec::<T>::with_capacity(wire.len());
        if T::COPY_OPTIMIZATION.is_enabled() {
            unsafe {
                copy_nonoverlapping(wire.as_ptr().cast(), result.as_mut_ptr(), wire.len());
            }
            unsafe {
                result.set_len(wire.len());
            }
        } else {
            for item in wire.iter() {
                result.push(T::from_wire_ref(item));
            }
        }
        result
    }
}
