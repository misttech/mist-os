// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Templates generate a lot of code which have tendencies to trip lints.
#![expect(clippy::diverging_sub_expression, dead_code, unreachable_code)]

use askama::Template;

use crate::config::Config;
use crate::id::IdExt as _;
use crate::ir::*;

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

    fn natural_id<'a>(&'a self, id: &'a CompId) -> PrefixedIdTemplate<'a> {
        PrefixedIdTemplate { id, prefix: "", context: self }
    }

    fn wire_id<'a>(&'a self, id: &'a CompId) -> PrefixedIdTemplate<'a> {
        PrefixedIdTemplate { id, prefix: "Wire", context: self }
    }

    fn wire_optional_id<'a>(&'a self, id: &'a CompId) -> PrefixedIdTemplate<'a> {
        PrefixedIdTemplate { id, prefix: "WireOptional", context: self }
    }

    fn natural_constant<'a>(
        &'a self,
        constant: &'a Constant,
        ty: &'a Type,
    ) -> NaturalConstantTemplate<'a> {
        NaturalConstantTemplate { constant, ty, context: self }
    }
}

#[derive(Template)]
#[template(path = "prefixed_id.askama", whitespace = "suppress")]
struct PrefixedIdTemplate<'a> {
    id: &'a CompId,
    prefix: &'a str,
    context: &'a Context,
}

#[derive(Template)]
#[template(path = "natural_constant.askama", whitespace = "suppress")]
struct NaturalConstantTemplate<'a> {
    constant: &'a Constant,
    ty: &'a Type,
    context: &'a Context,
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
            context: &'a Context,
        }

        impl Context {
            fn $fn<'a>(&'a self, $field: &'a $ir) -> $name<'a> {
                $name { $field, context: self }
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

mod filters {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    use core::fmt::Display;

    pub fn ident<T: Display>(value: T) -> askama::Result<String> {
        let string = value.to_string();
        Ok(ESCAPED.get(&string).map(ToString::to_string).unwrap_or(string))
    }

    static ESCAPED: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        ESCAPED_LIST.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    });
    const ESCAPED_LIST: &[(&str, &str)] = &[
        ("abstract", "r#abstract"),
        ("as", "r#as"),
        ("async", "r#async"),
        ("await", "r#await"),
        ("become", "r#become"),
        ("box", "r#box"),
        ("break", "r#break"),
        ("const", "r#const"),
        ("continue", "r#continue"),
        ("crate", "crate_"),
        ("do", "r#do"),
        ("dyn", "r#dyn"),
        ("else", "r#else"),
        ("enum", "r#enum"),
        ("extern", "r#extern"),
        ("false", "r#false"),
        ("final", "r#final"),
        ("fn", "r#fn"),
        ("for", "r#for"),
        ("if", "r#if"),
        ("impl", "r#impl"),
        ("in", "r#in"),
        ("let", "r#let"),
        ("loop", "r#loop"),
        ("macro", "r#macro"),
        ("macro_rules", "r#macro_rules"),
        ("match", "r#match"),
        ("mod", "r#mod"),
        ("move", "r#move"),
        ("mut", "r#mut"),
        ("override", "r#override"),
        ("pub", "r#pub"),
        ("priv", "r#priv"),
        ("ref", "r#ref"),
        ("return", "r#return"),
        ("self", "self_"),
        ("Self", "Self_"),
        ("static", "r#static"),
        ("struct", "r#struct"),
        ("super", "super_"),
        ("trait", "r#trait"),
        ("true", "r#true"),
        ("try", "r#try"),
        ("type", "r#type"),
        ("typeof", "r#typeof"),
        ("union", "r#union"),
        ("unsafe", "r#unsafe"),
        ("unsized", "r#unsized"),
        ("use", "r#use"),
        ("virtual", "r#virtual"),
        ("where", "r#where"),
        ("while", "r#while"),
        ("yield", "r#yield"),
    ];
}
