// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for working with and exposing packet statistic counters.

use core::sync::atomic::{AtomicU64, Ordering};
use net_types::ip::{Ipv4, Ipv6};

use crate::{
    device::{arp::ArpCounters, DeviceCounters},
    ip::{
        icmp::{IcmpRxCounters, IcmpTxCounters, NdpCounters},
        IpCounters, Ipv4Counters, Ipv6Counters,
    },
    transport::udp::UdpCounters,
    NonSyncContext, SyncCtx,
};

/// An atomic counter for packet statistics, e.g. IPv4 packets received.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub(crate) fn increment(&self) {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        let _: u64 = v.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically retrieves the counter value as a `u64`.
    pub fn get(&self) -> u64 {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        v.load(Ordering::Relaxed)
    }
}

/// Stack counters for export outside of core.
pub struct StackCounters<'a> {
    /// IPv4 layer common counters.
    pub ipv4_common: &'a IpCounters<Ipv4>,
    /// IPv6 layer common counters.
    pub ipv6_common: &'a IpCounters<Ipv6>,
    /// IPv4 layer specific counters.
    pub ipv4: &'a Ipv4Counters,
    /// IPv6 layer specific counters.
    pub ipv6: &'a Ipv6Counters,
    /// ARP layer counters.
    pub arp: &'a ArpCounters,
    /// UDP layer counters for IPv4.
    pub udpv4: &'a UdpCounters<Ipv4>,
    /// UDP layer counters for IPv6.
    pub udpv6: &'a UdpCounters<Ipv6>,
    /// ICMP layer counters for IPv4 Rx-path.
    pub icmpv4_rx: &'a IcmpRxCounters<Ipv4>,
    /// ICMP layer counters for IPv4 Tx-path.
    pub icmpv4_tx: &'a IcmpTxCounters<Ipv4>,
    /// ICMP layer counters for IPv6 Rx-path.
    pub icmpv6_rx: &'a IcmpRxCounters<Ipv6>,
    /// ICMP layer counters for IPv4 Tx-path.
    pub icmpv6_tx: &'a IcmpTxCounters<Ipv6>,
    /// NDP counters.
    pub ndp: &'a NdpCounters,
    /// Device layer counters.
    pub devices: &'a DeviceCounters,
}

/// Visitor for stack counters.
pub trait CounterVisitor {
    /// Performs a user-defined operation on stack counters.
    fn visit_counters(&self, counters: StackCounters<'_>);
}

/// Provides access to stack counters via a visitor.
pub fn inspect_counters<BC: NonSyncContext, V: CounterVisitor>(
    core_ctx: &SyncCtx<BC>,
    visitor: &V,
) {
    let counters = StackCounters {
        ipv4_common: core_ctx.state.ip_counters::<Ipv4>(),
        ipv6_common: core_ctx.state.ip_counters::<Ipv6>(),
        ipv4: core_ctx.state.ipv4().counters(),
        ipv6: core_ctx.state.ipv6().counters(),
        arp: core_ctx.state.arp_counters(),
        udpv4: core_ctx.state.udp_counters::<Ipv4>(),
        udpv6: core_ctx.state.udp_counters::<Ipv6>(),
        icmpv4_rx: core_ctx.state.icmp_rx_counters::<Ipv4>(),
        icmpv4_tx: core_ctx.state.icmp_tx_counters::<Ipv4>(),
        icmpv6_rx: core_ctx.state.icmp_rx_counters::<Ipv6>(),
        icmpv6_tx: core_ctx.state.icmp_tx_counters::<Ipv6>(),
        ndp: core_ctx.state.ndp_counters(),
        devices: core_ctx.state.device_counters(),
    };
    visitor.visit_counters(counters);
}
