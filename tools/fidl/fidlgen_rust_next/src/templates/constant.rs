// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Constant, ConstantKind, DeclType, LiteralKind, Type, TypeKind};

#[derive(Template)]
#[template(path = "constant.askama", whitespace = "suppress")]
pub struct ConstantTemplate<'a> {
    constant: &'a Constant,
    ty: &'a Type,
    context: &'a Context,
}

impl<'a> ConstantTemplate<'a> {
    pub fn new(constant: &'a Constant, ty: &'a Type, context: &'a Context) -> Self {
        Self { constant, ty, context }
    }
}

impl Contextual for ConstantTemplate<'_> {
    fn context(&self) -> &Context {
        self.context
    }
}
