// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::num::NonZeroI64;

use serde::Deserialize;

use super::r#type::Type;
use super::type_shape::TypeShape;
use super::{Attributes, CompIdent};
use crate::de::Index;

#[derive(Clone, Debug, Deserialize)]
pub struct Union {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub members: Vec<UnionMember>,
    pub name: CompIdent,
    #[serde(rename = "resource")]
    pub is_resource: bool,
    #[serde(rename = "strict")]
    pub is_strict: bool,
    #[serde(rename = "type_shape_v2")]
    pub shape: TypeShape,
}

impl Index for Union {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct UnionMember {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: String,
    pub ordinal: NonZeroI64,
    #[serde(rename = "type")]
    pub ty: Type,
}
