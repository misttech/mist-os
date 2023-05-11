// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Encoding diagnostic records using the Fuchsia Tracing format.

use crate::{ArgType, Header, Metatag, SeverityExt, StringRef};
use fidl_fuchsia_diagnostics::Severity;
use fidl_fuchsia_diagnostics_stream as fstream;
use fuchsia_zircon as zx;
use std::{array::TryFromSliceError, borrow::Borrow, fmt::Debug, io::Cursor, ops::Deref};
use thiserror::Error;
use tracing::{Event, Metadata, Subscriber};
use tracing_core::{
    field::{Field, Visit},
    span,
};
use tracing_log::NormalizeEvent;
use tracing_subscriber::{
    layer::Context,
    registry::{LookupSpan, Scope},
};

/// An `Encoder` wraps any value implementing `MutableBuffer` and writes diagnostic stream records
/// into it.
pub struct Encoder<B> {
    pub(crate) buf: B,
    found_error: Option<EncodingError>,
}

/// Parameters for `Encoder/write_event`.
pub struct WriteEventParams<'a, E, T, MS> {
    /// The event to write as a record.
    pub event: E,
    /// Tags associated with the log event.
    pub tags: &'a [T],
    /// Metatags associated with the log event.
    pub metatags: MS,
    /// The process that emitted the log.
    pub pid: zx::Koid,
    /// The thread that emitted the log.
    pub tid: zx::Koid,
    /// Number of events that were dropped before this one.
    pub dropped: u32,
}

impl<B> Encoder<B>
where
    B: MutableBuffer,
{
    /// Create a new `Encoder` from the provided buffer.
    pub fn new(buf: B) -> Self {
        Self { buf, found_error: None }
    }

    /// Returns a reference to the underlying buffer being used for encoding.
    pub fn inner(&self) -> &B {
        &self.buf
    }

    /// Writes a [`tracing::Event`] to the buffer as a record.
    ///
    /// Fails if there is insufficient space in the buffer for encoding.
    pub fn write_event<'a, E, MS, T>(
        &mut self,
        params: WriteEventParams<'a, E, T, MS>,
    ) -> Result<(), EncodingError>
    where
        E: RecordEvent,
        MS: Iterator<Item = &'a Metatag>,
        T: AsRef<str>,
    {
        let WriteEventParams { event, tags, metatags, pid, tid, dropped } = params;
        let severity = event.severity();
        self.write_inner(event.timestamp(), severity, |this| {
            this.write_argument(Argument { name: "pid", value: pid.into() })?;
            this.write_argument(Argument { name: "tid", value: tid.into() })?;
            if dropped > 0 {
                this.write_argument(Argument { name: "num_dropped", value: dropped.into() })?;
            }

            // If the severity is ERROR or higher, we add the file and line information.
            if severity >= Severity::Error {
                if let Some(mut file) = event.file() {
                    let split = file.split("../");
                    file = split.last().unwrap();
                    this.write_argument(Argument { name: "file", value: Value::Text(file) })?;
                }

                if let Some(line) = event.line() {
                    this.write_argument(Argument { name: "line", value: line.into() })?;
                }
            }

            // Write the metatags as tags (if any were given)
            for metatag in metatags {
                match metatag {
                    Metatag::Target => this.write_argument(Argument {
                        name: "tag",
                        value: Value::Text(event.target()),
                    })?,
                }
            }

            event.write_arguments(this)?;

            for tag in tags {
                this.write_argument(Argument { name: "tag", value: Value::Text(tag.as_ref()) })?;
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Writes a Record to the buffer.
    pub fn write_record<R>(&mut self, record: &R) -> Result<(), EncodingError>
    where
        R: RecordFields,
    {
        self.write_inner(record.timestamp(), record.severity(), |this| record.write_arguments(this))
    }

    fn write_inner<F>(
        &mut self,
        timestamp: zx::Time,
        severity: Severity,
        write_args: F,
    ) -> Result<(), EncodingError>
    where
        F: FnOnce(&mut Self) -> Result<(), EncodingError>,
    {
        // TODO(fxbug.dev/59992): on failure, zero out the region we were using
        let starting_idx = self.buf.cursor();
        // Prepare the header, we'll finish writing once we know the full size of the record.
        let header_slot = self.buf.put_slot(std::mem::size_of::<u64>())?;
        self.write_i64(timestamp.into_nanos())?;

        write_args(self)?;

        let mut header = Header(0);
        header.set_type(crate::TRACING_FORMAT_LOG_RECORD_TYPE);
        header.set_severity(severity.into_primitive());

        let length = self.buf.cursor() - starting_idx;
        header.set_len(length);

        assert_eq!(length % 8, 0, "all records must be written 8-byte aligned");
        self.buf.fill_slot(header_slot, &header.0.to_le_bytes());
        Ok(())
    }

    /// Writes an argument with this encoder.
    pub fn write_argument<'a>(
        &mut self,
        argument: impl Borrow<Argument<'a>>,
    ) -> Result<(), EncodingError> {
        let argument = argument.borrow();
        let starting_idx = self.buf.cursor();

        let header_slot = self.buf.put_slot(std::mem::size_of::<Header>())?;

        let mut header = Header(0);

        self.write_string(argument.name)?;
        header.set_name_ref(StringRef::for_str(argument.name).mask());

        match &argument.value {
            Value::SignedInt(s) => {
                header.set_type(ArgType::I64 as u8);
                self.write_i64(*s)
            }
            Value::UnsignedInt(u) => {
                header.set_type(ArgType::U64 as u8);
                self.write_u64(*u)
            }
            Value::Floating(f) => {
                header.set_type(ArgType::F64 as u8);
                self.write_f64(*f)
            }
            Value::Text(t) => {
                header.set_type(ArgType::String as u8);
                header.set_value_ref(StringRef::for_str(t).mask());
                self.write_string(t)
            }
            Value::Boolean(b) => {
                header.set_type(ArgType::Bool as u8);
                header.set_bool_val(*b);
                Ok(())
            }
        }?;

        let record_len = self.buf.cursor() - starting_idx;
        assert_eq!(record_len % 8, 0, "arguments must be 8-byte aligned");

        header.set_size_words((record_len / 8) as u16);
        self.buf.fill_slot(header_slot, &header.0.to_le_bytes());

        Ok(())
    }

    fn maybe_write_argument<'a>(
        &mut self,
        field: &Field,
        value: impl Into<Value<'a>>,
    ) -> Result<(), EncodingError> {
        let name = field.name();
        if !matches!(name, "log.target" | "log.module_path" | "log.file" | "log.line") {
            self.write_argument(Argument { name, value: value.into() })?;
        }
        Ok(())
    }

    /// Write an unsigned integer.
    fn write_u64(&mut self, n: u64) -> Result<(), EncodingError> {
        self.buf.put_u64_le(n).map_err(|_| EncodingError::BufferTooSmall)
    }

    /// Write a signed integer.
    fn write_i64(&mut self, n: i64) -> Result<(), EncodingError> {
        self.buf.put_i64_le(n).map_err(|_| EncodingError::BufferTooSmall)
    }

    /// Write a floating-point number.
    fn write_f64(&mut self, n: f64) -> Result<(), EncodingError> {
        self.buf.put_f64(n).map_err(|_| EncodingError::BufferTooSmall)
    }

    /// Write a string padded to 8-byte alignment.
    fn write_string(&mut self, src: &str) -> Result<(), EncodingError> {
        self.write_bytes(src.as_bytes())
    }

    /// Write bytes padded to 8-byte alignment.
    fn write_bytes(&mut self, src: &[u8]) -> Result<(), EncodingError> {
        self.buf.put_slice(src).map_err(|_| EncodingError::BufferTooSmall)?;
        unsafe {
            let align = std::mem::size_of::<u64>();
            let num_padding_bytes = (align - src.len() % align) % align;
            // TODO(fxbug.dev/59993) need to enforce that the buffer is zeroed
            self.buf.advance_cursor(num_padding_bytes);
        }
        Ok(())
    }
}

/// The attributes of a Span pre-encoded for usage in child events.
pub struct EncodedSpanArguments {
    encoder: Encoder<Cursor<ResizableBuffer>>,
}

impl EncodedSpanArguments {
    /// Encodes the given span attributes.
    pub fn new(attrs: &span::Attributes<'_>) -> Result<Self, EncodingError> {
        let mut encoder = Encoder::new(Cursor::new(ResizableBuffer(Vec::new())));
        attrs.record(&mut encoder);
        if let Some(err) = encoder.found_error {
            return Err(err);
        }
        Ok(Self { encoder })
    }

    fn copy_to<B: MutableBuffer>(&self, encoder: &mut Encoder<B>) -> Result<(), EncodingError> {
        let buffer = self.encoder.inner();
        let end = buffer.cursor().min(buffer.get_ref().0.len());
        encoder.write_bytes(&buffer.get_ref().0[..end])
    }
}

impl<B: MutableBuffer> Visit for Encoder<B> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if let Err(err) = self.maybe_write_argument(field, format!("{value:?}").as_str()) {
            self.found_error = Some(err);
        };
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if let Err(err) = self.maybe_write_argument(field, value) {
            self.found_error = Some(err);
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Err(err) = self.maybe_write_argument(field, value) {
            self.found_error = Some(err);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if let Err(err) = self.maybe_write_argument(field, value) {
            self.found_error = Some(err);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if let Err(err) = self.maybe_write_argument(field, value) {
            self.found_error = Some(err);
        }
    }
}

/// An argument for the logging format.
#[derive(Clone, Debug, PartialEq)]
pub struct Argument<'a> {
    /// The name of the logging argument.
    pub name: &'a str,
    /// The value of the logging argument.
    pub value: Value<'a>,
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
    Text(&'a str),
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

impl<'a> From<&'a str> for Value<'a> {
    fn from(text: &'a str) -> Value<'a> {
        Value::Text(text)
    }
}

impl From<bool> for Value<'static> {
    fn from(boolean: bool) -> Value<'static> {
        Value::Boolean(boolean)
    }
}

/// Trait implemented by types which can be written by the Encoder.
pub trait RecordEvent {
    /// Returns the record severity.
    fn severity(&self) -> Severity;
    /// Returns the name of the file where the record was emitted.
    fn file(&self) -> Option<&str>;
    /// Returns the number of the line in the file where the record was emitted.
    fn line(&self) -> Option<u32>;
    /// Returns the target of the record.
    fn target(&self) -> &str;
    /// Consumes this type and writes all the arguments.
    fn write_arguments<B: MutableBuffer>(
        self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError>;
    /// Returns the timestamp associated to this record.
    fn timestamp(&self) -> zx::Time;
}

/// Trait implemented by complete Records.
pub trait RecordFields {
    /// Returns the record severity.
    fn severity(&self) -> Severity;

    /// Returns the timestamp associated to this record.
    fn timestamp(&self) -> zx::Time;

    /// Consumes this type and writes all the arguments.
    fn write_arguments<B: MutableBuffer>(
        &self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError>;
}

/// An event emitted by `tracing`.
pub struct TracingEvent<'a, S> {
    event: &'a Event<'a>,
    context: Option<Context<'a, S>>,
    metadata: StoredMetadata<'a>,
    timestamp: zx::Time,
}

// Just like Cow, but without requiring the inner type to be Clone.
enum StoredMetadata<'a> {
    Borrowed(&'a Metadata<'a>),
    Owned(Metadata<'a>),
}

impl<'a> Deref for StoredMetadata<'a> {
    type Target = Metadata<'a>;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(meta) => meta,
            Self::Owned(meta) => meta,
        }
    }
}

impl<'a, S> TracingEvent<'a, S> {
    /// Wraps a tracing event with its associated context.
    pub fn new(event: &'a Event<'_>, context: Context<'a, S>) -> TracingEvent<'a, S> {
        Self::inner(event, Some(context))
    }

    // Just for benchmark purposes since we can't construct a Context manually.
    #[doc(hidden)]
    pub fn from_event(event: &'a Event<'_>) -> TracingEvent<'a, S> {
        Self::inner(event, None)
    }

    fn inner(event: &'a Event<'_>, context: Option<Context<'a, S>>) -> TracingEvent<'a, S> {
        // normalizing is needed to get log records to show up in trace metadata correctly
        if let Some(metadata) = event.normalized_metadata() {
            Self {
                event,
                context,
                metadata: StoredMetadata::Owned(metadata),
                timestamp: zx::Time::get_monotonic(),
            }
        } else {
            Self {
                event,
                context,
                metadata: StoredMetadata::Borrowed(event.metadata()),
                timestamp: zx::Time::get_monotonic(),
            }
        }
    }
}

impl<'a, S> RecordEvent for TracingEvent<'a, S>
where
    for<'lookup> S: Subscriber + LookupSpan<'lookup>,
{
    fn severity(&self) -> Severity {
        self.metadata.severity()
    }

    fn file(&self) -> Option<&str> {
        self.metadata.file()
    }

    fn line(&self) -> Option<u32> {
        self.metadata.line()
    }

    fn target(&self) -> &str {
        self.metadata.target()
    }

    fn timestamp(&self) -> zx::Time {
        self.timestamp
    }

    fn write_arguments<B: MutableBuffer>(
        self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError> {
        writer.found_error = None;
        if let Some(context) = self.context {
            let span_iter =
                context.event_scope(self.event).map(Scope::from_root).into_iter().flatten();
            for span in span_iter {
                let extensions = span.extensions();
                if let Some(encoded) = extensions.get::<EncodedSpanArguments>() {
                    encoded.copy_to(writer)?;
                }
            }
        }
        self.event.record(writer);
        if let Some(err) = writer.found_error.take() {
            return Err(err);
        }
        Ok(())
    }
}

/// Arguments to create a record for testing purposes.
pub struct TestRecord<'a> {
    /// Severity of the log
    pub severity: Severity,
    /// Timestamp of the test record.
    pub timestamp: zx::Time,
    /// File that emitted the log.
    pub file: Option<&'a str>,
    /// Line in the file that emitted the log.
    pub line: Option<u32>,
    /// Additional record arguments.
    pub record_arguments: Vec<Argument<'a>>,
}

impl TestRecord<'_> {
    /// Creates a test record from a record.
    pub fn from<'a>(file: &'a str, line: u32, record: &'a fstream::Record) -> TestRecord<'a> {
        TestRecord {
            severity: record.severity,
            timestamp: zx::Time::from_nanos(record.timestamp),
            file: Some(file),
            line: Some(line),
            record_arguments: record.arguments.iter().map(Argument::from).collect(),
        }
    }
}

impl RecordEvent for TestRecord<'_> {
    fn severity(&self) -> Severity {
        self.severity
    }

    fn file(&self) -> Option<&str> {
        self.file
    }

    fn line(&self) -> Option<u32> {
        self.line
    }

    fn target(&self) -> &str {
        unimplemented!("Unused at the moment");
    }

    fn timestamp(&self) -> zx::Time {
        self.timestamp
    }

    fn write_arguments<B: MutableBuffer>(
        self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError> {
        for argument in self.record_arguments {
            writer.write_argument(argument)?;
        }
        Ok(())
    }
}

impl RecordFields for fstream::Record {
    fn severity(&self) -> Severity {
        self.severity
    }

    fn write_arguments<B: MutableBuffer>(
        &self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError> {
        for arg in &self.arguments {
            writer.write_argument(Argument::from(arg))?;
        }
        Ok(())
    }

    fn timestamp(&self) -> zx::Time {
        zx::Time::from_nanos(self.timestamp)
    }
}

impl<'a> PartialEq<fstream::Argument> for Argument<'a> {
    fn eq(&self, other: &fstream::Argument) -> bool {
        let arg = Argument::from(other);
        *self == arg
    }
}

impl<'a> From<&'a fstream::Argument> for Argument<'a> {
    fn from(arg: &'a fstream::Argument) -> Argument<'a> {
        let value = match &arg.value {
            fstream::Value::SignedInt(value) => Value::SignedInt(*value),
            fstream::Value::UnsignedInt(value) => Value::UnsignedInt(*value),
            fstream::Value::Floating(value) => Value::Floating(*value),
            fstream::Value::Text(value) => Value::Text(value.as_str()),
            fstream::Value::Boolean(value) => Value::Boolean(*value),
            _ => unreachable!("we should have covered all values"),
        };
        Argument { name: arg.name.as_str(), value }
    }
}

/// Analogous to `bytes::BufMut` with some additions to be able to write at specific offsets.
pub trait MutableBuffer {
    /// Returns the number of total bytes this container can store. Shared memory buffers are not
    /// expected to resize and this should return the same value during the entire lifetime of the
    /// buffer.
    fn capacity(&self) -> usize;

    /// Returns the current position into which the next write is expected.
    fn cursor(&self) -> usize;

    /// Advance the write cursor by `n` bytes.
    ///
    /// # Safety
    ///
    /// This is marked unsafe because a malformed caller may
    /// cause a subsequent out-of-bounds write.
    unsafe fn advance_cursor(&mut self, n: usize);

    /// Write a copy of the `src` slice into the buffer, starting at the provided offset.
    ///
    /// # Safety
    ///
    /// Implementations are not expected to bounds check the requested copy, although they may do
    /// so and still satisfy this trait's contract.
    unsafe fn put_slice_at(&mut self, src: &[u8], offset: usize);

    /// Returns whether the buffer has sufficient remaining capacity to write an incoming value.
    fn has_remaining(&self, num_bytes: usize) -> bool;

    /// Advances the write cursor without immediately writing any bytes to the buffer. The returned
    /// struct offers the ability to later write to the provided portion of the buffer.
    fn put_slot(&mut self, width: usize) -> Result<WriteSlot, EncodingError> {
        if self.has_remaining(width) {
            let slot = WriteSlot { range: self.cursor()..(self.cursor() + width) };
            unsafe {
                self.advance_cursor(width);
            }
            Ok(slot)
        } else {
            Err(EncodingError::BufferTooSmall)
        }
    }

    /// Write `src` into the provided slot that was created at a previous point in the stream.
    fn fill_slot(&mut self, slot: WriteSlot, src: &[u8]) {
        assert_eq!(
            src.len(),
            slot.range.end - slot.range.start,
            "WriteSlots can only insert exactly-sized content into the buffer"
        );
        unsafe {
            self.put_slice_at(src, slot.range.start);
        }
    }

    /// Writes the contents of the `src` buffer to `self`, starting at `self.cursor()` and
    /// advancing the cursor by `src.len()`.
    ///
    /// # Panics
    ///
    /// This function panics if there is not enough remaining capacity in `self`.
    fn put_slice(&mut self, src: &[u8]) -> Result<(), EncodingError> {
        if self.has_remaining(src.len()) {
            unsafe {
                self.put_slice_at(src, self.cursor());
                self.advance_cursor(src.len());
            }
            Ok(())
        } else {
            Err(EncodingError::NoCapacity)
        }
    }

    /// Writes an unsigned 64 bit integer to `self` in little-endian byte order.
    ///
    /// Advances the cursor by 8 bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::BufMut;
    ///
    /// let mut buf = vec![0; 8];
    /// buf.put_u64_le_at(0x0102030405060708, 0);
    /// assert_eq!(buf, b"\x08\x07\x06\x05\x04\x03\x02\x01");
    /// ```
    ///
    /// # Panics
    ///
    /// This function panics if there is not enough remaining capacity in `self`.
    fn put_u64_le(&mut self, n: u64) -> Result<(), EncodingError> {
        self.put_slice(&n.to_le_bytes())
    }

    /// Writes a signed 64 bit integer to `self` in little-endian byte order.
    ///
    /// The cursor position is advanced by 8.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::BufMut;
    ///
    /// let mut buf = vec![0; 8];
    /// buf.put_i64_le_at(0x0102030405060708, 0);
    /// assert_eq!(buf, b"\x08\x07\x06\x05\x04\x03\x02\x01");
    /// ```
    ///
    /// # Panics
    ///
    /// This function panics if there is not enough remaining capacity in `self`.
    fn put_i64_le(&mut self, n: i64) -> Result<(), EncodingError> {
        self.put_slice(&n.to_le_bytes())
    }

    /// Writes a double-precision IEEE 754 floating point number to `self`.
    ///
    /// The cursor position is advanced by 8.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::BufMut;
    ///
    /// let mut buf = vec![];
    /// buf.put_i64_le(0x0102030405060708);
    /// assert_eq!(buf, b"\x08\x07\x06\x05\x04\x03\x02\x01");
    /// ```
    ///
    /// # Panics
    ///
    /// This function panics if there is not enough remaining capacity in `self`.
    fn put_f64(&mut self, n: f64) -> Result<(), EncodingError> {
        self.put_slice(&n.to_bits().to_ne_bytes())
    }
}

/// A region of the buffer which was advanced past and can later be filled in.
#[must_use]
pub struct WriteSlot {
    range: std::ops::Range<usize>,
}

#[derive(Debug)]
struct ResizableBuffer(Vec<u8>);

impl MutableBuffer for Cursor<ResizableBuffer> {
    fn capacity(&self) -> usize {
        self.get_ref().0.len()
    }

    fn cursor(&self) -> usize {
        self.position() as usize
    }

    fn has_remaining(&self, _num_bytes: usize) -> bool {
        true
    }

    unsafe fn advance_cursor(&mut self, n: usize) {
        self.set_position(self.position() + n as u64);
    }

    unsafe fn put_slice_at(&mut self, to_put: &[u8], offset: usize) {
        let this = &mut self.get_mut().0;
        if offset < this.len() {
            let available = this.len() - offset;

            // Copy the elements that fit into the buffer.
            let min = available.min(to_put.len());
            let dest = &mut this[offset..(offset + min)];
            dest.copy_from_slice(&to_put[..min]);

            // If we couldn't fit all elements, then extend the buffer with the remaining elements.
            if available < to_put.len() {
                this.extend_from_slice(&to_put[available..]);
            }
        } else {
            // If the offset is bigger than the length, fill with zeros up to the offset and then
            // write the slice.
            this.resize(offset, 0);
            this.extend_from_slice(to_put);
        }
    }
}

impl<'a, T: MutableBuffer + ?Sized> MutableBuffer for &'a mut T {
    fn has_remaining(&self, num_bytes: usize) -> bool {
        (**self).has_remaining(num_bytes)
    }
    fn capacity(&self) -> usize {
        (**self).capacity()
    }

    fn cursor(&self) -> usize {
        (**self).cursor()
    }

    unsafe fn advance_cursor(&mut self, n: usize) {
        (**self).advance_cursor(n);
    }

    unsafe fn put_slice_at(&mut self, to_put: &[u8], offset: usize) {
        (**self).put_slice_at(to_put, offset);
    }
}

impl<T: MutableBuffer + ?Sized> MutableBuffer for Box<T> {
    fn has_remaining(&self, num_bytes: usize) -> bool {
        (**self).has_remaining(num_bytes)
    }
    fn capacity(&self) -> usize {
        (**self).capacity()
    }

    fn cursor(&self) -> usize {
        (**self).cursor()
    }

    unsafe fn advance_cursor(&mut self, n: usize) {
        (**self).advance_cursor(n);
    }

    unsafe fn put_slice_at(&mut self, to_put: &[u8], offset: usize) {
        (**self).put_slice_at(to_put, offset);
    }
}

impl MutableBuffer for Cursor<Vec<u8>> {
    fn has_remaining(&self, num_bytes: usize) -> bool {
        (self.cursor() + num_bytes) <= self.capacity()
    }

    fn capacity(&self) -> usize {
        self.get_ref().len()
    }

    fn cursor(&self) -> usize {
        self.position() as usize
    }

    unsafe fn advance_cursor(&mut self, n: usize) {
        self.set_position(self.position() + n as u64);
    }

    unsafe fn put_slice_at(&mut self, to_put: &[u8], offset: usize) {
        let dest = &mut self.get_mut()[offset..(offset + to_put.len())];
        dest.copy_from_slice(to_put);
    }
}

impl MutableBuffer for Cursor<&mut [u8]> {
    fn has_remaining(&self, num_bytes: usize) -> bool {
        (self.cursor() + num_bytes) <= self.capacity()
    }

    fn capacity(&self) -> usize {
        self.get_ref().len()
    }

    fn cursor(&self) -> usize {
        self.position() as usize
    }

    unsafe fn advance_cursor(&mut self, n: usize) {
        self.set_position(self.position() + n as u64);
    }

    unsafe fn put_slice_at(&mut self, to_put: &[u8], offset: usize) {
        let dest = &mut self.get_mut()[offset..(offset + to_put.len())];
        dest.copy_from_slice(to_put);
    }
}

impl<const N: usize> MutableBuffer for Cursor<[u8; N]> {
    fn has_remaining(&self, num_bytes: usize) -> bool {
        (self.cursor() + num_bytes) <= self.capacity()
    }
    fn capacity(&self) -> usize {
        self.get_ref().len()
    }

    fn cursor(&self) -> usize {
        self.position() as usize
    }

    unsafe fn advance_cursor(&mut self, n: usize) {
        self.set_position(self.position() + n as u64);
    }

    unsafe fn put_slice_at(&mut self, to_put: &[u8], offset: usize) {
        let dest = &mut self.get_mut()[offset..(offset + to_put.len())];
        dest.copy_from_slice(to_put);
    }
}

/// An error that occurred while encoding data to the stream format.
#[derive(Debug, Error)]
pub enum EncodingError {
    /// The provided buffer is too small.
    #[error("buffer is too small")]
    BufferTooSmall,

    /// We attempted to encode values which are not yet supported by this implementation of
    /// the Fuchsia Tracing format.
    #[error("unsupported value type")]
    Unsupported,

    /// We attempted to write to a buffer with no remaining capacity.
    #[error("the buffer has no remaining capacity")]
    NoCapacity,
}

impl From<TryFromSliceError> for EncodingError {
    fn from(_: TryFromSliceError) -> Self {
        EncodingError::BufferTooSmall
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_record;
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    use tracing::{info_span, Subscriber};
    use tracing_subscriber::{
        layer::{Context, Layer, SubscriberExt},
        Registry,
    };

    #[fuchsia::test]
    fn build_basic_record() {
        let mut encoder = Encoder::new(Cursor::new([0u8; 1024]));
        encoder
            .write_event(WriteEventParams::<_, &str, _> {
                event: TestRecord {
                    severity: Severity::Info,
                    timestamp: zx::Time::from_nanos(12345),
                    file: None,
                    line: None,
                    record_arguments: vec![],
                },
                tags: &[],
                metatags: std::iter::empty(),
                pid: zx::Koid::from_raw(0),
                tid: zx::Koid::from_raw(0),
                dropped: 0,
            })
            .expect("wrote event");
        let (record, _) = parse_record(encoder.inner().get_ref()).expect("wrote valid record");
        assert_eq!(
            record,
            fstream::Record {
                timestamp: 12345,
                severity: Severity::Info,
                arguments: vec![
                    fstream::Argument {
                        name: "pid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    }
                ]
            }
        );
    }

    #[fuchsia::test]
    fn build_records_with_location() {
        let mut encoder = Encoder::new(Cursor::new([0u8; 1024]));
        encoder
            .write_event(WriteEventParams::<_, &str, _> {
                event: TestRecord {
                    severity: Severity::Error,
                    timestamp: zx::Time::from_nanos(12345),
                    file: Some("foo.rs"),
                    line: Some(10),
                    record_arguments: vec![],
                },
                tags: &[],
                metatags: std::iter::empty(),
                pid: zx::Koid::from_raw(0),
                tid: zx::Koid::from_raw(0),
                dropped: 0,
            })
            .expect("wrote event");
        let (record, _) = parse_record(encoder.inner().get_ref()).expect("wrote valid record");
        assert_eq!(
            record,
            fstream::Record {
                timestamp: 12345,
                severity: Severity::Error,
                arguments: vec![
                    fstream::Argument {
                        name: "pid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "file".to_string(),
                        value: fstream::Value::Text("foo.rs".into())
                    },
                    fstream::Argument {
                        name: "line".to_string(),
                        value: fstream::Value::UnsignedInt(10)
                    },
                ]
            }
        );
    }

    #[fuchsia::test]
    fn build_record_with_dropped_count() {
        let mut encoder = Encoder::new(Cursor::new([0u8; 1024]));
        encoder
            .write_event(WriteEventParams::<_, &str, _> {
                event: TestRecord {
                    severity: Severity::Warn,
                    timestamp: zx::Time::from_nanos(12345),
                    file: None,
                    line: None,
                    record_arguments: vec![],
                },
                tags: &[],
                metatags: std::iter::empty(),
                pid: zx::Koid::from_raw(0),
                tid: zx::Koid::from_raw(0),
                dropped: 7,
            })
            .expect("wrote event");
        let (record, _) = parse_record(encoder.inner().get_ref()).expect("wrote valid record");
        assert_eq!(
            record,
            fstream::Record {
                timestamp: 12345,
                severity: Severity::Warn,
                arguments: vec![
                    fstream::Argument {
                        name: "pid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "num_dropped".to_string(),
                        value: fstream::Value::UnsignedInt(7)
                    },
                ]
            }
        );
    }

    #[derive(Debug)]
    struct PrintMe(u32);

    #[allow(clippy::type_complexity)]
    static LAST_RECORD: Lazy<Mutex<Option<Encoder<Cursor<[u8; 1024]>>>>> =
        Lazy::new(|| Mutex::new(None));

    struct EncoderLayer;
    impl<S> Layer<S> for EncoderLayer
    where
        for<'lookup> S: Subscriber + LookupSpan<'lookup>,
    {
        fn on_event(&self, event: &Event<'_>, cx: Context<'_, S>) {
            let mut encoder = Encoder::new(Cursor::new([0u8; 1024]));
            encoder
                .write_event(WriteEventParams {
                    event: TracingEvent::new(event, cx),
                    tags: &["a-tag"],
                    metatags: [Metatag::Target].iter(),
                    pid: zx::Koid::from_raw(0),
                    tid: zx::Koid::from_raw(0),
                    dropped: 0,
                })
                .expect("wrote event");
            *LAST_RECORD.lock().unwrap() = Some(encoder);
        }

        fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
            let span = ctx.span(id).expect("Span not found, this is a bug");
            let mut extensions = span.extensions_mut();
            if extensions.get_mut::<EncodedSpanArguments>().is_none() {
                let encoded = EncodedSpanArguments::new(attrs).expect("encoded");
                extensions.insert(encoded);
            }
        }
    }

    #[test]
    fn build_record_from_tracing_event() {
        let before_timestamp = zx::Time::get_monotonic().into_nanos();
        let _s = tracing::subscriber::set_default(Registry::default().with(EncoderLayer));
        tracing::info!(
            is_a_str = "hahaha",
            is_debug = ?PrintMe(5),
            is_signed = -500,
            is_unsigned = 1000u64,
            is_bool = false,
            "blarg this is a message"
        );

        let guard = LAST_RECORD.lock().unwrap();
        let encoder = guard.as_ref().unwrap();
        let (record, _) = parse_record(encoder.inner().get_ref()).expect("wrote valid record");
        assert!(record.timestamp > before_timestamp);
        assert_eq!(
            record,
            fstream::Record {
                timestamp: record.timestamp,
                severity: Severity::Info,
                arguments: vec![
                    fstream::Argument {
                        name: "pid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tag".to_string(),
                        value: fstream::Value::Text(
                            "diagnostics_log_encoding_lib_test::encode::tests".into()
                        ),
                    },
                    fstream::Argument {
                        name: "message".to_string(),
                        value: fstream::Value::Text("blarg this is a message".into())
                    },
                    fstream::Argument {
                        name: "is_a_str".to_string(),
                        value: fstream::Value::Text("hahaha".into())
                    },
                    fstream::Argument {
                        name: "is_debug".to_string(),
                        value: fstream::Value::Text("PrintMe(5)".into())
                    },
                    fstream::Argument {
                        name: "is_signed".to_string(),
                        value: fstream::Value::SignedInt(-500)
                    },
                    fstream::Argument {
                        name: "is_unsigned".to_string(),
                        value: fstream::Value::UnsignedInt(1000)
                    },
                    fstream::Argument {
                        name: "is_bool".to_string(),
                        value: fstream::Value::Boolean(false)
                    },
                    fstream::Argument {
                        name: "tag".to_string(),
                        value: fstream::Value::Text("a-tag".into(),)
                    },
                ]
            }
        );
    }

    #[test]
    fn spans_are_supported() {
        let before_timestamp = zx::Time::get_monotonic().into_nanos();
        let _s = tracing::subscriber::set_default(Registry::default().with(EncoderLayer));
        let span = info_span!("my span", tag = "span_tag", span_field = 42);
        span.in_scope(|| {
            let nested_span =
                info_span!("my other span", tag = "nested_span_tag", nested_span_field = "hello");
            nested_span.in_scope(|| {
                tracing::info!(is_bool = true, "a log in spans");
            });
        });

        let guard = LAST_RECORD.lock().unwrap();
        let encoder = guard.as_ref().unwrap();
        let (record, _) = parse_record(encoder.inner().get_ref()).expect("wrote valid record");
        assert!(record.timestamp > before_timestamp);
        assert_eq!(
            record,
            fstream::Record {
                timestamp: record.timestamp,
                severity: Severity::Info,
                arguments: vec![
                    fstream::Argument {
                        name: "pid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tid".to_string(),
                        value: fstream::Value::UnsignedInt(0)
                    },
                    fstream::Argument {
                        name: "tag".to_string(),
                        value: fstream::Value::Text(
                            "diagnostics_log_encoding_lib_test::encode::tests".into()
                        ),
                    },
                    fstream::Argument {
                        name: "tag".to_string(),
                        value: fstream::Value::Text("span_tag".into(),),
                    },
                    fstream::Argument {
                        name: "span_field".to_string(),
                        value: fstream::Value::SignedInt(42),
                    },
                    fstream::Argument {
                        name: "tag".to_string(),
                        value: fstream::Value::Text("nested_span_tag".into(),),
                    },
                    fstream::Argument {
                        name: "nested_span_field".to_string(),
                        value: fstream::Value::Text("hello".into()),
                    },
                    fstream::Argument {
                        name: "message".to_string(),
                        value: fstream::Value::Text("a log in spans".into())
                    },
                    fstream::Argument {
                        name: "is_bool".to_string(),
                        value: fstream::Value::Boolean(true)
                    },
                    fstream::Argument {
                        name: "tag".to_string(),
                        value: fstream::Value::Text("a-tag".into(),)
                    },
                ]
            }
        );
    }

    #[test]
    fn resizable_vec_mutable_buffer() {
        // Putting a slice at offset=len is equivalent to concatenating.
        let mut vec = Cursor::new(ResizableBuffer(vec![1u8, 2, 3]));
        unsafe {
            vec.put_slice_at(&[4, 5, 6], 3);
        }
        assert_eq!(vec.get_ref().0, vec![1, 2, 3, 4, 5, 6]);

        // Putting a slice at an offset inside the buffer, is equivalent to replacing the items
        // there.
        let mut vec = Cursor::new(ResizableBuffer(vec![1, 3, 7, 9, 11, 13, 15]));
        unsafe {
            vec.put_slice_at(&[2, 4, 6], 2);
        }
        assert_eq!(vec.get_ref().0, vec![1, 3, 2, 4, 6, 13, 15]);

        // Putting a slice at an index in range replaces all the items and extends if needed.
        let mut vec = Cursor::new(ResizableBuffer(vec![1, 2, 3]));
        unsafe {
            vec.put_slice_at(&[4, 5, 6, 7], 0);
        }
        assert_eq!(vec.get_ref().0, vec![4, 5, 6, 7]);

        // Putting a slice at an offset beyond the buffer, fills with zeros the items in between.
        let mut vec = Cursor::new(ResizableBuffer(vec![1, 2, 3]));
        unsafe {
            vec.put_slice_at(&[4, 5, 6], 5);
        }
        assert_eq!(vec.get_ref().0, vec![1, 2, 3, 0, 0, 4, 5, 6]);
    }
}
