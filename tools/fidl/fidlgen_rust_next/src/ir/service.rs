// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, Ident, Type};
use crate::de::Index;

#[derive(Clone, Debug, Deserialize)]
pub struct Service {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    pub members: Vec<ServiceMember>,
}

impl Index for Service {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        &self.name
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServiceMember {
    #[expect(dead_code)]
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: Ident,
    #[serde(rename = "type")]
    pub ty: Type,
}
