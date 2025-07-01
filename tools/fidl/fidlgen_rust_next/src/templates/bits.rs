// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use super::{filters, Context, Contextual};
use crate::id::IdExt as _;
use crate::ir::{Bits, PrimSubtype, Type, TypeKind};

#[derive(Template)]
#[template(path = "bits.askama", whitespace = "preserve")]
pub struct BitsTemplate<'a> {
    bits: &'a Bits,
    context: Context<'a>,
}

impl<'a> BitsTemplate<'a> {
    pub fn new(bits: &'a Bits, context: Context<'a>) -> Self {
        Self { bits, context }
    }

    fn subtype(&self) -> PrimSubtype {
        let Type { kind: TypeKind::Primitive { subtype }, .. } = &self.bits.ty else {
            panic!("invalid non-integral primitive subtype for bits");
        };
        *subtype
    }
}

impl Contextual for BitsTemplate<'_> {
    fn context(&self) -> Context<'_> {
        self.context
    }
}
