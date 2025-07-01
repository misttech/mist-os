// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::ir::{Const, TypeKind};

#[derive(Template)]
#[template(path = "const.askama", whitespace = "preserve")]
pub struct ConstTemplate<'a> {
    cnst: &'a Const,
    context: &'a Context,
}

impl<'a> ConstTemplate<'a> {
    pub fn new(cnst: &'a Const, context: &'a Context) -> Self {
        Self { cnst, context }
    }
}

impl Contextual for ConstTemplate<'_> {
    fn context(&self) -> &Context {
        self.context
    }
}
