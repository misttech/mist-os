// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use super::Compiler;
use crate::compiler::util::{
    emit_doc_string, prim_subtype_natural_name, prim_subtype_wire_name, IdExt as _,
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
        "#,
    )?;

    // Decode impl

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

    if b.is_strict {
        writeln!(
            out,
            r#"
            ::fidl_next::munge!(let Self {{ value }} = slot);
            let set = {natural_ty}::from(*value);
            if set & !{name}::all().bits() != 0 {{
                return Err(fidl_next::DecodeError::InvalidBits {{
                    expected: {name}::all().bits() as usize,
                    actual: set as usize,
                }});
            }}
            "#
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

    // From impl

    write!(
        out,
        r#"
        impl ::core::convert::From<{name}> for Wire{name} {{
            fn from(natural: {name}) -> Self {{
                Self {{
                    value: {wire_ty}::from(natural.bits()),
                }}
            }}
        }}
        "#
    )?;

    Ok(())
}
