// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::emit_natural_comp_ident;
use crate::compiler::Compiler;
use crate::ir::{CompIdent, Type};

use super::primitive_subtype;

fn emit_const_type<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ty: &Type,
) -> Result<(), Error> {
    match ty {
        Type::Array { .. }
        | Type::Vector { .. }
        | Type::Handle { .. }
        | Type::Endpoint { .. }
        | Type::Internal { .. } => panic!("invalid constant type"),
        Type::String { .. } => write!(out, "&str"),
        Type::Primitive { subtype, .. } => write!(out, "{}", primitive_subtype(subtype)),
        Type::Identifier { identifier, .. } => emit_natural_comp_ident(compiler, out, identifier),
    }
}

pub fn emit_constant<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let c = &compiler.schema.const_declarations[ident];

    write!(out, "pub const {}: ", c.name.type_name(),)?;
    emit_const_type(compiler, out, &c.ty)?;
    write!(out, " = {};", &c.value.expression)?;

    Ok(())
}
