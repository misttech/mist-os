// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::any::Any;

use serde::Deserialize;

use super::{Attributes, CompIdent, TypeShape};
use crate::de::Index;

/// Identifies which kind of declaration something is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclType {
    Alias,
    Bits,
    Const,
    Enum,
    #[serde(rename = "experimental_resource")]
    Resource,
    NewType,
    Overlay,
    Protocol,
    Service,
    Struct,
    Table,
    Union,
}

/// A schema declaration.
pub trait Decl: Any {
    /// Returns the type of the declaration.
    fn decl_type(&self) -> DeclType;

    /// Returns the name of the declaration.
    fn name(&self) -> &CompIdent;

    /// Returns the attributes of the declaration.
    fn attributes(&self) -> &Attributes;

    /// Returns the naming context of the declaration, if any.
    fn naming_context(&self) -> Option<&[String]> {
        None
    }

    /// Returns the type shape of the declaration, if any.
    fn type_shape(&self) -> Option<&TypeShape> {
        None
    }
}

impl<T: Decl> Index for T {
    type Key = CompIdent;

    fn key(&self) -> &Self::Key {
        self.name()
    }
}
