// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::IdExt as _;
use crate::compiler::Compiler;
use crate::ir::CompId;

use super::emit_type;

pub fn emit_alias<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompId,
) -> Result<(), Error> {
    let a = &compiler.schema.alias_declarations[ident];

    let name = a.name.decl_name().camel();
    write!(out, "pub type {name} = ")?;
    emit_type(compiler, out, &a.ty)?;
    writeln!(out, ";")?;

    Ok(())
}
