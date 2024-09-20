// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::natural::emit_type;
use crate::compiler::query::IsWireStatic;
use crate::compiler::util::emit_doc_string;
use crate::compiler::Compiler;
use crate::ir::CompIdent;

pub fn emit_struct<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let s = &compiler.library.struct_declarations[ident];

    let is_wire_static = compiler.query::<IsWireStatic>(ident);

    let name = s.name.type_name();
    let wire_params = if is_wire_static { "" } else { "<'buf>" };

    // Write wire type

    emit_doc_string(out, s.attributes.doc_string())?;
    if !s.is_resource {
        writeln!(out, "#[derive(Clone)]")?;
    }
    if compiler.config.emit_debug_impls {
        writeln!(out, "#[derive(Debug)]")?;
    }
    writeln!(out, "pub struct {name} {{")?;

    for member in &s.members {
        let name = &member.name;

        write!(out, "pub {name}: ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ",")?;
    }

    writeln!(out, "}}")?;

    // Write Encode impl

    writeln!(
        out,
        r#"
        impl ::fidl::Encode for {name} {{
            type Encoded<'buf> = Wire{name}{wire_params};

            fn encode(
                &mut self,
                encoder: &mut ::fidl::encode::Encoder,
                slot: ::fidl::Slot<'_, Self::Encoded<'_>>,
            ) -> Result<(), ::fidl::encode::Error> {{
        "#,
    )?;

    // Have to do some nasty manual formatting to get destructuring to be
    // formatted correctly

    writeln!(out, "::fidl::munge! {{")?;
    writeln!(out, "            let Self::Encoded {{")?;

    for member in &s.members {
        let name = &member.name;
        writeln!(out, "                {name},")?;
    }

    writeln!(out, "            }} = slot;")?;
    writeln!(out, "        }}")?;

    for member in &s.members {
        let name = &member.name;
        write!(out, "::fidl::Encode::encode(&mut self.{name}, encoder, {name})?;")?;
    }

    writeln!(
        out,
        r#"
                Ok(())
            }}
        }}

        impl ::fidl::EncodeOption for Box<{name}> {{
            type EncodedOption<'buf> =
                ::fidl::WireBox<'buf, Wire{name}{wire_params}>;

            fn encode_option(
                this: Option<&mut Self>,
                encoder: &mut ::fidl::encode::Encoder,
                slot: ::fidl::Slot<'_, Self::EncodedOption<'_>>,
            ) -> Result<(), ::fidl::encode::Error> {{
                if let Some(inner) = this {{
                    encoder.encode(inner)?;
                    ::fidl::WireBox::encode_present(slot);
                }} else {{
                    ::fidl::WireBox::encode_absent(slot);
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
        impl{wire_params} ::fidl::TakeFrom<Wire{name}{wire_params}>
            for {name}
        {{
            fn take_from(from: &mut Wire{name}{wire_params}) -> Self {{
                Self {{
        "#,
    )?;

    for member in &s.members {
        let name = &member.name;
        writeln!(out, "{name}: ::fidl::TakeFrom::take_from(&mut from.{name}),")?;
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
