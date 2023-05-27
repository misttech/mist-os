// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT.
// Generated from FIDL library `zither.experimental.zx.types` by zither, a Fuchsia platform tool.

#![allow(unused_imports)]

use zerocopy::{AsBytes, FromBytes, FromZeroes};

/// 'a'
pub const CHAR_CONST: u8 = 97;

pub const SIZE_CONST: usize = 100;

pub const UINTPTR_CONST: usize = 0x1234abcd5678ffff;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructWithPrimitives {
    pub char_field: u8,
    pub size_field: usize,
    pub uintptr_field: usize,
}

pub type Uint8Alias = u8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructWithPointers {
    pub u64ptr: *const u64,
    pub charptr: *const u8,
    pub usizeptr: *const usize,
    pub byteptr: *const u8,
    pub voidptr: *const u8,
    pub aliasptr: *const Uint8Alias,
}

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct StructWithStringArrays {
    pub str: [u8; 10],
    pub strs: [[u8; 6]; 4],
}
