// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::natural::emit_type;
use crate::compiler::util::{emit_doc_string, snake_to_camel};
use crate::compiler::Compiler;
use crate::ir::CompIdent;

pub fn emit_union<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let u = &compiler.schema.union_declarations[ident];

    let name = &u.name.type_name();
    let is_static = u.shape.max_out_of_line == 0;

    let params = if is_static { "" } else { "<'buf>" };

    // Write natural type

    emit_doc_string(out, u.attributes.doc_string())?;
    if !u.is_resource {
        writeln!(out, "#[derive(Clone)]")?;
    }
    if compiler.config.emit_debug_impls {
        writeln!(out, "#[derive(Debug)]")?;
    }
    writeln!(
        out,
        r#"
        pub enum {name} {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);

        writeln!(out, "{member_name}(")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, "),")?;
    }

    if !u.is_strict {
        writeln!(out, "Unknown(u64),")?;
    }

    writeln!(out, "}}")?;

    // Wrire Encode impls

    writeln!(
        out,
        r#"
        impl ::fidl_next::Encodable for {name} {{
            type Encoded<'buf> = Wire{name}{params};
        }}

        impl<___E> ::fidl_next::Encode<___E> for {name}
        where
            ___E: ::fidl_next::Encoder + ?Sized,
        "#,
    )?;

    for member in &u.members {
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ": ::fidl_next::Encode<___E>,")?;
    }

    writeln!(
        out,
        r#"
        {{
            fn encode(
                &mut self,
                encoder: &mut ___E,
                slot: ::fidl_next::Slot<'_, Self::Encoded<'_>>,
            ) -> Result<(), ::fidl_next::EncodeError> {{
                ::fidl_next::munge!(let Wire{name} {{ raw, _phantom: _ }} = slot);

                match self {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);
        let ord = member.ordinal;

        writeln!(
            out,
            "Self::{member_name}(value) => ::fidl_next::RawWireUnion::encode_as::<___E, ",
        )?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(value, {ord}, encoder, raw)?,")?;
    }

    if !u.is_strict {
        writeln!(
            out,
            r#"
            Self::Unknown(ordinal) => return Err(
                ::fidl_next::EncodeError::UnknownUnionOrdinal(
                    *ordinal as usize
                )
            ),
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                }}

                Ok(())
            }}
        }}

        impl ::fidl_next::EncodableOption for Box<{name}> {{
            type EncodedOption<'buf> = WireOptional{name}{params};
        }}

        impl<___E> ::fidl_next::EncodeOption<___E> for Box<{name}>
        where
            ___E: ::fidl_next::Encoder + ?Sized,
            {name}: ::fidl_next::Encode<___E>,
        {{
            fn encode_option(
                this: Option<&mut Self>,
                encoder: &mut ___E,
                mut slot: ::fidl_next::Slot<'_, Self::EncodedOption<'_>>,
            ) -> Result<(), ::fidl_next::EncodeError> {{
                ::fidl_next::munge!(let WireOptional{name} {{ raw, _phantom: _ }} = slot.as_mut());

                if let Some(inner) = this {{
                    let slot = unsafe {{
                        ::fidl_next::Slot::new_unchecked(slot.as_mut_ptr().cast())
                    }};
                    ::fidl_next::Encode::encode(
                        &mut **inner,
                        encoder,
                        slot,
                    )?;
                }} else {{
                    ::fidl_next::RawWireUnion::encode_absent(raw);
                }}

                Ok(())
            }}
        }}
        "#,
    )?;

    // Write TakeFrom impl

    writeln!(
        out,
        r#"
        impl{params} ::fidl_next::TakeFrom<Wire{name}{params}> for {name} {{
            fn take_from(from: &mut Wire{name}{params}) -> Self {{
                match from.raw.ordinal() {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);
        let ord = member.ordinal;

        writeln!(
            out,
            r#"
            {ord} => Self::{member_name}(::fidl_next::TakeFrom::take_from(
                unsafe {{ from.raw.get_mut().deref_mut_unchecked() }}
            )),
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                    _ => unsafe {{ ::core::hint::unreachable_unchecked() }},
                }}
            }}
        }}

        impl{params} ::fidl_next::TakeFrom<WireOptional{name}{params}>
            for Option<Box<{name}>>
        {{
            fn take_from(from: &mut WireOptional{name}{params}) -> Self {{
                if let Some(inner) = from.as_mut() {{
                    Some(::fidl_next::TakeFrom::take_from(inner))
                }} else {{
                    None
                }}
            }}
        }}
        "#,
    )?;

    Ok(())
}
