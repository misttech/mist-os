// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
#[macro_use]
mod testing;

mod args;
mod blob;
mod error;
mod event;
mod header;
mod init;
mod log;
mod metadata;
mod objects;
mod scheduling;
mod session;
mod string;
mod thread;

pub use args::{Arg, ArgValue};
pub use blob::{BlobRecord, BlobType, LargeBlobMetadata, LargeBlobRecord};
pub use error::{ParseError, ParseWarning};
pub use event::{EventPayload, EventRecord};
pub use log::LogRecord;
pub use metadata::{Provider, ProviderEvent};
pub use objects::{KernelObjRecord, UserspaceObjRecord};
pub use scheduling::{
    ContextSwitchEvent, LegacyContextSwitchEvent, SchedulingRecord, ThreadState, ThreadWakeupEvent,
};
pub use session::{parse_full_session, SessionParser};
pub use thread::{ProcessKoid, ThreadKoid};

use crate::{
    blob::{RawBlobRecord, RawLargeBlobRecord},
    error::ParseResult,
    event::RawEventRecord,
    init::InitRecord,
    log::RawLogRecord,
    metadata::{MetadataRecord, TraceInfoMetadataRecord},
    objects::{RawKernelObjRecord, RawUserspaceObjRecord},
    scheduling::RawSchedulingRecord,
    session::ResolveCtx,
    string::StringRecord,
    thread::ThreadRecord,
};

#[derive(Clone, Debug, PartialEq)]
pub enum TraceRecord {
    Event(EventRecord),
    Blob(BlobRecord),
    UserspaceObj(UserspaceObjRecord),
    KernelObj(KernelObjRecord),
    Scheduling(SchedulingRecord),
    Log(LogRecord),
    LargeBlob(LargeBlobRecord),
    ProviderEvent { provider: Provider, event: ProviderEvent },
}

impl TraceRecord {
    pub fn process(&self) -> Option<ProcessKoid> {
        match self {
            Self::Event(EventRecord { process, .. })
            | Self::Log(LogRecord { process, .. })
            | Self::UserspaceObj(UserspaceObjRecord { process, .. }) => Some(*process),
            Self::Scheduling(s) => s.process(),
            Self::KernelObj(k) => k.process(),
            Self::Blob(..) | Self::LargeBlob(..) | Self::ProviderEvent { .. } => None,
        }
    }

    pub fn thread(&self) -> Option<ThreadKoid> {
        match self {
            Self::Event(EventRecord { thread, .. }) | Self::Log(LogRecord { thread, .. }) => {
                Some(*thread)
            }
            Self::Scheduling(s) => Some(s.thread()),
            Self::Blob(..)
            | Self::KernelObj(..)
            | Self::LargeBlob(..)
            | Self::ProviderEvent { .. }
            | Self::UserspaceObj(..) => None,
        }
    }

    fn resolve(ctx: &mut ResolveCtx, raw: RawTraceRecord<'_>) -> Result<Option<Self>, ParseError> {
        Ok(match raw {
            // Callers who want to handle unknown record types should use the raw record types.
            RawTraceRecord::Unknown { raw_type } => {
                ctx.add_warning(ParseWarning::UnknownTraceRecordType(raw_type));
                None
            }

            RawTraceRecord::Metadata(m) => ctx.on_metadata_record(m)?,
            RawTraceRecord::Init(i) => {
                ctx.on_init_record(i);
                None
            }
            RawTraceRecord::String(s) => {
                ctx.on_string_record(s);
                None
            }
            RawTraceRecord::Thread(t) => {
                ctx.on_thread_record(t);
                None
            }
            RawTraceRecord::Event(e) => Some(Self::Event(EventRecord::resolve(ctx, e))),
            RawTraceRecord::Blob(b) => Some(Self::Blob(BlobRecord::resolve(ctx, b))),
            RawTraceRecord::UserspaceObj(u) => {
                Some(Self::UserspaceObj(UserspaceObjRecord::resolve(ctx, u)))
            }
            RawTraceRecord::KernelObj(k) => Some(Self::KernelObj(KernelObjRecord::resolve(ctx, k))),
            RawTraceRecord::Scheduling(s) => {
                SchedulingRecord::resolve(ctx, s).map(Self::Scheduling)
            }
            RawTraceRecord::Log(l) => Some(Self::Log(LogRecord::resolve(ctx, l))),
            RawTraceRecord::LargeBlob(lb) => LargeBlobRecord::resolve(ctx, lb).map(Self::LargeBlob),
        })
    }
}

#[derive(Debug, PartialEq)]
enum RawTraceRecord<'a> {
    Metadata(MetadataRecord),
    Init(InitRecord),
    String(StringRecord<'a>),
    Thread(ThreadRecord),
    Event(RawEventRecord<'a>),
    Blob(RawBlobRecord<'a>),
    UserspaceObj(RawUserspaceObjRecord<'a>),
    KernelObj(RawKernelObjRecord<'a>),
    Scheduling(RawSchedulingRecord<'a>),
    Log(RawLogRecord<'a>),
    LargeBlob(RawLargeBlobRecord<'a>),
    Unknown { raw_type: u8 },
}

trace_header! {
    BaseTraceHeader {}
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedWithOriginalBytes<'a, T> {
    pub parsed: T,
    pub bytes: &'a [u8],
}

const METADATA_RECORD_TYPE: u8 = 0;
const INIT_RECORD_TYPE: u8 = 1;
const STRING_RECORD_TYPE: u8 = 2;
const THREAD_RECORD_TYPE: u8 = 3;
const EVENT_RECORD_TYPE: u8 = 4;
const BLOB_RECORD_TYPE: u8 = 5;
const USERSPACE_OBJ_RECORD_TYPE: u8 = 6;
const KERNEL_OBJ_RECORD_TYPE: u8 = 7;
const SCHEDULING_RECORD_TYPE: u8 = 8;
const LOG_RECORD_TYPE: u8 = 9;
const LARGE_RECORD_TYPE: u8 = 15;

impl<'a> RawTraceRecord<'a> {
    fn parse(buf: &'a [u8]) -> ParseResult<'a, ParsedWithOriginalBytes<'a, Self>> {
        use nom::combinator::map;
        let base_header = BaseTraceHeader::parse(buf)?.1;
        let size_bytes = base_header.size_words() as usize * 8;
        if size_bytes == 0 {
            return Err(nom::Err::Failure(ParseError::InvalidSize));
        }
        if size_bytes > buf.len() {
            return Err(nom::Err::Incomplete(nom::Needed::Size(size_bytes - buf.len())));
        }

        let (buf, rem) = buf.split_at(size_bytes);
        let (_, parsed) = match base_header.raw_type() {
            METADATA_RECORD_TYPE => map(MetadataRecord::parse, |m| Self::Metadata(m))(buf),
            INIT_RECORD_TYPE => map(InitRecord::parse, |i| Self::Init(i))(buf),
            STRING_RECORD_TYPE => map(StringRecord::parse, |s| Self::String(s))(buf),
            THREAD_RECORD_TYPE => map(ThreadRecord::parse, |t| Self::Thread(t))(buf),
            EVENT_RECORD_TYPE => map(RawEventRecord::parse, |e| Self::Event(e))(buf),
            BLOB_RECORD_TYPE => map(RawBlobRecord::parse, |b| Self::Blob(b))(buf),
            USERSPACE_OBJ_RECORD_TYPE => {
                map(RawUserspaceObjRecord::parse, |u| Self::UserspaceObj(u))(buf)
            }
            KERNEL_OBJ_RECORD_TYPE => map(RawKernelObjRecord::parse, |k| Self::KernelObj(k))(buf),
            SCHEDULING_RECORD_TYPE => map(RawSchedulingRecord::parse, |s| Self::Scheduling(s))(buf),
            LOG_RECORD_TYPE => map(RawLogRecord::parse, |l| Self::Log(l))(buf),
            LARGE_RECORD_TYPE => map(RawLargeBlobRecord::parse, |l| Self::LargeBlob(l))(buf),
            raw_type => Ok((&[][..], Self::Unknown { raw_type })),
        }?;

        Ok((rem, ParsedWithOriginalBytes { parsed, bytes: buf }))
    }

    fn is_magic_number(&self) -> bool {
        matches!(
            self,
            Self::Metadata(MetadataRecord::TraceInfo(TraceInfoMetadataRecord::MagicNumber)),
        )
    }
}

/// Take the first `unpadded_len` bytes from a buffer, returning a suffix beginning at the next
/// world-aligned region and discarding padding bytes.
fn take_n_padded<'a>(unpadded_len: usize, buf: &'a [u8]) -> ParseResult<'a, &'a [u8]> {
    let padded_len = unpadded_len + word_padding(unpadded_len);
    if padded_len > buf.len() {
        return Err(nom::Err::Incomplete(nom::Needed::Size(padded_len - buf.len())));
    }
    let (with_padding, rem) = buf.split_at(padded_len);
    let (unpadded, _padding) = with_padding.split_at(unpadded_len);
    Ok((rem, unpadded))
}

fn word_padding(unpadded_len: usize) -> usize {
    match unpadded_len % 8 {
        0 | 8 => 0,
        nonzero => 8 - nonzero,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_empty_bytes() {
        let (trailing, parsed) = take_n_padded(0, &[1, 1, 1, 1]).unwrap();
        assert_eq!(parsed, []);
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn take_unpadded_bytes() {
        let (trailing, parsed) = take_n_padded(8, &[5, 5, 5, 5, 5, 5, 5, 5, 1, 1, 1, 1]).unwrap();
        assert_eq!(parsed, [5, 5, 5, 5, 5, 5, 5, 5]);
        assert_eq!(trailing, [1, 1, 1, 1]);
    }

    #[test]
    fn take_padded_bytes() {
        let (trailing, parsed) = take_n_padded(6, &[5, 5, 5, 5, 5, 5, 0, 0, 1, 1, 1, 1]).unwrap();
        assert_eq!(parsed, [5, 5, 5, 5, 5, 5]);
        assert_eq!(trailing, [1, 1, 1, 1],);
    }
}
