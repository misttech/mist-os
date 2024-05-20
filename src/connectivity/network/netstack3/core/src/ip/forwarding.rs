// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IP forwarding definitions.

use alloc::vec::Vec;
use core::fmt::Debug;

use net_types::{
    ip::{GenericOverIp, Ip, IpAddress as _, Ipv4, Ipv4Addr, Subnet},
    SpecifiedAddr, Witness as _,
};
use thiserror::Error;
use tracing::debug;

use crate::{
    device::{AnyDevice, DeviceIdContext},
    ip::{
        types::{
            AddableEntry, Destination, Entry, EntryAndGeneration, IpTypesIpExt, NextHop,
            OrderedEntry, RawMetric,
        },
        IpLayerBindingsContext, IpLayerEvent, IpLayerIpExt,
    },
};

/// Provides access to a device for the purposes of IP forwarding.
pub trait IpForwardingDeviceContext<I: Ip>: DeviceIdContext<AnyDevice> {
    /// Returns the routing metric for the device.
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric;

    /// Returns true if the IP device is enabled.
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool;
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

/// Requests that a route be added to the forwarding table.
pub(crate) fn request_context_add_route<
    I: IpLayerIpExt,
    DeviceId,
    BC: IpLayerBindingsContext<I, DeviceId>,
>(
    bindings_ctx: &mut BC,
    entry: AddableEntry<I::Addr, DeviceId>,
) {
    bindings_ctx.on_event(IpLayerEvent::AddRoute(entry))
}

/// Requests that routes matching these specifiers be removed from the
/// forwarding table.
pub(crate) fn request_context_del_routes<
    I: IpLayerIpExt,
    DeviceId,
    BC: IpLayerBindingsContext<I, DeviceId>,
>(
    bindings_ctx: &mut BC,
    del_subnet: Subnet<I::Addr>,
    del_device: DeviceId,
    del_gateway: Option<SpecifiedAddr<I::Addr>>,
) {
    bindings_ctx.on_event(IpLayerEvent::RemoveRoutes {
        subnet: del_subnet,
        device: del_device,
        gateway: del_gateway,
    })
}

/// An IP forwarding table.
///
/// `ForwardingTable` maps destination subnets to the nearest IP hosts (on the
/// local network) able to route IP packets to those subnets.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
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
    pub(super) table: Vec<EntryAndGeneration<I::Addr, D>>,
}

impl<I: Ip, D> Default for ForwardingTable<I, D> {
    fn default() -> ForwardingTable<I, D> {
        ForwardingTable { table: Vec::default() }
    }
}

impl<I: IpTypesIpExt, D: Clone + Debug + PartialEq> ForwardingTable<I, D> {
    /// Adds `entry` to the forwarding table if it does not already exist.
    ///
    /// On success, a reference to the inserted entry is returned.
    pub fn add_entry(
        &mut self,
        entry: EntryAndGeneration<I::Addr, D>,
    ) -> Result<&EntryAndGeneration<I::Addr, D>, crate::error::ExistsError>
    where
        D: PartialOrd,
    {
        debug!("adding route: {}", entry);
        let Self { table } = self;

        if table.contains(&entry) {
            // If we already have this exact route, don't add it again.
            return Err(crate::error::ExistsError);
        }

        let ordered_entry: OrderedEntry<'_, _, _> = (&entry).into();
        // Note, compare with "greater than or equal to" here, to ensure
        // that existing entries are preferred over new entries.
        let index = table.partition_point(|entry| ordered_entry.ge(&entry.into()));

        table.insert(index, entry);

        Ok(&table[index])
    }

    // Applies the given predicate to the entries in the forwarding table,
    // removing (and returning) those that yield `true` while retaining those
    // that yield `false`.
    #[cfg(any(test, feature = "testutils"))]
    fn del_entries<F: Fn(&Entry<I::Addr, D>) -> bool>(
        &mut self,
        predicate: F,
    ) -> alloc::vec::Vec<Entry<I::Addr, D>> {
        // TODO(https://github.com/rust-lang/rust/issues/43244): Use
        // drain_filter to avoid extra allocation.
        let Self { table } = self;
        let owned_table = core::mem::take(table);
        let (removed, owned_table) =
            owned_table.into_iter().partition(|entry| predicate(&entry.entry));
        *table = owned_table;
        removed.into_iter().map(|entry| entry.entry).collect()
    }

    /// Get an iterator over all of the forwarding entries ([`Entry`]) this
    /// `ForwardingTable` knows about.
    pub(crate) fn iter_table(&self) -> impl Iterator<Item = &Entry<I::Addr, D>> {
        self.table.iter().map(|entry| &entry.entry)
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
    pub(crate) fn lookup<CC: IpForwardingDeviceContext<I, DeviceId = D>>(
        &self,
        core_ctx: &mut CC,
        local_device: Option<&D>,
        address: I::Addr,
    ) -> Option<Destination<I::Addr, D>> {
        self.lookup_filter_map(core_ctx, local_device, address, |_: &mut CC, _: &D| Some(()))
            .map(|(Destination { device, next_hop }, ())| Destination {
                device: device.clone(),
                next_hop,
            })
            .next()
    }

    pub(crate) fn lookup_filter_map<'a, CC: IpForwardingDeviceContext<I, DeviceId = D>, R>(
        &'a self,
        core_ctx: &'a mut CC,
        local_device: Option<&'a D>,
        address: I::Addr,
        mut f: impl FnMut(&mut CC, &D) -> Option<R> + 'a,
    ) -> impl Iterator<Item = (Destination<I::Addr, &D>, R)> + 'a {
        let Self { table } = self;

        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        enum BroadcastCase<I: IpTypesIpExt> {
            AllOnes(I::BroadcastMarker),
            Subnet(I::BroadcastMarker),
            NotBroadcast,
        }

        let bound_device_all_ones_broadcast_exemption = core::iter::once_with(move || {
            // If we're bound to a device and trying to broadcast on the local
            // network, then provide a matched broadcast route.
            let local_device = local_device?;
            let next_hop = I::map_ip::<_, Option<NextHop<I::Addr>>>(
                address,
                |address| {
                    (address == Ipv4::LIMITED_BROADCAST_ADDRESS.get())
                        .then_some(NextHop::Broadcast(()))
                },
                |_address| None,
            )?;
            Some(Destination { next_hop, device: local_device })
        })
        .filter_map(|x| x);

        let viable_table_entries = table.iter().filter_map(move |entry| {
            let EntryAndGeneration {
                entry: Entry { subnet, device, gateway, metric: _ },
                generation: _,
            } = entry;
            if !subnet.contains(&address) {
                return None;
            }
            if local_device.is_some_and(|local_device| local_device != device) {
                return None;
            }

            let broadcast_case = I::map_ip::<_, BroadcastCase<I>>(
                (address, *subnet),
                |(address, subnet)| {
                    // As per RFC 919 section 7,
                    //      The address 255.255.255.255 denotes a broadcast on a local hardware
                    //      network, which must not be forwarded.
                    if address == Ipv4::LIMITED_BROADCAST_ADDRESS.get() {
                        BroadcastCase::AllOnes(())
                    // Or the destination address is the highest address in the subnet.
                    // Per RFC 922,
                    //       Since the local network layer can always map an IP address into data
                    //       link layer address, the choice of an IP "broadcast host number" is
                    //       somewhat arbitrary.  For simplicity, it should be one not likely to be
                    //       assigned to a real host.  The number whose bits are all ones has this
                    //       property; this assignment was first proposed in [6].  In the few cases
                    //       where a host has been assigned an address with a host-number part of
                    //       all ones, it does not seem onerous to require renumbering.
                    // We require that the subnet contain more than one address (i.e. that the
                    // prefix length is not 32) in order to decide that an address is a subnet
                    // broadcast address.
                    } else if subnet.prefix() < Ipv4Addr::BYTES * 8 && subnet.broadcast() == address
                    {
                        BroadcastCase::Subnet(())
                    } else {
                        BroadcastCase::NotBroadcast
                    }
                },
                // IPv6 has no notion of "broadcast".
                |(_address, _subnet)| BroadcastCase::NotBroadcast,
            );

            let next_hop = match broadcast_case {
                // Always broadcast to the all-ones destination.
                BroadcastCase::AllOnes(marker) => NextHop::Broadcast(marker),
                // Only broadcast to the subnet broadcast address if the route does not have a
                // gateway.
                BroadcastCase::Subnet(marker) => {
                    gateway.map_or(NextHop::Broadcast(marker), NextHop::Gateway)
                }
                BroadcastCase::NotBroadcast => {
                    gateway.map_or(NextHop::RemoteAsNeighbor, NextHop::Gateway)
                }
            };

            Some(Destination { next_hop, device })
        });

        bound_device_all_ones_broadcast_exemption.chain(viable_table_entries).filter_map(
            move |destination| {
                let device = &destination.device;
                if !core_ctx.is_ip_device_enabled(device) {
                    return None;
                }
                f(core_ctx, device).map(|r| (destination, r))
            },
        )
    }
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    // This allows us to conveniently define testutils available only within
    // tests in this crate, as well as testutils available for use by tests that
    // build this crate with the testutils feature enabled. (Morally, all such
    // tests are actually tests of this crate -- this is a quirk of the GN build
    // system for the netstack3-core loom tests.)
    #[cfg(test)]
    pub(crate) use super::testutil_testonly::*;

    use crate::ip::{
        types::{AddableMetric, Generation, Metric},
        IpStateContext,
    };

    use super::*;

    // Converts the given [`AddableMetric`] into the corresponding [`Metric`],
    // observing the device's metric, if applicable.
    fn observe_metric<I: Ip, CC: IpForwardingDeviceContext<I>>(
        core_ctx: &mut CC,
        device: &CC::DeviceId,
        metric: AddableMetric,
    ) -> Metric {
        match metric {
            AddableMetric::ExplicitMetric(value) => Metric::ExplicitMetric(value),
            AddableMetric::MetricTracksInterface => {
                Metric::MetricTracksInterface(core_ctx.get_routing_metric(device))
            }
        }
    }

    /// Add a route directly to the forwarding table, instead of merely
    /// dispatching an event requesting that the route be added.
    pub(crate) fn add_route<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, CC::DeviceId>,
        CC: IpStateContext<I, BC>,
    >(
        core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        entry: crate::ip::types::AddableEntry<I::Addr, CC::DeviceId>,
    ) -> Result<(), AddRouteError>
    where
        CC::DeviceId: PartialOrd,
    {
        let crate::ip::types::AddableEntry { subnet, device, gateway, metric } = entry;
        core_ctx.with_ip_routing_table_mut(|core_ctx, table| {
            let metric = observe_metric(core_ctx, &device, metric);
            let _entry = table.add_entry(EntryAndGeneration {
                entry: Entry { subnet, device, gateway, metric },
                generation: Generation::initial(),
            })?;
            Ok(())
        })
    }

    /// Delete all routes to a subnet, returning `Err` if no route was found to
    /// be deleted.
    ///
    /// Note, `del_routes_to_subnet` will remove *all* routes to a
    /// `subnet`, including routes that consider `subnet` on-link for some device
    /// and routes that require packets destined to a node within `subnet` to be
    /// routed through some next-hop node.
    // TODO(https://fxbug.dev/42077399): Unify this with other route removal methods.
    pub(crate) fn del_routes_to_subnet<
        I: IpLayerIpExt,
        BC: IpLayerBindingsContext<I, CC::DeviceId>,
        CC: IpStateContext<I, BC>,
    >(
        core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        del_subnet: Subnet<I::Addr>,
    ) -> Result<(), crate::error::NotFoundError> {
        core_ctx.with_ip_routing_table_mut(|_core_ctx, table| {
            let removed =
                table.del_entries(|Entry { subnet, device: _, gateway: _, metric: _ }| {
                    subnet == &del_subnet
                });
            if removed.is_empty() {
                return Err(crate::error::NotFoundError);
            } else {
                Ok(())
            }
        })
    }

    pub(crate) fn del_device_routes<
        I: IpLayerIpExt,
        CC: IpStateContext<I, BC>,
        BC: IpLayerBindingsContext<I, CC::DeviceId>,
    >(
        core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        del_device: &CC::DeviceId,
    ) {
        debug!("deleting routes on device: {del_device:?}");

        let _: Vec<_> = core_ctx.with_ip_routing_table_mut(|_core_ctx, table| {
            table.del_entries(|Entry { subnet: _, device, gateway: _, metric: _ }| {
                device == del_device
            })
        });
    }
}

#[cfg(test)]
mod testutil_testonly {
    use alloc::collections::HashSet;

    use derivative::Derivative;
    use net_types::ip::IpAddress;

    use super::*;

    use crate::{
        context::testutil::FakeCoreCtx,
        device::StrongDeviceIdentifier,
        ip::{testutil::FakeIpDeviceIdCtx, types::Metric},
    };

    /// Adds an on-link forwarding entry for the specified address and device.
    pub(crate) fn add_on_link_forwarding_entry<A: IpAddress, D: Clone + Debug + PartialEq + Ord>(
        table: &mut ForwardingTable<A::Version, D>,
        ip: SpecifiedAddr<A>,
        device: D,
    ) where
        A::Version: IpTypesIpExt,
    {
        let subnet = Subnet::new(*ip, A::BYTES * 8).unwrap();
        let entry =
            Entry { subnet, device, gateway: None, metric: Metric::ExplicitMetric(RawMetric(0)) };
        assert_eq!(crate::ip::forwarding::testutil::add_entry(table, entry.clone()), Ok(&entry));
    }

    // Provide tests with access to the private `ForwardingTable.add_entry` fn.
    pub(crate) fn add_entry<I: IpTypesIpExt, D: Clone + Debug + PartialEq + Ord>(
        table: &mut ForwardingTable<I, D>,
        entry: Entry<I::Addr, D>,
    ) -> Result<&Entry<I::Addr, D>, crate::error::ExistsError> {
        table
            .add_entry(EntryAndGeneration {
                entry,
                generation: crate::ip::types::Generation::initial(),
            })
            .map(|entry| &entry.entry)
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    pub(crate) struct FakeIpForwardingContext<D> {
        disabled_devices: HashSet<D>,
        ip_device_id_ctx: FakeIpDeviceIdCtx<D>,
    }

    impl<D> FakeIpForwardingContext<D> {
        pub(crate) fn disabled_devices_mut(&mut self) -> &mut HashSet<D> {
            &mut self.disabled_devices
        }
    }

    impl<D> AsRef<FakeIpDeviceIdCtx<D>> for FakeIpForwardingContext<D> {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<D> {
            &self.ip_device_id_ctx
        }
    }

    impl<D> AsMut<FakeIpDeviceIdCtx<D>> for FakeIpForwardingContext<D> {
        fn as_mut(&mut self) -> &mut FakeIpDeviceIdCtx<D> {
            &mut self.ip_device_id_ctx
        }
    }

    pub(crate) type FakeIpForwardingCtx<D> = FakeCoreCtx<FakeIpForwardingContext<D>, (), D>;

    impl<I: Ip, D: StrongDeviceIdentifier> IpForwardingDeviceContext<I> for FakeIpForwardingCtx<D>
    where
        Self: DeviceIdContext<AnyDevice, DeviceId = D>,
    {
        fn get_routing_metric(&mut self, _device_id: &Self::DeviceId) -> RawMetric {
            unimplemented!()
        }

        fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
            !self.state.disabled_devices.contains(device_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use fakealloc::collections::HashSet;
    use ip_test_macro::ip_test;
    use itertools::Itertools;
    use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
    use net_types::ip::{IpAddress as _, Ipv6, Ipv6Addr};
    use test_case::test_case;
    use tracing::trace;

    use super::*;
    use crate::{
        device::{
            ethernet::{EthernetCreationProperties, EthernetLinkDevice},
            testutil::MultipleDevicesId,
            DeviceId,
        },
        ip::{
            forwarding::testutil::FakeIpForwardingCtx,
            types::{AddableEntryEither, AddableMetric, Metric},
        },
        testutil::{CtxPairExt as _, TestAddrs},
    };

    type FakeCtx = FakeIpForwardingCtx<MultipleDevicesId>;

    impl<I: IpTypesIpExt, D: Clone + Debug + PartialEq> ForwardingTable<I, D> {
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

    trait TestIpExt: crate::testutil::TestIpExt + IpTypesIpExt {
        fn subnet(v: u8, neg_prefix: u8) -> Subnet<Self::Addr>;

        fn next_hop_addr_sub(
            v: u8,
            neg_prefix: u8,
        ) -> (SpecifiedAddr<Self::Addr>, Subnet<Self::Addr>);
    }

    impl TestIpExt for Ipv4 {
        fn subnet(v: u8, neg_prefix: u8) -> Subnet<Ipv4Addr> {
            Subnet::new(Ipv4Addr::new([v, 0, 0, 0]), 32 - neg_prefix).unwrap()
        }

        fn next_hop_addr_sub(v: u8, neg_prefix: u8) -> (SpecifiedAddr<Ipv4Addr>, Subnet<Ipv4Addr>) {
            (SpecifiedAddr::new(Ipv4Addr::new([v, 0, 0, 1])).unwrap(), Ipv4::subnet(v, neg_prefix))
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
    }

    fn simple_setup<I: Ip + TestIpExt>() -> (
        ForwardingTable<I, MultipleDevicesId>,
        TestAddrs<I::Addr>,
        SpecifiedAddr<I::Addr>,
        Subnet<I::Addr>,
        MultipleDevicesId,
        Metric,
    ) {
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();

        let config = I::TEST_ADDRS;
        let subnet = config.subnet;
        let device = MultipleDevicesId::A;
        // `neg_prefix` passed here must be at least 2 (as with a neg_prefix of
        // 1 we end up constructing the broadcast address instead).
        let (next_hop, next_hop_subnet) = I::next_hop_addr_sub(1, 2);
        let metric = Metric::ExplicitMetric(RawMetric(9999));

        // Should add the route successfully.
        let entry = Entry { subnet, device: device.clone(), gateway: None, metric };
        assert_eq!(super::testutil::add_entry(&mut table, entry.clone()), Ok(&entry));
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry]);

        // Attempting to add the route again should fail.
        assert_eq!(
            super::testutil::add_entry(&mut table, entry.clone()).unwrap_err(),
            crate::error::ExistsError
        );
        assert_eq!(table.iter_table().collect::<Vec<_>>(), &[&entry]);

        // Add the route but as a next hop route.
        let entry2 =
            Entry { subnet: next_hop_subnet, device: device.clone(), gateway: None, metric };
        assert_eq!(super::testutil::add_entry(&mut table, entry2.clone()), Ok(&entry2));
        let entry3 =
            Entry { subnet: subnet, device: device.clone(), gateway: Some(next_hop), metric };
        assert_eq!(super::testutil::add_entry(&mut table, entry3.clone()), Ok(&entry3));
        assert_eq!(
            table.iter_table().collect::<HashSet<_>>(),
            HashSet::from([&entry, &entry2, &entry3])
        );

        // Attempting to add the route again should fail.
        assert_eq!(
            super::testutil::add_entry(&mut table, entry3.clone()).unwrap_err(),
            crate::error::ExistsError
        );
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
                .del_entries(|Entry { subnet, device: _, gateway: _, metric: _ }| {
                    subnet == &config.subnet
                })
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([
                Entry { subnet: config.subnet, device: device.clone(), gateway: None, metric },
                Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: Some(next_hop),
                    metric,
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
        let mut core_ctx = FakeCtx::default();

        // Do lookup for our next hop (should be the device).
        assert_eq!(
            table.lookup(&mut core_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );

        // Do lookup for some address within `subnet`.
        assert_eq!(
            table.lookup(&mut core_ctx, None, *config.local_ip),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *config.remote_ip),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );

        // Add a default route to facilitate testing the limited broadcast address.
        // Without a default route being present, the all-ones broadcast address won't match any
        // route's destination subnet, so the route lookup will fail.
        let default_route_entry = Entry {
            subnet: Subnet::new(I::UNSPECIFIED_ADDRESS, 0).expect("default subnet"),
            device: device.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(
            super::testutil::add_entry(&mut table, default_route_entry.clone()),
            Ok(&default_route_entry)
        );

        // Do lookup for broadcast addresses.
        I::map_ip::<_, ()>(
            (&table, &config),
            |(table, config)| {
                assert_eq!(
                    table.lookup(&mut core_ctx, None, config.subnet.broadcast()),
                    Some(Destination { next_hop: NextHop::Broadcast(()), device: device.clone() })
                );

                assert_eq!(
                    table.lookup(&mut core_ctx, None, Ipv4::LIMITED_BROADCAST_ADDRESS.get()),
                    Some(Destination { next_hop: NextHop::Broadcast(()), device: device.clone() })
                );
            },
            |(_table, _config)| {
                // Do nothing since IPv6 doesn't have broadcast.
            },
        );

        // Remove the default route.
        assert_eq!(
            table
                .del_entries(|Entry { subnet, device: _, gateway: _, metric: _ }| {
                    subnet.prefix() == 0
                })
                .into_iter()
                .collect::<Vec<_>>(),
            alloc::vec![default_route_entry.clone()]
        );

        // Delete routes to the subnet and make sure that we can no longer route
        // to destinations in the subnet.
        assert_eq!(
            table
                .del_entries(|Entry { subnet, device: _, gateway: _, metric: _ }| {
                    subnet == &config.subnet
                })
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([
                Entry { subnet: config.subnet, device: device.clone(), gateway: None, metric },
                Entry {
                    subnet: config.subnet,
                    device: device.clone(),
                    gateway: Some(next_hop),
                    metric,
                }
            ])
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );
        assert_eq!(table.lookup(&mut core_ctx, None, *config.local_ip), None);
        assert_eq!(table.lookup(&mut core_ctx, None, *config.remote_ip), None);
        I::map_ip::<_, ()>(
            (&table, &config),
            |(table, config)| {
                assert_eq!(table.lookup(&mut core_ctx, None, config.subnet.broadcast()), None);
                assert_eq!(
                    table.lookup(&mut core_ctx, None, Ipv4::LIMITED_BROADCAST_ADDRESS.get()),
                    None
                );
            },
            |(_table, _config)| {
                // Do nothing since IPv6 doesn't have broadcast.
            },
        );

        // Make the subnet routable again but through a gateway.
        let gateway_entry = Entry {
            subnet: config.subnet,
            device: device.clone(),
            gateway: Some(next_hop),
            metric: Metric::ExplicitMetric(RawMetric(0)),
        };
        assert_eq!(
            super::testutil::add_entry(&mut table, gateway_entry.clone()),
            Ok(&gateway_entry)
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *config.local_ip),
            Some(Destination { next_hop: NextHop::Gateway(next_hop), device: device.clone() })
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *config.remote_ip),
            Some(Destination { next_hop: NextHop::Gateway(next_hop), device: device.clone() })
        );

        // Add a default route to facilitate testing the limited broadcast address.
        let default_route_entry = Entry {
            subnet: Subnet::new(I::UNSPECIFIED_ADDRESS, 0).expect("default subnet"),
            device: device.clone(),
            gateway: Some(next_hop),
            metric,
        };
        assert_eq!(
            super::testutil::add_entry(&mut table, default_route_entry.clone()),
            Ok(&default_route_entry)
        );

        // Do lookup for broadcast addresses.
        I::map_ip::<_, ()>(
            (&table, &config, next_hop),
            |(table, config, next_hop)| {
                assert_eq!(
                    table.lookup(&mut core_ctx, None, config.subnet.broadcast()),
                    Some(Destination {
                        next_hop: NextHop::Gateway(next_hop),
                        device: device.clone()
                    })
                );

                assert_eq!(
                    table.lookup(&mut core_ctx, None, Ipv4::LIMITED_BROADCAST_ADDRESS.get()),
                    Some(Destination { next_hop: NextHop::Broadcast(()), device: device.clone() })
                );
            },
            |(_table, _config, _next_hop)| {
                // Do nothing since IPv6 doesn't have broadcast.
            },
        );
    }

    #[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
    enum BroadcastCaseNextHop {
        Neighbor,
        Gateway,
    }

    #[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
    enum LookupResultNextHop {
        Neighbor,
        Gateway,
        Broadcast,
    }

    #[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
    struct LookupResult {
        next_hop: LookupResultNextHop,
        device: MultipleDevicesId,
    }

    #[test_case::test_matrix(
        [None, Some(BroadcastCaseNextHop::Neighbor), Some(BroadcastCaseNextHop::Gateway)],
        [None, Some(MultipleDevicesId::A), Some(MultipleDevicesId::B)]
    )]
    fn all_ones_broadcast_lookup(
        default_route: Option<BroadcastCaseNextHop>,
        bind_device: Option<MultipleDevicesId>,
    ) {
        let mut core_ctx = FakeCtx::default();
        let expected_lookup_result = match (default_route, bind_device) {
            // Sending to all-ones with a bound device always results in a broadcast.
            (_, Some(device)) => {
                Some(LookupResult { next_hop: LookupResultNextHop::Broadcast, device })
            }
            // With no matching route and no bound device, we don't know where to broadcast to,
            // so the lookup fails.
            (None, None) => None,
            (Some(_next_hop), None) => {
                // Regardless of the default route's configured next hop, sending to all-ones
                // should result in a broadcast.
                Some(LookupResult {
                    next_hop: LookupResultNextHop::Broadcast,
                    device: MultipleDevicesId::A,
                })
            }
        };

        let mut table = ForwardingTable::<Ipv4, MultipleDevicesId>::default();
        if let Some(next_hop) = default_route {
            let entry = Entry {
                subnet: Subnet::new(Ipv4::UNSPECIFIED_ADDRESS, 0).expect("default subnet"),
                device: MultipleDevicesId::A,
                gateway: match next_hop {
                    BroadcastCaseNextHop::Neighbor => None,
                    BroadcastCaseNextHop::Gateway => {
                        Some(SpecifiedAddr::new(net_ip_v4!("192.168.0.1")).unwrap())
                    }
                },
                metric: Metric::ExplicitMetric(RawMetric(0)),
            };
            assert_eq!(super::testutil::add_entry(&mut table, entry.clone()), Ok(&entry));
        }

        let got_lookup_result = table
            .lookup(&mut core_ctx, bind_device.as_ref(), Ipv4::LIMITED_BROADCAST_ADDRESS.get())
            .map(|Destination { next_hop, device }| LookupResult {
                next_hop: match next_hop {
                    NextHop::RemoteAsNeighbor => LookupResultNextHop::Neighbor,
                    NextHop::Gateway(_) => LookupResultNextHop::Gateway,
                    NextHop::Broadcast(()) => LookupResultNextHop::Broadcast,
                },
                device,
            });

        assert_eq!(got_lookup_result, expected_lookup_result);
    }

    #[test_case::test_matrix(
        [None, Some(BroadcastCaseNextHop::Neighbor), Some(BroadcastCaseNextHop::Gateway)],
        [None, Some(BroadcastCaseNextHop::Neighbor), Some(BroadcastCaseNextHop::Gateway)],
        [None, Some(MultipleDevicesId::A), Some(MultipleDevicesId::B)]
    )]
    fn subnet_broadcast_lookup(
        default_route: Option<BroadcastCaseNextHop>,
        subnet_route: Option<BroadcastCaseNextHop>,
        bind_device: Option<MultipleDevicesId>,
    ) {
        let mut core_ctx = FakeCtx::default();
        let expected_lookup_result = match bind_device {
            // Binding to a device not matching any routes in the table will fail the lookup.
            Some(MultipleDevicesId::B) | Some(MultipleDevicesId::C) => None,
            Some(MultipleDevicesId::A) | None => match (default_route, subnet_route) {
                // No matching routes.
                (None, None) => None,
                // The subnet route will take precedence over the default route.
                (None | Some(_), Some(next_hop)) => {
                    Some(LookupResult {
                        device: MultipleDevicesId::A,
                        next_hop: match next_hop {
                            // Allow broadcasting when this route is on-link.
                            BroadcastCaseNextHop::Neighbor => LookupResultNextHop::Broadcast,
                            // Continue to unicast when the route has a gateway, even though this is
                            // the subnet's broadcast address.
                            BroadcastCaseNextHop::Gateway => LookupResultNextHop::Gateway,
                        },
                    })
                }
                (Some(next_hop), None) => {
                    Some(LookupResult {
                        device: MultipleDevicesId::A,
                        next_hop: match next_hop {
                            // Since this is just matching the default route, it looks like
                            // a regular unicast route rather than a broadcast one.
                            BroadcastCaseNextHop::Neighbor => LookupResultNextHop::Neighbor,
                            BroadcastCaseNextHop::Gateway => LookupResultNextHop::Gateway,
                        },
                    })
                }
            },
        };

        let subnet = net_declare::net_subnet_v4!("192.168.0.0/24");
        let gateway = SpecifiedAddr::new(net_ip_v4!("192.168.0.1")).unwrap();

        let mut table = ForwardingTable::<Ipv4, MultipleDevicesId>::default();
        if let Some(next_hop) = default_route {
            let entry = Entry {
                subnet: Subnet::new(Ipv4::UNSPECIFIED_ADDRESS, 0).expect("default subnet"),
                device: MultipleDevicesId::A,
                gateway: match next_hop {
                    BroadcastCaseNextHop::Neighbor => None,
                    BroadcastCaseNextHop::Gateway => Some(gateway),
                },
                metric: Metric::ExplicitMetric(RawMetric(0)),
            };
            assert_eq!(super::testutil::add_entry(&mut table, entry.clone()), Ok(&entry));
        }

        if let Some(next_hop) = subnet_route {
            let entry = Entry {
                subnet,
                device: MultipleDevicesId::A,
                gateway: match next_hop {
                    BroadcastCaseNextHop::Neighbor => None,
                    BroadcastCaseNextHop::Gateway => Some(gateway),
                },
                metric: Metric::ExplicitMetric(RawMetric(0)),
            };
            assert_eq!(super::testutil::add_entry(&mut table, entry.clone()), Ok(&entry));
        }

        let got_lookup_result = table
            .lookup(&mut core_ctx, bind_device.as_ref(), subnet.broadcast())
            .map(|Destination { next_hop, device }| LookupResult {
                next_hop: match next_hop {
                    NextHop::RemoteAsNeighbor => LookupResultNextHop::Neighbor,
                    NextHop::Gateway(_) => LookupResultNextHop::Gateway,
                    NextHop::Broadcast(()) => LookupResultNextHop::Broadcast,
                },
                device,
            });

        assert_eq!(got_lookup_result, expected_lookup_result);
    }

    #[ip_test]
    fn test_default_route_ip<I: Ip + TestIpExt>() {
        let mut core_ctx = FakeCtx::default();
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
        assert_eq!(super::testutil::add_entry(&mut table, entry.clone()), Ok(&entry));
        table.print_table();
        assert_eq!(
            table.lookup(&mut core_ctx, None, *addr1).unwrap(),
            Destination { next_hop: NextHop::RemoteAsNeighbor, device: device0.clone() }
        );
        assert_eq!(table.lookup(&mut core_ctx, None, *addr2), None);

        // Add a default route.
        //
        // Our expected forwarding table should look like:
        //  sub1 -> device0
        //  default -> addr1 w/ device0

        let default_sub = Subnet::new(I::UNSPECIFIED_ADDRESS, 0).unwrap();
        let default_entry =
            Entry { subnet: default_sub, device: device0.clone(), gateway: Some(addr1), metric };

        assert_eq!(
            super::testutil::add_entry(&mut table, default_entry.clone()),
            Ok(&default_entry)
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *addr1).unwrap(),
            Destination { next_hop: NextHop::RemoteAsNeighbor, device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *addr2).unwrap(),
            Destination { next_hop: NextHop::Gateway(addr1), device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *addr3).unwrap(),
            Destination { next_hop: NextHop::Gateway(addr1), device: device0.clone() }
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, I::UNSPECIFIED_ADDRESS).unwrap(),
            Destination { next_hop: NextHop::Gateway(addr1), device: device0.clone() }
        );
    }

    #[ip_test]
    fn test_device_filter_with_varying_prefix_lengths<I: Ip + TestIpExt>() {
        const MORE_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        const LESS_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::B;

        let mut core_ctx = FakeCtx::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        // `neg_prefix` passed here must be at least 2 (as with a neg_prefix of
        // 1 we end up constructing the broadcast address instead).
        let (remote, more_specific_sub) = I::next_hop_addr_sub(1, 2);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 3);
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
        assert_eq!(
            super::testutil::add_entry(&mut table, less_specific_entry.clone()),
            Ok(&less_specific_entry)
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *remote),
            Some(Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: LESS_SPECIFIC_SUB_DEVICE.clone()
            }),
            "matches route"
        );
        assert_eq!(
            table.lookup(&mut core_ctx, Some(&LESS_SPECIFIC_SUB_DEVICE), *remote),
            Some(Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: LESS_SPECIFIC_SUB_DEVICE.clone()
            }),
            "route matches specified device"
        );
        assert_eq!(
            table.lookup(&mut core_ctx, Some(&MORE_SPECIFIC_SUB_DEVICE), *remote),
            None,
            "no route with the specified device"
        );

        let more_specific_entry = Entry {
            subnet: more_specific_sub,
            device: MORE_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(
            super::testutil::add_entry(&mut table, more_specific_entry.clone()),
            Ok(&more_specific_entry)
        );
        assert_eq!(
            table.lookup(&mut core_ctx, None, *remote).unwrap(),
            Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: MORE_SPECIFIC_SUB_DEVICE.clone()
            },
            "matches most specific route"
        );
        assert_eq!(
            table.lookup(&mut core_ctx, Some(&LESS_SPECIFIC_SUB_DEVICE), *remote),
            Some(Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: LESS_SPECIFIC_SUB_DEVICE.clone()
            }),
            "matches less specific route with the specified device"
        );
        assert_eq!(
            table.lookup(&mut core_ctx, Some(&MORE_SPECIFIC_SUB_DEVICE), *remote).unwrap(),
            Destination {
                next_hop: NextHop::RemoteAsNeighbor,
                device: MORE_SPECIFIC_SUB_DEVICE.clone()
            },
            "matches the most specific route with the specified device"
        );
    }

    #[ip_test]
    fn test_lookup_filter_map<I: Ip + TestIpExt>() {
        let mut core_ctx = FakeCtx::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();

        // `neg_prefix` passed here must be at least 2 (as with a neg_prefix of
        // 1 we end up constructing the broadcast address instead).
        let (next_hop, more_specific_sub) = I::next_hop_addr_sub(1, 2);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 3);
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
            let _: &_ =
                super::testutil::add_entry(&mut table, more_specific_entry).expect("was added");
        }
        // B and C have the same route but with different metrics.
        for (device, metric) in [(MultipleDevicesId::B, 100), (MultipleDevicesId::C, 200)] {
            let less_specific_entry = Entry {
                subnet: less_specific_sub,
                device,
                gateway: None,
                metric: Metric::ExplicitMetric(RawMetric(metric)),
            };
            let _: &_ =
                super::testutil::add_entry(&mut table, less_specific_entry).expect("was added");
        }

        fn lookup_with_devices<I: IpTypesIpExt>(
            table: &ForwardingTable<I, MultipleDevicesId>,
            next_hop: SpecifiedAddr<I::Addr>,
            core_ctx: &mut FakeCtx,
            devices: &[MultipleDevicesId],
        ) -> Vec<Destination<I::Addr, MultipleDevicesId>> {
            table
                .lookup_filter_map(core_ctx, None, *next_hop, |_, d| {
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
            table.lookup(&mut core_ctx, None, *next_hop),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::A })
        );
        // Without filtering, we should get A, then B, then C.
        assert_eq!(
            lookup_with_devices(&table, next_hop, &mut core_ctx, &MultipleDevicesId::all()),
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
                &mut core_ctx,
                &[MultipleDevicesId::B, MultipleDevicesId::C]
            ),
            &[
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::B },
                Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::C }
            ]
        );

        // If we only allow C, we won't get the other devices.
        assert_eq!(
            lookup_with_devices(&table, next_hop, &mut core_ctx, &[MultipleDevicesId::C]),
            &[Destination { next_hop: NextHop::RemoteAsNeighbor, device: MultipleDevicesId::C }]
        );
    }

    #[ip_test]
    fn test_multiple_routes_to_subnet_through_different_devices<I: Ip + TestIpExt>() {
        const DEVICE1: MultipleDevicesId = MultipleDevicesId::A;
        const DEVICE2: MultipleDevicesId = MultipleDevicesId::B;

        let mut core_ctx = FakeCtx::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        // `neg_prefix` passed here must be at least 2 (as with a neg_prefix of
        // 1 we end up constructing the broadcast address instead).
        let (remote, sub) = I::next_hop_addr_sub(1, 2);
        let metric = Metric::ExplicitMetric(RawMetric(0));

        let entry1 = Entry { subnet: sub, device: DEVICE1.clone(), gateway: None, metric };
        assert_eq!(super::testutil::add_entry(&mut table, entry1.clone()), Ok(&entry1));
        let entry2 = Entry { subnet: sub, device: DEVICE2.clone(), gateway: None, metric };
        assert_eq!(super::testutil::add_entry(&mut table, entry2.clone()), Ok(&entry2));
        let lookup = table.lookup(&mut core_ctx, None, *remote);
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
            table.lookup(&mut core_ctx, Some(&DEVICE1), *remote),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: DEVICE1.clone() }),
        );
        assert_eq!(
            table.lookup(&mut core_ctx, Some(&DEVICE2), *remote),
            Some(Destination { next_hop: NextHop::RemoteAsNeighbor, device: DEVICE2.clone() }),
        );
    }

    #[ip_test]
    #[test_case(|core_ctx, device, device_unusable| {
        let disabled_devices = core_ctx.state.disabled_devices_mut();
        if device_unusable {
            let _: bool = disabled_devices.insert(device);
        } else {
            let _: bool = disabled_devices.remove(&device);
        }
    }; "device_disabled")]
    fn test_usable_device<I: Ip + TestIpExt>(
        set_inactive: fn(&mut FakeCtx, MultipleDevicesId, bool),
    ) {
        const MORE_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        const LESS_SPECIFIC_SUB_DEVICE: MultipleDevicesId = MultipleDevicesId::B;

        let mut core_ctx = FakeCtx::default();
        let mut table = ForwardingTable::<I, MultipleDevicesId>::default();
        // `neg_prefix` passed here must be at least 2 (as with a neg_prefix of
        // 1 we end up constructing the broadcast address instead).
        let (remote, more_specific_sub) = I::next_hop_addr_sub(1, 2);
        let less_specific_sub = {
            let (addr, sub) = I::next_hop_addr_sub(1, 3);
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
        assert_eq!(
            super::testutil::add_entry(&mut table, less_specific_entry.clone()),
            Ok(&less_specific_entry)
        );
        for (device_unusable, expected) in [
            // If the device is unusable, then we cannot use routes through it.
            (true, None),
            (
                false,
                Some(Destination {
                    next_hop: NextHop::RemoteAsNeighbor,
                    device: LESS_SPECIFIC_SUB_DEVICE.clone(),
                }),
            ),
        ] {
            set_inactive(&mut core_ctx, LESS_SPECIFIC_SUB_DEVICE, device_unusable);
            assert_eq!(
                table.lookup(&mut core_ctx, None, *remote),
                expected,
                "device_unusable={}",
                device_unusable,
            );
        }

        let more_specific_entry = Entry {
            subnet: more_specific_sub,
            device: MORE_SPECIFIC_SUB_DEVICE.clone(),
            gateway: None,
            metric,
        };
        assert_eq!(
            super::testutil::add_entry(&mut table, more_specific_entry.clone()),
            Ok(&more_specific_entry)
        );
        for (device_unusable, expected) in [
            (
                false,
                Some(Destination {
                    next_hop: NextHop::RemoteAsNeighbor,
                    device: MORE_SPECIFIC_SUB_DEVICE.clone(),
                }),
            ),
            // If the device is unusable, then we cannot use routes through it,
            // but can use routes through other (active) devices.
            (
                true,
                Some(Destination {
                    next_hop: NextHop::RemoteAsNeighbor,
                    device: LESS_SPECIFIC_SUB_DEVICE.clone(),
                }),
            ),
        ] {
            set_inactive(&mut core_ctx, MORE_SPECIFIC_SUB_DEVICE, device_unusable);
            assert_eq!(
                table.lookup(&mut core_ctx, None, *remote),
                expected,
                "device_unusable={}",
                device_unusable,
            );
        }

        // If no devices are usable, then we can't get a route.
        set_inactive(&mut core_ctx, LESS_SPECIFIC_SUB_DEVICE, true);
        assert_eq!(table.lookup(&mut core_ctx, None, *remote), None,);
    }

    #[ip_test]
    fn test_add_entry_keeps_table_sorted<I: Ip + IpTypesIpExt>() {
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
                assert_eq!(super::testutil::add_entry(&mut table, entry.clone()), Ok(entry));
            }
            assert_eq!(table.iter_table().cloned().collect::<Vec<_>>(), expected_table);
        }
    }

    #[ip_test]
    #[test_case(true; "when there is an on-link route to the gateway")]
    #[test_case(false; "when there is no on-link route to the gateway")]
    fn select_device_for_gateway<I: Ip + TestIpExt>(on_link_route: bool) {
        let mut ctx = crate::testutil::Ctx::<crate::testutil::FakeBindingsCtx>::new_with_builder(
            crate::state::StackStateBuilder::default(),
        );

        let device_id: DeviceId<_> = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: I::TEST_ADDRS.local_mac,
                    max_frame_size: crate::device::ethernet::MaxEthernetFrameSize::from_mtu(
                        I::MINIMUM_LINK_MTU,
                    )
                    .unwrap(),
                },
                crate::testutil::DEFAULT_INTERFACE_METRIC,
            )
            .into();

        let gateway = SpecifiedAddr::new(
            // Set the last bit to make it an address inside the fake config's
            // subnet.
            I::map_ip::<_, I::Addr>(
                I::TEST_ADDRS.subnet.network(),
                |addr| {
                    let mut bytes = addr.ipv4_bytes();
                    bytes[bytes.len() - 1] = 1;
                    Ipv4Addr::from(bytes)
                },
                |addr| {
                    let mut bytes = addr.ipv6_bytes();
                    bytes[bytes.len() - 1] = 1;
                    Ipv6Addr::from(bytes)
                },
            )
            .to_ip_addr(),
        )
        .expect("should be specified");

        // Try to resolve a device for a gateway that we have no route to.
        assert_eq!(ctx.core_api().routes_any().select_device_for_gateway(gateway), None);

        // Add a route to the gateway.
        let route_to_add = if on_link_route {
            AddableEntryEither::from(AddableEntry::without_gateway(
                I::TEST_ADDRS.subnet,
                device_id.clone(),
                AddableMetric::ExplicitMetric(RawMetric(0)),
            ))
        } else {
            AddableEntryEither::from(AddableEntry::with_gateway(
                I::TEST_ADDRS.subnet,
                device_id.clone(),
                I::TEST_ADDRS.remote_ip,
                AddableMetric::ExplicitMetric(RawMetric(0)),
            ))
        };

        assert_eq!(ctx.test_api().add_route(route_to_add), Ok(()));

        // It still won't resolve successfully because the device is not enabled yet.
        assert_eq!(ctx.core_api().routes_any().select_device_for_gateway(gateway), None);

        ctx.test_api().enable_device(&device_id);

        // Now, try to resolve a device for the gateway.
        assert_eq!(
            ctx.core_api().routes_any().select_device_for_gateway(gateway),
            if on_link_route { Some(device_id) } else { None }
        );
    }

    struct AddGatewayRouteTestCase {
        enable_before_final_route_add: bool,
        expected_first_result: Result<(), AddRouteError>,
        expected_second_result: Result<(), AddRouteError>,
    }

    #[ip_test]
    #[test_case(AddGatewayRouteTestCase {
        enable_before_final_route_add: false,
        expected_first_result: Ok(()),
        expected_second_result: Ok(()),
    }; "with_specified_device_no_enable")]
    #[test_case(AddGatewayRouteTestCase {
        enable_before_final_route_add: true,
        expected_first_result: Ok(()),
        expected_second_result: Ok(()),
    }; "with_specified_device_enabled")]
    fn add_gateway_route<I: Ip + TestIpExt>(test_case: AddGatewayRouteTestCase) {
        let AddGatewayRouteTestCase {
            enable_before_final_route_add,
            expected_first_result,
            expected_second_result,
        } = test_case;
        let mut ctx =
            crate::testutil::FakeCtx::new_with_builder(crate::state::StackStateBuilder::default());
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        let gateway_subnet = I::map_ip(
            (),
            |()| net_subnet_v4!("10.0.0.0/16"),
            |()| net_subnet_v6!("::0a00:0000/112"),
        );

        let device_id: DeviceId<_> = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: I::TEST_ADDRS.local_mac,
                    max_frame_size: crate::device::ethernet::MaxEthernetFrameSize::from_mtu(
                        I::MINIMUM_LINK_MTU,
                    )
                    .unwrap(),
                },
                crate::testutil::DEFAULT_INTERFACE_METRIC,
            )
            .into();
        let gateway_device = device_id.clone();

        // Attempt to add the gateway route when there is no known route to the
        // gateway.
        assert_eq!(
            ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::with_gateway(
                gateway_subnet,
                gateway_device.clone(),
                I::TEST_ADDRS.remote_ip,
                AddableMetric::ExplicitMetric(RawMetric(0))
            ))),
            expected_first_result,
        );

        assert_eq!(
            ctx.test_api().del_routes_to_subnet(gateway_subnet.into()),
            expected_first_result.map_err(|_: AddRouteError| crate::error::NotFoundError),
        );

        // Then, add a route to the gateway, and try again, expecting success.
        assert_eq!(
            ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::without_gateway(
                I::TEST_ADDRS.subnet,
                device_id.clone(),
                AddableMetric::ExplicitMetric(RawMetric(0))
            ))),
            Ok(())
        );

        if enable_before_final_route_add {
            ctx.test_api().enable_device(&device_id);
        }
        assert_eq!(
            ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::with_gateway(
                gateway_subnet,
                gateway_device,
                I::TEST_ADDRS.remote_ip,
                AddableMetric::ExplicitMetric(RawMetric(0))
            ))),
            expected_second_result,
        );
    }

    #[ip_test]
    fn test_route_tracks_interface_metric<I: Ip + TestIpExt>() {
        let mut ctx = crate::testutil::Ctx::<crate::testutil::FakeBindingsCtx>::new_with_builder(
            crate::state::StackStateBuilder::default(),
        );
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();

        let metric = RawMetric(9999);
        let device_id =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: I::TEST_ADDRS.local_mac,
                    max_frame_size: crate::device::ethernet::MaxEthernetFrameSize::from_mtu(
                        I::MINIMUM_LINK_MTU,
                    )
                    .unwrap(),
                },
                metric,
            );
        assert_eq!(
            ctx.test_api().add_route(AddableEntryEither::from(AddableEntry::without_gateway(
                I::TEST_ADDRS.subnet,
                device_id.clone().into(),
                AddableMetric::MetricTracksInterface
            ))),
            Ok(())
        );
        assert_eq!(
            ctx.core_api().routes_any().get_all_routes(),
            &[Entry {
                subnet: I::TEST_ADDRS.subnet,
                device: device_id.clone().into(),
                gateway: None,
                metric: Metric::MetricTracksInterface(metric)
            }
            .into()]
        );

        // Remove the device and routes to clear all dangling references.
        ctx.test_api().clear_routes_and_remove_ethernet_device(device_id);
    }
}
