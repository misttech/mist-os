// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::ParseWarning;
use crate::fxt_builder::FxtBuilder;
use crate::session::ResolveCtx;
use crate::string::{StringRef, STRING_REF_INLINE_BIT};
use crate::{trace_header, ParseError, ParseResult};
use flyweights::FlyStr;
use nom::number::complete::{le_f64, le_i64, le_u64};

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: FlyStr,
    pub value: ArgValue,
}

impl Arg {
    pub(crate) fn resolve_n(ctx: &mut ResolveCtx, raw: Vec<RawArg<'_>>) -> Vec<Self> {
        raw.into_iter().filter_map(|a| Self::resolve(ctx, a)).collect()
    }

    fn resolve(ctx: &mut ResolveCtx, raw: RawArg<'_>) -> Option<Self> {
        let name = ctx.resolve_str(raw.name);
        if let Some(value) = ArgValue::resolve(ctx, raw.value) {
            Some(Self { name, value })
        } else {
            ctx.add_warning(ParseWarning::SkippingArgWithUnknownType { name });
            None
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct RawArg<'a> {
    pub name: StringRef<'a>,
    pub value: RawArgValue<'a>,
}

impl<'a> RawArg<'a> {
    pub(crate) fn parse_n(count: u8, buf: &'a [u8]) -> ParseResult<'a, Vec<Self>> {
        nom::multi::count(Self::parse, count as usize)(buf)
    }

    pub(crate) fn parse(buf: &'a [u8]) -> ParseResult<'a, Self> {
        use nom::combinator::map;

        let (buf, base_header) = BaseArgHeader::parse(buf)?;
        let (rem, payload) = base_header.take_payload(buf)?;
        let (payload, name) = StringRef::parse(base_header.name_ref(), payload)?;

        let arg_ty = base_header.raw_type();
        let (empty, value) = match arg_ty {
            NULL_ARG_TYPE => Ok((payload, RawArgValue::Null)),
            BOOL_ARG_TYPE => {
                let header = BoolHeader::new(base_header.0).map_err(nom::Err::Failure)?;
                Ok((payload, RawArgValue::Boolean(header.value() != 0)))
            }
            I32_ARG_TYPE => {
                let header = I32Header::new(base_header.0).map_err(nom::Err::Failure)?;
                Ok((payload, RawArgValue::Signed32(header.value())))
            }
            U32_ARG_TYPE => {
                let header = U32Header::new(base_header.0).map_err(nom::Err::Failure)?;
                Ok((payload, RawArgValue::Unsigned32(header.value())))
            }
            I64_ARG_TYPE => map(le_i64, |i| RawArgValue::Signed64(i))(payload),
            U64_ARG_TYPE => map(le_u64, |u| RawArgValue::Unsigned64(u))(payload),
            F64_ARG_TYPE => map(le_f64, |f| RawArgValue::Double(f))(payload),
            STR_ARG_TYPE => {
                let header = StringHeader::new(base_header.0).map_err(nom::Err::Failure)?;
                map(move |b| StringRef::parse(header.value_ref(), b), |s| RawArgValue::String(s))(
                    payload,
                )
            }
            PTR_ARG_TYPE => map(le_u64, |p| RawArgValue::Pointer(p))(payload),
            KOBJ_ARG_TYPE => map(le_u64, |k| RawArgValue::KernelObj(k))(payload),
            unknown => Ok((&[][..], RawArgValue::Unknown { raw_type: unknown, bytes: payload })),
        }?;

        if empty.is_empty() {
            Ok((rem, Self { name, value }))
        } else {
            Err(nom::Err::Failure(ParseError::InvalidSize))
        }
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>, String> {
        let arg_name_ref = match self.name {
            StringRef::Index(id) => id.into(),
            StringRef::Inline(name) => name.len() as u16 | STRING_REF_INLINE_BIT,
            StringRef::Empty => {
                return Err("Argument is missing a name.".to_string());
            }
        };

        match &self.value {
            RawArgValue::Null => {
                let mut header = NullHeader::empty();
                header.set_name_ref(arg_name_ref);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.build())
            }
            RawArgValue::Boolean(val) => {
                let mut header = BoolHeader::empty();
                header.set_name_ref(arg_name_ref);
                header.set_value(*val as u8);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.build())
            }
            RawArgValue::Signed32(val) => {
                let mut header = I32Header::empty();
                header.set_name_ref(arg_name_ref);
                header.set_value(*val);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.build())
            }
            RawArgValue::Unsigned32(val) => {
                let mut header = U32Header::empty();
                header.set_name_ref(arg_name_ref);
                header.set_value(*val);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.build())
            }
            RawArgValue::Signed64(val) => {
                let mut header = I64Header::empty();
                header.set_name_ref(arg_name_ref);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.atom(val.to_le_bytes()).build())
            }
            RawArgValue::Unsigned64(val) => {
                let mut header = U64Header::empty();
                header.set_name_ref(arg_name_ref);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.atom(val.to_le_bytes()).build())
            }
            RawArgValue::Double(val) => {
                let mut header = F64Header::empty();
                header.set_name_ref(arg_name_ref);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.atom(val.to_le_bytes()).build())
            }
            RawArgValue::String(str_val) => {
                let mut header = StringHeader::empty();
                header.set_name_ref(arg_name_ref);
                header.set_value_ref(match str_val {
                    StringRef::Index(id) => (*id).into(),
                    StringRef::Inline(val) => val.len() as u16 | STRING_REF_INLINE_BIT,
                    StringRef::Empty => 0u16,
                });
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                if let StringRef::Inline(value_str) = str_val {
                    builder = builder.atom(value_str);
                }
                Ok(builder.build())
            }
            RawArgValue::Pointer(val) => {
                let mut header = PtrHeader::empty();
                header.set_name_ref(arg_name_ref);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.atom(val.to_le_bytes()).build())
            }
            RawArgValue::KernelObj(val) => {
                let mut header = KobjHeader::empty();
                header.set_name_ref(arg_name_ref);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.atom(val.to_le_bytes()).build())
            }
            RawArgValue::Unknown { raw_type, bytes } => {
                let mut header = BaseArgHeader::empty();
                header.set_raw_type(*raw_type);
                let mut builder = FxtBuilder::new(header);
                if let StringRef::Inline(name_str) = self.name {
                    builder = builder.atom(name_str);
                }
                Ok(builder.atom(bytes).build())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Null,
    Boolean(bool),
    Signed32(i32),
    Unsigned32(u32),
    Signed64(i64),
    Unsigned64(u64),
    Double(f64),
    String(FlyStr),
    Pointer(u64),
    KernelObj(u64),
}

impl ArgValue {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn signed_32(&self) -> Option<i32> {
        match self {
            Self::Signed32(n) => Some(*n),
            _ => None,
        }
    }

    pub fn unsigned_32(&self) -> Option<u32> {
        match self {
            Self::Unsigned32(n) => Some(*n),
            _ => None,
        }
    }

    pub fn signed_64(&self) -> Option<i64> {
        match self {
            Self::Signed64(n) => Some(*n),
            _ => None,
        }
    }

    pub fn unsigned_64(&self) -> Option<u64> {
        match self {
            Self::Unsigned64(n) => Some(*n),
            _ => None,
        }
    }

    pub fn double(&self) -> Option<f64> {
        match self {
            Self::Double(n) => Some(*n),
            _ => None,
        }
    }

    pub fn string(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn pointer(&self) -> Option<u64> {
        match self {
            Self::Pointer(p) => Some(*p),
            _ => None,
        }
    }

    pub fn kernel_obj(&self) -> Option<u64> {
        match self {
            Self::KernelObj(k) => Some(*k),
            _ => None,
        }
    }

    fn resolve(ctx: &mut ResolveCtx, raw: RawArgValue<'_>) -> Option<Self> {
        Some(match raw {
            RawArgValue::Null => ArgValue::Null,
            RawArgValue::Boolean(b) => ArgValue::Boolean(b),
            RawArgValue::Signed32(s) => ArgValue::Signed32(s),
            RawArgValue::Unsigned32(u) => ArgValue::Unsigned32(u),
            RawArgValue::Signed64(s) => ArgValue::Signed64(s),
            RawArgValue::Unsigned64(u) => ArgValue::Unsigned64(u),
            RawArgValue::Double(f) => ArgValue::Double(f),
            RawArgValue::String(s) => ArgValue::String(ctx.resolve_str(s)),
            RawArgValue::Pointer(p) => ArgValue::Pointer(p),
            RawArgValue::KernelObj(k) => ArgValue::KernelObj(k),
            RawArgValue::Unknown { .. } => {
                return None;
            }
        })
    }
}

impl std::fmt::Display for ArgValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgValue::Null => write!(f, "null"),
            ArgValue::Boolean(v) => write!(f, "{v}"),
            ArgValue::Signed32(v) => write!(f, "{v}"),
            ArgValue::Unsigned32(v) => write!(f, "{v}"),
            ArgValue::Signed64(v) => write!(f, "{v}"),
            ArgValue::Unsigned64(v) => write!(f, "{v}"),
            ArgValue::Double(v) => write!(f, "{v}"),
            ArgValue::String(v) => write!(f, "{v}"),
            ArgValue::Pointer(v) => write!(f, "{v:#016x}"),
            ArgValue::KernelObj(v) => write!(f, "{v}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RawArgValue<'a> {
    Null,
    Boolean(bool),
    Signed32(i32),
    Unsigned32(u32),
    Signed64(i64),
    Unsigned64(u64),
    Double(f64),
    String(StringRef<'a>),
    Pointer(u64),
    KernelObj(u64),
    Unknown { raw_type: u8, bytes: &'a [u8] },
}

macro_rules! arg_header {
    ($name:ident $(($arg_ty:expr))? { $($record_specific:tt)* }) => {
        trace_header! {
            $name $(($arg_ty))? {
                $($record_specific)*
                u16, name_ref: 16, 31;
            }
        }
    };
}

pub(crate) const NULL_ARG_TYPE: u8 = 0;
pub(crate) const I32_ARG_TYPE: u8 = 1;
pub(crate) const U32_ARG_TYPE: u8 = 2;
pub(crate) const I64_ARG_TYPE: u8 = 3;
pub(crate) const U64_ARG_TYPE: u8 = 4;
pub(crate) const F64_ARG_TYPE: u8 = 5;
pub(crate) const STR_ARG_TYPE: u8 = 6;
pub(crate) const PTR_ARG_TYPE: u8 = 7;
pub(crate) const KOBJ_ARG_TYPE: u8 = 8;
pub(crate) const BOOL_ARG_TYPE: u8 = 9;

// Used to probe the arg type.
arg_header! {
    BaseArgHeader {}
}

arg_header! {
    NullHeader (NULL_ARG_TYPE) {}
}

arg_header! {
    I32Header (I32_ARG_TYPE) {
        i32, value: 32, 63;
    }
}

arg_header! {
    U32Header (U32_ARG_TYPE) {
        u32, value: 32, 63;
    }
}

arg_header! {
    I64Header (I64_ARG_TYPE) {}
}

arg_header! {
    U64Header (U64_ARG_TYPE) {}
}

arg_header! {
    F64Header (F64_ARG_TYPE) {}
}

arg_header! {
    StringHeader (STR_ARG_TYPE) {
        u16, value_ref: 32, 47;
    }
}

arg_header! {
    PtrHeader (PTR_ARG_TYPE) {}
}

arg_header! {
    KobjHeader (KOBJ_ARG_TYPE) {}
}

arg_header! {
    BoolHeader (BOOL_ARG_TYPE) {
        u8, value: 32, 32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;

    #[test]
    fn null_arg_name_index() {
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(10);

        let arg_record_bytes = FxtBuilder::new(header).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Null,
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }
    #[test]
    fn null_arg_name_inline() {
        let name = "hello";
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).build();
        let raw_arg_record = RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Null };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn i32_arg_name_index() {
        let mut header = I32Header::empty();
        header.set_name_ref(10);
        header.set_value(-19);

        let arg_record_bytes = FxtBuilder::new(header).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Signed32(-19),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn i32_arg_name_inline() {
        let name = "hello";
        let mut header = I32Header::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_value(-19);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Signed32(-19) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn u32_arg_name_index() {
        let mut header = U32Header::empty();
        header.set_name_ref(10);
        header.set_value(23);

        let arg_record_bytes = FxtBuilder::new(header).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Unsigned32(23),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn u32_arg_name_inline() {
        let name = "hello";
        let mut header = U32Header::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_value(23);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Unsigned32(23) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn i64_arg_name_index() {
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(10);
        header.set_raw_type(I64_ARG_TYPE);

        let arg_record_bytes = FxtBuilder::new(header).atom((-79i64).to_le_bytes()).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Signed64(-79),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn i64_arg_name_inline() {
        let name = "hello";
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_raw_type(I64_ARG_TYPE);

        let arg_record_bytes =
            FxtBuilder::new(header).atom(name).atom((-845i64).to_le_bytes()).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Signed64(-845) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn u64_arg_name_index() {
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(10);
        header.set_raw_type(U64_ARG_TYPE);

        let arg_record_bytes = FxtBuilder::new(header).atom(1024u64.to_le_bytes()).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Unsigned64(1024),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn u64_arg_name_inline() {
        let name = "hello";
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_raw_type(U64_ARG_TYPE);

        let arg_record_bytes =
            FxtBuilder::new(header).atom(name).atom(4096u64.to_le_bytes()).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Unsigned64(4096) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn f64_arg_name_index() {
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(10);
        header.set_raw_type(F64_ARG_TYPE);

        let arg_record_bytes = FxtBuilder::new(header).atom(1007.893f64.to_le_bytes()).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Double(1007.893),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn f64_arg_name_inline() {
        let name = "hello";
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_raw_type(F64_ARG_TYPE);

        let arg_record_bytes =
            FxtBuilder::new(header).atom(name).atom(23634.1231f64.to_le_bytes()).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Double(23634.1231) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn string_arg_name_index_value_index() {
        let mut header = StringHeader::empty();
        header.set_name_ref(10);
        header.set_value_ref(11);

        let arg_record_bytes = FxtBuilder::new(header).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::String(StringRef::Index(NonZeroU16::new(11).unwrap())),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn string_arg_name_index_value_inline() {
        let value = "123-456-7890";
        let mut header = StringHeader::empty();
        header.set_name_ref(10);
        header.set_value_ref(value.len() as u16 | STRING_REF_INLINE_BIT);

        let arg_record_bytes = FxtBuilder::new(header).atom(value).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::String(StringRef::Inline(value)),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn string_arg_name_inline_value_index() {
        let name = "hello";
        let mut header = StringHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_value_ref(13);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).build();
        let raw_arg_record = RawArg {
            name: StringRef::Inline(name),
            value: RawArgValue::String(StringRef::Index(NonZeroU16::new(13).unwrap())),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn string_arg_name_inline_value_inline() {
        let name = "hello";
        let value = "123-456-7890";
        let mut header = StringHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_value_ref(value.len() as u16 | STRING_REF_INLINE_BIT);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).atom(value).build();
        let raw_arg_record = RawArg {
            name: StringRef::Inline(name),
            value: RawArgValue::String(StringRef::Inline(value)),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn pointer_arg_name_index() {
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(10);
        header.set_raw_type(PTR_ARG_TYPE);

        let arg_record_bytes = FxtBuilder::new(header).atom(256u64.to_le_bytes()).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Pointer(256),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn pointer_arg_name_inline() {
        let name = "hello";
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_raw_type(PTR_ARG_TYPE);

        let arg_record_bytes =
            FxtBuilder::new(header).atom(name).atom(512u64.to_le_bytes()).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Pointer(512) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn koid_arg_name_index() {
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(10);
        header.set_raw_type(KOBJ_ARG_TYPE);

        let arg_record_bytes = FxtBuilder::new(header).atom(17u64.to_le_bytes()).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::KernelObj(17),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn koid_arg_name_inline() {
        let name = "hello";
        let mut header = BaseArgHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_raw_type(KOBJ_ARG_TYPE);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).atom(21u64.to_le_bytes()).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::KernelObj(21) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn bool_arg_name_index() {
        let mut header = BoolHeader::empty();
        header.set_name_ref(10);
        header.set_value(true as u8);

        let arg_record_bytes = FxtBuilder::new(header).build();
        let raw_arg_record = RawArg {
            name: StringRef::Index(NonZeroU16::new(10).unwrap()),
            value: RawArgValue::Boolean(true),
        };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }

    #[test]
    fn bool_arg_name_inline() {
        let name = "hello";
        let mut header = BoolHeader::empty();
        header.set_name_ref(name.len() as u16 | STRING_REF_INLINE_BIT);
        header.set_value(true as u8);

        let arg_record_bytes = FxtBuilder::new(header).atom(name).build();
        let raw_arg_record =
            RawArg { name: StringRef::Inline("hello"), value: RawArgValue::Boolean(true) };

        assert_parses_to_arg!(arg_record_bytes, raw_arg_record);
        assert_eq!(raw_arg_record.serialize().unwrap(), arg_record_bytes);
    }
}
