// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

//! This crate provides an implementation of Fuchsia Diagnostic Streams, often referred to as
//! "logs."

#![warn(missing_docs)]

use bitfield::bitfield;
use std::borrow::{Borrow, Cow};
use tracing::{Level, Metadata};

pub use fidl_fuchsia_diagnostics::Severity;

pub mod encode;
pub mod parse;

/// A raw severity.
pub type RawSeverity = u8;

/// A log record.
#[derive(Debug, PartialEq)]
pub struct Record<'a> {
    /// Time at which the log was emitted.
    pub timestamp: zx::BootInstant,
    /// The severity of the log.
    pub severity: RawSeverity,
    /// Arguments associated with the log.
    pub arguments: Vec<Argument<'a>>,
}

impl Record<'_> {
    /// Consumes the current value and returns one in the static lifetime.
    pub fn to_owned(self) -> Record<'static> {
        Record {
            timestamp: self.timestamp,
            severity: self.severity,
            arguments: self.arguments.into_iter().map(|arg| arg.to_owned()).collect(),
        }
    }
}

/// An argument of the log record identified by a name and with an associated value.
#[derive(Clone, Debug, PartialEq)]
pub enum Argument<'a> {
    /// Process ID
    Pid(zx::Koid),
    /// Thread ID
    Tid(zx::Koid),
    /// A log tag
    Tag(StringRef<'a>),
    /// Number of dropped logs
    Dropped(u64),
    /// A filename
    File(StringRef<'a>),
    /// A log message
    Message(StringRef<'a>),
    /// A line number in a file
    Line(u64),
    /// A custom argument with a given name and value
    Other {
        /// The name of the argument.
        name: StringRef<'a>,
        /// The value of the argument.
        value: Value<'a>,
    },
}

const PID: &'static str = "pid";
const TID: &'static str = "tid";
const TAG: &'static str = "tag";
const NUM_DROPPED: &'static str = "num_dropped";
const MESSAGE: &'static str = "message";
const FILE: &'static str = "file";
const LINE: &'static str = "line";

impl<'a> Argument<'a> {
    /// Creates a new argument given its name and a value.
    pub fn new(name: impl Into<StringRef<'a>>, value: impl Into<Value<'a>>) -> Self {
        let name: StringRef<'a> = name.into();
        match (name.as_str(), value.into()) {
            (PID, Value::UnsignedInt(pid)) => Self::pid(zx::Koid::from_raw(pid)),
            (TID, Value::UnsignedInt(pid)) => Self::tid(zx::Koid::from_raw(pid)),
            (TAG, Value::Text(tag)) => Self::tag(tag),
            (NUM_DROPPED, Value::UnsignedInt(dropped)) => Self::dropped(dropped),
            (FILE, Value::Text(file)) => Self::file(file),
            (LINE, Value::UnsignedInt(line)) => Self::line(line),
            (MESSAGE, Value::Text(msg)) => Self::message(msg),
            (_, value) => Self::other(name, value),
        }
    }

    #[inline]
    /// Creates a new argument for a process id.
    pub fn pid(koid: zx::Koid) -> Self {
        Argument::Pid(koid)
    }

    #[inline]
    /// Creates a new argument for a thread id.
    pub fn tid(koid: zx::Koid) -> Self {
        Argument::Tid(koid)
    }

    #[inline]
    /// Creates a new argument for a log message.
    pub fn message(message: impl Into<StringRef<'a>>) -> Self {
        Argument::Message(message.into())
    }

    #[inline]
    /// Creates a new argument for a tag.
    pub fn tag(value: impl Into<StringRef<'a>>) -> Self {
        Argument::Tag(value.into())
    }

    #[inline]
    /// Creates a new argument for the number of dropped logs.
    pub fn dropped(value: u64) -> Self {
        Argument::Dropped(value)
    }

    #[inline]
    /// Creates a new argument for a file.
    pub fn file(value: impl Into<StringRef<'a>>) -> Self {
        Argument::File(value.into())
    }

    #[inline]
    /// Creates a new argument for a line number.
    pub fn line(value: u64) -> Self {
        Argument::Line(value)
    }

    // We keep this private for the places where we know we don't need to interpret as a known
    // field.
    #[inline]
    pub(crate) fn other(name: impl Into<StringRef<'a>>, value: impl Into<Value<'a>>) -> Self {
        Argument::Other { name: name.into(), value: value.into() }
    }

    /// Consumes the current value and returns one in the static lifetime.
    pub fn to_owned(self) -> Argument<'static> {
        match self {
            Self::Pid(pid) => Argument::Pid(pid),
            Self::Tid(tid) => Argument::Tid(tid),
            Self::Tag(tag) => Argument::Tag(tag.to_owned()),
            Self::Dropped(dropped) => Argument::Dropped(dropped),
            Self::File(file) => Argument::File(file.to_owned()),
            Self::Line(line) => Argument::Line(line),
            Self::Message(msg) => Argument::Message(msg.to_owned()),
            Self::Other { name, value } => {
                Argument::Other { name: name.to_owned(), value: value.to_owned() }
            }
        }
    }

    /// Returns the name of the argument.
    pub fn name(&self) -> &str {
        match self {
            Self::Pid(_) => PID,
            Self::Tid(_) => TID,
            Self::Tag(_) => TAG,
            Self::Dropped(_) => NUM_DROPPED,
            Self::File(_) => FILE,
            Self::Line(_) => LINE,
            Self::Message(_) => MESSAGE,
            Self::Other { name: StringRef::Empty, .. } => "",
            Self::Other { name: StringRef::Inline(name), .. } => name.as_ref(),
        }
    }

    /// Returns the value of the argument.
    pub fn value(&'a self) -> Value<'a> {
        match self {
            Self::Pid(pid) => Value::UnsignedInt(pid.raw_koid()),
            Self::Tid(tid) => Value::UnsignedInt(tid.raw_koid()),
            Self::Tag(tag) => Value::Text(tag.clone_borrowed()),
            Self::Dropped(num_dropped) => Value::UnsignedInt(*num_dropped),
            Self::File(file) => Value::Text(file.clone_borrowed()),
            Self::Message(msg) => Value::Text(msg.clone_borrowed()),
            Self::Line(line) => Value::UnsignedInt(*line),
            Self::Other { value, .. } => value.clone_borrowed(),
        }
    }
}

/// The value of a logging argument.
#[derive(Clone, Debug, PartialEq)]
pub enum Value<'a> {
    /// A signed integer value for a logging argument.
    SignedInt(i64),
    /// An unsigned integer value for a logging argument.
    UnsignedInt(u64),
    /// A floating point value for a logging argument.
    Floating(f64),
    /// A boolean value for a logging argument.
    Boolean(bool),
    /// A string value for a logging argument.
    Text(StringRef<'a>),
}

impl<'a> Value<'a> {
    fn to_owned(self) -> Value<'static> {
        match self {
            Self::Text(s) => Value::Text(s.to_owned()),
            Self::SignedInt(n) => Value::SignedInt(n),
            Self::UnsignedInt(n) => Value::UnsignedInt(n),
            Self::Floating(n) => Value::Floating(n),
            Self::Boolean(n) => Value::Boolean(n),
        }
    }

    fn clone_borrowed(&'a self) -> Value<'a> {
        match self {
            Self::Text(s) => Self::Text(s.clone_borrowed()),
            Self::SignedInt(n) => Self::SignedInt(*n),
            Self::UnsignedInt(n) => Self::UnsignedInt(*n),
            Self::Floating(n) => Self::Floating(*n),
            Self::Boolean(n) => Self::Boolean(*n),
        }
    }
}

impl From<i32> for Value<'_> {
    fn from(number: i32) -> Value<'static> {
        Value::SignedInt(number as i64)
    }
}

impl From<i64> for Value<'_> {
    fn from(number: i64) -> Value<'static> {
        Value::SignedInt(number)
    }
}

impl From<u64> for Value<'_> {
    fn from(number: u64) -> Value<'static> {
        Value::UnsignedInt(number)
    }
}

impl From<u32> for Value<'_> {
    fn from(number: u32) -> Value<'static> {
        Value::UnsignedInt(number as u64)
    }
}

impl From<zx::Koid> for Value<'_> {
    fn from(koid: zx::Koid) -> Value<'static> {
        Value::UnsignedInt(koid.raw_koid())
    }
}

impl From<f64> for Value<'_> {
    fn from(number: f64) -> Value<'static> {
        Value::Floating(number)
    }
}

impl<'a, T> From<T> for Value<'a>
where
    T: Into<StringRef<'a>>,
{
    fn from(text: T) -> Value<'a> {
        Value::Text(text.into())
    }
}

impl From<bool> for Value<'static> {
    fn from(boolean: bool) -> Value<'static> {
        Value::Boolean(boolean)
    }
}

/// The tracing format supports many types of records, we're sneaking in as a log message.
const TRACING_FORMAT_LOG_RECORD_TYPE: u8 = 9;

bitfield! {
    /// A header in the tracing format. Expected to precede every Record and Argument.
    ///
    /// The tracing format specifies [Record headers] and [Argument headers] as distinct types, but
    /// their layouts are the same in practice, so we represent both bitfields using the same
    /// struct.
    ///
    /// [Record headers]: https://fuchsia.dev/fuchsia-src/development/tracing/trace-format#record_header
    /// [Argument headers]: https://fuchsia.dev/fuchsia-src/development/tracing/trace-format#argument_header
    pub struct Header(u64);
    impl Debug;

    /// Record type.
    u8, raw_type, set_type: 3, 0;

    /// Record size as a multiple of 8 bytes.
    u16, size_words, set_size_words: 15, 4;

    /// String ref for the associated name, if any.
    u16, name_ref, set_name_ref: 31, 16;

    /// Boolean value, if any.
    bool, bool_val, set_bool_val: 32;

    /// Reserved for record-type-specific data.
    u16, value_ref, set_value_ref: 47, 32;

    /// Severity of the record, if any.
    u8, severity, set_severity: 63, 56;
}

impl Header {
    /// Sets the length of the item the header refers to. Panics if not 8-byte aligned.
    fn set_len(&mut self, new_len: usize) {
        assert_eq!(new_len % 8, 0, "encoded message must be 8-byte aligned");
        self.set_size_words((new_len / 8) as u16 + u16::from(new_len % 8 > 0))
    }
}

/// Tag derived from metadata.
///
/// Unlike tags, metatags are not represented as strings and instead must be resolved from event
/// metadata. This means that they may resolve to different text for different events.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Metatag {
    /// The location of a span or event.
    ///
    /// The target is typically a module path, but this can be configured by a particular span or
    /// event when it is constructed.
    Target,
}

/// These literal values are specified by the tracing format:
///
/// https://fuchsia.dev/fuchsia-src/development/tracing/trace-format#argument_header
#[repr(u8)]
enum ArgType {
    Null = 0,
    I32 = 1,
    U32 = 2,
    I64 = 3,
    U64 = 4,
    F64 = 5,
    String = 6,
    Pointer = 7,
    Koid = 8,
    Bool = 9,
}

impl TryFrom<u8> for ArgType {
    type Error = parse::ParseError;
    fn try_from(b: u8) -> Result<Self, Self::Error> {
        Ok(match b {
            0 => ArgType::Null,
            1 => ArgType::I32,
            2 => ArgType::U32,
            3 => ArgType::I64,
            4 => ArgType::U64,
            5 => ArgType::F64,
            6 => ArgType::String,
            7 => ArgType::Pointer,
            8 => ArgType::Koid,
            9 => ArgType::Bool,
            _ => return Err(parse::ParseError::ValueOutOfValidRange),
        })
    }
}

/// A reference to a string, which may be owned or borrowed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StringRef<'a> {
    /// An empty string.
    Empty,
    /// A string.
    Inline(Cow<'a, str>),
}

impl<'a> StringRef<'a> {
    fn mask(&self) -> u16 {
        match self {
            StringRef::Empty => 0,
            StringRef::Inline(s) => (s.len() as u16) | (1 << 15),
        }
    }

    fn to_owned(self) -> StringRef<'static> {
        match self {
            Self::Empty => StringRef::Empty,
            Self::Inline(Cow::Owned(s)) => StringRef::Inline(Cow::Owned(s)),
            Self::Inline(Cow::Borrowed(s)) => StringRef::Inline(Cow::Owned(s.to_owned())),
        }
    }

    fn clone_borrowed(&'a self) -> Self {
        match self {
            Self::Empty => Self::Empty,
            Self::Inline(Cow::Owned(s)) => Self::Inline(Cow::Borrowed(s.as_ref())),
            Self::Inline(Cow::Borrowed(s)) => Self::Inline(Cow::Borrowed(s)),
        }
    }

    /// Returns a reference to a underlying string or an empty static string when empty.
    pub fn as_str(&'a self) -> &'a str {
        match self {
            StringRef::Empty => "",
            StringRef::Inline(s) => s.borrow(),
        }
    }
}

impl<'a> From<String> for StringRef<'a> {
    fn from(string: String) -> StringRef<'a> {
        match string.len() {
            0 => StringRef::Empty,
            _ => StringRef::Inline(Cow::Owned(string)),
        }
    }
}

impl<'a> From<&'a str> for StringRef<'a> {
    fn from(string: &'a str) -> StringRef<'a> {
        match string.len() {
            0 => StringRef::Empty,
            _ => StringRef::Inline(Cow::Borrowed(string)),
        }
    }
}

impl std::fmt::Display for StringRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let StringRef::Inline(s) = self {
            write!(f, "{s}")?;
        }
        Ok(())
    }
}

/// A type which has a `Severity`.
pub trait SeverityExt {
    /// Return the severity of this value.
    fn severity(&self) -> Severity;

    /// Return the raw severity of this value.
    fn raw_severity(&self) -> RawSeverity;
}

impl SeverityExt for Metadata<'_> {
    fn severity(&self) -> Severity {
        match *self.level() {
            Level::ERROR => Severity::Error,
            Level::WARN => Severity::Warn,
            Level::INFO => Severity::Info,
            Level::DEBUG => Severity::Debug,
            Level::TRACE => Severity::Trace,
        }
    }

    fn raw_severity(&self) -> RawSeverity {
        match *self.level() {
            Level::ERROR => Severity::Error.into_primitive(),
            Level::WARN => Severity::Warn.into_primitive(),
            Level::INFO => Severity::Info.into_primitive(),
            Level::DEBUG => Severity::Debug.into_primitive(),
            Level::TRACE => Severity::Trace.into_primitive(),
        }
    }
}

impl SeverityExt for log::Level {
    fn severity(&self) -> Severity {
        match self {
            log::Level::Error => Severity::Error,
            log::Level::Warn => Severity::Warn,
            log::Level::Info => Severity::Info,
            log::Level::Debug => Severity::Debug,
            log::Level::Trace => Severity::Trace,
        }
    }

    fn raw_severity(&self) -> RawSeverity {
        self.severity().into_primitive()
    }
}

/// A type which can be created from a `Severity` value.
pub trait FromSeverity {
    /// Creates `Self` from `severity`.
    fn from_severity(severity: &Severity) -> Self;
}

impl FromSeverity for log::LevelFilter {
    fn from_severity(severity: &Severity) -> Self {
        match severity {
            Severity::Error => log::LevelFilter::Error,
            Severity::Warn => log::LevelFilter::Warn,
            Severity::Info => log::LevelFilter::Info,
            Severity::Debug => log::LevelFilter::Debug,
            Severity::Trace => log::LevelFilter::Trace,
            // NB: Not a clean mapping.
            Severity::Fatal => log::LevelFilter::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{Encoder, EncoderOpts, EncodingError, MutableBuffer};
    use crate::parse::try_parse_record;
    use std::fmt::Debug;
    use std::io::Cursor;

    fn parse_argument<'a>(bytes: &'a [u8]) -> (&'a [u8], Argument<'static>) {
        let (remaining, decoded_from_full) =
            nom::error::dbg_dmp(&crate::parse::parse_argument, "roundtrip")(bytes).unwrap();
        (remaining, decoded_from_full.to_owned())
    }

    fn parse_record<'a>(bytes: &'a [u8]) -> (&'a [u8], Record<'static>) {
        let (remaining, decoded_from_full) =
            nom::error::dbg_dmp(&crate::parse::try_parse_record, "roundtrip")(bytes).unwrap();
        (remaining, decoded_from_full.to_owned())
    }

    const BUF_LEN: usize = 1024;

    pub(crate) fn assert_roundtrips<T, F>(
        val: T,
        encoder_method: impl Fn(&mut Encoder<Cursor<Vec<u8>>>, &T) -> Result<(), EncodingError>,
        parser: F,
        canonical: Option<&[u8]>,
    ) where
        T: Debug + PartialEq,
        F: Fn(&[u8]) -> (&[u8], T),
    {
        let mut encoder = Encoder::new(Cursor::new(vec![0; BUF_LEN]), EncoderOpts::default());
        encoder_method(&mut encoder, &val).unwrap();

        // next we'll parse the record out of a buf with padding after the record
        let (_, decoded_from_full) = parser(encoder.buf.get_ref());
        assert_eq!(val, decoded_from_full, "decoded version with trailing padding must match");

        if let Some(canonical) = canonical {
            let recorded = encoder.buf.get_ref().split_at(canonical.len()).0;
            assert_eq!(canonical, recorded, "encoded repr must match the canonical value provided");

            let (zero_buf, decoded) = parser(recorded);
            assert_eq!(val, decoded, "decoded version must match what we tried to encode");
            assert_eq!(zero_buf.len(), 0, "must parse record exactly out of provided buffer");
        }
    }

    /// Bit pattern for the log record type, severity info, and a record of two words: one header,
    /// one timestamp.
    const MINIMAL_LOG_HEADER: u64 = 0x3000000000000029;

    #[fuchsia::test]
    fn minimal_header() {
        let mut poked = Header(0);
        poked.set_type(TRACING_FORMAT_LOG_RECORD_TYPE);
        poked.set_size_words(2);
        poked.set_severity(Severity::Info.into_primitive());

        assert_eq!(
            poked.0, MINIMAL_LOG_HEADER,
            "minimal log header should only describe type, size, and severity"
        );
    }

    #[fuchsia::test]
    fn no_args_roundtrip() {
        let mut expected_record = MINIMAL_LOG_HEADER.to_le_bytes().to_vec();
        let timestamp = zx::BootInstant::from_nanos(5_000_000i64);
        expected_record.extend(timestamp.into_nanos().to_le_bytes());

        assert_roundtrips(
            Record { timestamp, severity: Severity::Info.into_primitive(), arguments: vec![] },
            Encoder::write_record,
            parse_record,
            Some(&expected_record),
        );
    }

    #[fuchsia::test]
    fn signed_arg_roundtrip() {
        assert_roundtrips(
            Argument::other("signed", -1999),
            |encoder, val| encoder.write_argument(val),
            parse_argument,
            None,
        );
    }

    #[fuchsia::test]
    fn unsigned_arg_roundtrip() {
        assert_roundtrips(
            Argument::other("unsigned", 42),
            |encoder, val| encoder.write_argument(val),
            parse_argument,
            None,
        );
    }

    #[fuchsia::test]
    fn text_arg_roundtrip() {
        assert_roundtrips(
            Argument::other("stringarg", "owo"),
            |encoder, val| encoder.write_argument(val),
            parse_argument,
            None,
        );
    }

    #[fuchsia::test]
    fn float_arg_roundtrip() {
        assert_roundtrips(
            Argument::other("float", 3.25),
            |encoder, val| encoder.write_argument(val),
            parse_argument,
            None,
        );
    }

    #[fuchsia::test]
    fn bool_arg_roundtrip() {
        assert_roundtrips(
            Argument::other("bool", false),
            |encoder, val| encoder.write_argument(val),
            parse_argument,
            None,
        );
    }

    #[fuchsia::test]
    fn arg_of_each_type_roundtrips() {
        assert_roundtrips(
            Record {
                timestamp: zx::BootInstant::get(),
                severity: Severity::Warn.into_primitive(),
                arguments: vec![
                    Argument::other("signed", -10),
                    Argument::other("unsigned", 7),
                    Argument::other("float", 3.25),
                    Argument::other("bool", true),
                    Argument::other("msg", "test message one"),
                ],
            },
            Encoder::write_record,
            parse_record,
            None,
        );
    }

    #[fuchsia::test]
    fn multiple_string_args() {
        assert_roundtrips(
            Record {
                timestamp: zx::BootInstant::get(),
                severity: Severity::Trace.into_primitive(),
                arguments: vec![
                    Argument::other("msg", "test message one"),
                    Argument::other("msg", "test message two"),
                    Argument::other("msg", "test message three"),
                ],
            },
            Encoder::write_record,
            parse_record,
            None,
        );
    }

    #[fuchsia::test]
    fn invalid_records() {
        // invalid word size
        let mut encoder = Encoder::new(Cursor::new(vec![0; BUF_LEN]), EncoderOpts::default());
        let mut header = Header(0);
        header.set_type(TRACING_FORMAT_LOG_RECORD_TYPE);
        header.set_size_words(0); // invalid, should be at least 2 as header and time are included
        encoder.buf.put_u64_le(header.0).unwrap();
        encoder.buf.put_i64_le(zx::BootInstant::get().into_nanos()).unwrap();
        encoder.write_argument(Argument::other("msg", "test message one")).unwrap();
        assert!(try_parse_record(encoder.buf.get_ref()).is_err());
    }
}
