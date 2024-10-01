// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::natural::emit_type;
use crate::compiler::util::emit_doc_string;
use crate::compiler::Compiler;
use crate::ir::CompIdent;

pub fn emit_table<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let t = &compiler.schema.table_declarations[ident];

    let name = t.name.type_name();

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
        let name = &member.name;

        write!(out, "pub {name}: Option<")?;
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
        let name = &member.name;
        let ord = member.ordinal;

        writeln!(out, "if self.{name}.is_some() {{ return {ord}; }}")?;
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
        impl ::fidl::Encodable for {name} {{
            type Encoded<'buf> = Wire{name}<'buf>;
        }}

        impl<___E> ::fidl::Encode<___E> for {name}
        where
            ___E: ::fidl::Encoder + ?Sized,
        "#,
    )?;

    for member in &t.members {
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
                ::fidl::munge!(let Wire{name} {{ table }} = slot);

                let max_ord = self.__max_ordinal();

                let mut backing = ::core::mem::MaybeUninit::<
                    ::fidl::WireEnvelope<'_>
                >::uninit();
                let mut preallocated = ::fidl::EncoderExt::preallocate::<
                    ::fidl::WireEnvelope<'_>
                >(encoder, max_ord);

                for i in 1..=max_ord {{
                    let mut slot = ::fidl::Slot::new(&mut backing);
                    match i {{
        "#,
    )?;

    for member in t.members.iter().rev() {
        let name = &member.name;
        let ord = member.ordinal;

        writeln!(
            out,
            r#"
            {ord} => if let Some({name}) = &mut self.{name} {{
                ::fidl::WireEnvelope::encode_value(
                    {name},
                    preallocated.encoder,
                    slot.as_mut(),
                )?;
            }} else {{
                ::fidl::WireEnvelope::encode_zero(slot.as_mut())
            }}
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                        _ => ::fidl::WireEnvelope::encode_zero(slot.as_mut()),
                    }}
                    preallocated.write_next(slot);
                }}

                ::fidl::WireTable::encode_len(table, max_ord);

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
                Self {{
        "#,
    )?;

    for member in t.members.iter().rev() {
        let name = &member.name;

        writeln!(out, "{name}: from.{name}_mut().map(::fidl::TakeFrom::take_from),",)?;
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
