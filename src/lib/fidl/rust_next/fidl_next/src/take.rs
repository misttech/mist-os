// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::MaybeUninit;

use crate::{f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le};

/// `From` conversions which take from a mutable reference.
pub trait TakeFrom<T: ?Sized> {
    /// Converts from the given `T`, taking any resources that can't be cloned.
    fn take_from(from: &mut T) -> Self;
}

macro_rules! impl_primitive {
    ($from:ty, $to:ty) => {
        impl TakeFrom<$from> for $to {
            #[inline]
            fn take_from(from: &mut $from) -> $to {
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
    fn take_from(from: &mut [WT; N]) -> Self {
        let mut result = MaybeUninit::<[T; N]>::uninit();
        for (i, item) in from.iter_mut().enumerate() {
            let taken = T::take_from(item);
            unsafe {
                result.as_mut_ptr().cast::<T>().add(i).write(taken);
            }
        }
        unsafe { result.assume_init() }
    }
}

impl<T: TakeFrom<WT>, WT> TakeFrom<WT> for Box<T> {
    fn take_from(from: &mut WT) -> Self {
        Box::new(T::take_from(from))
    }
}
