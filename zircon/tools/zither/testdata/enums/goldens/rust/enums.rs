// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT.
// Generated from FIDL library `zither.enums` by zither, a Fuchsia platform tool.

#![allow(unused_imports)]

use zerocopy::AsBytes;

#[repr(u8)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    Red = 0,
    Orange = 1,
    Yellow = 2,
    Green = 3,
    Blue = 4,
    Indigo = 5,
    Violet = 6,
}

#[repr(u8)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Uint8Limits {
    Min = 0,
    Max = 0b11111111,
}

#[repr(u16)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Uint16Limits {
    Min = 0,
    Max = 0xffff,
}

#[repr(u32)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Uint32Limits {
    Min = 0,
    Max = 0xffffffff,
}

#[repr(u64)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Uint64Limits {
    Min = 0,
    Max = 0xffffffffffffffff,
}

#[repr(i8)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Int8Limits {
    Min = -0x80,
    Max = 0x7f,
}

#[repr(i16)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Int16Limits {
    Min = -0x8000,
    Max = 0x7fff,
}

#[repr(i32)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Int32Limits {
    Min = -0x80000000,
    Max = 0x7fffffff,
}

#[repr(i64)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Int64Limits {
    Min = -0x8000000000000000,
    Max = 0x7fffffffffffffff,
}

pub const FOUR: u16 = 0b100;

#[repr(u16)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumWithExpressions {
    OrWithLiteral = 3,  // 0b01 | 0b10
    OrWithConstant = 5, // 0b001 | FOUR
}

/// Enum with a one-line comment.
#[repr(u8)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumWithOneLineComment {
    /// Enum member with one-line comment.
    MemberWithOneLineComment = 0,

    /// Enum member
    ///     with a
    ///         many-line
    ///           comment.
    MemberWithManyLineComment = 1,
}

/// Enum
///
///     with a
///         many-line
///           comment.
#[repr(u16)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumWithManyLineComment {
    Member = 0,
}

pub const RED: Color = Color::Red;

pub const UINT8_MIN: Uint8Limits = Uint8Limits::Min;

pub const UINT8_MAX: Uint8Limits = Uint8Limits::Max;

pub const UINT16_MIN: Uint16Limits = Uint16Limits::Min;

pub const UINT16_MAX: Uint16Limits = Uint16Limits::Max;

pub const UINT32_MIN: Uint32Limits = Uint32Limits::Min;

pub const UINT32_MAX: Uint32Limits = Uint32Limits::Max;

pub const UINT64_MIN: Uint64Limits = Uint64Limits::Min;

pub const UINT64_MAX: Uint64Limits = Uint64Limits::Max;

pub const INT8_MIN: Int8Limits = Int8Limits::Min;

pub const INT8_MAX: Int8Limits = Int8Limits::Max;

pub const INT16_MIN: Int16Limits = Int16Limits::Min;

pub const INT16_MAX: Int16Limits = Int16Limits::Max;

pub const INT32_MIN: Int32Limits = Int32Limits::Min;

pub const INT32_MAX: Int32Limits = Int32Limits::Max;

pub const INT64_MIN: Int64Limits = Int64Limits::Min;

pub const INT64_MAX: Int64Limits = Int64Limits::Max;
