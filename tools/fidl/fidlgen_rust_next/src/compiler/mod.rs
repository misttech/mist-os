// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Templates generate a lot of code which have tendencies to trip lints.
#![expect(clippy::diverging_sub_expression, dead_code, unreachable_code)]

mod resource_binding;
mod util;

use askama::Template;

use self::util::IdExt as _;
use crate::ir::*;

pub use self::resource_binding::ResourceBindings;

pub struct Config {
    pub emit_debug_impls: bool,
    pub resource_bindings: ResourceBindings,
}

#[derive(Template)]
#[template(path = "schema.askama")]
pub struct Compiler<'a> {
    schema: &'a Schema,
    config: Config,
}

impl<'a> Compiler<'a> {
    pub fn new(schema: &'a Schema, config: Config) -> Self {
        Self { schema, config }
    }

    fn natural_id<'b>(&'b self, id: &'b CompId) -> PrefixedIdTemplate<'b> {
        PrefixedIdTemplate { id, prefix: "", compiler: self }
    }

    fn wire_id<'b>(&'b self, id: &'b CompId) -> PrefixedIdTemplate<'b> {
        PrefixedIdTemplate { id, prefix: "Wire", compiler: self }
    }

    fn wire_optional_id<'b>(&'b self, id: &'b CompId) -> PrefixedIdTemplate<'b> {
        PrefixedIdTemplate { id, prefix: "WireOptional", compiler: self }
    }

    fn natural_constant<'b>(
        &'b self,
        constant: &'b Constant,
        ty: &'b Type,
    ) -> NaturalConstantTemplate<'b> {
        NaturalConstantTemplate { constant, ty, compiler: self }
    }
}

#[derive(Template)]
#[template(path = "prefixed_id.askama", whitespace = "suppress")]
struct PrefixedIdTemplate<'b> {
    id: &'b CompId,
    prefix: &'b str,
    compiler: &'b Compiler<'b>,
}

#[derive(Template)]
#[template(path = "natural_constant.askama", whitespace = "suppress")]
struct NaturalConstantTemplate<'b> {
    constant: &'b Constant,
    ty: &'b Type,
    compiler: &'b Compiler<'b>,
}

macro_rules! template {
    ($fn:ident($field:ident: $ir:ty) -> $name:ident = $path:literal) => {
        template!($fn($field: $ir) -> $name = $path, whitespace = "preserve");
    };
    ($fn:ident($field:ident: $ir:ty) -> $name:ident = $path:literal, whitespace = $ws:literal) => {
        #[derive(Template)]
        #[template(path = $path, whitespace = $ws)]
        struct $name<'a> {
            $field: &'a $ir,
            compiler: &'a Compiler<'a>,
        }

        impl Compiler<'_> {
            fn $fn<'b>(&'b self, $field: &'b $ir) -> $name<'b> {
                $name { $field, compiler: self }
            }
        }
    }
}

template!(alias(alias: TypeAlias) -> TypeAliasTemplate = "alias.askama");
template!(bits(bits: Bits) -> BitsTemplate = "bits.askama");
template!(cnst(cnst: Const) -> ConstTemplate = "const.askama");
template!(enm(enm: Enum) -> EnumTemplate = "enum.askama");
template!(strct(strct: Struct) -> StructTemplate = "struct.askama");
template!(table(table: Table) -> TableTemplate = "table.askama");
template!(union(union: Union) -> UnionTemplate = "union.askama");

template!(natural_int(int: IntType) -> NaturalIntTemplate = "natural_int.askama", whitespace = "suppress");
template!(natural_prim(prim: PrimSubtype) -> NaturalPrimTemplate = "natural_prim.askama", whitespace = "suppress");
template!(natural_type(ty: Type) -> NaturalTypeTemplate = "natural_type.askama");

template!(wire_int(int: IntType) -> WireIntTemplate = "wire_int.askama", whitespace = "suppress");
template!(wire_prim(prim: PrimSubtype) -> WirePrimTemplate = "wire_prim.askama", whitespace = "suppress");
template!(wire_type(ty: Type) -> WireTypeTemplate = "wire_type.askama");

impl BitsTemplate<'_> {
    fn subtype(&self) -> PrimSubtype {
        let Type { kind: TypeKind::Primitive { subtype }, .. } = &self.bits.ty else {
            panic!("invalid non-integral primitive subtype for bits");
        };
        *subtype
    }
}

impl UnionTemplate<'_> {
    fn has_only_static_members(&self) -> bool {
        let mut result = true;
        for member in &self.union.members {
            if member.ty.shape.max_out_of_line != 0 {
                result = false;
                break;
            }
        }
        result
    }
}
