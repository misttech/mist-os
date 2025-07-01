// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::Union;

#[derive(Template)]
#[template(path = "union.askama", whitespace = "preserve")]
pub struct UnionTemplate<'a> {
    union: &'a Union,
    context: &'a Context,
}

impl<'a> UnionTemplate<'a> {
    pub fn new(union: &'a Union, context: &'a Context) -> Self {
        Self { union, context }
    }
}

impl Contextual for UnionTemplate<'_> {
    fn context(&self) -> &Context {
        self.context
    }
}

struct UnionTemplateStrings {
    de: &'static str,
    static_: &'static str,
    phantom: &'static str,
    decode_unknown: &'static str,
    decode_as: &'static str,
    encode_as: &'static str,
}

impl UnionTemplate<'_> {
    fn has_only_static_members(&self) -> bool {
        self.union.members.iter().all(|m| m.ty.shape.max_out_of_line == 0)
    }

    fn template_strings(&self) -> &'static UnionTemplateStrings {
        if self.union.shape.max_out_of_line == 0 {
            &UnionTemplateStrings {
                de: "",
                static_: "",
                phantom: "()",
                decode_unknown: "decode_unknown_static",
                decode_as: "decode_as_static",
                encode_as: "encode_as_static",
            }
        } else {
            &UnionTemplateStrings {
                de: "<'de>",
                static_: "<'static>",
                phantom: "&'de mut [::fidl_next::Chunk]",
                decode_unknown: "decode_unknown",
                decode_as: "decode_as",
                encode_as: "encode_as",
            }
        }
    }
}
