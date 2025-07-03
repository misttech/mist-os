// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;

use crate::ir::{IntType, PrimSubtype};

pub struct NaturalPrimTemplate<'a>(pub &'a PrimSubtype);

impl fmt::Display for NaturalPrimTemplate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self.0 {
                PrimSubtype::Bool => "bool",
                PrimSubtype::Float32 => "f32",
                PrimSubtype::Float64 => "f64",
                PrimSubtype::Int8 => "i8",
                PrimSubtype::Int16 => "i16",
                PrimSubtype::Int32 => "i32",
                PrimSubtype::Int64 => "i64",
                PrimSubtype::Uint8 => "u8",
                PrimSubtype::Uint16 => "u16",
                PrimSubtype::Uint32 => "u32",
                PrimSubtype::Uint64 => "u64",
            }
        )
    }
}

pub struct NaturalIntTemplate<'a>(pub &'a IntType);

impl fmt::Display for NaturalIntTemplate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self.0 {
                IntType::Int8 => "i8",
                IntType::Int16 => "i16",
                IntType::Int32 => "i32",
                IntType::Int64 => "i64",
                IntType::Uint8 => "u8",
                IntType::Uint16 => "u16",
                IntType::Uint32 => "u32",
                IntType::Uint64 => "u64",
            }
        )
    }
}

pub struct WirePrimTemplate<'a>(pub &'a PrimSubtype);

impl fmt::Display for WirePrimTemplate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self.0 {
                PrimSubtype::Bool => "bool",
                PrimSubtype::Float32 => "::fidl_next::WireF32",
                PrimSubtype::Float64 => "::fidl_next::WireF64",
                PrimSubtype::Int8 => "i8",
                PrimSubtype::Int16 => "::fidl_next::WireI16",
                PrimSubtype::Int32 => "::fidl_next::WireI32",
                PrimSubtype::Int64 => "::fidl_next::WireI64",
                PrimSubtype::Uint8 => "u8",
                PrimSubtype::Uint16 => "::fidl_next::WireU16",
                PrimSubtype::Uint32 => "::fidl_next::WireU32",
                PrimSubtype::Uint64 => "::fidl_next::WireU64",
            }
        )
    }
}

pub struct WireIntTemplate<'a>(pub &'a IntType);

impl fmt::Display for WireIntTemplate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self.0 {
                IntType::Int8 => "i8",
                IntType::Int16 => "::fidl_next::WireI16",
                IntType::Int32 => "::fidl_next::WireI32",
                IntType::Int64 => "::fidl_next::WireI64",
                IntType::Uint8 => "u8",
                IntType::Uint16 => "::fidl_next::WireU16",
                IntType::Uint32 => "::fidl_next::WireU32",
                IntType::Uint64 => "::fidl_next::WireU64",
            }
        )
    }
}
