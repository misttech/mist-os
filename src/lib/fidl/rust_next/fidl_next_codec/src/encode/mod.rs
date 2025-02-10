// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides encoding for FIDL types.

mod error;

pub use self::error::*;

use crate::{
    f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le, Encoder, EncoderExt as _, Slot,
    WireBox,
};

/// A type which can be encoded as FIDL.
pub trait Encodable {
    /// The wire type for the value.
    type Encoded<'buf>;
}

/// Encodes a value.
pub trait Encode<E: ?Sized>: Encodable {
    /// Encodes this value into an encoder and slot.
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError>;
}

/// A type which can be encoded as FIDL when optional.
pub trait EncodableOption {
    /// The wire type for the optional value.
    type EncodedOption<'buf>;
}

/// Encodes an optional value.
pub trait EncodeOption<E: ?Sized>: EncodableOption {
    /// Encodes this optional value into an encoder and slot.
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        slot: Slot<'_, Self::EncodedOption<'_>>,
    ) -> Result<(), EncodeError>;
}

macro_rules! impl_primitive {
    ($ty:ty, $enc:ty) => {
        impl Encodable for $ty {
            type Encoded<'buf> = $enc;
        }

        impl<E: ?Sized> Encode<E> for $ty {
            #[inline]
            fn encode(
                &mut self,
                _: &mut E,
                mut slot: Slot<'_, Self::Encoded<'_>>,
            ) -> Result<(), EncodeError> {
                slot.write(<$enc>::from(*self));
                Ok(())
            }
        }

        impl EncodableOption for $ty {
            type EncodedOption<'buf> = WireBox<$enc>;
        }

        impl<E: Encoder + ?Sized> EncodeOption<E> for $ty {
            #[inline]
            fn encode_option(
                this: Option<&mut Self>,
                encoder: &mut E,
                slot: Slot<'_, Self::EncodedOption<'_>>,
            ) -> Result<(), EncodeError> {
                if let Some(value) = this {
                    encoder.encode_next(value)?;
                    WireBox::encode_present(slot);
                } else {
                    WireBox::encode_absent(slot);
                }

                Ok(())
            }
        }
    };
}

macro_rules! impl_primitives {
    ($($ty:ty, $enc:ty);* $(;)?) => {
        $(
            impl_primitive!($ty, $enc);
        )*
    }
}

impl_primitives! {
    (), (); bool, bool; i8, i8; u8, u8;

    i16, i16_le; i32, i32_le; i64, i64_le;
    u16, u16_le; u32, u32_le; u64, u64_le;
    f32, f32_le; f64, f64_le;

    i16_le, i16_le; i32_le, i32_le; i64_le, i64_le;
    u16_le, u16_le; u32_le, u32_le; u64_le, u64_le;
    f32_le, f32_le; f64_le, f64_le;
}

impl<T: Encodable, const N: usize> Encodable for [T; N] {
    type Encoded<'buf> = [T::Encoded<'buf>; N];
}

impl<E: ?Sized, T: Encode<E>, const N: usize> Encode<E> for [T; N] {
    fn encode(
        &mut self,
        encoder: &mut E,
        mut slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError> {
        for (i, item) in self.iter_mut().enumerate() {
            item.encode(encoder, slot.index(i))?;
        }
        Ok(())
    }
}

impl<T: Encodable> Encodable for Box<T> {
    type Encoded<'buf> = T::Encoded<'buf>;
}

impl<E: ?Sized, T: Encode<E>> Encode<E> for Box<T> {
    fn encode(
        &mut self,
        encoder: &mut E,
        slot: Slot<'_, Self::Encoded<'_>>,
    ) -> Result<(), EncodeError> {
        T::encode(self, encoder, slot)
    }
}

#[cfg(test)]
mod tests {
    use crate::chunks;
    use crate::testing::assert_encoded;

    #[test]
    fn encode_bool() {
        assert_encoded(true, &chunks![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_encoded(false, &chunks![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn encode_ints() {
        assert_encoded(0xa3u8, &chunks![0xa3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_encoded(-0x45i8, &chunks![0xbb, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        assert_encoded(0x1234u16, &chunks![0x34, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_encoded(-0x1234i16, &chunks![0xcc, 0xed, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        assert_encoded(0x12345678u32, &chunks![0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00]);
        assert_encoded(-0x12345678i32, &chunks![0x88, 0xa9, 0xcb, 0xed, 0x00, 0x00, 0x00, 0x00]);

        assert_encoded(
            0x123456789abcdef0u64,
            &chunks![0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12],
        );
        assert_encoded(
            -0x123456789abcdef0i64,
            &chunks![0x10, 0x21, 0x43, 0x65, 0x87, 0xa9, 0xcb, 0xed],
        );
    }

    #[test]
    fn encode_floats() {
        assert_encoded(
            ::core::f32::consts::PI,
            &chunks![0xdb, 0x0f, 0x49, 0x40, 0x00, 0x00, 0x00, 0x00],
        );
        assert_encoded(
            ::core::f64::consts::PI,
            &chunks![0x18, 0x2d, 0x44, 0x54, 0xfb, 0x21, 0x09, 0x40],
        );
    }

    #[test]
    fn encode_box() {
        assert_encoded(None::<u64>, &chunks![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_encoded(
            Some(0x123456789abcdef0u64),
            &chunks![
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56,
                0x34, 0x12,
            ],
        );
    }

    #[test]
    fn encode_vec() {
        assert_encoded(
            None::<Vec<u32>>,
            &chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
        );
        assert_encoded(
            Some(vec![0x12345678u32, 0x9abcdef0u32]),
            &chunks![
                0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0x78, 0x56, 0x34, 0x12, 0xf0, 0xde, 0xbc, 0x9a,
            ],
        );
        assert_encoded(
            Some(Vec::<u32>::new()),
            &chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff,
            ],
        );
    }

    #[test]
    fn encode_string() {
        assert_encoded(
            None::<String>,
            &chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
        );
        assert_encoded(
            Some("0123".to_string()),
            &chunks![
                0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0x30, 0x31, 0x32, 0x33, 0x00, 0x00, 0x00, 0x00,
            ],
        );
        assert_encoded(
            Some(String::new()),
            &chunks![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff,
            ],
        );
    }
}
