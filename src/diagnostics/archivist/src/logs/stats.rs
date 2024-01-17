// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::logs::stored_message::StoredMessage;
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

    pub fn increment_rolled_out<T: StoredMessage + ?Sized>(&self, msg: &T) {
        self.rolled_out.count(msg);
    }

    pub fn ingest_message<T: StoredMessage + ?Sized>(&self, msg: &T) {
        self.last_timestamp.set(msg.timestamp());
        self.total.count(msg);
        match msg.severity() {
            Severity::Trace => self.trace.count(msg),
            Severity::Debug => self.debug.count(msg),
            Severity::Info => self.info.count(msg),
            Severity::Warn => self.warn.count(msg),
            Severity::Error => self.error.count(msg),
            Severity::Fatal => self.fatal.count(msg),
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
    fn count<T: StoredMessage + ?Sized>(&self, msg: &T) {
        self.number.add(1);
        self.bytes.add(msg.size() as u64);
    }
}
