// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::MessageError;
use byteorder::{ByteOrder, LittleEndian};
use diagnostics_data::{
    BuilderArgs, ExtendedMoniker, LogsData, LogsDataBuilder, LogsField, LogsProperty, Severity,
};
use diagnostics_log_encoding::{Argument, Record, Value};
use flyweights::FlyStr;
use libc::{c_char, c_int};
use moniker::Moniker;
use std::{mem, str};
mod constants;
pub mod error;
pub use constants::*;

#[cfg(test)]
mod test;

#[derive(Clone)]
pub struct MonikerWithUrl {
    pub moniker: ExtendedMoniker,
    pub url: FlyStr,
}

/// Transforms the given legacy log message (already parsed) into a `LogsData` containing the
/// given identity information.
pub fn from_logger(source: MonikerWithUrl, msg: LoggerMessage) -> LogsData {
    let (raw_severity, severity) = Severity::parse_exact(msg.raw_severity);
    let mut builder = LogsDataBuilder::new(BuilderArgs {
        timestamp: msg.timestamp,
        component_url: Some(source.url),
        moniker: source.moniker,
        severity,
    })
    .set_pid(msg.pid)
    .set_tid(msg.tid)
    .set_dropped(msg.dropped_logs)
    .set_message(msg.message);
    if let Some(raw_severity) = raw_severity {
        builder = builder.set_raw_severity(raw_severity);
    }
    for tag in &msg.tags {
        builder = builder.add_tag(tag.as_ref());
    }
    builder.build()
}

#[cfg(fuchsia_api_level_at_least = "HEAD")]
fn parse_archivist_args<'a>(
    mut builder: LogsDataBuilder,
    input: &'a Record<'a>,
) -> Result<(LogsDataBuilder, usize), MessageError> {
    let mut has_component_url = false;
    let mut has_moniker = false;
    let mut archivist_argument_count = 0;
    for argument in input.arguments.iter().rev() {
        // If Archivist records are expected, they should always be at the end.
        // If no Archivist records are expected, treat them as regular key-value-pairs.
        match argument {
            Argument::Other { value, name } => {
                if name == fidl_fuchsia_diagnostics::COMPONENT_URL_ARG_NAME {
                    if let Value::Text(url) = value {
                        builder = builder.set_url(Some(FlyStr::new(url.clone())));
                        archivist_argument_count += 1;
                        has_component_url = true;
                        continue;
                    }
                }
                if name == fidl_fuchsia_diagnostics::MONIKER_ARG_NAME {
                    if let Value::Text(moniker) = value {
                        builder = builder.set_moniker(ExtendedMoniker::parse_str(moniker)?);
                        archivist_argument_count += 1;
                        has_moniker = true;
                        continue;
                    }
                }
                if name == fidl_fuchsia_diagnostics::ROLLED_OUT_ARG_NAME {
                    if let Value::UnsignedInt(ival) = value {
                        builder = builder.set_rolled_out(*ival);
                        archivist_argument_count += 1;
                        continue;
                    }
                }
                break;
            }
            _ => break,
        }
    }
    if !has_component_url {
        return Err(MessageError::MissingUrl);
    }
    if !has_moniker {
        return Err(MessageError::MissingMoniker);
    }
    Ok((builder, archivist_argument_count))
}

#[cfg(fuchsia_api_level_less_than = "HEAD")]
fn parse_archivist_args<'a>(
    builder: LogsDataBuilder,
    _input: &'a Record<'a>,
) -> Result<(LogsDataBuilder, usize), MessageError> {
    Ok((builder, 0))
}

fn parse_logs_data<'a>(
    input: &'a Record<'a>,
    source: Option<MonikerWithUrl>,
) -> Result<LogsData, MessageError> {
    let (raw_severity, severity) = Severity::parse_exact(input.severity);
    let has_attribution = source.is_some();
    let (maybe_moniker, maybe_url) =
        source.map(|value| (Some(value.moniker), Some(value.url))).unwrap_or((None, None));
    let mut builder = LogsDataBuilder::new(BuilderArgs {
        component_url: maybe_url,
        moniker: maybe_moniker.unwrap_or(ExtendedMoniker::ComponentInstance(
            Moniker::parse_str("placeholder").unwrap(),
        )),
        severity,
        timestamp: input.timestamp,
    });
    if let Some(raw_severity) = raw_severity {
        builder = builder.set_raw_severity(raw_severity);
    }
    let archivist_argument_count = if has_attribution {
        0
    } else {
        let (new_builder, count) = parse_archivist_args(builder, input)?;
        builder = new_builder;
        count
    };

    for argument in input.arguments.iter().take(input.arguments.len() - archivist_argument_count) {
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
                builder = builder.set_dropped(*dropped);
            }
            Argument::File(file) => {
                builder = builder.set_file(file.as_ref());
            }
            Argument::Line(line) => {
                builder = builder.set_line(*line);
            }
            Argument::Message(msg) => {
                builder = builder.set_message(msg.as_ref());
            }
            Argument::Other { value, name } => {
                let name = LogsField::Other(name.to_string());
                builder = builder.add_key(match value {
                    Value::SignedInt(v) => LogsProperty::Int(name, *v),
                    Value::UnsignedInt(v) => LogsProperty::Uint(name, *v),
                    Value::Floating(v) => LogsProperty::Double(name, *v),
                    Value::Text(v) => LogsProperty::String(name, v.to_string()),
                    Value::Boolean(v) => LogsProperty::Bool(name, *v),
                })
            }
        }
    }

    Ok(builder.build())
}

/// Constructs a `LogsData` from the provided bytes, assuming the bytes
/// are in the format specified as in the [log encoding], and come from
///
/// an Archivist LogStream with moniker, URL, and dropped logs output enabled.
/// [log encoding] https://fuchsia.dev/fuchsia-src/development/logs/encodings
pub fn from_extended_record(bytes: &[u8]) -> Result<(LogsData, &[u8]), MessageError> {
    let (input, remaining) = diagnostics_log_encoding::parse::parse_record(bytes)?;
    let record = parse_logs_data(&input, None)?;
    Ok((record, remaining))
}

/// Constructs a `LogsData` from the provided bytes, assuming the bytes
/// are in the format specified as in the [log encoding].
///
/// [log encoding] https://fuchsia.dev/fuchsia-src/development/logs/encodings
pub fn from_structured(source: MonikerWithUrl, bytes: &[u8]) -> Result<LogsData, MessageError> {
    let (input, _remaining) = diagnostics_log_encoding::parse::parse_record(bytes)?;
    let record = parse_logs_data(&input, Some(source))?;
    Ok(record)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoggerMessage {
    pub timestamp: zx::BootInstant,
    pub raw_severity: u8,
    pub pid: u64,
    pub tid: u64,
    pub size_bytes: usize,
    pub dropped_logs: u64,
    pub message: Box<str>,
    pub tags: Vec<Box<str>>,
}

/// Parse the provided buffer as if it implements the [logger/syslog wire format].
///
/// Note that this is distinct from the parsing we perform for the debuglog log, which also
/// takes a `&[u8]` and is why we don't implement this as `TryFrom`.
///
/// [logger/syslog wire format]: https://fuchsia.googlesource.com/fuchsia/+/HEAD/zircon/system/ulib/syslog/include/lib/syslog/wire_format.h
impl TryFrom<&[u8]> for LoggerMessage {
    type Error = MessageError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() < MIN_PACKET_SIZE {
            return Err(MessageError::ShortRead { len: bytes.len() });
        }

        let terminator = bytes[bytes.len() - 1];
        if terminator != 0 {
            return Err(MessageError::NotNullTerminated { terminator });
        }

        let pid = LittleEndian::read_u64(&bytes[..8]);
        let tid = LittleEndian::read_u64(&bytes[8..16]);
        let timestamp = zx::BootInstant::from_nanos(LittleEndian::read_i64(&bytes[16..24]));

        let raw_severity = LittleEndian::read_i32(&bytes[24..28]);
        let raw_severity = if raw_severity > (u8::MAX as i32) {
            u8::MAX
        } else if raw_severity < 0 {
            0
        } else {
            u8::try_from(raw_severity).unwrap()
        };
        let dropped_logs = LittleEndian::read_u32(&bytes[28..METADATA_SIZE]) as u64;

        // start reading tags after the header
        let mut cursor = METADATA_SIZE;
        let mut tag_len = bytes[cursor] as usize;
        let mut tags = Vec::new();
        while tag_len != 0 {
            if tags.len() == MAX_TAGS {
                return Err(MessageError::TooManyTags);
            }

            if tag_len > MAX_TAG_LEN - 1 {
                return Err(MessageError::TagTooLong { index: tags.len(), len: tag_len });
            }

            if (cursor + tag_len + 1) > bytes.len() {
                return Err(MessageError::OutOfBounds);
            }

            let tag_start = cursor + 1;
            let tag_end = tag_start + tag_len;
            let tag = String::from_utf8_lossy(&bytes[tag_start..tag_end]);
            tags.push(tag.into());

            cursor = tag_end;
            tag_len = bytes[cursor] as usize;
        }

        let msg_start = cursor + 1;
        let mut msg_end = cursor + 1;
        while msg_end < bytes.len() {
            if bytes[msg_end] > 0 {
                msg_end += 1;
                continue;
            }
            let message = String::from_utf8_lossy(&bytes[msg_start..msg_end]).into_owned();
            let message_len = message.len();
            let result = LoggerMessage {
                timestamp,
                raw_severity,
                message: message.into_boxed_str(),
                pid,
                tid,
                dropped_logs,
                tags,
                size_bytes: cursor + message_len + 1,
            };
            return Ok(result);
        }

        Err(MessageError::OutOfBounds)
    }
}

#[allow(non_camel_case_types)]
pub type fx_log_severity_t = c_int;

#[repr(C)]
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq)]
pub struct fx_log_metadata_t {
    pub pid: zx::sys::zx_koid_t,
    pub tid: zx::sys::zx_koid_t,
    pub time: zx::sys::zx_time_t,
    pub severity: fx_log_severity_t,
    pub dropped_logs: u32,
}

#[repr(C)]
#[derive(Clone)]
pub struct fx_log_packet_t {
    pub metadata: fx_log_metadata_t,
    // Contains concatenated tags and message and a null terminating character at
    // the end.
    // char(tag_len) + "tag1" + char(tag_len) + "tag2\0msg\0"
    pub data: [c_char; MAX_DATAGRAM_LEN - METADATA_SIZE],
}

impl Default for fx_log_packet_t {
    fn default() -> fx_log_packet_t {
        fx_log_packet_t {
            data: [0; MAX_DATAGRAM_LEN - METADATA_SIZE],
            metadata: Default::default(),
        }
    }
}

impl fx_log_packet_t {
    /// This struct has no padding bytes, but we can't use zerocopy because it needs const
    /// generics to support arrays this large.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                (self as *const Self) as *const u8,
                mem::size_of::<fx_log_packet_t>(),
            )
        }
    }

    /// Fills data with a single value for defined region.
    pub fn fill_data(&mut self, region: std::ops::Range<usize>, with: c_char) {
        self.data[region].iter_mut().for_each(|c| *c = with);
    }

    /// Copies bytes to data at specifies offset.
    pub fn add_data<T: std::convert::TryInto<c_char> + Copy>(&mut self, offset: usize, bytes: &[T])
    where
        <T as std::convert::TryInto<c_char>>::Error: std::fmt::Debug,
    {
        self.data[offset..(offset + bytes.len())]
            .iter_mut()
            .enumerate()
            .for_each(|(i, x)| *x = bytes[i].try_into().unwrap());
    }
}
