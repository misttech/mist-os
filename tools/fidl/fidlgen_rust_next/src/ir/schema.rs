// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use serde::Deserialize;

use super::{
    Bits, CompId, CompIdent, Const, Decl, DeclType, Enum, Library, Protocol, Service, Struct,
    Table, TypeAlias, TypeShape, Union,
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
    #[serde(deserialize_with = "crate::de::index")]
    pub service_declarations: HashMap<CompIdent, Service>,
    #[serde(deserialize_with = "crate::de::index")]
    pub struct_declarations: HashMap<CompIdent, Struct>,
    #[serde(deserialize_with = "crate::de::index")]
    pub external_struct_declarations: HashMap<CompIdent, Struct>,
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
    pub fn get_local_decl(&self, ident: &CompId) -> Option<&dyn Decl> {
        match self.declarations.get(ident)? {
            DeclType::Alias => Some(self.alias_declarations.get(ident)?),
            DeclType::Bits => Some(self.bits_declarations.get(ident)?),
            DeclType::Const => Some(self.const_declarations.get(ident)?),
            DeclType::Enum => Some(self.enum_declarations.get(ident)?),
            DeclType::Protocol => Some(self.protocol_declarations.get(ident)?),
            DeclType::Service => Some(self.service_declarations.get(ident)?),
            DeclType::Struct => Some(self.struct_declarations.get(ident)?),
            DeclType::Table => Some(self.table_declarations.get(ident)?),
            DeclType::Union => Some(self.union_declarations.get(ident)?),
            DeclType::NewType | DeclType::Resource | DeclType::Overlay => None,
        }
    }

    pub fn get_decl_type(&self, ident: &CompId) -> Option<DeclType> {
        let library = ident.library();
        if library == self.name {
            Some(self.get_local_decl(ident)?.decl_type())
        } else {
            Some(self.library_dependencies.get(library)?.declarations.get(ident)?.kind)
        }
    }

    pub fn get_type_shape(&self, ident: &CompId) -> Option<&TypeShape> {
        let library = ident.library();
        if library == self.name {
            self.get_local_decl(ident)?.type_shape()
        } else {
            self.library_dependencies.get(library)?.declarations.get(ident)?.shape.as_ref()
        }
    }
}
