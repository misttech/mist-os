// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use crate::ir::{CompIdent, HandleRights, HandleSubtype, PrimSubtype, TypeShape};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointRole {
    Client,
    Server,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Type {
    #[serde(flatten)]
    pub kind: TypeKind,
    #[serde(rename = "type_shape_v2")]
    pub shape: TypeShape,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind_v2", rename_all = "snake_case")]
pub enum TypeKind {
    Array {
        element_type: Box<Type>,
        element_count: u32,
    },
    Vector {
        element_type: Box<Type>,
        #[serde(default, rename = "maybe_element_count")]
        element_count: Option<u32>,
        nullable: bool,
    },
    String {
        #[serde(default, rename = "maybe_element_count")]
        element_count: Option<u32>,
        nullable: bool,
    },
    Handle {
        nullable: bool,
        #[expect(dead_code)]
        rights: HandleRights,
        #[expect(dead_code)]
        subtype: HandleSubtype,
        #[expect(dead_code)]
        resource_identifier: String,
    },
    Endpoint {
        nullable: bool,
        role: EndpointRole,
        #[expect(dead_code)]
        protocol: CompIdent,
        #[expect(dead_code)]
        protocol_transport: String,
    },
    Primitive {
        subtype: PrimSubtype,
    },
    Identifier {
        identifier: CompIdent,
        nullable: bool,
        #[expect(dead_code)]
        #[serde(default)]
        protocol_transport: String,
    },
    Internal {
        subtype: InternalSubtype,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalSubtype {
    FrameworkError,
}
