// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Comparisons that are specific to type simplification, since they rely on invariants from the
//! simplification passes. The comparisons also ensure compatibility. Changes to the ordering of
//! Type, ValueType, and InlineValue members do not affect these comparisons.

use std::cmp::Ordering;

use crate::schema::{Field, InlineValue, StructExtras, Type, ValueType};

/// Compares two sorted slices to produce a stable ordering. This order is not equivalent to Rust's
/// [`Ord`] impl for slices, and is instead made to reduce the number of nested comparisons.
fn cmp_sorted_by<T>(mut lhs: &[T], mut rhs: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> Ordering {
    let ord = lhs.len().cmp(&rhs.len());
    if ord.is_ne() {
        return ord;
    }

    loop {
        match (lhs, rhs) {
            ([], []) => return Ordering::Equal,
            ([lhs], [rhs]) => return cmp(lhs, rhs),
            ([lhs_val, lhs_rest @ ..], [rhs_val, rhs_rest @ ..]) => {
                let ord = cmp(lhs_val, rhs_val);
                if ord.is_ne() {
                    return ord;
                }

                lhs = lhs_rest;
                rhs = rhs_rest;
            }
            _ => unreachable!("length of lhs and rhs slices are somehow not equal, despite checks"),
        }
    }
}

/// Checks that all pairs of slice values match the predicate.
///
/// For example, the slice `[1, 2, 3]` will call the predicate for `1, 2` then `2, 3`.
///
/// Always returns true for empty or single-element slices.
pub(super) fn test_all_pairs<T>(slice: &[T], predicate: impl Fn(&T, &T) -> bool) -> bool {
    slice.windows(2).all(|x| predicate(&x[0], &x[1]))
}

/// Checks that the given slice is sorted.
pub(super) fn is_sorted_by<T>(slice: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> bool {
    test_all_pairs(slice, |a, b| cmp(a, b).is_le())
}

/// Checks that the given slice is sorted and contains no duplicate values.
pub(super) fn is_sorted_and_deduped_by<T>(slice: &[T], cmp: impl Fn(&T, &T) -> Ordering) -> bool {
    test_all_pairs(slice, |a, b| cmp(a, b).is_lt())
}

/// Separates ordinals for simple types and complex types, so simple types always come first in
/// sorted lists. This reduces the number of nested comparisons needed to compare deeply nested
/// types.
const COMPLEX_COMPARE_BIAS: usize = 0x8000;

/// Compares two values. Assumes slices in arrays and objects have been presorted.
pub(super) fn cmp_sorted_inline_values(lhs: &InlineValue<'_>, rhs: &InlineValue<'_>) -> Ordering {
    fn ordinal(value: &InlineValue<'_>) -> usize {
        // For backwards compatibility, do not change ordinals for existing values.
        match value {
            InlineValue::Null => 0,
            InlineValue::Bool(..) => 1,
            InlineValue::UInt(..) => 2,
            InlineValue::Int(..) => 3,
            InlineValue::Float(..) => 4,
            InlineValue::String(..) => 5,
            InlineValue::Array(..) => 6 | COMPLEX_COMPARE_BIAS,
            InlineValue::Object(..) => 7 | COMPLEX_COMPARE_BIAS,
        }
    }

    if std::ptr::eq(lhs, rhs) {
        return Ordering::Equal;
    }

    let ord = ordinal(lhs).cmp(&ordinal(rhs));
    if ord.is_ne() {
        return ord;
    }

    match (lhs, rhs) {
        (InlineValue::Null, InlineValue::Null) => Ordering::Equal,
        (InlineValue::Bool(lhs), InlineValue::Bool(rhs)) => lhs.cmp(rhs),
        (InlineValue::UInt(lhs), InlineValue::UInt(rhs)) => lhs.cmp(rhs),
        (InlineValue::Int(lhs), InlineValue::Int(rhs)) => lhs.cmp(rhs),
        (InlineValue::Float(lhs), InlineValue::Float(rhs)) => lhs.total_cmp(rhs),
        (InlineValue::String(lhs), InlineValue::String(rhs)) => lhs.cmp(rhs),
        (InlineValue::Array(lhs), InlineValue::Array(rhs)) => {
            cmp_sorted_by(lhs, rhs, |l, r| cmp_sorted_inline_values(*l, *r))
        }
        (InlineValue::Object(lhs), InlineValue::Object(rhs)) => {
            cmp_sorted_by(lhs, rhs, |(lhs, lhs_value), (rhs, rhs_value)| {
                lhs.cmp(rhs).then_with(|| cmp_sorted_inline_values(lhs_value, rhs_value))
            })
        }
        _ => unreachable!(),
    }
}

/// Defines the ordering of two struct fields, excluding their type.
pub(super) fn struct_field_ord<'a>(field: &'a Field<'a>) -> impl Ord + 'a {
    (field.key, field.optional)
}

fn type_ordinal(ty: &Type<'_>) -> usize {
    // For backwards compatibility, do not change ordinals for existing values.
    match ty {
        Type::Void => 0,
        Type::Union(..) => 1 | COMPLEX_COMPARE_BIAS,
        Type::Alias { .. } => 2,
        Type::Struct { .. } => 3 | COMPLEX_COMPARE_BIAS,
        Type::Enum { .. } => 4 | COMPLEX_COMPARE_BIAS,
        Type::Tuple { .. } => 5 | COMPLEX_COMPARE_BIAS,
        Type::Array { .. } => 6 | COMPLEX_COMPARE_BIAS,
        Type::Map { .. } => 7 | COMPLEX_COMPARE_BIAS,
        Type::Type { .. } => 8,
        Type::Any => 9,
        Type::Constant { .. } => 10 | COMPLEX_COMPARE_BIAS,
    }
}

fn value_type_ordinal(ty: &ValueType) -> usize {
    // For backwards compatibility, do not change ordinals for existing values.
    match ty {
        ValueType::Null => 0,
        ValueType::Bool => 1,
        ValueType::Integer => 2,
        ValueType::Double => 3,
        ValueType::String => 4,
        ValueType::Array => 5,
        ValueType::Object => 6,
    }
}

fn struct_extras_ordinal(v: &Option<StructExtras<'_>>) -> usize {
    match v {
        None => 0,
        Some(StructExtras::Deny) => 1,
        Some(StructExtras::Flatten(..)) => 2 | COMPLEX_COMPARE_BIAS,
    }
}

/// Compares two processed types.
///
/// Type comparison is expected to happen after the types are simplified. This reduces the need to
/// deeply compare nested types.
pub(super) fn cmp_processed_types<'a>(lhs: &'a Type<'a>, rhs: &'a Type<'a>) -> Ordering {
    if std::ptr::eq(lhs, rhs) {
        return Ordering::Equal;
    }

    let ord = type_ordinal(lhs).cmp(&type_ordinal(rhs));
    if ord.is_ne() {
        return ord;
    }

    match (lhs, rhs) {
        (Type::Void, Type::Void) => Ordering::Equal,
        (Type::Any, Type::Any) => Ordering::Equal,
        (Type::Type { ty: lhs }, Type::Type { ty: rhs }) => {
            value_type_ordinal(lhs).cmp(&value_type_ordinal(rhs))
        }
        (Type::Constant { value: lhs }, Type::Constant { value: rhs }) => {
            cmp_sorted_inline_values(lhs, rhs)
        }
        (Type::Union(lhs), Type::Union(rhs))
        | (Type::Tuple { fields: lhs }, Type::Tuple { fields: rhs }) => {
            cmp_sorted_by(lhs, rhs, |l, r| cmp_processed_types(*l, *r))
        }
        (
            Type::Array { size: lhs_size, ty: lhs_ty },
            Type::Array { size: rhs_size, ty: rhs_ty },
        ) => lhs_size.cmp(rhs_size).then_with(|| cmp_processed_types(lhs_ty, rhs_ty)),
        (
            Type::Map { key: lhs_key, value: lhs_value },
            Type::Map { key: rhs_key, value: rhs_value },
        ) => cmp_processed_types(lhs_key, rhs_key)
            .then_with(|| cmp_processed_types(lhs_value, rhs_value)),
        // The types have already been processed, so any aliases left around are known to be
        // recursive. During a stable sort, returning `Ordering::Equal` will maintain the original
        // order of the aliases. If this is (somehow) not enough, a stable hash calculated during
        // the `ProcessAlias` phase should be used for stable ordering.
        (Type::Alias { .. }, Type::Alias { .. }) => Ordering::Equal,
        (
            Type::Struct { fields: lhs_fields, extras: lhs_extras },
            Type::Struct { fields: rhs_fields, extras: rhs_extras },
        ) => {
            // Compare struct field name and optionality, then types, then extras
            cmp_sorted_by(lhs_fields, rhs_fields, |lhs, rhs| {
                struct_field_ord(lhs)
                    .cmp(&struct_field_ord(rhs))
                    .then_with(|| cmp_processed_types(lhs.value, rhs.value))
            })
            .then_with(|| struct_extras_ordinal(lhs_extras).cmp(&struct_extras_ordinal(rhs_extras)))
            .then_with(|| {
                // Both extras are the same variant. Compare their contents.
                match (lhs_extras, rhs_extras) {
                    (None, None) | (Some(StructExtras::Deny), Some(StructExtras::Deny)) => {
                        Ordering::Equal
                    }
                    (Some(StructExtras::Flatten(lhs)), Some(StructExtras::Flatten(rhs))) => {
                        cmp_sorted_by(lhs, rhs, |lhs, rhs| cmp_processed_types(lhs, rhs))
                    }
                    _ => unreachable!(),
                }
            })
        }
        (Type::Enum { variants: lhs }, Type::Enum { variants: rhs }) => {
            cmp_sorted_by(lhs, rhs, |(lhs_name, lhs), (rhs_name, rhs)| {
                lhs_name.cmp(rhs_name).then_with(|| cmp_processed_types(lhs, rhs))
            })
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::schema::RecursiveType;

    #[test]
    fn test_cmp_sorted_by() {
        // Equal
        assert_eq!(cmp_sorted_by::<u32>(&[], &[], Ord::cmp), Ordering::Equal);
        assert_eq!(cmp_sorted_by::<u32>(&[1], &[1], Ord::cmp), Ordering::Equal);
        assert_eq!(cmp_sorted_by(&[1, 2, 3, 4], &[1, 2, 3, 4], Ord::cmp), Ordering::Equal);

        // Different lengths
        assert_eq!(cmp_sorted_by(&[1, 2, 3, 4], &[1, 2, 3, 4, 5], Ord::cmp), Ordering::Less);
        assert_eq!(cmp_sorted_by(&[1, 2, 3, 4, 5], &[1, 2, 3, 4], Ord::cmp), Ordering::Greater);

        // Same lengths
        assert_eq!(cmp_sorted_by::<u32>(&[2], &[1], Ord::cmp), Ordering::Greater);

        // 2 > 1
        assert_eq!(cmp_sorted_by(&[2, 3, 4, 5], &[1, 2, 3, 5], Ord::cmp), Ordering::Greater);

        // 4 > 3
        assert_eq!(cmp_sorted_by(&[1, 2, 3, 4], &[1, 2, 3, 3], Ord::cmp), Ordering::Greater);

        // 1 == 1, 4 == 4; 4 > 3
        assert_eq!(cmp_sorted_by(&[1, 2, 4, 4], &[1, 2, 3, 4], Ord::cmp), Ordering::Greater);

        // 1 == 1, 3 == 3; 3 > 2
        assert_eq!(cmp_sorted_by(&[1, 3, 3], &[1, 2, 3], Ord::cmp), Ordering::Greater);
    }

    #[test]
    fn test_is_sorted_by() {
        assert!(is_sorted_by(&[1, 2, 3, 4], Ord::cmp));
        assert!(!is_sorted_by(&[1, 2, 4, 3], Ord::cmp));

        // Zero and one length slices are always considered sorted.
        assert!(is_sorted_by(&[], <i32 as Ord>::cmp));
        assert!(is_sorted_by(&[1], Ord::cmp));
    }

    #[test]
    fn test_is_sorted_and_deduped_by() {
        assert!(is_sorted_and_deduped_by(&[1, 2, 3, 4], Ord::cmp));
        assert!(!is_sorted_and_deduped_by(&[1, 2, 4, 3], Ord::cmp));
        assert!(!is_sorted_and_deduped_by(&[1, 2, 3, 3], Ord::cmp));

        // Zero and one length slices are always considered sorted.
        assert!(is_sorted_and_deduped_by(&[], <i32 as Ord>::cmp));
        assert!(is_sorted_and_deduped_by(&[1], Ord::cmp));
    }

    #[test]
    fn test_inline_value_cmp() {
        // Sorted values to drive comparison tests.
        // Pairs of values are compared to ensure comparisons return the correct result.
        // Values are compared by ordinal first, then by contents when the ordinals match.
        const SORTED: &[InlineValue<'_>] = &[
            InlineValue::Null,
            InlineValue::Bool(false),
            InlineValue::Bool(true),
            InlineValue::UInt(0),
            InlineValue::UInt(42),
            InlineValue::Int(-1),
            InlineValue::Int(128),
            InlineValue::Float(-1.0),
            InlineValue::Float(1.0),
            InlineValue::String(""),
            InlineValue::String("hello"),
            InlineValue::String("world"),
            InlineValue::Array(&[]),
            InlineValue::Array(&[&InlineValue::UInt(0)]),
            InlineValue::Array(&[&InlineValue::UInt(0), &InlineValue::UInt(1)]),
            InlineValue::Array(&[
                &InlineValue::UInt(0),
                &InlineValue::UInt(1),
                &InlineValue::UInt(2),
            ]),
            InlineValue::Array(&[
                &InlineValue::UInt(0),
                &InlineValue::UInt(1),
                &InlineValue::UInt(3),
            ]),
            InlineValue::Object(&[
                ("a", &InlineValue::UInt(0)),
                ("b", &InlineValue::UInt(1)),
                ("c", &InlineValue::UInt(2)),
            ]),
            InlineValue::Object(&[
                ("a", &InlineValue::UInt(2)),
                ("b", &InlineValue::UInt(3)),
                ("c", &InlineValue::UInt(4)),
            ]),
            InlineValue::Object(&[
                ("b", &InlineValue::UInt(3)),
                ("c", &InlineValue::UInt(4)),
                ("d", &InlineValue::UInt(5)),
            ]),
        ];

        test_all_pairs(SORTED, |lhs, rhs| {
            eprintln!("Compare pair:\n  {lhs:?} &\n  {rhs:?}");

            assert_eq!(cmp_sorted_inline_values(lhs, lhs), Ordering::Equal);
            assert_eq!(cmp_sorted_inline_values(rhs, rhs), Ordering::Equal);

            assert_eq!(cmp_sorted_inline_values(lhs, rhs), Ordering::Less);
            assert_eq!(cmp_sorted_inline_values(rhs, lhs), Ordering::Greater);

            true
        });
    }

    #[test]
    fn test_type_cmp() {
        // Sorted types to drive comparison tests.
        // Pairs of types are compared to ensure comparisons return the correct result.
        // Types are compared by ordinal first, then by contents when the ordinals match.
        const SORTED: &[Type<'_>] = &[
            Type::Void,
            Type::Alias {
                name: std::any::type_name::<()>,
                id: std::any::TypeId::of::<()>,
                ty: RecursiveType::Plain(&Type::Void),
            },
            Type::Type { ty: ValueType::Null },
            Type::Type { ty: ValueType::Bool },
            Type::Type { ty: ValueType::Integer },
            Type::Type { ty: ValueType::Double },
            Type::Type { ty: ValueType::String },
            Type::Type { ty: ValueType::Array },
            Type::Type { ty: ValueType::Object },
            Type::Any,
            Type::Union(&[]),
            Type::Union(&[&Type::Void]),
            Type::Union(&[&Type::Void, &Type::Void]),
            Type::Union(&[&Type::Void, &Type::Any]),
            Type::Struct { fields: &[], extras: None },
            // Struct with two required fields
            Type::Struct {
                fields: &[
                    Field { key: "a", value: &Type::Void, optional: false },
                    Field { key: "b", value: &Type::Void, optional: false },
                ],
                extras: None,
            },
            // Struct with two required fields, where the last field's type is Any instead of Void
            Type::Struct {
                fields: &[
                    Field { key: "a", value: &Type::Void, optional: false },
                    Field { key: "b", value: &Type::Any, optional: false },
                ],
                extras: None,
            },
            // Struct with two fields, the last one being optional
            Type::Struct {
                fields: &[
                    Field { key: "a", value: &Type::Void, optional: false },
                    Field { key: "b", value: &Type::Void, optional: true },
                ],
                extras: None,
            },
            // Struct with three fields, the first two required and the last optional
            Type::Struct {
                fields: &[
                    Field { key: "a", value: &Type::Void, optional: false },
                    Field { key: "b", value: &Type::Any, optional: false },
                    Field { key: "c", value: &Type::Void, optional: true },
                ],
                extras: None,
            },
            Type::Enum { variants: &[] },
            Type::Enum { variants: &[("A", &Type::Void)] },
            Type::Enum { variants: &[("A", &Type::Void), ("B", &Type::Void)] },
            Type::Enum { variants: &[("A", &Type::Void), ("B", &Type::Any)] },
            Type::Enum { variants: &[("B", &Type::Void), ("C", &Type::Void)] },
            Type::Enum { variants: &[("B", &Type::Void), ("C", &Type::Void), ("D", &Type::Void)] },
            Type::Tuple { fields: &[] },
            Type::Tuple { fields: &[&Type::Void] },
            Type::Tuple { fields: &[&Type::Void, &Type::Void] },
            Type::Tuple { fields: &[&Type::Void, &Type::Any] },
            Type::Tuple { fields: &[&Type::Void, &Type::Void, &Type::Void] },
            Type::Array { size: None, ty: &Type::Void },
            Type::Array { size: None, ty: &Type::Any },
            Type::Array { size: Some(1), ty: &Type::Void },
            Type::Array { size: Some(2), ty: &Type::Void },
            Type::Map { key: &Type::Void, value: &Type::Void },
            Type::Map { key: &Type::Void, value: &Type::Any },
            Type::Map { key: &Type::Any, value: &Type::Any },
            Type::Constant { value: &InlineValue::Null },
            Type::Constant { value: &InlineValue::Bool(true) },
        ];

        test_all_pairs(SORTED, |lhs, rhs| {
            eprintln!("Compare pair:\n  {lhs:?} &\n  {rhs:?}");

            assert_eq!(cmp_processed_types(lhs, lhs), Ordering::Equal);
            assert_eq!(cmp_processed_types(rhs, rhs), Ordering::Equal);

            assert_eq!(cmp_processed_types(lhs, rhs), Ordering::Less);
            assert_eq!(cmp_processed_types(rhs, lhs), Ordering::Greater);

            true
        });
    }
}
