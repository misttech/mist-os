// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inspect utilities.
//!
//! This module provides utilities for publishing netstack3 diagnostics data to
//! Inspect.

use super::{BindingsNonSyncCtxImpl, Ctx, DeviceIdExt};
use net_types::ip::{Ip, IpAddress, IpVersion, Ipv4, Ipv6};
use netstack3_core::{data_structures::id_map::EntryKey, device::DeviceId, ip, transport::tcp};
use std::{borrow::Cow, num::NonZeroU16, ops::Deref};

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    /// Convert a [`tcp::socket::SocketId`] into a unique integer.
    ///
    /// Guarantees that no two unique `SocketId`s (even for different IP
    /// versions) will have have the same output value.
    fn transform_id<I: Ip>(id: tcp::socket::SocketId<I>) -> usize {
        let (index, variant) = match id {
            tcp::socket::SocketId::Unbound(id) => (id.get_key_index(), 0),
            tcp::socket::SocketId::Bound(id) => (id.get_key_index(), 1),
            tcp::socket::SocketId::Listener(id) => (id.get_key_index(), 2),
            tcp::socket::SocketId::Connection(id) => (id.get_key_index(), 3),
        };

        let unique_for_ip_version = index * 4 + variant;
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
    let Ctx { sync_ctx, non_sync_ctx: _ } = ctx;
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
    let Ctx { sync_ctx, non_sync_ctx: _ } = ctx;
    let mut visitor =
        Visitor(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()));
    ip::forwarding::with_routes::<Ipv4, BindingsNonSyncCtxImpl, _>(sync_ctx, &mut visitor);
    ip::forwarding::with_routes::<Ipv6, BindingsNonSyncCtxImpl, _>(sync_ctx, &mut visitor);
    let Visitor(inspector) = visitor;
    inspector
}
