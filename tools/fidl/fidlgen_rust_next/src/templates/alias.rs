// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{Context, Contextual};
use crate::id::IdExt;
use crate::ir::TypeAlias;
use crate::templates::natural_type::NaturalTypeTemplate;
use crate::templates::reserved::escape;
use crate::templates::wire_type::WireTypeTemplate;

#[derive(Template)]
#[template(path = "alias.askama", whitespace = "preserve")]
pub struct AliasTemplate<'a> {
    alias: &'a TypeAlias,
    context: Context<'a>,

    name: String,
    wire_name: String,
    is_static: bool,
    natural_ty: NaturalTypeTemplate<'a>,
    wire_ty: WireTypeTemplate<'a>,
}

impl<'a> AliasTemplate<'a> {
    pub fn new(alias: &'a TypeAlias, context: Context<'a>) -> Self {
        let base_name = alias.name.decl_name().camel();
        let wire_name = format!("Wire{base_name}");

        Self {
            alias,
            context,

            name: escape(base_name),
            wire_name: escape(wire_name),
            is_static: alias.ty.shape.max_out_of_line == 0,
            natural_ty: context.natural_type(&alias.ty),
            wire_ty: context.wire_type(&alias.ty),
        }
    }
}

impl<'a> Contextual<'a> for AliasTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}
