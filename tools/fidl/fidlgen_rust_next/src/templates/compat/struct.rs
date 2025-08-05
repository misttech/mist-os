// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::Struct;
use crate::templates::{Context, Contextual, Denylist};

use super::{filters, CompatTemplate};

#[derive(Template)]
#[template(path = "compat/struct.askama")]
pub struct StructCompatTemplate<'a> {
    strct: &'a Struct,
    compat: &'a CompatTemplate<'a>,

    name: String,
    compat_name: String,
    denylist: Denylist,
}

impl<'a> Contextual<'a> for StructCompatTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.compat.context()
    }
}

impl<'a> StructCompatTemplate<'a> {
    pub fn new(strct: &'a Struct, compat: &'a CompatTemplate<'a>) -> Self {
        Self {
            strct,
            compat,

            name: strct.name.decl_name().camel(),
            compat_name: filters::escape_compat_camel(strct.name.decl_name()),
            denylist: compat.rust_or_rust_next_denylist(&strct.name),
        }
    }
}
