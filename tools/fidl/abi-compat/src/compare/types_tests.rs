// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use maplit::btreeset;

use super::test::*;
use super::Primitive::*;
use super::*;
use CompatibilityDegree::*;
use Flexibility::*;
use Optionality::*;
use StringPattern::*;
use Type::*;

pub fn type_compatible(sender: &Type, receiver: &Type) -> CompatibilityDegree {
    compare_types(sender, receiver).compatibility_degree()
}

/// Generate incompatible types
fn incompatible() -> (Type, Type) {
    (
        Enum(Path::empty(), Strict, Uint32, btreeset! { 1, 2, 3, 4 }),
        Enum(Path::empty(), Strict, Uint32, btreeset! { 1, 2, 3 }),
    )
}

/// Generate weakly compatible types
fn weakly_compatible() -> (Type, Type) {
    (
        Enum(Path::empty(), Flexible, Uint32, btreeset! {1,2,3,4}),
        Enum(Path::empty(), Flexible, Uint32, btreeset! {1,2,3}),
    )
}

/// Generate strongly compatible types
fn strongly_compatible() -> (Type, Type) {
    (
        Enum(Path::empty(), Flexible, Uint32, btreeset! {1,2,3}),
        Enum(Path::empty(), Flexible, Uint32, btreeset! {1,2,3}),
    )
}

#[test]
fn helpers() {
    let (send, recv) = incompatible();
    assert_eq!(Incompatible, type_compatible(&send, &recv));

    let (send, recv) = weakly_compatible();
    assert_eq!(WeaklyCompatible, type_compatible(&send, &recv));

    let (send, recv) = strongly_compatible();
    assert_eq!(StronglyCompatible, type_compatible(&send, &recv));
}

#[test]
fn primitive() {
    assert!(compare_fidl_type(
        "PrimitiveStruct",
        "
    type PrimitiveStruct = struct {
        @available(replaced=2)
        value bool;
        @available(added=2)
        value bool;
    };",
    )
    .is_compatible());

    assert!(compare_fidl_type(
        "PrimitiveStruct",
        "
    type PrimitiveStruct = struct {
        @available(replaced=2)
        value bool;
        @available(added=2)
        value int8;
    };",
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2")
            .message("Incompatible primitive types, sender(@1):bool, receiver(@2):int8"),
        ProblemPattern::type_error("2", "1")
            .message("Incompatible primitive types, sender(@2):int8, receiver(@1):bool")
    ]));
}

#[test]
fn string() {
    assert!(compare_fidl_type(
        "Strings",
        "
        type Strings = struct {
            // Lengths
            same_length string:10;
            @available(replaced=2)
            change_length string:10;
            @available(added=2)
            change_length string:20;

            // Optionality
            @available(replaced=2)
            become_optional string:10;
            @available(added=2)
            become_optional string:<10,optional>;
        };"
    )
    .has_problems(vec![
        ProblemPattern::type_error("2", "1")
            .message("Incompatible string lengths, sender(@2):20, receiver(@1):10")
            .path(Ends(".change_length")),
        ProblemPattern::type_error("2", "1")
            .message("Sender(@2) string is optional but receiver(@1) is required")
            .path(Ends(".become_optional"))
    ]));
}

#[test]
fn enums() {
    // Primitive type
    assert!(compare_fidl_type(
        "Enum",
        "
        @available(replaced=2)
        type Enum = enum : uint32 { M = 1; };
        @available(added=2)
        type Enum = enum : int32 { M = 1; };"
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2")
            .message("Incompatible enum types, sender(@1):uint32, receiver(@2):int32"),
        ProblemPattern::type_error("2", "1")
            .message("Incompatible enum types, sender(@2):int32, receiver(@1):uint32")
    ]));

    // Strictness difference
    assert!(compare_fidl_type(
        "Enum",
        "
        @available(replaced=2)
        type Enum = strict enum {
            A = 1;
        };
        @available(added=2)
        type Enum = flexible enum {
            A = 1;
        };
        "
    )
    .is_compatible());

    // Strict member difference
    assert!(compare_fidl_type(
        "Enum",
        "
        type Enum = strict enum {
            A = 1;
            @available(added=2)
            B = 2;
        };"
    )
    .has_problems(vec![
        ProblemPattern::type_error("2", "1").message("Extra strict enum members in sender(@2): 2")
    ]));

    // Flexible member difference
    assert!(compare_fidl_type(
        "Enum",
        "
                type Enum = flexible enum {
                    A = 1;
                    @available(added=2)
                    B = 2;
                };"
    )
    .has_problems(vec![ProblemPattern::type_warning("2", "1")
        .message("Extra flexible enum members in sender(@2): 2")]));

    // Strictness difference, member difference.
    assert!(compare_fidl_type(
        "Enum",
        "
        @available(replaced=2)
        type Enum = strict enum {
            A = 1;
            B = 2;
        };
        @available(added=2)
        type Enum = flexible enum {
            A = 1;
            C = 3;
        };
        "
    )
    .has_problems(vec![
        ProblemPattern::type_warning("1", "2")
            .message("Extra flexible enum members in sender(@1): 2"),
        ProblemPattern::type_error("2", "1").message("Extra strict enum members in sender(@2): 3")
    ]));

    // Primitive conversion
    assert!(compare_fidl_type(
        "Wrapper",
        "
        @available(replaced=2)
        type Enum = enum : uint32 { M = 1; };
        @available(added=2)
        alias Enum = uint32;
        type Wrapper = struct { e Enum; };
        "
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2"),
        ProblemPattern::type_error("2", "1")
    ]));
}

#[test]
fn bits() {
    // Primitive type
    assert!(compare_fidl_type(
        "Bits",
        "
        @available(replaced=2)
        type Bits = bits : uint32 { M = 1; };
        @available(added=2)
        type Bits = bits : uint64 { M = 1; };"
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2")
            .message("Incompatible bits types, sender(@1):uint32, receiver(@2):uint64"),
        ProblemPattern::type_error("2", "1")
            .message("Incompatible bits types, sender(@2):uint64, receiver(@1):uint32")
    ]));

    // Strictness difference
    assert!(compare_fidl_type(
        "Bits",
        "
        @available(replaced=2)
        type Bits = strict bits {
            A = 1;
        };
        @available(added=2)
        type Bits = flexible bits {
            A = 1;
        };
        "
    )
    .is_compatible());

    // Strict member difference
    assert!(compare_fidl_type(
        "Bits",
        "
        type Bits = strict bits {
            A = 1;
            @available(added=2)
            B = 2;
        };"
    )
    .has_problems(vec![
        ProblemPattern::type_error("2", "1").message("Extra strict bits members in sender(@2): 2")
    ]));

    // Flexible member difference
    assert!(compare_fidl_type(
        "Bits",
        "
                type Bits = flexible bits {
                    A = 1;
                    @available(added=2)
                    B = 2;
                };"
    )
    .has_problems(vec![ProblemPattern::type_warning("2", "1")
        .message("Extra flexible bits members in sender(@2): 2")]));

    // Strictness difference, member difference.
    assert!(compare_fidl_type(
        "Bits",
        "
        @available(replaced=2)
        type Bits = strict bits {
            A = 1;
            B = 2;
        };
        @available(added=2)
        type Bits = flexible bits {
            A = 1;
            C = 4;
        };
        "
    )
    .has_problems(vec![
        ProblemPattern::type_warning("1", "2")
            .message("Extra flexible bits members in sender(@1): 2"),
        ProblemPattern::type_error("2", "1").message("Extra strict bits members in sender(@2): 4")
    ]));

    // Primitive conversion
    assert!(compare_fidl_type(
        "Wrapper",
        "
        @available(replaced=2)
        type Bits = bits : uint32 { M = 1; };
        @available(added=2)
        alias Bits = uint32;
        type Wrapper = struct { e Bits; };
        "
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2"),
        ProblemPattern::type_error("2", "1")
    ]));
}

#[test]
fn handle() {
    // Can't use compare_fidl_type because we don't have access to library zx.

    use HandleType::*;

    let untyped = Handle(Path::empty(), None, Required, HandleRights::default());
    let channel = Handle(Path::empty(), Some(Channel), Required, HandleRights::default());
    let vmo = Handle(Path::empty(), Some(VMO), Required, HandleRights::default());

    // Handle types
    assert_eq!(StronglyCompatible, type_compatible(&untyped, &untyped));
    assert_eq!(StronglyCompatible, type_compatible(&channel, &untyped));
    assert_eq!(StronglyCompatible, type_compatible(&channel, &channel));
    assert_eq!(Incompatible, type_compatible(&untyped, &channel));
    assert_eq!(Incompatible, type_compatible(&vmo, &channel));

    // Optionality
    assert_eq!(
        StronglyCompatible,
        type_compatible(
            &Handle(Path::empty(), Some(Channel), Required, HandleRights::default()),
            &Handle(Path::empty(), Some(Channel), Required, HandleRights::default())
        )
    );
    assert_eq!(
        StronglyCompatible,
        type_compatible(
            &Handle(Path::empty(), Some(Channel), Optional, HandleRights::default()),
            &Handle(Path::empty(), Some(Channel), Optional, HandleRights::default())
        )
    );

    assert_eq!(
        StronglyCompatible,
        type_compatible(
            &Handle(Path::empty(), Some(Channel), Required, HandleRights::default()),
            &Handle(Path::empty(), Some(Channel), Optional, HandleRights::default())
        )
    );

    assert_eq!(
        Incompatible,
        type_compatible(
            &Handle(Path::empty(), Some(Channel), Optional, HandleRights::default()),
            &Handle(Path::empty(), Some(Channel), Required, HandleRights::default())
        )
    );

    // Handle rights
    assert!(compare_fidl_type(
        "HandleRights",
        "
        using zx;
        type HandleRights = resource struct {
            @available(replaced=2)
            no_rights_to_some_rights zx.Handle:CHANNEL;
            @available(added=2)
            no_rights_to_some_rights zx.Handle:<CHANNEL, zx.Rights.WRITE | zx.Rights.READ>;

            @available(replaced=2)
            reduce_rights zx.Handle:<CHANNEL, zx.Rights.WRITE | zx.Rights.READ>;
            @available(added=2)
            reduce_rights zx.Handle:<CHANNEL, zx.Rights.WRITE>;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_warning("1", "2").path(Ends("HandleRights.no_rights_to_some_rights")),
        ProblemPattern::type_error("2", "1").path(Ends("HandleRights.reduce_rights")),
    ]));
}

#[test]
fn array() {
    assert!(compare_fidl_type(
        "Arrays",
        "
        type Arrays = struct {
            @available(replaced=2)
            size_changed array<uint32, 10>;
            @available(added=2)
            size_changed array<uint32, 20>;

            @available(replaced=2)
            member_incompatible array<uint32, 10>;
            @available(added=2)
            member_incompatible array<float32, 10>;

            @available(replaced=2)
            member_soft_change array<flexible enum { M = 1; }, 10>;
            @available(added=2)
            member_soft_change array<flexible enum { M = 1; N = 2; }, 10>;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2").path(Ends("Arrays.size_changed")),
        ProblemPattern::type_error("2", "1").path(Ends("Arrays.size_changed")),
        ProblemPattern::type_error("1", "2").path(Ends("Arrays.member_incompatible[]")),
        ProblemPattern::type_error("2", "1").path(Ends("Arrays.member_incompatible[]")),
        ProblemPattern::type_warning("2", "1").path(Ends("Arrays.member_soft_change[]"))
    ]));
}

#[test]
fn vector() {
    assert!(compare_fidl_type(
        "Vectors",
        "
        type Vectors = struct {
            @available(replaced=2)
            size_changed vector<uint32>:10;
            @available(added=2)
            size_changed vector<uint32>:20;

            @available(replaced=2)
            member_incompatible vector<uint32>:10;
            @available(added=2)
            member_incompatible vector<float32>:10;

            @available(replaced=2)
            member_soft_change vector<flexible enum { M = 1; }>:10;
            @available(added=2)
            member_soft_change vector<flexible enum { M = 1; N = 2; }>:10;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_error("2", "1").path(Ends("Vectors.size_changed")),
        ProblemPattern::type_error("1", "2").path(Ends("Vectors.member_incompatible[]")),
        ProblemPattern::type_error("2", "1").path(Ends("Vectors.member_incompatible[]")),
        ProblemPattern::type_warning("2", "1").path(Ends("Vectors.member_soft_change[]"))
    ]));
}

#[test]
fn structs() {
    // Number of members
    assert!(compare_fidl_type(
        "NumMembers",
        "
        type NumMembers = struct {
            one int32;
            @available(removed=2)
            two int16;
            @available(removed=2)
            three int16;
            @available(added=2)
            four int32;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2")
            .message(Begins("Struct has different number of members")),
        ProblemPattern::type_error("2", "1")
            .message(Begins("Struct has different number of members")),
        ProblemPattern::type_error("1", "2").message(Begins("Incompatible primitive types")),
        ProblemPattern::type_error("2", "1").message(Begins("Incompatible primitive types")),
    ]));

    // Member Compatibility
    assert!(compare_fidl_type(
        "MemberCompat",
        "
        type MemberCompat = struct {
            @available(replaced=2)
            weak flexible enum { M = 1; };
            @available(added=2)
            weak flexible enum { M = 1; N = 2; };

            @available(replaced=2)
            strong int32;
            @available(added=2)
            strong float32;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_error("1", "2")
            .path(Ends(".strong"))
            .message(Begins("Incompatible primitive types")),
        ProblemPattern::type_error("2", "1")
            .path(Ends(".strong"))
            .message(Begins("Incompatible primitive types")),
        ProblemPattern::type_warning("2", "1")
            .path(Ends(".weak"))
            .message(Begins("Extra flexible enum members"))
    ]));
}

#[test]
fn table() {
    assert!(compare_fidl_type(
        "Table",
        "
        type Table = table {
            // unchanged
            1: one uint32;

            // defined <-> absent
            @available(removed=2)
            2: two uint32;

            // incompatible types
            @available(replaced=2)
            3: three string;
            @available(added=2)
            3: three int32;

            // weakly compatible types
            @available(replaced=2)
            4: four flexible enum { M = 1; };
            @available(added=2)
            4: four flexible enum { M = 1; N = 2; };

            // missing member
            @available(removed=2)
            5: five float64;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_warning("1", "2")
            .path(Ends("/Table"))
            .message("Table in sender(@1) has members that receiver(@2) does not have."),
        ProblemPattern::type_error("1", "2")
            .path(Ends("Table.three"))
            .message(Begins("Incompatible types")),
        ProblemPattern::type_error("2", "1")
            .path(Ends("Table.three"))
            .message(Begins("Incompatible types")),
        ProblemPattern::type_warning("2", "1").path(Ends("Table.four")),
    ]));
}

#[test]
fn union() {
    // TODO: strict unions

    assert!(compare_fidl_type(
        "Union",
        "
        type Union = union {
            // unchanged
            1: one uint32;

            // defined <-> absent
            @available(removed=2)
            2: two uint32;

            // incompatible types
            @available(replaced=2)
            3: three string;
            @available(added=2)
            3: three int32;

            // weakly compatible types
            @available(replaced=2)
            4: four flexible enum { M = 1; };
            @available(added=2)
            4: four flexible enum { M = 1; N = 2; };

            // missing member
            @available(removed=2)
            5: five float64;
        };
    "
    )
    .has_problems(vec![
        ProblemPattern::type_warning("1", "2")
            .path(Ends("/Union"))
            .message("Union in sender(@1) has members that union in receiver(@2) does not have."),
        ProblemPattern::type_error("1", "2")
            .path(Ends("Union.three"))
            .message(Begins("Incompatible types")),
        ProblemPattern::type_error("2", "1")
            .path(Ends("Union.three"))
            .message(Begins("Incompatible types")),
        ProblemPattern::type_warning("2", "1").path(Ends("Union.four")),
    ]));
}

#[test]
fn similar() {
    assert!(compare_fidl_type(
        "Similar",
        "
    @available(replaced=2)
    type Similar = flexible enum : uint32 {};
    @available(added=2)
    type Similar = flexible bits : uint32 {};
    "
    )
    .has_problems(vec![
        ProblemPattern::type_error("2", "1"),
        ProblemPattern::type_error("1", "2")
    ]));
    assert!(compare_fidl_type(
        "Similar",
        "
    @available(replaced=2)
    type Similar = table {};
    @available(added=2)
    type Similar = flexible union {};
    "
    )
    .has_problems(vec![
        ProblemPattern::type_error("2", "1"),
        ProblemPattern::type_error("1", "2")
    ]));
}
