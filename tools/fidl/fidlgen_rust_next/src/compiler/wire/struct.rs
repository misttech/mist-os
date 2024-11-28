// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::{emit_doc_string, IdExt as _};
use crate::compiler::wire::{emit_type, emit_type_check};
use crate::compiler::Compiler;
use crate::ir::CompId;

pub fn emit_struct<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    let s = &compiler.schema.struct_declarations[ident];

    let is_static = s.shape.max_out_of_line == 0;

    let name = s.name.decl_name().camel();
    let params = if is_static { "" } else { "<'buf>" };

    // Write wire struct

    emit_doc_string(out, s.attributes.doc_string())?;
    if is_static && !s.is_resource {
        writeln!(out, "#[derive(Clone)]")?;
    }
    if compiler.config.emit_debug_impls {
        writeln!(out, "#[derive(Debug)]")?;
    }
    writeln!(
        out,
        r#"
        #[repr(C)]
        pub struct Wire{name}{params} {{
        "#,
    )?;

    for member in &s.members {
        let member_name = member.name.snake();

        write!(out, "pub {member_name}: ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ",")?;
    }

    writeln!(out, "}}")?;

    // Write decode impl

    let decode_params = if is_static { "" } else { "'buf, " };
    writeln!(
        out,
        r#"
        unsafe impl<{decode_params}___D> ::fidl_next::Decode<___D> for Wire{name}{params}
        where
            ___D: ?Sized,
        "#,
    )?;

    for member in &s.members {
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
        "#,
    )?;

    // Have to do some nasty manual formatting to get destructuring to be
    // formatted correctly

    writeln!(out, "::fidl_next::munge! {{")?;
    writeln!(out, "            let Self {{")?;

    for member in &s.members {
        let member_name = member.name.snake();
        writeln!(out, "                mut {member_name},")?;
    }

    writeln!(out, "            }} = slot;")?;
    writeln!(out, "        }}")?;

    for member in &s.members {
        let member_name = member.name.snake();
        write!(out, "::fidl_next::Decode::decode({member_name}.as_mut(), decoder)?;")?;
        emit_type_check(
            out,
            |out| {
                writeln!(out, "let {member_name} = unsafe {{ {member_name}.deref_unchecked() }};")
            },
            &member_name,
            &member.ty,
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

    Ok(())
}
