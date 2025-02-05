// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Defines the public API exposed to bindings by the IP module.

use alloc::format;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;
use assert_matches::assert_matches;
use net_types::ip::{Ip, IpAddr, IpVersionMarker, Ipv4, Ipv6};
use net_types::{SpecifiedAddr, Witness as _};

use netstack3_base::sync::{PrimaryRc, RwLock};
use netstack3_base::{
    AnyDevice, ContextPair, DeferredResourceRemovalContext, DeviceIdContext, Inspector,
    InspectorDeviceExt, ReferenceNotifiersExt as _, RemoveResourceResultWithContext,
    StrongDeviceIdentifier, WrapBroadcastMarker,
};

use crate::internal::base::{
    self, IpLayerBindingsContext, IpLayerContext, IpLayerIpExt, IpRouteTableContext,
    IpRouteTablesContext, IpStateContext as _, ResolveRouteError, RoutingTableId,
};
use crate::internal::device::{
    IpDeviceBindingsContext, IpDeviceConfigurationContext, IpDeviceIpExt,
};
use crate::internal::routing::rules::{MarkMatcher, Marks, Rule, RuleAction, RuleMatcher};
use crate::internal::routing::RoutingTable;
use crate::internal::types::{
    Destination, Entry, EntryAndGeneration, Metric, NextHop, OrderedEntry, ResolvedRoute,
    RoutableIpAddr,
};

/// The routes API for a specific IP version `I`.
pub struct RoutesApi<I: Ip, C>(C, IpVersionMarker<I>);

impl<I: Ip, C> RoutesApi<I, C> {
    /// Creates a new API instance.
    pub fn new(ctx: C) -> Self {
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

    /// Allocates a new table in Core.
    pub fn new_table(
        &mut self,
        bindings_id: impl Into<u32>,
    ) -> RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId> {
        self.core_ctx().with_ip_routing_tables_mut(|tables| {
            let new_table =
                PrimaryRc::new(RwLock::new(RoutingTable::with_bindings_id(bindings_id.into())));
            let table_id = RoutingTableId::new(PrimaryRc::clone_strong(&new_table));
            assert_matches!(tables.insert(table_id.clone(), new_table), None);
            table_id
        })
    }

    /// Removes a table in Core.
    ///
    /// # Panics
    ///
    /// Panics if `id` is the main table id.
    pub fn remove_table(
        &mut self,
        id: RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    ) -> RemoveResourceResultWithContext<(), C::BindingsContext> {
        assert!(id != self.main_table_id(), "main table should never be removed");
        self.core_ctx().with_ip_routing_tables_mut(|tables| {
            let table = assert_matches!(
                tables.remove(&id),
                Some(removed) => removed
            );
            C::BindingsContext::unwrap_or_notify_with_new_reference_notifier(table, |_| ())
        })
    }

    /// Gets the core ID of the main table.
    pub fn main_table_id(
        &mut self,
    ) -> RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId> {
        self.core_ctx().main_table_id()
    }

    /// Collects all the routes in the specified table into `target`.
    pub fn collect_routes_into<
        X: From<Entry<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>>,
        T: Extend<X>,
    >(
        &mut self,
        table_id: &RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        target: &mut T,
    ) {
        self.core_ctx().with_ip_routing_table(table_id, |_core_ctx, table| {
            target.extend(table.iter_table().cloned().map(Into::into))
        })
    }

    /// Collects all the routes in the main table into `target`.
    pub fn collect_main_table_routes_into<
        X: From<Entry<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>>,
        T: Extend<X>,
    >(
        &mut self,
        target: &mut T,
    ) {
        self.core_ctx().with_main_ip_routing_table(|_core_ctx, table| {
            target.extend(table.iter_table().cloned().map(Into::into))
        })
    }

    /// Like the Iterator fold accumulator.
    ///
    /// Applies the given `cb` to each route across all routing tables.
    pub fn fold_routes<B, F>(&mut self, init: B, mut cb: F) -> B
    where
        F: FnMut(
            B,
            &RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
            &Entry<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        ) -> B,
    {
        self.core_ctx().with_ip_routing_tables(|ctx, tables| {
            tables.keys().fold(init, |state, table_id| {
                ctx.with_ip_routing_table(table_id, |_ctx, table| {
                    table.iter_table().fold(state, |state, entry| cb(state, table_id, entry))
                })
            })
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
        base::resolve_output_route_to_destination(
            self.core_ctx(),
            None,
            None,
            destination,
            &Marks::default(),
        )
    }

    /// Selects the device to use for gateway routes when the device was
    /// unspecified by the client.
    pub fn select_device_for_gateway(
        &mut self,
        gateway: SpecifiedAddr<I::Addr>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId> {
        self.core_ctx().with_main_ip_routing_table_mut(|core_ctx, table| {
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

    /// Writes rules and routes tables information to the provided `inspector`.
    pub fn inspect<N: Inspector>(&mut self, inspector: &mut N, main_table_id: u32)
    where
        for<'a> N::ChildInspector<'a>:
            InspectorDeviceExt<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    {
        inspector.record_child("Rules", |inspector| {
            self.inspect_rules(inspector, main_table_id);
        });
        inspector.record_child("RoutingTables", |inspector| {
            self.inspect_routes(inspector, main_table_id);
        })
    }

    /// Writes rules table information to the provided `inspector`.
    pub fn inspect_rules<N: Inspector>(&mut self, inspector: &mut N, main_table_id: u32) {
        self.core_ctx().with_rules_table(|core_ctx, rule_table| {
            for Rule {
                matcher:
                    RuleMatcher { source_address_matcher, traffic_origin_matcher, mark_matchers },
                action,
            } in rule_table.iter()
            {
                inspector.record_unnamed_child(|inspector| {
                    inspector.record_child("Matchers", |inspector| {
                        let source_address_matcher = source_address_matcher
                            .map_or_else(|| String::new(), |m| m.0.to_string());
                        inspector.record_str("SourceAddressMatcher", &source_address_matcher);
                        inspector.record_debug("TrafficOriginMatcher", traffic_origin_matcher);
                        inspector.record_child("MarkMatchers", |inspector| {
                            for (domain, matcher) in
                                mark_matchers.iter().filter_map(|(d, m)| m.map(|m| (d, m)))
                            {
                                match matcher {
                                    MarkMatcher::Unmarked => {
                                        inspector.record_str(domain.to_str(), "Unmarked")
                                    }
                                    MarkMatcher::Marked { start, end, mask } => inspector
                                        .record_debug_child(domain, |inspector| {
                                            inspector.record_str("Mask", &format!("{mask:#010x}"));
                                            inspector.record_str(
                                                "Range",
                                                &format!("{start:#x}..{end:#x}"),
                                            );
                                        }),
                                }
                            }
                        });
                    });
                    inspector.record_child("Action", |inspector| match action {
                        RuleAction::Unreachable => inspector.record_str("Action", "Unreachable"),
                        RuleAction::Lookup(table_id) => {
                            let bindings_id = core_ctx
                                .with_ip_routing_table(table_id, |_core_ctx, table| {
                                    table.bindings_id
                                });
                            let bindings_id = bindings_id.unwrap_or(main_table_id);
                            inspector.record_str("Lookup", &format!("{bindings_id}"))
                        }
                    })
                })
            }
        })
    }

    /// Writes routing table information to the provided `inspector`.
    pub fn inspect_routes<
        N: Inspector + InspectorDeviceExt<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    >(
        &mut self,
        inspector: &mut N,
        main_table_id: u32,
    ) {
        self.core_ctx().with_ip_routing_tables(|core_ctx, tables| {
            for table_id in tables.keys() {
                core_ctx.with_ip_routing_table(table_id, |_core_ctx, table| {
                    let bindings_id = table.bindings_id.unwrap_or(main_table_id);
                    inspector.record_display_child(bindings_id, |inspector| {
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
                })
            }
        });
    }

    /// Replaces the entire route table atomically.
    pub fn set_routes(
        &mut self,
        table_id: &RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        mut entries: Vec<
            EntryAndGeneration<I::Addr, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        >,
    ) {
        // Make sure to sort the entries _before_ taking the routing table lock.
        entries.sort_unstable_by(|a, b| {
            OrderedEntry::<'_, _, _>::from(a).cmp(&OrderedEntry::<'_, _, _>::from(b))
        });
        self.core_ctx().with_ip_routing_table_mut(table_id, |_core_ctx, table| {
            table.table = entries;
        });
    }

    /// Replaces the entire rule table atomically.
    pub fn set_rules(
        &mut self,
        rules: Vec<Rule<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>>,
    ) {
        self.core_ctx().with_rules_table_mut(|_core_ctx, rule_table| {
            rule_table.replace(rules);
        })
    }

    /// Gets all table IDs.
    #[cfg(feature = "testutils")]
    pub fn list_table_ids(
        &mut self,
    ) -> Vec<RoutingTableId<I, <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>> {
        self.core_ctx().with_ip_routing_tables(|_ctx, tables| tables.keys().cloned().collect())
    }
}

/// The routes API interacting with all IP versions.
pub struct RoutesAnyApi<C>(C);

impl<C> RoutesAnyApi<C> {
    /// Creates a new API instance.
    pub fn new(ctx: C) -> Self {
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

    #[cfg(feature = "testutils")]
    /// Gets all the installed routes.
    pub fn get_all_routes_in_main_table(
        &mut self,
    ) -> Vec<
        crate::internal::types::EntryEither<
            <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
        >,
    > {
        let mut vec = Vec::new();
        self.ip::<Ipv4>().collect_main_table_routes_into(&mut vec);
        self.ip::<Ipv6>().collect_main_table_routes_into(&mut vec);
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
    IpDeviceBindingsContext<I, D> + IpLayerBindingsContext<I, D> + DeferredResourceRemovalContext
where
    D: StrongDeviceIdentifier,
    I: IpLayerIpExt + IpDeviceIpExt,
{
}

impl<I, D, BC> RoutesApiBindingsContext<I, D> for BC
where
    D: StrongDeviceIdentifier,
    I: IpLayerIpExt + IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, D>
        + IpLayerBindingsContext<I, D>
        + DeferredResourceRemovalContext,
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
