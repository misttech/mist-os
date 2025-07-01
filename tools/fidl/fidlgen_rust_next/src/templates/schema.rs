// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use askama::Template;

use crate::ir::{Bits, Const, DeclType, Enum, Protocol, Service, Struct, Table, TypeAlias, Union};

use super::{
    AliasTemplate, BitsTemplate, CompatTemplate, ConstTemplate, Context, Contextual, Denylist,
    EnumTemplate, ProtocolTemplate, ServiceTemplate, StructTemplate, TableTemplate, UnionTemplate,
};

#[derive(Template)]
#[template(path = "schema.askama")]
pub struct SchemaTemplate<'a> {
    context: Context<'a>,
}

impl<'a> SchemaTemplate<'a> {
    pub fn new(context: Context<'a>) -> Self {
        Self { context }
    }

    fn alias(&self, alias: &'a TypeAlias) -> AliasTemplate<'a> {
        AliasTemplate::new(alias, self.context)
    }

    fn bits(&self, bits: &'a Bits) -> BitsTemplate<'a> {
        BitsTemplate::new(bits, self.context)
    }

    fn cnst(&self, cnst: &'a Const) -> ConstTemplate<'a> {
        ConstTemplate::new(cnst, self.context)
    }

    fn compat(&self) -> CompatTemplate<'a> {
        CompatTemplate::new(self.context)
    }

    fn enm(&self, enm: &'a Enum) -> EnumTemplate<'a> {
        EnumTemplate::new(enm, self.context)
    }

    fn protocol(&self, protocol: &'a Protocol) -> ProtocolTemplate<'a> {
        ProtocolTemplate::new(protocol, self.context)
    }

    fn service(&self, service: &'a Service) -> ServiceTemplate<'a> {
        ServiceTemplate::new(service, self.context)
    }

    fn strct(&self, strct: &'a Struct) -> StructTemplate<'a> {
        StructTemplate::new(strct, self.context)
    }

    fn table(&self, table: &'a Table) -> TableTemplate<'a> {
        TableTemplate::new(table, self.context)
    }

    fn union(&self, union: &'a Union) -> UnionTemplate<'a> {
        UnionTemplate::new(union, self.context)
    }
}

impl Contextual for SchemaTemplate<'_> {
    fn context(&self) -> Context<'_> {
        self.context
    }
}
