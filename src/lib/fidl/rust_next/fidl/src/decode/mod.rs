// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides decoding for FIDL types.

mod decoder;
mod error;
mod owned;

pub use self::decoder::*;
pub use self::error::*;
pub use self::owned::*;
use crate::{f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le, Chunk, Handle, Slot};

/// Decodes a value from the given slot.
///
/// # Safety
///
/// If `decode` returns `Ok`, then the provided `slot` must now contain a valid
/// value of the implementing type.
pub unsafe trait Decode<'buf>: 'buf {
    /// Decodes a value into a slot using a decoder.
    ///
    /// If decoding succeeds, `slot` will contain a valid value. If decoding fails, an error will be
    /// returned.
    fn decode(slot: Slot<'_, Self>, decoder: &mut Decoder<'buf>) -> Result<(), Error>;
}

/// Decodes a value from chunks and handles.
pub fn from_chunks<'buf, T: Decode<'buf>>(
    chunks: &'buf mut [Chunk],
    handles: Vec<Handle>,
) -> Result<Owned<'buf, T>, Error> {
    Decoder::new(chunks, handles).decode::<T>()
}

macro_rules! impl_primitive {
    ($ty:ty) => {
        unsafe impl Decode<'_> for $ty {
            #[inline]
            fn decode(_: Slot<'_, Self>, _: &mut Decoder<'_>) -> Result<(), Error> {
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
    i8,
    i16_le,
    i32_le,
    i64_le,

    u8,
    u16_le,
    u32_le,
    u64_le,

    f32_le,
    f64_le,
}

unsafe impl Decode<'_> for bool {
    #[inline]
    fn decode(slot: Slot<'_, Self>, _: &mut Decoder<'_>) -> Result<(), Error> {
        let value = unsafe { slot.as_ptr().cast::<u8>().read() };
        match value {
            0 | 1 => (),
            invalid => return Err(Error::InvalidBool(invalid)),
        }
        Ok(())
    }
}

unsafe impl<'buf, T: Decode<'buf>, const N: usize> Decode<'buf> for [T; N] {
    fn decode(mut slot: Slot<'_, Self>, decoder: &mut Decoder<'buf>) -> Result<(), Error> {
        for i in 0..N {
            T::decode(slot.index(i), decoder)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le};

    use crate::test_util::assert_decoded;
    use crate::wire::{WireBox, WireHandle, WireString, WireVector};
    use crate::{Handle, WireOptionalString, WireOptionalVector};

    #[test]
    fn decode_bool() {
        assert_decoded::<bool>(
            &mut chunks!(0x01 0x00 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert!(*x),
        );
        assert_decoded::<bool>(
            &mut chunks!(0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert!(!*x),
        );
    }

    #[test]
    fn decode_ints() {
        assert_decoded::<u8>(
            &mut chunks!(0xa3 0x00 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, 0xa3u8),
        );
        assert_decoded::<i8>(
            &mut chunks!(0xbb 0x00 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, -0x45i8),
        );

        assert_decoded::<u16_le>(
            &mut chunks!(0x34 0x12 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, 0x1234u16),
        );
        assert_decoded::<i16_le>(
            &mut chunks!(0xcc 0xed 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, -0x1234i16),
        );

        assert_decoded::<u32_le>(
            &mut chunks!(0x78 0x56 0x34 0x12 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, 0x12345678u32),
        );
        assert_decoded::<i32_le>(
            &mut chunks!(0x88 0xa9 0xcb 0xed 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, -0x12345678i32),
        );

        assert_decoded::<u64_le>(
            &mut chunks!(0xf0 0xde 0xbc 0x9a 0x78 0x56 0x34 0x12),
            Vec::new(),
            |x| assert_eq!(*x, 0x123456789abcdef0u64),
        );
        assert_decoded::<i64_le>(
            &mut chunks!(0x10 0x21 0x43 0x65 0x87 0xa9 0xcb 0xed),
            Vec::new(),
            |x| assert_eq!(*x, -0x123456789abcdef0i64),
        );
    }

    #[test]
    fn decode_floats() {
        assert_decoded::<f32_le>(
            &mut chunks!(0xdb 0x0f 0x49 0x40 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(*x, ::core::f32::consts::PI),
        );
        assert_decoded::<f64_le>(
            &mut chunks!(0x18 0x2d 0x44 0x54 0xfb 0x21 0x09 0x40),
            Vec::new(),
            |x| assert_eq!(*x, ::core::f64::consts::PI),
        );
    }

    #[test]
    fn decode_box() {
        assert_decoded::<WireBox<'_, u64_le>>(
            &mut chunks!(0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(x.as_ref(), None),
        );
        assert_decoded::<WireBox<'_, u64_le>>(
            &mut chunks!(
                0xff 0xff 0xff 0xff 0xff 0xff 0xff 0xff
                0xf0 0xde 0xbc 0x9a 0x78 0x56 0x34 0x12
            ),
            Vec::new(),
            |x| assert_eq!(*x.as_ref().unwrap(), 0x123456789abcdef0u64),
        );
    }

    #[test]
    fn decode_handle() {
        assert_decoded::<WireHandle>(
            &mut chunks!(0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00),
            Vec::new(),
            |x| assert_eq!(x.as_raw_handle(), None),
        );
        assert_decoded::<WireHandle>(
            &mut chunks!(0xff 0xff 0xff 0xff 0x00 0x00 0x00 0x00),
            vec![Handle::from_raw(0x12345678.try_into().unwrap())],
            |x| assert_eq!(x.as_raw_handle(), Some(0x12345678.try_into().unwrap())),
        );
    }

    #[test]
    fn decode_vec() {
        assert_decoded::<WireOptionalVector<'_, u32_le>>(
            &mut chunks!(
                0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00
                0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00
            ),
            Vec::new(),
            |x| assert!(x.as_ref().is_none()),
        );
        assert_decoded::<WireVector<'_, u32_le>>(
            &mut chunks!(
                0x02 0x00 0x00 0x00 0x00 0x00 0x00 0x00
                0xff 0xff 0xff 0xff 0xff 0xff 0xff 0xff
                0x78 0x56 0x34 0x12 0xf0 0xde 0xbc 0x9a
            ),
            Vec::new(),
            |x| {
                assert_eq!(
                    x.as_slice(),
                    [u32_le::from_native(0x12345678), u32_le::from_native(0x9abcdef0),].as_slice(),
                )
            },
        );
        assert_decoded::<WireVector<'_, u32_le>>(
            &mut chunks!(
                0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00
                0xff 0xff 0xff 0xff 0xff 0xff 0xff 0xff
            ),
            Vec::new(),
            |x| assert_eq!(x.len(), 0),
        );
    }

    #[test]
    fn decode_string() {
        assert_decoded::<WireOptionalString<'_>>(
            &mut chunks!(
                0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00
                0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00
            ),
            Vec::new(),
            |x| assert!(x.is_none()),
        );
        assert_decoded::<WireString<'_>>(
            &mut chunks!(
                0x04 0x00 0x00 0x00 0x00 0x00 0x00 0x00
                0xff 0xff 0xff 0xff 0xff 0xff 0xff 0xff
                0x30 0x31 0x32 0x33 0x00 0x00 0x00 0x00
            ),
            Vec::new(),
            |x| assert_eq!(x.as_str(), "0123"),
        );
        assert_decoded::<WireString<'_>>(
            &mut chunks!(
                0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x00
                0xff 0xff 0xff 0xff 0xff 0xff 0xff 0xff
            ),
            Vec::new(),
            |x| assert_eq!(x.len(), 0),
        );
    }
}
