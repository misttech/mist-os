// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inspect utilities.
//!
//! This module provides utilities for publishing netstack3 diagnostics data to
//! Inspect.

use std::{fmt::Debug, string::ToString as _};

use fuchsia_inspect::Node;
use net_types::{
    ip::{Ip, IpAddress, Ipv4, Ipv6},
    Witness as _,
};
use netstack3_core::{
    device::{DeviceId, EthernetLinkDevice, WeakDeviceId},
    neighbor,
};

use crate::bindings::{
    devices::{
        DeviceIdAndName, DeviceSpecificInfo, DynamicCommonInfo, DynamicNetdeviceInfo, NetdeviceInfo,
    },
    BindingsCtx, Ctx, DeviceIdExt as _, StackTime,
};

/// A visitor for diagnostics data that has distinct Ipv4 and Ipv6 variants.
struct DualIpVisitor<'a> {
    node: &'a Node,
    count: usize,
}

impl<'a> DualIpVisitor<'a> {
    fn new(node: &'a Node) -> Self {
        Self { node, count: 0 }
    }

    /// Records a child Inspect node with an incrementing id that is unique
    /// across IP versions.
    fn record_unique_child<F>(&mut self, f: F)
    where
        F: FnOnce(&Node),
    {
        let Self { node, count } = self;
        let id = core::mem::replace(count, *count + 1);
        node.record_child(format!("{id}"), f)
    }
}

struct BindingsInspector<'a> {
    node: &'a Node,
    unnamed_count: usize,
}

impl<'a> BindingsInspector<'a> {
    fn new(node: &'a Node) -> Self {
        Self { node, unnamed_count: 0 }
    }
}

impl<'a> netstack3_core::inspect::Inspector for BindingsInspector<'a> {
    type ChildInspector<'l> = BindingsInspector<'l>;

    fn record_child<F: FnOnce(&mut Self::ChildInspector<'_>)>(&mut self, name: &str, f: F) {
        self.node.record_child(name, |node| f(&mut BindingsInspector::new(node)))
    }

    fn record_unnamed_child<F: FnOnce(&mut Self::ChildInspector<'_>)>(&mut self, f: F) {
        let Self { node: _, unnamed_count } = self;
        let id = core::mem::replace(unnamed_count, *unnamed_count + 1);
        self.record_child(&format!("{id}"), f)
    }

    fn record_uint<T: Into<u64>>(&mut self, name: &str, value: T) {
        self.node.record_uint(name, value.into())
    }

    fn record_int<T: Into<i64>>(&mut self, name: &str, value: T) {
        self.node.record_int(name, value.into())
    }

    fn record_double<T: Into<f64>>(&mut self, name: &str, value: T) {
        self.node.record_double(name, value.into())
    }

    fn record_str(&mut self, name: &str, value: &str) {
        self.node.record_string(name, value)
    }

    fn record_string(&mut self, name: &str, value: String) {
        self.node.record_string(name, value)
    }

    fn record_bool(&mut self, name: &str, value: bool) {
        self.node.record_bool(name, value)
    }
}

impl<'a> netstack3_core::inspect::SocketAddressZoneProvider<WeakDeviceId<BindingsCtx>>
    for BindingsInspector<'a>
{
    fn device_identifier_as_address_zone(id: WeakDeviceId<BindingsCtx>) -> impl std::fmt::Display {
        id.bindings_id().id
    }
}

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let mut bindings_inspector = BindingsInspector::new(inspector.root());
    ctx.api().tcp::<Ipv4>().inspect(&mut bindings_inspector);
    ctx.api().tcp::<Ipv6>().inspect(&mut bindings_inspector);
    inspector
}

/// Publishes netstack3 routing table diagnostics data to Inspect.
pub(crate) fn routes(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    impl<'a, I: Ip> netstack3_core::routes::RoutesVisitor<'a, I, DeviceId<BindingsCtx>>
        for DualIpVisitor<'a>
    {
        fn visit<'b>(
            &mut self,
            per_route: impl Iterator<Item = &'b netstack3_core::routes::Entry<I::Addr, DeviceId<BindingsCtx>>>
                + 'b,
        ) where
            'a: 'b,
        {
            for route in per_route {
                self.record_unique_child(|node| {
                    let netstack3_core::routes::Entry { subnet, device, gateway, metric } = route;
                    node.record_string("Destination", format!("{}", subnet));
                    node.record_uint("InterfaceId", device.bindings_id().id.into());
                    match gateway {
                        Some(gateway) => {
                            node.record_string("Gateway", format!("{}", gateway));
                        }
                        None => {
                            node.record_string("Gateway", "[NONE]");
                        }
                    }
                    match metric {
                        netstack3_core::routes::Metric::MetricTracksInterface(metric) => {
                            node.record_uint("Metric", (*metric).into());
                            node.record_bool("MetricTracksInterface", true);
                        }
                        netstack3_core::routes::Metric::ExplicitMetric(metric) => {
                            node.record_uint("Metric", (*metric).into());
                            node.record_bool("MetricTracksInterface", false);
                        }
                    }
                })
            }
        }
    }
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let mut visitor = DualIpVisitor::new(inspector.root());
    ctx.api().routes::<Ipv4>().with_routes(&mut visitor);
    ctx.api().routes::<Ipv6>().with_routes(&mut visitor);
    inspector
}

pub(crate) fn devices(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    // Snapshot devices out so we're not holding onto the devices lock for too
    // long.
    let devices =
        ctx.bindings_ctx().devices.with_devices(|devices| devices.cloned().collect::<Vec<_>>());
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let node = inspector.root();
    for device_id in devices {
        let external_state = device_id.external_state();
        let DeviceIdAndName { id: binding_id, name } = device_id.bindings_id();
        node.record_child(format!("{binding_id}"), |node| {
            node.record_string("Name", &name);
            node.record_uint("InterfaceId", (*binding_id).into());
            node.record_child("IPv4", |node| {
                ctx.api().device_ip::<Ipv4>().inspect(&device_id, &mut BindingsInspector::new(node))
            });
            node.record_child("IPv6", |node| {
                ctx.api().device_ip::<Ipv6>().inspect(&device_id, &mut BindingsInspector::new(node))
            });
            external_state.with_common_info(
                |DynamicCommonInfo {
                     admin_enabled,
                     mtu,
                     addresses: _,
                     control_hook: _,
                     events: _,
                 }| {
                    node.record_bool("AdminEnabled", *admin_enabled);
                    node.record_uint("MTU", mtu.get().into());
                },
            );
            match external_state {
                DeviceSpecificInfo::Netdevice(
                    info @ NetdeviceInfo { mac, dynamic: _, handler: _, static_common_info: _ },
                ) => {
                    node.record_bool("Loopback", false);
                    node.record_child("NetworkDevice", |node| {
                        node.record_string("MacAddress", mac.get().to_string());
                        info.with_dynamic_info(
                            |DynamicNetdeviceInfo {
                                 phy_up,
                                 common_info: _,
                                 neighbor_event_sink: _,
                             }| {
                                node.record_bool("PhyUp", *phy_up);
                            },
                        );
                    });
                }
                DeviceSpecificInfo::Loopback(_info) => {
                    node.record_bool("Loopback", true);
                }
                // TODO(https://fxbug.dev/42051633): Add relevant
                // inspect data for pure IP devices.
                DeviceSpecificInfo::PureIp(_info) => {
                    node.record_bool("loopback", false);
                }
            }
        })
    }
    inspector
}

pub(crate) fn neighbors(mut ctx: Ctx) -> fuchsia_inspect::Inspector {
    impl<'a, A: IpAddress, LinkAddress: Debug> neighbor::NeighborVisitor<A, LinkAddress, StackTime>
        for DualIpVisitor<'a>
    {
        fn visit_neighbors(
            &mut self,
            neighbors: impl Iterator<Item = neighbor::NeighborStateInspect<A, LinkAddress, StackTime>>,
        ) {
            for neighbor in neighbors {
                let netstack3_core::neighbor::NeighborStateInspect {
                    state,
                    ip_address,
                    link_address,
                    last_confirmed_at,
                } = neighbor;
                self.record_unique_child(|node| {
                    node.record_string("State", state);
                    node.record_string("IpAddress", format!("{}", ip_address));
                    if let Some(link_address) = link_address {
                        node.record_string("LinkAddress", format!("{:?}", link_address));
                    };
                    if let Some(StackTime(last_confirmed_at)) = last_confirmed_at {
                        node.record_int("LastConfirmedAt", last_confirmed_at.into_nanos());
                    }
                })
            }
        }
    }
    let inspector = fuchsia_inspect::Inspector::new(Default::default());

    // Get a snapshot of all supported devices. Ethernet is the only device type
    // that supports neighbors.
    let ethernet_devices = ctx.bindings_ctx().devices.with_devices(|devices| {
        devices
            .filter_map(|d| match d {
                DeviceId::Ethernet(d) => Some(d.clone()),
                // NUD is not supported on Loopback or pure IP devices.
                DeviceId::Loopback(_) | DeviceId::PureIp(_) => None,
            })
            .collect::<Vec<_>>()
    });
    for device in ethernet_devices {
        inspector.root().record_child(&device.bindings_id().name, |node| {
            let mut visitor = DualIpVisitor::new(node);
            ctx.api()
                .neighbor::<Ipv4, EthernetLinkDevice>()
                .inspect_neighbors(&device, &mut visitor);
            ctx.api()
                .neighbor::<Ipv6, EthernetLinkDevice>()
                .inspect_neighbors(&device, &mut visitor);
        });
    }
    inspector
}

pub(crate) fn counters(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    ctx.api().counters().inspect_stack_counters(&mut BindingsInspector::new(inspector.root()));
    inspector
}
