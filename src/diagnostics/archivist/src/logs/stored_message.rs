// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    identity::ComponentIdentity,
    logs::{debuglog, stats::LogStreamStats},
};
use anyhow::format_err;
use diagnostics_data::{LogsData, Severity};
use diagnostics_message::error::MessageError;
use diagnostics_message::{LoggerMessage, METADATA_SIZE};
use fuchsia_zircon as zx;
use std::{convert::TryInto, sync::Arc};

#[derive(Debug)]
pub struct StoredMessage {
    bytes: MessageBytes,
    stats: Option<Arc<LogStreamStats>>,
}

#[derive(Debug)]
enum MessageBytes {
    Legacy(LoggerMessage),
    Structured { bytes: Box<[u8]>, severity: Severity, timestamp: i64 },
    DebugLog { msg: zx::sys::zx_log_record_t, severity: Severity, size: usize },
    Invalid { err: MessageError, severity: Severity, timestamp: i64 },
}

impl StoredMessage {
    pub fn legacy(buf: &[u8], stats: Arc<LogStreamStats>) -> Self {
        match buf.try_into() {
            Ok(msg) => StoredMessage { bytes: MessageBytes::Legacy(msg), stats: Some(stats) },
            Err(err) => StoredMessage::invalid(err, stats),
        }
    }

    pub fn structured(buf: Vec<u8>, stats: Arc<LogStreamStats>) -> Self {
        match diagnostics_message::parse_basic_structured_info(&buf) {
            Ok((timestamp, severity)) => StoredMessage {
                bytes: MessageBytes::Structured {
                    bytes: buf.into_boxed_slice(),
                    severity,
                    timestamp,
                },
                stats: Some(stats),
            },
            Err(err) => StoredMessage::invalid(err, stats),
        }
    }

    fn invalid(err: MessageError, stats: Arc<LogStreamStats>) -> Self {
        // When we fail to parse a message set a WARN for it and use the timestamp for when the
        // message was received. We'll be adding an error for this.
        let severity = Severity::Warn;
        let timestamp = zx::Time::get_monotonic().into_nanos();
        StoredMessage {
            bytes: MessageBytes::Invalid { err, severity, timestamp },
            stats: Some(stats),
        }
    }

    pub fn debuglog(record: zx::sys::zx_log_record_t) -> Option<Self> {
        let data_len = record.datalen as usize;

        let mut contents = String::from_utf8_lossy(&record.data[0..data_len]).into_owned();
        if let Some(b'\n') = contents.bytes().last() {
            contents.pop();
        }

        // TODO(fxbug.dev/32998): Once we support structured logs we won't need this
        // hack to match a string in klogs.
        const MAX_STRING_SEARCH_SIZE: usize = 170;
        let last = contents
            .char_indices()
            .nth(MAX_STRING_SEARCH_SIZE)
            .map(|(i, _)| i)
            .unwrap_or(contents.len());

        // Don't look beyond the 170th character in the substring to limit the cost
        // of the substring search operation.
        let early_contents = &contents[..last];

        let severity = if early_contents.contains("ERROR:") {
            Severity::Error
        } else if early_contents.contains("WARNING:") {
            Severity::Warn
        } else {
            Severity::Info
        };

        let size = METADATA_SIZE + 5 /*'klog' tag*/ + contents.len() + 1;
        Some(StoredMessage {
            bytes: MessageBytes::DebugLog { msg: record, severity, size },
            stats: None,
        })
    }

    pub fn size(&self) -> usize {
        match &self.bytes {
            MessageBytes::Legacy(msg) => msg.size_bytes,
            MessageBytes::Structured { bytes, .. } => bytes.len(),
            MessageBytes::DebugLog { size, .. } => *size,
            MessageBytes::Invalid { .. } => std::mem::size_of::<MessageError>(),
        }
    }

    pub fn has_stats(&self) -> bool {
        self.stats.is_some()
    }

    pub fn with_stats(&mut self, stats: Arc<LogStreamStats>) {
        self.stats = Some(stats)
    }

    pub fn severity(&self) -> Severity {
        match &self.bytes {
            MessageBytes::Legacy(msg) => msg.severity,
            MessageBytes::Structured { severity, .. } => *severity,
            MessageBytes::DebugLog { severity, .. } => *severity,
            MessageBytes::Invalid { severity, .. } => *severity,
        }
    }

    pub fn timestamp(&self) -> i64 {
        match &self.bytes {
            MessageBytes::Legacy(msg) => msg.timestamp,
            MessageBytes::Structured { timestamp, .. } => *timestamp,
            MessageBytes::DebugLog { msg, .. } => msg.timestamp,
            MessageBytes::Invalid { timestamp, .. } => *timestamp,
        }
    }
}

impl Drop for StoredMessage {
    fn drop(&mut self) {
        if let Some(stats) = &self.stats {
            stats.increment_rolled_out(&*self);
        }
    }
}

impl StoredMessage {
    pub fn parse(&self, source: &ComponentIdentity) -> Result<LogsData, anyhow::Error> {
        match &self.bytes {
            MessageBytes::Legacy(msg) => {
                Ok(diagnostics_message::from_logger(source.into(), msg.clone()))
            }
            MessageBytes::Structured { bytes, .. } => {
                let data = diagnostics_message::from_structured(source.into(), bytes)?;
                Ok(data)
            }
            MessageBytes::DebugLog { msg, .. } => debuglog::convert_debuglog_to_log_message(msg)
                .ok_or(format_err!("couldn't convert debuglog message")),
            MessageBytes::Invalid { err, .. } => Err(err.clone().into()),
        }
    }
}
