// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::{
    emit_doc_string, int_type_natural_name, int_type_wire_name, IdentExt as _,
};
use crate::compiler::Compiler;
use crate::ir::{CompIdent, IntType};

pub fn emit_enum<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let e = &compiler.schema.enum_declarations[ident];

    let name = e.name.type_name().camel();
    let natural_ty = int_type_natural_name(e.ty);
    let wire_ty = int_type_wire_name(e.ty);

    // Write wire type

    emit_doc_string(out, e.attributes.doc_string())?;
    writeln!(out, "#[derive(Clone, Copy)]")?;
    if compiler.config.emit_debug_impls {
        writeln!(out, "#[derive(Debug)]")?;
    }
    writeln!(
        out,
        r#"
        #[repr(transparent)]
        pub struct Wire{name} {{
            value: {wire_ty},
        }}

        impl Wire{name} {{
        "#,
    )?;

    for member in &e.members {
        let const_name = member.name.screaming_snake();
        let value = &member.value.value;

        write!(out, "pub const {const_name}: Wire{name} = Wire{name} {{ value: ")?;

        match e.ty {
            IntType::Int8 | IntType::Uint8 => write!(out, "{value}")?,
            IntType::Int16
            | IntType::Int32
            | IntType::Int64
            | IntType::Uint16
            | IntType::Uint32
            | IntType::Uint64 => {
                write!(out, "{wire_ty}::from_native({value})")?;
            }
        }

        writeln!(out, " }};")?;
    }

    writeln!(out, "}}")?;

    // Write decode impl

    writeln!(
        out,
        r#"
        unsafe impl<___D> ::fidl_next::Decode<___D> for Wire{name}
        where
            ___D: ?Sized,
        {{
            fn decode(
                slot: ::fidl_next::Slot<'_, Self>,
                _: &mut ___D,
            ) -> Result<(), ::fidl_next::DecodeError> {{
        "#,
    )?;

    if e.is_strict {
        writeln!(
            out,
            r#"
            ::fidl_next::munge!(let Self {{ value }} = slot);

            match {natural_ty}::from(*value) {{
            "#,
        )?;

        for member in &e.members {
            let value = &member.value.value;
            write!(out, "| {value}")?;
        }

        writeln!(
            out,
            r#"
                => (),
                unknown => return Err(::fidl_next::DecodeError::InvalidEnumOrdinal(
                    unknown as usize,
                )),
            }}
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                Ok(())
            }}
        }}
        "#,
    )?;

    // Write From impl

    write!(
        out,
        r#"
        impl ::core::convert::From<{name}> for Wire{name} {{
            fn from(natural: {name}) -> Self {{
                match natural {{
        "#,
    )?;

    for member in &e.members {
        let natural_member_name = member.name.camel();
        let wire_member_name = member.name.screaming_snake();

        write!(out, "{name}::{natural_member_name} => Wire{name}::{wire_member_name},")?;
    }

    if !e.is_strict {
        writeln!(
            out,
            r#"
            {name}::Unknown(value) => Wire{name} {{
                value: {wire_ty}::from(value),
            }},
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                }}
            }}
        }}
        "#,
    )?;

    Ok(())
}
