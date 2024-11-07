// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::snake_to_camel;
use crate::compiler::Compiler;
use crate::ir::CompIdent;

use super::emit_type;

pub fn emit_alias<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let a = &compiler.schema.alias_declarations[ident];

    let name = snake_to_camel(a.name.type_name());
    write!(out, "pub type Wire{}", name)?;

    // TODO: this doesn't always emit the correct lifetime parameters
    if a.ty.shape.max_out_of_line != 0 {
        write!(out, "<'buf>")?;
    }

    write!(out, " = ")?;
    emit_type(compiler, out, &a.ty)?;
    writeln!(out, ";")?;

    Ok(())
}
