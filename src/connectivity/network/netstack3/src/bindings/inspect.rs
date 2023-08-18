// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inspect utilities.
//!
//! This module provides utilities for publishing netstack3 diagnostics data to
//! Inspect.

use super::{devices::DeviceSpecificInfo, BindingsNonSyncCtxImpl, Ctx, DeviceIdExt};
use fuchsia_inspect::ArrayProperty as _;
use net_types::{
    ip::{Ip, IpAddress, IpVersion, Ipv4, Ipv6},
    Witness as _,
};
use netstack3_core::{device::DeviceId, ip, transport::tcp};
use std::{borrow::Cow, num::NonZeroU16, ops::Deref, string::ToString as _};

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    /// Convert a [`tcp::socket::SocketId`] into a unique integer.
    ///
    /// Guarantees that no two unique `SocketId`s (even for different IP
    /// versions) will have have the same output value.
    fn transform_id<I: Ip>(id: tcp::socket::SocketId<I>) -> usize {
        let unique_for_ip_version: usize = id.into();
        2 * unique_for_ip_version
            + match I::VERSION {
                IpVersion::V4 => 0,
                IpVersion::V6 => 1,
            }
    }

    struct Visitor(fuchsia_inspect::Inspector);
    impl tcp::socket::InfoVisitor for &'_ mut Visitor {
        type VisitResult = ();
        fn visit<I: Ip>(
            self,
            per_socket: impl Iterator<Item = tcp::socket::SocketStats<I>>,
        ) -> Self::VisitResult {
            let Visitor(inspector) = self;
            for socket in per_socket {
                let tcp::socket::SocketStats { id, local, remote } = socket;
                inspector.root().record_child(format!("{}", transform_id(id)), |node| {
                    node.record_string("TransportProtocol", "TCP");
                    node.record_string(
                        "NetworkProtocol",
                        match I::VERSION {
                            IpVersion::V4 => "IPv4",
                            IpVersion::V6 => "IPv6",
                        },
                    );
                    fn format_addr_port<'a, A: IpAddress, S: Deref<Target = A>>(
                        (addr, port): (S, NonZeroU16),
                    ) -> Cow<'a, str> {
                        Cow::Owned(format!("{}:{}", *addr, port))
                    }
                    node.record_string(
                        "LocalAddress",
                        local.map_or("[NOT BOUND]".into(), |(addr, port)| {
                            format_addr_port((&addr.map_or(I::UNSPECIFIED_ADDRESS, |a| *a), port))
                        }),
                    );
                    node.record_string(
                        "RemoteAddress",
                        remote.map_or("[NOT CONNECTED]".into(), format_addr_port),
                    )
                })
            }
        }
    }
    let sync_ctx = ctx.sync_ctx();
    let mut visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    tcp::socket::with_info::<Ipv4, _, _>(sync_ctx, &mut visitor);
    tcp::socket::with_info::<Ipv6, _, _>(sync_ctx, &mut visitor);
    let Visitor(inspector) = visitor;
    inspector
}

/// Publishes netstack3 routing table diagnostics data to Inspect.
pub(crate) fn routes(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    struct Visitor(fuchsia_inspect::Inspector);
    impl<'a> ip::forwarding::RoutesVisitor<'a, BindingsNonSyncCtxImpl> for &'_ mut Visitor {
        type VisitResult = ();
        fn visit<'b, I: Ip>(
            self,
            per_route: impl Iterator<Item = &'b ip::types::Entry<I::Addr, DeviceId<BindingsNonSyncCtxImpl>>>
                + 'b,
        ) -> Self::VisitResult
        where
            'a: 'b,
        {
            let Visitor(inspector) = self;
            for (i, route) in per_route.enumerate() {
                inspector.root().record_child(format!("{}", i), |node| {
                    let ip::types::Entry { subnet, device, gateway, metric } = route;
                    node.record_string("Destination", format!("{}", subnet));
                    node.record_uint(
                        "InterfaceId",
                        device.external_state().static_common_info().binding_id.into(),
                    );
                    match gateway {
                        Some(gateway) => {
                            node.record_string("Gateway", format!("{}", gateway));
                        }
                        None => {
                            node.record_string("Gateway", "[NONE]");
                        }
                    }
                    match metric {
                        ip::types::Metric::MetricTracksInterface(metric) => {
                            node.record_uint("Metric", (*metric).into());
                            node.record_bool("MetricTracksInterface", true);
                        }
                        ip::types::Metric::ExplicitMetric(metric) => {
                            node.record_uint("Metric", (*metric).into());
                            node.record_bool("MetricTracksInterface", false);
                        }
                    }
                })
            }
        }
    }
    let sync_ctx = ctx.sync_ctx();
    let mut visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    ip::forwarding::with_routes::<Ipv4, BindingsNonSyncCtxImpl, _>(sync_ctx, &mut visitor);
    ip::forwarding::with_routes::<Ipv6, BindingsNonSyncCtxImpl, _>(sync_ctx, &mut visitor);
    let Visitor(inspector) = visitor;
    inspector
}

pub(crate) fn devices(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    let inspector = fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default());
    ctx.non_sync_ctx().devices.with_devices(|devices| {
        for ref device in devices {
            let id = device.external_state().static_common_info().binding_id;
            inspector.root().record_child(format!("{id}"), |node| {
                node.record_string("Name", &device.external_state().static_common_info().name);
                node.record_uint("InterfaceId", id.into());
                device.external_state().with_common_info(|info| {
                    node.record_bool("AdminEnabled", info.admin_enabled);
                    node.record_uint("MTU", info.mtu.get().into());
                    let ip_addresses =
                        node.create_string_array("IpAddresses", info.addresses.len());
                    for (j, address) in info.addresses.keys().enumerate() {
                        ip_addresses.set(j, address.get().to_string());
                    }
                    node.record(ip_addresses);
                });
                match device.external_state() {
                    DeviceSpecificInfo::Netdevice(info) => {
                        node.record_bool("Loopback", false);
                        node.record_child("NetworkDevice", |node| {
                            node.record_string("MacAddress", info.mac.get().to_string());
                            info.with_dynamic_info(|dyn_info| {
                                node.record_bool("PhyUp", dyn_info.phy_up);
                            });
                        });
                    }
                    DeviceSpecificInfo::Loopback(_info) => {
                        node.record_bool("Loopback", true);
                    }
                }
            })
        }
    });
    inspector
}
