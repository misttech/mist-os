// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IP forwarding definitions.

use alloc::vec::Vec;
use core::{fmt::Debug, slice::Iter};

use net_types::{
    ip::{Ip, Subnet},
    SpecifiedAddr,
};
use thiserror::Error;
use tracing::debug;

use crate::{
    error::NotFoundError,
    ip::{
        types::{
            AddableEntry, AddableMetric, DecomposedAddableEntry, Destination, Entry, Metric,
            NextHop, RawMetric,
        },
        AnyDevice, DeviceIdContext, IpLayerEvent, IpLayerIpExt, IpLayerNonSyncContext,
        IpStateContext,
    },
};

/// Provides access to a device for the purposes of IP forwarding.
pub(crate) trait IpForwardingDeviceContext: DeviceIdContext<AnyDevice> {
    /// Returns the routing metric for the device.
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric;
}

/// An error encountered when adding a forwarding entry.
#[derive(Error, Debug, PartialEq)]
pub enum AddRouteError {
    /// Indicates that the route already exists.
    #[error("Already exists")]
    AlreadyExists,

    /// Indicates the gateway is not a neighbor of this node.
    #[error("Gateway is not a neighbor")]
    GatewayNotNeighbor,
}

impl From<crate::error::ExistsError> for AddRouteError {
    fn from(crate::error::ExistsError: crate::error::ExistsError) -> AddRouteError {
        AddRouteError::AlreadyExists
    }
}

/// Add a route to the forwarding table.
pub(crate) fn add_route<
    I: IpLayerIpExt,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
    SC: IpStateContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    entry: AddableEntry<I::Addr, SC::DeviceId>,
) -> Result<(), AddRouteError> {
    let DecomposedAddableEntry { subnet, device, gateway, metric } = entry.decompose();
    sync_ctx.with_ip_routing_table_mut(|sync_ctx, table| {
        let (device, gateway) = match (device, gateway) {
            (None, None) => unreachable!("AddableEntry must have device or gateway set"),
            (Some(device), None) => (device, None),
            (device, Some(gateway)) => {
                // Verify that the routing table has an on-link route to the
                // gateway via the specified device.
                table.lookup(sync_ctx, device.as_ref(), *gateway).map_or(
                    Err(AddRouteError::GatewayNotNeighbor),
                    |Destination { next_hop: found_next_hop, device: found_device }| {
                        match found_next_hop {
                            NextHop::RemoteAsNeighbor => {
                                if let Some(given_device) = device {
                                    // We restricted the above lookup by the
                                    // given device, so the found device must be
                                    // equal.
                                    assert!(found_device == given_device);
                                }
                                Ok((found_device, Some(gateway)))
                            }
                            NextHop::Gateway(_intermediary_gateway) => {
                                Err(AddRouteError::GatewayNotNeighbor)
                            }
                        }
                    },
                )?
            }
        };
        let metric = observe_metric(sync_ctx, &device, metric);
        let entry = table.add_entry(Entry { subnet, device, gateway, metric })?;
        Ok(ctx.on_event(IpLayerEvent::RouteAdded(entry.clone())))
    })
}

fn del_entries<
    I: IpLayerIpExt,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
    SC: IpStateContext<I, C>,
    F: Fn(&Entry<I::Addr, SC::DeviceId>) -> bool,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    predicate: F,
) -> Result<(), NotFoundError> {
    sync_ctx.with_ip_routing_table_mut(|_sync_ctx, table| {
        let removed = table.del_entries(predicate);
        if removed.is_empty() {
            Err(NotFoundError)
        } else {
            Ok(removed
                .into_iter()
                .for_each(|entry| ctx.on_event(IpLayerEvent::RouteRemoved(entry))))
        }
    })
}

/// Delete all routes to a subnet, returning `Err` if no route was found to
/// be deleted.
///
/// Note, `del_subnet_routes` will remove *all* routes to a `subnet`,
/// including routes that consider `subnet` on-link for some device and
/// routes that require packets destined to a node within `subnet` to be
/// routed through some next-hop node.
// TODO(https://fxbug.dev/126729): Unify this with other route removal methods.
pub(crate) fn del_subnet_route<
    I: IpLayerIpExt,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
    SC: IpStateContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    del_subnet: Subnet<I::Addr>,
) -> Result<(), NotFoundError> {
    debug!("deleting routes to subnet: {del_subnet}");
    del_entries(sync_ctx, ctx, |Entry { subnet, device: _, gateway: _, metric: _ }| {
        subnet == &del_subnet
    })
}

/// Delete all routes on a device.
///
/// Unlike ['del_subnet_routes'], this function always succeeds, even if
/// there are no device routes.
// TODO(https://fxbug.dev/126729): Unify this with other route removal methods.
pub(crate) fn del_device_routes<
    I: IpLayerIpExt,
    SC: IpStateContext<I, C>,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    del_device: &SC::DeviceId,
) {
    debug!("deleting routes on device: {del_device:?}");
    let _: Result<(), NotFoundError> =
        del_entries(sync_ctx, ctx, |Entry { subnet: _, device, gateway: _, metric: _ }| {
            device == del_device
        });
}

/// Deletes all routes matching the provided specifiers exactly.
///
/// Returns all the deleted routes, or an error if no routes were deleted.
// TODO(https://fxbug.dev/126729): Unify this with other route removal methods.
pub(super) fn del_specific_routes<
    I: IpLayerIpExt,
    C: IpLayerNonSyncContext<I, SC::DeviceId>,
    SC: IpStateContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    del_subnet: Subnet<I::Addr>,
    del_device: &SC::DeviceId,
    del_gateway: Option<SpecifiedAddr<I::Addr>>,
) -> Result<(), NotFoundError> {
    debug!(
        "deleting routes with subnet={del_subnet} device={del_device:?} gateway={del_gateway:?}",
    );
    del_entries(sync_ctx, ctx, |Entry { subnet, device, gateway, metric: _ }| {
        device == del_device && subnet == &del_subnet && gateway == &del_gateway
    })
}

// Converts the given [`AddableMetric`] into the corresponding [`Metric`],
// observing the device's metric, if applicable.
fn observe_metric<SC: IpForwardingDeviceContext>(
    sync_ctx: &mut SC,
    device: &SC::DeviceId,
    metric: AddableMetric,
) -> Metric {
    match metric {
        AddableMetric::ExplicitMetric(value) => Metric::ExplicitMetric(value),
        AddableMetric::MetricTracksInterface => {
            Metric::MetricTracksInterface(sync_ctx.get_routing_metric(device))
        }
    }
}

/// An IP forwarding table.
///
/// `ForwardingTable` maps destination subnets to the nearest IP hosts (on the
/// local network) able to route IP packets to those subnets.
pub struct ForwardingTable<I: Ip, D> {
    /// All the routes available to forward a packet.
    ///
    /// `table` may have redundant, but unique, paths to the same
    /// destination.
    ///
    /// Entries in the table are sorted from most-preferred to least preferred.
    /// Preference is determined first by longest prefix, then by lowest metric,
    /// then by locality (prefer on-link routes over off-link routes), and
    /// finally by the entry's tenure in the table.
    table: Vec<Entry<I::Addr, D>>,
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
        entry: Entry<I::Addr, D>,
    ) -> Result<&Entry<I::Addr, D>, crate::error::ExistsError> {
        debug!("adding route: {}", entry);
        let Self { table } = self;

        if table.contains(&entry) {
            // If we already have this exact route, don't add it again.
            return Err(crate::error::ExistsError);
        }

        // `OrderedLocality` provides an implementation of
        // `core::cmp::PartialOrd` for a routes "locality". Define an enum, so
        // that `OnLink` routes are sorted before `OffLink` routes. See
        // https://doc.rust-lang.org/std/cmp/trait.PartialOrd.html#derivable
        // for more details.
        #[derive(PartialEq, PartialOrd)]
        enum OrderedLocality {
            // The route does not have a gateway.
            OnLink,
            // The route does have a gateway.
            OffLink,
        }
        // `OrderedRoute` provides an implementation of `std::cmp::PartialOrd`
        // for routes. Note that the fields are consulted in the order they are
        // declared. For more details, see
        // https://doc.rust-lang.org/std/cmp/trait.PartialOrd.html#derivable.
        #[derive(PartialEq, PartialOrd)]
        struct OrderedEntry {
            // Order longer prefixes before shorter prefixes.
            prefix_len: core::cmp::Reverse<u8>,
            // Order lower metrics before larger metrics.
            metric: u32,
            // Order `OnLink` routes before `OffLink` routes.
            locality: OrderedLocality,
        }
        impl<A: net_types::ip::IpAddress, D> From<&Entry<A, D>> for OrderedEntry {
            fn from(entry: &Entry<A, D>) -> OrderedEntry {
                let Entry { subnet, device: _, gateway, metric } = entry;
                OrderedEntry {
                    prefix_len: core::cmp::Reverse(subnet.prefix()),
                    metric: metric.value().into(),
                    locality: gateway
                        .map_or(OrderedLocality::OnLink, |_gateway| OrderedLocality::OffLink),
                }
            }
        }

        let ordered_entry: OrderedEntry = (&entry).into();
        // Note, compare with "greater than or equal to" here, to ensure
        // that existing entries are preferred over new entries.
        let index = table.partition_point(|entry| ordered_entry.ge(&entry.into()));

        table.insert(index, entry);

        Ok(&table[index])
    }

    // Applies the given predicate to the entries in the forwarding table,
    // removing (and returning) those that yield `true` while retaining those
    // that yield `false`.
    fn del_entries<F: Fn(&Entry<I::Addr, D>) -> bool>(
        &mut self,
        predicate: F,
    ) -> Vec<Entry<I::Addr, D>> {
        // TODO(https://github.com/rust-lang/rust/issues/43244): Use
        // drain_filter to avoid extra allocation.
        let Self { table } = self;
        let owned_table = core::mem::replace(table, Vec::new());
        let (removed, owned_table) = owned_table.into_iter().partition(|entry| predicate(entry));
        *table = owned_table;
        removed
    }

    /// Get an iterator over all of the forwarding entries ([`Entry`]) this
    /// `ForwardingTable` knows about.
    pub(crate) fn iter_table(&self) -> Iter<'_, Entry<I::Addr, D>> {
        self.table.iter()
    }

    /// Look up the forwarding entry for an address in the table.
    ///
    /// Look up the forwarding entry for an address in the table, returning the
    /// next hop and device over which the address is reachable.
    ///
    /// If `device` is specified, the available routes are limited to those that
    /// egress over the device.
    ///
    /// If multiple entries match `address` or the first entry will be selected.
    /// See [`ForwardingTable`] for more details of how entries are sorted.
    pub(crate) fn lookup<SC: DeviceIdContext<AnyDevice, DeviceId = D>>(
        &self,
        sync_ctx: &mut SC,
        local_device: Option<&D>,
        address: I::Addr,
    ) -> Option<Destination<I::Addr, D>> {
        self.lookup_filter_map(sync_ctx, local_device, address, |_: &mut SC, _: &D| Some(()))
            .map(|(Destination { device, next_hop }, ())| Destination {
                device: device.clone(),
                next_hop,
            })
            .next()
    }

    pub(crate) fn lookup_filter_map<'a, SC: DeviceIdContext<AnyDevice, DeviceId = D>, R>(
        &'a self,
        sync_ctx: &'a mut SC,
        local_device: Option<&'a D>,
        address: I::Addr,
        mut f: impl FnMut(&mut SC, &D) -> Option<R> + 'a,
    ) -> impl Iterator<Item = (Destination<I::Addr, &D>, R)> + 'a {
        let Self { table } = self;
        // Get all potential routes we could take to reach `address`.
        table.iter().filter_map(move |entry| {
            let Entry { subnet, device, gateway, metric: _ } = entry;
            if !subnet.contains(&address) {
                return None;
            }
            if local_device.map_or(false, |local_device| local_device != device) {
                return None;
            }
            // TODO(https://fxbug.dev/125338): Also make sure that the IP device
            // is enabled.
            if !sync_ctx.is_device_installed(device) {
                return None;
            }

            f(sync_ctx, device).map(|r| {
                let next_hop = gateway.map_or(NextHop::RemoteAsNeighbor, NextHop::Gateway);
                (Destination { next_hop, device }, r)
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
        entry: Entry<I::Addr, D>,
    ) -> Result<&Entry<I::Addr, D>, crate::error::ExistsError> {
        table.add_entry(entry)
    }
}
#[cfg(test)]
mod tests {
    use core::marker::PhantomData;

    use fakealloc::collections::HashSet;
    use ip_test_macro::ip_test;
    use itertools::Itertools;
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::{
        ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
        SpecifiedAddr,
    };
    use tracing::trace;

    use super::*;
    use crate::{
        context::testutil::FakeSyncCtx,
        device::testutil::MultipleDevicesId,
        ip::{
            testutil::FakeIpDeviceIdCtx,
            types::{Metric, RawMetric},
        },
        testutil::FakeEventDispatcherConfig,
    };

    #[derive(Default)]
    struct FakeForwardingContext<I> {
        ip_device_id_ctx: FakeIpDeviceIdCtx<MultipleDevicesId>,
        _marker: PhantomData<I>,
    }

    impl<I> AsRef<FakeIpDeviceIdCtx<MultipleDevicesId>> for FakeForwardingContext<I> {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<MultipleDevicesId> {
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
        let entry = Entry { subnet, device: device.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry.clone()), Ok(&entry));
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry]);

        // Attempting to add the route again should fail.
        assert_eq!(table.add_entry(entry.clone()).unwrap_err(), crate::error::ExistsError);
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry]);

        // Add the route but as a next hop route.
        let entry2 =
            Entry { subnet: next_hop_subnet, device: device.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry2.clone()), Ok(&entry2));
        let entry3 =
            Entry { subnet: subnet, device: device.clone(), gateway: Some(next_hop), metric };
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
            table
                .del_entries(
                    |Entry { subnet, device: _, gateway: _, metric: _ }| subnet == &config.subnet
                )
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([
                Entry { subnet: config.subnet, device: device.clone(), gateway: None, metric },
                Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: Some(next_hop),
                    metric
                }
            ])
        );

        assert_eq!(
            table.iter_table().collect::<Vec<_>>(),
            &[&Entry { subnet: next_hop_subnet, device: device.clone(), gateway: None, metric }]
        );
    }

    #[ip_test]
    fn test_simple_lookup<I: Ip + TestIpExt>() {
        let (mut table, config, next_hop, _next_hop_subnet, device, metric) = simple_setup::<I>();
        let mut sync_ctx = FakeCtx::<I>::default();

        // Do lookup for our next hop (should be the device).
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );

        // Do lookup for some address within `subnet`.
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *config.local_ip),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *config.remote_ip),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );

        // Delete routes to the subnet and make sure that we can no longer route
        // to destinations in the subnet.
        assert_eq!(
            table
                .del_entries(
                    |Entry { subnet, device: _, gateway: _, metric: _ }| subnet == &config.subnet
                )
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([
                Entry { subnet: config.subnet, device: device.clone(), gateway: None, metric },
                Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: Some(next_hop),
                    metric
                }
            ])
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );
        assert_eq!(table.lookup(&mut sync_ctx, None, *config.local_ip), None);
        assert_eq!(table.lookup(&mut sync_ctx, None, *config.remote_ip), None);

        // Make the subnet routable again but through a gateway.
        let gateway_entry = Entry {
            subnet: config.subnet,
            device: device.clone(),
            gateway: Some(next_hop),
            metric: Metric::ExplicitMetric(RawMetric(0)),
        };
        assert_eq!(table.add_entry(gateway_entry.clone()), Ok(&gateway_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *config.local_ip),
            Some(Destination { next_hop: NextHop::Gateway(next_hop), device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *config.remote_ip),
            Some(Destination { next_hop: NextHop::Gateway(next_hop), device: device.clone() })
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

        let entry = Entry { subnet: sub1, device: device0.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry.clone()), Ok(&entry));
        table.print_table();
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *addr1).unwrap(),
            Destination { next_hop: NextHop::RemoteAsNeighbor, device: device0.clone() }
        );
        assert_eq!(table.lookup(&mut sync_ctx, None, *addr2), None);

        // Add a default route.
        //
        // Our expected forwarding table should look like:
        //  sub1 -> device0
        //  default -> addr1 w/ device0

        let default_sub = Subnet::new(I::UNSPECIFIED_ADDRESS, 0).unwrap();
        let default_entry =
            Entry { subnet: default_sub, device: device0.clone(), gateway: Some(addr1), metric };

        assert_eq!(table.add_entry(default_entry.clone()), Ok(&default_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *addr1).unwrap(),
            Destination { next_hop: NextHop::RemoteAsNeighbor, device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *addr2).unwrap(),
            Destination { next_hop: NextHop::Gateway(addr1), device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *addr3).unwrap(),
            Destination { next_hop: NextHop::Gateway(addr1), device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, None, I::UNSPECIFIED_ADDRESS).unwrap(),
            Destination { next_hop: NextHop::Gateway(addr1), device: device0.clone() }
        );
    }

    #[ip_test]
    fn test_device_filter_with_varying_prefix_lengths<I: Ip + TestIpExt>() {
        const MORE_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        const LESS_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::B;

        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let (remote, more_specific_sub) = I::next_hop_addr_sub(1, 1);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 2);
            assert_eq!(remote, addr);
            sub
        };
        let metric = Metric::ExplicitMetric(RawMetric(0));
        let less_specific_entry = Entry {
            subnet: less_specific_sub,
            device: LESS_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(less_specific_entry.clone()), Ok(&less_specific_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *remote),
            Some(Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: LESS_SPECIFIC_SUB_DEVICE.clone()
            }),
            "matches route"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&LESS_SPECIFIC_SUB_DEVICE), *remote),
            Some(Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: LESS_SPECIFIC_SUB_DEVICE.clone()
            }),
            "route matches specified device"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&MORE_SPECIFIC_SUB_DEVICE), *remote),
            None,
            "no route with the specified device"
        );

        let more_specific_entry = Entry {
            subnet: more_specific_sub,
            device: MORE_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(more_specific_entry.clone()), Ok(&more_specific_entry));
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *remote).unwrap(),
            Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: MORE_SPECIFIC_SUB_DEVICE.clone()
            },
            "matches most specific route"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&LESS_SPECIFIC_SUB_DEVICE), *remote),
            Some(Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: LESS_SPECIFIC_SUB_DEVICE.clone()
            }),
            "matches less specific route with the specified device"
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&MORE_SPECIFIC_SUB_DEVICE), *remote).unwrap(),
            Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: MORE_SPECIFIC_SUB_DEVICE.clone()
            },
            "matches the most specific route with the specified device"
        );
    }

    #[ip_test]
    fn test_lookup_filter_map<I: Ip + TestIpExt>() {
        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();

        let (next_hop, more_specific_sub) = I::next_hop_addr_sub(1, 1);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 2);
            assert_eq!(next_hop, addr);
            sub
        };

        // MultipleDevicesId::A always has a more specific route than B or C.
        {
            let metric = Metric::ExplicitMetric(RawMetric(0));
            let more_specific_entry = Entry {
                subnet: more_specific_sub,
                device: MultipleDevicesId::A,
                gateway: None,
                metric,
            };
            let _: &_ = table.add_entry(more_specific_entry).expect("was added");
        }
        // B and C have the same route but with different metrics.
        for (device, metric) in [(MultipleDevicesId::B, 100), (MultipleDevicesId::C, 200)] {
            let less_specific_entry = Entry {
                subnet: less_specific_sub,
                device,
                gateway: None,
                metric: Metric::ExplicitMetric(RawMetric(metric)),
            };
            let _: &_ = table.add_entry(less_specific_entry).expect("was added");
        }

        fn lookup_with_devices<I: Ip>(
            table: &ForwardingTable<I, MultipleDevicesId>,
            next_hop: SpecifiedAddr<I::Addr>,
            sync_ctx: &mut FakeSyncCtx<FakeForwardingContext<I>, (), MultipleDevicesId>,
            devices: &[MultipleDevicesId],
        ) -> Vec<Destination<I::Addr, MultipleDevicesId>> {
            table
                .lookup_filter_map(sync_ctx, None, *next_hop, |_, d| {
                    devices.iter().contains(d).then_some(())
                })
                .map(|(Destination { next_hop, device }, ())| Destination {
                    next_hop,
                    device: device.clone(),
                })
                .collect::<Vec<_>>()
        }

        // Looking up the address without constraints should always give a route
        // through device A.
        assert_eq!(
            table.lookup(&mut sync_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::A })
        );
        // Without filtering, we should get A, then B, then C.
        assert_eq!(
            lookup_with_devices(&table, next_hop, &mut sync_ctx, &MultipleDevicesId::all()),
            &[
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::A },
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::B },
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::C },
            ]
        );

        // If we filter out A, we get B and C.
        assert_eq!(
            lookup_with_devices(
                &table,
                next_hop,
                &mut sync_ctx,
                &[MultipleDevicesId::B, MultipleDevicesId::C]
            ),
            &[
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::B },
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::C }
            ]
        );

        // If we only allow C, we won't get the other devices.
        assert_eq!(
            lookup_with_devices(&table, next_hop, &mut sync_ctx, &[MultipleDevicesId::C]),
            &[Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::C }]
        );
    }

    #[ip_test]
    fn test_multiple_routes_to_subnet_through_different_devices<I: Ip + TestIpExt>() {
        const DEVICE1: MultipleDevicesId = MultipleDevicesId::A;
        const DEVICE2: MultipleDevicesId = MultipleDevicesId::B;

        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let (remote, sub) = I::next_hop_addr_sub(1, 1);
        let metric = Metric::ExplicitMetric(RawMetric(0));

        let entry1 = Entry { subnet: sub, device: DEVICE1.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry1.clone()), Ok(&entry1));
        let entry2 = Entry { subnet: sub, device: DEVICE2.clone(), gateway: None, metric };
        assert_eq!(table.add_entry(entry2.clone()), Ok(&entry2));
        let lookup = table.lookup(&mut sync_ctx, None, *remote);
        assert!(
            [
                Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: DEVICE1.clone() }),
                Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: DEVICE2.clone() })
            ]
            .contains(&lookup),
            "lookup = {:?}",
            lookup
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&DEVICE1), *remote),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: DEVICE1.clone() }),
        );
        assert_eq!(
            table.lookup(&mut sync_ctx, Some(&DEVICE2), *remote),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: DEVICE2.clone() }),
        );
    }

    #[ip_test]
    fn test_use_active_device<I: Ip + TestIpExt>() {
        const MORE_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        const LESS_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::B;

        let mut sync_ctx = FakeCtx::<I>::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        let (remote, more_specific_sub) = I::next_hop_addr_sub(1, 1);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 2);
            assert_eq!(remote, addr);
            sub
        };
        let metric = Metric::ExplicitMetric(RawMetric(0));

        let less_specific_entry = Entry {
            subnet: less_specific_sub,
            device: LESS_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(less_specific_entry.clone()), Ok(&less_specific_entry));
        for (device_removed, expected) in [
            // If the device is removed, then we cannot use routes through it.
            (true, None),
            (
                false,
                Some(Destination {
                    next_hop: NextHop::RemoteAsNeighbor,
                    device: LESS_SPECIFIC_SUB_DEVICE.clone(),
                }),
            ),
        ] {
            sync_ctx
                .get_mut()
                .ip_device_id_ctx
                .set_device_removed(LESS_SPECIFIC_SUB_DEVICE, device_removed);
            assert_eq!(
                table.lookup(&mut sync_ctx, None, *remote),
                expected,
                "device_removed={}",
                device_removed,
            );
        }

        let more_specific_entry = Entry {
            subnet: more_specific_sub,
            device: MORE_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(table.add_entry(more_specific_entry.clone()), Ok(&more_specific_entry));
        for (device_removed, expected) in [
            (
                false,
                Some(Destination {
                    next_hop: NextHop::RemoteAsNeighbor,
                    device: MORE_SPECIFIC_SUB_DEVICE.clone(),
                }),
            ),
            // If the device is removed, then we cannot use routes through it,
            // but can use routes through other (active) devices.
            (
                true,
                Some(Destination {
                    next_hop: NextHop::RemoteAsNeighbor,
                    device: LESS_SPECIFIC_SUB_DEVICE.clone(),
                }),
            ),
        ] {
            sync_ctx
                .get_mut()
                .ip_device_id_ctx
                .set_device_removed(MORE_SPECIFIC_SUB_DEVICE, device_removed);
            assert_eq!(
                table.lookup(&mut sync_ctx, None, *remote),
                expected,
                "device_removed={}",
                device_removed,
            );
        }

        // If no devices exist, then we can't get a route.
        sync_ctx.get_mut().ip_device_id_ctx.set_device_removed(LESS_SPECIFIC_SUB_DEVICE, true);
        assert_eq!(table.lookup(&mut sync_ctx, None, *remote), None,);
    }

    #[ip_test]
    fn test_add_entry_keeps_table_sorted<I: Ip>() {
        const DEVICE_A: MultipleDevicesId = MultipleDevicesId::A;
        const DEVICE_B: MultipleDevicesId = MultipleDevicesId::B;
        let (more_specific_sub, less_specific_sub) = I::map_ip(
            (),
            |()| (net_subnet_v4!("192.168.0.0/24"), net_subnet_v4!("192.168.0.0/16")),
            |()| (net_subnet_v6!("fe80::/64"), net_subnet_v6!("fe80::/16")),
        );
        let lower_metric = Metric::ExplicitMetric(RawMetric(0));
        let higher_metric = Metric::ExplicitMetric(RawMetric(1));
        let on_link = None;
        let off_link = Some(SpecifiedAddr::<I::Addr>::new(I::map_ip(
            (),
            |()| net_ip_v4!("192.168.0.1"),
            |()| net_ip_v6!("fe80::1"),
        )))
        .unwrap();

        fn entry<I: Ip, D>(
            d: D,
            s: Subnet<I::Addr>,
            m: Metric,
            g: Option<SpecifiedAddr<I::Addr>>,
        ) -> Entry<I::Addr, D> {
            Entry { device: d, subnet: s, metric: m, gateway: g }
        }

        // Expect the forwarding table to be sorted by longest matching prefix,
        // followed by metric, followed by on/off link, followed by insertion
        // order.
        // Note that the test adds entries for `DEVICE_B` after `DEVICE_A`.
        let expected_table = [
            entry::<I, _>(DEVICE_A, more_specific_sub, lower_metric, on_link),
            entry::<I, _>(DEVICE_B, more_specific_sub, lower_metric, on_link),
            entry::<I, _>(DEVICE_A, more_specific_sub, lower_metric, off_link),
            entry::<I, _>(DEVICE_B, more_specific_sub, lower_metric, off_link),
            entry::<I, _>(DEVICE_A, more_specific_sub, higher_metric, on_link),
            entry::<I, _>(DEVICE_B, more_specific_sub, higher_metric, on_link),
            entry::<I, _>(DEVICE_A, more_specific_sub, higher_metric, off_link),
            entry::<I, _>(DEVICE_B, more_specific_sub, higher_metric, off_link),
            entry::<I, _>(DEVICE_A, less_specific_sub, lower_metric, on_link),
            entry::<I, _>(DEVICE_B, less_specific_sub, lower_metric, on_link),
            entry::<I, _>(DEVICE_A, less_specific_sub, lower_metric, off_link),
            entry::<I, _>(DEVICE_B, less_specific_sub, lower_metric, off_link),
            entry::<I, _>(DEVICE_A, less_specific_sub, higher_metric, on_link),
            entry::<I, _>(DEVICE_B, less_specific_sub, higher_metric, on_link),
            entry::<I, _>(DEVICE_A, less_specific_sub, higher_metric, off_link),
            entry::<I, _>(DEVICE_B, less_specific_sub, higher_metric, off_link),
        ];
        let device_a_routes = expected_table
            .iter()
            .cloned()
            .filter(|entry| entry.device == DEVICE_A)
            .collect::<Vec<_>>();
        let device_b_routes = expected_table
            .iter()
            .cloned()
            .filter(|entry| entry.device == DEVICE_B)
            .collect::<Vec<_>>();

        // Add routes to the table in all possible permutations, asserting that
        // they always yield the expected order. Add `DEVICE_B` routes after
        // `DEVICE_A` routes.
        for insertion_order in device_a_routes.iter().permutations(device_a_routes.len()) {
            let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
            for entry in insertion_order.into_iter().chain(device_b_routes.iter()) {
                assert_eq!(table.add_entry(entry.clone()), Ok(entry));
            }
            assert_eq!(table.iter_table().cloned().collect::<Vec<_>>(), expected_table);
        }
    }
}
