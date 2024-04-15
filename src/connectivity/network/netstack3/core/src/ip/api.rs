// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the public API exposed to bindings by the IP module.

use alloc::vec::Vec;
use net_types::{
    ip::{Ip, IpAddr, IpVersionMarker, Ipv4, Ipv6},
    SpecifiedAddr, Witness as _,
};

use crate::{
    context::ContextPair,
    device::{self, AnyDevice, DeviceIdContext},
    inspect::{Inspector, InspectorDeviceExt},
    ip::{
        base::{IpLayerBindingsContext, IpLayerContext, ResolveRouteError},
        device::{IpDeviceBindingsContext, IpDeviceConfigurationContext, IpDeviceIpExt},
        types::{
            Destination, Entry, EntryAndGeneration, EntryEither, Metric, NextHop, OrderedEntry,
            ResolvedRoute, RoutableIpAddr, WrapBroadcastMarker,
        },
        IpLayerIpExt, IpStateContext,
    },
};

/// The routes API for a specific IP version `I`.
pub struct RoutesApi<I: Ip, C>(C, IpVersionMarker<I>);

impl<I: Ip, C> RoutesApi<I, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, IpVersionMarker::new())
    }
}

impl<I, C> RoutesApi<I, C>
where
    I: IpLayerIpExt + IpDeviceIpExt,
    C: ContextPair,
    C::CoreContext: RoutesApiCoreContext<I, C::BindingsContext>,
    C::BindingsContext:
        RoutesApiBindingsContext<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId: Ord,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair, IpVersionMarker { .. }) = self;
        pair.core_ctx()
    }

    /// Collects all the routes into `target`.
    pub fn collect_routes_into<
        X: From<Entry<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>>,
        T: Extend<X>,
    >(
        &mut self,
        target: &mut T,
    ) {
        self.core_ctx().with_ip_routing_table(|_core_ctx, table| {
            target.extend(table.iter_table().cloned().map(Into::into))
        })
    }

    /// Resolve the route to a given destination.
    ///
    /// Returns `Some` [`ResolvedRoute`] with details for reaching the destination,
    /// or `None` if the destination is unreachable.
    pub fn resolve_route(
        &mut self,
        destination: Option<RoutableIpAddr<I::Addr>>,
    ) -> Result<
        ResolvedRoute<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        ResolveRouteError,
    > {
        crate::ip::base::resolve_route_to_destination(self.core_ctx(), None, None, destination)
    }

    /// Selects the device to use for gateway routes when the device was
    /// unspecified by the client.
    pub fn select_device_for_gateway(
        &mut self,
        gateway: SpecifiedAddr<I::Addr>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId> {
        self.core_ctx().with_ip_routing_table_mut(|core_ctx, table| {
            table.lookup(core_ctx, None, *gateway).and_then(
                |Destination { next_hop: found_next_hop, device: found_device }| {
                    match found_next_hop {
                        NextHop::RemoteAsNeighbor => Some(found_device),
                        NextHop::Broadcast(marker) => {
                            I::map_ip::<_, ()>(
                                WrapBroadcastMarker(marker),
                                |WrapBroadcastMarker(())| (),
                                |WrapBroadcastMarker(never)| match never {},
                            );
                            Some(found_device)
                        }
                        NextHop::Gateway(_intermediary_gateway) => None,
                    }
                },
            )
        })
    }

    /// Writes routing table information to the provided `inspector`.
    pub fn inspect<
        'a,
        N: Inspector + InspectorDeviceExt<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    >(
        &mut self,
        inspector: &mut N,
    ) {
        self.core_ctx().with_ip_routing_table(|_core_ctx, table| {
            for Entry { subnet, device, gateway, metric } in table.iter_table() {
                inspector.record_unnamed_child(|inspector| {
                    inspector.record_display("Destination", subnet);
                    N::record_device(inspector, "InterfaceId", device);
                    match gateway {
                        Some(gateway) => {
                            inspector.record_ip_addr("Gateway", gateway.get());
                        }
                        None => {
                            inspector.record_str("Gateway", "[NONE]");
                        }
                    }
                    let (metric, tracks_interface) = match metric {
                        Metric::MetricTracksInterface(metric) => (metric, true),
                        Metric::ExplicitMetric(metric) => (metric, false),
                    };
                    inspector.record_uint("Metric", *metric);
                    inspector.record_bool("MetricTracksInterface", tracks_interface);
                });
            }
        })
    }

    /// Set the routes in the routing table.
    ///
    /// While doing a full `set` of the routing table with each modification is
    /// suboptimal for performance, it simplifies the API exposed by core for route
    /// table modifications to allow for evolution of the routing table in the
    /// future.
    pub fn set_routes(
        &mut self,
        mut entries: Vec<
            EntryAndGeneration<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        >,
    ) {
        // Make sure to sort the entries _before_ taking the routing table lock.
        entries.sort_unstable_by(|a, b| {
            OrderedEntry::<'_, _, _>::from(a).cmp(&OrderedEntry::<'_, _, _>::from(b))
        });
        self.core_ctx().with_ip_routing_table_mut(|_core_ctx, table| {
            table.table = entries;
        });
    }
}

/// The routes API interacting with all IP versions.
pub struct RoutesAnyApi<C>(C);

impl<C> RoutesAnyApi<C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

impl<C> RoutesAnyApi<C>
where
    C: ContextPair,
    C::CoreContext: RoutesApiCoreContext<Ipv4, C::BindingsContext>
        + RoutesApiCoreContext<Ipv6, C::BindingsContext>,
    C::BindingsContext: RoutesApiBindingsContext<Ipv4, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>
        + RoutesApiBindingsContext<Ipv6, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId: Ord,
{
    fn ip<I: Ip>(&mut self) -> RoutesApi<I, &mut C> {
        let Self(pair) = self;
        RoutesApi::new(pair)
    }

    /// Gets all the installed routes.
    pub fn get_all_routes(
        &mut self,
    ) -> Vec<EntryEither<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>> {
        let mut vec = Vec::new();
        self.ip::<Ipv4>().collect_routes_into(&mut vec);
        self.ip::<Ipv6>().collect_routes_into(&mut vec);
        vec
    }

    /// Like [`RoutesApi::select_device_for_gateway`] but for any IP version.
    pub fn select_device_for_gateway(
        &mut self,
        gateway: SpecifiedAddr<IpAddr>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId> {
        match gateway.into() {
            IpAddr::V4(gateway) => self.ip::<Ipv4>().select_device_for_gateway(gateway),
            IpAddr::V6(gateway) => self.ip::<Ipv6>().select_device_for_gateway(gateway),
        }
    }
}

/// A marker trait for all the bindings context traits required to fulfill the
/// [`RoutesApi`].
pub trait RoutesApiBindingsContext<I, D>:
    IpDeviceBindingsContext<I, D> + IpLayerBindingsContext<I, D>
where
    D: device::StrongId,
    I: IpLayerIpExt + IpDeviceIpExt,
{
}

impl<I, D, BC> RoutesApiBindingsContext<I, D> for BC
where
    D: device::StrongId,
    I: IpLayerIpExt + IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, D> + IpLayerBindingsContext<I, D>,
{
}

/// A marker trait for all the core context traits required to fulfill the
/// [`RoutesApi`].
pub trait RoutesApiCoreContext<I, BC>:
    IpLayerContext<I, BC> + IpDeviceConfigurationContext<I, BC>
where
    I: IpLayerIpExt + IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, <Self as DeviceIdContext<AnyDevice>>::DeviceId>
        + IpLayerBindingsContext<I, <Self as DeviceIdContext<AnyDevice>>::DeviceId>,
{
}

impl<I, BC, CC> RoutesApiCoreContext<I, BC> for CC
where
    CC: IpLayerContext<I, BC> + IpDeviceConfigurationContext<I, BC>,
    I: IpLayerIpExt + IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, <Self as DeviceIdContext<AnyDevice>>::DeviceId>
        + IpLayerBindingsContext<I, <Self as DeviceIdContext<AnyDevice>>::DeviceId>,
{
}
