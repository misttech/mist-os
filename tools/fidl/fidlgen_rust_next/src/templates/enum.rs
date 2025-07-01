// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Enum, IntType};

#[derive(Template)]
#[template(path = "enum.askama", whitespace = "preserve")]
pub struct EnumTemplate<'a> {
    enm: &'a Enum,
    context: Context<'a>,
}

impl<'a> EnumTemplate<'a> {
    pub fn new(enm: &'a Enum, context: Context<'a>) -> Self {
        Self { enm, context }
    }
}

impl Contextual for EnumTemplate<'_> {
    fn context(&self) -> Context<'_> {
        self.context
    }
}
