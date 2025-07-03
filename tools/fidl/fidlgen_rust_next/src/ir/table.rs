// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::num::NonZeroI64;

use serde::Deserialize;

use super::{Attributes, CompIdent, Decl, DeclType, Ident, Type, TypeShape};

#[derive(Clone, Debug, Deserialize)]
pub struct Table {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    pub naming_context: Vec<String>,
    pub members: Vec<TableMember>,
    #[serde(rename = "resource")]
    pub is_resource: bool,
    #[serde(rename = "type_shape_v2")]
    pub shape: TypeShape,
}

impl Decl for Table {
    fn decl_type(&self) -> DeclType {
        DeclType::Table
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
pub struct TableMember {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    #[serde(rename = "type")]
    pub ty: Type,
    pub ordinal: NonZeroI64,
}
