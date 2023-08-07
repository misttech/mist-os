// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common types for dealing with ip table entries.

use core::fmt::{Debug, Display, Formatter};

use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpInvariant, Ipv4Addr, Ipv6Addr, Subnet, SubnetEither},
    SpecifiedAddr,
};

/// The priority of a forwarding entry. Lower metrics are preferred.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct RawMetric(pub u32);

impl Display for RawMetric {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), core::fmt::Error> {
        let RawMetric(metric) = self;
        write!(f, "{}", metric)
    }
}

impl From<RawMetric> for u32 {
    fn from(RawMetric(metric): RawMetric) -> u32 {
        metric
    }
}

impl From<RawMetric> for u64 {
    fn from(RawMetric(metric): RawMetric) -> u64 {
        u64::from(metric)
    }
}

/// The default interface routing metric.
pub const DEFAULT_INTERFACE_METRIC: RawMetric = RawMetric(100);

/// The metric for an [`AddableEntry`].
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum AddableMetric {
    /// The entry's metric is unspecified, and shall defer to the routing metric
    /// of its interface.
    MetricTracksInterface,
    /// The entry's metric shall be the following explicit value.
    ExplicitMetric(RawMetric),
}

/// `AddableEntry` is a routing entry that may be used to add a new entry to the
/// forwarding table.
///
/// See [`Entry`] for the type used to represent a route in the forwarding
/// table.
///
/// `AddableEntry` guarantees that at least one of the egress device or
/// gateway is set.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct AddableEntry<A: IpAddress, D> {
    subnet: Subnet<A>,
    device: Option<D>,
    gateway: Option<SpecifiedAddr<A>>,
    metric: AddableMetric,
}

// A mirror image of `AddableEntry` whose fields are public, allowing the entry
// to be decomposed into its constituent parts.
pub(super) struct DecomposedAddableEntry<A: IpAddress, D> {
    pub(super) subnet: Subnet<A>,
    pub(super) device: Option<D>,
    pub(super) gateway: Option<SpecifiedAddr<A>>,
    pub(super) metric: AddableMetric,
}

impl<D, A: IpAddress> AddableEntry<A, D> {
    /// Creates a new [`AddableEntry`] with a specified gateway.
    pub fn with_gateway(
        subnet: Subnet<A>,
        device: Option<D>,
        gateway: SpecifiedAddr<A>,
        metric: AddableMetric,
    ) -> Self {
        Self { subnet, device, gateway: Some(gateway), metric }
    }

    /// Creates a new [`AddableEntry`] with a specified device.
    pub fn without_gateway(subnet: Subnet<A>, device: D, metric: AddableMetric) -> Self {
        Self { subnet, device: Some(device), gateway: None, metric }
    }

    /// Convert the [`AddableEntry`] into a [`DecomposedAddableEntry`].
    pub(super) fn decompose(self) -> DecomposedAddableEntry<A, D> {
        let AddableEntry { subnet, device, gateway, metric } = self;
        DecomposedAddableEntry { subnet, device, gateway, metric }
    }
}

/// An IPv4 forwarding entry or an IPv6 forwarding entry.
#[allow(missing_docs)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum AddableEntryEither<D> {
    V4(AddableEntry<Ipv4Addr, D>),
    V6(AddableEntry<Ipv6Addr, D>),
}

impl<D> AddableEntryEither<D> {
    /// Creates a new [`AddableEntryEither`] with the specified device as the
    /// next hop.
    pub fn without_gateway(subnet: SubnetEither, device: D, metric: AddableMetric) -> Self {
        match subnet {
            SubnetEither::V4(subnet) => {
                AddableEntry::without_gateway(subnet, device, metric).into()
            }
            SubnetEither::V6(subnet) => {
                AddableEntry::without_gateway(subnet, device, metric).into()
            }
        }
    }
}

impl<A: IpAddress, D> From<AddableEntry<A, D>> for AddableEntryEither<D> {
    fn from(entry: AddableEntry<A, D>) -> AddableEntryEither<D> {
        #[derive(GenericOverIp)]
        struct EntryHolder<I: Ip, D>(AddableEntry<I::Addr, D>);
        let IpInvariant(entry_either) = A::Version::map_ip(
            EntryHolder(entry),
            |EntryHolder(entry)| IpInvariant(AddableEntryEither::V4(entry)),
            |EntryHolder(entry)| IpInvariant(AddableEntryEither::V6(entry)),
        );
        entry_either
    }
}

/// The metric for an [`Entry`].
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum Metric {
    /// The entry's metric tracks its interface's routing metric and has the
    /// included value.
    MetricTracksInterface(RawMetric),
    /// The entry's metric was explicitly set to the included value.
    ExplicitMetric(RawMetric),
}

impl Metric {
    /// Returns the underlying value of the `Metric`.
    pub fn value(&self) -> RawMetric {
        match self {
            Self::MetricTracksInterface(value) => *value,
            Self::ExplicitMetric(value) => *value,
        }
    }
}

/// A forwarding entry.
///
/// `Entry` is a `Subnet` with an egress device and optional gateway.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Entry<A: IpAddress, D> {
    /// The matching subnet.
    pub subnet: Subnet<A>,
    /// The destination device.
    pub device: D,
    /// An optional gateway if the subnet is not on link.
    // TODO (https://fxbug.dev/123288): Restrict `gateway` to `UnicastAddr`.
    pub gateway: Option<SpecifiedAddr<A>>,
    /// The metric of the entry.
    pub metric: Metric,
}

impl<A: IpAddress, D: Debug> Display for Entry<A, D> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), core::fmt::Error> {
        let Entry { subnet, device, gateway, metric } = self;
        match gateway {
            Some(gateway) => {
                write!(f, "{:?} (via {}) -> {} metric {}", device, gateway, subnet, metric.value())
            }
            None => write!(f, "{:?} -> {} metric {}", device, subnet, metric.value()),
        }
    }
}

/// An IPv4 forwarding entry or an IPv6 forwarding entry.
#[allow(missing_docs)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum EntryEither<D> {
    V4(Entry<Ipv4Addr, D>),
    V6(Entry<Ipv6Addr, D>),
}

impl<A: IpAddress, D> From<Entry<A, D>> for EntryEither<D> {
    fn from(entry: Entry<A, D>) -> EntryEither<D> {
        #[derive(GenericOverIp)]
        struct EntryHolder<I: Ip, D>(Entry<I::Addr, D>);
        let IpInvariant(entry_either) = A::Version::map_ip(
            EntryHolder(entry),
            |EntryHolder(entry)| IpInvariant(EntryEither::V4(entry)),
            |EntryHolder(entry)| IpInvariant(EntryEither::V6(entry)),
        );
        entry_either
    }
}

/// The next hop for a [`Destination`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NextHop<A> {
    /// Indicates that the next-hop for a the packet is the remote since it is a
    /// neighboring node (on-link).
    RemoteAsNeighbor,
    /// Indicates that the next-hop is a gateway/router since the remote is not
    /// a neighboring node (off-link).
    Gateway(SpecifiedAddr<A>),
}

/// The resolved route to a destination IP address.
#[derive(Debug, Copy, Clone, PartialEq, Eq, GenericOverIp)]
pub struct ResolvedRoute<I: Ip, D> {
    /// The source address to use when forwarding packets towards the
    /// destination.
    pub src_addr: SpecifiedAddr<I::Addr>,
    /// The device over which this destination can be reached.
    pub device: D,
    /// The next hop via which this destination can be reached.
    pub next_hop: NextHop<I::Addr>,
}

/// The destination of an outbound IP packet.
///
/// Outbound IP packets are sent to a particular device (specified by the
/// `device` field).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Destination<A, D> {
    /// Indicates the next hop via which this destination can be reached.
    pub(crate) next_hop: NextHop<A>,
    /// Indicates the device over which this destination can be reached.
    pub(crate) device: D,
}
