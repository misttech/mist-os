// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::{forget, MaybeUninit};
use core::ptr::copy_nonoverlapping;

use crate::{
    CopyOptimization, WireF32, WireF64, WireI16, WireI32, WireI64, WireU16, WireU32, WireU64,
};

/// A type which is convertible from a wire type.
pub trait FromWire<W>: Sized {
    /// Whether the conversion from `W` to `Self` is equivalent to copying the raw bytes of `W`.
    ///
    /// Copy optimization is disabled by default.
    const COPY_OPTIMIZATION: CopyOptimization<W, Self> = CopyOptimization::disable();

    /// Converts the given `wire` to this type.
    fn from_wire(wire: W) -> Self;
}

/// An optional type which is convertible from a wire type.
pub trait FromWireOption<W>: Sized {
    /// Converts the given `wire` to an option of this type.
    fn from_wire_option(wire: W) -> Option<Self>;
}

macro_rules! impl_primitive {
    ($ty:ty) => {
        impl_primitive!($ty, $ty);
    };
    ($ty:ty, $enc:ty) => {
        impl FromWire<$enc> for $ty {
            const COPY_OPTIMIZATION: CopyOptimization<$enc, $ty> =
                CopyOptimization::<$enc, $ty>::PRIMITIVE;

            fn from_wire(wire: $enc) -> Self {
                wire.into()
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

impl<T: FromWire<W>, W, const N: usize> FromWire<[W; N]> for [T; N] {
    fn from_wire(wire: [W; N]) -> Self {
        let mut result = MaybeUninit::<[T; N]>::uninit();
        if T::COPY_OPTIMIZATION.is_enabled() {
            // SAFETY: `T` has copy optimization enabled and so is safe to copy bytewise.
            unsafe {
                copy_nonoverlapping(wire.as_ptr().cast(), result.as_mut_ptr(), 1);
            }
            forget(wire);
        } else {
            for (i, item) in wire.into_iter().enumerate() {
                unsafe {
                    result.as_mut_ptr().cast::<T>().add(i).write(T::from_wire(item));
                }
            }
        }
        unsafe { result.assume_init() }
    }
}

impl<T: FromWire<W>, W> FromWire<W> for Box<T> {
    fn from_wire(wire: W) -> Self {
        Box::new(T::from_wire(wire))
    }
}

impl<T: FromWireOption<W>, W> FromWire<W> for Option<T> {
    fn from_wire(wire: W) -> Self {
        T::from_wire_option(wire)
    }
}
