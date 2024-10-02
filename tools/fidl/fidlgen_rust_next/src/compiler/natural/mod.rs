// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod r#enum;
mod r#struct;
mod table;
mod r#union;

use std::io::{Error, Write};

use crate::compiler::util::emit_natural_comp_ident;
use crate::compiler::Compiler;
use crate::ir::{DeclType, InternalSubtype, PrimSubtype, Type};

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
                write!(out, "Option<")?;
            }
            write!(out, "Vec<")?;
            emit_type(compiler, out, element_type)?;
            write!(out, ">")?;
            if *nullable {
                write!(out, ">")?;
            }
        }
        Type::String { nullable, .. } => {
            if *nullable {
                write!(out, "Option<")?;
            }
            write!(out, "String")?;
            if *nullable {
                write!(out, ">")?;
            }
        }
        Type::Handle { nullable, .. } => {
            if !*nullable {
                write!(out, "{}", compiler.config.resource_bindings.handle.natural_path)?;
            } else {
                write!(out, "Option<{}>", compiler.config.resource_bindings.handle.natural_path)?;
            }
        }
        Type::Request { nullable, .. } => {
            if !*nullable {
                write!(out, "{}", compiler.config.resource_bindings.server_end.natural_path)?;
            } else {
                write!(
                    out,
                    "Option<{}>",
                    compiler.config.resource_bindings.server_end.natural_path
                )?;
            }
        }
        Type::Primitive { subtype } => {
            write!(
                out,
                "{}",
                match subtype {
                    PrimSubtype::Bool => "bool",
                    PrimSubtype::Float32 => "f32",
                    PrimSubtype::Float64 => "f64",
                    PrimSubtype::Int8 => "i8",
                    PrimSubtype::Int16 => "i16",
                    PrimSubtype::Int32 => "i32",
                    PrimSubtype::Int64 => "i64",
                    PrimSubtype::Uint8 => "u8",
                    PrimSubtype::Uint16 => "u16",
                    PrimSubtype::Uint32 => "u32",
                    PrimSubtype::Uint64 => "u64",
                }
            )?;
        }
        Type::Identifier { identifier, nullable, .. } => {
            match compiler.schema.get_decl_type(identifier).unwrap() {
                DeclType::Enum | DeclType::Table => {
                    emit_natural_comp_ident(compiler, out, identifier)?;
                }
                DeclType::Struct | DeclType::Union => {
                    if *nullable {
                        write!(out, "Option<Box<")?;
                    }
                    emit_natural_comp_ident(compiler, out, identifier)?;
                    if *nullable {
                        write!(out, ">>")?;
                    }
                }
                DeclType::Alias => todo!(),
                DeclType::Bits => todo!(),
                DeclType::Const => todo!(),
                DeclType::Resource => todo!(),
                DeclType::NewType => todo!(),
                DeclType::Overlay => todo!(),
                DeclType::Protocol => {
                    write!(
                        out,
                        "Option<{}>",
                        compiler.config.resource_bindings.client_end.natural_path
                    )?;
                }
                DeclType::Service => todo!(),
            }
        }
        Type::Internal { subtype } => match subtype {
            InternalSubtype::FrameworkError => write!(out, "::fidl_next::FrameworkError")?,
        },
    }

    Ok(())
}
