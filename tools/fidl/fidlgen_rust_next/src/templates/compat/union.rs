// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::Union;
use crate::templates::{Context, Contextual, Denylist};

use super::{filters, CompatTemplate};

#[derive(Template)]
#[template(path = "compat/union.askama")]
pub struct UnionCompatTemplate<'a> {
    union_: &'a Union,
    compat: &'a CompatTemplate<'a>,

    name: String,
    compat_name: String,
    denylist: Denylist,
}

impl<'a> Contextual<'a> for UnionCompatTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.compat.context()
    }
}

impl<'a> UnionCompatTemplate<'a> {
    pub fn new(union: &'a Union, compat: &'a CompatTemplate<'a>) -> Self {
        Self {
            union_: union,
            compat,

            name: union.name.decl_name().camel(),
            compat_name: filters::escape_compat_camel(union.name.decl_name()),
            denylist: compat.rust_or_rust_next_denylist(&union.name),
        }
    }
}
