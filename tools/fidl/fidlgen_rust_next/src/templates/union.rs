// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::Union;
use crate::templates::reserved::escape;

#[derive(Template)]
#[template(path = "union.askama", whitespace = "preserve")]
pub struct UnionTemplate<'a> {
    union: &'a Union,
    context: Context<'a>,

    is_static: bool,
    name: String,
    wire_name: String,
    wire_optional_name: String,
    mod_name: String,

    de: &'static str,
    static_: &'static str,
    phantom: &'static str,
    decode_unknown: &'static str,
    decode_as: &'static str,
    encode_as: &'static str,
}

impl<'a> UnionTemplate<'a> {
    pub fn new(union: &'a Union, context: Context<'a>) -> Self {
        let is_static = union.shape.max_out_of_line == 0;
        let base_name = union.name.decl_name().camel();
        let wire_name = format!("Wire{base_name}");
        let wire_optional_name = format!("WireOptional{base_name}");
        let mod_name = union.name.decl_name().snake();

        let (de, static_, phantom, decode_unknown, decode_as, encode_as) = if is_static {
            ("", "", "()", "decode_unknown_static", "decode_as_static", "encode_as_static")
        } else {
            (
                "<'de>",
                "<'static>",
                "&'de mut [::fidl_next::Chunk]",
                "decode_unknown",
                "decode_as",
                "encode_as",
            )
        };

        Self {
            union,
            context,

            is_static,
            name: escape(base_name),
            wire_name: escape(wire_name),
            wire_optional_name: escape(wire_optional_name),
            mod_name: escape(mod_name),

            de,
            static_,
            phantom,
            decode_unknown,
            decode_as,
            encode_as,
        }
    }

    fn has_only_static_members(&self) -> bool {
        self.union.members.iter().all(|m| m.ty.shape.max_out_of_line == 0)
    }
}

impl<'a> Contextual<'a> for UnionTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.context
    }
}
