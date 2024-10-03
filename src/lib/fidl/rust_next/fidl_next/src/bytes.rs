// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{f32_le, f64_le, i16_le, i32_le, i64_le, u16_le, u32_le, u64_le};

/// A type which can always be transmuted from raw bytes.
///
/// This trait requires that the implementing type have no invalid bit patterns
/// for any of its fields.
///
/// # Safety
///
/// It must be sound to transmute arbitrary bytes into the implementing type.
pub unsafe trait FromBytes {}

/// A type which can always be transmuted to raw bytes.
///
/// # Safety
///
/// It must be sound to transmute this type into raw bytes. In particular, the
/// implementing type must not have padding.
pub unsafe trait IntoBytes {}

macro_rules! impl_from_bytes {
    ($($ty:ty),* $(,)?) => {
        $(unsafe impl FromBytes for $ty {})*
    };
}

macro_rules! impl_into_bytes {
    ($($ty:ty),* $(,)?) => {
        $(unsafe impl IntoBytes for $ty {})*
    };
}

impl_from_bytes! {
    i8, i16_le, i32_le, i64_le,
    u8, u16_le, u32_le, u64_le,
    f32_le, f64_le,
}

impl_into_bytes! {
    bool,
    i8, i16, i32, i64,
    i16_le, i32_le, i64_le,
    u8, u16, u32, u64,
    u16_le, u32_le, u64_le,
    f32, f64,
    f32_le, f64_le,
}

unsafe impl<T: FromBytes, const N: usize> FromBytes for [T; N] {}
unsafe impl<T: IntoBytes, const N: usize> IntoBytes for [T; N] {}

unsafe impl<T> IntoBytes for *const T {}
unsafe impl<T> IntoBytes for *mut T {}
