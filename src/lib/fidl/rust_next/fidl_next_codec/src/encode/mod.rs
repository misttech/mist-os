// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides encoding for FIDL types.

mod error;

use core::mem::MaybeUninit;
use core::ptr::copy_nonoverlapping;

pub use self::error::EncodeError;

use crate::{
    CopyOptimization, Encoder, EncoderExt as _, Wire, WireBox, WireF32, WireF64, WireI16, WireI32,
    WireI64, WireU16, WireU32, WireU64,
};

/// A type which can be encoded as FIDL.
pub trait Encodable {
    /// Whether the conversion from `Self` to `Self::Encoded` is equivalent to copying the raw bytes
    /// of `Self`.
    ///
    /// Copy optimization is disabled by default.
    const COPY_OPTIMIZATION: CopyOptimization<Self, Self::Encoded> = CopyOptimization::disable();

    /// The wire type for the value.
    type Encoded: Wire;
}

/// Encodes a value.
///
/// # Safety
///
/// `encode` must initialize all non-padding bytes of `out`.
pub unsafe trait Encode<E: ?Sized>: Encodable + Sized {
    /// Encodes this value into an encoder and output.
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError>;
}

/// Encodes a reference.
///
/// # Safety
///
/// `encode` must initialize all non-padding bytes of `out`.
pub unsafe trait EncodeRef<E: ?Sized>: Encode<E> {
    /// Encodes this reference into an encoder and output.
    fn encode_ref(
        &self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError>;
}

/// A type which can be encoded as FIDL when optional.
pub trait EncodableOption {
    /// The wire type for the optional value.
    type EncodedOption: Wire;
}

/// Encodes an optional value.
///
/// # Safety
///
/// `encode_option` must initialize all non-padding bytes of `out`.
pub unsafe trait EncodeOption<E: ?Sized>: EncodableOption + Sized {
    /// Encodes this optional value into an encoder and output.
    fn encode_option(
        this: Option<Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError>;
}

/// Encodes an optional reference.
///
/// # Safety
///
/// `encode_option_ref` must initialize all non-padding bytes of `out`.
pub unsafe trait EncodeOptionRef<E: ?Sized>: EncodeOption<E> {
    /// Encodes this optional reference into an encoder and output.
    fn encode_option_ref(
        this: Option<&Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError>;
}

impl<T: Encodable> Encodable for &T {
    type Encoded = T::Encoded;
}

unsafe impl<E: ?Sized, T: EncodeRef<E>> Encode<E> for &T {
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        T::encode_ref(self, encoder, out)
    }
}

impl<T: EncodableOption> EncodableOption for &T {
    type EncodedOption = T::EncodedOption;
}

unsafe impl<E: ?Sized, T: EncodeOptionRef<E>> EncodeOption<E> for &T {
    fn encode_option(
        this: Option<Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        T::encode_option_ref(this, encoder, out)
    }
}

impl<T: Encodable> Encodable for Box<T> {
    type Encoded = T::Encoded;
}

unsafe impl<E: ?Sized, T: Encode<E>> Encode<E> for Box<T> {
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        T::encode(*self, encoder, out)
    }
}

unsafe impl<E: ?Sized, T: EncodeRef<E>> EncodeRef<E> for Box<T> {
    fn encode_ref(
        &self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        T::encode_ref(self, encoder, out)
    }
}

impl<T: EncodableOption> EncodableOption for Box<T> {
    type EncodedOption = T::EncodedOption;
}

unsafe impl<E: ?Sized, T: EncodeOption<E>> EncodeOption<E> for Box<T> {
    fn encode_option(
        this: Option<Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        T::encode_option(this.map(|value| *value), encoder, out)
    }
}

unsafe impl<E: ?Sized, T: EncodeOptionRef<E>> EncodeOptionRef<E> for Box<T> {
    fn encode_option_ref(
        this: Option<&Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError> {
        T::encode_option_ref(this.map(|value| &**value), encoder, out)
    }
}

macro_rules! impl_primitive {
    ($ty:ty) => {
        impl_primitive!($ty, $ty);
    };
    ($ty:ty, $enc:ty) => {
        impl Encodable for $ty {
            const COPY_OPTIMIZATION: CopyOptimization<$ty, $enc> =
                CopyOptimization::<$ty, $enc>::PRIMITIVE;

            type Encoded = $enc;
        }

        unsafe impl<E: ?Sized> Encode<E> for $ty {
            #[inline]
            fn encode(
                self,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                self.encode_ref(encoder, out)
            }
        }

        unsafe impl<E: ?Sized> EncodeRef<E> for $ty {
            #[inline]
            fn encode_ref(
                &self,
                _: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                out.write(<$enc>::from(*self));
                Ok(())
            }
        }

        impl EncodableOption for $ty {
            type EncodedOption = WireBox<'static, $enc>;
        }

        unsafe impl<E: Encoder + ?Sized> EncodeOption<E> for $ty {
            #[inline]
            fn encode_option(
                this: Option<Self>,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::EncodedOption>,
            ) -> Result<(), EncodeError> {
                Self::encode_option_ref(this.as_ref(), encoder, out)
            }
        }

        unsafe impl<E: Encoder + ?Sized> EncodeOptionRef<E> for $ty {
            #[inline]
            fn encode_option_ref(
                this: Option<&Self>,
                encoder: &mut E,
                out: &mut MaybeUninit<Self::EncodedOption>,
            ) -> Result<(), EncodeError> {
                if let Some(value) = this {
                    encoder.encode_next(value)?;
                    WireBox::encode_present(out);
                } else {
                    WireBox::encode_absent(out);
                }

                Ok(())
            }
        }
    };
}

macro_rules! impl_primitives {
    ($($ty:ty $(, $enc:ty)?);* $(;)?) => {
        $(
            impl_primitive!($ty $(, $enc)?);
        )*
    }
}

impl_primitives! {
    ();

    bool;

    i8;
    i16, WireI16; i32, WireI32; i64, WireI64;
    WireI16; WireI32; WireI64;

    u8;
    u16, WireU16; u32, WireU32; u64, WireU64;
    WireU16; WireU32; WireU64;

    f32, WireF32; f64, WireF64;
    WireF32; WireF64;
}

impl<T: Encodable, const N: usize> Encodable for [T; N] {
    const COPY_OPTIMIZATION: CopyOptimization<Self, Self::Encoded> =
        T::COPY_OPTIMIZATION.infer_array();

    type Encoded = [T::Encoded; N];
}

fn encode_to_array<A, E, T, const N: usize>(
    value: A,
    encoder: &mut E,
    out: &mut MaybeUninit<[T::Encoded; N]>,
) -> Result<(), EncodeError>
where
    A: AsRef<[T]> + IntoIterator,
    A::Item: Encode<E, Encoded = T::Encoded>,
    E: ?Sized,
    T: Encode<E>,
{
    if T::COPY_OPTIMIZATION.is_enabled() {
        // SAFETY: `T` has copy optimization enabled and so is safe to copy to the output.
        unsafe {
            copy_nonoverlapping(value.as_ref().as_ptr().cast(), out.as_mut_ptr(), 1);
        }
    } else {
        for (i, item) in value.into_iter().enumerate() {
            // SAFETY: `out` is a `MaybeUninit<[T::Encoded; N]>` and so consists of `N` copies of
            // `T::Encoded` in order with no additional padding. We can make a `&mut MaybeUninit` to
            // the `i`th element by:
            // 1. Getting a pointer to the contents of the `MaybeUninit<[T::Encoded; N]>` (the
            //    pointer is of type `*mut [T::Encoded; N]`).
            // 2. Casting it to `*mut MaybeUninit<T::Encoded>`. Note that `MaybeUninit<T>` always
            //    has the same layout as `T`.
            // 3. Adding `i` to reach the `i`th element.
            // 4. Dereferencing as `&mut`.
            let out_i = unsafe { &mut *out.as_mut_ptr().cast::<MaybeUninit<T::Encoded>>().add(i) };
            item.encode(encoder, out_i)?;
        }
    }
    Ok(())
}

unsafe impl<E, T, const N: usize> Encode<E> for [T; N]
where
    E: ?Sized,
    T: Encode<E>,
{
    fn encode(
        self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encode_to_array(self, encoder, out)
    }
}

unsafe impl<E, T, const N: usize> EncodeRef<E> for [T; N]
where
    E: ?Sized,
    T: EncodeRef<E>,
{
    fn encode_ref(
        &self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        encode_to_array(self, encoder, out)
    }
}

#[cfg(test)]
mod tests {
    use crate::chunks;
    use crate::testing::assert_encoded;

    #[test]
    fn encode_unit() {
        assert_encoded((), &chunks![]);
    }

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
