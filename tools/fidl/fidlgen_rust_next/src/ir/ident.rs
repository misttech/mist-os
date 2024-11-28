// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::borrow::Borrow;
use core::ops::Deref;

use serde::Deserialize;

/// A FIDL identifier.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct Ident {
    string: String,
}

impl Deref for Ident {
    type Target = Id;

    fn deref(&self) -> &Self::Target {
        Id::from_str(&self.string)
    }
}

impl Borrow<Id> for Ident {
    fn borrow(&self) -> &Id {
        Id::from_str(&self.string)
    }
}

/// A borrowed FIDL identifier.
#[derive(Debug)]
#[repr(transparent)]
pub struct Id {
    str: str,
}

impl Id {
    /// Returns a new `Id` from the given string.
    pub fn from_str(name: &str) -> &Self {
        unsafe { &*(name as *const str as *const Self) }
    }

    /// Returns the underlying, non-canonicalized string.
    pub fn non_canonical(&self) -> &str {
        &self.str
    }
}

/// A compound FIDL identifier.
#[derive(Clone, Debug, Deserialize, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct CompIdent {
    string: String,
}

impl Deref for CompIdent {
    type Target = CompId;

    fn deref(&self) -> &CompId {
        CompId::from_str(&self.string)
    }
}

impl Borrow<CompId> for CompIdent {
    fn borrow(&self) -> &CompId {
        CompId::from_str(&self.string)
    }
}

/// A borrowed compound FIDL identifier.
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct CompId {
    str: str,
}

impl CompId {
    /// Returns a `CompId` wrapping the given `str`.
    pub fn from_str(s: &str) -> &Self {
        unsafe { &*(s as *const str as *const Self) }
    }

    /// Splits this identifier into a library name and decl name.
    pub fn split(&self) -> (&str, &Id) {
        let (library, type_name) = self.str.split_once('/').unwrap();
        (library, Id::from_str(type_name))
    }

    /// Returns the library of the identifier.
    pub fn library(&self) -> &str {
        self.split().0
    }

    /// Get the name excluding the library and member name.
    pub fn decl_name(&self) -> &Id {
        self.split().1
    }
}

/// A compound FIDL identifier which may additionally reference a member.
#[derive(Clone, Debug, Deserialize, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct CompIdentOrMember {
    string: String,
}

impl CompIdentOrMember {
    /// Splits this identifier into a library name, decl name, and member name (if any).
    pub fn split(&self) -> (&CompId, Option<&Id>) {
        let slash_pos = self.string.find('/').unwrap();
        if let Some(dot_pos) = self.string.rfind('.') {
            if dot_pos > slash_pos {
                return (
                    CompId::from_str(&self.string[..dot_pos]),
                    Some(Id::from_str(&self.string[dot_pos + 1..])),
                );
            }
        }
        (CompId::from_str(&self.string), None)
    }
}
