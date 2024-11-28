// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::natural::emit_type;
use crate::compiler::util::{emit_doc_string, IdExt as _};
use crate::compiler::Compiler;
use crate::ir::CompId;

pub fn emit_table<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    let t = &compiler.schema.table_declarations[ident];

    let name = t.name.decl_name().camel();

    // Write natural type

    emit_doc_string(out, t.attributes.doc_string())?;
    if !t.is_resource {
        writeln!(out, "#[derive(Clone)]")?;
    }
    if compiler.config.emit_debug_impls {
        writeln!(out, "#[derive(Debug)]")?;
    }
    writeln!(out, "pub struct {name} {{")?;

    for member in &t.members {
        let member_name = member.name.snake();

        write!(out, "pub {member_name}: Option<")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">,")?;
    }

    writeln!(out, "}}")?;

    // Write inherent impl

    writeln!(
        out,
        r#"
        impl {name} {{
            fn __max_ordinal(&self) -> usize {{
        "#,
    )?;

    for member in t.members.iter().rev() {
        let member_name = member.name.snake();
        let ord = member.ordinal;

        writeln!(out, "if self.{member_name}.is_some() {{ return {ord}; }}")?;
    }

    writeln!(
        out,
        r#"
                0
            }}
        }}
        "#,
    )?;

    // Write Encode impl

    writeln!(
        out,
        r#"
        impl ::fidl_next::Encodable for {name} {{
            type Encoded<'buf> = Wire{name}<'buf>;
        }}

        impl<___E> ::fidl_next::Encode<___E> for {name}
        where
            ___E: ::fidl_next::Encoder + ?Sized,
        "#,
    )?;

    for member in &t.members {
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
                ::fidl_next::munge!(let Wire{name} {{ table }} = slot);

                let max_ord = self.__max_ordinal();

                let mut backing = ::core::mem::MaybeUninit::<
                    ::fidl_next::WireEnvelope
                >::uninit();
                let mut preallocated = ::fidl_next::EncoderExt::preallocate::<
                    ::fidl_next::WireEnvelope
                >(encoder, max_ord);

                for i in 1..=max_ord {{
                    let mut slot = ::fidl_next::Slot::new(&mut backing);
                    match i {{
        "#,
    )?;

    for member in t.members.iter().rev() {
        let member_name = member.name.snake();
        let ord = member.ordinal;

        writeln!(
            out,
            r#"
            {ord} => if let Some({member_name}) = &mut self.{member_name} {{
                ::fidl_next::WireEnvelope::encode_value(
                    {member_name},
                    preallocated.encoder,
                    slot.as_mut(),
                )?;
            }} else {{
                ::fidl_next::WireEnvelope::encode_zero(slot.as_mut())
            }}
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                        _ => ::fidl_next::WireEnvelope::encode_zero(slot.as_mut()),
                    }}
                    preallocated.write_next(slot);
                }}

                ::fidl_next::WireTable::encode_len(table, max_ord);

                Ok(())
            }}
        }}
        "#,
    )?;

    // Write TakeFrom impl

    writeln!(
        out,
        r#"
        impl<'buf> ::fidl_next::TakeFrom<Wire{name}<'buf>> for {name} {{
            fn take_from(from: &mut Wire{name}<'buf>) -> Self {{
                Self {{
        "#,
    )?;

    for member in t.members.iter().rev() {
        let member_name = member.name.snake();

        writeln!(
            out,
            "{member_name}: from.{member_name}_mut().map(::fidl_next::TakeFrom::take_from),"
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
