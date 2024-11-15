// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::{emit_doc_string, IdentExt as _};
use crate::compiler::wire::{emit_type, emit_type_check};
use crate::compiler::Compiler;
use crate::ir::CompIdent;

// TODO: wire tables need a drop impl

pub fn emit_table<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let t = &compiler.schema.table_declarations[ident];

    let name = t.name.type_name().camel();

    // Write wire type

    emit_doc_string(out, t.attributes.doc_string())?;
    writeln!(
        out,
        r#"
        #[repr(C)]
        pub struct Wire{name}<'buf> {{
            table: ::fidl_next::WireTable<'buf>,
        }}
        "#,
    )?;

    // Write decode impl

    writeln!(
        out,
        r#"
        unsafe impl<'buf, ___D> ::fidl_next::Decode<___D> for Wire{name}<'buf>
        where
            ___D: ::fidl_next::Decoder<'buf> + ?Sized,
        "#,
    )?;

    for member in &t.members {
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ": ::fidl_next::Decode<___D>,")?;
    }

    writeln!(
        out,
        r#"
        {{
            fn decode(
                slot: ::fidl_next::Slot<'_, Self>,
                decoder: &mut ___D,
            ) -> Result<(), ::fidl_next::DecodeError> {{
                ::fidl_next::munge!(let Self {{ table }} = slot);

                ::fidl_next::WireTable::decode_with(
                    table,
                    decoder,
                    |ordinal, mut slot, decoder| match ordinal {{
                        0 => unsafe {{ ::core::hint::unreachable_unchecked() }},
        "#,
    )?;

    for member in &t.members {
        let member_name = member.name.snake();
        let ord = member.ordinal;
        write!(
            out,
            r#"
            {ord} => {{
                ::fidl_next::WireEnvelope::decode_as::<___D, 
            "#,
        )?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(slot.as_mut(), decoder)?;")?;
        emit_type_check(
            out,
            |out| {
                write!(
                    out,
                    r#"
                    let {member_name} = unsafe {{
                        slot.deref_unchecked().deref_unchecked::<
                    "#
                )?;
                emit_type(compiler, out, &member.ty)?;
                writeln!(out, ">() }};")
            },
            &member_name,
            &member.ty,
        )?;
        writeln!(
            out,
            r#"
                Ok(())
            }}
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
                        _ => ::fidl_next::WireEnvelope::decode_unknown(
                            slot,
                            decoder,
                        ),
                    }},
                )
            }}
        }}
        "#
    )?;

    // Write inherent impls

    writeln!(out, "impl<'buf> Wire{name}<'buf> {{")?;

    for member in &t.members {
        let ord = member.ordinal;
        let member_name = member.name.snake();

        write!(out, "pub fn {member_name}(&self) -> Option<&")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(
            out,
            r#"
            > {{
                unsafe {{
                    Some(self.table.get({ord})?.deref_unchecked())
                }}
            }}
            "#,
        )?;

        write!(out, "pub fn {member_name}_mut(&mut self) -> Option<&mut ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(
            out,
            r#"
            > {{
                unsafe {{
                    Some(self.table.get_mut({ord})?.deref_mut_unchecked())
                }}
            }}
            "#,
        )?;

        write!(out, "pub fn take_{member_name}(&mut self) -> Option<")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(
            out,
            r#"
            > {{
                unsafe {{
                    Some(self.table.get_mut({ord})?.take_unchecked())
                }}
            }}
            "#,
        )?;
    }

    writeln!(out, "}}")?;

    // Write debug impl

    if compiler.config.emit_debug_impls {
        writeln!(
            out,
            r#"
            impl ::core::fmt::Debug for Wire{name}<'_> {{
                fn fmt(
                    &self,
                    f: &mut ::core::fmt::Formatter<'_>,
                ) -> Result<(), ::core::fmt::Error> {{
                    f.debug_struct("{name}")
            "#,
        )?;

        for member in &t.members {
            let member_name = member.name.snake();
            writeln!(out, ".field(\"{member_name}\", &self.{member_name}())")?;
        }

        writeln!(
            out,
            r#"
                    .finish()
                }}
            }}
            "#,
        )?;
    }

    Ok(())
}
