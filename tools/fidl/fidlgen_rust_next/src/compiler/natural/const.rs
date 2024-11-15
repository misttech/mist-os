// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::{
    emit_doc_string, emit_natural_comp_ident, emit_natural_constant, IdExt as _,
};
use crate::compiler::Compiler;
use crate::ir::{CompId, TypeKind};

use super::primitive_subtype;

pub fn emit_const<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    let c = &compiler.schema.const_declarations[ident];

    emit_doc_string(out, c.attributes.doc_string())?;

    let name = c.name.decl_name().screaming_snake();

    write!(out, "pub const {name}: ")?;
    match &c.ty.kind {
        TypeKind::String { .. } => write!(out, "&str = \"{}\";", c.value.value.escape_default())?,
        TypeKind::Primitive { subtype, .. } => {
            write!(out, "{} = {};", primitive_subtype(subtype), c.value.value)?
        }
        TypeKind::Identifier { identifier, .. } => {
            emit_natural_comp_ident(compiler, out, identifier)?;
            write!(out, " = ")?;
            emit_natural_constant(compiler, out, &c.value, &c.ty)?;
            writeln!(out, ";")?;
        }
        _ => panic!("invalid constant type"),
    }

    Ok(())
}
