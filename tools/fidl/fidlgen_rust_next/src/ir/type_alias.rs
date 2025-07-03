// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, Decl, DeclType, Type};

#[derive(Clone, Debug, Deserialize)]
pub struct TypeAlias {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    #[serde(rename = "type")]
    pub ty: Type,
}

impl Decl for TypeAlias {
    fn decl_type(&self) -> DeclType {
        DeclType::Bits
    }

    fn name(&self) -> &CompIdent {
        &self.name
    }

    fn attributes(&self) -> &Attributes {
        &self.attributes
    }

    fn naming_context(&self) -> Option<&[String]> {
        None
    }
}
