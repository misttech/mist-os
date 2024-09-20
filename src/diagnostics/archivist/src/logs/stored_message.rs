// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use crate::logs::stats::LogStreamStats;
use anyhow::Result;
use bstr::{BStr, ByteSlice};
use diagnostics_data::{LegacySeverity, LogsData, Severity};
use diagnostics_log_encoding::encode::{
    Encoder, EncoderOpts, EncodingError, MutableBuffer, RecordEvent, WriteEventParams,
};
use diagnostics_message::LoggerMessage;
use fidl_fuchsia_diagnostics_stream::RawSeverity;
use fidl_fuchsia_logger::MAX_DATAGRAM_LEN_BYTES;
use std::fmt::Debug;
use std::io::Cursor;
use std::sync::Arc;
use {fidl_fuchsia_diagnostics as fdiagnostics, fuchsia_zircon as zx};

#[derive(Debug)]
pub struct StoredMessage {
    bytes: Box<[u8]>,
    severity: Severity,
    timestamp: zx::BootTime,
    stats: Arc<LogStreamStats>,
}

impl StoredMessage {
    pub fn new(buf: Box<[u8]>, stats: &Arc<LogStreamStats>) -> Option<Self> {
        match diagnostics_log_encoding::parse::basic_info(&buf) {
            Ok((timestamp, severity)) => {
                let transcoded_i32 = i8::from_le_bytes(severity.to_le_bytes()) as i32;
                let Ok(severity) = LegacySeverity::try_from(transcoded_i32) else {
                    stats.increment_invalid(buf.len());
                    return None;
                };
                Some(StoredMessage {
                    bytes: buf,
                    severity: severity.into(),
                    timestamp,
                    stats: Arc::clone(stats),
                })
            }
            _ => {
                stats.increment_invalid(buf.len());
                None
            }
        }
    }

    pub fn from_legacy(buf: Box<[u8]>, stats: &Arc<LogStreamStats>) -> Option<Self> {
        let Ok(LoggerMessage {
            timestamp,
            severity,
            verbosity,
            message,
            pid,
            tid,
            dropped_logs,
            tags,
            size_bytes: _,
        }) = LoggerMessage::try_from(buf.as_ref())
        else {
            stats.increment_invalid(buf.len());
            return None;
        };
        let severity = verbosity.and_then(get_severity_from_verbosity).unwrap_or(severity);
        let mut encoder =
            Encoder::new(Cursor::new([0u8; MAX_DATAGRAM_LEN_BYTES as _]), EncoderOpts::default());
        let _ = encoder.write_event(WriteEventParams {
            event: LegacyMessageRecord {
                severity: fdiagnostics::Severity::from(severity).into_primitive(),
                data: &message,
                timestamp,
            },
            tags: &tags,
            metatags: std::iter::empty(),
            pid: zx::Koid::from_raw(pid),
            tid: zx::Koid::from_raw(tid),
            dropped: dropped_logs,
        });
        let cursor = encoder.take();
        let position = cursor.position() as usize;
        let buf = cursor.get_ref();
        Some(Self {
            timestamp,
            severity,
            bytes: Box::from(&buf[..position]),
            stats: Arc::clone(stats),
        })
    }

    pub fn from_debuglog(
        record: zx::DebugLogRecord,
        dropped: u64,
        stats: Arc<LogStreamStats>,
    ) -> Self {
        let mut data = record.data();
        if let Some(b'\n') = data.last() {
            data = &data[..data.len() - 1];
        }

        let severity = match record.severity {
            zx::DebugLogSeverity::Trace => fdiagnostics::Severity::Trace,
            zx::DebugLogSeverity::Debug => fdiagnostics::Severity::Debug,
            zx::DebugLogSeverity::Warn => fdiagnostics::Severity::Warn,
            zx::DebugLogSeverity::Error => fdiagnostics::Severity::Error,
            zx::DebugLogSeverity::Fatal => fdiagnostics::Severity::Fatal,
            zx::DebugLogSeverity::Unknown => fdiagnostics::Severity::Info,
            zx::DebugLogSeverity::Info => {
                // By default `zx_log_record_t` carries INFO severity. Since `zx_debuglog_write`
                // doesn't support setting a severity, historically logs have been tagged and
                // annotated with their severity in the message. If we get here attempt to use the
                // severity in the message, otherwise fallback to INFO.
                const MAX_STRING_SEARCH_SIZE: usize = 170;
                let last = data
                    .char_indices()
                    .nth(MAX_STRING_SEARCH_SIZE)
                    .map(|(i, _, _)| i)
                    .unwrap_or(data.len());
                let early_contents = &data[..last];
                if early_contents.contains_str("ERROR:") {
                    fdiagnostics::Severity::Error
                } else if early_contents.contains_str("WARNING:") {
                    fdiagnostics::Severity::Warn
                } else {
                    fdiagnostics::Severity::Info
                }
            }
        };

        let mut encoder =
            Encoder::new(Cursor::new([0u8; MAX_DATAGRAM_LEN_BYTES as _]), EncoderOpts::default());
        let _ = encoder.write_event(WriteEventParams {
            event: DebugLogRecordEvent {
                severity: severity.into_primitive(),
                data,
                timestamp: record.timestamp,
            },
            tags: &["klog"],
            metatags: std::iter::empty(),
            pid: record.pid,
            tid: record.tid,
            dropped,
        });
        let cursor = encoder.take();
        let position = cursor.position() as usize;
        let buf = cursor.get_ref();

        Self {
            bytes: Box::from(&buf[..position]),
            severity: severity.into(),
            timestamp: record.timestamp,
            stats,
        }
    }

    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    pub fn severity(&self) -> Severity {
        self.severity
    }

    pub fn timestamp(&self) -> zx::BootTime {
        self.timestamp
    }

    pub fn parse(&self, source: &ComponentIdentity) -> Result<LogsData> {
        let data = diagnostics_message::from_structured(source.into(), &self.bytes)?;
        Ok(data)
    }
}

fn get_severity_from_verbosity(verbosity: i8) -> Option<Severity> {
    if verbosity == 0 {
        return None;
    }
    let severity = fdiagnostics::Severity::Info
        .into_primitive()
        .saturating_sub(verbosity.try_into().unwrap_or(0));
    if severity < fdiagnostics::Severity::Info.into_primitive() {
        return Some(Severity::Trace);
    }
    Some(Severity::Debug)
}

impl Drop for StoredMessage {
    fn drop(&mut self) {
        self.stats.increment_rolled_out(&*self);
    }
}

struct LegacyMessageRecord<'a> {
    severity: RawSeverity,
    data: &'a str,
    timestamp: zx::BootTime,
}

impl RecordEvent for LegacyMessageRecord<'_> {
    fn severity(&self) -> RawSeverity {
        self.severity
    }

    fn file(&self) -> Option<&str> {
        None
    }

    fn line(&self) -> Option<u32> {
        None
    }

    fn target(&self) -> &str {
        ""
    }

    fn timestamp(&self) -> zx::BootTime {
        self.timestamp
    }

    fn write_arguments<B: MutableBuffer>(
        self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError> {
        writer.write_argument(diagnostics_log_encoding::encode::Argument {
            name: "message",
            value: self.data.into(),
        })?;
        Ok(())
    }
}

struct DebugLogRecordEvent<'a> {
    severity: RawSeverity,
    data: &'a BStr,
    timestamp: zx::BootTime,
}

impl RecordEvent for DebugLogRecordEvent<'_> {
    fn severity(&self) -> RawSeverity {
        self.severity
    }

    fn file(&self) -> Option<&str> {
        None
    }

    fn line(&self) -> Option<u32> {
        None
    }

    fn target(&self) -> &str {
        ""
    }

    fn timestamp(&self) -> zx::BootTime {
        self.timestamp
    }

    fn write_arguments<B: MutableBuffer>(
        self,
        writer: &mut Encoder<B>,
    ) -> Result<(), EncodingError> {
        writer.write_argument(diagnostics_log_encoding::encode::Argument {
            name: "message",
            value: self.data.to_str_lossy().as_ref().into(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::debuglog::KERNEL_IDENTITY;
    use crate::logs::testing::TestDebugEntry;
    use diagnostics_data::{BuilderArgs, LogsDataBuilder};
    use fidl_fuchsia_logger::LogMessage;

    #[fuchsia::test]
    fn convert_debuglog_to_log_message_test() {
        let klog = TestDebugEntry::new("test log".as_bytes());
        let data = StoredMessage::from_debuglog(klog.record, 10, Default::default())
            .parse(&KERNEL_IDENTITY)
            .unwrap();
        assert_eq!(
            data,
            LogsDataBuilder::new(BuilderArgs {
                timestamp: klog.record.timestamp.into(),
                component_url: Some(KERNEL_IDENTITY.url.clone()),
                moniker: KERNEL_IDENTITY.moniker.clone(),
                severity: Severity::Info,
            })
            .set_dropped(10)
            .set_pid(klog.record.pid.raw_koid())
            .set_tid(klog.record.tid.raw_koid())
            .add_tag("klog")
            .set_message("test log".to_string())
            .build()
        );
        // make sure the `klog` tag still shows up for legacy listeners
        let log_message: LogMessage = data.into();
        assert_eq!(
            log_message,
            LogMessage {
                pid: klog.record.pid.raw_koid(),
                tid: klog.record.tid.raw_koid(),
                time: klog.record.timestamp.into_nanos(),
                severity: fdiagnostics::Severity::Info.into_primitive() as i32,
                dropped_logs: 10,
                tags: vec!["klog".to_string()],
                msg: "test log".to_string(),
            }
        );

        // maximum allowed klog size
        let klog = TestDebugEntry::new(&vec![b'a'; zx::sys::ZX_LOG_RECORD_DATA_MAX]);
        let data = StoredMessage::from_debuglog(klog.record, 0, Default::default())
            .parse(&KERNEL_IDENTITY)
            .unwrap();
        assert_eq!(
            data,
            LogsDataBuilder::new(BuilderArgs {
                timestamp: klog.record.timestamp.into(),
                component_url: Some(KERNEL_IDENTITY.url.clone()),
                moniker: KERNEL_IDENTITY.moniker.clone(),
                severity: Severity::Info,
            })
            .set_pid(klog.record.pid.raw_koid())
            .set_tid(klog.record.tid.raw_koid())
            .add_tag("klog")
            .set_message(String::from_utf8(vec![b'a'; zx::sys::ZX_LOG_RECORD_DATA_MAX]).unwrap())
            .build()
        );

        // empty message
        let klog = TestDebugEntry::new(&[]);
        let data = StoredMessage::from_debuglog(klog.record, 0, Default::default())
            .parse(&KERNEL_IDENTITY)
            .unwrap();
        assert_eq!(
            data,
            LogsDataBuilder::new(BuilderArgs {
                timestamp: klog.record.timestamp.into(),
                component_url: Some(KERNEL_IDENTITY.url.clone()),
                moniker: KERNEL_IDENTITY.moniker.clone(),
                severity: Severity::Info,
            })
            .set_pid(klog.record.pid.raw_koid())
            .set_tid(klog.record.tid.raw_koid())
            .add_tag("klog")
            .set_message("".to_string())
            .build()
        );

        // invalid utf-8
        let klog = TestDebugEntry::new(b"\x00\x9f\x92");
        let data = StoredMessage::from_debuglog(klog.record, 0, Default::default())
            .parse(&KERNEL_IDENTITY)
            .unwrap();
        assert_eq!(data.msg().unwrap(), "\x00��");
    }
}
