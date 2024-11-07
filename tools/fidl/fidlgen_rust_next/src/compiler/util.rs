// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use super::Compiler;
use crate::ir::{CompIdent, IntType, PrimSubtype};

pub fn emit_prefixed_comp_ident<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
    prefix: &str,
) -> Result<(), Error> {
    let (lib, ty) = ident.split();
    if lib == compiler.schema.name {
        write!(out, "crate::{prefix}{ty}")?;
    } else {
        let escaped = lib.replace(".", "_");
        write!(out, "fidl_{escaped}::{prefix}{ty}")?;
    }

    Ok(())
}

pub fn emit_natural_comp_ident<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    emit_prefixed_comp_ident(compiler, out, ident, "")
}

pub fn emit_wire_comp_ident<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    emit_prefixed_comp_ident(compiler, out, ident, "Wire")
}

pub fn emit_doc_string<W: Write>(out: &mut W, doc: Option<&str>) -> Result<(), Error> {
    if let Some(doc) = doc {
        for line in doc.lines() {
            let line = line.trim();
            writeln!(out, "/// {line}")?;
        }
    }

    Ok(())
}

pub fn snake_to_camel(s: &str) -> String {
    let mut result = String::new();
    for piece in s.split('_') {
        let mut first = true;
        for c in piece.chars() {
            if first {
                first = false;
                result.extend(c.to_uppercase());
            } else {
                result.extend(c.to_lowercase());
            }
        }
    }
    result
}

pub fn int_type_wire_name(int: IntType) -> &'static str {
    match int {
        IntType::Int8 => "i8",
        IntType::Int16 => "::fidl_next::i16_le",
        IntType::Int32 => "::fidl_next::i32_le",
        IntType::Int64 => "::fidl_next::i64_le",
        IntType::Uint8 => "u8",
        IntType::Uint16 => "::fidl_next::u16_le",
        IntType::Uint32 => "::fidl_next::u32_le",
        IntType::Uint64 => "::fidl_next::u64_le",
    }
}

pub fn int_type_natural_name(int: IntType) -> &'static str {
    match int {
        IntType::Int8 => "i8",
        IntType::Int16 => "i16",
        IntType::Int32 => "i32",
        IntType::Int64 => "i64",
        IntType::Uint8 => "u8",
        IntType::Uint16 => "u16",
        IntType::Uint32 => "u32",
        IntType::Uint64 => "u64",
    }
}

pub fn prim_subtype_wire_name(prim: PrimSubtype) -> &'static str {
    match prim {
        PrimSubtype::Bool => "bool",
        PrimSubtype::Float32 => "::fidl_next::f32_le",
        PrimSubtype::Float64 => "::fidl_next::f64_le",
        PrimSubtype::Int8 => "i8",
        PrimSubtype::Int16 => "::fidl_next::i16_le",
        PrimSubtype::Int32 => "::fidl_next::i32_le",
        PrimSubtype::Int64 => "::fidl_next::i64_le",
        PrimSubtype::Uint8 => "u8",
        PrimSubtype::Uint16 => "::fidl_next::u16_le",
        PrimSubtype::Uint32 => "::fidl_next::u32_le",
        PrimSubtype::Uint64 => "::fidl_next::u64_le",
    }
}
