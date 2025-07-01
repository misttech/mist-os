// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, Decl, DeclType, Ident, Type, TypeShape};

#[derive(Clone, Debug, Deserialize)]
pub struct Struct {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    pub naming_context: Vec<String>,
    pub members: Vec<StructMember>,
    #[serde(rename = "resource")]
    pub is_resource: bool,
    #[serde(rename = "type_shape_v2")]
    pub shape: TypeShape,
    pub is_empty_success_struct: bool,
}

impl Decl for Struct {
    fn decl_type(&self) -> DeclType {
        DeclType::Struct
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
pub struct StructMember {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    #[serde(rename = "type")]
    pub ty: Type,
    #[serde(rename = "field_shape_v2")]
    pub field_shape: FieldShape,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FieldShape {
    pub offset: u32,
    pub padding: u32,
}
