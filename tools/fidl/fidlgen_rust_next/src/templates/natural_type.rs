// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;

use super::{Context, Contextual};
use crate::ir::{EndpointRole, InternalSubtype, Type, TypeKind};

pub struct NaturalTypeTemplate<'a> {
    context: Context<'a>,
    ty: &'a Type,
}

impl<'a> NaturalTypeTemplate<'a> {
    pub fn new(ty: &'a Type, context: Context<'a>) -> Self {
        Self { context, ty }
    }
}

impl<'a> Contextual<'a> for NaturalTypeTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}

impl fmt::Display for NaturalTypeTemplate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.ty.kind {
            TypeKind::Array { element_type, element_count } => {
                let natural_ty = Self::new(element_type, self.context);
                write!(f, "[{natural_ty}; {element_count}]")?;
            }
            TypeKind::Vector { element_type, nullable, .. } => {
                let natural_ty = Self::new(element_type, self.context);
                if *nullable {
                    write!(f, "Option<Vec<{natural_ty}>>")?;
                } else {
                    write!(f, "Vec<{natural_ty}>")?;
                }
            }
            TypeKind::String { nullable, .. } => {
                if *nullable {
                    write!(f, "Option<String>")?;
                } else {
                    write!(f, "String")?;
                }
            }
            TypeKind::Handle { nullable, .. } => {
                let handle_ty = &self.resource_bindings().handle.natural_path;
                if *nullable {
                    write!(f, "Option<{handle_ty}>")?;
                } else {
                    write!(f, "{handle_ty}")?;
                }
            }
            TypeKind::Endpoint { nullable, role, protocol, .. } => {
                let role = match role {
                    EndpointRole::Client => "::fidl_next::ClientEnd",
                    EndpointRole::Server => "::fidl_next::ServerEnd",
                };
                let protocol_id = self.natural_id(protocol);
                if *nullable {
                    write!(
                        f,
                        "Option<{role}<{protocol_id}, {}>>",
                        self.resource_bindings().channel.natural_path
                    )?;
                } else {
                    write!(
                        f,
                        "{role}<{protocol_id}, {}>",
                        self.resource_bindings().channel.natural_path
                    )?;
                }
            }
            TypeKind::Primitive { subtype } => {
                write!(f, "{}", self.natural_prim(subtype))?;
            }
            TypeKind::Identifier { identifier, nullable, .. } => {
                let natural_id = self.natural_id(identifier);
                if *nullable {
                    write!(f, "Option<Box<{natural_id}>>")?;
                } else {
                    write!(f, "{natural_id}")?;
                }
            }
            TypeKind::Internal { subtype } => match subtype {
                InternalSubtype::FrameworkError => {
                    write!(f, "::fidl_next::FrameworkError")?;
                }
            },
        }

        Ok(())
    }
}
