// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT.
// Generated from FIDL library `zither.bits` by zither, a Fuchsia platform tool.

#![allow(unused_imports)]

use bitflags::bitflags;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

bitflags! {
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct Uint8Bits : u8 {
        const ONE = 1 << 0;
        const TWO = 1 << 1;
        const FOUR = 1 << 2;
        const EIGHT = 1 << 3;
        const SIXTEEN = 1 << 4;
        const THIRTY_TWO = 1 << 5;
        const SIXTY_FOUR = 1 << 6;
        const ONE_HUNDRED_TWENTY_EIGHT = 1 << 7;
  }
}

bitflags! {
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct Uint16Bits : u16 {
        const ZEROTH = 1 << 0;
        const FIRST = 1 << 1;
        const SECOND = 1 << 2;
        const THIRD = 1 << 3;
        const FOURTH = 1 << 4;
        const FIFTH = 1 << 5;
        const SIXTH = 1 << 6;
        const SEVENTH = 1 << 7;
        const EIGHT = 1 << 8;
        const NINTH = 1 << 9;
        const TENTH = 1 << 10;
        const ELEVENTH = 1 << 11;
        const TWELFTH = 1 << 12;
        const THIRTEENTH = 1 << 13;
        const FOURTEENTH = 1 << 14;
        const FIFTHTEENTH = 1 << 15;
  }
}

bitflags! {
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct Uint32Bits : u32 {
        const POW_0 = 1 << 0;
        const POW_31 = 1 << 31;
  }
}

bitflags! {
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct Uint64Bits : u64 {
        const POW_0 = 1 << 0;
        const POW_63 = 1 << 63;
  }
}

bitflags! {
    /// Bits with a one-line comment.
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct BitsWithOneLineComment : u8 {

        /// Bits member with one-line comment.
        const MEMBER_WITH_ONE_LINE_COMMENT = 1 << 0;

        /// Bits member
        ///     with a
        ///         many-line
        ///           comment.
        const MEMBER_WITH_MANY_LINE_COMMENT = 1 << 6;
  }
}

bitflags! {
    /// Bits
    ///
    ///     with a
    ///         many-line
    ///           comment.
    #[repr(C)]
    #[derive(AsBytes, FromZeroes, FromBytes)]
    pub struct BitsWithManyLineComment : u16 {
        const MEMBER = 1 << 0;
  }
}

pub const SEVENTY_TWO: Uint8Bits = Uint8Bits::from_bits_truncate(0b1001000); // Uint8Bits.SIXTY_FOUR | Uint8Bits.EIGHT

pub const SOME_BITS: Uint16Bits = Uint16Bits::from_bits_truncate(0b1001000000010); // Uint16Bits.FIRST | Uint16Bits.NINTH | Uint16Bits.TWELFTH

pub const U32_POW_0: Uint32Bits = Uint32Bits::POW_0;

pub const U64_POW_63: Uint64Bits = Uint64Bits::POW_63;
