// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT.
// Generated from FIDL library `zither.structs` by zither, a Fuchsia platform tool.

#![allow(unused_imports)]

use bitflags::bitflags;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct Empty {}

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct Singleton {
    pub value: u8,
}

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct Doubtleton {
    pub first: Singleton,
    pub second: Singleton,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrimitiveMembers {
    pub i64: i64,
    pub u64: u64,
    pub i32: i32,
    pub u32: u32,
    pub i16: i16,
    pub u16: u16,
    pub i8: i8,
    pub u8: u8,
    pub b: bool,
}

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct ArrayMembers {
    pub u8s: [u8; 10],
    pub singletons: [Singleton; 6],
    pub nested_arrays1: [[u8; 10]; 20],
    pub nested_arrays2: [[[i8; 1]; 2]; 3],
}

#[repr(i32)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Enum {
    Zero = 0,
    One = 1,
}

bitflags! {
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct Bits : u16 {
        const ONE = 1 << 0;
        const TWO = 1 << 1;
  }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumAndBitsMembers {
    pub e: Enum,
    pub b: Bits,
}

/// Struct with a one-line comment.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructWithOneLineComment {
    /// Struct member with one-line comment.
    pub member_with_one_line_comment: u32,

    /// Struct member
    ///     with a
    ///         many-line
    ///           comment.
    pub member_with_many_line_comment: bool,
}

/// Struct
///
///     with a
///         many-line
///           comment.
#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct StructWithManyLineComment {
    pub member: u16,
}
