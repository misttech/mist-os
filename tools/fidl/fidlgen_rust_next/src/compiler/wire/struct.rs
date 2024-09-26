// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::query::IsWireStatic;
use crate::compiler::util::emit_doc_string;
use crate::compiler::wire::{emit_type, emit_type_check};
use crate::compiler::Compiler;
use crate::ir::CompIdent;

pub fn emit_struct<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let s = &compiler.library.struct_declarations[ident];

    let is_static = compiler.query::<IsWireStatic>(ident);

    let name = s.name.type_name();
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
        let name = &member.name;

        write!(out, "pub {name}: ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ",")?;
    }

    writeln!(out, "}}")?;

    // Write decode impl

    writeln!(
        out,
        r#"
        unsafe impl<'buf> ::fidl::Decode<'buf> for Wire{name}{params} {{
            fn decode(
                slot: ::fidl::Slot<'_, Self>,
                decoder: &mut ::fidl::decode::Decoder<'buf>,
            ) -> Result<(), ::fidl::decode::Error> {{
        "#,
    )?;

    // Have to do some nasty manual formatting to get destructuring to be
    // formatted correctly

    writeln!(out, "::fidl::munge! {{")?;
    writeln!(out, "            let Self {{")?;

    for member in &s.members {
        let name = &member.name;
        writeln!(out, "                mut {name},")?;
    }

    writeln!(out, "            }} = slot;")?;
    writeln!(out, "        }}")?;

    for member in &s.members {
        let name = &member.name;
        write!(out, "::fidl::Decode::<'buf>::decode({name}.as_mut(), decoder)?;")?;
        emit_type_check(
            out,
            |out| writeln!(out, "let {name} = unsafe {{ {name}.deref_unchecked() }};"),
            name,
            &member.ty.kind,
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
