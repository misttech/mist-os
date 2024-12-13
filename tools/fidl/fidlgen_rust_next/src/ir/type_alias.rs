// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use crate::de::Index;

use super::{Attributes, CompIdent, Type};

#[derive(Clone, Debug, Deserialize)]
pub struct TypeAlias {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    #[serde(rename = "type")]
    pub ty: Type,
}

impl Index for TypeAlias {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
    }
}
