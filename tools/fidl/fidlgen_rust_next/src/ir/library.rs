// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use serde::Deserialize;

use super::{CompIdent, DeclType, Enum, Struct, Table, Union};

/// Root of the JSON IR datastructure for a library.
#[derive(Deserialize)]
pub struct Library {
    pub name: String,
    // #[serde(deserialize_with = "crate::de::index")]
    // pub const_declarations: Vec<Const>,
    // pub bits_declarations: Vec<Bits>,
    #[serde(deserialize_with = "crate::de::index")]
    pub enum_declarations: HashMap<CompIdent, Enum>,
    // pub interface_declarations: Vec<Protocol>,
    // pub service_declarations: Vec<Service>,
    #[serde(deserialize_with = "crate::de::index")]
    pub struct_declarations: HashMap<CompIdent, Struct>,
    #[serde(deserialize_with = "crate::de::index")]
    pub external_struct_declarations: HashMap<CompIdent, Struct>,
    #[serde(deserialize_with = "crate::de::index")]
    pub table_declarations: HashMap<CompIdent, Table>,
    #[serde(deserialize_with = "crate::de::index")]
    pub union_declarations: HashMap<CompIdent, Union>,
    // pub type_alias_declarations: Vec<TypeAlias>,
    pub declaration_order: Vec<CompIdent>,
    pub declarations: HashMap<CompIdent, DeclType>,
    // pub library_dependencies: Vec<Library>,
}
