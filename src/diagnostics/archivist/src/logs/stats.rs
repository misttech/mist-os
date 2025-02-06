// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::Severity;
use fuchsia_inspect::{IntProperty, Node, NumericProperty, Property, StringProperty, UintProperty};
use fuchsia_inspect_derive::Inspect;

#[derive(Debug, Default, Inspect)]
pub struct LogStreamStats {
    sockets_opened: UintProperty,
    sockets_closed: UintProperty,
    last_timestamp: IntProperty,
    total: LogCounter,
    rolled_out: LogCounter,
    fatal: LogCounter,
    error: LogCounter,
    warn: LogCounter,
    info: LogCounter,
    debug: LogCounter,
    trace: LogCounter,
    url: StringProperty,
    invalid: LogCounter,
    inspect_node: Node,
}

impl LogStreamStats {
    pub fn set_url(&self, url: &str) {
        self.url.set(url);
    }

    pub fn open_socket(&self) {
        self.sockets_opened.add(1);
    }

    pub fn close_socket(&self) {
        self.sockets_closed.add(1);
    }

    pub fn increment_rolled_out(&self, msg_len: usize) {
        self.rolled_out.increment_bytes(msg_len);
    }

    pub fn increment_invalid(&self, bytes: usize) {
        self.invalid.increment_bytes(bytes);
    }

    pub fn ingest_message(&self, bytes: usize, severity: Severity) {
        self.total.count(bytes);
        match severity {
            Severity::Trace => self.trace.count(bytes),
            Severity::Debug => self.debug.count(bytes),
            Severity::Info => self.info.count(bytes),
            Severity::Warn => self.warn.count(bytes),
            Severity::Error => self.error.count(bytes),
            Severity::Fatal => self.fatal.count(bytes),
        }
    }
}

#[derive(Debug, Default, Inspect)]
struct LogCounter {
    number: UintProperty,
    bytes: UintProperty,

    inspect_node: Node,
}

impl LogCounter {
    fn count(&self, bytes: usize) {
        self.number.add(1);
        self.bytes.add(bytes as u64);
    }

    fn increment_bytes(&self, bytes: usize) {
        self.number.add(1);
        self.bytes.add(bytes as u64);
    }
}
