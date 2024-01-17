// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    identity::ComponentIdentity,
    logs::{debuglog, stats::LogStreamStats},
};
use anyhow::{format_err, Result};
use diagnostics_data::{LogsData, Severity};
use diagnostics_message::error::MessageError;
use diagnostics_message::{LoggerMessage, METADATA_SIZE};
use fuchsia_zircon as zx;
use std::{convert::TryInto, fmt::Debug, sync::Arc};

pub type GenericStoredMessage = Box<dyn StoredMessage>;

pub trait StoredMessage: Debug + Send + Sync {
    fn size(&self) -> usize;
    fn severity(&self) -> Severity;
    fn timestamp(&self) -> i64;
    fn parse(&self, source: &ComponentIdentity) -> Result<LogsData, anyhow::Error>;
}

#[derive(Debug)]
pub struct LegacyStoredMessage {
    msg: LoggerMessage,
    stats: Arc<LogStreamStats>,
}

impl LegacyStoredMessage {
    pub fn new(buf: Vec<u8>, stats: Arc<LogStreamStats>) -> GenericStoredMessage {
        match buf.as_slice().try_into() {
            Ok(msg) => Box::new(Self { msg, stats }),
            Err(err) => Box::new(InvalidStoredMessage::new(err, stats)),
        }
    }
}

impl Drop for LegacyStoredMessage {
    fn drop(&mut self) {
        self.stats.increment_rolled_out(&*self);
    }
}

impl StoredMessage for LegacyStoredMessage {
    fn size(&self) -> usize {
        self.msg.size_bytes
    }

    fn severity(&self) -> Severity {
        self.msg.severity
    }

    fn timestamp(&self) -> i64 {
        self.msg.timestamp
    }

    fn parse(&self, source: &ComponentIdentity) -> Result<LogsData> {
        Ok(diagnostics_message::from_logger(source.into(), self.msg.clone()))
    }
}

#[derive(Debug)]
pub struct StructuredStoredMessage {
    bytes: Box<[u8]>,
    severity: Severity,
    timestamp: i64,
    stats: Arc<LogStreamStats>,
}

impl StructuredStoredMessage {
    pub fn new(buf: Vec<u8>, stats: Arc<LogStreamStats>) -> GenericStoredMessage {
        match diagnostics_message::parse_basic_structured_info(&buf) {
            Ok((timestamp, severity)) => Box::new(StructuredStoredMessage {
                bytes: buf.into_boxed_slice(),
                severity,
                timestamp,
                stats,
            }),
            Err(err) => Box::new(InvalidStoredMessage::new(err, stats)),
        }
    }
}

impl Drop for StructuredStoredMessage {
    fn drop(&mut self) {
        self.stats.increment_rolled_out(&*self);
    }
}

impl StoredMessage for StructuredStoredMessage {
    fn size(&self) -> usize {
        self.bytes.len()
    }

    fn severity(&self) -> Severity {
        self.severity
    }

    fn timestamp(&self) -> i64 {
        self.timestamp
    }

    fn parse(&self, source: &ComponentIdentity) -> Result<LogsData> {
        let data = diagnostics_message::from_structured(source.into(), &self.bytes)?;
        Ok(data)
    }
}

#[derive(Debug)]
pub struct DebugLogStoredMessage {
    msg: zx::sys::zx_log_record_t,
    severity: Severity,
    size: usize,
    stats: Arc<LogStreamStats>,
}

impl DebugLogStoredMessage {
    pub fn new(record: zx::sys::zx_log_record_t, stats: Arc<LogStreamStats>) -> Self {
        let data_len = record.datalen as usize;

        let mut contents = String::from_utf8_lossy(&record.data[0..data_len]).into_owned();
        if let Some(b'\n') = contents.bytes().last() {
            contents.pop();
        }

        // TODO(https://fxbug.dev/32998): Once we support structured logs we won't need this
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
        DebugLogStoredMessage { msg: record, severity, size, stats }
    }
}

impl Drop for DebugLogStoredMessage {
    fn drop(&mut self) {
        self.stats.increment_rolled_out(&*self);
    }
}

impl StoredMessage for DebugLogStoredMessage {
    fn size(&self) -> usize {
        self.size
    }

    fn severity(&self) -> Severity {
        self.severity
    }

    fn timestamp(&self) -> i64 {
        self.msg.timestamp
    }

    fn parse(&self, _source: &ComponentIdentity) -> Result<LogsData> {
        debuglog::convert_debuglog_to_log_message(&self.msg)
            .ok_or(format_err!("couldn't convert debuglog message"))
    }
}

#[derive(Debug)]
pub struct InvalidStoredMessage {
    err: MessageError,
    severity: Severity,
    timestamp: i64,
    stats: Arc<LogStreamStats>,
}

impl InvalidStoredMessage {
    pub fn new(err: MessageError, stats: Arc<LogStreamStats>) -> Self {
        // When we fail to parse a message set a WARN for it and use the timestamp for when the
        // message was received. We'll be adding an error for this.
        let severity = Severity::Warn;
        let timestamp = zx::Time::get_monotonic().into_nanos();
        InvalidStoredMessage { err, severity, timestamp, stats }
    }
}

impl Drop for InvalidStoredMessage {
    fn drop(&mut self) {
        self.stats.increment_rolled_out(&*self);
    }
}

impl StoredMessage for InvalidStoredMessage {
    fn size(&self) -> usize {
        std::mem::size_of::<MessageError>()
    }

    fn severity(&self) -> Severity {
        self.severity
    }

    fn timestamp(&self) -> i64 {
        self.timestamp
    }

    fn parse(&self, _source: &ComponentIdentity) -> Result<LogsData> {
        Err(self.err.clone().into())
    }
}
