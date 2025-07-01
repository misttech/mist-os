// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;

use super::Context;
use crate::id::IdExt;
use crate::ir::{CompId, DeclType};

pub struct IdTemplate<'a> {
    context: &'a Context,
    id: &'a CompId,
    prefix: &'a str,
}

impl<'a> IdTemplate<'a> {
    fn new(id: &'a CompId, prefix: &'a str, context: &'a Context) -> Self {
        Self { context, id, prefix }
    }

    pub fn natural(id: &'a CompId, context: &'a Context) -> Self {
        Self::new(id, "", context)
    }

    pub fn wire(id: &'a CompId, context: &'a Context) -> Self {
        Self::new(id, "Wire", context)
    }

    pub fn wire_optional(id: &'a CompId, context: &'a Context) -> Self {
        Self::new(id, "WireOptional", context)
    }
}

impl fmt::Display for IdTemplate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (lib, ty) = self.id.split();

        // Special case: zx::ObjType
        if lib == "zx" && ty.non_canonical() == "ObjType" {
            return write!(f, "::fidl_next::fuchsia::zx::ObjectType");
        }

        // Crate prefix
        if lib == self.context.schema.name {
            write!(f, "crate::")?;
        } else if lib == "zx" {
            write!(f, "::fidl_next::fuchsia::zx::")?;
        } else {
            let escaped = lib.replace('.', "_");
            write!(f, "::fidl_next_{escaped}::")?;
        }

        // Type name
        let base_name = match self.context.schema.get_decl_type(self.id).unwrap() {
            DeclType::Alias
            | DeclType::Bits
            | DeclType::Enum
            | DeclType::Struct
            | DeclType::Table
            | DeclType::Union
            | DeclType::Protocol => ty.camel(),
            DeclType::Const => ty.screaming_snake(),
            DeclType::Resource | DeclType::NewType | DeclType::Overlay | DeclType::Service => {
                todo!()
            }
        };
        let mut name = format!("{}{base_name}", self.prefix);
        if super::is_reserved(&name) {
            name.push('_');
        }

        write!(f, "{name}")?;

        Ok(())
    }
}
