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

    fn wire_type<'a>(&'a self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate { ty, anonymous: false, context: self }
    }

    fn anonymous_wire_type<'a>(&'a self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate { ty, anonymous: true, context: self }
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

fn doc_string(attributes: &Attributes) -> DocStringTemplate<'_> {
    DocStringTemplate { attributes }
}

#[derive(Template)]
#[template(path = "doc_string.askama")]
struct DocStringTemplate<'a> {
    attributes: &'a Attributes,
}

#[derive(Template)]
#[template(path = "prefixed_id.askama", whitespace = "suppress")]
struct PrefixedIdTemplate<'a> {
    id: &'a CompId,
    prefix: &'a str,
    context: &'a Context,
}

#[derive(Template)]
#[template(path = "wire_type.askama")]
struct WireTypeTemplate<'a> {
    ty: &'a Type,
    anonymous: bool,
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
template!(protocol(protocol: Protocol) -> ProtocolTemplate = "protocol.askama");
template!(strct(strct: Struct) -> StructTemplate = "struct.askama");
template!(table(table: Table) -> TableTemplate = "table.askama");
template!(union(union: Union) -> UnionTemplate = "union.askama");

template!(natural_int(int: IntType) -> NaturalIntTemplate = "natural_int.askama", whitespace = "suppress");
template!(natural_prim(prim: PrimSubtype) -> NaturalPrimTemplate = "natural_prim.askama", whitespace = "suppress");
template!(natural_type(ty: Type) -> NaturalTypeTemplate = "natural_type.askama");

template!(wire_int(int: IntType) -> WireIntTemplate = "wire_int.askama", whitespace = "suppress");
template!(wire_prim(prim: PrimSubtype) -> WirePrimTemplate = "wire_prim.askama", whitespace = "suppress");

impl BitsTemplate<'_> {
    fn subtype(&self) -> PrimSubtype {
        let Type { kind: TypeKind::Primitive { subtype }, .. } = &self.bits.ty else {
            panic!("invalid non-integral primitive subtype for bits");
        };
        *subtype
    }
}

struct UnionTemplateStrings {
    params: &'static str,
    phantom: &'static str,
    decode_unknown: &'static str,
    decode_as: &'static str,
    encode_as: &'static str,
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

    fn template_strings(&self) -> &'static UnionTemplateStrings {
        if self.union.shape.max_out_of_line == 0 {
            &UnionTemplateStrings {
                params: "",
                phantom: "()",
                decode_unknown: "decode_unknown_static",
                decode_as: "decode_as_static",
                encode_as: "encode_as_static",
            }
        } else {
            &UnionTemplateStrings {
                params: "<'buf>",
                phantom: "&'buf mut [::fidl_next::Chunk]",
                decode_unknown: "decode_unknown",
                decode_as: "decode_as",
                encode_as: "encode_as",
            }
        }
    }
}

mod filters {
    use std::collections::HashSet;
    use std::sync::LazyLock;

    use core::fmt::Display;

    pub fn ident<T: Display>(value: T) -> askama::Result<String> {
        let string = value.to_string();
        if KEYWORDS.contains(&string) {
            Ok(format!("{string}_"))
        } else {
            Ok(string)
        }
    }

    static KEYWORDS: LazyLock<HashSet<String>> =
        LazyLock::new(|| KEYWORDS_LIST.iter().map(|k| k.to_string()).collect());
    const KEYWORDS_LIST: &[&str] = &[
        "abstract",
        "as",
        "async",
        "await",
        "become",
        "box",
        "break",
        "const",
        "continue",
        "crate",
        "do",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "final",
        "fn",
        "for",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "macro",
        "macro_rules",
        "match",
        "mod",
        "move",
        "mut",
        "override",
        "pub",
        "priv",
        "ref",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "try",
        "type",
        "typeof",
        "union",
        "unsafe",
        "unsized",
        "use",
        "virtual",
        "where",
        "while",
        "yield",
    ];
}
