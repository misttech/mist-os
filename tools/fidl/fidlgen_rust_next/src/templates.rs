// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Templates generate a lot of code which have tendencies to trip lints.
#![expect(clippy::diverging_sub_expression, dead_code, unreachable_code)]

use core::fmt;
use std::collections::BTreeSet;

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

    fn compat_crate_name(&self) -> String {
        format!("fidl_{}", self.schema.name.replace('.', "_"))
    }

    fn natural_id<'a>(&'a self, id: &'a CompId) -> PrefixedIdTemplate<'a> {
        PrefixedIdTemplate { id, prefix: "", context: self }
    }

    fn wire_id<'a>(&'a self, id: &'a CompId) -> PrefixedIdTemplate<'a> {
        PrefixedIdTemplate { id, prefix: "Wire", context: self }
    }

    fn wire_type<'a>(&'a self, ty: &'a Type) -> WireTypeTemplate<'a> {
        WireTypeTemplate { ty, context: self }
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

    fn compat(&self) -> CompatTemplate<'_> {
        CompatTemplate { context: self }
    }

    fn bindings_compat_restriction(&self, ident: &CompId) -> BindingsRestriction {
        self.bindings_restriction_inner(&["rust", "rust_next"], ident, None)
    }

    fn bindings_denylist_restriction(&self, ident: &CompId) -> BindingsRestriction {
        self.bindings_restriction_inner(&["rust_next"], ident, None)
    }

    fn bindings_restriction_inner(
        &self,
        bindings_names: &[&str],
        ident: &CompId,
        next: Option<&str>,
    ) -> BindingsRestriction {
        fn denylist_contains_any(attributes: &Attributes, names: &[&str]) -> bool {
            attributes.attributes.get("bindings_denylist").is_some_and(|attr| {
                attr.args["value"].value.value.split(", ").any(|x| names.iter().any(|y| *x == **y))
            })
        }

        let (attributes, naming_context) = match self.schema.declarations[ident] {
            DeclType::Bits => {
                let bits = &self.schema.bits_declarations[ident];
                (&bits.attributes, &bits.naming_context)
            }
            DeclType::Enum => {
                let enm = &self.schema.enum_declarations[ident];
                (&enm.attributes, &enm.naming_context)
            }
            DeclType::Struct => {
                let strct = &self.schema.struct_declarations[ident];
                (&strct.attributes, &strct.naming_context)
            }
            DeclType::Table => {
                let table = &self.schema.table_declarations[ident];
                (&table.attributes, &table.naming_context)
            }
            DeclType::Union => {
                let union = &self.schema.union_declarations[ident];
                (&union.attributes, &union.naming_context)
            }
            DeclType::Protocol => {
                let protocol = &self.schema.protocol_declarations[ident];

                let is_method_denylisted = next.is_some_and(|next| {
                    protocol.methods.iter().any(|m| {
                        m.name.non_canonical() == next
                            && denylist_contains_any(&m.attributes, bindings_names)
                    })
                });
                if denylist_contains_any(&protocol.attributes, bindings_names)
                    || is_method_denylisted
                {
                    return BindingsRestriction::Never;
                }

                return match protocol.transport() {
                    Some("Syscall") => BindingsRestriction::Never,
                    Some("Driver") => BindingsRestriction::CfgDriver,
                    _ => BindingsRestriction::Always,
                };
            }
            _ => return BindingsRestriction::Always,
        };

        if denylist_contains_any(attributes, bindings_names) {
            BindingsRestriction::Never
        } else {
            self.bindings_naming_context_restriction(bindings_names, naming_context)
        }
    }

    fn bindings_naming_context_restriction(
        &self,
        bindings_names: &[&str],
        naming_context: &[String],
    ) -> BindingsRestriction {
        let mut aggregate = format!("{}/", self.schema.name);
        let mut result = BindingsRestriction::Always;

        for (name, next) in naming_context.iter().zip(naming_context.iter().skip(1)) {
            aggregate = format!("{aggregate}{name}");
            let comp_ident = CompIdent::new(aggregate.clone());
            if self.schema.declarations.contains_key(&comp_ident) {
                result = result.max(self.bindings_restriction_inner(
                    bindings_names,
                    &comp_ident,
                    Some(next),
                ));
            }
        }
        result
    }
}

enum BindingsRestriction {
    Always,
    CfgDriver,
    Never,
}

impl BindingsRestriction {
    fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Always, x) | (x, Self::Always) => x,
            (Self::CfgDriver, Self::CfgDriver) => Self::CfgDriver,
            _ => Self::Never,
        }
    }
}

impl fmt::Display for BindingsRestriction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Always => Ok(()),
            Self::CfgDriver => write!(f, "#[cfg(feature = \"driver\")]"),
            Self::Never => panic!("attempted to emit a bindings restriction of 'never'"),
        }
    }
}

fn doc_string(attributes: &Attributes) -> DocStringTemplate<'_> {
    DocStringTemplate { attributes }
}

#[derive(Template)]
#[template(path = "compat.askama")]
struct CompatTemplate<'a> {
    context: &'a Context,
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
template!(service(service: Service) -> ServiceTemplate = "service.askama");
template!(strct(strct: Struct) -> StructTemplate = "struct.askama");
template!(empty_success_struct(strct: Struct) -> EmptySuccessStructTemplate = "empty_success_struct.askama");
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

impl ProtocolTemplate<'_> {
    fn prelude_method_type_idents(&self) -> BTreeSet<CompIdent> {
        let mut result = BTreeSet::new();

        fn get_identifier(ty: &Type) -> Option<CompIdent> {
            if let Type { kind: TypeKind::Identifier { identifier, .. }, .. } = ty {
                Some(identifier.clone())
            } else {
                None
            }
        }

        for method in self.protocol.methods.iter() {
            // We always include the request payload in the prelude if there is one
            if let Some(request) = method.maybe_request_payload.as_deref() {
                result.extend(get_identifier(request));
            }

            if let Some(success) = method.maybe_response_success_type.as_deref() {
                // The response type is a result, so we only want to include the success and error
                // types in the prelude
                result.extend(get_identifier(success));

                if let Some(error) = method.maybe_response_err_type.as_deref() {
                    result.extend(get_identifier(error));
                }
            } else if let Some(response) = method.maybe_response_payload.as_deref() {
                // The response type is not a result, so we want to include the response payload
                // type in the prelude
                result.extend(get_identifier(response));
            }
        }

        result
    }
}

struct ZeroPaddingRange {
    offset: u32,
    width: u32,
}

impl StructTemplate<'_> {
    fn zero_padding_ranges(&self) -> Vec<ZeroPaddingRange> {
        let mut ranges = Vec::new();
        let mut end = self.strct.shape.inline_size;
        for member in self.strct.members.iter().rev() {
            let padding = member.field_shape.padding;
            if padding != 0 {
                ranges.push(ZeroPaddingRange { offset: end - padding, width: padding });
            }
            end = member.field_shape.offset;
        }

        ranges
    }
}

struct UnionTemplateStrings {
    decode_unknown: &'static str,
    decode_as: &'static str,
    encode_as: &'static str,
}

impl UnionTemplate<'_> {
    fn template_strings(&self) -> &'static UnionTemplateStrings {
        if self.union.shape.max_out_of_line == 0 {
            &UnionTemplateStrings {
                decode_unknown: "decode_unknown_static",
                decode_as: "decode_as_static",
                encode_as: "encode_as_static",
            }
        } else {
            &UnionTemplateStrings {
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

    use crate::id::IdExt;
    use crate::ir::Id;

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
        "unsafe",
        "unsized",
        "use",
        "virtual",
        "where",
        "while",
        "yield",
    ];

    pub fn compat_snake(value: &Id) -> askama::Result<String> {
        compat_with(value, IdExt::snake)
    }

    pub fn compat_camel(value: &Id) -> askama::Result<String> {
        compat_with(value, IdExt::camel)
    }

    fn compat_with(value: &Id, case: impl Fn(&Id) -> String) -> askama::Result<String> {
        if compat_conflicts(value.non_canonical()) {
            Ok(format!("{}_", case(value)))
        } else {
            Ok(case(value))
        }
    }

    fn compat_conflicts(value: &str) -> bool {
        KEYWORDS.contains(value)
            || COMPAT_RESERVED_SUFFIX_LIST.iter().any(|suffix| value.ends_with(suffix))
    }

    const COMPAT_RESERVED_SUFFIX_LIST: &[&str] =
        &["Impl", "Marker", "Proxy", "ProxyProtocol", "ControlHandle", "Responder", "Server"];
}
