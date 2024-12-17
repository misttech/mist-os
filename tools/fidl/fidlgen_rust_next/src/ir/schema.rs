// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use serde::Deserialize;

use super::{
    Bits, CompId, CompIdent, Const, DeclType, Enum, Library, Protocol, Struct, Table, TypeAlias,
    TypeShape, Union,
};

/// A FIDL JSON IR schema.
#[derive(Deserialize)]
pub struct Schema {
    pub name: String,
    #[serde(deserialize_with = "crate::de::index")]
    pub alias_declarations: HashMap<CompIdent, TypeAlias>,
    #[serde(deserialize_with = "crate::de::index")]
    pub bits_declarations: HashMap<CompIdent, Bits>,
    #[serde(deserialize_with = "crate::de::index")]
    pub const_declarations: HashMap<CompIdent, Const>,
    #[serde(deserialize_with = "crate::de::index")]
    pub enum_declarations: HashMap<CompIdent, Enum>,
    #[serde(deserialize_with = "crate::de::index")]
    pub protocol_declarations: HashMap<CompIdent, Protocol>,
    // pub interface_declarations: Vec<Protocol>,
    // pub service_declarations: Vec<Service>,
    #[serde(deserialize_with = "crate::de::index")]
    pub struct_declarations: HashMap<CompIdent, Struct>,
    // #[serde(deserialize_with = "crate::de::index")]
    // pub external_struct_declarations: HashMap<CompIdent, Struct>,
    #[serde(deserialize_with = "crate::de::index")]
    pub table_declarations: HashMap<CompIdent, Table>,
    #[serde(deserialize_with = "crate::de::index")]
    pub union_declarations: HashMap<CompIdent, Union>,
    pub declaration_order: Vec<CompIdent>,
    pub declarations: HashMap<CompIdent, DeclType>,
    #[serde(deserialize_with = "crate::de::index")]
    pub library_dependencies: HashMap<String, Library>,
}

impl Schema {
    pub fn get_decl_type(&self, ident: &CompId) -> Option<&DeclType> {
        let library = ident.library();
        if library == self.name {
            self.declarations.get(ident)
        } else {
            self.library_dependencies.get(library)?.declarations.get(ident).map(|decl| &decl.kind)
        }
    }

    pub fn get_type_shape(&self, ident: &CompId) -> Option<&TypeShape> {
        let library = ident.library();
        if library == self.name {
            match self.declarations.get(ident)? {
                DeclType::Struct => Some(&self.struct_declarations.get(ident)?.shape),
                DeclType::Table => Some(&self.table_declarations.get(ident)?.shape),
                DeclType::Union => Some(&self.union_declarations.get(ident)?.shape),
                // Enums don't include a type shape because we can technically get that information
                // from its underlying integer type
                DeclType::Enum => None,
                DeclType::Bits | DeclType::NewType => todo!(),
                // These aren't types and don't have type shapes
                DeclType::Alias
                | DeclType::Const
                | DeclType::Resource
                | DeclType::Overlay
                | DeclType::Protocol
                | DeclType::Service => None,
            }
        } else {
            self.library_dependencies.get(library)?.declarations.get(ident)?.shape.as_ref()
        }
    }
}
