// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Counters kept in bindings context.

use netstack3_core::inspect::{Inspectable, Inspector};
use netstack3_core::types::Counter;

/// Holder for generic bindings counters.
#[derive(Debug, Default)]
pub(crate) struct BindingsCounters {
    pub(crate) power: PowerCounters,
}

impl Inspectable for BindingsCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { power } = self;
        inspector.record_inspectable("Power", power);
    }
}

/// Holder for power-related counters.
#[derive(Debug, Default)]
pub(crate) struct PowerCounters {
    pub(crate) dropped_rx_leases: Counter,
}

impl Inspectable for PowerCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { dropped_rx_leases } = self;
        inspector.record_counter("DroppedRxLeases", dropped_rx_leases);
    }
}
