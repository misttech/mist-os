// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::compiler::Compiler;
use crate::ir::{Enum, Struct, Table, Type, TypeKind, Union};

#[derive(Default)]
pub struct Properties {
    is_wire_static: Option<bool>,
}

pub trait Property {
    type Type: Copy;

    fn select(properties: &mut Properties) -> &mut Option<Self::Type>;

    fn calculate_struct(compiler: &mut Compiler<'_>, s: &Struct) -> Self::Type;
    fn calculate_table(compiler: &mut Compiler<'_>, t: &Table) -> Self::Type;
    fn calculate_enum(compiler: &mut Compiler<'_>, e: &Enum) -> Self::Type;
    fn calculate_union(compiler: &mut Compiler<'_>, u: &Union) -> Self::Type;
    fn calculate_type(compiler: &mut Compiler<'_>, ty: &Type) -> Self::Type;
}

pub struct IsWireStatic;

impl Property for IsWireStatic {
    type Type = bool;

    fn select(properties: &mut Properties) -> &mut Option<Self::Type> {
        &mut properties.is_wire_static
    }

    fn calculate_struct(compiler: &mut Compiler<'_>, s: &Struct) -> Self::Type {
        for member in &s.members {
            if !Self::calculate_type(compiler, &member.ty) {
                return false;
            }
        }

        true
    }

    fn calculate_table(_: &mut Compiler<'_>, _: &Table) -> Self::Type {
        false
    }

    fn calculate_enum(_: &mut Compiler<'_>, _: &Enum) -> Self::Type {
        true
    }

    fn calculate_union(_: &mut Compiler<'_>, _: &Union) -> Self::Type {
        false
    }

    fn calculate_type(compiler: &mut Compiler<'_>, ty: &Type) -> Self::Type {
        match &ty.kind {
            TypeKind::Primitive { .. } | TypeKind::Handle { .. } => true,
            TypeKind::Vector { .. } | TypeKind::String { .. } => false,
            TypeKind::Array { element_type, .. } => Self::calculate_type(compiler, element_type),
            TypeKind::Identifier { identifier, nullable, .. } => {
                !nullable && compiler.query::<Self>(identifier)
            }
            TypeKind::Request { .. } => todo!(),
        }
    }
}
