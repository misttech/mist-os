// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inspect utilities.
//!
//! This module provides utilities for publishing netstack3 diagnostics data to
//! Inspect.

use std::{fmt::Display, string::ToString as _};

use fuchsia_inspect::Node;
use net_types::{
    ethernet::Mac,
    ip::{Ipv4, Ipv6},
    UnicastAddr, Witness as _,
};
use netstack3_core::device::{DeviceId, EthernetLinkDevice, WeakDeviceId};

use tracing::warn;

use crate::bindings::{
    devices::{
        DeviceIdAndName, DeviceSpecificInfo, DynamicCommonInfo, DynamicNetdeviceInfo, EthernetInfo,
    },
    BindingsCtx, Ctx, DeviceIdExt as _,
};

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

    fn record_usize<T: Into<usize>>(&mut self, name: &str, value: T) {
        let value: u64 = value.into().try_into().unwrap_or_else(|e| {
            warn!("failed to inspect usize value that does not fit in a u64: {e:?}");
            u64::MAX
        });
        self.node.record_uint(name, value)
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

impl<'a> netstack3_core::inspect::InspectorDeviceExt<DeviceId<BindingsCtx>>
    for BindingsInspector<'a>
{
    fn record_device<I: netstack3_core::Inspector>(
        inspector: &mut I,
        name: &str,
        device: &DeviceId<BindingsCtx>,
    ) {
        inspector.record_uint(name, device.bindings_id().id)
    }

    fn device_identifier_as_address_zone(id: DeviceId<BindingsCtx>) -> impl Display {
        id.bindings_id().id
    }
}

impl<'a> netstack3_core::inspect::InspectorDeviceExt<WeakDeviceId<BindingsCtx>>
    for BindingsInspector<'a>
{
    fn record_device<I: netstack3_core::Inspector>(
        inspector: &mut I,
        name: &str,
        device: &WeakDeviceId<BindingsCtx>,
    ) {
        inspector.record_uint(name, device.bindings_id().id)
    }

    fn device_identifier_as_address_zone(id: WeakDeviceId<BindingsCtx>) -> impl Display {
        id.bindings_id().id
    }
}

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let mut bindings_inspector = BindingsInspector::new(inspector.root());
    ctx.api().tcp::<Ipv4>().inspect(&mut bindings_inspector);
    ctx.api().tcp::<Ipv6>().inspect(&mut bindings_inspector);
    ctx.api().udp::<Ipv4>().inspect(&mut bindings_inspector);
    ctx.api().udp::<Ipv6>().inspect(&mut bindings_inspector);
    ctx.api().icmp_echo::<Ipv4>().inspect(&mut bindings_inspector);
    ctx.api().icmp_echo::<Ipv6>().inspect(&mut bindings_inspector);
    inspector
}

/// Publishes netstack3 routing table diagnostics data to Inspect.
pub(crate) fn routes(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let mut bindings_inspector = BindingsInspector::new(inspector.root());
    ctx.api().routes::<Ipv4>().inspect(&mut bindings_inspector);
    ctx.api().routes::<Ipv6>().inspect(&mut bindings_inspector);
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
                DeviceSpecificInfo::Ethernet(info @ EthernetInfo { mac, .. }) => {
                    node.record_bool("Loopback", false);
                    info.with_dynamic_info(|dynamic| {
                        record_network_device(node, &dynamic.netdevice, Some(mac))
                    });
                }
                DeviceSpecificInfo::Loopback(_info) => {
                    node.record_bool("Loopback", true);
                }
                DeviceSpecificInfo::PureIp(info) => {
                    node.record_bool("Loopback", false);
                    info.with_dynamic_info(|dynamic| record_network_device(node, dynamic, None));
                }
            }
            ctx.api().device_any().inspect(&device_id, &mut BindingsInspector::new(node));
        })
    }
    inspector
}

/// Record information about the Netdevice as a child of the given inspect node.
fn record_network_device(
    node: &Node,
    DynamicNetdeviceInfo { phy_up, common_info: _ }: &DynamicNetdeviceInfo,
    mac: Option<&UnicastAddr<Mac>>,
) {
    node.record_child("NetworkDevice", |node| {
        if let Some(mac) = mac {
            node.record_string("MacAddress", mac.get().to_string());
        }
        node.record_bool("PhyUp", *phy_up);
    })
}

pub(crate) fn neighbors(mut ctx: Ctx) -> fuchsia_inspect::Inspector {
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
            let mut inspector = BindingsInspector::new(node);
            ctx.api()
                .neighbor::<Ipv4, EthernetLinkDevice>()
                .inspect_neighbors(&device, &mut inspector);
            ctx.api()
                .neighbor::<Ipv6, EthernetLinkDevice>()
                .inspect_neighbors(&device, &mut inspector);
        });
    }
    inspector
}

pub(crate) fn counters(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    ctx.api().counters().inspect_stack_counters(&mut BindingsInspector::new(inspector.root()));
    inspector
}
