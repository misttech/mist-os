// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module is concerned with comparing different platform versions. It
//! contains data structure to represent the platform API in ways that are
//! convenient for comparison, and the comparison algorithms.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Display,
};

use anyhow::Result;
use bitflags::bitflags;
use flyweights::FlyStr;
use itertools::Itertools;

use crate::ir::Openness;

mod handle;
pub use handle::*;
mod path;
pub use path::*;
mod primitive;
pub use primitive::*;
mod problems;
pub use problems::*;
mod test;

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum Optionality {
    Optional,
    Required,
}

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum Flexibility {
    Strict,
    Flexible,
}
impl Display for Flexibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Flexibility::Strict => f.write_str("strict"),
            Flexibility::Flexible => f.write_str("flexible"),
        }
    }
}

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum Transport {
    Channel,
}

impl Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Channel => f.write_str("Channel"),
        }
    }
}

#[derive(PartialEq, PartialOrd, Debug, Clone)]
pub enum Type {
    Primitive(Path, Primitive),
    String(Path, u64, Optionality),
    Enum(Path, Flexibility, Primitive, BTreeSet<i128>),
    Bits(Path, Flexibility, Primitive, BTreeSet<i128>),
    Handle(Path, Option<HandleType>, Optionality, HandleRights),
    ClientEnd(Path, String, Transport, Optionality, Box<Protocol>),
    ServerEnd(Path, String, Transport, Optionality),
    Array(Path, u64, Box<Type>),
    Vector(Path, u64, Box<Type>, Optionality),
    Struct(Path, Vec<Type>),
    Table(Path, BTreeMap<u64, Type>),
    Union(Path, Flexibility, BTreeMap<u64, Type>),
    /// Cycle is use to break cycles in recursive type declaration.
    /// `Path` is the path to where the cycle is being broken.
    /// `String` is the identifier used to point back to the start of the cycle.
    /// `usize` is how many path components are in the cycle.
    Cycle(Path, String, usize),
    // Internal types?
    FrameworkError(Path),
}

impl Type {
    #[allow(unused)]
    fn path(&self) -> &Path {
        match self {
            Type::Primitive(path, _) => path,
            Type::String(path, _, _) => path,
            Type::Enum(path, _, _, _) => path,
            Type::Bits(path, _, _, _) => path,
            Type::Handle(path, _, _, _) => path,
            Type::ClientEnd(path, _, _, _, _) => path,
            Type::ServerEnd(path, _, _, _) => path,
            Type::Array(path, _, _) => path,
            Type::Vector(path, _, _, _) => path,
            Type::Struct(path, _) => path,
            Type::Table(path, _) => path,
            Type::Union(path, _, _) => path,
            Type::Cycle(path, _, _) => path,
            Type::FrameworkError(path) => path,
        }
    }
}

fn value_list(values: &BTreeSet<i128>) -> String {
    let v: Vec<String> = values.iter().sorted().map(|o| format!("{o}")).collect();
    v.join(", ")
}

fn member_list(members: &BTreeMap<u64, Type>) -> String {
    let members: Vec<String> =
        members.keys().sorted().map(|o| format!("{o}: {}", members[o])).collect();
    members.join(", ")
}

impl Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Primitive(_, primitive) => primitive.fmt(f),
            Type::String(_, size, opt) => match opt {
                Optionality::Optional => write!(f, "string:<{size},optional>"),
                Optionality::Required => write!(f, "string:{size}"),
            },
            Type::Enum(_, flex, primitive, members) => {
                write!(f, "{flex} enum : {primitive} {{ {} }}", value_list(members))
            }
            Type::Bits(_, flex, primitive, members) => {
                write!(f, "{flex} bits : {primitive} {{ {} }}", value_list(members))
            }
            Type::Handle(_, _, _, _) => f.write_str("zx.Handle:TODO"),
            Type::ClientEnd(_, protocol, _, _, _) => write!(f, "client_end:TODO({protocol})"),
            Type::ServerEnd(_, protocol, _, _) => write!(f, "server_end:TODO({protocol})"),
            Type::Array(_, _, _) => f.write_str("array:TODO"),
            Type::Vector(_, _, _, _) => f.write_str("vector:TODO"),
            Type::Struct(_, _) => f.write_str("struct:TODO"),
            Type::Table(_, _) => f.write_str("table:TODO"),
            Type::Union(_, flex, members) => {
                write!(f, "{flex} union {{ {} }}", member_list(members))
            }
            Type::Cycle(_, id, len) => write!(f, "(cycle id={id} len={len})"),
            Type::FrameworkError(_) => f.write_str("fidl.TransportError"),
        }
    }
}

pub fn compare_types(sender: &Type, receiver: &Type) -> CompatibilityProblems {
    use Flexibility::*;
    use Optionality::*;
    use Type::*;

    let mut problems = CompatibilityProblems::default();

    match (sender, receiver) {
        (Primitive(send_path, s), Primitive(recv_path, r)) => {
            if s != r {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Incompatible primitive types, sender:{s}, receiver:{r}"),
                )
            }
        }
        (String(send_path, send_len, send_opt), String(recv_path, recv_len, recv_opt)) => {
            if *recv_opt == Required && *send_opt == Optional {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Sender string is optional but receiver is required"),
                );
            }
            if send_len > recv_len {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Incompatible string lengths, sender:{send_len}, receiver:{recv_len}"),
                );
            }
        }
        (
            Enum(send_path, _, send_type, send_values),
            Enum(recv_path, recv_flexibility, recv_type, recv_values),
        ) => {
            if send_type != recv_type {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Incompatible enum types, sender:{send_type}, receiver:{recv_type}"),
                )
            } else {
                let missing_values: Vec<_> =
                    send_values.difference(recv_values).sorted().map(|v| format!("{v}")).collect();
                if !missing_values.is_empty() {
                    let missing_values_str = missing_values.join(", ");
                    if recv_flexibility == &Flexible {
                        problems.type_warning(
                            send_path,
                            recv_path,
                            format!("Extra flexible enum members in sender: {missing_values_str}"),
                        );
                    } else {
                        problems.type_error(
                            send_path,
                            recv_path,
                            format!("Extra strict enum members in sender: {missing_values_str}"),
                        );
                    }
                }
            }
        }
        (
            Bits(send_path, _, send_type, send_values),
            Bits(recv_path, recv_flexibility, recv_type, recv_values),
        ) => {
            if send_type != recv_type {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Incompatible bits types, sender:{send_type}, receiver:{recv_type}"),
                )
            } else {
                let missing_values: Vec<_> =
                    send_values.difference(recv_values).sorted().map(|v| format!("{v}")).collect();
                if !missing_values.is_empty() {
                    let missing_values_str = missing_values.join(", ");
                    if recv_flexibility == &Flexible {
                        problems.type_warning(
                            send_path,
                            recv_path,
                            format!("Extra flexible bits members in sender: {missing_values_str}"),
                        );
                    } else {
                        problems.type_error(
                            send_path,
                            recv_path,
                            format!("Extra strict bits members in sender: {missing_values_str}"),
                        );
                    }
                }
            }
        }
        (
            Handle(send_path, send_type, send_opt, send_rights),
            Handle(recv_path, recv_type, recv_opt, recv_rights),
        ) => {
            if *send_opt == Optional && *recv_opt == Required {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Sender handle is optional but receiver is required"),
                );
            }
            if recv_type.is_some() && recv_type != send_type {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!(
                        "Incompatible handle types, sender:{}, receiver:{}",
                        recv_type.unwrap(),
                        match send_type {
                            Some(handle_type) => format!("{handle_type}"),
                            None => format!("zx.Handle"),
                        }
                    ),
                );
            }
            if (HandleRights::SAME_RIGHTS.intersects(*recv_rights)) {
                // The receiver doesn't care what rights the sender has
                assert_eq!(*recv_rights, HandleRights::SAME_RIGHTS);
            } else if (HandleRights::SAME_RIGHTS.intersects(*send_rights)) {
                assert_eq!(*send_rights, HandleRights::SAME_RIGHTS);
                // The sender can send any kinds of rights.
                problems.type_warning(
                    send_path,
                    recv_path,
                    format!(
                        "Sender doesn't specify handle rights but receiver does: {:?}",
                        recv_rights
                    ),
                );
            } else {
                // Both send_rights and recv_rights are specified.
                if send_rights.contains(*recv_rights) {
                    // Receive rights are the same or a subset of send rights. This is fine.
                } else {
                    problems.type_error(
                        send_path,
                        recv_path,
                        format!(
                            "Incompatible handle rights, sender:{:?}, receiver:{:?}",
                            send_rights, recv_rights
                        ),
                    );
                }
            }
        }
        (Array(send_path, send_size, send_type), Array(recv_path, recv_size, recv_type)) => {
            if send_size != recv_size {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Array length mismatch, sender:{}, receiver:{}", send_size, recv_size),
                );
            }
            problems.append(compare_types(send_type, recv_type));
        }
        (
            Vector(send_path, send_size, send_type, send_opt),
            Vector(recv_path, recv_size, recv_type, recv_opt),
        ) => {
            if *send_opt == Optional && *recv_opt == Required {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("Sender vector is optional but receiver is required"),
                );
            }

            if send_size > recv_size {
                problems.type_error(send_path, recv_path, format!("Sender vector is larger than receiver vector, sender:{send_size}, receiver:{recv_size}"));
            }
            problems.append(compare_types(send_type, recv_type));
        }
        (Struct(send_path, send_members), Struct(recv_path, recv_members)) => {
            if send_members.len() != recv_members.len() {
                problems.type_error(
                    send_path,
                    recv_path,
                    format!(
                        "Struct has different number of members, sender:{}, receiver:{}",
                        send_members.len(),
                        recv_members.len()
                    ),
                )
            }
            for (send, recv) in send_members.iter().zip(recv_members.iter()) {
                problems.append(compare_types(send, recv));
            }
        }
        (Table(send_path, send_members), Table(recv_path, recv_members)) => {
            let send_ordinals = BTreeSet::from_iter(send_members.keys().cloned());
            let recv_ordinals = BTreeSet::from_iter(recv_members.keys().cloned());
            let common_ordinals = BTreeSet::from_iter(send_ordinals.intersection(&recv_ordinals));
            let send_only = BTreeSet::from_iter(send_ordinals.difference(&recv_ordinals));

            for o in common_ordinals {
                problems.append(compare_types(&send_members[o], &recv_members[o]));
            }

            if !send_only.is_empty() {
                problems.type_warning(
                    send_path,
                    recv_path,
                    format!("Table in sender has members that receiver does not have."),
                );
            }
        }

        (Union(send_path, _, send_members), Union(recv_path, recv_flex, recv_members)) => {
            let send_ordinals = BTreeSet::from_iter(send_members.keys().cloned());
            let recv_ordinals = BTreeSet::from_iter(recv_members.keys().cloned());
            let common_ordinals = BTreeSet::from_iter(send_ordinals.intersection(&recv_ordinals));
            let send_only = BTreeSet::from_iter(send_ordinals.difference(&recv_ordinals));

            for o in common_ordinals {
                problems.append(compare_types(&send_members[o], &recv_members[o]));
            }

            if !send_only.is_empty() {
                if recv_flex == &Flexibility::Flexible {
                    problems.type_warning(
                        send_path,
                        recv_path,
                        format!(
                            "Union in sender has members that union in receiver does not have."
                        ),
                    );
                } else {
                    problems.type_error(
                        send_path,
                        recv_path,
                        format!("Union in sender has members that strict union in receiver does not have."),
                );
                }
            }
        }

        (
            ClientEnd(send_path, send_name, send_transport, send_optional, _send_protocol),
            ClientEnd(recv_path, recv_name, recv_transport, recv_optional, _recv_protocol),
        ) => {
            if send_transport != recv_transport {
                problems.type_error(send_path, recv_path, format!("client_end transports don't match sender:{send_transport}, receiver:{recv_transport}"))
            }
            if send_name != recv_name {
                // TODO: do a structural rather than name comparison
                problems.type_error(
                    send_path,
                    recv_path,
                    format!(
                        "client_end protocol names don't match sender:{send_name}, receiver:{recv_name}"
                    ),
                )
            }
            if send_optional == &Optionality::Optional && recv_optional == &Optionality::Required {
                // TODO: reword
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("client_end optionality incompatible"),
                )
            }
        }
        (
            ServerEnd(send_path, send_protocol, send_transport, send_optional),
            ServerEnd(recv_path, recv_protocol, recv_transport, recv_optional),
        ) => {
            if send_transport != recv_transport {
                problems.type_error(send_path, recv_path, format!("server_end transports don't match sender:{send_transport}, receiver:{recv_transport}"))
            }
            if send_protocol != recv_protocol {
                // TODO: do a structural rather than name comparison
                problems.type_error(send_path, recv_path, format!("server_end protocols don't match sender:{send_protocol}, receiver:{recv_protocol}"))
            }
            if send_optional == &Optionality::Optional && recv_optional == &Optionality::Required {
                // TODO: reword
                problems.type_error(
                    send_path,
                    recv_path,
                    format!("server_end optionality incompatible"),
                )
            }
        }

        (FrameworkError(_), FrameworkError(_)) => (),

        (Cycle(send_path, _, send_cycle), Cycle(recv_path, _, recv_cycle)) => {
            if send_cycle != recv_cycle {
                problems.type_error(send_path, recv_path, format!("Cycle length differs between sender:{send_cycle} and receiver:{recv_cycle}"))
            }
        }

        (send, recv) => {
            problems.type_error(
                send.path(),
                recv.path(),
                format!("Incompatible types, sender:{send}, receiver:{recv}"),
            );
        }
    };
    problems
}

#[cfg(test)]
mod types_tests {
    use maplit::{btreemap, btreeset};

    use crate::convert::convert_platform;
    use crate::ir::IR;

    use super::problems::StringPattern::*;
    use super::test::*;
    use super::Primitive::*;
    use super::*;
    use CompatibilityDegree::*;
    use Flexibility::*;
    use Optionality::*;
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
                .message("Incompatible primitive types, sender:bool, receiver:int8"),
            ProblemPattern::type_error("2", "1")
                .message("Incompatible primitive types, sender:int8, receiver:bool")
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
                .message("Incompatible string lengths, sender:20, receiver:10")
                .path(Ends(".change_length")),
            ProblemPattern::type_error("2", "1")
                .message("Sender string is optional but receiver is required")
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
                .message("Incompatible enum types, sender:uint32, receiver:int32"),
            ProblemPattern::type_error("2", "1")
                .message("Incompatible enum types, sender:int32, receiver:uint32")
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
            ProblemPattern::type_error("2", "1").message("Extra strict enum members in sender: 2")
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
            .message("Extra flexible enum members in sender: 2")]));

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
                .message("Extra flexible enum members in sender: 2"),
            ProblemPattern::type_error("2", "1").message("Extra strict enum members in sender: 3")
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
                .message("Incompatible bits types, sender:uint32, receiver:uint64"),
            ProblemPattern::type_error("2", "1")
                .message("Incompatible bits types, sender:uint64, receiver:uint32")
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
            ProblemPattern::type_error("2", "1").message("Extra strict bits members in sender: 2")
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
            .message("Extra flexible bits members in sender: 2")]));

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
                .message("Extra flexible bits members in sender: 2"),
            ProblemPattern::type_error("2", "1").message("Extra strict bits members in sender: 4")
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
            ProblemPattern::type_warning("1", "2")
                .path(Ends("HandleRights.no_rights_to_some_rights")),
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

                // defined <-> reserved
                @available(removed=2)
                2: two uint32;
                @available(added=2)
                2: reserved;

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
                .message("Table in sender has members that receiver does not have."),
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

                // defined <-> reserved
                @available(removed=2)
                2: two uint32;
                @available(added=2)
                2: reserved;

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
                .message("Union in sender has members that union in receiver does not have."),
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

    #[test]
    fn strict_enum() {
        let problems = test::compare_fidl_library(
            "1",
            "2",
            "
    @available(added=1)
    library fuchsia.test.strictenum;

    type Foo = strict enum : uint32 {
        A = 1;
        B = 2;
        C = 3;
        @available(removed=2)
        D = 4;
        @available(added=2)
        E = 5;
    };

    @discoverable
    closed protocol Bar {
        strict SetFoo(struct { foo Foo; });
    };

    ",
            "fuchsia.test.strictenum",
        )
        .unwrap();

        assert!(problems.has_problem(
            ProblemPattern::type_error("1", "2").message("Extra strict enum members in sender: 4")
        ));

        assert!(problems.has_problem(
            ProblemPattern::type_error("2", "1").message("Extra strict enum members in sender: 5")
        ));

        assert_eq!(4, problems.len());
    }
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub enum Method {
    TwoWay { path: Path, request: Type, response: Type },
    OneWay { path: Path, request: Type },
    Event { path: Path, payload: Type },
}

impl Method {
    fn kind(&self) -> &'static str {
        match self {
            Method::TwoWay { path: _, request: _, response: _ } => "two-way",
            Method::OneWay { path: _, request: _ } => "one-way",
            Method::Event { path: _, payload: _ } => "event",
        }
    }

    fn path(&self) -> &Path {
        match self {
            Method::TwoWay { path, request: _, response: _ } => path,
            Method::OneWay { path, request: _ } => path,
            Method::Event { path, payload: _ } => path,
        }
    }

    #[allow(unused_variables)]
    pub fn compatible(client: &Self, server: &Self) -> Result<CompatibilityProblems> {
        let mut problems = CompatibilityProblems::default();
        match (client, server) {
            (
                Method::TwoWay { path: c_name, request: c_request, response: c_response },
                Method::TwoWay { path: s_name, request: s_request, response: s_response },
            ) => {
                // Request
                let compat = compare_types(c_request, s_request);
                if compat.is_incompatible() {
                    problems.protocol(client.path(), server.path(), "Incompatible request types");
                }
                problems.append(compat);
                // Response
                let compat = compare_types(s_response, c_response);
                if compat.is_incompatible() {
                    // TODO: should this be sender/receiver rather than client/server?
                    problems.protocol(client.path(), server.path(), "Incompatible response types");
                }
                problems.append(compat);
            }
            (
                Method::OneWay { path: c_name, request: c_request },
                Method::OneWay { path: s_name, request: s_request },
            ) => {
                let compat = compare_types(c_request, s_request);
                if compat.is_incompatible() {
                    problems.protocol(client.path(), server.path(), "Incompatible request types");
                }
                problems.append(compat);
            }
            (
                Method::Event { path: c_name, payload: c_payload },
                Method::Event { path: s_name, payload: s_payload },
            ) => {
                let compat = compare_types(s_payload, c_payload);
                if compat.is_incompatible() {
                    problems.protocol(client.path(), server.path(), "Incompatible event types");
                }
                problems.append(compat);
            }
            _ => (), // Interaction kind mismatch handled elsewhere.
        }

        Ok(problems)
    }
}

pub trait Declaration {}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct Protocol {
    pub name: FlyStr,
    pub path: Path,
    pub openness: Openness,
    pub methods: BTreeMap<u64, Method>,
}

impl Protocol {
    // TODO: this needs to be reworked in light of the explicit external/platform split RFC
    fn compatible(external: &Self, platform: &Self) -> Result<CompatibilityProblems> {
        let mut problems = CompatibilityProblems::default();

        // Check for protocol openness mismatch.
        if external.openness != platform.openness {
            // TODO: This is probably too strict.
            // There should be a warning about a difference but an error if
            // there are methods or event that can't be handled.
            problems.platform(
                &external.path,
                &platform.path,
                format!(
                    "openness mismatch between external:{} and platform:{}",
                    external.path.api_level(),
                    platform.path.api_level()
                ),
            );
        }

        let external_ordinals: BTreeSet<u64> = external.methods.keys().cloned().collect();
        let platform_ordinals: BTreeSet<u64> = platform.methods.keys().cloned().collect();

        // Compare common interactions
        let common_ordinals: BTreeSet<u64> =
            external_ordinals.intersection(&platform_ordinals).cloned().collect();
        for o in &common_ordinals {
            let external_method = external.methods.get(o).unwrap();
            let platform_method = platform.methods.get(o).unwrap();
            if external_method.kind() != platform_method.kind() {
                problems.platform(
                    external_method.path(),
                    platform_method.path(),
                    format!(
                        "interaction kind differs between external:{} and platform:{}",
                        external_method.kind(),
                        platform_method.kind()
                    ),
                );
            }
            problems.append(Method::compatible(external_method, platform_method)?);
            problems.append(Method::compatible(platform_method, external_method)?);
        }

        Ok(problems)
    }
}

#[derive(Default, Clone, Debug)]
pub struct Platform {
    pub api_level: FlyStr,
    pub discoverable: HashMap<String, Protocol>,
    pub tear_off: HashMap<String, Protocol>,
}

pub fn compatible(external: &Platform, platform: &Platform) -> Result<CompatibilityProblems> {
    let mut problems = CompatibilityProblems::default();

    let external_discoverable: HashSet<String> = external.discoverable.keys().cloned().collect();
    let platform_discoverable: HashSet<String> = platform.discoverable.keys().cloned().collect();
    for p in external_discoverable.difference(&platform_discoverable) {
        problems.platform(
            &external.discoverable[p].path,
            &Path::new(&platform.api_level),
            format!("Discoverable protocol missing from: platform"),
        );
    }

    for p in platform_discoverable.difference(&external_discoverable) {
        problems.platform(
            &Path::new(&platform.api_level),
            &platform.discoverable[p].path,
            format!("Discoverable protocol missing from: external"),
        );
    }

    for p in external_discoverable.intersection(&platform_discoverable) {
        problems
            .append(Protocol::compatible(&external.discoverable[p], &platform.discoverable[p])?);
    }

    problems.sort();

    Ok(problems)
}
