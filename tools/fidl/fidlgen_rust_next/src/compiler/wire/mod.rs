// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod r#enum;
mod r#struct;
mod table;
mod r#union;

use std::io::{Error, Write};

use crate::compiler::query::IsWireStatic;
use crate::compiler::util::{
    emit_prefixed_comp_ident, emit_wire_comp_ident, prim_subtype_wire_name,
};
use crate::compiler::Compiler;
use crate::ir::{DeclType, InternalSubtype, Type};

pub use self::r#enum::emit_enum;
pub use self::r#struct::emit_struct;
pub use self::r#union::emit_union;
pub use self::table::emit_table;

fn emit_type<W: Write>(compiler: &mut Compiler<'_>, out: &mut W, ty: &Type) -> Result<(), Error> {
    match &ty {
        Type::Array { element_type, element_count } => {
            write!(out, "[")?;
            emit_type(compiler, out, element_type)?;
            write!(out, "; {element_count}]")?;
        }
        Type::Vector { element_type, nullable, .. } => {
            if *nullable {
                write!(out, "::fidl::WireOptionalVector<'buf, ")?;
            } else {
                write!(out, "::fidl::WireVector<'buf, ")?;
            }
            emit_type(compiler, out, element_type)?;
            write!(out, ">")?;
        }
        Type::String { nullable, .. } => {
            if *nullable {
                write!(out, "::fidl::WireOptionalString<'buf>")?;
            } else {
                write!(out, "::fidl::WireString<'buf>")?;
            }
        }
        // Handle and request could eventually be unified under "resource types"
        Type::Handle { nullable, .. } => {
            if !*nullable {
                write!(out, "{}", compiler.config.resource_bindings.handle.wire_path)?;
            } else {
                write!(out, "{}", compiler.config.resource_bindings.handle.optional_wire_path)?;
            }
        }
        Type::Request { nullable, .. } => {
            if !*nullable {
                write!(out, "{}", compiler.config.resource_bindings.server_end.wire_path)?;
            } else {
                write!(out, "{}", compiler.config.resource_bindings.server_end.optional_wire_path)?;
            }
        }
        Type::Primitive { subtype } => {
            write!(out, "{}", prim_subtype_wire_name(*subtype))?;
        }
        Type::Identifier { identifier, nullable, .. } => {
            match compiler.schema.declarations[identifier] {
                DeclType::Enum => {
                    emit_wire_comp_ident(compiler, out, identifier)?;
                }
                DeclType::Table => {
                    emit_wire_comp_ident(compiler, out, identifier)?;
                    write!(out, "<'buf>")?;
                }
                DeclType::Struct => {
                    if *nullable {
                        write!(out, "::fidl::WireBox<'buf, ")?;
                    }
                    emit_wire_comp_ident(compiler, out, identifier)?;
                    if !compiler.query::<IsWireStatic>(identifier) {
                        write!(out, "<'buf>")?;
                    }
                    if *nullable {
                        write!(out, ">")?;
                    }
                }
                DeclType::Union => {
                    if *nullable {
                        emit_prefixed_comp_ident(compiler, out, identifier, "WireOptional")?
                    } else {
                        emit_wire_comp_ident(compiler, out, identifier)?;
                    }
                    write!(out, "<'buf>")?;
                }
                DeclType::Protocol => {
                    if !*nullable {
                        write!(out, "{}", compiler.config.resource_bindings.client_end.wire_path)?;
                    } else {
                        write!(
                            out,
                            "{}",
                            compiler.config.resource_bindings.client_end.optional_wire_path
                        )?;
                    }
                }
                DeclType::Alias => todo!(),
                DeclType::Bits => todo!(),
                DeclType::Const => todo!(),
                DeclType::Resource => todo!(),
                DeclType::NewType => todo!(),
                DeclType::Overlay => todo!(),
                DeclType::Service => todo!(),
            }
        }
        Type::Internal { subtype } => match subtype {
            InternalSubtype::FrameworkError => write!(out, "::fidl::FrameworkError")?,
        },
    }

    Ok(())
}

fn emit_type_check<W: Write>(
    out: &mut W,
    write_deref: impl FnOnce(&mut W) -> Result<(), Error>,
    name: &str,
    kind: &Type,
) -> Result<(), Error> {
    match kind {
        Type::Array { .. } => (),
        Type::Vector { element_count, nullable, .. } => {
            if let Some(limit) = element_count {
                write_deref(out)?;
                if *nullable {
                    writeln!(out, "if let Some({name}) = {name}.as_ref() {{")?;
                }
                writeln!(
                    out,
                    r#"
                if {name}.len() > {limit} {{
                    return Err(::fidl::DecodeError::VectorTooLong {{
                        size: {name}.len() as u64,
                        limit: {limit},
                    }});
                }}
                "#,
                )?;
                if *nullable {
                    writeln!(out, "}}")?;
                }
            }
        }
        Type::String { element_count, nullable } => {
            if let Some(limit) = element_count {
                write_deref(out)?;
                if *nullable {
                    writeln!(out, "if let Some({name}) = {name}.as_ref() {{")?;
                }
                writeln!(
                    out,
                    r#"
                if {name}.len() > {limit} {{
                    return Err(::fidl::DecodeError::VectorTooLong {{
                        size: {name}.len() as u64,
                        limit: {limit},
                    }});
                }}
                "#,
                )?;
                if *nullable {
                    writeln!(out, "}}")?;
                }
            }
        }
        Type::Handle { .. }
        | Type::Request { .. }
        | Type::Primitive { .. }
        | Type::Identifier { .. } => (),
        Type::Internal { subtype } => match subtype {
            InternalSubtype::FrameworkError => (),
        },
    }

    Ok(())
}
