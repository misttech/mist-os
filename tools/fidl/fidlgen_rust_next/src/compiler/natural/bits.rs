// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use super::Compiler;
use crate::compiler::util::{
    emit_doc_string, prim_subtype_natural_name, prim_subtype_wire_name, IdExt,
};
use crate::ir::{CompId, Type, TypeKind};

pub fn emit_bits<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    let b = &compiler.schema.bits_declarations[ident];

    let name = b.name.decl_name().camel();
    let Type { kind: TypeKind::Primitive { subtype }, .. } = &b.ty else {
        panic!("invalid non-integral primitive subtype for bits");
    };
    let natural_ty = prim_subtype_natural_name(*subtype);
    let wire_ty = prim_subtype_wire_name(*subtype);

    emit_doc_string(out, b.attributes.doc_string())?;
    writeln!(
        out,
        r#"
        ::fidl_next::bitflags! {{
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]"#,
    )?;
    if compiler.config.emit_debug_impls {
        writeln!(out, "    #[derive(Debug)]")?;
    }

    writeln!(out, "    pub struct {name}: {natural_ty} {{")?;

    for member in &b.members {
        let member_name = member.name.screaming_snake();
        let value = &member.value.value;

        emit_doc_string(out, member.attributes.doc_string())?;
        writeln!(out, "        const {member_name} = {value};")?;
    }

    if !b.is_strict {
        writeln!(out, "        const _ = !0;")?;
    }

    writeln!(
        out,
        r#"    }}
}}
        "#,
    )?;

    // Encode impl

    writeln!(
        out,
        r#"
        impl ::fidl_next::Encodable for {name} {{
            type Encoded<'buf> = Wire{name};
        }}

        impl<___E> ::fidl_next::Encode<___E> for {name}
        where
            ___E: ?Sized,
        {{
            fn encode(
                &mut self,
                _: &mut ___E,
                slot: ::fidl_next::Slot<'_, Self::Encoded<'_>>,
            ) -> Result<(), ::fidl_next::EncodeError> {{
                ::fidl_next::munge!(let Wire{name} {{ mut value }} = slot);
                *value = {wire_ty}::from(self.bits());
                Ok(())
             }}
        }}
        "#
    )?;

    // From impl

    write!(
        out,
        r#"
        impl ::core::convert::From<Wire{name}> for {name} {{
            fn from(wire: Wire{name}) -> Self {{
                Self::from_bits_retain({natural_ty}::from(wire.value))
            }}
        }}
        "#
    )?;

    // TakeFrom impl

    writeln!(
        out,
        r#"
        impl ::fidl_next::TakeFrom<Wire{name}> for {name} {{
            fn take_from(from: &mut Wire{name}) -> Self {{
                Self::from(*from)
            }}
        }}
        "#,
    )?;

    Ok(())
}
