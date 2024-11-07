// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use crate::de::Index;

use super::{CompIdent, Literal, Type};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Const {
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
    pub expression: String,
    pub value: String,
    #[expect(dead_code)]
    #[serde(flatten)]
    pub kind: ConstantKind,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstantKind {
    Identifier {
        #[expect(dead_code)]
        identifier: CompIdent,
    },
    Literal {
        #[expect(dead_code)]
        literal: Literal,
    },
    BinaryOperator,
}
