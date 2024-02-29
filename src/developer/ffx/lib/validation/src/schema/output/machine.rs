// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{any::TypeId, collections::HashMap, ops::Deref};

use serde::ser::SerializeMap;

use crate::{
    schema::{self, json},
    simplify,
};
use ffx_validation_proc_macro::schema;

struct AliasId;
struct StructField;
struct ValueType;
struct Type;
pub struct MachineSchema;

schema! {
    type AliasId = String;

    type StructField = struct {
        key: String,
        value: Type,
        optional: bool,
    };

    type ValueType = "null" | "bool" | "integer" | "double" | "string" | "json_array" | "json_object";

    #[recursive]
    type Type =
        struct { kind: "alias", id: AliasId } |
        struct { kind: "union", types: [Type] } |
        struct { kind: "struct", fields: [StructField], unknown_fields: Type } |
        struct { kind: "tuple", fields: [Type] } |
        struct { kind: "array", size: usize?, element: Type } |
        struct { kind: "map", key: Type, value: Type } |
        struct { kind: ValueType | "any" } |
        struct { kind: "constant", json: json::Any };

    type MachineSchema = struct {
        root: Type,
        alias: schema::Map<AliasId, Type>,
    };
}

pub fn serialize<'a>(
    bump: &'a bumpalo::Bump,
    ctx: &simplify::Ctx<'a>,
    root: &'a schema::Type<'a>,
) -> impl serde::Serialize + 'a {
    let (aliases, alias_id_map) = ctx.sorted_aliases(bump);
    let ctx = SerializeCtx { alias_id_map, aliases };

    Root { ctx, root }
}

struct SerializeCtx<'a> {
    alias_id_map: HashMap<TypeId, &'a str>,
    aliases: Vec<(&'a str, &'a schema::Type<'a>)>,
}

impl<'a> SerializeCtx<'a> {
    fn alias_id(&self, id: &TypeId) -> &'a str {
        self.alias_id_map[id]
    }
}

struct Root<'a> {
    ctx: SerializeCtx<'a>,
    root: &'a schema::Type<'a>,
}

impl<'a> Deref for Root<'a> {
    type Target = SerializeCtx<'a>;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

struct SerializeItem<'a, T> {
    ctx: &'a Root<'a>,
    item: T,
}

impl<'a, T> Deref for SerializeItem<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

type SerializeType<'a> = SerializeItem<'a, &'a schema::Type<'a>>;
type SerializeTypes<'a> = SerializeItem<'a, &'a [&'a schema::Type<'a>]>;
type SerializeAliases<'a> = SerializeItem<'a, &'a [(&'a str, &'a schema::Type<'a>)]>;

type SerializeStructField<'a> = SerializeItem<'a, &'a schema::Field<'a>>;
type SerializeStructFields<'a> = SerializeItem<'a, &'a [schema::Field<'a>]>;

macro_rules! map {
    ($s:ident =>) => {};
    ($s:ident => $key:ident: $value:expr $(, $($rest:tt)*)?) => {{
        $s.serialize_entry(stringify!($key), $value)?;
        $(map!($s => $($rest)*);)?
    }};
}

impl<'a> serde::Serialize for Root<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut s = serializer.serialize_map(None)?;
        map! {
            s =>
            root: &SerializeType { ctx: self, item: self.root },
            alias: &SerializeAliases { ctx: self, item: &self.ctx.aliases },
        };
        s.end()
    }
}

impl<'a> serde::Serialize for SerializeType<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let ctx = self.ctx;
        let mut s = serializer.serialize_map(None)?;
        match self.item {
            schema::Type::Void => map! { s => kind: "void" },
            schema::Type::Union(tys) => {
                map! { s => kind: "union", types: &SerializeTypes { ctx, item: tys } }
            }
            schema::Type::Alias { name: _, id, ty: _ } => {
                map! { s => kind: "alias", id: ctx.alias_id(&(id)()) }
            }
            schema::Type::Struct { fields, extras } => {
                let unknown_fields = match extras {
                    None => &schema::Type::Any,
                    Some(schema::StructExtras::Deny) => &schema::Type::Void,
                    Some(schema::StructExtras::Flatten(..)) => {
                        panic!("flattened map fields are not yet supported")
                    }
                };

                map! {
                    s =>
                    kind: "struct",
                    fields: &SerializeStructFields { ctx, item: fields },
                    unknown_fields: &SerializeType { ctx, item: unknown_fields }
                }
            }
            schema::Type::Enum { variants: _ } => {
                unreachable!("schema simplification pass should rewrite enums")
            }
            schema::Type::Tuple { fields } => map! {
                s => kind: "tuple", fields: &SerializeTypes { ctx, item: fields }
            },
            schema::Type::Array { size, ty } => map! { s =>
                kind: "array",
                size: size,
                element: &Self { ctx, item: ty }
            },
            schema::Type::Map { key, value } => map! { s =>
                kind: "map",
                key: &Self { ctx, item: key },
                value: &Self { ctx, item: value }
            },
            schema::Type::Type { ty } => map! { s => kind: match ty {
                schema::ValueType::Null => "null",
                schema::ValueType::Bool => "bool",
                schema::ValueType::Integer => "integer",
                schema::ValueType::Double => "double",
                schema::ValueType::String => "string",
                schema::ValueType::Array => "json_array",
                schema::ValueType::Object => "json_object",
            } },
            schema::Type::Any => map! { s => kind: "any" },
            schema::Type::Constant { value } => {
                map! { s => kind: "constant", json: &serde_json::Value::from(*value) }
            }
        }
        s.end()
    }
}

impl<'a> serde::Serialize for SerializeTypes<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.iter().map(|ty| SerializeType { ctx: self.ctx, item: *ty }))
    }
}

impl<'a> serde::Serialize for SerializeAliases<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_map(
            self.iter().map(|(id, ty)| (*id, SerializeType { ctx: self.ctx, item: *ty })),
        )
    }
}

impl<'a> serde::Serialize for SerializeStructField<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let ctx = self.ctx;
        let mut s = serializer.serialize_map(None)?;
        map! { s =>
            key: &self.key,
            value: &SerializeType { ctx, item: self.value },
            optional: &self.optional,
        }
        s.end()
    }
}

impl<'a> serde::Serialize for SerializeStructFields<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(
            self.iter().map(|field| SerializeStructField { ctx: self.ctx, item: field }),
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_serialize_types() {
        struct MyAliasStruct;
        schema! {
            type MyAliasStruct = ();
        }

        let mut ctx = SerializeCtx { alias_id_map: HashMap::new(), aliases: vec![] };

        ctx.alias_id_map.insert(std::any::TypeId::of::<MyAliasStruct>(), "alias_id");

        let ctx = &Root { ctx, root: &schema::Type::Void };

        let case = |ty: &schema::Type<'_>, json: &serde_json::Value| {
            assert_eq!(&serde_json::to_value(&SerializeType { ctx, item: ty }).unwrap(), json);
        };

        case(&schema::Type::Void, &serde_json::json!({"kind": "void"}));

        case(
            &schema::Type::Union(&[&schema::Type::Void, &schema::Type::Any]),
            &serde_json::json!({"kind": "union", "types": [{"kind": "void"}, {"kind": "any"}]}),
        );

        case(
            <MyAliasStruct as schema::Schema>::TYPE,
            &serde_json::json!({"kind": "alias", "id": "alias_id"}),
        );

        struct TestStruct;
        schema! {
            #[transparent]
            type TestStruct = struct { a: u32, b: bool, c?: String };
        }

        case(
            <TestStruct as schema::Schema>::TYPE,
            &serde_json::json!({
                "kind": "struct",
                "fields": [
                    {"key": "a", "value": {"kind": "integer"}, "optional": false},
                    {"key": "b", "value": {"kind": "bool"}, "optional": false},
                    {"key": "c", "value": {"kind": "string"}, "optional": true},
                ],
                "unknown_fields": {"kind": "any"},
            }),
        );

        case(
            <[u32] as schema::Schema>::TYPE,
            &serde_json::json!({
                "kind": "array",
                "size": null,
                "element": {"kind": "integer"},
            }),
        );

        case(
            <[u32; 8] as schema::Schema>::TYPE,
            &serde_json::json!({
                "kind": "array",
                "size": 8,
                "element": {"kind": "integer"},
            }),
        );

        case(
            <json::Map<String, u32> as schema::Schema>::TYPE,
            &serde_json::json!({
                "kind": "map",
                "key": {"kind": "string"},
                "value": {"kind": "integer"},
            }),
        );

        const TYPE_TO_KIND: &'static [(schema::ValueType, &'static str)] = &[
            (schema::ValueType::Null, "null"),
            (schema::ValueType::Bool, "bool"),
            (schema::ValueType::Integer, "integer"),
            (schema::ValueType::Double, "double"),
            (schema::ValueType::String, "string"),
            (schema::ValueType::Array, "json_array"),
            (schema::ValueType::Object, "json_object"),
        ];

        for (ty, kind) in TYPE_TO_KIND {
            case(
                &schema::Type::Type { ty: *ty },
                &serde_json::json!({
                    "kind": kind,
                }),
            );
        }

        case(
            &schema::Type::Any,
            &serde_json::json!({
                "kind": "any",
            }),
        );

        case(
            &schema::Type::Constant { value: &schema::InlineValue::Bool(true) },
            &serde_json::json!({
                "kind": "constant",
                "json": true,
            }),
        );
    }

    #[test]
    fn test_serialize_machine_schema() {
        let mut schema = <MachineSchema as schema::Schema>::TYPE;
        let bump = bumpalo::Bump::new();
        let mut ctx = simplify::Ctx::new();
        ctx.process_type(&bump, &mut schema);

        let actual =
            serde_json::to_value(&schema::output::machine::serialize(&bump, &ctx, schema)).unwrap();

        eprintln!("{}\n\n\n", serde_json::to_string(&actual).unwrap());

        let expected = serde_json::json!({
            "alias":{"0000": {"kind":"union","types":[
                {"fields":[{"key":"kind","optional":false,"value":{"kind":"union","types":[{"kind":"union","types":[
                    {"json":"bool","kind":"constant"},
                    {"json":"double","kind":"constant"},
                    {"json":"integer","kind":"constant"},
                    {"json":"json_array","kind":"constant"},
                    {"json":"json_object","kind":"constant"},
                    {"json":"null","kind":"constant"},
                    {"json":"string","kind":"constant"}
                ]},{"json":"any","kind":"constant"}]}}],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[{"key":"fields","optional":false,"value":{"element":{"id":"0000","kind":"alias"},"kind":"array","size":null}},{"key":"kind","optional":false,"value":{"json":"tuple","kind":"constant"}}],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[{"key":"id","optional":false,"value":{"kind":"string"}},{"key":"kind","optional":false,"value":{"json":"alias","kind":"constant"}}],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[{"key":"json","optional":false,"value":{"kind":"any"}},{"key":"kind","optional":false,"value":{"json":"constant","kind":"constant"}}],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[{"key":"kind","optional":false,"value":{"json":"union","kind":"constant"}},{"key":"types","optional":false,"value":{"element":{"id":"0000","kind":"alias"},"kind":"array","size":null}}],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[{"key":"element","optional":false,"value":{"id":"0000","kind":"alias"}},{"key":"kind","optional":false,"value":{"json":"array","kind":"constant"}},{"key":"size","optional":false,"value":{"kind":"union","types":[{"kind":"null"},{"kind":"integer"}]}}],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[
                    {"key":"fields","optional":false,"value":{"element":{"fields":[
                        {"key":"key","optional":false,"value":{"kind":"string"}},
                        {"key":"optional","optional":false,"value":{"kind":"bool"}},
                        {"key":"value","optional":false,"value":{"id":"0000","kind":"alias"}}
                    ],"kind":"struct","unknown_fields":{"kind":"any"}},"kind":"array","size":null}},
                    {"key":"kind","optional":false,"value":{"json":"struct","kind":"constant"}},
                    {"key":"unknown_fields","optional":false,"value":{"id":"0000","kind":"alias"}}
                ],"kind":"struct","unknown_fields":{"kind":"any"}},
                {"fields":[{"key":"key","optional":false,"value":{"id":"0000","kind":"alias"}},{"key":"kind","optional":false,"value":{"json":"map","kind":"constant"}},{"key":"value","optional":false,"value":{"id":"0000","kind":"alias"}}],"kind":"struct","unknown_fields":{"kind":"any"}}
            ]}},
            "root":{"fields":[
                {"key":"alias","optional":false,"value":{"key":{"kind":"string"},"kind":"map","value":{"id":"0000","kind":"alias"}}},
                {"key":"root","optional":false,"value":{"id":"0000","kind":"alias"}}
            ],"kind":"struct","unknown_fields":{"kind":"any"}}
        });

        if let Err(_) = crate::validate::compare::ValidationTestDiff::check(&actual, &expected) {
            panic!("Simplified schema does not match expected schema");
        }

        // Validate that the serialized machine schema matches the schema itself.
        // Additionally, match against the simplified schema to ensure it is equivalent to the original schema.
        crate::validate::validate(<MachineSchema as schema::Schema>::TYPE, &actual).unwrap();
        crate::validate::validate(schema, &actual).unwrap();
    }
}
