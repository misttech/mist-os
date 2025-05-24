// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::marker::PhantomData;
use core::mem::{ManuallyDrop, MaybeUninit};

use crate::{
    munge, Chunk, Decode, DecodeError, Decoder, Encodable, Encode, EncodeError, EncodeRef, Encoder,
    FromWire, FromWireRef, RawWireUnion, Slot, Wire,
};

/// A FIDL result union.
#[repr(transparent)]
pub struct WireResult<'de, T, E> {
    raw: RawWireUnion,
    _phantom: PhantomData<(&'de mut [Chunk], T, E)>,
}

unsafe impl<T: Wire, E: Wire> Wire for WireResult<'static, T, E> {
    type Decoded<'de> = WireResult<'de, T::Decoded<'de>, E::Decoded<'de>>;

    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        munge!(let Self { raw, _phantom: _ } = out);
        RawWireUnion::zero_padding(raw);
    }
}

const ORD_OK: u64 = 1;
const ORD_ERR: u64 = 2;

impl<T, E> WireResult<'_, T, E> {
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

    /// Returns a `Result` of the owned value or error.
    pub fn into_result(self) -> Result<T, E> {
        let raw = ManuallyDrop::new(self.raw);
        match raw.ordinal() {
            ORD_OK => unsafe { Ok(raw.get().read_unchecked()) },
            ORD_ERR => unsafe { Err(raw.get().read_unchecked()) },
            _ => unsafe { ::core::hint::unreachable_unchecked() },
        }
    }
}

impl<T, E> fmt::Debug for WireResult<'_, T, E>
where
    T: fmt::Debug,
    E: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

unsafe impl<D, T, E> Decode<D> for WireResult<'static, T, E>
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
    type Encoded = WireResult<'static, T::Encoded, E::Encoded>;
}

unsafe impl<Enc, T, E> Encode<Enc> for Result<T, E>
where
    Enc: Encoder + ?Sized,
    T: Encode<Enc>,
    E: Encode<Enc>,
{
    fn encode(
        self,
        encoder: &mut Enc,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        munge!(let WireResult { raw, _phantom: _ } = out);

        match self {
            Ok(value) => RawWireUnion::encode_as::<Enc, T>(value, ORD_OK, encoder, raw)?,
            Err(error) => RawWireUnion::encode_as::<Enc, E>(error, ORD_ERR, encoder, raw)?,
        }

        Ok(())
    }
}

unsafe impl<Enc, T, E> EncodeRef<Enc> for Result<T, E>
where
    Enc: Encoder + ?Sized,
    T: EncodeRef<Enc>,
    E: EncodeRef<Enc>,
{
    fn encode_ref(
        &self,
        encoder: &mut Enc,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        self.as_ref().encode(encoder, out)
    }
}

impl<T, E, WT, WE> FromWire<WireResult<'_, WT, WE>> for Result<T, E>
where
    T: FromWire<WT>,
    E: FromWire<WE>,
{
    #[inline]
    fn from_wire(wire: WireResult<'_, WT, WE>) -> Self {
        match wire.into_result() {
            Ok(value) => Ok(T::from_wire(value)),
            Err(error) => Err(E::from_wire(error)),
        }
    }
}

impl<T, E, WT, WE> FromWireRef<WireResult<'_, WT, WE>> for Result<T, E>
where
    T: FromWireRef<WT>,
    E: FromWireRef<WE>,
{
    #[inline]
    fn from_wire_ref(wire: &WireResult<'_, WT, WE>) -> Self {
        match wire.as_ref() {
            Ok(value) => Ok(T::from_wire_ref(value)),
            Err(error) => Err(E::from_wire_ref(error)),
        }
    }
}
