// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{doc_string, filters, Context};
use crate::ir::TypeAlias;

#[derive(Template)]
#[template(path = "alias.askama", whitespace = "preserve")]
pub struct AliasTemplate<'a> {
    alias: &'a TypeAlias,
    context: &'a Context,
}

impl<'a> AliasTemplate<'a> {
    pub fn new(alias: &'a TypeAlias, context: &'a Context) -> Self {
        Self { alias, context }
    }
}
