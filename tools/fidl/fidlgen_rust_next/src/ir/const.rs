// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, CompIdentOrMember, Decl, DeclType, Literal, Type};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Const {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    #[serde(rename = "type")]
    pub ty: Type,
    pub value: Constant,
}

impl Decl for Const {
    fn decl_type(&self) -> DeclType {
        DeclType::Const
    }

    fn name(&self) -> &CompIdent {
        &self.name
    }

    fn attributes(&self) -> &Attributes {
        &self.attributes
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Constant {
    pub value: String,
    #[serde(flatten)]
    pub kind: ConstantKind,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstantKind {
    Identifier { identifier: CompIdentOrMember },
    Literal { literal: Literal },
    BinaryOperator,
}
