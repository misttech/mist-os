// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct Ident {
    string: String,
}

impl Ident {
    pub fn as_id(&self) -> Id<'_> {
        Id { str: self.string.as_str() }
    }

    pub fn non_canonical(&self) -> &str {
        self.string.as_str()
    }
}

pub struct Id<'a> {
    str: &'a str,
}

impl<'a> Id<'a> {
    pub fn new(name: &'a str) -> Self {
        Self { str: name }
    }

    pub fn non_canonical(&self) -> &'a str {
        self.str
    }
}
