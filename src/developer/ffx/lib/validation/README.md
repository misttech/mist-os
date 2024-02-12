# Schema and Validation Internals

This covers:

* Schema representation, and how it maps to JSON-like outputs
* How the procedural and derive macro build the `Schema` trait impl
* Piecewise JSON validation

## Schema Representation

Similar to Serde, schemas describe structured types and include escape hatches
for free-form JSON value types. Nested types are not allowed to be recursive,
with one exception.

The schema leaf types consist of:

* `Void`: a value is not expected
* `Any`: any value is expected
* `ValueType`: a value of a specific JSON type is expected
* `Constant`: a value is expected to match a JSON snippet

The schema composite types consist of:

* `Union`: at least one type is expected to match
* `Alias`: a unique named type that is functionally equivalent to the type it aliases
  * Due to its uniqueness it is allowed to be recursive.
* `Struct`: an object that consists of optional and required named fields
  * Unknown fields can be denied or delegated to a struct or JSON map.
* `Enum`: shorthand for a Serde-like enum, where variants may have required data
  * A data-less variant is represented as the `Void` type. It desugars into a
    string constant with the variant's name
  * A variant with data desugars into the form
    `{"<Variant Name>": <Variant Data>}`
* `Tuple`: a fixed-length array of heterogeneous values
* `Array`: a homogenous array of types with an optional fixed length
* `Map`: a free-form JSON object that expects keys and values to match the given types

Schemas are implemented by-handle for some core Rust types, like integers,
slices, and arrays.

### Recursive Aliases

Some subtools utilize recursive types to represent tree data structures. Because of
this, `Type::Alias` is allowed to reference itself as part of its structure.

Other composite types are non-recursive. For example, a `Union` cannot contain
itself _except_ if that reference is behind an `Alias`.

### Schema Types and Ownership

The schema structure (`schema::Type`) does not own any data, including
the subtypes in composites (like `Union`, `Struct`, etc).

This is required because:

* Rust types cannot contain themselves by value, only by reference.
* Const contexts cannot use heap allocated types like `Box`
* Types are used in contexts where they are not dropped (for example in a bump allocator)

Every subtype within the schema structure is instead behind a reference, and the `'a`
lifetime parameter allows these types to be allocated at runtime.

### Creating Types at Runtime with Bumpalo

Bumpalo is the de-facto way to temporarily construct `schema::Type`s at runtime.
Types allocated via Bumpalo are owned by the bump allocator, and all types allocated
in it are deallocated when the bump allocator is dropped.

One caveat is that drop handlers for values allocated in the bump allocator never run.
A `Box` within a bump allocated value is never freed, an `Arc` in a bump allocated type
never decreases the reference count, a `File` in a bump allocated type never closes the
file handle. This is another reason why `schema::Type` does not own any of its data.

Here's an example of runtime type creation using Bumpalo:

```rust
let bump = bumpalo::Bump::new();

let five_string_tuple: &Type<'_> = bump.alloc(
  Type::Tuple {
    fields: bump.alloc_slice_fill_copy(5, String::TYPE),
  }
);
```

## Procedural and Derive macros

For documentation on the macro syntax and attributes, see the doc comments in
`proc_macro/src/lib.rs`. The documentation is also surfaced through Rust
Analyzer when hovering over a use-site of the macros.

Both macros build a `Schema` trait impl.

For example, with the following schema:

```rust
schema! {
  #[transparent]
  impl<T: Schema> for Option<T> = T?;
}
```

the following trait impl is generated:

```rust
impl<T: Schema> ::ffx_validation::Schema for Option<T> {
    const TYPE: &'static ::ffx_validation::schema::Type<'static> =
        &::ffx_validation::schema::Type::Union(&[
            <T as ::ffx_validation::Schema>::TYPE,
            &::ffx_validation::schema::Type::Type { ty: ::ffx_validation::schema::ValueType::Null },
        ]);
}
```

which expects a value matching either the schema of the type `T` or null.

Similarly, the derive macro generates a similar trait impl from Rust structs and
enums:

```rust
#[derive(Schema)]
struct DeriveStruct {
    a: u32,
    b: String,
    c: Option<u32>,
}
```

```rust
impl ::ffx_validation::Schema for DeriveStruct
where
    u32: ::ffx_validation::Schema,
    String: ::ffx_validation::Schema,
    Option<u32>: ::ffx_validation::Schema,
{
    const TYPE: &'static ::ffx_validation::schema::Type<'static> =
        &::ffx_validation::schema::Type::Alias {
            name: ::std::any::type_name::<Self>,
            id: ::std::any::TypeId::of::<Self>,
            ty: ::ffx_validation::schema::RecursiveTypeBarrier::Type(
                &::ffx_validation::schema::Type::Struct {
                    fields: &[
                        ::ffx_validation::schema::Field {
                            key: "a",
                            value: <u32 as ::ffx_validation::Schema>::TYPE,
                            optional: false,
                            ..::ffx_validation::schema::FIELD
                        },
                        ::ffx_validation::schema::Field {
                            key: "b",
                            value: <String as ::ffx_validation::Schema>::TYPE,
                            optional: false,
                            ..::ffx_validation::schema::FIELD
                        },
                        ::ffx_validation::schema::Field {
                            key: "c",
                            value: <Option<u32> as ::ffx_validation::Schema>::TYPE,
                            optional: false,
                            ..::ffx_validation::schema::FIELD
                        },
                    ],
                    extras: None,
                },
            ),
        };
}
```

### Macro Input to Schema Output Coverage

Not all schema types are representable by the procedural and derive macros.
These types are instead manually written in `src/schema.rs`.

Here's a rundown of macro inputs that generate outputs for each type:

#### Void

`Type::Void` cannot be generated using the schema macros directly.
`schema::Nothing` is implemented by hand to allow it to be accessed.

#### Union

Union types can only be generated from the procedural macro.

`schema! { type MyType = u32 | String; }`

Generates:

`Type::Union(&[u32::TYPE, String::TYPE])`

#### Alias

Type aliases can be generated from both the procedural and derive macro. They
are generated for all declarations by default.

`schema! { type MyStruct = u32; }`

`#[derive(Schema)] struct MyStruct(u32);`

Generates:

```rust
Type::Alias {
  name: std::any::type_name::<MyStruct>,
  id: std::any::TypeId::of::<MyStruct>,
  ty: u32::TYPE
}
```

#### Struct

Structs can be generated from both the procedural and derive macro. Optional
fields (fields that are not always present) currently cannot be generated from
the derive macro.

`schema! { type MyStruct = struct { a: u32, b: f32, c: String } }`

```rust
#[derive(Schema)]
struct MyStruct {
  a: u32,
  b: f32,
  c: String,
}
```

Generates:

```rust
Type::Alias {
  name: std::any::type_name::<MyStruct>,
  id: std::any::TypeId::of::<MyStruct>,
  ty: &Type::Struct {
    fields: &[
      Field { key: "a", value: u32::TYPE, optional: false },
      Field { key: "b", value: f32::TYPE, optional: false },
      Field { key: "c", value: String::TYPE, optional: false },
    ]
  }
}
```

#### Enums

Enums can be generated from both the procedural and derive macro.

```rust
schema! {
  type Enum = enum {
      A, B, C(String), D { field: u32, b: u32 },
  };
}
```

```rust
#[derive(Schema)]
enum Enum {
  A, B, C(String), D { field: u32, b: u32 }
}
```

Generates:

```rust
Type::Alias {
  name: std::any::type_name::<Enum>,
  id: std::any::TypeId::of::<Enum>,
  ty: &Type::Enum {
    variants: &[
      ("A", &Type::Void),
      ("B", &Type::Void),
      ("C", String::TYPE),
      ("D", &Type::Struct {
        fields: &[
          Field { key: "field", value: u32::TYPE, optional: false },
          Field { key: "b", value: u32::TYPE, optional: false },
        ]
      }),
    ]
  }
}
```

#### Tuples

Tuples cannot be _directly_ generated by either macro. However the derive macro
generates it for tuple structs. The procedural macro instead depends on the
schemas generated by the `make_tuple` macro in `src/schema.rs`.

```rust
#[derive(Schema)]
struct TupleStruct(u32, String, bool);
```

Generates:

```rust
Type::Alias {
  name: std::any::type_name::<TupleStruct>,
  id: std::any::TypeId::of::<TupleStruct>,
  ty: &Type::Tuple {
    fields: &[
      u32::TYPE,
      String::TYPE,
      bool::TYPE,
    ]
  }
}
```

#### Arrays

Array types cannot be directly generated by either macro. Both fixed-size and
unsized arrays are manually implemented on `[T; N]` and `[T]` in `src/schema.rs`.

#### Map

Map types cannot be directly generated by either macro. It is manually
implemented on `HashMap` and `BTreeMap` in `src/schema.rs`.

#### Type (ValueType) and Any

Value types cannot be directly generated by the schema macro. The null value
type is generated by the nullable `?` suffix. The marker types manually
implemented in `schema::json` must be used instead.

`schema! { #[transparent] type Option<T> = T?; }`

Generates:

```rust
Type::Union(&[
  T::TYPE,
  &Type::Type { ty: ValueType::Null },
])
```

#### Constants

String, integer, float, and boolean constants can be generated from the
procedural macro. Array and object constants are not parsed yet.

`schema! { #[transparent] type TaggedStructs = struct { tag: "a" } | struct { tag: "b" } }`

Generates:

```rust
Type::Union(&[
  &Type::Struct {
    fields: &[
      Field { key: "tag", value: &Type::Constant { value: &InlineValue::String("a") }, optional: false },
    ]
  },
  &Type::Struct {
    fields: &[
      Field { key: "tag", value: &Type::Constant { value: &InlineValue::String("b") }, optional: false },
    ]
  },
])
```

### Macro Debugging

Rust Analyzer works exceptionally well for debugging the expansion of procedural
and derive macros. This can be done with the "Expand macro recursively at caret"
action with the Rust Analyzer extension in Visual Studio Code.

To expand the derive macro the caret must be placed on `Schema` within the derive attribute.

When modifying the macro, it must be rebuild and Rust Analyzer needs to reload
the workspace. `fx build host_<arch>/ffx_validation_lib_test` is the easiest way
to rebuild the macro.

## Validation

Schema validation is implemented by a pairwise walk through the JSON value and
expected schema.

For example, matching the JSON value:

```json
{"integer": 123, "structure": {"string": "a string"}}
```

against the schema type:

```rust
schema! {
  type MyStruct = struct {
    integer: u32,
    structure: struct {
      string: String,
    },
  };
}
```

will validate at the outer level, then traverse into the `integer` and
`structure` field in both the schema and json value.

Validation errors are eagerly created during traversal, and are silenced
if validation succeeds for a value. For example: validating the union type
`String | u32` against `1234` will generate a type error when matching the
integer against the `String`, but will silence this error when the integer
matches the `u32`.
