// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Table, TypeKind};
use crate::templates::reserved::escape;

#[derive(Template)]
#[template(path = "table.askama", whitespace = "preserve")]
pub struct TableTemplate<'a> {
    table: &'a Table,
    context: Context<'a>,

    name: String,
    wire_name: String,
}

impl<'a> TableTemplate<'a> {
    pub fn new(table: &'a Table, context: Context<'a>) -> Self {
        let base_name = table.name.decl_name().camel();
        let wire_name = format!("Wire{base_name}");

        Self { table, context, name: escape(base_name), wire_name: escape(wire_name) }
    }
}

impl<'a> Contextual<'a> for TableTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}
