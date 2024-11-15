// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::IdentExt as _;
use crate::compiler::Compiler;
use crate::ir::CompIdent;

use super::emit_type;

pub fn emit_alias<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let a = &compiler.schema.alias_declarations[ident];

    let name = a.name.type_name().camel();
    write!(out, "pub type {name} = ")?;
    emit_type(compiler, out, &a.ty)?;
    writeln!(out, ";")?;

    Ok(())
}
