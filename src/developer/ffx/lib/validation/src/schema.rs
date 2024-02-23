// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{any::TypeId, fmt::Display, ops::ControlFlow};

mod inline_value;
mod macros;

use ffx_validation_proc_macro::schema;
pub use ffx_validation_proc_macro::Schema;
pub use inline_value::InlineValue;

pub type StaticType = &'static Type<'static>;

/// An enum that represents the entire type hierarchy to match against a value.
///
/// Types can be constructed at compile time in const contexts, and can be
/// constructed at runtime with a bump allocator like `bumpalo::Bump`.
///
/// Referenced types must be non-recursive, with exception to alias types.
///
/// This structure must not own any types that must be dropped, like `Box`, `Vec`
/// etc. These types are also not constructable in const contexts.
pub enum Type<'a> {
    /// An empty type, where no value is expected.
    ///
    /// When placed in a union type this is functionally a no-op.
    Void,

    // Composites:
    /// A union consisting of multiple accepted types.
    Union(&'a [&'a Type<'a>]),
    /// A named type alias that allows for recursion and deduplication.
    // TODO: Once type_name and TypeId are allowed in const contexts remove these function wrappers
    Alias { name: fn() -> &'a str, id: fn() -> TypeId, ty: RecursiveType<'a> },
    /// A structural type consisting of named fields.
    ///
    /// For special behavior for unknown fields, specify a [StructExtras] option.
    Struct { fields: &'a [Field<'a, &'a Type<'a>>], extras: Option<StructExtras<'a>> },
    /// A discriminated union where variants are named.
    Enum { variants: &'a [(&'a str, &'a Type<'a>)] },
    /// A fixed-size heterogeneous collection of types.
    Tuple { fields: &'a [&'a Type<'a>] },
    /// A homogeneous collection of types, with an optional minimum size.
    Array { size: Option<usize>, ty: &'a Type<'a> },
    /// A collection of key value type pairs.
    Map { key: &'a Type<'a>, value: &'a Type<'a> },

    // Terminals:
    /// A singular JSON value type.
    Type { ty: ValueType },
    /// A wildcard type that matches any value (except "absent" values).
    ///
    /// When placed in a union type all other types are functionally a no-op.
    Any,
    /// A JSON value constant, which must match 1:1.
    Constant { value: &'a InlineValue<'a> },
}

/// Workaround to allow recursive types to be referenced in const contexts.
///
/// When building types at runtime [`RecursiveType::Plain`] can be used for types by-reference,
/// while [`RecursiveType::Fn`] is used in const contexts.
#[derive(Copy, Clone)]
pub enum RecursiveType<'a> {
    Plain(&'a Type<'a>),
    Fn(fn() -> &'static Type<'static>),
}

// This custom Debug impl is needed to skip traversal into aliases, which are allowed to recurse.
impl<'a> std::fmt::Debug for Type<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Void => write!(f, "Void"),
            Self::Union(arg0) => f.debug_tuple("Union").field(arg0).finish(),
            Self::Alias { name, id, ty: _ } => {
                let name = name();
                let id = id();
                f.debug_struct("Alias")
                    .field("name", &name)
                    .field("id", &id)
                    .field("ty", &"<omitted>")
                    .finish()
            }
            Self::Struct { fields, extras } => {
                f.debug_struct("Struct").field("fields", fields).field("extras", extras).finish()
            }
            Self::Enum { variants } => f.debug_struct("Enum").field("variants", variants).finish(),
            Self::Tuple { fields } => f.debug_struct("Tuple").field("fields", fields).finish(),
            Self::Array { size, ty } => {
                f.debug_struct("Array").field("size", size).field("ty", ty).finish()
            }
            Self::Map { key, value } => {
                f.debug_struct("Map").field("key", key).field("value", value).finish()
            }
            Self::Type { ty } => f.debug_struct("Type").field("ty", ty).finish(),
            Self::Any => write!(f, "Any"),
            Self::Constant { value } => f.debug_struct("Constant").field("value", value).finish(),
        }
    }
}

impl<'a> Type<'a> {
    /// Types are used in contexts where the drop handler cannot or should not run.
    ///
    /// This verifies that the drop handler does not need to be called, as Rust types can drop
    /// their fields without implementing the [`Drop`] trait.
    #[allow(dead_code)]
    const ASSERT_NO_DROP: () = {
        assert!(!std::mem::needs_drop::<Type<'a>>());
    };

    // Forces `ASSERT_NO_DROP` to be evaluated at compile time.
    const fn _assert_no_drop() {
        Self::ASSERT_NO_DROP
    }

    /// Walks through a schema type's structure and collects its constituents.
    ///
    /// The walk process can exit early with [ControlFlow::Break]. Generally only the [Walker]
    /// should request an early exit.
    pub fn walk<W: Walker<'a> + ?Sized>(&'a self, walker: &mut W) -> ControlFlow<()> {
        match self {
            Type::Void => ControlFlow::Continue(()),
            Type::Union(tys) => {
                for ty in *tys {
                    ty.walk(walker)?
                }
                ControlFlow::Continue(())
            }
            Type::Alias { name, id, ty } => walker.add_alias(name(), id(), ty),
            Type::Struct { fields, extras } => walker.add_struct(fields, *extras),
            Type::Enum { variants } => walker.add_enum(variants),
            Type::Tuple { fields } => walker.add_tuple(fields),
            Type::Array { size, ty } => walker.add_array(*size, ty),
            Type::Map { key, value } => walker.add_map(key, value),
            Type::Type { ty } => walker.add_type(*ty),
            Type::Any => walker.add_any(),
            Type::Constant { value } => walker.add_constant(value),
        }
    }

    pub(crate) fn is_leaf(&self) -> bool {
        match self {
            Type::Void | Type::Any | Type::Type { .. } | Type::Constant { .. } => true,
            Type::Union(..)
            | Type::Alias { .. }
            | Type::Struct { .. }
            | Type::Enum { .. }
            | Type::Tuple { .. }
            | Type::Array { .. }
            | Type::Map { .. } => false,
        }
    }
}

impl<'a> std::fmt::Debug for RecursiveType<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl<'a> From<RecursiveType<'a>> for &'a Type<'a> {
    fn from(value: RecursiveType<'a>) -> Self {
        match value {
            RecursiveType::Plain(ty) => ty,
            RecursiveType::Fn(f) => (f)(),
        }
    }
}

impl<'a> AsRef<Type<'a>> for RecursiveType<'a> {
    fn as_ref(&self) -> &'a Type<'a> {
        match *self {
            Self::Plain(ty) => ty,
            Self::Fn(f) => (f)(),
        }
    }
}

impl<'a> std::ops::Deref for RecursiveType<'a> {
    type Target = Type<'a>;

    fn deref(&self) -> &'a Self::Target {
        match *self {
            Self::Plain(ty) => ty,
            Self::Fn(f) => (f)(),
        }
    }
}

/// Enum variants covering all JSON types.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ValueType {
    Null,
    Bool,
    Integer,
    Double,
    String,
    Array,
    Object,
}

impl<'a> From<&'a serde_json::Value> for ValueType {
    fn from(value: &'a serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(_) => Self::Bool,
            serde_json::Value::Number(n) => {
                if n.is_f64() {
                    Self::Double
                } else {
                    Self::Integer
                }
            }
            serde_json::Value::String(_) => Self::String,
            serde_json::Value::Array(_) => Self::Array,
            serde_json::Value::Object(_) => Self::Object,
        }
    }
}

impl Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ValueType::Null => "null",
            ValueType::Bool => "bool",
            ValueType::Integer => "int",
            ValueType::Double => "double",
            ValueType::String => "string",
            ValueType::Array => "array",
            ValueType::Object => "object",
        })
    }
}

/// Specifies the behavior of extra fields not declared by the struct.
#[derive(Copy, Clone, Debug)]
pub enum StructExtras<'a> {
    /// Extra, unknown, struct fields cause a deserialization error.
    Deny,
    /// Remaining struct fields are passed to the given types.
    ///
    /// Invalid if a type is not a struct or [ValueType::Object].
    Flatten(&'a [&'a Type<'a>]),
}

/// Represents a singular field within a struct.
#[derive(Copy, Clone, Debug)]
pub struct Field<'a, T = &'a Type<'a>> {
    pub key: &'a str,
    pub value: T,

    /// Whether the field is allowed to be absent.
    pub optional: bool,
}

pub const FIELD: Field<'_> = Field { key: "", value: &Type::Void, optional: false };

impl<'a, T: Copy> Field<'a, T> {
    pub const fn optional(self) -> Self {
        Self { optional: true, ..self }
    }

    pub const fn is_optional(&self) -> bool {
        self.optional
    }
}

#[macro_export]
macro_rules! field {
    ($name:tt: $t:ty) => {
        $crate::schema::Field {
            key: stringify!($name),
            value: <$t as $crate::schema::Schema>::TYPE,
            ..$crate::schema::FIELD
        }
    };
    ($name:tt: $l:literal) => {
        $crate::schema::Field {
            key: stringify!($name),
            value: $crate::schema::Type::Constant(|| json!($l)),
            ..$crate::schema::FIELD
        }
    };
}

/// A trait that walks through a type and collects information about its structure.
///
/// During traversal, implementations return its interest in additional follow-up types with
/// [ControlFlow::Continue], or [ControlFlow::Break] to exit traversal.
pub trait Walker<'a> {
    /// Add a type alias as a part of the schema type.
    fn add_alias(&mut self, name: &'a str, id: TypeId, ty: &'a Type<'a>) -> ControlFlow<()>;

    /// Add a struct to the schema type.
    ///
    /// For special behavior for unknown fields, specify a [StructExtras] option.
    fn add_struct(
        &mut self,
        fields: &'a [Field<'a>],
        extras: Option<StructExtras<'a>>,
    ) -> ControlFlow<()>;

    /// Add a serde-style enum to the schema type.
    fn add_enum(&mut self, variants: &'a [(&'a str, &'a Type<'a>)]) -> ControlFlow<()>;

    /// Add a tuple of types to the schema type.
    fn add_tuple(&mut self, fields: &'a [&'a Type<'a>]) -> ControlFlow<()>;

    /// Add an array to the schema type, with either a fixed or dynamic size.
    fn add_array(&mut self, size: Option<usize>, ty: &'a Type<'a>) -> ControlFlow<()>;

    /// Add a map of key values to the schema type.
    fn add_map(&mut self, key: &'a Type<'a>, value: &'a Type<'a>) -> ControlFlow<()>;

    // Terminals:

    /// Add a plain JSON type to the schema type.
    fn add_type(&mut self, ty: ValueType) -> ControlFlow<()>;

    /// Add a wildcard type to the schema type.
    fn add_any(&mut self) -> ControlFlow<()>;

    /// Add a JSON constant to the schema type.
    fn add_constant(&mut self, value: &'a InlineValue<'a>) -> ControlFlow<()>;
}

pub trait Schema {
    const TYPE: StaticType;
}

// Base schemas for JSON values:

macro_rules! json_types {
    ($($name:ident),*) => {
        $(
            pub struct $name;

            impl Schema for $name {
                const TYPE: StaticType = &Type::Type { ty: ValueType::$name };
            }
        )*
    };
}

pub mod json {
    use std::marker::PhantomData;

    use super::*;

    json_types! {
        Null,
        Bool,
        Integer,
        Double,
        String,
        Array,
        Object
    }

    pub struct Any;
    impl Schema for Any {
        const TYPE: StaticType = &Type::Any;
    }

    pub struct Map<K, V> {
        _phantom_data: PhantomData<(K, V)>,
    }
    impl<K: Schema, V: Schema> Schema for Map<K, V> {
        const TYPE: StaticType = &Type::Map { key: K::TYPE, value: V::TYPE };
    }
}

// Schemas for common types

pub struct Nothing;
impl Schema for Nothing {
    const TYPE: StaticType = &Type::Void;
}

macro_rules! impl_prim {
    ($ty:ident: $($impl_ty:ty)*) => {
        $(
            impl Schema for $impl_ty {
                const TYPE: StaticType = json::$ty::TYPE;
            }
        )*
    };
}

impl_prim!(Integer: u64 u32 u16 u8 i64 i32 i16 i8);
impl_prim!(Double: f32 f64);
impl_prim!(Bool: bool);
impl_prim!(String: str String);
impl_prim!(Any: serde_json::Value);

impl Schema for () {
    const TYPE: StaticType = json::Null::TYPE;
}

impl<T: Schema> Schema for [T] {
    const TYPE: StaticType = &Type::Array { size: None, ty: T::TYPE };
}

impl<const N: usize, T: Schema> Schema for [T; N] {
    const TYPE: StaticType = &Type::Array { size: Some(N), ty: T::TYPE };
}

macro_rules! make_tuple {
    ($first:ident, $($id:ident,)*) => {
        impl<$first: Schema, $($id: Schema,)*> Schema for ($first, $($id,)*) {
            const TYPE: StaticType = &Type::Tuple {
                fields: &[
                    $first::TYPE,
                    $($id::TYPE,)*
                ]
            };
        }

        make_tuple!($($id,)*);
    };
    () => {}
}

make_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P,);

schema! {
    #[transparent]
    impl<T: Schema> for Option<T> = T?;

    #[transparent]
    impl<T: Schema> for Box<T> = T;

    #[transparent]
    impl<T: Schema> for Vec<T> = [T];

    #[transparent]
    impl<K: Schema + Eq + std::hash::Hash, V: Schema> for std::collections::HashMap<K, V> =
        json::Map::<K, V>;

    #[transparent]
    impl<K: Schema + Ord, V: Schema> for std::collections::BTreeMap<K, V> = json::Map::<K, V>;
}
