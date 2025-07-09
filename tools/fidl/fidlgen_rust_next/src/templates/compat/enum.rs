// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::Enum;
use crate::templates::reserved::escape;
use crate::templates::{Context, Contextual, Denylist};

use super::{filters, CompatTemplate};

#[derive(Template)]
#[template(path = "compat/enum.askama")]
pub struct EnumCompatTemplate<'a> {
    enm: &'a Enum,
    compat: &'a CompatTemplate<'a>,

    name: String,
    compat_name: String,
    denylist: Denylist,
}

impl<'a> Contextual<'a> for EnumCompatTemplate<'a> {
    fn context(&self) -> Context<'a> {
        self.compat.context()
    }
}

impl<'a> EnumCompatTemplate<'a> {
    pub fn new(enm: &'a Enum, compat: &'a CompatTemplate<'a>) -> Self {
        Self {
            enm,
            compat,

            name: escape(enm.name.decl_name().camel()),
            compat_name: filters::compat_camel(enm.name.decl_name()).unwrap(),
            denylist: compat.rust_or_rust_next_denylist(&enm.name),
        }
    }
}
