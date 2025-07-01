// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Templates generate a lot of code which have tendencies to trip lints.
#![expect(clippy::diverging_sub_expression, dead_code, unreachable_code)]

mod alias;
mod bits;
mod r#const;
mod constant;
mod denylist;
mod r#enum;
mod filters;
mod id;
mod natural_type;
mod prim;
mod protocol;
mod reserved;
mod service;
mod r#struct;
mod table;
mod r#union;
mod wire_type;

use askama::Template;

use crate::config::Config;
use crate::id::IdExt as _;
use crate::ir::*;

use self::alias::*;
use self::bits::*;
use self::constant::*;
use self::denylist::*;
use self::id::*;
use self::natural_type::*;
use self::prim::*;
use self::protocol::*;
use self::r#const::*;
use self::r#enum::*;
use self::r#struct::*;
use self::r#union::*;
use self::reserved::*;
use self::service::*;
use self::table::*;
use self::wire_type::*;

#[derive(Template)]
#[template(path = "schema.askama")]
pub struct Context {
    schema: Schema,
    config: Config,
}

impl Context {
    pub fn new(schema: Schema, config: Config) -> Self {
        Self { schema, config }
    }

    fn alias<'a>(&'a self, alias: &'a TypeAlias) -> AliasTemplate<'a> {
        AliasTemplate::new(alias, self)
    }

    fn bits<'a>(&'a self, bits: &'a Bits) -> BitsTemplate<'a> {
        BitsTemplate::new(bits, self)
    }

    fn cnst<'a>(&'a self, cnst: &'a Const) -> ConstTemplate<'a> {
        ConstTemplate::new(cnst, self)
    }

    fn compat(&self) -> CompatTemplate<'_> {
        CompatTemplate { context: self }
    }

    fn enm<'a>(&'a self, enm: &'a Enum) -> EnumTemplate<'a> {
        EnumTemplate::new(enm, self)
    }

    fn protocol<'a>(&'a self, protocol: &'a Protocol) -> ProtocolTemplate<'a> {
        ProtocolTemplate::new(protocol, self)
    }

    fn service<'a>(&'a self, service: &'a Service) -> ServiceTemplate<'a> {
        ServiceTemplate::new(service, self)
    }

    fn strct<'a>(&'a self, strct: &'a Struct) -> StructTemplate<'a> {
        StructTemplate::new(strct, self)
    }

    fn table<'a>(&'a self, table: &'a Table) -> TableTemplate<'a> {
        TableTemplate::new(table, self)
    }

    fn union<'a>(&'a self, union: &'a Union) -> UnionTemplate<'a> {
        UnionTemplate::new(union, self)
    }
}

impl Contextual for Context {
    fn context(&self) -> &Context {
        self
    }
}

pub trait Contextual {
    fn context(&self) -> &Context;

    // Helpers

    fn doc_string<'a>(&self, attributes: &'a Attributes) -> DocStringTemplate<'a> {
        DocStringTemplate { attributes }
    }

    fn emit_debug_impls(&self) -> bool {
        self.context().config.emit_debug_impls
    }

    fn natural_id<'a>(&'a self, id: &'a CompId) -> IdTemplate<'a> {
        IdTemplate::natural(id, self.context())
    }

    fn wire_id<'a>(&'a self, id: &'a CompId) -> IdTemplate<'a> {
        IdTemplate::wire(id, self.context())
    }

    fn wire_optional_id<'a>(&'a self, id: &'a CompId) -> IdTemplate<'a> {
        IdTemplate::wire_optional(id, self.context())
    }

    fn natural_int<'a>(&self, int: &'a IntType) -> NaturalIntTemplate<'a> {
        NaturalIntTemplate(int)
    }

    fn natural_prim<'a>(&self, prim: &'a PrimSubtype) -> NaturalPrimTemplate<'a> {
        NaturalPrimTemplate(prim)
    }

    fn natural_type<'a>(&'a self, ty: &'a Type) -> NaturalTypeTemplate<'a> {
        NaturalTypeTemplate::new(ty, self.context())
    }

    fn wire_int<'a>(&self, int: &'a IntType) -> WireIntTemplate<'a> {
        WireIntTemplate(int)
    }

    fn wire_prim<'a>(&self, prim: &'a PrimSubtype) -> WirePrimTemplate<'a> {
        WirePrimTemplate(prim)
    }

    fn wire_type<'a>(&'a self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate::with_de(ty, self.context())
    }

    fn static_wire_type<'a>(&'a self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate::with_static(ty, self.context())
    }

    fn anonymous_wire_type<'a>(&'a self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate::with_anonymous(ty, self.context())
    }

    fn constant<'a>(&'a self, constant: &'a Constant, ty: &'a Type) -> ConstantTemplate<'a> {
        ConstantTemplate::new(constant, ty, self.context())
    }

    fn rust_next_denylist(&self, ident: &CompId) -> Denylist {
        Denylist::rust_next(&self.context().schema, ident)
    }

    fn rust_or_rust_next_denylist(&self, ident: &CompId) -> Denylist {
        Denylist::rust_or_rust_next(&self.context().schema, ident)
    }
}

#[derive(Template)]
#[template(path = "compat.askama")]
struct CompatTemplate<'a> {
    context: &'a Context,
}

impl CompatTemplate<'_> {
    fn compat_crate_name(&self) -> String {
        format!("fidl_{}", self.context.schema.name.replace('.', "_"))
    }
}

impl Contextual for CompatTemplate<'_> {
    fn context(&self) -> &Context {
        self.context
    }
}

#[derive(Template)]
#[template(path = "doc_string.askama")]
pub struct DocStringTemplate<'a> {
    attributes: &'a Attributes,
}
