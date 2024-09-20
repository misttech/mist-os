// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use crate::ir::{CompIdent, HandleRights, HandleSubtype, ObjectType, PrimSubtype};

#[derive(Clone, Debug, Deserialize)]
pub struct Type {
    #[serde(flatten)]
    pub kind: TypeKind,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
        #[allow(dead_code)]
        obj_type: ObjectType,
        #[allow(dead_code)]
        rights: HandleRights,
        #[allow(dead_code)]
        subtype: HandleSubtype,
        #[allow(dead_code)]
        resource_identifier: String,
    },
    Request {
        #[allow(dead_code)]
        nullable: bool,
        #[allow(dead_code)]
        subtype: CompIdent,
        #[allow(dead_code)]
        protocol_transport: String,
    },
    Primitive {
        subtype: PrimSubtype,
    },
    Identifier {
        identifier: CompIdent,
        nullable: bool,
        #[allow(dead_code)]
        #[serde(default)]
        protocol_transport: String,
    },
}
