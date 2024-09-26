// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::constant::Constant;
use super::{Attributes, CompIdent, IntType};
use crate::de::Index;

#[derive(Clone, Debug, Deserialize)]
pub struct Enum {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub members: Vec<EnumMember>,
    pub name: CompIdent,
    #[serde(rename = "strict")]
    pub is_strict: bool,
    #[serde(rename = "type")]
    pub ty: IntType,
}

impl Index for Enum {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct EnumMember {
    #[allow(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: String,
    #[serde(rename = "value")]
    pub constant: Constant,
}
