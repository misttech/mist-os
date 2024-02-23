// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod test {
    use crate::schema::{Field, Schema, Type, ValueType};
    use ffx_validation_proc_macro::schema;

    // Allow RA to expand derive macros.
    // Rust Analyzer cannot resolve the derive macro if "shadows" a trait.
    // See https://github.com/rust-lang/rust-analyzer/issues/7408
    #[cfg(rust_analyzer)]
    use ffx_validation_proc_macro::Schema;

    /// Destructures a value using a let binding, panicking when the match fails.
    macro_rules! let_assert {
        ($p:pat = $e:expr) => {
            let tmp = $e;
            let $p = tmp else { panic!("Match failed: {:?}", tmp) };
        };
    }

    /// Asserts that the two references refer to the same address.
    #[track_caller]
    fn assert_exact<T>(a: &T, b: &T) {
        assert!(std::ptr::addr_eq(a, b));
    }

    // Marker structs
    struct Optional;
    struct RustType;
    struct InlineStruct;
    struct Tuple;

    struct Union;

    struct Generic<T>(T);
    struct NotASchema<T>(T);

    struct Transparent;

    struct ForeignType;

    struct ForeignTypeMarker;

    struct Enum;

    struct RecursiveAlias;

    // Schema macro syntax tests.
    schema! {
        type RustType = Option<u32>;
        type Optional = String?;
        type InlineStruct = struct {
            a: u32,
            b: u32,
            c: u32,
            d?: u64,
        };
        type Tuple = (RustType, Optional, InlineStruct);

        type Union = RustType | Optional | InlineStruct;

        type Generic<T> where T: Schema + 'static = T;
        impl<T> for Generic<NotASchema<T>> where T: Schema + 'static = T;

        #[transparent] type Transparent = u32;

        #[foreign(ForeignType)] type ForeignTypeMarker = (i32, i32, i32);

        type Enum = enum {
            A, B, C(String), D { a: u32, b: u32 },
        };

        #[recursive]
        type RecursiveAlias = (u32, RecursiveAlias);
    }

    #[test]
    fn test_rust_type() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = RustType::TYPE);
        assert_eq!((name)(), std::any::type_name::<RustType>());
        assert_eq!((id)(), std::any::TypeId::of::<RustType>());

        // For Option<u32>, which is a union between u32 & null
        let_assert!(Type::Union([ty, null]) = &*ty);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);
        let_assert!(Type::Type { ty: ValueType::Null } = null);
    }

    #[test]
    fn test_optional() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = Optional::TYPE);
        assert_eq!((name)(), std::any::type_name::<Optional>());
        assert_eq!((id)(), std::any::TypeId::of::<Optional>());

        // For a union between String & null
        let_assert!(Type::Union([ty, null]) = &*ty);
        let_assert!(Type::Type { ty: ValueType::String } = ty);
        let_assert!(Type::Type { ty: ValueType::Null } = null);
    }

    #[test]
    fn test_inline_struct() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = InlineStruct::TYPE);
        assert_eq!((name)(), std::any::type_name::<InlineStruct>());
        assert_eq!((id)(), std::any::TypeId::of::<InlineStruct>());

        // For a struct with four integer fields, the last one being optional
        let_assert!(Type::Struct { fields: [a, b, c, d], extras: None } = &*ty);

        let_assert!(Field { key: "a", value: ty, optional: false } = a);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);

        let_assert!(Field { key: "b", value: ty, optional: false } = b);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);

        let_assert!(Field { key: "c", value: ty, optional: false } = c);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);

        let_assert!(Field { key: "d", value: ty, optional: true } = d);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);
    }

    #[test]
    fn test_tuple() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = Tuple::TYPE);
        assert_eq!((name)(), std::any::type_name::<Tuple>());
        assert_eq!((id)(), std::any::TypeId::of::<Tuple>());

        // For a tuple with three fields
        let_assert!(Type::Tuple { fields: &[ty_1, ty_2, ty_3] } = &*ty);

        assert_exact(ty_1, RustType::TYPE);
        assert_exact(ty_2, Optional::TYPE);
        assert_exact(ty_3, InlineStruct::TYPE);
    }

    #[test]
    fn test_union() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = Union::TYPE);
        assert_eq!((name)(), std::any::type_name::<Union>());
        assert_eq!((id)(), std::any::TypeId::of::<Union>());

        // For a union with three fields
        let_assert!(Type::Union(&[ty_1, ty_2, ty_3]) = &*ty);

        assert_exact(ty_1, RustType::TYPE);
        assert_exact(ty_2, Optional::TYPE);
        assert_exact(ty_3, InlineStruct::TYPE);
    }

    #[test]
    fn test_generic() {
        fn check<T: 'static, V: Schema + 'static>()
        where
            Generic<T>: Schema,
        {
            // Should introduce a type alias
            let_assert!(&Type::Alias { name, id, ty } = Generic::<T>::TYPE);
            assert_eq!((name)(), std::any::type_name::<Generic<T>>());
            assert_eq!((id)(), std::any::TypeId::of::<Generic<T>>());

            // That aliases directly to the give type
            assert_exact::<Type<'_>>(&*ty, V::TYPE);
        }

        check::<u32, u32>();
        check::<String, String>();
        check::<NotASchema<u32>, u32>();
        check::<NotASchema<String>, String>();
    }

    #[test]
    fn test_transparent() {
        // Should not introduce a type alias
        assert_exact(Transparent::TYPE, u32::TYPE);
    }

    #[test]
    fn test_foreign_type() {
        // Should introduce a type alias for the foreign type (not the marker)
        let_assert!(&Type::Alias { name, id, ty } = ForeignTypeMarker::TYPE);
        assert_eq!((name)(), std::any::type_name::<ForeignType>());
        assert_eq!((id)(), std::any::TypeId::of::<ForeignType>());

        // For a tuple with three fields
        let_assert!(Type::Tuple { fields: &[ty_1, ty_2, ty_3] } = &*ty);

        assert_exact(ty_1, i32::TYPE);
        assert_exact(ty_2, i32::TYPE);
        assert_exact(ty_3, i32::TYPE);
    }

    #[test]
    fn test_enum() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = Enum::TYPE);
        assert_eq!((name)(), std::any::type_name::<Enum>());
        assert_eq!((id)(), std::any::TypeId::of::<Enum>());

        // For an enum with four variants
        let_assert!(
            Type::Enum {
                variants: &[("A", variant_a), ("B", variant_b), ("C", variant_c), ("D", variant_d)]
            } = &*ty
        );

        // Variant A and B do not hold any data
        let_assert!(Type::Void = variant_a);
        let_assert!(Type::Void = variant_b);

        // Variant C holds a string
        assert_exact(variant_c, String::TYPE);

        // Variant D is a struct with two fields
        let_assert!(Type::Struct { fields: [field_a, field_b], extras: None } = variant_d);

        let_assert!(Field { key: "a", value: ty, optional: false } = *field_a);
        assert_exact(ty, u32::TYPE);

        let_assert!(Field { key: "b", value: ty, optional: false } = *field_b);
        assert_exact(ty, u32::TYPE);
    }

    #[test]
    fn test_recursive_alias() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = RecursiveAlias::TYPE);
        assert_eq!((name)(), std::any::type_name::<RecursiveAlias>());
        assert_eq!((id)(), std::any::TypeId::of::<RecursiveAlias>());

        // For a tuple with two fields, one being recursive.
        let_assert!(Type::Tuple { fields: &[ty_1, ty_2] } = ty.as_ref());

        let_assert!(Type::Type { ty: ValueType::Integer } = ty_1);
        let_assert!(Type::Alias { name, id, ty: _ } = ty_2);
        assert_eq!((name)(), std::any::type_name::<RecursiveAlias>());
        assert_eq!((id)(), std::any::TypeId::of::<RecursiveAlias>());
    }

    // Schema derive macro syntax tests

    #[derive(Schema)]
    #[allow(dead_code)]
    struct DeriveStruct {
        a: u32,
        b: String,
        c: Option<u32>,
    }

    #[test]
    fn test_derive_struct() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveStruct::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveStruct>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveStruct>());

        // For a struct with three fields
        let_assert!(Type::Struct { fields: [a, b, c], extras: None } = &*ty);

        let_assert!(Field { key: "a", value: ty, optional: false } = a);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);

        let_assert!(Field { key: "b", value: ty, optional: false } = b);
        let_assert!(Type::Type { ty: ValueType::String } = ty);

        let_assert!(Field { key: "c", value: ty, optional: false } = c);
        let_assert!(Type::Union([ty, null]) = ty);
        let_assert!(Type::Type { ty: ValueType::Integer } = ty);
        let_assert!(Type::Type { ty: ValueType::Null } = null);
    }

    #[derive(Schema)]
    struct DeriveStructUnit;

    #[test]
    fn test_derive_struct_unit() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveStructUnit::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveStructUnit>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveStructUnit>());

        // For a null value
        let_assert!(Type::Type { ty: ValueType::Null } = &*ty);
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    struct DeriveStructUnnamed(u32, String, Option<u32>);

    #[test]
    fn test_derive_struct_unnamed() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveStructUnnamed::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveStructUnnamed>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveStructUnnamed>());

        // For a tuple with three fields
        let_assert!(Type::Tuple { fields: &[ty_1, ty_2, ty_3] } = &*ty);

        assert_exact(ty_1, u32::TYPE);
        assert_exact(ty_2, String::TYPE);
        assert_exact(ty_3, <Option<u32>>::TYPE);
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    struct DeriveStructNewType(u32);

    #[test]
    fn test_derive_struct_new_type() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveStructNewType::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveStructNewType>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveStructNewType>());

        // Wrapping a u32
        assert_exact::<Type<'_>>(&*ty, u32::TYPE);
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    struct DeriveStructEmptyUnnamed();

    #[test]
    fn test_derive_struct_empty_unnamed() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveStructEmptyUnnamed::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveStructEmptyUnnamed>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveStructEmptyUnnamed>());

        // For an empty tuple
        let_assert!(Type::Tuple { fields: &[] } = &*ty);
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    struct DeriveStructGeneric<T: Copy>
    where
        T: Clone,
    {
        a: T,
        b: String,
        c: Option<T>,
    }

    #[test]
    fn test_derive_struct_generic() {
        fn check<T: 'static + Schema + Copy>() {
            // Should introduce a type alias
            let_assert!(&Type::Alias { name, id, ty } = DeriveStructGeneric::<T>::TYPE);
            assert_eq!((name)(), std::any::type_name::<DeriveStructGeneric<T>>());
            assert_eq!((id)(), std::any::TypeId::of::<DeriveStructGeneric<T>>());

            // For a struct with three fields
            let_assert!(Type::Struct { fields: [a, b, c], extras: None } = &*ty);

            let_assert!(Field { key: "a", value: ty, optional: false } = *a);
            assert_exact(ty, T::TYPE);

            let_assert!(Field { key: "b", value: ty, optional: false } = b);
            let_assert!(Type::Type { ty: ValueType::String } = ty);

            let_assert!(Field { key: "c", value: ty, optional: false } = *c);
            assert_exact(ty, <Option<T>>::TYPE);
        }

        check::<u32>();
        check::<(u32, u32, u32)>();
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    enum DeriveEnum {
        One,
        Two,
        Three,
    }

    #[test]
    fn test_derive_enum() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveEnum::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveEnum>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveEnum>());

        // For an enum with three variants
        let_assert!(
            Type::Enum {
                variants: &[("One", variant_1), ("Two", variant_2), ("Three", variant_3)]
            } = &*ty
        );

        let_assert!(Type::Void = variant_1);
        let_assert!(Type::Void = variant_2);
        let_assert!(Type::Void = variant_3);
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    enum DeriveDataEnum {
        Neither,
        Num(u32),
        NumTriple(u32, u32, u32),
        Str(String),
        Both { num: u32, str: String },
    }

    #[test]
    fn test_derive_data_enum() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveDataEnum::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveDataEnum>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveDataEnum>());

        // For an enum with five variants
        let_assert!(
            Type::Enum {
                variants: &[
                    ("Neither", neither),
                    ("Num", num),
                    ("NumTriple", num_triple),
                    ("Str", string),
                    ("Both", both)
                ]
            } = &*ty
        );

        // Neither expects no data
        let_assert!(Type::Void = neither);

        // Num is a newtype around u32
        assert_exact(num, u32::TYPE);

        // NumTriple is a tuple of 3 u32s
        let_assert!(Type::Tuple { fields: &[ty_1, ty_2, ty_3] } = num_triple);

        assert_exact(ty_1, u32::TYPE);
        assert_exact(ty_2, u32::TYPE);
        assert_exact(ty_3, u32::TYPE);

        // Str is a newtype around String
        assert_exact(string, String::TYPE);

        // Both is a struct containing a u32 and String
        let_assert!(Type::Struct { fields: [num, str], extras: None } = both);

        let_assert!(Field { key: "num", value: ty, optional: false } = *num);
        assert_exact(ty, u32::TYPE);

        let_assert!(Field { key: "str", value: ty, optional: false } = *str);
        assert_exact(ty, String::TYPE);
    }

    #[derive(Schema)]
    #[allow(dead_code)]
    enum DeriveStrangeEnum {
        A,
        B(),
        C {},
    }

    #[test]
    fn test_derive_strange_enum() {
        // Should introduce a type alias
        let_assert!(&Type::Alias { name, id, ty } = DeriveStrangeEnum::TYPE);
        assert_eq!((name)(), std::any::type_name::<DeriveStrangeEnum>());
        assert_eq!((id)(), std::any::TypeId::of::<DeriveStrangeEnum>());

        // For an enum with three variants
        let_assert!(
            Type::Enum { variants: &[("A", variant_a), ("B", variant_b), ("C", variant_c)] } = &*ty
        );

        let_assert!(Type::Void = variant_a);

        // Variant B is an empty tuple
        let_assert!(Type::Tuple { fields: [] } = variant_b);

        // Variant C is an empty struct
        let_assert!(Type::Struct { fields: [], extras: None } = variant_c);
    }
}
