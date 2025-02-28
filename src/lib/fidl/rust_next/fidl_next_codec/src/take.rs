// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::MaybeUninit;
use core::ptr::copy_nonoverlapping;

use crate::{f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le, CopyOptimization};

/// `From` conversions which may take from a reference using interior mutability.
pub trait TakeFrom<T: ?Sized> {
    /// An optimization flag that allows the bytes of this type to be copied directly during
    /// conversion instead of calling `take_from`.
    ///
    /// This optimization is disabled by default. To enable this optimization, you must unsafely
    /// attest that `Self` is trivially copyable using [`CopyOptimization::enable`] or
    /// [`CopyOptimization::enable_if`].
    const COPY_OPTIMIZATION: CopyOptimization<Self> = CopyOptimization::disable();

    /// Converts from the given `T`, taking any resources that can't be cloned.
    fn take_from(from: &T) -> Self;
}

macro_rules! impl_primitive {
    ($from:ty, $to:ty) => {
        impl TakeFrom<$from> for $to {
            // Copy optimization for primitives is enabled if their size if <= 1 or the target is
            // little-endian.
            const COPY_OPTIMIZATION: CopyOptimization<Self> = unsafe {
                CopyOptimization::enable_if(
                    size_of::<Self>() <= 1 || cfg!(target_endian = "little"),
                )
            };

            #[inline]
            fn take_from(from: &$from) -> $to {
                (*from).into()
            }
        }
    };
}

macro_rules! impl_primitives {
    ($($from:ty, $to:ty);* $(;)?) => {
        $(
            impl_primitive!($from, $to);
        )*
    }
}

impl_primitives! {
    bool, bool;

    i8, i8;
    i16_le, i16; i16_le, i16_le;
    i32_le, i32; i32_le, i32_le;
    i64_le, i64; i64_le, i64_le;

    u8, u8;
    u16_le, u16; u16_le, u16_le;
    u32_le, u32; u32_le, u32_le;
    u64_le, u64; u64_le, u64_le;

    f32_le, f32; f32_le, f32_le;
    f64_le, f64; f64_le, f64_le;
}

impl<T: TakeFrom<WT>, WT, const N: usize> TakeFrom<[WT; N]> for [T; N] {
    const COPY_OPTIMIZATION: CopyOptimization<Self> =
        unsafe { CopyOptimization::enable_if(T::COPY_OPTIMIZATION.is_enabled()) };

    fn take_from(from: &[WT; N]) -> Self {
        let mut result = MaybeUninit::<[T; N]>::uninit();
        if T::COPY_OPTIMIZATION.is_enabled() {
            // SAFETY: `T` has copy optimization enabled and so is safe to copy bytewise.
            unsafe {
                copy_nonoverlapping(from.as_ptr().cast(), result.as_mut_ptr(), 1);
            }
        } else {
            for (i, item) in from.iter().enumerate() {
                let taken = T::take_from(item);
                unsafe {
                    result.as_mut_ptr().cast::<T>().add(i).write(taken);
                }
            }
        }
        unsafe { result.assume_init() }
    }
}

impl<T: TakeFrom<WT>, WT> TakeFrom<WT> for Box<T> {
    fn take_from(from: &WT) -> Self {
        Box::new(T::take_from(from))
    }
}
