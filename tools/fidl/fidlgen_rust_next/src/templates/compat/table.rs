// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::Table;
use crate::templates::{Context, Contextual, Denylist};

use super::{filters, CompatTemplate};

#[derive(Template)]
#[template(path = "compat/table.askama")]
pub struct TableCompatTemplate<'a> {
    table: &'a Table,
    compat: &'a CompatTemplate<'a>,

    name: String,
    compat_name: String,
    denylist: Denylist,
}

impl<'a> Contextual<'a> for TableCompatTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.compat.context()
    }
}

impl<'a> TableCompatTemplate<'a> {
    pub fn new(table: &'a Table, compat: &'a CompatTemplate<'a>) -> Self {
        Self {
            table,
            compat,

            name: table.name.decl_name().camel(),
            compat_name: filters::escape_compat_camel(table.name.decl_name()),
            denylist: compat.rust_or_rust_next_denylist(&table.name),
        }
    }
}
