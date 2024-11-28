// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use crate::de::Index;

use super::{Attributes, CompIdent, CompIdentOrMember, Literal, Type};

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

impl Index for Const {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
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
