// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::ir::TypeAlias;

#[derive(Template)]
#[template(path = "alias.askama", whitespace = "preserve")]
pub struct AliasTemplate<'a> {
    alias: &'a TypeAlias,
    context: Context<'a>,
}

impl<'a> AliasTemplate<'a> {
    pub fn new(alias: &'a TypeAlias, context: Context<'a>) -> Self {
        Self { alias, context }
    }
}

impl Contextual for AliasTemplate<'_> {
    fn context(&self) -> Context<'_> {
        self.context
    }
}
