// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::marker::PhantomData;

use crate::{WireF32, WireF64, WireI16, WireI32, WireI64, WireU16, WireU32, WireU64};

/// An optimization hint about whether the conversion from `T` to `U` is equivalent to copying the
/// raw bytes of `T`.
pub struct CopyOptimization<T: ?Sized, U: ?Sized>(bool, PhantomData<(*mut T, *mut U)>);

impl<T: ?Sized, U: ?Sized> CopyOptimization<T, U> {
    /// Returns a `CopyOptimization` hint with the optimization enabled.
    ///
    /// # Safety
    ///
    /// `T` and `U` must be the same size and must not have any uninit bytes (e.g. padding).
    pub const unsafe fn enable() -> Self {
        Self(true, PhantomData)
    }

    /// Returns a `CopyOptimization` hint with the optimization enabled if `value` is `true`.
    ///
    /// # Safety
    ///
    /// `T` and `U` must be the same size and must not have any uninit bytes (e.g. padding) if
    /// `value` is `true`.
    pub const unsafe fn enable_if(value: bool) -> Self {
        Self(value, PhantomData)
    }

    /// Returns a `CopyOptimization` hint with the optimization disabled.
    pub const fn disable() -> Self {
        Self(false, PhantomData)
    }

    /// Returns whether the optimization is enabled.
    pub const fn is_enabled(&self) -> bool {
        self.0
    }

    /// Infers whether the conversion from `[T; N]` to `[U; N]` is copy-optimizable based on the
    /// conversion from `T` to `U`.
    pub const fn infer_array<const N: usize>(&self) -> CopyOptimization<[T; N], [U; N]>
    where
        T: Sized,
        U: Sized,
    {
        unsafe { CopyOptimization::enable_if(self.is_enabled()) }
    }

    /// Infers whether the conversion from `[T]` to `[U]` is copy-optimizable based on the
    /// conversion from `T` to `U`.
    pub const fn infer_slice(&self) -> CopyOptimization<[T], [U]>
    where
        T: Sized,
        U: Sized,
    {
        unsafe { CopyOptimization::enable_if(self.is_enabled()) }
    }
}

impl<T: ?Sized> CopyOptimization<T, T> {
    /// Returns an enabled `CopyOptimization`, as copy optimization is always enabled from a type to
    /// itself.
    pub const fn identity() -> Self {
        unsafe { Self::enable() }
    }
}

macro_rules! impl_primitive {
    ($ty:ty) => {
        impl CopyOptimization<$ty, $ty> {
            /// Whether copy optimization between the two primitive types is enabled.
            pub const PRIMITIVE: Self = Self::identity();
        }
    };
    ($natural:ty, $wire:ty) => {
        impl_primitive!($wire);

        impl CopyOptimization<$natural, $wire> {
            /// Whether copy optimization between the two primitive types is enabled.
            pub const PRIMITIVE: Self =
                // SAFETY: Copy optimization for primitives is enabled if their size if <= 1 or the
                // target is little-endian.
                unsafe {
                    CopyOptimization::enable_if(
                        size_of::<Self>() <= 1 || cfg!(target_endian = "little"),
                    )
                };
        }

        impl CopyOptimization<$wire, $natural> {
            /// Whether copy optimization between the two primitive types is enabled.
            pub const PRIMITIVE: Self =
                // SAFETY: Copy optimization between these two primitives is commutative.
                unsafe {
                    CopyOptimization::enable_if(
                        CopyOptimization::<$natural, $wire>::PRIMITIVE.is_enabled(),
                    )
                };
        }
    };
}

macro_rules! impl_primitives {
    ($($natural:ty $(, $wire:ty)?);* $(;)?) => {
        $(
            impl_primitive!($natural $(, $wire)?);
        )*
    }
}

impl_primitives! {
    ();

    bool;

    i8;
    i16, WireI16;
    i32, WireI32;
    i64, WireI64;

    u8;
    u16, WireU16;
    u32, WireU32;
    u64, WireU64;

    f32, WireF32;
    f64, WireF64;
}
