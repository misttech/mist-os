// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IP forwarding definitions.

use alloc::vec::Vec;
use core::{fmt::Debug, slice::Iter};

use log::debug;
use net_types::{
    ip::{Ip, IpAddress, Subnet},
    SpecifiedAddr,
};
use thiserror::Error;

use crate::ip::{
    self,
    types::{AddableMetric, Metric, DEFAULT_INTERFACE_METRIC},
    ExistsError, IpDeviceIdContext, IpLayerContext, IpLayerEvent, IpLayerIpExt,
    IpLayerNonSyncContext,
};

/// Add a route with a gateway to the forwarding table, returning `Err` if the
/// route is already in the table.
pub(crate) fn add_gateway_route<
    I: IpLayerIpExt,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
    SC: IpLayerContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    subnet: Subnet<I::Addr>,
    next_hop: SpecifiedAddr<I::Addr>,
    metric: AddableMetric,
) -> Result<(), AddRouteError> {
    sync_ctx.with_ip_routing_table_mut(|sync_ctx, table| {
        let device = table.lookup(sync_ctx, None, next_hop).map_or(
            Err(AddRouteError::GatewayNotNeighbor),
            |Destination { next_hop: found_next_hop, device }| {
                // If the found route to the `next_hop` has it's own unique
                // next hop, then the given `next_hop` is not a neighbor.
                if next_hop != found_next_hop {
                    Err(AddRouteError::GatewayNotNeighbor)
                } else {
                    Ok(device)
                }
            },
        )?;
        let metric = observe_metric(metric);
        let entry = table
            .add_entry(crate::ip::types::Entry { subnet, device, gateway: Some(next_hop), metric })
            .map_err::<AddRouteError, _>(From::from)?;
        Ok(ctx.on_event(IpLayerEvent::RouteAdded(entry.clone())))
    })
}

/// Add a device route to the forwarding table, returning `Err` if the
/// route is already in the table.
pub(crate) fn add_device_route<
    I: IpLayerIpExt,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
    SC: IpLayerContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    subnet: Subnet<I::Addr>,
    device: SC::DeviceId,
    metric: AddableMetric,
) -> Result<(), ExistsError> {
    sync_ctx.with_ip_routing_table_mut(|_sync_ctx, table| {
        let metric = observe_metric(metric);
        let entry =
            table.add_entry(crate::ip::types::Entry { subnet, device, gateway: None, metric })?;
        Ok(ctx.on_event(IpLayerEvent::RouteAdded(entry.clone())))
    })
}

// Converts the given [`AddableMetric`] into the corresponding [`Metric`],
// observing the device's metric, if applicable.
fn observe_metric(metric: AddableMetric) -> Metric {
    // TODO(https://fxbug.dev/122489): Lookup the interface's metric.
    match metric {
        AddableMetric::ExplicitMetric(value) => Metric::ExplicitMetric(value),
        AddableMetric::MetricTracksInterface => {
            Metric::MetricTracksInterface(DEFAULT_INTERFACE_METRIC)
        }
    }
}

// TODO(joshlf):
// - How do we detect circular routes? Do we attempt to detect at rule
//   installation time? At runtime? Using what algorithm?

/// The destination of an outbound IP packet.
///
/// Outbound IP packets are sent to a particular device (specified by the
/// `device` field). They are sent to a particular IP host on the local network
/// attached to that device, identified by `next_hop`. Note that `next_hop` is
/// not necessarily the destination IP address of the IP packet. In particular,
/// if the destination is not on the local network, the `next_hop` will be the
/// IP address of the next IP router on the way to the destination.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Destination<A: IpAddress, D> {
    pub(crate) next_hop: SpecifiedAddr<A>,
    pub(crate) device: D,
}

/// An error encountered when adding a forwarding entry.
#[derive(Error, Debug, PartialEq)]
pub enum AddRouteError {
    /// Indicates that the route already exists.
    #[error("Already exists")]
    AlreadyExists,

    /// Indicates the gateway is not a neighbor of the host.
    #[error("Gateway is not a neighbor")]
    GatewayNotNeighbor,
}

impl From<crate::error::ExistsError> for AddRouteError {
    fn from(crate::error::ExistsError: crate::error::ExistsError) -> AddRouteError {
        AddRouteError::AlreadyExists
    }
}

/// An IP forwarding table.
///
/// `ForwardingTable` maps destination subnets to the nearest IP hosts (on the
/// local network) able to route IP packets to those subnets.
// TODO(https://fxbug.dev/122489): Consider Route Metrics when making routing
// decisions.
pub struct ForwardingTable<I: Ip, D> {
    /// All the routes available to forward a packet.
    ///
    /// `table` may have redundant, but unique, paths to the same
    /// destination.
    ///
    /// The entries are sorted based on the subnet's prefix length and
    /// local-ness; when there's a tie in prefix length, on-linkness breaks the
    /// tie (with the on-link routes appearing before off-link ones).
    table: Vec<ip::types::Entry<I::Addr, D>>,
}

impl<I: Ip, D> Default for ForwardingTable<I, D> {
    fn default() -> ForwardingTable<I, D> {
        ForwardingTable { table: Vec::new() }
    }
}

impl<I: Ip, D: Clone + Debug + PartialEq> ForwardingTable<I, D> {
    /// Adds `entry` to the forwarding table if it does not already exist.
    ///
    /// On success, a reference to the inserted entry is returned.
    fn add_entry(
        &mut self,
        entry: ip::types::Entry<I::Addr, D>,
    ) -> Result<&ip::types::Entry<I::Addr, D>, crate::error::ExistsError> {
        debug!("adding route: {}", entry);
        let Self { table } = self;

        if table.contains(&entry) {
            // If we already have this exact route, don't add it again.
            return Err(crate::error::ExistsError);
        }

        // Insert the new entry after the last route to a more specific and/or
        // local subnet to maintain the invariant that the table is sorted by
        // subnet prefix length and local-ness.
        // TODO(https://fxbug.dev/122489): Consider Route Metrics when sorting.
        let ip::types::Entry { subnet, device: _, gateway: _, metric: _ } = entry;
        let prefix = subnet.prefix();
        let index =
            table.partition_point(|ip::types::Entry { subnet, device: _, gateway, metric: _ }| {
                let subnet_prefix = subnet.prefix();
                subnet_prefix > prefix
                    || (subnet_prefix == prefix
                        && match gateway {
                            Some(SpecifiedAddr { .. }) => false,
                            // on-link routes have a higher preference.
                            None => true,
                        })
            });

        table.insert(index, entry);

        Ok(&table[index])
    }

    // Applies the given predicate to the entries in the forwarding table,
    // removing (and returning) those that yield `true` while retaining those
    // that yield `false`.
    fn del_routes<F: Fn(&ip::types::Entry<I::Addr, D>) -> bool>(
        &mut self,
        predicate: F,
    ) -> Vec<ip::types::Entry<I::Addr, D>> {
        // TODO(https://github.com/rust-lang/rust/issues/43244): Use
        // drain_filter to avoid extra allocation.
        let Self { table } = self;
        let owned_table = core::mem::replace(table, Vec::new());
        let (removed, owned_table) = owned_table.into_iter().partition(|entry| predicate(entry));
        *table = owned_table;
        removed
    }

    /// Delete all routes to a subnet, returning `Err` if no route was found to
    /// be deleted.
    ///
    /// Returns all the deleted entries on success.
    ///
    /// Note, `del_subnet_routes` will remove *all* routes to a `subnet`,
    /// including routes that consider `subnet` on-link for some device and
    /// routes that require packets destined to a node within `subnet` to be
    /// routed through some next-hop node.
    pub(crate) fn del_subnet_routes(
        &mut self,
        subnet: Subnet<I::Addr>,
    ) -> Result<Vec<ip::types::Entry<I::Addr, D>>, crate::error::NotFoundError> {
        debug!("deleting routes to subnet: {}", subnet);
        let removed =
            self.del_routes(|entry: &ip::types::Entry<I::Addr, D>| entry.subnet == subnet);
        if removed.is_empty() {
            // If a path to `subnet` was not in our installed table, then it
            // definitely won't be in our active routes cache.
            return Err(crate::error::NotFoundError);
        }
        Ok(removed)
    }

    /// Delete all routes on a device.
    ///
    /// Unlike ['del_subnet_routes'], this function always succeeds, even if
    /// there are no device routes.
    pub(crate) fn del_device_routes(&mut self, device: D) -> Vec<ip::types::Entry<I::Addr, D>> {
        debug!("deleting routes on device: {:?}", device);
        self.del_routes(|entry: &ip::types::Entry<I::Addr, D>| entry.device == device)
    }

    /// Get an iterator over all of the forwarding entries ([`Entry`]) this
    /// `ForwardingTable` knows about.
    pub(crate) fn iter_table(&self) -> Iter<'_, ip::types::Entry<I::Addr, D>> {
        self.table.iter()
    }

    /// Look up an address in the table.
    ///
    /// Look up an IP address in the table, returning a next hop IP address and
    /// a device to send over. If `address` is link-local, then the returned
    /// next hop will be `address`. Otherwise, it will be the link-local address
    /// of an IP router capable of delivering packets to `address`.
    ///
    /// If `device` is specified, the available routes are limited to those that
    /// egress over the device.
    ///
    /// If `address` matches an entry which maps to an IP address, `lookup` will
    /// look that address up in the table as well, continuing until a link-local
    /// address and device are found.
    ///
    /// If multiple entries match `address` or any intermediate IP address, the
    /// entry with the longest prefix will be chosen.
    ///
    /// The unspecified address (0.0.0.0 in IPv4 and :: in IPv6) are not
    /// routable and will return None even if they have been added to the table.
    pub(crate) fn lookup<SC: IpDeviceIdContext<I, DeviceId = D>>(
        &self,
        sync_ctx: &mut SC,
        local_device: Option<&D>,
        address: SpecifiedAddr<I::Addr>,
    ) -> Option<Destination<I::Addr, D>> {
        let Self { table } = self;

        // Get all potential routes we could take to reach `address`.
        table.iter().find_map(|ip::types::Entry { subnet, device, gateway, metric: _ }| {
            (subnet.contains(&address)
                && local_device.map_or(true, |d| d == device)
                && sync_ctx.is_device_installed(device))
            .then(|| {
                let next_hop =
                    if let Some(next_hop) = gateway { next_hop.clone() } else { address };

                Destination { next_hop, device: device.clone() }
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    // Provide tests with access to the private `ForwardingTable.add_entry` fn.
    pub(crate) fn add_entry<I: Ip, D: Clone + Debug + PartialEq>(
        table: &mut ForwardingTable<I, D>,
        entry: crate::ip::types::Entry<I::Addr, D>,
    ) -> Result<&crate::ip::types::Entry<I::Addr, D>, crate::error::ExistsError> {
        table.add_entry(entry)
    }
}
#[cfg(test)]
mod tests {
    use fakealloc::collections::HashSet;
    use ip_test_macro::ip_test;
    use log::trace;
    use net_types::ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};

    use super::*;
    use crate::{
        context::testutil::FakeSyncCtx,
        ip::{
            testutil::{FakeIpDeviceIdCtx, MultipleDevicesId},
            types::{Metric, RawMetric},
        },
        testutil::FakeEventDispatcherConfig,
    };

    #[derive(Default)]
    struct FakeForwardingContext<I> {
        ip_device_id_ctx: FakeIpDeviceIdCtx<I, MultipleDevicesId>,
    }

    impl<I> AsRef<FakeIpDeviceIdCtx<I, MultipleDevicesId>> for FakeForwardingContext<I> {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<I, MultipleDevicesId> {
            &self.ip_device_id_ctx
        }
    }

    type FakeCtx<I> = FakeSyncCtx<FakeForwardingContext<I>, (), MultipleDevicesId>;

    impl<I: Ip, D: Clone + Debug + PartialEq> ForwardingTable<I, D> {
        /// Print the table.
        fn print_table(&self) {
            trace!("Installed Routing table:");

            if self.table.is_empty() {
                trace!("    No Routes");
                return;
            }

            for entry in self.iter_table() {
                trace!("    {}", entry)
            }
        }
    }

    trait TestIpExt: crate::testutil::TestIpExt {
        fn subnet(v: u8, neg_prefix: u8) -> Subnet<Self::Addr>;

        fn next_hop_addr_sub(
            v: u8,
            neg_prefix: u8,
        ) -> (SpecifiedAddr<Self::Addr>, Subnet<Self::Addr>);

        fn next_hop_addr() -> SpecifiedAddr<Self::Addr>;
    }

    impl TestIpExt for Ipv4 {
        fn subnet(v: u8, neg_prefix: u8) -> Subnet<Ipv4Addr> {
            Subnet::new(Ipv4Addr::new([v, 0, 0, 0]), 32 - neg_prefix).unwrap()
        }

        fn next_hop_addr_sub(v: u8, neg_prefix: u8) -> (SpecifiedAddr<Ipv4Addr>, Subnet<Ipv4Addr>) {
            (SpecifiedAddr::new(Ipv4Addr::new([v, 0, 0, 1])).unwrap(), Ipv4::subnet(v, neg_prefix))
        }

        fn next_hop_addr() -> SpecifiedAddr<Ipv4Addr> {
            SpecifiedAddr::new(Ipv4Addr::new([10, 0, 0, 1])).unwrap()
        }
    }

    impl TestIpExt for Ipv6 {
        fn subnet(v: u8, neg_prefix: u8) -> Subnet<Ipv6Addr> {
            Subnet::new(
                Ipv6Addr::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, v, 0, 0, 0]),
                128 - neg_prefix,
            )
            .unwrap()
        }

        fn next_hop_addr_sub(v: u8, neg_prefix: u8) -> (SpecifiedAddr<Ipv6Addr>, Subnet<Ipv6Addr>) {
            (
                SpecifiedAddr::new(Ipv6Addr::from([
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, v, 0, 0, 1,
                ]))
                .unwrap(),
                Ipv6::subnet(v, neg_prefix),
            )
        }

        fn next_hop_addr() -> SpecifiedAddr<Ipv6Addr> {
            SpecifiedAddr::new(Ipv6Addr::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 1]))
                .unwrap()
        }
    }

    fn simple_setup<I: Ip + TestIpExt>() -> (
        ForwardingTable<I, MultipleDevicesId>,
        FakeEventDispatcherConfig<I::Addr>,
        SpecifiedAddr<I::Addr>,
        Subnet<I::Addr>,
        MultipleDevicesId,
        Metric,
    ) {
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();

        let config = I::FAKE_CONFIG;
        let subnet = config.subnet;
        let device = MultipleDevicesId::A;
        let (next_hop, next_hop_subnet) = I::next_hop_addr_sub(1, 1);
        let metric = Metric::ExplicitMetric(RawMetric(9999));

        // Should add the route successfully.
        let entry = ip::types::Entry { subnet, device: device.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry.clone()), Ok(&entry));
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry]);

        // Attempting to add the route again should fail.
        assert_eq!(table.add_entry(entry.clone()).unwrap_err(), crate::error::ExistsError);
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry]);

        // Add the route but as a next hop route.
        let entry2 = ip::types::Entry {
            subnet: next_hop_subnet,
            device: device.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(entry2.clone()), Ok(&entry2));
        let entry3 = ip::types::Entry {
            subnet: subnet,
            device: device.clone(),
            gateway: Some(next_hop),
            metric,
        };
        assert_eq!(table.add_entry(entry3.clone()), Ok(&entry3));
        assert_eq!(
            table.iter_table().collect::<HashSet<_>>(),
            HashSet::from([&entry, &entry2, &entry3])
        );

        // Attempting to add the route again should fail.
        assert_eq!(table.add_entry(entry3.clone()).unwrap_err(), crate::error::ExistsError);
        assert_eq!(
            table.iter_table().collect::<HashSet<_>>(),
            HashSet::from([&entry, &entry2, &entry3,])
        );

        (table, config, next_hop, next_hop_subnet, device, metric)
    }

    #[ip_test]
    fn test_simple_add_del<I: Ip + TestIpExt>() {
        let (mut table, config, next_hop, next_hop_subnet, device, metric) = simple_setup::<I>();
        assert_eq!(table.iter_table().count(), 3);

        // Delete all routes to subnet.
        assert_eq!(
            table.del_subnet_routes(config.subnet).unwrap().into_iter().collect::<HashSet<_>>(),
            HashSet::from([
                ip::types::Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: None,
                    metric
                },
                ip::types::Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: Some(next_hop),
                    metric
                }
            ])
        );

        assert_eq!(
            table.iter_table().collect::<Vec<_>>(),
            &[&ip::types::Entry {
                subnet: next_hop_subnet,
                device: device.clone(),
                gateway: None,
                metric
            }]
        );
    }

    #[ip_test]
    fn test_delete_device_routes<I: Ip + TestIpExt>() {
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();

        let config = I::FAKE_CONFIG;
        let subnet = config.subnet;
        let device_a = MultipleDevicesId::A;
        let device_b = MultipleDevicesId::B;
        let metric = Metric::ExplicitMetric(RawMetric(0));

        // Should add the routes successfully.
        let entry_a = ip::types::Entry { subnet, device: device_a.clone(), gateway: None, metric };
        let entry_b = ip::types::Entry { subnet, device: device_b.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry_a.clone()), Ok(&entry_a));
        assert_eq!(table.add_entry(entry_b.clone()), Ok(&entry_b));
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry_a, &entry_b]);

        assert_eq!(
            table.del_device_routes(device_a.clone()).into_iter().collect::<Vec<_>>(),
            &[entry_a]
        );

        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry_b]);
    }

    #[ip_test]
    fn test_simple_lookup<I: Ip + TestIpExt>() {
        let (mut table, config, next_hop, _next_hop_subnet, device, metric) = simple_setup::<I>();
        let mut sync_ctx = FakeCtx::<I>::default();

        // Do lookup for our next hop (should be the device).
        assert_eq!(
            table.lookup(&mut sync_ctx, None, next_hop),
            Some(Destination { next_hop, device: device.clone() })
        );

        // Do lookup for some address within `subnet`.
        assert_eq!(
            table.lookup(&mut sync_ctx, None, config.local_ip),
            Some(Destination { next_hop: config.local_ip, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, config.remote_ip),
            Some(Destination { next_hop: config.remote_ip, device: device.clone() })
        );

        // Delete routes to the subnet and make sure that we can no longer route
        // to destinations in the subnet.
        assert_eq!(
            table.del_subnet_routes(config.subnet).unwrap().into_iter().collect::<HashSet<_>>(),
            HashSet::from([
                ip::types::Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: None,
                    metric
                },
                ip::types::Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: Some(next_hop),
                    metric
                }
            ])
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, next_hop),
            Some(Destination { next_hop, device: device.clone() })
        );
        assert_eq!(table.lookup(&mut sync_ctx, None, config.local_ip), None);
        assert_eq!(table.lookup(&mut sync_ctx, None, config.remote_ip), None);

        // Make the subnet routable again but through a gateway.
        let gateway_entry = ip::types::Entry {
            subnet: config.subnet,
            device: device.clone(),
            gateway: Some(next_hop),
            metric: Metric::ExplicitMetric(RawMetric(0)),
        };
        assert_eq!(table.add_entry(gateway_entry.clone()), Ok(&gateway_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, next_hop),
            Some(Destination { next_hop, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, config.local_ip),
            Some(Destination { next_hop, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, config.remote_ip),
            Some(Destination { next_hop, device: device.clone() })
        );
    }

    #[ip_test]
    fn test_default_route_ip<I: Ip + TestIpExt>() {
        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let device0 = MultipleDevicesId::A;
        let (addr1, sub1) = I::next_hop_addr_sub(1, 24);
        let (addr2, _) = I::next_hop_addr_sub(2, 24);
        let (addr3, _) = I::next_hop_addr_sub(3, 24);
        let metric = Metric::ExplicitMetric(RawMetric(0));

        // Add the following routes:
        //  sub1 -> device0
        //
        // Our expected forwarding table should look like:
        //  sub1 -> device0

        let entry =
            ip::types::Entry { subnet: sub1, device: device0.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry.clone()), Ok(&entry));
        table.print_table();
        assert_eq!(
            table.lookup(&mut sync_ctx, None, addr1).unwrap(),
            Destination { next_hop: addr1, device: device0.clone() }
        );
        assert_eq!(table.lookup(&mut sync_ctx, None, addr2), None);

        // Add a default route.
        //
        // Our expected forwarding table should look like:
        //  sub1 -> device0
        //  default -> addr1 w/ device0

        let default_sub = Subnet::new(I::UNSPECIFIED_ADDRESS, 0).unwrap();
        let default_entry = ip::types::Entry {
            subnet: default_sub,
            device: device0.clone(),
            gateway: Some(addr1),
            metric,
        };

        assert_eq!(table.add_entry(default_entry.clone()), Ok(&default_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, addr1).unwrap(),
            Destination { next_hop: addr1, device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, addr2).unwrap(),
            Destination { next_hop: addr1, device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, addr3).unwrap(),
            Destination { next_hop: addr1, device: device0.clone() }
        );
    }

    #[ip_test]
    fn test_device_filter_with_varying_prefix_lengths<I: Ip + TestIpExt>() {
        const MORE_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        const LESS_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::B;

        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let (next_hop, more_specific_sub) = I::next_hop_addr_sub(1, 1);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 2);
            assert_eq!(next_hop, addr);
            sub
        };
        let metric = Metric::ExplicitMetric(RawMetric(0));
        let less_specific_entry = ip::types::Entry {
            subnet: less_specific_sub,
            device: LESS_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(less_specific_entry.clone()), Ok(&less_specific_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, next_hop),
            Some(Destination { next_hop, device: LESS_SPECIFIC_SUB_DEVICE.clone() }),
            "matches route"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&LESS_SPECIFIC_SUB_DEVICE), next_hop),
            Some(Destination { next_hop, device: LESS_SPECIFIC_SUB_DEVICE.clone() }),
            "route matches specified device"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&MORE_SPECIFIC_SUB_DEVICE), next_hop),
            None,
            "no route with the specified device"
        );

        let more_specific_entry = ip::types::Entry {
            subnet: more_specific_sub,
            device: MORE_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(more_specific_entry.clone()), Ok(&more_specific_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, next_hop).unwrap(),
            Destination { next_hop, device: MORE_SPECIFIC_SUB_DEVICE.clone() },
            "matches most specific route"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&LESS_SPECIFIC_SUB_DEVICE), next_hop),
            Some(Destination { next_hop, device: LESS_SPECIFIC_SUB_DEVICE.clone() }),
            "matches less specific route with the specified device"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&MORE_SPECIFIC_SUB_DEVICE), next_hop).unwrap(),
            Destination { next_hop, device: MORE_SPECIFIC_SUB_DEVICE.clone() },
            "matches the most specific route with the specified device"
        );
    }

    #[ip_test]
    fn test_multiple_routes_to_subnet_through_different_devices<I: Ip + TestIpExt>() {
        const DEVICE1: MultipleDevicesId = MultipleDevicesId::A;
        const DEVICE2: MultipleDevicesId = MultipleDevicesId::B;

        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let (next_hop, sub) = I::next_hop_addr_sub(1, 1);
        let metric = Metric::ExplicitMetric(RawMetric(0));

        let entry1 =
            ip::types::Entry { subnet: sub, device: DEVICE1.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry1.clone()), Ok(&entry1));
        let entry2 =
            ip::types::Entry { subnet: sub, device: DEVICE2.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry2.clone()), Ok(&entry2));
        let lookup = table.lookup(&mut sync_ctx, None, next_hop);
        assert!(
            [
                Some(Destination { next_hop, device: DEVICE1.clone() }),
                Some(Destination { next_hop, device: DEVICE2.clone() })
            ]
            .contains(&lookup),
            "lookup = {:?}",
            lookup
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&DEVICE1), next_hop),
            Some(Destination { next_hop, device: DEVICE1.clone() }),
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&DEVICE2), next_hop),
            Some(Destination { next_hop, device: DEVICE2.clone() }),
        );
    }

    #[ip_test]
    fn test_use_active_device<I: Ip + TestIpExt>() {
        const MORE_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        const LESS_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::B;

        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let (next_hop, more_specific_sub) = I::next_hop_addr_sub(1, 1);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 2);
            assert_eq!(next_hop, addr);
            sub
        };
        let metric = Metric::ExplicitMetric(RawMetric(0));

        let less_specific_entry = ip::types::Entry {
            subnet: less_specific_sub,
            device: LESS_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(less_specific_entry.clone()), Ok(&less_specific_entry));
        for (device_removed, expected) in [
            // If the device is removed, then we cannot use routes through it.
            (true, None),
            (false, Some(Destination { next_hop, device: LESS_SPECIFIC_SUB_DEVICE.clone() })),
        ] {
            sync_ctx
                .get_mut()
                .ip_device_id_ctx
                .set_device_removed(LESS_SPECIFIC_SUB_DEVICE, device_removed);
            assert_eq!(
                table.lookup(&mut sync_ctx, None, next_hop),
                expected,
                "device_removed={}",
                device_removed,
            );
        }

        let more_specific_entry = ip::types::Entry {
            subnet: more_specific_sub,
            device: MORE_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(more_specific_entry.clone()), Ok(&more_specific_entry));
        for (device_removed, expected) in [
            (false, Some(Destination { next_hop, device: MORE_SPECIFIC_SUB_DEVICE.clone() })),
            // If the device is removed, then we cannot use routes through it,
            // but can use routes through other (active) devices.
            (true, Some(Destination { next_hop, device: LESS_SPECIFIC_SUB_DEVICE.clone() })),
        ] {
            sync_ctx
                .get_mut()
                .ip_device_id_ctx
                .set_device_removed(MORE_SPECIFIC_SUB_DEVICE, device_removed);
            assert_eq!(
                table.lookup(&mut sync_ctx, None, next_hop),
                expected,
                "device_removed={}",
                device_removed,
            );
        }

        // If no devices exist, then we can't get a route.
        sync_ctx.get_mut().ip_device_id_ctx.set_device_removed(LESS_SPECIFIC_SUB_DEVICE, true);
        assert_eq!(table.lookup(&mut sync_ctx, None, next_hop), None,);
    }
}
