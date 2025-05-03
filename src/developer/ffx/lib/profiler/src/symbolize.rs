// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::parse::{Pid, Tid};
use ffx_symbolize::ResolvedLocation;
use std::collections::HashMap;

/// A resolved address.
#[derive(Clone, PartialEq)]
pub struct ResolvedAddress {
    /// Address for which source locations were resolved.
    pub addr: u64,
    /// Source locations found at `addr`.
    pub locations: Vec<ResolvedLocation>,
}

impl std::fmt::Debug for ResolvedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAddress")
            .field("addr", &format_args!("0x{:x}", self.addr))
            .field("lines", &self.locations)
            .finish()
    }
}

/// Symbolized record hash map. key: pid, value: all of the records belong to this pid.
#[derive(PartialEq, Clone, Debug, Default)]
pub struct SymbolizedRecords {
    records: HashMap<Pid, Vec<SymbolizedRecord>>,
}

impl SymbolizedRecords {
    #[allow(dead_code)]
    fn add_pid(&mut self, pid: Pid) {
        self.records.insert(pid, Vec::new());
    }
}

/// Symbolized bt list for a single tid.
#[derive(PartialEq, Clone, Debug)]
struct SymbolizedRecord {
    tid: Tid,
    backtraces: Vec<ResolvedAddress>,
}

impl SymbolizedRecord {
    #[allow(dead_code)]
    fn add_backtrace(&mut self, backtrace: ResolvedAddress) {
        self.backtraces.push(backtrace);
    }
}
