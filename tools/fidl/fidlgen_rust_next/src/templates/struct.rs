// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Struct, TypeKind};

#[derive(Template)]
#[template(path = "struct.askama", whitespace = "preserve")]
pub struct StructTemplate<'a> {
    strct: &'a Struct,
    context: &'a Context,
}

impl<'a> StructTemplate<'a> {
    pub fn new(strct: &'a Struct, context: &'a Context) -> Self {
        Self { strct, context }
    }
}

impl Contextual for StructTemplate<'_> {
    fn context(&self) -> &Context {
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
