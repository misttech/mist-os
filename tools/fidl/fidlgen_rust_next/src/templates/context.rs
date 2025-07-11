// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::config::ResourceBindings;
use crate::ir::{Attributes, CompId, Constant, IntType, PrimSubtype, Schema, Type};
use crate::Config;

use super::{
    ConstantTemplate, Denylist, DocStringTemplate, IdTemplate, NaturalIntTemplate,
    NaturalPrimTemplate, NaturalTypeTemplate, WireIntTemplate, WirePrimTemplate, WireTypeTemplate,
};

#[derive(Clone, Copy)]
pub struct Context<'a> {
    schema: &'a Schema,
    config: &'a Config,
}

impl<'a> Context<'a> {
    pub fn new(schema: &'a Schema, config: &'a Config) -> Self {
        Self { schema, config }
    }
}

pub trait Contextual<'a> {
    fn context(&self) -> Context<'a>;

    // Helpers

    fn schema(&self) -> &'a Schema {
        self.context().schema
    }

    fn resource_bindings(&self) -> &'a ResourceBindings {
        &self.context().config.resource_bindings
    }

    fn doc_string(&self, attributes: &'a Attributes) -> DocStringTemplate<'a> {
        DocStringTemplate::new(attributes)
    }

    fn emit_compat(&self) -> bool {
        self.context().config.emit_compat
    }

    fn emit_debug_impls(&self) -> bool {
        self.context().config.emit_debug_impls
    }

    fn natural_id(&self, id: &'a CompId) -> IdTemplate<'a> {
        IdTemplate::natural(id, self.context())
    }

    fn wire_id(&self, id: &'a CompId) -> IdTemplate<'a> {
        IdTemplate::wire(id, self.context())
    }

    fn wire_optional_id(&self, id: &'a CompId) -> IdTemplate<'a> {
        IdTemplate::wire_optional(id, self.context())
    }

    fn natural_int(&self, int: &'a IntType) -> NaturalIntTemplate<'a> {
        NaturalIntTemplate(int)
    }

    fn natural_prim(&self, prim: &'a PrimSubtype) -> NaturalPrimTemplate<'a> {
        NaturalPrimTemplate(prim)
    }

    fn natural_type(&self, ty: &'a Type) -> NaturalTypeTemplate<'a> {
        NaturalTypeTemplate::new(ty, self.context())
    }

    fn wire_int(&self, int: &'a IntType) -> WireIntTemplate<'a> {
        WireIntTemplate(int)
    }

    fn wire_prim(&self, prim: &'a PrimSubtype) -> WirePrimTemplate<'a> {
        WirePrimTemplate(prim)
    }

    fn wire_type(&self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate::with_de(ty, self.context())
    }

    fn static_wire_type(&self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate::with_static(ty, self.context())
    }

    fn anonymous_wire_type(&self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate::with_anonymous(ty, self.context())
    }

    fn constant(&self, constant: &'a Constant, ty: &'a Type) -> ConstantTemplate<'a> {
        ConstantTemplate::new(constant, ty, self.context())
    }

    fn rust_next_denylist(&self, ident: &CompId) -> Denylist {
        Denylist::for_ident(self.context().schema, ident, &["rust_next"])
    }
}

impl<'a> Contextual<'a> for Context<'a> {
    fn context(&self) -> Context<'a> {
        *self
    }
}
