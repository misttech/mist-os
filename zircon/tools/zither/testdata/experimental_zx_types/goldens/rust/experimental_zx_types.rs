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

#[repr(C)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, FromBytes, FromZeroes, PartialEq)]
pub struct OverlayStructVariant {
    pub value: u64,
}

#[repr(C)]
#[derive(AsBytes, Clone, Copy)]
pub struct OverlayWithEquallySizedVariants {
    pub discriminant: OverlayWithEquallySizedVariantsDiscriminant,
    pub variant: OverlayWithEquallySizedVariantsVariant,
}

#[repr(u64)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayWithEquallySizedVariantsDiscriminant {
    A = 1,
    B = 2,
    C = 3,
    D = 4,
}

// TODO(https://github.com/rust-lang/rust/issues/49804): Define anonymously.
#[repr(C)]
#[derive(AsBytes, Clone, Copy)]
pub union OverlayWithEquallySizedVariantsVariant {
    pub a: u64,
    pub b: i64,
    pub c: OverlayStructVariant,
    pub d: u64,
}

impl OverlayWithEquallySizedVariants {
    pub fn is_a(&self) -> bool {
        self.discriminant == OverlayWithEquallySizedVariantsDiscriminant::A
    }

    pub fn as_a(&mut self) -> Option<&mut u64> {
        if self.is_a() {
            return None;
        }
        unsafe { Some(&mut self.variant.a) }
    }

    pub fn is_b(&self) -> bool {
        self.discriminant == OverlayWithEquallySizedVariantsDiscriminant::B
    }

    pub fn as_b(&mut self) -> Option<&mut i64> {
        if self.is_b() {
            return None;
        }
        unsafe { Some(&mut self.variant.b) }
    }

    pub fn is_c(&self) -> bool {
        self.discriminant == OverlayWithEquallySizedVariantsDiscriminant::C
    }

    pub fn as_c(&mut self) -> Option<&mut OverlayStructVariant> {
        if self.is_c() {
            return None;
        }
        unsafe { Some(&mut self.variant.c) }
    }

    pub fn is_d(&self) -> bool {
        self.discriminant == OverlayWithEquallySizedVariantsDiscriminant::D
    }

    pub fn as_d(&mut self) -> Option<&mut u64> {
        if self.is_d() {
            return None;
        }
        unsafe { Some(&mut self.variant.d) }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OverlayWithDifferentlySizedVariants {
    pub discriminant: OverlayWithDifferentlySizedVariantsDiscriminant,
    pub variant: OverlayWithDifferentlySizedVariantsVariant,
}

#[repr(u64)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayWithDifferentlySizedVariantsDiscriminant {
    A = 1,
    B = 2,
    C = 3,
}

// TODO(https://github.com/rust-lang/rust/issues/49804): Define anonymously.
#[repr(C)]
#[derive(Clone, Copy)]
pub union OverlayWithDifferentlySizedVariantsVariant {
    pub a: OverlayStructVariant,
    pub b: u32,
    pub c: bool,
}

impl OverlayWithDifferentlySizedVariants {
    pub fn is_a(&self) -> bool {
        self.discriminant == OverlayWithDifferentlySizedVariantsDiscriminant::A
    }

    pub fn as_a(&mut self) -> Option<&mut OverlayStructVariant> {
        if self.is_a() {
            return None;
        }
        unsafe { Some(&mut self.variant.a) }
    }

    pub fn is_b(&self) -> bool {
        self.discriminant == OverlayWithDifferentlySizedVariantsDiscriminant::B
    }

    pub fn as_b(&mut self) -> Option<&mut u32> {
        if self.is_b() {
            return None;
        }
        unsafe { Some(&mut self.variant.b) }
    }

    pub fn is_c(&self) -> bool {
        self.discriminant == OverlayWithDifferentlySizedVariantsDiscriminant::C
    }

    pub fn as_c(&mut self) -> Option<&mut bool> {
        if self.is_c() {
            return None;
        }
        unsafe { Some(&mut self.variant.c) }
    }
}
