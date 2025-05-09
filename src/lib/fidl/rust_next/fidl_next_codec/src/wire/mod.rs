// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod boxed;
mod envelope;
mod ptr;
mod result;
mod string;
mod table;
mod union;
mod vec;

pub use self::boxed::*;
pub use self::envelope::*;
pub use self::ptr::*;
pub use self::result::*;
pub use self::string::*;
pub use self::table::*;
pub use self::union::*;
pub use self::vec::*;

use core::mem::MaybeUninit;

use crate::{WireF32, WireF64, WireI16, WireI32, WireI64, WireU16, WireU32, WireU64};

/// A FIDL wire type.
///
/// # Safety
///
/// - References to decoded data yielded by `Self::Decoded<'de>` must not outlive `'de`.
/// - `zero_padding` must write zeroes to (at least) the padding bytes of `out`.
pub unsafe trait Wire: 'static + Sized {
    /// The decoded wire type, restricted to the `'de` lifetime.
    type Decoded<'de>: 'de;

    /// Writes zeroes to the padding for this type, if any.
    fn zero_padding(out: &mut MaybeUninit<Self>);
}

macro_rules! impl_primitive {
    ($ty:ty) => {
        unsafe impl Wire for $ty {
            type Decoded<'de> = Self;

            #[inline]
            fn zero_padding(_: &mut MaybeUninit<Self>) {}
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
    bool,
    i8, WireI16, WireI32, WireI64,
    u8, WireU16, WireU32, WireU64,
    WireF32, WireF64,
}

unsafe impl<T: Wire, const N: usize> Wire for [T; N] {
    type Decoded<'de> = [T::Decoded<'de>; N];

    #[inline]
    fn zero_padding(out: &mut MaybeUninit<Self>) {
        for i in 0..N {
            let out_i = unsafe { &mut *out.as_mut_ptr().cast::<MaybeUninit<T>>().add(i) };
            T::zero_padding(out_i);
        }
    }
}
