// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::r#type::Type;
use super::{Attributes, CompIdent, Constant, Ident};
use crate::de::Index;

#[derive(Clone, Debug, Deserialize)]
pub struct Bits {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    pub members: Vec<BitsMember>,
    #[serde(rename = "strict")]
    pub is_strict: bool,
    #[serde(rename = "type")]
    pub ty: Type,
}

impl Index for Bits {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct BitsMember {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    pub value: Constant,
}
