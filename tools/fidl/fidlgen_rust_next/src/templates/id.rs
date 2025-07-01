// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;

use crate::id::IdExt;
use crate::ir::{CompId, DeclType, Schema};

pub struct IdTemplate<'a> {
    schema: &'a Schema,
    id: &'a CompId,
    prefix: &'a str,
}

impl<'a> IdTemplate<'a> {
    fn new(schema: &'a Schema, id: &'a CompId, prefix: &'a str) -> Self {
        Self { schema, id, prefix }
    }

    pub fn natural(schema: &'a Schema, id: &'a CompId) -> Self {
        Self::new(schema, id, "")
    }

    pub fn wire(schema: &'a Schema, id: &'a CompId) -> Self {
        Self::new(schema, id, "Wire")
    }

    pub fn wire_optional(schema: &'a Schema, id: &'a CompId) -> Self {
        Self::new(schema, id, "WireOptional")
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
        if lib == self.schema.name {
            write!(f, "crate::")?;
        } else if lib == "zx" {
            write!(f, "::fidl_next::fuchsia::zx::")?;
        } else {
            let escaped = lib.replace('.', "_");
            write!(f, "::fidl_next_{escaped}::")?;
        }

        // Type name
        let base_name = match self.schema.get_decl_type(self.id).unwrap() {
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
