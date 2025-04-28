// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;
use core::hint::unreachable_unchecked;
use core::mem::MaybeUninit;

use fidl_next_codec::{
    munge, Decode, DecodeError, Encodable, Encode, EncodeError, EncodeRef, Slot, TakeFrom, WireI32,
    ZeroPadding,
};

/// An internal framework error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum FrameworkError {
    /// The protocol method was not recognized by the receiver.
    UnknownMethod = -2,
}

/// An internal framework error.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct WireFrameworkError {
    inner: WireI32,
}

unsafe impl ZeroPadding for WireFrameworkError {
    #[inline]
    fn zero_padding(_: &mut MaybeUninit<Self>) {}
}

impl fmt::Debug for WireFrameworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        FrameworkError::from(*self).fmt(f)
    }
}

impl From<WireFrameworkError> for FrameworkError {
    fn from(value: WireFrameworkError) -> Self {
        match *value.inner {
            -2 => Self::UnknownMethod,
            _ => unsafe { unreachable_unchecked() },
        }
    }
}

unsafe impl<D: ?Sized> Decode<D> for WireFrameworkError {
    fn decode(slot: Slot<'_, Self>, _: &mut D) -> Result<(), DecodeError> {
        munge!(let Self { inner } = slot);
        match **inner {
            -2 => Ok(()),
            code => Err(DecodeError::InvalidFrameworkError(code)),
        }
    }
}

impl Encodable for FrameworkError {
    type Encoded = WireFrameworkError;
}

unsafe impl<E: ?Sized> Encode<E> for FrameworkError {
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        self.encode_ref(encoder, out)
    }
}

unsafe impl<E: ?Sized> EncodeRef<E> for FrameworkError {
    fn encode_ref(
        &self,
        _: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        munge!(let WireFrameworkError { inner } = out);
        inner.write(WireI32(match self {
            Self::UnknownMethod => -2,
        }));

        Ok(())
    }
}

impl TakeFrom<WireFrameworkError> for FrameworkError {
    fn take_from(from: &WireFrameworkError) -> Self {
        Self::from(*from)
    }
}
