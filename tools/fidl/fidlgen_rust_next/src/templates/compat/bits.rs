// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::Bits;
use crate::templates::reserved::escape;
use crate::templates::{Context, Contextual, Denylist};

use super::{filters, CompatTemplate};

#[derive(Template)]
#[template(path = "compat/bits.askama")]
pub struct BitsCompatTemplate<'a> {
    bits: &'a Bits,
    compat: &'a CompatTemplate<'a>,

    name: String,
    compat_name: String,
    denylist: Denylist,
}

impl<'a> Contextual<'a> for BitsCompatTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.compat.context()
    }
}

impl<'a> BitsCompatTemplate<'a> {
    pub fn new(bits: &'a Bits, compat: &'a CompatTemplate<'a>) -> Self {
        Self {
            bits,
            compat,

            name: escape(bits.name.decl_name().camel()),
            compat_name: filters::compat_camel(bits.name.decl_name()).unwrap(),
            denylist: compat.rust_or_rust_next_denylist(&bits.name),
        }
    }
}
