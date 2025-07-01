// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Struct, TypeKind};
use crate::templates::reserved::escape;

#[derive(Template)]
#[template(path = "struct.askama", whitespace = "preserve")]
pub struct StructTemplate<'a> {
    strct: &'a Struct,
    context: Context<'a>,

    is_static: bool,
    has_padding: bool,
    name: String,
    wire_name: String,

    de: &'static str,
    infer: &'static str,
    static_: &'static str,
}

impl<'a> StructTemplate<'a> {
    pub fn new(strct: &'a Struct, context: Context<'a>) -> Self {
        let is_static = strct.shape.max_out_of_line == 0;
        let base_name = strct.name.decl_name().camel();
        let wire_name = format!("Wire{base_name}");

        let (de, infer, static_) =
            if is_static { ("", "", "") } else { ("<'de>", "<'_>", "<'static>") };

        Self {
            strct,
            context,

            is_static,
            has_padding: strct.shape.has_padding,
            name: escape(base_name),
            wire_name: escape(wire_name),

            de,
            infer,
            static_,
        }
    }
}

impl<'a> Contextual<'a> for StructTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}

struct ZeroPaddingRange {
    offset: u32,
    width: u32,
}

impl StructTemplate<'_> {
    fn zero_padding_ranges(&self) -> Vec<ZeroPaddingRange> {
        let mut ranges = Vec::new();
        let mut end = self.strct.shape.inline_size;
        for member in self.strct.members.iter().rev() {
            let padding = member.field_shape.padding;
            if padding != 0 {
                ranges.push(ZeroPaddingRange { offset: end - padding, width: padding });
            }
            end = member.field_shape.offset;
        }

        ranges
    }
}
