// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use fidl_codec::{library as lib, Value as FidlValue};
use futures::future::BoxFuture;
use num::{bigint::BigInt, bigint::TryFromBigIntError, traits::ToPrimitive as _};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Error, Result};

mod in_use_handle;
mod iterator;

pub use in_use_handle::InUseHandle;
pub use iterator::{RangeCursor, ReplayableIterator, ReplayableIteratorCursor};

macro_rules! error {
    ($($data:tt)*) => { Error::from(anyhow!($($data)*)) };
}

/// Combines the [`lib::Type`] object from the FIDL codec with the [`lib::LookupResult`]
/// object. Basically the former contains base types and also handles to more
/// complex types whereas the latter contains detailed specifications of the
/// more complex types.
enum LookupResultOrType<'a> {
    LookupResult(lib::LookupResult<'a>),
    Type(lib::Type),
}

impl<'a> std::fmt::Display for LookupResultOrType<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn nullable_mark(nullable: &bool) -> &'static str {
            if *nullable {
                "?"
            } else {
                ""
            }
        }

        match self {
            LookupResultOrType::LookupResult(lib::LookupResult::Bits(lib::Bits {
                name, ..
            }))
            | LookupResultOrType::LookupResult(lib::LookupResult::Enum(lib::Enum {
                name, ..
            }))
            | LookupResultOrType::LookupResult(lib::LookupResult::Protocol(lib::Protocol {
                name,
                ..
            }))
            | LookupResultOrType::LookupResult(lib::LookupResult::Struct(lib::Struct {
                name,
                ..
            }))
            | LookupResultOrType::LookupResult(lib::LookupResult::Table(lib::TableOrUnion {
                name,
                ..
            }))
            | LookupResultOrType::LookupResult(lib::LookupResult::Union(lib::TableOrUnion {
                name,
                ..
            })) => {
                write!(f, "{}", name)
            }
            LookupResultOrType::Type(lib::Type::Unknown(_))
            | LookupResultOrType::Type(lib::Type::UnknownString(_)) => write!(f, "<UNKNOWN>"),
            LookupResultOrType::Type(lib::Type::Identifier { name, nullable }) => {
                write!(f, "{}{}", name, nullable_mark(nullable))
            }
            LookupResultOrType::Type(lib::Type::Request { identifier, rights: _, nullable }) => {
                write!(f, "{}{}", identifier, nullable_mark(nullable))
            }
            LookupResultOrType::Type(lib::Type::Bool) => write!(f, "bool"),
            LookupResultOrType::Type(lib::Type::U8) => write!(f, "u8"),
            LookupResultOrType::Type(lib::Type::U16) => write!(f, "u16"),
            LookupResultOrType::Type(lib::Type::U32) => write!(f, "u32"),
            LookupResultOrType::Type(lib::Type::U64) => write!(f, "u64"),
            LookupResultOrType::Type(lib::Type::I8) => write!(f, "i8"),
            LookupResultOrType::Type(lib::Type::I16) => write!(f, "i16"),
            LookupResultOrType::Type(lib::Type::I32) => write!(f, "i32"),
            LookupResultOrType::Type(lib::Type::I64) => write!(f, "i64"),
            LookupResultOrType::Type(lib::Type::F32) => write!(f, "f32"),
            LookupResultOrType::Type(lib::Type::F64) => write!(f, "f64"),
            LookupResultOrType::Type(lib::Type::Array(ty, size)) => {
                write!(f, "{}[{}]", LookupResultOrType::Type((**ty).clone()), size)
            }
            LookupResultOrType::Type(lib::Type::Vector { ty, element_count: _, nullable }) => {
                write!(
                    f,
                    "{}[]{}",
                    LookupResultOrType::Type((**ty).clone()),
                    nullable_mark(nullable)
                )
            }
            LookupResultOrType::Type(lib::Type::String { nullable, byte_count: _ }) => {
                write!(f, "String{}", nullable_mark(nullable))
            }
            LookupResultOrType::Type(lib::Type::Handle { object_type, .. }) => match object_type {
                &fidl::ObjectType::BTI => write!(f, "bti"),
                &fidl::ObjectType::CHANNEL => write!(f, "channel"),
                &fidl::ObjectType::CLOCK => write!(f, "clock"),
                &fidl::ObjectType::DEBUGLOG => write!(f, "debuglog"),
                &fidl::ObjectType::EVENT => write!(f, "event"),
                &fidl::ObjectType::EVENTPAIR => write!(f, "eventpair"),
                &fidl::ObjectType::EXCEPTION => write!(f, "exception"),
                &fidl::ObjectType::INTERRUPT => write!(f, "interrupt"),
                &fidl::ObjectType::IOMMU => write!(f, "iommu"),
                &fidl::ObjectType::JOB => write!(f, "job"),
                &fidl::ObjectType::MSI => write!(f, "msi"),
                &fidl::ObjectType::NONE => write!(f, "handle"),
                &fidl::ObjectType::PAGER => write!(f, "pager"),
                &fidl::ObjectType::PCI_DEVICE => write!(f, "pci_device"),
                &fidl::ObjectType::PMT => write!(f, "pmt"),
                &fidl::ObjectType::PORT => write!(f, "port"),
                &fidl::ObjectType::PROCESS => write!(f, "process"),
                &fidl::ObjectType::PROFILE => write!(f, "profile"),
                &fidl::ObjectType::RESOURCE => write!(f, "resource"),
                &fidl::ObjectType::SOCKET => write!(f, "socket"),
                &fidl::ObjectType::STREAM => write!(f, "stream"),
                &fidl::ObjectType::SUSPEND_TOKEN => write!(f, "suspend_token"),
                &fidl::ObjectType::THREAD => write!(f, "thread"),
                &fidl::ObjectType::TIMER => write!(f, "timer"),
                &fidl::ObjectType::VCPU => write!(f, "vcpu"),
                &fidl::ObjectType::VMAR => write!(f, "vmar"),
                &fidl::ObjectType::VMO => write!(f, "vmo"),
                _ => write!(f, "unknown_type_handle"),
            },
            LookupResultOrType::Type(lib::Type::FrameworkError) => {
                write!(f, "<<FrameworkError>>")
            }
        }
    }
}

/// An invocable value, such as a function or closure.
#[derive(Clone)]
pub struct Invocable(
    Arc<dyn Fn(Vec<Value>, Option<Value>) -> BoxFuture<'static, Result<Value>> + Send + Sync>,
);

impl Invocable {
    /// Construct a new invocable.
    pub fn new(
        f: Arc<
            dyn Fn(Vec<Value>, Option<Value>) -> BoxFuture<'static, Result<Value>> + Send + Sync,
        >,
    ) -> Self {
        Self(f)
    }

    /// Invoke this invocable.
    pub async fn invoke(self, args: Vec<Value>, underscore: Option<Value>) -> Result<Value> {
        self.0(args, underscore).await
    }
}

pub type Value = FidlValue<PlaygroundValue>;

pub trait ValueExt: Sized {
    /// Clones this value. May modify the original value to introduce reference counting.
    fn duplicate(&mut self) -> Self;

    /// Convert this value to a raw `BigInt` if possible.
    fn try_big_num(self) -> Result<BigInt, Self>;

    /// Convert this value to a `usize` if possible.
    fn try_usize(self) -> Result<usize, Self>;

    /// If this object is a client with the given protocol, return the raw channel.
    fn try_client_channel(
        self,
        ns: &lib::Namespace,
        expected_protocol: &str,
    ) -> Result<fidl::Channel, Self>;

    /// Convert a playground value to one that is ready for transfer via FIDL by
    /// converting playground-specific types. This performs *minimal* type checking; only
    /// what happens in the course of figuring out what conversion is appropriate.
    /// The FIDL codec should catch the rest.
    fn to_fidl_value(self, ns: &lib::Namespace, ty: &lib::Type) -> Result<FidlValue>;

    /// Get an [`InUseHandle`] from this value. Will convert directly-stored
    /// handle values or return the wrapped [`InUseHandle`] if applicable.
    fn to_in_use_handle(self) -> Option<InUseHandle>;

    /// Whether this value represents a client endpoint for the given service.
    fn is_client(&self, service: &str) -> bool;
}

impl ValueExt for Value {
    fn duplicate(&mut self) -> Self {
        match self {
            Value::Null => Value::Null,
            Value::Bool(a) => Value::Bool(*a),
            Value::U8(a) => Value::U8(*a),
            Value::U16(a) => Value::U16(*a),
            Value::U32(a) => Value::U32(*a),
            Value::U64(a) => Value::U64(*a),
            Value::I8(a) => Value::I8(*a),
            Value::I16(a) => Value::I16(*a),
            Value::I32(a) => Value::I32(*a),
            Value::I64(a) => Value::I64(*a),
            Value::F32(a) => Value::F32(*a),
            Value::F64(a) => Value::F64(*a),
            Value::String(a) => Value::String(a.clone()),
            Value::Object(a) => {
                Value::Object(a.iter_mut().map(|(a, b)| (a.clone(), b.duplicate())).collect())
            }
            Value::Bits(a, b) => Value::Bits(a.clone(), Box::new(b.duplicate())),
            Value::Enum(a, b) => Value::Enum(a.clone(), Box::new(b.duplicate())),
            Value::Union(a, b, c) => Value::Union(a.clone(), b.clone(), Box::new(c.duplicate())),
            Value::List(a) => Value::List(a.iter_mut().map(|x| x.duplicate()).collect()),

            Value::ServerEnd(_, _) => {
                let Value::ServerEnd(a, b) = std::mem::replace(self, Value::Null) else {
                    unreachable!();
                };
                let mut playground_value =
                    PlaygroundValue::InUseHandle(InUseHandle::server_end(a, b));
                *self = Value::OutOfLine(playground_value.duplicate());
                Value::OutOfLine(playground_value)
            }
            Value::ClientEnd(_, _) => {
                let Value::ClientEnd(a, b) = std::mem::replace(self, Value::Null) else {
                    unreachable!();
                };
                let mut playground_value =
                    PlaygroundValue::InUseHandle(InUseHandle::client_end(a, b));
                *self = Value::OutOfLine(playground_value.duplicate());
                Value::OutOfLine(playground_value)
            }
            Value::Handle(_, _) => {
                let Value::Handle(a, b) = std::mem::replace(self, Value::Null) else {
                    unreachable!();
                };
                let mut playground_value = PlaygroundValue::InUseHandle(InUseHandle::handle(a, b));
                *self = Value::OutOfLine(playground_value.duplicate());
                Value::OutOfLine(playground_value)
            }
            Value::OutOfLine(a) => Value::OutOfLine(a.duplicate()),
        }
    }

    fn try_big_num(self) -> Result<BigInt, Value> {
        match self {
            Value::U8(i) => Ok(i.into()),
            Value::U16(i) => Ok(i.into()),
            Value::U32(i) => Ok(i.into()),
            Value::U64(i) => Ok(i.into()),
            Value::I8(i) => Ok(i.into()),
            Value::I16(i) => Ok(i.into()),
            Value::I32(i) => Ok(i.into()),
            Value::I64(i) => Ok(i.into()),
            Value::Bits(s, v) => v.try_big_num().map_err(|v| Value::Bits(s, Box::new(v))),
            Value::Enum(s, v) => v.try_big_num().map_err(|v| Value::Enum(s, Box::new(v))),
            Value::OutOfLine(PlaygroundValue::Num(s)) => Ok(s),
            _ => Err(self),
        }
    }

    fn try_usize(self) -> Result<usize, Value> {
        match self {
            Value::U8(i) => Ok(i.into()),
            Value::U16(i) => Ok(i.into()),
            Value::U32(i) => i.try_into().map_err(|_| Value::U32(i)),
            Value::U64(i) => i.try_into().map_err(|_| Value::U64(i)),
            Value::I8(i) => i.try_into().map_err(|_| Value::I8(i)),
            Value::I16(i) => i.try_into().map_err(|_| Value::I16(i)),
            Value::I32(i) => i.try_into().map_err(|_| Value::I32(i)),
            Value::I64(i) => i.try_into().map_err(|_| Value::I64(i)),
            Value::Bits(s, v) => v.try_usize().map_err(|v| Value::Bits(s, Box::new(v))),
            Value::Enum(s, v) => v.try_usize().map_err(|v| Value::Enum(s, Box::new(v))),
            Value::OutOfLine(PlaygroundValue::Num(s)) => {
                s.try_into().map_err(|x: TryFromBigIntError<BigInt>| {
                    Value::OutOfLine(PlaygroundValue::Num(x.into_original()))
                })
            }
            _ => Err(self),
        }
    }

    fn try_client_channel(
        self,
        ns: &lib::Namespace,
        expected_protocol: &str,
    ) -> Result<fidl::Channel, Self> {
        match self {
            Value::ClientEnd(c, proto) if ns.inherits(&proto, expected_protocol) => Ok(c),
            Value::OutOfLine(PlaygroundValue::InUseHandle(ref i)) => {
                if let Ok(FidlValue::ClientEnd(c, _)) = i.take_client(expected_protocol) {
                    Ok(c)
                } else {
                    Err(self)
                }
            }
            other => Err(other),
        }
    }

    fn to_fidl_value(self, ns: &lib::Namespace, ty: &lib::Type) -> Result<FidlValue> {
        let ret = match self {
            Value::Null => FidlValue::Null,
            Value::Bool(a) => FidlValue::Bool(a),
            Value::U8(a) => FidlValue::U8(a),
            Value::U16(a) => FidlValue::U16(a),
            Value::U32(a) => FidlValue::U32(a),
            Value::U64(a) => FidlValue::U64(a),
            Value::I8(a) => FidlValue::I8(a),
            Value::I16(a) => FidlValue::I16(a),
            Value::I32(a) => FidlValue::I32(a),
            Value::I64(a) => FidlValue::I64(a),
            Value::F32(a) => FidlValue::F32(a),
            Value::F64(a) => FidlValue::F64(a),
            Value::String(a) => FidlValue::String(a),
            Value::Object(mut a) => match ty {
                lib::Type::Identifier { name, nullable: _ } => match ns.lookup(name)? {
                    lib::LookupResult::Struct(st) => FidlValue::Object(
                        a.into_iter()
                            .map(|(key, val)| {
                                let member = st
                                    .members
                                    .iter()
                                    .find(|x| x.name == key)
                                    .ok_or(error!("{name} does not have a member {key}"))?;
                                Ok((key, val.to_fidl_value(ns, &member.ty)?))
                            })
                            .collect::<Result<_>>()?,
                    ),
                    lib::LookupResult::Table(tbl) => FidlValue::Object(
                        a.into_iter()
                            .map(|(key, val)| {
                                let (_, member) = tbl
                                    .members
                                    .iter()
                                    .find(|(_, x)| x.name == key)
                                    .ok_or(error!("{name} does not have a member {key}"))?;
                                Ok((key, val.to_fidl_value(ns, &member.ty)?))
                            })
                            .collect::<Result<_>>()?,
                    ),
                    lib::LookupResult::Union(un) => {
                        let item = a.pop();
                        let (key, value) =
                            if let Some((key, value)) = item.filter(|_| a.is_empty()) {
                                (key, value)
                            } else {
                                return Err(error!(
                                    "Struct coerced to union must have exactly one member"
                                ));
                            };
                        let (_, member) = un
                            .members
                            .iter()
                            .find(|(_, x)| x.name == key)
                            .ok_or(error!("{name} does not have a member {key}"))?;
                        FidlValue::Union(
                            name.clone(),
                            key,
                            Box::new(value.to_fidl_value(ns, &member.ty)?),
                        )
                    }
                    other => {
                        return Err(error!(
                            "Object used in FIDL must be used as a struct, table, or union (Cannot convert {a:?} to {})",
                            LookupResultOrType::LookupResult(other)
                        ))
                    }
                },
                other => return Err(error!("Cannot convert object to type {other:?}")),
            },
            Value::Bits(a, b) => {
                let lib::LookupResult::Bits(bits) = ns.lookup(&a)? else {
                    return Err(error!("Value of type {a} is encoded as bits but isn't bits"));
                };
                FidlValue::Bits(a, Box::new(b.to_fidl_value(ns, &bits.ty)?))
            }
            Value::Enum(a, b) => {
                let lib::LookupResult::Enum(en) = ns.lookup(&a)? else {
                    return Err(error!(
                        "Value of type {a} is encoded as an enum but isn't an enum"
                    ));
                };
                FidlValue::Enum(a, Box::new(b.to_fidl_value(ns, &en.ty)?))
            }
            Value::Union(a, b, c) => {
                let lib::LookupResult::Union(union) = ns.lookup(&a)? else {
                    return Err(error!(
                        "Value of type {a} is encoded as a union but isn't a union"
                    ));
                };
                for member in union.members.values() {
                    if member.name == b {
                        return Ok(FidlValue::Union(a, b, Box::new(c.to_fidl_value(ns, &member.ty)?)));
                    }
                }

                return Err(error!("Union of type {a} has no member {b}"));
            }
            Value::List(a) => match ty {
                lib::Type::Identifier { name, nullable: _ } => match ns.lookup(name)? {
                    lib::LookupResult::Struct(st) => {
                        if a.len() > st.members.len() {
                            return Err(error!("List too short to encode as struct '{name}'"));
                        }
                        FidlValue::Object(
                            st.members
                                .iter()
                                .zip(a.into_iter().chain(std::iter::repeat_with(|| Value::Null)))
                                .map(|(x, y)| {
                                    y.to_fidl_value(ns, &x.ty).map(|y| (x.name.to_owned(), y))
                                })
                                .collect::<Result<_>>()?,
                        )
                    }
                    lib::LookupResult::Table(tbl) => {
                        let mut members = Vec::with_capacity(a.len());
                        for (i, value) in a.into_iter().enumerate() {
                            let ordinal = i as u64 + 1;
                            let Some(member) = tbl.members.get(&ordinal) else {
                                if matches!(value, Value::Null) {
                                    continue;
                                } else {
                                    return Err(error!("Cannot convert list to table because cannot encode a value at ordinal {ordinal}"));
                                }
                            };
                            members.push((member.name.to_owned(), value.to_fidl_value(ns, &member.ty)?));
                        }
                        FidlValue::Object(members)
                    }
                    _ => return Err(error!("Cannot encode list as type {ty:?}")),
                },
                lib::Type::Array(ty, _) => FidlValue::List(
                    a.into_iter().map(|x| x.to_fidl_value(ns, &*ty)).collect::<Result<_>>()?,
                ),
                lib::Type::Vector { ty, .. } => FidlValue::List(
                    a.into_iter().map(|x| x.to_fidl_value(ns, ty)).collect::<Result<_>>()?,
                ),
                _ => return Err(error!("Cannot encode list as type {ty:?}")),
            },
            Value::ServerEnd(a, b) => FidlValue::ServerEnd(a, b),
            Value::ClientEnd(a, b) => FidlValue::ClientEnd(a, b),
            Value::Handle(a, b) => FidlValue::Handle(a, b),
            Value::OutOfLine(s) => s.to_fidl_value(ns, ty)?,
        };

        Ok(ret)
    }

    fn to_in_use_handle(self) -> Option<InUseHandle> {
        match self {
            Value::ServerEnd(h, p) => Some(InUseHandle::server_end(h, p)),
            Value::ClientEnd(h, p) => Some(InUseHandle::client_end(h, p)),
            Value::Handle(h, t) => Some(InUseHandle::handle(h, t)),
            Value::OutOfLine(PlaygroundValue::InUseHandle(s)) => Some(s),
            _ => None,
        }
    }

    fn is_client(&self, service: &str) -> bool {
        match self {
            Value::ClientEnd(_, s) => s == service,
            Value::OutOfLine(PlaygroundValue::InUseHandle(s)) => {
                s.get_client_protocol().ok().map(|x| &x == service).unwrap_or(false)
            }
            _ => false,
        }
    }
}

/// These are value types which aren't strictly part of the FIDL value structure
/// but instead serve a purpose within the playground environment.
pub enum PlaygroundValue {
    /// A function or closure.
    Invocable(Invocable),
    /// A handle which can have multiple owners.
    InUseHandle(InUseHandle),
    /// A number with no precision limits.
    Num(BigInt),
    /// An iterator.
    Iterator(ReplayableIterator),
    /// A value with a type hint associated with it.
    TypeHinted(String, Box<Value>),
}

impl PlaygroundValue {
    /// Convert this playground value to a raw FIDL value if possible.
    fn to_fidl_value_by_type_or_lookup(
        self,
        ns: &lib::Namespace,
        ty: LookupResultOrType<'_>,
    ) -> Result<FidlValue> {
        match (ty, self) {
            (LookupResultOrType::Type(ty), PlaygroundValue::TypeHinted(_, y)) => {
                y.to_fidl_value(ns, &ty)
            }
            (LookupResultOrType::Type(lib::Type::U8), PlaygroundValue::Num(x)) => {
                x.to_u8().map(FidlValue::U8).ok_or(error!("Overflow converting to u8"))
            }
            (LookupResultOrType::Type(lib::Type::U16), PlaygroundValue::Num(x)) => {
                x.to_u16().map(FidlValue::U16).ok_or(error!("Overflow converting to u16"))
            }
            (LookupResultOrType::Type(lib::Type::U32), PlaygroundValue::Num(x)) => {
                x.to_u32().map(FidlValue::U32).ok_or(error!("Overflow converting to u32"))
            }
            (LookupResultOrType::Type(lib::Type::U64), PlaygroundValue::Num(x)) => {
                x.to_u64().map(FidlValue::U64).ok_or(error!("Overflow converting to u64"))
            }
            (LookupResultOrType::Type(lib::Type::I8), PlaygroundValue::Num(x)) => {
                x.to_i8().map(FidlValue::I8).ok_or(error!("Overflow converting to i8"))
            }
            (LookupResultOrType::Type(lib::Type::I16), PlaygroundValue::Num(x)) => {
                x.to_i16().map(FidlValue::I16).ok_or(error!("Overflow converting to i16"))
            }
            (LookupResultOrType::Type(lib::Type::I32), PlaygroundValue::Num(x)) => {
                x.to_i32().map(FidlValue::I32).ok_or(error!("Overflow converting to i32"))
            }
            (LookupResultOrType::Type(lib::Type::I64), PlaygroundValue::Num(x)) => {
                x.to_i64().map(FidlValue::I64).ok_or(error!("Overflow converting to i64"))
            }
            (LookupResultOrType::Type(lib::Type::F32), PlaygroundValue::Num(x)) => {
                x.to_f32().map(FidlValue::F32).ok_or(error!("Overflow converting to f32"))
            }
            (LookupResultOrType::Type(lib::Type::F64), PlaygroundValue::Num(x)) => {
                x.to_f64().map(FidlValue::F64).ok_or(error!("Overflow converting to f64"))
            }
            (LookupResultOrType::Type(lib::Type::Identifier { name, .. }), v) => v
                .to_fidl_value_by_type_or_lookup(
                    ns,
                    LookupResultOrType::LookupResult(ns.lookup(&name)?),
                ),
            (
                LookupResultOrType::LookupResult(lib::LookupResult::Bits(lib::Bits { ty, .. })),
                v,
            ) => v.to_fidl_value_by_type_or_lookup(ns, LookupResultOrType::Type(ty.clone())),
            (
                LookupResultOrType::LookupResult(lib::LookupResult::Enum(lib::Enum { ty, .. })),
                v,
            ) => v.to_fidl_value_by_type_or_lookup(ns, LookupResultOrType::Type(ty.clone())),
            (
                LookupResultOrType::Type(lib::Type::Request { identifier, .. }),
                PlaygroundValue::InUseHandle(h),
            ) => {
                let h = h.take_server(&identifier)?;
                let FidlValue::ServerEnd(_, s) = &h else {
                    return Err(error!("Cannot conver type to server for {identifier}"));
                };
                if &identifier == s {
                    Ok(h)
                } else {
                    Err(error!("Cannot convert handle to {} to handle to {}", s, identifier))
                }
            }
            (
                LookupResultOrType::LookupResult(lib::LookupResult::Protocol(lib::Protocol {
                    name,
                    ..
                })),
                PlaygroundValue::InUseHandle(h),
            ) => {
                let h = h.take_client(&name)?;
                let FidlValue::ClientEnd(_, s) = &h else {
                    return Err(error!("Cannot conver type to server for {name}"));
                };
                if name == s {
                    Ok(h)
                } else {
                    Err(error!("Cannot convert handle to {} to handle to {}", s, name))
                }
            }
            (LookupResultOrType::Type(ty), PlaygroundValue::Invocable(_)) => {
                Err(error!("Cannot convert invocable to {ty:?}"))
            }
            (LookupResultOrType::Type(ty), PlaygroundValue::Num(_)) => {
                Err(error!("Cannot convert number to {ty:?}"))
            }
            (LookupResultOrType::Type(ty), PlaygroundValue::Iterator(_)) => {
                Err(error!("Cannot convert iterator to {ty:?}"))
            }
            _ => Err(error!("Cannot convert to FIDL type")),
        }
    }

    /// Convert this playground value to a raw FIDL value if possible.
    fn to_fidl_value(self, ns: &lib::Namespace, ty: &lib::Type) -> Result<FidlValue> {
        self.to_fidl_value_by_type_or_lookup(ns, LookupResultOrType::Type(ty.clone()))
    }

    /// Duplicate this value. May modify the original value to convert
    /// single-owner forms to refcounted forms etc.
    fn duplicate(&mut self) -> Self {
        match self {
            PlaygroundValue::Invocable(a) => PlaygroundValue::Invocable(a.clone()),
            PlaygroundValue::InUseHandle(a) => PlaygroundValue::InUseHandle(a.clone()),
            PlaygroundValue::Num(a) => PlaygroundValue::Num(a.clone()),
            PlaygroundValue::Iterator(a) => PlaygroundValue::Iterator(a.clone()),
            PlaygroundValue::TypeHinted(a, b) => {
                PlaygroundValue::TypeHinted(a.clone(), Box::new(b.duplicate()))
            }
        }
    }
}

/// Compares two playground values. Automatically handles some niceties like upcasting integers etc.
pub fn playground_semantic_compare(this: &Value, other: &Value) -> Option<Ordering> {
    fn try_promote(a: &Value) -> Option<BigInt> {
        match a {
            Value::U8(x) => Some((*x).into()),
            Value::U16(x) => Some((*x).into()),
            Value::U32(x) => Some((*x).into()),
            Value::U64(x) => Some((*x).into()),
            Value::I8(x) => Some((*x).into()),
            Value::I16(x) => Some((*x).into()),
            Value::I32(x) => Some((*x).into()),
            Value::I64(x) => Some((*x).into()),
            Value::OutOfLine(PlaygroundValue::Num(x)) => Some(x.clone()),
            _ => None,
        }
    }

    fn cmp_to_fidl(a: &PlaygroundValue, b: &Value) -> Option<Ordering> {
        match (a, b) {
            (a, Value::Bits(_, b)) => cmp_to_fidl(a, b),
            (a, Value::Enum(_, b)) => cmp_to_fidl(a, b),
            (PlaygroundValue::Num(a), Value::U8(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::U16(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::U32(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::U64(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::I8(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::I16(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::I32(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            (PlaygroundValue::Num(a), Value::I64(b)) => PartialOrd::partial_cmp(a, &(*b).into()),
            _ => None,
        }
    }

    match (this, other) {
        (a @ Value::OutOfLine(_), b @ Value::OutOfLine(_)) => {
            if let (Some(a), Some(b)) = (try_promote(a), try_promote(b)) {
                PartialOrd::partial_cmp(&a, &b)
            } else {
                None
            }
        }
        (Value::OutOfLine(a), b) => cmp_to_fidl(a, b),
        (a, Value::OutOfLine(b)) => cmp_to_fidl(b, a).map(Ordering::reverse),
        (Value::Bits(_, a), b) => playground_semantic_compare(a, b),
        (a, Value::Bits(_, b)) => playground_semantic_compare(a, b),
        (Value::Enum(_, a), b) => playground_semantic_compare(a, b),
        (a, Value::Enum(_, b)) => playground_semantic_compare(a, b),
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        (Value::Bool(a), Value::Bool(b)) => PartialOrd::partial_cmp(a, b),
        (Value::String(a), Value::String(b)) => PartialOrd::partial_cmp(a, b),
        (Value::Object(a), Value::Object(b)) => {
            if a.len() != b.len() {
                None
            } else {
                let mut values = HashMap::new();

                for (name, value) in a.iter() {
                    values.insert(name, value);
                }

                let mut ret = Some(Ordering::Equal);

                for (name, value) in b.iter() {
                    if let Some(v) = values.get(name) {
                        if playground_semantic_compare(value, *v)
                            .map(|x| x.is_eq())
                            .unwrap_or(false)
                        {
                            continue;
                        }
                    }

                    ret = None;
                    break;
                }

                ret
            }
        }
        (Value::U8(a), Value::U8(b)) => PartialOrd::partial_cmp(a, b),
        (Value::U16(a), Value::U16(b)) => PartialOrd::partial_cmp(a, b),
        (Value::U32(a), Value::U32(b)) => PartialOrd::partial_cmp(a, b),
        (Value::U64(a), Value::U64(b)) => PartialOrd::partial_cmp(a, b),
        (Value::I8(a), Value::I8(b)) => PartialOrd::partial_cmp(a, b),
        (Value::I16(a), Value::I16(b)) => PartialOrd::partial_cmp(a, b),
        (Value::I32(a), Value::I32(b)) => PartialOrd::partial_cmp(a, b),
        (Value::I64(a), Value::I64(b)) => PartialOrd::partial_cmp(a, b),
        (Value::List(a), Value::List(b)) => a
            .iter()
            .zip(b.iter())
            .map(|(a, b)| playground_semantic_compare(a, b))
            .find(|x| *x != Some(Ordering::Equal))
            .unwrap_or(PartialOrd::partial_cmp(&a.len(), &b.len())),
        (a, b) => {
            if let (Some(a), Some(b)) = (try_promote(a), try_promote(b)) {
                PartialOrd::partial_cmp(&a, &b)
            } else {
                None
            }
        }
    }
}

impl std::fmt::Debug for PlaygroundValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for PlaygroundValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaygroundValue::Invocable(Invocable(a)) => write!(f, "<function@{:p}>", a),
            PlaygroundValue::Num(x) => std::fmt::Display::fmt(x, f),
            PlaygroundValue::Iterator(_) => write!(f, "<iterator>"),
            PlaygroundValue::InUseHandle(_) => write!(f, "<handle in use>"),
            PlaygroundValue::TypeHinted(hint, v) => write!(f, "@{hint} {v}"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::HandleBased;
    use futures::FutureExt;

    #[test]
    fn compare() {
        assert!(playground_semantic_compare(&Value::U8(2), &Value::U32(1)).unwrap().is_gt());
        assert!(playground_semantic_compare(&Value::U32(2), &Value::U8(1)).unwrap().is_gt());
        assert!(playground_semantic_compare(&Value::I32(-2), &Value::U8(1)).unwrap().is_lt());
        assert!(playground_semantic_compare(
            &Value::Bits("pumpkin".into(), Box::new(Value::U8(2))),
            &Value::U32(1)
        )
        .unwrap()
        .is_gt());
        assert!(playground_semantic_compare(
            &Value::Enum("pumpkin".into(), Box::new(Value::U8(2))),
            &Value::U32(1)
        )
        .unwrap()
        .is_gt());
        assert!(playground_semantic_compare(
            &Value::String("a".into()),
            &Value::String("a".into())
        )
        .unwrap()
        .is_eq());
        assert!(playground_semantic_compare(
            &Value::String("a".into()),
            &Value::String("b".into())
        )
        .unwrap()
        .is_lt());
        assert!(playground_semantic_compare(
            &Value::List(vec![Value::U8(1), Value::U8(2)]),
            &Value::List(vec![Value::U8(1), Value::U8(2)])
        )
        .unwrap()
        .is_eq());
        assert!(playground_semantic_compare(
            &Value::List(vec![Value::U8(1), Value::U8(2)]),
            &Value::List(vec![Value::U8(1), Value::U8(3)])
        )
        .unwrap()
        .is_lt());
        assert!(playground_semantic_compare(
            &Value::List(vec![Value::U8(1), Value::U8(2)]),
            &Value::List(vec![Value::U8(1), Value::U8(2), Value::U8(0)])
        )
        .unwrap()
        .is_lt());
        assert!(playground_semantic_compare(
            &Value::Object(vec![("foo".into(), Value::U8(1)), ("bar".into(), Value::U8(2))]),
            &Value::Object(vec![("bar".into(), Value::U8(2)), ("foo".into(), Value::U8(1))]),
        )
        .unwrap()
        .is_eq());
        assert!(playground_semantic_compare(
            &Value::Object(vec![("foo".into(), Value::U8(1)), ("bar".into(), Value::U8(1))]),
            &Value::Object(vec![("bar".into(), Value::U8(2)), ("foo".into(), Value::U8(1))]),
        )
        .is_none());
    }

    #[test]
    fn to_fidl_struct() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();

        let ty = lib::Type::Identifier {
            name: "test.fidlcodec.examples/PrimitiveTypes".to_owned(),
            nullable: false,
        };
        let fidl_value = Value::Object(vec![
            ("s".to_owned(), Value::String("floop".to_owned())),
            ("b".to_owned(), Value::Bool(true)),
            ("i8".to_owned(), Value::I8(1)),
            ("i16".to_owned(), Value::I16(2)),
            ("i32".to_owned(), Value::I32(3)),
            ("i64".to_owned(), Value::I64(4)),
            ("u8".to_owned(), Value::U8(5)),
            ("u16".to_owned(), Value::U16(6)),
            ("u32".to_owned(), Value::U32(7)),
            ("u64".to_owned(), Value::U64(8)),
            ("f32".to_owned(), Value::F32(9.0)),
            ("f64".to_owned(), Value::F64(10.0)),
        ])
        .to_fidl_value(&ns, &ty)
        .unwrap();
        let FidlValue::Object(fields) = fidl_value else { panic!() };

        let mut checks: HashMap<_, _> = [
            (
                "s",
                (&|x: &FidlValue| if let FidlValue::String(s) = x { s == "floop" } else { false })
                    as &dyn Fn(&FidlValue) -> _,
            ),
            ("b", &|x| matches!(x, FidlValue::Bool(true))),
            ("i8", &|x| matches!(x, FidlValue::I8(1))),
            ("i16", &|x| matches!(x, FidlValue::I16(2))),
            ("i32", &|x| matches!(x, FidlValue::I32(3))),
            ("i64", &|x| matches!(x, FidlValue::I64(4))),
            ("u8", &|x| matches!(x, FidlValue::U8(5))),
            ("u16", &|x| matches!(x, FidlValue::U16(6))),
            ("u32", &|x| matches!(x, FidlValue::U32(7))),
            ("u64", &|x| matches!(x, FidlValue::U64(8))),
            ("f32", &|x| if let FidlValue::F32(f) = x { *f == 9.0 } else { false }),
            ("f64", &|x| if let FidlValue::F64(f) = x { *f == 10.0 } else { false }),
        ]
        .into_iter()
        .collect();

        assert!(fields.len() == 12);
        for (key, value) in fields.into_iter() {
            let check = checks.remove(key.as_str()).unwrap();
            assert!(check(&value), "Field {key:?} had value {value:?}");
        }
    }

    #[test]
    fn to_fidl_struct_extra_field() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();

        let ty = lib::Type::Identifier {
            name: "test.fidlcodec.examples/PrimitiveTypes".to_owned(),
            nullable: false,
        };
        Value::Object(vec![
            ("s".to_owned(), Value::String("floop".to_owned())),
            ("b".to_owned(), Value::Bool(true)),
            ("i8".to_owned(), Value::I8(1)),
            ("i16".to_owned(), Value::I16(2)),
            ("i32".to_owned(), Value::I32(3)),
            ("i64".to_owned(), Value::I64(4)),
            ("u8".to_owned(), Value::U8(5)),
            ("u16".to_owned(), Value::U16(6)),
            ("u32".to_owned(), Value::U32(7)),
            ("u64".to_owned(), Value::U64(8)),
            ("f32".to_owned(), Value::F32(9.0)),
            ("f64".to_owned(), Value::F64(10.0)),
            ("extr_field".to_owned(), Value::F64(10.0)),
        ])
        .to_fidl_value(&ns, &ty)
        .unwrap_err();
    }

    #[test]
    fn to_fidl_table() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();

        let ty = lib::Type::Identifier {
            name: "test.fidlcodec.examples/ValueTable".to_owned(),
            nullable: false,
        };
        let fidl_value = Value::Object(vec![
            ("first_int16".to_owned(), Value::I16(2)),
            (
                "second_struct".to_owned(),
                Value::Object(vec![
                    ("value1".to_owned(), Value::String("pumpkin".to_owned())),
                    ("value2".to_owned(), Value::String("floop".to_owned())),
                ]),
            ),
            (
                "third_union".to_owned(),
                Value::Object(vec![("variant_i".to_owned(), Value::I32(3))]),
            ),
        ])
        .to_fidl_value(&ns, &ty)
        .unwrap();
        let FidlValue::Object(fields) = fidl_value else { panic!() };

        let mut checks: HashMap<_, _> = [
            (
                "first_int16",
                (&|x: &FidlValue| matches!(x, FidlValue::I16(2))) as &dyn Fn(&FidlValue) -> bool,
            ),
            ("second_struct", &|x| {
                if let FidlValue::Object(fields) = x {
                    fields.len() == 2
                        && fields.iter().any(|(k, v)| {
                            k == "value1"
                                && if let FidlValue::String(v) = v { v == "pumpkin" } else { false }
                        })
                        && fields.iter().any(|(k, v)| {
                            k == "value2"
                                && if let FidlValue::String(v) = v { v == "floop" } else { false }
                        })
                } else {
                    false
                }
            }),
            ("third_union", &|x| {
                if let FidlValue::Union(ident, variant, field) = x {
                    ident == "test.fidlcodec.examples/IntStructUnion"
                        && variant == "variant_i"
                        && matches!(**field, FidlValue::I32(3))
                } else {
                    false
                }
            }),
        ]
        .into_iter()
        .collect();

        assert!(fields.len() == 3);
        for (key, value) in fields.into_iter() {
            let check = checks.remove(key.as_str()).unwrap();
            assert!(check(&value), "Field {key:?} had value {value:?}");
        }
    }

    #[test]
    fn list_to_fidl_struct() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();

        let ty = lib::Type::Identifier {
            name: "test.fidlcodec.examples/PrimitiveTypes".to_owned(),
            nullable: false,
        };
        let fidl_value = Value::List(vec![
            Value::String("floop".to_owned()),
            Value::Bool(true),
            Value::I8(1),
            Value::I16(2),
            Value::I32(3),
            Value::I64(4),
            Value::U8(5),
            Value::U16(6),
            Value::U32(7),
            Value::U64(8),
            Value::F32(9.0),
            Value::F64(10.0),
        ])
        .to_fidl_value(&ns, &ty)
        .unwrap();
        let FidlValue::Object(fields) = fidl_value else { panic!() };

        let mut checks: HashMap<_, _> = [
            (
                "s",
                (&|x: &FidlValue| if let FidlValue::String(s) = x { s == "floop" } else { false })
                    as &dyn Fn(&FidlValue) -> _,
            ),
            ("b", &|x| matches!(x, FidlValue::Bool(true))),
            ("i8", &|x| matches!(x, FidlValue::I8(1))),
            ("i16", &|x| matches!(x, FidlValue::I16(2))),
            ("i32", &|x| matches!(x, FidlValue::I32(3))),
            ("i64", &|x| matches!(x, FidlValue::I64(4))),
            ("u8", &|x| matches!(x, FidlValue::U8(5))),
            ("u16", &|x| matches!(x, FidlValue::U16(6))),
            ("u32", &|x| matches!(x, FidlValue::U32(7))),
            ("u64", &|x| matches!(x, FidlValue::U64(8))),
            ("f32", &|x| if let FidlValue::F32(f) = x { *f == 9.0 } else { false }),
            ("f64", &|x| if let FidlValue::F64(f) = x { *f == 10.0 } else { false }),
        ]
        .into_iter()
        .collect();

        assert!(fields.len() == 12);
        for (key, value) in fields.into_iter() {
            let check = checks.remove(key.as_str()).unwrap();
            assert!(check(&value), "Field {key:?} had value {value:?}");
        }
    }

    #[test]
    fn list_to_fidl_table() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();

        let ty = lib::Type::Identifier {
            name: "test.fidlcodec.examples/ValueTable".to_owned(),
            nullable: false,
        };
        let fidl_value = Value::List(vec![
            Value::I16(2),
            Value::List(vec![
                Value::String("pumpkin".to_owned()),
                Value::String("floop".to_owned()),
            ]),
            Value::Null,
            Value::Object(vec![("variant_i".to_owned(), Value::I32(3))]),
        ])
        .to_fidl_value(&ns, &ty)
        .unwrap();
        let FidlValue::Object(fields) = fidl_value else { panic!() };

        let mut checks: HashMap<_, _> = [
            (
                "first_int16",
                (&|x: &FidlValue| matches!(x, FidlValue::I16(2))) as &dyn Fn(&FidlValue) -> bool,
            ),
            ("second_struct", &|x| {
                if let FidlValue::Object(fields) = x {
                    fields.len() == 2
                        && fields.iter().any(|(k, v)| {
                            k == "value1"
                                && if let FidlValue::String(v) = v { v == "pumpkin" } else { false }
                        })
                        && fields.iter().any(|(k, v)| {
                            k == "value2"
                                && if let FidlValue::String(v) = v { v == "floop" } else { false }
                        })
                } else {
                    false
                }
            }),
            ("third_union", &|x| {
                if let FidlValue::Union(ident, variant, field) = x {
                    ident == "test.fidlcodec.examples/IntStructUnion"
                        && variant == "variant_i"
                        && matches!(**field, FidlValue::I32(3))
                } else {
                    false
                }
            }),
        ]
        .into_iter()
        .collect();

        assert!(fields.len() == 3);
        for (key, value) in fields.into_iter() {
            let check = checks.remove(key.as_str()).unwrap();
            assert!(check(&value), "Field {key:?} had value {value:?}");
        }
    }

    #[test]
    fn list_to_fidl_table_reserved_field() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();

        let ty = lib::Type::Identifier {
            name: "test.fidlcodec.examples/ValueTable".to_owned(),
            nullable: false,
        };
        Value::List(vec![
            Value::I16(2),
            Value::List(vec![
                Value::String("pumpkin".to_owned()),
                Value::String("floop".to_owned()),
            ]),
            Value::I16(5),
            Value::Object(vec![("variant_i".to_owned(), Value::I32(3))]),
        ])
        .to_fidl_value(&ns, &ty)
        .unwrap_err();
    }

    #[fuchsia::test]
    async fn clonable_invocable() {
        let a = Invocable::new(Arc::new(|mut v, o| {
            async move {
                assert!(o.is_none());
                let value = v.pop().unwrap();
                assert!(v.is_empty());
                let Value::U8(v) = value else { panic!() };
                Ok(Value::U8(v + 2))
            }
            .boxed()
        }));
        let b = a.clone();
        let Ok(Value::U8(a)) = a.invoke(vec![Value::U8(7)], None).await else {
            panic!();
        };
        assert_eq!(9, a);
        let Ok(Value::U8(b)) = b.invoke(vec![Value::U8(3)], None).await else {
            panic!();
        };
        assert_eq!(5, b);
    }

    #[test]
    fn promote_handles() {
        let mut ns = lib::Namespace::new();
        ns.load(test_fidl::TEST_FIDL).unwrap();
        let (a, b) = fidl::Channel::create();

        let mut client =
            Value::ClientEnd(a, "test.fidlcodec.examples/FidlCodecTestProtocol".to_owned());
        let mut server =
            Value::ServerEnd(b, "test.fidlcodec.examples/FidlCodecTestProtocol".to_owned());
        let client_dup = client.duplicate();
        let server_dup = server.duplicate();

        assert!(matches!(client_dup, Value::OutOfLine(PlaygroundValue::InUseHandle(_))));
        assert!(matches!(server_dup, Value::OutOfLine(PlaygroundValue::InUseHandle(_))));

        let Ok(FidlValue::ClientEnd(_channel, protocol)) = client_dup.to_fidl_value(
            &ns,
            &lib::Type::Identifier {
                name: "test.fidlcodec.examples/FidlCodecTestProtocol".to_owned(),
                nullable: false,
            },
        ) else {
            panic!();
        };
        assert_eq!("test.fidlcodec.examples/FidlCodecTestProtocol", &protocol);

        let Ok(FidlValue::ServerEnd(_channel, protocol)) = server_dup.to_fidl_value(
            &ns,
            &lib::Type::Request {
                identifier: "test.fidlcodec.examples/FidlCodecTestProtocol".to_owned(),
                rights: fidl::Rights::SAME_RIGHTS,
                nullable: false,
            },
        ) else {
            panic!();
        };
        assert_eq!("test.fidlcodec.examples/FidlCodecTestProtocol", &protocol);

        let Value::OutOfLine(PlaygroundValue::InUseHandle(client)) = client else { panic!() };

        assert!(client.take_client("test.fidlcodec.examples/FidlCodecTestProtocol").is_err());

        let Value::OutOfLine(PlaygroundValue::InUseHandle(server)) = server else { panic!() };

        assert!(server.take_server("test.fidlcodec.examples/FidlCodecTestProtocol").is_err());
    }

    #[test]
    fn duplicate_raw_handle() {
        let (socket, _b) = fidl::Socket::create_stream();
        let socket = socket.into_handle();
        let mut socket = Value::Handle(socket, fidl::ObjectType::SOCKET);
        let socket_dup = socket.duplicate();
        let Value::OutOfLine(PlaygroundValue::InUseHandle(socket)) = socket else { panic!() };
        let Value::OutOfLine(PlaygroundValue::InUseHandle(socket_dup)) = socket_dup else {
            panic!()
        };

        assert_eq!(socket.id().unwrap(), socket_dup.id().unwrap());
        assert_eq!(Some(fidl::ObjectType::SOCKET), socket.object_type());
        assert_eq!(Some(fidl::ObjectType::SOCKET), socket_dup.object_type());
    }
}
