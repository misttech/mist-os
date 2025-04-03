// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Provides encoding for FIDL types.

mod error;

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr::copy_nonoverlapping;

pub use self::error::EncodeError;

use crate::{
    Encoder, EncoderExt as _, WireBox, WireF32, WireF64, WireI16, WireI32, WireI64, WireU16,
    WireU32, WireU64,
};

/// An optimization hint about whether `T` is trivially copyable.
pub struct CopyOptimization<T: ?Sized>(bool, PhantomData<T>);

impl<T: ?Sized> CopyOptimization<T> {
    /// Returns a `CopyOptimization` hint with the optimization enabled for `T`.
    ///
    /// # Safety
    ///
    /// `T` must not have any uninit bytes (e.g. padding).
    pub const unsafe fn enable() -> Self {
        Self(true, PhantomData)
    }

    /// Returns a `CopyOptimization` hint with the optimization enabled for `T` if `value` is
    /// `true`.
    ///
    /// # Safety
    ///
    /// `T` must not have any uninit bytes (e.g. padding) if `value` is `true`.
    pub const unsafe fn enable_if(value: bool) -> Self {
        Self(value, PhantomData)
    }

    /// Returns a `CopyOptimization` hint with the optimization disabled for `T`.
    pub const fn disable() -> Self {
        Self(false, PhantomData)
    }

    /// Returns whether the optimization is enabled for `T`.
    pub const fn is_enabled(&self) -> bool {
        self.0
    }
}

/// Zeroes the padding of this type.
///
/// # Safety
///
/// `zero_padding` must write zeroes to at least the padding bytes of the pointed-to memory.
pub unsafe trait ZeroPadding: Sized {
    /// Writes zeroes to the padding for this type, if any.
    fn zero_padding(out: &mut MaybeUninit<Self>);
}

unsafe impl<T: ZeroPadding, const N: usize> ZeroPadding for [T; N] {
    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        for i in 0..N {
            let out_i = unsafe { &mut *out.as_mut_ptr().cast::<MaybeUninit<T>>().add(i) };
            T::zero_padding(out_i);
        }
    }
}

macro_rules! impl_zero_padding_for_primitive {
    ($ty:ty) => {
        unsafe impl ZeroPadding for $ty {
            #[inline]
            fn zero_padding(_: &mut MaybeUninit<Self>) {}
        }
    };
}

macro_rules! impl_zero_padding_for_primitives {
    ($($ty:ty),* $(,)?) => {
        $(
            impl_zero_padding_for_primitive!($ty);
        )*
    }
}

impl_zero_padding_for_primitives! {
    (), bool, i8, u8,
    WireI16, WireI32, WireI64,
    WireU16, WireU32, WireU64,
    WireF32, WireF64,
}

/// A type which can be encoded as FIDL.
pub trait Encodable {
    /// An optimization flag that allows the bytes of this type to be copied directly during
    /// encoding instead of calling `encode`.
    ///
    /// This optimization is disabled by default. To enable this optimization, you must unsafely
    /// attest that `Self` is trivially copyable using [`CopyOptimization::enable`] or
    /// [`CopyOptimization::enable_if`].
    const COPY_OPTIMIZATION: CopyOptimization<Self> = CopyOptimization::disable();

    /// The wire type for the value.
    type Encoded: ZeroPadding;
}

/// Encodes a value.
///
/// # Safety
///
/// `encode` must initialize all non-padding bytes of `out`.
pub unsafe trait Encode<E: ?Sized>: Encodable {
    /// Encodes this value into an encoder and output.
    fn encode(
        &mut self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError>;
}

/// A type which can be encoded as FIDL when optional.
pub trait EncodableOption {
    /// The wire type for the optional value.
    type EncodedOption: ZeroPadding;
}

/// Encodes an optional value.
///
/// # Safety
///
/// `encode_option` must initialize all non-padding bytes of `out`.
pub unsafe trait EncodeOption<E: ?Sized>: EncodableOption {
    /// Encodes this optional value into an encoder and output.
    fn encode_option(
        this: Option<&mut Self>,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::EncodedOption>,
    ) -> Result<(), EncodeError>;
}

macro_rules! impl_encode_for_primitive {
    ($ty:ty, $enc:ty) => {
        impl Encodable for $ty {
            // Copy optimization for primitives is enabled if their size is <= 1 or the target is
            // little-endian.
            const COPY_OPTIMIZATION: CopyOptimization<Self> = unsafe {
                CopyOptimization::enable_if(
                    size_of::<Self>() <= 1 || cfg!(target_endian = "little"),
                )
            };

            type Encoded = $enc;
        }

        unsafe impl<E: ?Sized> Encode<E> for $ty {
            #[inline]
            fn encode(
                &mut self,
                _: &mut E,
                out: &mut MaybeUninit<Self::Encoded>,
            ) -> Result<(), EncodeError> {
                out.write(<$enc>::from(*self));
                Ok(())
            }
        }

        impl EncodableOption for $ty {
            type EncodedOption = WireBox<$enc>;
        }

        unsafe impl<E: Encoder + ?Sized> EncodeOption<E> for $ty {
            #[inline]
            fn encode_option(
                this: Option<&mut Self>,
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

macro_rules! impl_encode_for_primitives {
    ($($ty:ty, $enc:ty);* $(;)?) => {
        $(
            impl_encode_for_primitive!($ty, $enc);
        )*
    }
}

impl_encode_for_primitives! {
    (), (); bool, bool; i8, i8; u8, u8;

    i16, WireI16; i32, WireI32; i64, WireI64;
    u16, WireU16; u32, WireU32; u64, WireU64;
    f32, WireF32; f64, WireF64;

    WireI16, WireI16; WireI32, WireI32; WireI64, WireI64;
    WireU16, WireU16; WireU32, WireU32; WireU64, WireU64;
    WireF32, WireF32; WireF64, WireF64;
}

impl<T: Encodable, const N: usize> Encodable for [T; N] {
    const COPY_OPTIMIZATION: CopyOptimization<Self> =
        unsafe { CopyOptimization::enable_if(T::COPY_OPTIMIZATION.is_enabled()) };

    type Encoded = [T::Encoded; N];
}

unsafe impl<E: ?Sized, T: Encode<E>, const N: usize> Encode<E> for [T; N] {
    fn encode(
        &mut self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        if T::COPY_OPTIMIZATION.is_enabled() {
            // SAFETY: `T` has copy optimization enabled and so is safe to copy to the output.
            unsafe {
                copy_nonoverlapping(self.as_ptr().cast(), out.as_mut_ptr(), 1);
            }
        } else {
            for (i, item) in self.iter_mut().enumerate() {
                let out_i =
                    unsafe { &mut *out.as_mut_ptr().cast::<MaybeUninit<T::Encoded>>().add(i) };
                item.encode(encoder, out_i)?;
            }
        }
        Ok(())
    }
}

impl<T: Encodable> Encodable for Box<T> {
    type Encoded = T::Encoded;
}

unsafe impl<E: ?Sized, T: Encode<E>> Encode<E> for Box<T> {
    fn encode(
        &mut self,
        encoder: &mut E,
        out: &mut MaybeUninit<Self::Encoded>,
    ) -> Result<(), EncodeError> {
        T::encode(self, encoder, out)
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
