// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::{Attributes, CompIdent, Decl, DeclType, Ident, Type};

#[derive(Clone, Debug, Deserialize)]
pub struct Service {
    #[serde(flatten)]
    pub attributes: Attributes,
    pub name: CompIdent,
    pub members: Vec<ServiceMember>,
}

impl Decl for Service {
    fn decl_type(&self) -> DeclType {
        DeclType::Service
    }

    fn name(&self) -> &CompIdent {
        &self.name
    }

    fn attributes(&self) -> &Attributes {
        &self.attributes
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
