// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Table, TypeKind};

#[derive(Template)]
#[template(path = "table.askama", whitespace = "preserve")]
pub struct TableTemplate<'a> {
    table: &'a Table,
    context: Context<'a>,
}

impl<'a> TableTemplate<'a> {
    pub fn new(table: &'a Table, context: Context<'a>) -> Self {
        Self { table, context }
    }
}

impl Contextual for TableTemplate<'_> {
    fn context(&self) -> Context<'_> {
        self.context
    }
}
