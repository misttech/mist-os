// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::num::NonZeroI64;

use serde::Deserialize;

use super::{Attributes, CompIdent, Decl, DeclType, Ident, Type, TypeShape};

#[derive(Clone, Debug, Deserialize)]
pub struct Union {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub members: Vec<UnionMember>,
    pub name: CompIdent,
    pub naming_context: Vec<String>,
    #[serde(rename = "resource")]
    pub is_resource: bool,
    pub is_result: bool,
    #[serde(rename = "strict")]
    pub is_strict: bool,
    #[serde(rename = "type_shape_v2")]
    pub shape: TypeShape,
}

impl Decl for Union {
    fn decl_type(&self) -> DeclType {
        DeclType::Union
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

    fn type_shape(&self) -> Option<&TypeShape> {
        Some(&self.shape)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct UnionMember {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    pub ordinal: NonZeroI64,
    #[serde(rename = "type")]
    pub ty: Type,
}
