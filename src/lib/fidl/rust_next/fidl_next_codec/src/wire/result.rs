// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::marker::PhantomData;

use crate::{
    munge, Decode, DecodeError, Decoder, Encodable, Encode, EncodeError, Encoder, RawWireUnion,
    Slot, TakeFrom, ZeroPadding,
};

/// A FIDL result union.
#[repr(transparent)]
pub struct WireResult<T, E> {
    raw: RawWireUnion,
    _phantom: PhantomData<(T, E)>,
}

unsafe impl<T, E> ZeroPadding for WireResult<T, E> {
    #[inline]
    unsafe fn zero_padding(ptr: *mut Self) {
        unsafe {
            RawWireUnion::zero_padding(ptr.cast());
        }
    }
}

const ORD_OK: u64 = 1;
const ORD_ERR: u64 = 2;

impl<T, E> WireResult<T, E> {
    /// Returns whether the result is `Ok`.
    pub fn is_ok(&self) -> bool {
        self.raw.ordinal() == ORD_OK
    }

    /// Returns whether the result is `Err`.
    pub fn is_err(&self) -> bool {
        self.raw.ordinal() == ORD_ERR
    }

    /// Returns the `Ok` value of the result, if any.
    pub fn ok(&self) -> Option<&T> {
        self.is_ok().then(|| unsafe { self.raw.get().deref_unchecked() })
    }

    /// Returns the `Err` value of the result, if any.
    pub fn err(&self) -> Option<&E> {
        self.is_err().then(|| unsafe { self.raw.get().deref_unchecked() })
    }

    /// Returns the contained `Ok` value.
    ///
    /// Panics if the result was not `Ok`.
    pub fn unwrap(&self) -> &T {
        self.ok().unwrap()
    }

    /// Returns the contained `Err` value.
    ///
    /// Panics if the result was not `Err`.
    pub fn unwrap_err(&self) -> &E {
        self.err().unwrap()
    }

    /// Returns a `Result` of a reference to the value or error.
    pub fn as_ref(&self) -> Result<&T, &E> {
        match self.raw.ordinal() {
            ORD_OK => unsafe { Ok(self.raw.get().deref_unchecked()) },
            ORD_ERR => unsafe { Err(self.raw.get().deref_unchecked()) },
            _ => unsafe { ::core::hint::unreachable_unchecked() },
        }
    }
}

impl<T, E> fmt::Debug for WireResult<T, E>
where
    T: fmt::Debug,
    E: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<D, T, E> Decode<D> for WireResult<T, E>
where
    D: Decoder + ?Sized,
    T: Decode<D>,
    E: Decode<D>,
{
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { mut raw, _phantom: _ } = slot);

        match RawWireUnion::encoded_ordinal(raw.as_mut()) {
            ORD_OK => RawWireUnion::decode_as::<D, T>(raw, decoder)?,
            ORD_ERR => RawWireUnion::decode_as::<D, E>(raw, decoder)?,
            ord => return Err(DecodeError::InvalidUnionOrdinal(ord as usize)),
        }

        Ok(())
    }
}

impl<T, E> Encodable for Result<T, E>
where
    T: Encodable,
    E: Encodable,
{
    type Encoded = WireResult<T::Encoded, E::Encoded>;
}

impl<Enc, T, E> Encode<Enc> for Result<T, E>
where
    Enc: Encoder + ?Sized,
    T: Encode<Enc>,
    E: Encode<Enc>,
{
    fn encode(
        &mut self,
        encoder: &mut Enc,
        slot: Slot<'_, Self::Encoded>,
    ) -> Result<(), EncodeError> {
        munge!(let WireResult { raw, _phantom: _ } = slot);

        match self {
            Ok(value) => RawWireUnion::encode_as::<Enc, T>(value, ORD_OK, encoder, raw)?,
            Err(error) => RawWireUnion::encode_as::<Enc, E>(error, ORD_ERR, encoder, raw)?,
        }

        Ok(())
    }
}

impl<T, E, WT, WE> TakeFrom<WireResult<WT, WE>> for Result<T, E>
where
    T: TakeFrom<WT>,
    E: TakeFrom<WE>,
{
    fn take_from(from: &WireResult<WT, WE>) -> Self {
        match from.as_ref() {
            Ok(value) => Ok(T::take_from(value)),
            Err(error) => Err(E::take_from(error)),
        }
    }
}
