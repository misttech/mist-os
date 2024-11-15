// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

use super::Id;

#[derive(Clone, Debug, Deserialize, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct CompIdent {
    inner: String,
}

impl CompIdent {
    /// Splits this identifier into a library name and decl name.
    pub fn split(&self) -> (&str, Id<'_>) {
        let (library, type_name) = self.inner.split_once('/').unwrap();
        (library, Id::new(type_name))
    }

    /// Returns the library of the identifier.
    /// TODO(b/369406218): Remove when used
    #[allow(dead_code)]
    pub fn library(&self) -> &str {
        self.split().0
    }

    /// Get the name excluding the library and member name.
    pub fn type_name(&self) -> Id<'_> {
        self.split().1
    }
}
