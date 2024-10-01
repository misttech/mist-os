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
        impl ::fidl::Encodable for {name} {{
            type Encoded<'buf> = Wire{name}<'buf>;
        }}

        impl<___E> ::fidl::Encode<___E> for {name}
        where
            ___E: ::fidl::Encoder + ?Sized,
        "#,
    )?;

    for member in &u.members {
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ": ::fidl::Encode<___E>,")?;
    }

    writeln!(
        out,
        r#"
        {{
            fn encode(
                &mut self,
                encoder: &mut ___E,
                slot: ::fidl::Slot<'_, Self::Encoded<'_>>,
            ) -> Result<(), ::fidl::EncodeError> {{
                ::fidl::munge!(let Wire{name} {{ raw }} = slot);

                match self {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);
        let ord = member.ordinal;

        writeln!(out, "Self::{member_name}(value) => ::fidl::RawWireUnion::encode_as::<___E, ",)?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(value, {ord}, encoder, raw)?,")?;
    }

    if !u.is_strict {
        writeln!(
            out,
            r#"
            Self::Unknown(ordinal) => return Err(
                ::fidl::EncodeError::UnknownUnionOrdinal(
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

        impl ::fidl::EncodableOption for Box<{name}> {{
            type EncodedOption<'buf> = WireOptional{name}<'buf>;
        }}

        impl<___E> ::fidl::EncodeOption<___E> for Box<{name}>
        where
            ___E: ::fidl::Encoder + ?Sized,
            {name}: ::fidl::Encode<___E>,
        {{
            fn encode_option(
                this: Option<&mut Self>,
                encoder: &mut ___E,
                mut slot: ::fidl::Slot<'_, Self::EncodedOption<'_>>,
            ) -> Result<(), ::fidl::EncodeError> {{
                ::fidl::munge!(let WireOptional{name} {{ raw }} = slot.as_mut());

                if let Some(inner) = this {{
                    let slot = unsafe {{
                        ::fidl::Slot::new_unchecked(slot.as_mut_ptr().cast())
                    }};
                    ::fidl::Encode::encode(
                        &mut **inner,
                        encoder,
                        slot,
                    )?;
                }} else {{
                    ::fidl::RawWireUnion::encode_absent(raw);
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
        impl<'buf> ::fidl::TakeFrom<Wire{name}<'buf>> for {name} {{
            fn take_from(from: &mut Wire{name}<'buf>) -> Self {{
                match from.raw.ordinal() {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);
        let ord = member.ordinal;

        writeln!(
            out,
            r#"
            {ord} => Self::{member_name}(::fidl::TakeFrom::take_from(
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

        impl<'buf> ::fidl::TakeFrom<WireOptional{name}<'buf>>
            for Option<Box<{name}>>
        {{
            fn take_from(from: &mut WireOptional{name}<'buf>) -> Self {{
                if let Some(inner) = from.as_mut() {{
                    Some(::fidl::TakeFrom::take_from(inner))
                }} else {{
                    None
                }}
            }}
        }}
        "#,
    )?;

    Ok(())
}
