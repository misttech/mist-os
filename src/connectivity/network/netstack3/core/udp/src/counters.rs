// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities for tracking the counts of various UDP events.

use net_types::ip::IpMarked;
use netstack3_base::{Counter, Inspectable, Inspector, InspectorExt as _};

/// Counters for the UDP layer.
pub type UdpCounters<I> = IpMarked<I, UdpCountersInner>;

/// Counters for the UDP layer.
#[derive(Default)]
pub struct UdpCountersInner {
    /// Count of ICMP error messages received.
    pub rx_icmp_error: Counter,
    /// Count of UDP datagrams received from the IP layer, including error
    /// cases.
    pub rx: Counter,
    /// Count of incoming UDP datagrams dropped because it contained a mapped IP
    /// address in the header.
    pub rx_mapped_addr: Counter,
    /// Count of incoming UDP datagrams dropped because of an unknown
    /// destination port.
    pub rx_unknown_dest_port: Counter,
    /// Count of incoming UDP datagrams dropped because their UDP header was in
    /// a malformed state.
    pub rx_malformed: Counter,
    /// Count of outgoing UDP datagrams sent from the socket layer, including
    /// error cases.
    pub tx: Counter,
    /// Count of outgoing UDP datagrams which failed to be sent out of the
    /// transport layer.
    pub tx_error: Counter,
}

impl Inspectable for UdpCountersInner {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let UdpCountersInner {
            rx_icmp_error,
            rx,
            rx_mapped_addr,
            rx_unknown_dest_port,
            rx_malformed,
            tx,
            tx_error,
        } = self;
        inspector.record_child("Rx", |inspector| {
            inspector.record_counter("Received", rx);
            inspector.record_child("Errors", |inspector| {
                inspector.record_counter("MappedAddr", rx_mapped_addr);
                inspector.record_counter("UnknownDstPort", rx_unknown_dest_port);
                inspector.record_counter("Malformed", rx_malformed);
            });
        });
        inspector.record_child("Tx", |inspector| {
            inspector.record_counter("Sent", tx);
            inspector.record_counter("Errors", tx_error);
        });
        inspector.record_counter("IcmpErrors", rx_icmp_error);
    }
}
