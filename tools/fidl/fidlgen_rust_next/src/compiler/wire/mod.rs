// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod alias;
mod bits;
mod r#enum;
mod r#struct;
mod table;
mod r#union;

use std::io::{Error, Write};

use crate::compiler::util::{
    emit_prefixed_comp_ident, emit_wire_comp_ident, prim_subtype_wire_name,
};
use crate::compiler::Compiler;
use crate::ir::{DeclType, EndpointRole, InternalSubtype, Type, TypeKind};

pub use self::alias::emit_alias;
pub use self::bits::emit_bits;
pub use self::r#enum::emit_enum;
pub use self::r#struct::emit_struct;
pub use self::r#union::emit_union;
pub use self::table::emit_table;

fn emit_type<W: Write>(compiler: &mut Compiler<'_>, out: &mut W, ty: &Type) -> Result<(), Error> {
    match &ty.kind {
        TypeKind::Array { element_type, element_count } => {
            write!(out, "[")?;
            emit_type(compiler, out, element_type)?;
            write!(out, "; {element_count}]")?;
        }
        TypeKind::Vector { element_type, nullable, .. } => {
            if *nullable {
                write!(out, "::fidl_next::WireOptionalVector<'buf, ")?;
            } else {
                write!(out, "::fidl_next::WireVector<'buf, ")?;
            }
            emit_type(compiler, out, element_type)?;
            write!(out, ">")?;
        }
        TypeKind::String { nullable, .. } => {
            if *nullable {
                write!(out, "::fidl_next::WireOptionalString<'buf>")?;
            } else {
                write!(out, "::fidl_next::WireString<'buf>")?;
            }
        }
        // Handle and request could eventually be unified under "resource types"
        TypeKind::Handle { nullable, .. } => {
            if !*nullable {
                write!(out, "{}", compiler.config.resource_bindings.handle.wire_path)?;
            } else {
                write!(out, "{}", compiler.config.resource_bindings.handle.optional_wire_path)?;
            }
        }
        TypeKind::Endpoint { nullable, role, .. } => {
            write!(out, "::fidl_next::EndpointResource<")?;
            if !*nullable {
                write!(out, "{}", compiler.config.resource_bindings.handle.wire_path)?;
            } else {
                write!(out, "{}", compiler.config.resource_bindings.handle.optional_wire_path)?;
            }
            write!(
                out,
                ", {}>",
                match role {
                    EndpointRole::Client => "::fidl_next::ClientEndpoint",
                    EndpointRole::Server => "::fidl_next::ServerEndpoint",
                },
            )?;
        }
        TypeKind::Primitive { subtype } => {
            write!(out, "{}", prim_subtype_wire_name(*subtype))?;
        }
        TypeKind::Identifier { identifier, nullable, .. } => {
            match compiler.schema.get_decl_type(identifier).unwrap() {
                DeclType::Bits | DeclType::Enum => {
                    emit_wire_comp_ident(compiler, out, identifier)?;
                }
                DeclType::Table => {
                    emit_wire_comp_ident(compiler, out, identifier)?;
                    write!(out, "<'buf>")?;
                }
                DeclType::Struct => {
                    if *nullable {
                        write!(out, "::fidl_next::WireBox<'buf, ")?;
                    }
                    emit_wire_comp_ident(compiler, out, identifier)?;

                    if let Some(shape) = compiler.schema.get_type_shape(identifier) {
                        if shape.max_out_of_line != 0 {
                            write!(out, "<'buf>")?;
                        }
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
                    if ty.shape.max_out_of_line != 0 {
                        write!(out, "<'buf>")?;
                    }
                }
                DeclType::Protocol => todo!(),
                DeclType::Alias => todo!(),
                DeclType::Const => todo!(),
                DeclType::Resource => {
                    // All resources are currently treated like handles
                    if !*nullable {
                        write!(out, "{}", compiler.config.resource_bindings.handle.wire_path)?;
                    } else {
                        write!(
                            out,
                            "{}",
                            compiler.config.resource_bindings.handle.optional_wire_path
                        )?;
                    }
                }
                DeclType::NewType => todo!(),
                DeclType::Overlay => todo!(),
                DeclType::Service => todo!(),
            }
        }
        TypeKind::Internal { subtype } => match subtype {
            InternalSubtype::FrameworkError => write!(out, "::fidl_next::FrameworkError")?,
        },
    }

    Ok(())
}

fn emit_type_check<W: Write>(
    out: &mut W,
    write_deref: impl FnOnce(&mut W) -> Result<(), Error>,
    name: &str,
    ty: &Type,
) -> Result<(), Error> {
    match &ty.kind {
        TypeKind::Array { .. } => (),
        TypeKind::Vector { element_count, nullable, .. } => {
            if let Some(limit) = element_count {
                write_deref(out)?;
                if *nullable {
                    writeln!(out, "if let Some({name}) = {name}.as_ref() {{")?;
                }
                writeln!(
                    out,
                    r#"
                if {name}.len() > {limit} {{
                    return Err(::fidl_next::DecodeError::VectorTooLong {{
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
        TypeKind::String { element_count, nullable } => {
            if let Some(limit) = element_count {
                write_deref(out)?;
                if *nullable {
                    writeln!(out, "if let Some({name}) = {name}.as_ref() {{")?;
                }
                writeln!(
                    out,
                    r#"
                if {name}.len() > {limit} {{
                    return Err(::fidl_next::DecodeError::VectorTooLong {{
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
        TypeKind::Handle { .. }
        | TypeKind::Endpoint { .. }
        | TypeKind::Primitive { .. }
        | TypeKind::Identifier { .. } => (),
        TypeKind::Internal { subtype } => match subtype {
            InternalSubtype::FrameworkError => (),
        },
    }

    Ok(())
}
