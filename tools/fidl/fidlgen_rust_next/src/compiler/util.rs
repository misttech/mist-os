// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use super::Compiler;
use crate::ir::{
    CompId, Constant, ConstantKind, DeclType, Id, IntType, LiteralKind, PrimSubtype, Type, TypeKind,
};

pub fn emit_prefixed_comp_ident<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
    prefix: &str,
) -> Result<(), Error> {
    let (lib, ty) = ident.split();
    let type_name = match compiler.schema.get_decl_type(ident).unwrap() {
        DeclType::Alias
        | DeclType::Bits
        | DeclType::Enum
        | DeclType::Struct
        | DeclType::Table
        | DeclType::Union => ty.camel(),
        DeclType::Const => ty.screaming_snake(),
        DeclType::NewType
        | DeclType::Overlay
        | DeclType::Protocol
        | DeclType::Resource
        | DeclType::Service => todo!(),
    };

    if lib == compiler.schema.name {
        write!(out, "crate::{prefix}{type_name}")?;
    } else if lib == "zx" {
        let name = match ty.non_canonical() {
            "ObjType" => "ObjectType",
            _ => &type_name,
        };
        write!(out, "fidl_next::zx::{prefix}{name}")?;
    } else {
        let escaped = lib.replace(".", "_");
        write!(out, "fidl_{escaped}::{prefix}{type_name}")?;
    }

    Ok(())
}

pub fn emit_natural_comp_ident<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    emit_prefixed_comp_ident(compiler, out, ident, "")
}

pub fn emit_wire_comp_ident<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    emit_prefixed_comp_ident(compiler, out, ident, "Wire")
}

pub fn emit_natural_constant<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    constant: &Constant,
    ty: &Type,
) -> Result<(), Error> {
    match &constant.kind {
        ConstantKind::Identifier { identifier } => {
            let (comp_id, member) = identifier.split();
            emit_natural_comp_ident(compiler, out, comp_id)?;

            if let Some(member) = member {
                match compiler.schema.get_decl_type(comp_id).unwrap() {
                    DeclType::Bits => write!(out, "::{}", member.screaming_snake())?,
                    DeclType::Enum => {
                        // Some zx types don't conform to the regular naming scheme
                        if comp_id.library() == "zx"
                            && comp_id.decl_name().non_canonical() == "ObjType"
                        {
                            write!(out, "::{}", member.screaming_snake())?;
                        } else {
                            write!(out, "::{}", member.camel())?;
                        }
                    }
                    _ => panic!("expected member to be of bits or enum"),
                }
            }
        }
        ConstantKind::Literal { literal } => match literal.kind {
            LiteralKind::String => write!(out, "\"{}\"", literal.value.escape_default())?,
            LiteralKind::Bool => write!(out, "{}", literal.value)?,
            LiteralKind::Numeric => match &ty.kind {
                TypeKind::Identifier { identifier, .. } => {
                    emit_natural_comp_ident(compiler, out, identifier)?;
                    write!(out, "::from_bits_retain({})", constant.value)?;
                }
                TypeKind::Primitive { .. } => write!(out, "{}", literal.value)?,
                _ => panic!("invalid constant type"),
            },
        },
        ConstantKind::BinaryOperator { .. } => {
            let TypeKind::Identifier { identifier, .. } = &ty.kind else {
                panic!("invalid constant type");
            };
            emit_natural_comp_ident(compiler, out, identifier)?;
            write!(out, "::from_bits_retain({})", constant.value)?;
        }
    }

    Ok(())
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

pub trait IdExt {
    fn split(&self) -> Split<'_>;

    fn camel(&self) -> String {
        let mut result = String::new();
        for piece in self.split() {
            let mut chars = piece.chars();
            result.push(chars.next().unwrap().to_ascii_uppercase());
            result.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
        result
    }

    fn snake(&self) -> String {
        let mut result = String::new();
        for piece in self.split() {
            if !result.is_empty() {
                result.push('_');
            }
            result.extend(piece.chars().map(|c| c.to_ascii_lowercase()));
        }
        result
    }

    fn screaming_snake(&self) -> String {
        let mut result = String::new();
        for piece in self.split() {
            if !result.is_empty() {
                result.push('_');
            }
            result.extend(piece.chars().map(|c| c.to_ascii_uppercase()));
        }
        result
    }
}

impl IdExt for Id {
    fn split(&self) -> Split<'_> {
        Split { str: self.non_canonical() }
    }
}

pub struct Split<'a> {
    str: &'a str,
}

impl<'a> Iterator for Split<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let mut char_indices = self.str.char_indices().skip_while(|(_, c)| *c == '_').peekable();

        let (start, mut prev) = char_indices.next()?;
        let mut end = self.str.len();

        while let Some((index, current)) = char_indices.next() {
            if current == '_' {
                end = index;
                break;
            }

            let prev_lower = prev.is_ascii_lowercase();
            let prev_digit = prev.is_ascii_digit();
            let current_upper = current.is_ascii_uppercase();
            let next_lower = char_indices.peek().is_some_and(|(_, c)| c.is_ascii_lowercase());

            let is_first_uppercase = (prev_lower || prev_digit) && current_upper;
            let is_last_uppercase = current_upper && next_lower;
            if is_first_uppercase || is_last_uppercase {
                end = index;
                break;
            }

            prev = current;
        }

        let result = &self.str[start..end];
        self.str = &self.str[end..];
        Some(result)
    }
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

pub fn prim_subtype_natural_name(prim: PrimSubtype) -> &'static str {
    match prim {
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

#[cfg(test)]
mod tests {
    use super::IdExt as _;
    use crate::ir::Id;

    const TEST_CASES: &[&str] = &[
        "foo_bar",
        "foo__bar",
        "FooBar",
        "fooBar",
        "FOOBar",
        "__foo_bar",
        "foo123bar",
        "foO123bar",
        "foo_123bar",
        "FOO123Bar",
        "FOO123bar",
    ];

    #[test]
    fn split() {
        const EXPECTEDS: [&[&str]; TEST_CASES.len()] = [
            &["foo", "bar"],
            &["foo", "bar"],
            &["Foo", "Bar"],
            &["foo", "Bar"],
            &["FOO", "Bar"],
            &["foo", "bar"],
            &["foo123bar"],
            &["fo", "O123bar"],
            &["foo", "123bar"],
            &["FOO123", "Bar"],
            &["FOO123bar"],
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).split().collect::<Vec<_>>(),
                expected,
                "{case} did not split correctly",
            );
        }
    }

    #[test]
    fn snake() {
        const EXPECTEDS: [&str; TEST_CASES.len()] = [
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo_bar",
            "foo123bar",
            "fo_o123bar",
            "foo_123bar",
            "foo123_bar",
            "foo123bar",
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).snake(),
                expected,
                "{case} was not transformed to snake case correctly",
            );
        }
    }

    #[test]
    fn camel() {
        const EXPECTEDS: [&str; TEST_CASES.len()] = [
            "FooBar",
            "FooBar",
            "FooBar",
            "FooBar",
            "FooBar",
            "FooBar",
            "Foo123bar",
            "FoO123bar",
            "Foo123bar",
            "Foo123Bar",
            "Foo123bar",
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).camel(),
                expected,
                "{case} was not transformed to camel case correctly",
            );
        }
    }

    #[test]
    fn screaming_snake() {
        const EXPECTEDS: [&str; TEST_CASES.len()] = [
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO_BAR",
            "FOO123BAR",
            "FO_O123BAR",
            "FOO_123BAR",
            "FOO123_BAR",
            "FOO123BAR",
        ];

        for (case, expected) in TEST_CASES.iter().zip(EXPECTEDS.iter()) {
            assert_eq!(
                &Id::from_str(case).screaming_snake(),
                expected,
                "{case} was not transformed to screaming snake case correctly",
            );
        }
    }
}
