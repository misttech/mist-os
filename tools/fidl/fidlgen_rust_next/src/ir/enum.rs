// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, Constant, Decl, DeclType, Ident, IntType};

#[derive(Clone, Debug, Deserialize)]
pub struct Enum {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub members: Vec<EnumMember>,
    pub name: CompIdent,
    pub naming_context: Vec<String>,
    #[serde(rename = "strict")]
    pub is_strict: bool,
    #[serde(rename = "type")]
    pub ty: IntType,
}

impl Decl for Enum {
    fn decl_type(&self) -> DeclType {
        DeclType::Enum
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
pub struct EnumMember {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    pub value: Constant,
}
