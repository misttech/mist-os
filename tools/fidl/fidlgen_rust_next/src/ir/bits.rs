// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, Constant, Decl, DeclType, Ident, Type};

#[derive(Clone, Debug, Deserialize)]
pub struct Bits {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    pub naming_context: Vec<String>,
    pub members: Vec<BitsMember>,
    #[serde(rename = "strict")]
    pub is_strict: bool,
    #[serde(rename = "type")]
    pub ty: Type,
}

impl Decl for Bits {
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
        Some(&self.naming_context)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct BitsMember {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    pub value: Constant,
}
