// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{any::TypeId, fmt::Display, ops::ControlFlow};

mod macros;

pub use ffx_validation_proc_macro::Schema;

/// Walks through a schema type's structure and collects its constituents.
///
/// The walk process can exit early with [ControlFlow::Break]. Generally only the [Walker]
/// should request an early exit.
pub type Walk = for<'a> fn(&'a mut (dyn Walker + 'a)) -> ControlFlow<()>;

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
pub enum StructExtras {
    /// Extra, unknown, struct fields cause a deserialization error.
    Deny,
    /// Remaining struct fields are passed to the given types.
    ///
    /// Invalid if a type is not a struct or [ValueType::Object].
    Flatten(&'static [Walk]),
}

/// Represents a singular field within a struct.
#[derive(Copy, Clone)]
pub struct Field<T = Walk> {
    pub key: &'static str,
    pub value: T,

    /// Whether the field is allowed to be absent.
    pub optional: bool,
}

pub const FIELD: Field<Walk> = Field { key: "", value: |_| unreachable!(), optional: false };

impl<T: Copy> Field<T> {
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
            value: <$t as $crate::schema::Schema>::walk_schema,
            ..$crate::schema::FIELD
        }
    };
    ($name:tt: $l:literal) => {
        $crate::schema::Field {
            key: stringify!($name),
            value: |w| w.add_constant(json!($l))?.ok(),
            ..$crate::schema::FIELD
        }
    };
}

/// A trait that walks through a type and collects information about its structure.
///
/// During traversal, implementations return its interest in additional follow-up types with
/// [ControlFlow::Continue], or [ControlFlow::Break] to exit traversal.
pub trait Walker {
    /// No-op that always returns [ControlFlow::Continue].
    fn ok(&self) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    /// Add a type alias as a part of the schema type.
    fn add_alias(
        &mut self,
        name: &'static str,
        id: TypeId,
        ty: Walk,
    ) -> ControlFlow<(), &mut dyn Walker>;

    /// Add a struct to the schema type.
    ///
    /// For special behavior for unknown fields, specify a [StructExtras] option.
    fn add_struct(
        &mut self,
        fields: &'static [Field],
        extras: Option<StructExtras>,
    ) -> ControlFlow<(), &mut dyn Walker>;

    /// Add a serde-style enum to the schema type.
    fn add_enum(
        &mut self,
        variants: &'static [(&'static str, Walk)],
    ) -> ControlFlow<(), &mut dyn Walker>;

    /// Add a tuple of types to the schema type.
    fn add_tuple(&mut self, fields: &'static [Walk]) -> ControlFlow<(), &mut dyn Walker>;

    /// Add an array to the schema type, with either a fixed or dynamic size.
    fn add_array(&mut self, size: Option<usize>, ty: Walk) -> ControlFlow<(), &mut dyn Walker>;

    /// Add a map of key values to the schema type.
    fn add_map(&mut self, key: Walk, value: Walk) -> ControlFlow<(), &mut dyn Walker>;

    // Terminals:

    /// Add a plain JSON type to the schema type.
    fn add_type(&mut self, ty: ValueType) -> ControlFlow<(), &mut dyn Walker>;

    /// Add a wildcard type to the schema type.
    fn add_any(&mut self) -> ControlFlow<(), &mut dyn Walker>;

    /// Add a JSON constant to the schema type.
    fn add_constant(&mut self, value: serde_json::Value) -> ControlFlow<(), &mut dyn Walker>;
}

pub trait Schema {
    // Macro creates a schema walker for the naive version. If the generated schema doesn't match
    // then it must be written manually.
    //
    // Benefits of walking schemas vs returning a huge object:
    //  * Lower compile time (functions are monomorphic)
    //  * Faster runtime (no need to do allocations)
    //  * Streaming schema validation (walk functions are stateless & static)
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()>;
}

// Base schemas for JSON values:

macro_rules! json_types {
    ($($name:ident),*) => {
        $(
            pub struct $name;

            impl Schema for $name {
                fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
                    walker.add_type(ValueType::$name)?.ok()
                }
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
        fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
            walker.add_any()?.ok()
        }
    }

    pub struct Map<K, V> {
        _phantom_data: PhantomData<(K, V)>,
    }
    impl<K: Schema, V: Schema> Schema for Map<K, V> {
        fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
            walker.add_map(K::walk_schema, V::walk_schema)?.ok()
        }
    }
}

// Schemas for common types

pub struct Nothing;
impl Schema for Nothing {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        walker.ok()
    }
}

pub fn nothing(walker: &mut dyn Walker) -> ControlFlow<()> {
    Nothing::walk_schema(walker)
}

macro_rules! impl_prim {
    ($ty:ident: $($impl_ty:ty)*) => {
        $(
            impl Schema for $impl_ty {
                fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
                    json::$ty::walk_schema(walker)
                }
            }
        )*
    };
}

// TODO: Migrate to use the schema macro when #[transparent] type aliases are implemented
impl_prim!(Integer: u64 u32 u16 u8 i64 i32 i16 i8);
impl_prim!(Double: f32 f64);
impl_prim!(Bool: bool);
impl_prim!(String: str String);
impl_prim!(Any: serde_json::Value);

impl<T: Schema> Schema for Option<T> {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        T::walk_schema(walker)?;
        json::Null::walk_schema(walker)
    }
}

impl<T: Schema> Schema for Box<T> {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        T::walk_schema(walker)
    }
}

impl<T: Schema> Schema for Vec<T> {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        <[T] as Schema>::walk_schema(walker)
    }
}

impl<K: Schema + Eq + std::hash::Hash, V: Schema> Schema for std::collections::HashMap<K, V> {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        json::Map::<K, V>::walk_schema(walker)
    }
}

impl<K: Schema + Ord, V: Schema> Schema for std::collections::BTreeMap<K, V> {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        json::Map::<K, V>::walk_schema(walker)
    }
}

impl Schema for () {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        json::Null::walk_schema(walker)
    }
}

impl<T: Schema> Schema for [T] {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        walker.add_array(None, T::walk_schema)?.ok()
    }
}

impl<const N: usize, T: Schema> Schema for [T; N] {
    fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
        walker.add_array(Some(N), T::walk_schema)?.ok()
    }
}

macro_rules! make_tuple {
    ($first:ident, $($id:ident,)*) => {
        impl<$first: Schema, $($id: Schema,)*> Schema for ($first, $($id,)*) {
            fn walk_schema(walker: &mut dyn Walker) -> ControlFlow<()> {
                let fields: &[Walk] = &[
                    $first::walk_schema as Walk,
                    $($id::walk_schema as Walk,)*
                ];
                walker.add_tuple(fields)?.ok()
            }
        }

        make_tuple!($($id,)*);
    };
    () => {}
}

make_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P,);
