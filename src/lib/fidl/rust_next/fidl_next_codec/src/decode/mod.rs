// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides decoding for FIDL types.

mod error;

pub use self::error::*;

use crate::{Slot, WireF32, WireF64, WireI16, WireI32, WireI64, WireU16, WireU32, WireU64};

/// Decodes a value from the given slot.
///
/// # Safety
///
/// If `decode` returns `Ok`, then the provided `slot` must now contain a valid
/// value of the implementing type.
pub unsafe trait Decode<D: ?Sized> {
    /// Decodes a value into a slot using a decoder.
    ///
    /// If decoding succeeds, `slot` will contain a valid value. If decoding fails, an error will be
    /// returned.
    fn decode(slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError>;
}

macro_rules! impl_primitive {
    ($ty:ty) => {
        unsafe impl<D: ?Sized> Decode<D> for $ty {
            #[inline]
            fn decode(_: Slot<'_, Self>, _: &mut D) -> Result<(), DecodeError> {
                Ok(())
            }
        }
    };
}

macro_rules! impl_primitives {
    ($($ty:ty),* $(,)?) => {
        $(
            impl_primitive!($ty);
        )*
    }
}

impl_primitives! {
    (),

    i8,
    WireI16,
    WireI32,
    WireI64,

    u8,
    WireU16,
    WireU32,
    WireU64,

    WireF32,
    WireF64,
}

unsafe impl<D: ?Sized> Decode<D> for bool {
    #[inline]
    fn decode(slot: Slot<'_, Self>, _: &mut D) -> Result<(), DecodeError> {
        let value = unsafe { slot.as_ptr().cast::<u8>().read() };
        match value {
            0 | 1 => (),
            invalid => return Err(DecodeError::InvalidBool(invalid)),
        }
        Ok(())
    }
}

unsafe impl<D: ?Sized, T: Decode<D>, const N: usize> Decode<D> for [T; N] {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut D) -> Result<(), DecodeError> {
        for i in 0..N {
            T::decode(slot.index(i), decoder)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{chunks, WireF32, WireF64, WireI16, WireI32, WireI64, WireU16, WireU32, WireU64};

    use crate::testing::assert_decoded;
    use crate::wire::{WireBox, WireString, WireVector};
    use crate::{WireOptionalString, WireOptionalVector};

    #[test]
    fn decode_bool() {
        assert_decoded::<bool>(&mut chunks![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], |x| {
            assert!(*x)
        });
        assert_decoded::<bool>(&mut chunks![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], |x| {
            assert!(!*x)
        });
    }

    #[test]
    fn decode_ints() {
        assert_decoded::<u8>(&mut chunks![0xa3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], |x| {
            assert_eq!(*x, 0xa3u8)
        });
        assert_decoded::<i8>(&mut chunks![0xbb, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], |x| {
            assert_eq!(*x, -0x45i8)
        });

        assert_decoded::<WireU16>(
            &mut chunks![0x34, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            |x| assert_eq!(*x, 0x1234u16),
        );
        assert_decoded::<WireI16>(
            &mut chunks![0xcc, 0xed, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            |x| assert_eq!(*x, -0x1234i16),
        );

        assert_decoded::<WireU32>(
            &mut chunks![0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00],
            |x| assert_eq!(*x, 0x12345678u32),
        );
        assert_decoded::<WireI32>(
            &mut chunks![0x88, 0xa9, 0xcb, 0xed, 0x00, 0x00, 0x00, 0x00],
            |x| assert_eq!(*x, -0x12345678i32),
        );

        assert_decoded::<WireU64>(
            &mut chunks![0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12],
            |x| assert_eq!(*x, 0x123456789abcdef0u64),
        );
        assert_decoded::<WireI64>(
            &mut chunks![0x10, 0x21, 0x43, 0x65, 0x87, 0xa9, 0xcb, 0xed],
            |x| assert_eq!(*x, -0x123456789abcdef0i64),
        );
    }

    #[test]
    fn decode_floats() {
        assert_decoded::<WireF32>(
            &mut chunks![0xdb, 0x0f, 0x49, 0x40, 0x00, 0x00, 0x00, 0x00],
            |x| assert_eq!(*x, ::core::f32::consts::PI),
        );
        assert_decoded::<WireF64>(
            &mut chunks![0x18, 0x2d, 0x44, 0x54, 0xfb, 0x21, 0x09, 0x40],
            |x| assert_eq!(*x, ::core::f64::consts::PI),
        );
    }

    #[test]
    fn decode_box() {
        assert_decoded::<WireBox<WireU64>>(
            &mut chunks![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            |x| assert_eq!(x.as_ref(), None),
        );
        assert_decoded::<WireBox<WireU64>>(
            &mut chunks![
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56,
                0x34, 0x12,
            ],
            |x| assert_eq!(*x.as_ref().unwrap(), 0x123456789abcdef0u64),
        );
    }

    #[test]
    fn decode_vec() {
        assert_decoded::<WireOptionalVector<WireU32>>(
            &mut chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
            |x| assert!(x.as_ref().is_none()),
        );
        assert_decoded::<WireVector<WireU32>>(
            &mut chunks![
                0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0x78, 0x56, 0x34, 0x12, 0xf0, 0xde, 0xbc, 0x9a,
            ],
            |x| assert_eq!(x.as_slice(), [0x12345678, 0x9abcdef0].as_slice(),),
        );
        assert_decoded::<WireVector<WireU32>>(
            &mut chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff,
            ],
            |x| assert_eq!(x.len(), 0),
        );
    }

    #[test]
    fn decode_string() {
        assert_decoded::<WireOptionalString>(
            &mut chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
            |x| assert!(x.is_none()),
        );
        assert_decoded::<WireString>(
            &mut chunks![
                0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0x30, 0x31, 0x32, 0x33, 0x00, 0x00, 0x00, 0x00,
            ],
            |x| assert_eq!(x.as_str(), "0123"),
        );
        assert_decoded::<WireString>(
            &mut chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff,
            ],
            |x| assert_eq!(x.len(), 0),
        );
    }
}
