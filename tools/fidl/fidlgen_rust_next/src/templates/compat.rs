// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::id::IdExt as _;
use crate::ir::DeclType;

use super::{filters, Context, Contextual, Denylist};

#[derive(Template)]
#[template(path = "compat.askama")]
pub struct CompatTemplate<'a> {
    context: Context<'a>,
}

impl<'a> CompatTemplate<'a> {
    pub fn new(context: Context<'a>) -> Self {
        Self { context }
    }

    fn compat_crate_name(&self) -> String {
        format!("fidl_{}", self.schema().name.replace('.', "_"))
    }
}

impl Contextual for CompatTemplate<'_> {
    fn context(&self) -> Context<'_> {
        self.context
    }
}
