// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Enum, IntType};
use crate::templates::prim::{NaturalIntTemplate, WireIntTemplate};
use crate::templates::reserved::escape;

#[derive(Template)]
#[template(path = "enum.askama", whitespace = "preserve")]
pub struct EnumTemplate<'a> {
    enm: &'a Enum,
    context: Context<'a>,

    name: String,
    wire_name: String,
    natural_int: NaturalIntTemplate<'a>,
    wire_int: WireIntTemplate<'a>,
}

impl<'a> EnumTemplate<'a> {
    pub fn new(enm: &'a Enum, context: Context<'a>) -> Self {
        let base_name = enm.name.decl_name().camel();
        let wire_name = format!("Wire{base_name}");

        Self {
            enm,
            context,

            name: escape(base_name),
            wire_name: escape(wire_name),
            natural_int: context.natural_int(&enm.ty),
            wire_int: context.wire_int(&enm.ty),
        }
    }
}

impl<'a> Contextual<'a> for EnumTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}
