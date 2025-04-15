// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::MessageError;
use crate::MonikerWithUrl;
use bumpalo::collections::{String as BumpaloString, Vec as BumpaloVec};
use bumpalo::Bump;
use diagnostics_data::{ExtendedMoniker, Severity};
use diagnostics_log_encoding::{Argument, Record, Value};
use flyweights::FlyStr;
use std::str;
use zx::BootInstant;

pub use crate::constants::*;

struct ArchivistArguments<'a> {
    builder: CPPLogMessageBuilder<'a>,
    archivist_argument_count: usize,
    record: Record<'a>,
}

#[cfg(fuchsia_api_level_less_than = "HEAD")]
fn parse_archivist_args<'a>(
    builder: CPPLogMessageBuilder<'a>,
    input: Record<'a>,
) -> Result<ArchivistArguments<'a>, MessageError> {
    Ok(ArchivistArguments { builder, archivist_argument_count: 0, record: input })
}

#[cfg(fuchsia_api_level_at_least = "HEAD")]
fn parse_archivist_args<'a>(
    mut builder: CPPLogMessageBuilder<'a>,
    input: Record<'a>,
) -> Result<ArchivistArguments<'a>, MessageError> {
    let mut has_moniker = false;
    let mut archivist_argument_count = 0;
    for argument in input.arguments.iter().rev() {
        // If Archivist records are expected, they should always be at the end.
        // If no Archivist records are expected, treat them as regular key-value-pairs.
        match argument {
            Argument::Other { value, name } => {
                if name == fidl_fuchsia_diagnostics::MONIKER_ARG_NAME {
                    if let Value::Text(moniker) = value {
                        builder = builder.set_moniker(ExtendedMoniker::parse_str(moniker)?);
                        archivist_argument_count += 1;
                        has_moniker = true;
                        continue;
                    }
                }
                if name == fidl_fuchsia_diagnostics::COMPONENT_URL_ARG_NAME {
                    if let Value::Text(_) = value {
                        archivist_argument_count += 1;
                        continue;
                    }
                }
                if name == fidl_fuchsia_diagnostics::ROLLED_OUT_ARG_NAME {
                    if let Value::UnsignedInt(_) = value {
                        archivist_argument_count += 1;
                        continue;
                    }
                }
                break;
            }
            _ => break,
        }
    }
    if !has_moniker {
        return Err(MessageError::MissingMoniker);
    }
    Ok(ArchivistArguments { builder, archivist_argument_count, record: input })
}

/// Array for FFI purposes between C++ and Rust.
/// If len is 0, ptr is allowed to be nullptr,
/// otherwise, ptr must be valid.
#[repr(C)]
pub struct CPPArray<T> {
    /// Number of elements in the array
    pub len: usize,
    /// Pointer to the first element in the array,
    /// may be null in the case of a 0 length array,
    /// but is not guaranteed to always be null of
    /// len is 0.
    pub ptr: *const T,
}

impl CPPArray<u8> {
    /// # Safety
    ///
    /// input must refer to a valid string, sized according to len.
    /// A valid string consists of UTF-8 characters. The caller
    /// is responsible for ensuring the byte sequence consists of valid UTF-8
    /// characters.
    ///
    pub unsafe fn as_utf8_str(&self) -> &str {
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len))
    }
}

impl From<&str> for CPPArray<u8> {
    fn from(value: &str) -> Self {
        CPPArray { len: value.len(), ptr: value.as_ptr() }
    }
}

impl From<Option<&str>> for CPPArray<u8> {
    fn from(value: Option<&str>) -> Self {
        if let Some(value) = value {
            CPPArray { len: value.len(), ptr: value.as_ptr() }
        } else {
            CPPArray { len: 0, ptr: std::ptr::null() }
        }
    }
}

impl<T> From<&Vec<T>> for CPPArray<T> {
    fn from(value: &Vec<T>) -> Self {
        CPPArray { len: value.len(), ptr: value.as_ptr() }
    }
}

/// Log message representation for FFI with C++
#[repr(C)]
pub struct LogMessage<'a> {
    /// Severity of a log message.
    severity: u8,
    /// Tags in a log message, guaranteed to be non-null.
    tags: CPPArray<CPPArray<u8>>,
    /// Process ID from a LogMessage, or 0 if unknown
    pid: u64,
    /// Thread ID from a LogMessage, or 0 if unknown
    tid: u64,
    /// Number of dropped log messages.
    dropped: u64,
    /// The UTF-encoded log message, guaranteed to be valid UTF-8.
    message: CPPArray<u8>,
    /// Timestamp on the boot timeline of the log message,
    /// in nanoseconds.
    timestamp: i64,
    /// Pointer to the builder is owned by this CPPLogMessage.
    /// Dropping this CPPLogMessage will free the builder.
    builder: *mut CPPLogMessageBuilder<'a>,
}

impl Drop for LogMessage<'_> {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: Rust guarantees destructors only run once in sound code.
            // Initializing the CPPLogMessage requires the builder to be set
            // to a valid pointer, so it is safe to drop the CPPLogMessageBuilder
            // in the destructor to free resources on the Rust side of the FFI boundary.
            std::ptr::drop_in_place(self.builder);
        }
    }
}

pub struct CPPLogMessageBuilder<'a> {
    severity: u8,
    tags: BumpaloVec<'a, BumpaloString<'a>>,
    pid: Option<u64>,
    tid: Option<u64>,
    dropped: u64,
    file: Option<String>,
    line: Option<u64>,
    moniker: Option<BumpaloString<'a>>,
    message: Option<String>,
    timestamp: i64,
    kvps: BumpaloVec<'a, Argument<'a>>,
    allocator: &'a Bump,
}

// Escape quotes in a string per the Feedback format
fn escape_quotes(input: &str, output: &mut BumpaloString<'_>) {
    for ch in input.chars() {
        if ch == '\"' {
            output.push('\\');
        }
        output.push(ch);
    }
}

impl<'a> CPPLogMessageBuilder<'a> {
    fn convert_string_vec(&self, strings: &[BumpaloString<'_>]) -> CPPArray<CPPArray<u8>> {
        CPPArray {
            len: strings.len(),
            ptr: self
                .allocator
                .alloc_slice_fill_iter(strings.iter().map(|value| value.as_str().into()))
                .as_ptr(),
        }
    }

    fn set_raw_severity(mut self, raw_severity: u8) -> Self {
        self.severity = raw_severity;
        self
    }

    fn add_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(BumpaloString::from_str_in(&tag.into(), self.allocator));
        self
    }

    fn set_pid(mut self, pid: u64) -> Self {
        self.pid = Some(pid);
        self
    }

    fn set_tid(mut self, tid: u64) -> Self {
        self.tid = Some(tid);
        self
    }

    fn set_dropped(mut self, dropped: u64) -> Self {
        self.dropped = dropped;
        self
    }

    fn set_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    fn set_line(mut self, line: u64) -> Self {
        self.line = Some(line);
        self
    }

    fn set_message(mut self, msg: impl Into<String>) -> Self {
        self.message = Some(msg.into());
        self
    }

    fn add_kvp(mut self, kvp: Argument<'a>) -> Self {
        self.kvps.push(kvp);
        self
    }
    #[cfg(fuchsia_api_level_at_least = "HEAD")]
    fn set_moniker(mut self, value: ExtendedMoniker) -> Self {
        self.moniker = Some(bumpalo::format!(in self.allocator,"{}", value));
        self
    }
    fn build(self) -> &'a mut LogMessage<'a> {
        let allocator = self.allocator;
        let builder = allocator.alloc(self);

        // Format the message in accordance with the Feedback format
        let msg_str = builder
            .message
            .as_ref()
            .map(|value| bumpalo::format!(in &allocator,"{value}",))
            .unwrap_or_else(|| BumpaloString::new_in(allocator));

        let mut kvp_str = BumpaloString::new_in(allocator);
        for kvp in &builder.kvps {
            kvp_str = bumpalo::format!(in &allocator, "{kvp_str} {}=", kvp.name());
            match kvp.value() {
                Value::Text(value) => {
                    kvp_str.push('"');
                    escape_quotes(&value, &mut kvp_str);
                    kvp_str.push('"');
                }
                Value::SignedInt(value) => {
                    kvp_str.push_str(&bumpalo::format!(in &allocator, "{}",value))
                }
                Value::UnsignedInt(value) => {
                    kvp_str.push_str(&bumpalo::format!(in &allocator, "{}",value))
                }
                Value::Floating(value) => {
                    kvp_str.push_str(&bumpalo::format!(in &allocator, "{}",value))
                }
                Value::Boolean(value) => {
                    if value {
                        kvp_str.push_str("true");
                    } else {
                        kvp_str.push_str("false");
                    }
                }
            }
        }

        let mut output = match (&builder.file, &builder.line) {
            (Some(file), Some(line)) => {
                let mut value = bumpalo::format!(in &allocator, "[{file}({line})]",);
                if !msg_str.is_empty() {
                    value.push(' ');
                }
                value
            }
            _ => BumpaloString::new_in(allocator),
        };

        output.push_str(&msg_str);
        output.push_str(&kvp_str);

        if let Some(moniker) = &builder.moniker {
            let component_name = moniker.split("/").last();
            if let Some(component_name) = component_name {
                if !builder.tags.iter().any(|value| value.as_str() == component_name) {
                    builder.tags.insert(0, bumpalo::format!(in &allocator, "{}", component_name));
                }
            }
        }

        let log_message = LogMessage {
            builder,
            severity: builder.severity,
            dropped: builder.dropped,
            tags: builder.convert_string_vec(&builder.tags),
            pid: builder.pid.unwrap_or(0),
            tid: builder.tid.unwrap_or(0),
            message: output.as_str().into(),
            timestamp: builder.timestamp,
        };

        allocator.alloc(log_message)
    }
}

struct CPPLogMessageBuilderBuilder<'a>(&'a Bump);

impl<'a> CPPLogMessageBuilderBuilder<'a> {
    fn configure(
        self,
        _component_url: Option<FlyStr>,
        moniker: Option<ExtendedMoniker>,
        severity: Severity,
        timestamp: BootInstant,
    ) -> Result<CPPLogMessageBuilder<'a>, MessageError> {
        Ok(CPPLogMessageBuilder {
            severity: severity as u8,
            tags: BumpaloVec::new_in(self.0),
            pid: None,
            tid: None,
            dropped: 0,
            file: None,
            timestamp: timestamp.into_nanos(),
            line: None,
            allocator: self.0,
            kvps: BumpaloVec::new_in(self.0),
            moniker: moniker.map(|value| bumpalo::format!(in self.0,"{}", value)),
            message: None,
        })
    }
}

fn build_logs_data<'a>(
    input: Record<'a>,
    source: Option<MonikerWithUrl>,
    allocator: &'a Bump,
    expect_extended_attribution: bool,
) -> Result<CPPLogMessageBuilder<'a>, MessageError> {
    let builder = CPPLogMessageBuilderBuilder(allocator);
    let (raw_severity, severity) = Severity::parse_exact(input.severity);
    let (maybe_moniker, maybe_url) =
        source.map(|value| (Some(value.moniker), Some(value.url))).unwrap_or((None, None));
    let mut builder = builder.configure(maybe_url, maybe_moniker, severity, input.timestamp)?;
    if let Some(raw_severity) = raw_severity {
        builder = builder.set_raw_severity(raw_severity);
    }
    let (archivist_argument_count, input) = if !expect_extended_attribution {
        (0, input)
    } else {
        let arguments = parse_archivist_args(builder, input)?;
        builder = arguments.builder;
        (arguments.archivist_argument_count, arguments.record)
    };
    let input_argument_len = input.arguments.len();
    for argument in input.arguments.into_iter().take(input_argument_len - archivist_argument_count)
    {
        match argument {
            Argument::Tag(tag) => {
                builder = builder.add_tag(tag.as_ref());
            }
            Argument::Pid(pid) => {
                builder = builder.set_pid(pid.raw_koid());
            }
            Argument::Tid(tid) => {
                builder = builder.set_tid(tid.raw_koid());
            }
            Argument::Dropped(dropped) => {
                builder = builder.set_dropped(dropped);
            }
            Argument::File(file) => {
                builder = builder.set_file(file.as_ref());
            }
            Argument::Line(line) => {
                builder = builder.set_line(line);
            }
            Argument::Message(msg) => {
                builder = builder.set_message(msg.as_ref());
            }
            Argument::Other { value: _, name: _ } => builder = builder.add_kvp(argument),
        }
    }

    Ok(builder)
}

/// Constructs a `CPPLogsMessage` from the provided bytes, assuming the bytes
/// are in the format specified as in the [log encoding], and come from
///
/// an Archivist LogStream with moniker, URL, and dropped logs output enabled.
/// [log encoding] https://fuchsia.dev/fuchsia-src/development/logs/encodings
pub fn ffi_from_extended_record<'a>(
    bytes: &'a [u8],
    allocator: &'a Bump,
    source: Option<MonikerWithUrl>,
    expect_extended_attribution: bool,
) -> Result<(&'a mut LogMessage<'a>, &'a [u8]), MessageError> {
    let (input, remaining) = diagnostics_log_encoding::parse::parse_record(bytes)?;
    let record = build_logs_data(input, source, allocator, expect_extended_attribution)?.build();
    Ok((record, remaining))
}
