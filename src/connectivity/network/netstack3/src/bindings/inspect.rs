// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Inspect utilities.
//!
//! This module provides utilities for publishing netstack3 diagnostics data to
//! Inspect.

use super::{
    devices::{
        DeviceIdAndName, DeviceSpecificInfo, DynamicCommonInfo, DynamicNetdeviceInfo, NetdeviceInfo,
    },
    BindingsCtx, Ctx, StackTime,
};
use fuchsia_inspect::ArrayProperty as _;
use net_types::{
    ip::{Ip, IpVersion, Ipv4, Ipv6},
    Witness as _,
};
use netstack3_core::{
    device::{self, DeviceId, WeakDeviceId},
    tcp,
};
use std::{fmt, string::ToString as _};

/// A visitor for diagnostics data that has distinct Ipv4 and Ipv6 variants.
struct DualIpVisitor {
    inspector: fuchsia_inspect::Inspector,
    count: usize,
}

impl DualIpVisitor {
    fn new() -> Self {
        Self {
            inspector: fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()),
            count: 0,
        }
    }

    /// Records a child Inspect node with an incrementing id that is unique
    /// across IP versions.
    fn record_unique_child<F>(&mut self, f: F)
    where
        F: FnOnce(&fuchsia_inspect::types::Node),
    {
        let Self { inspector, count } = self;
        let id = core::mem::replace(count, *count + 1);
        inspector.root().record_child(format!("{id}"), f)
    }
}

/// A visitor for diagnostics data.
struct Visitor(fuchsia_inspect::Inspector);

impl Visitor {
    fn new() -> Self {
        Self(fuchsia_inspect::Inspector::new(fuchsia_inspect::InspectorConfig::default()))
    }
}

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    impl<I: Ip> tcp::InfoVisitor<I, WeakDeviceId<BindingsCtx>> for DualIpVisitor {
        fn visit(&mut self, socket: tcp::SocketStats<I, WeakDeviceId<BindingsCtx>>) {
            let tcp::SocketStats { local, remote } = socket;
            self.record_unique_child(|node| {
                node.record_string("TransportProtocol", "TCP");
                node.record_string(
                    "NetworkProtocol",
                    match I::VERSION {
                        IpVersion::V4 => "IPv4",
                        IpVersion::V6 => "IPv6",
                    },
                );
                node.record_string(
                    "LocalAddress",
                    local.map_or("[NOT BOUND]".into(), |socket| {
                        format!("{}", socket.map_zone(|device| device.bindings_id().id))
                    }),
                );
                node.record_string(
                    "RemoteAddress",
                    remote.map_or("[NOT CONNECTED]".into(), |socket| {
                        format!("{}", socket.map_zone(|device| device.bindings_id().id))
                    }),
                )
            })
        }
    }
    let mut visitor = DualIpVisitor::new();
    ctx.api().tcp::<Ipv4>().with_info(&mut visitor);
    ctx.api().tcp::<Ipv6>().with_info(&mut visitor);

    visitor.inspector
}

/// Publishes netstack3 routing table diagnostics data to Inspect.
pub(crate) fn routes(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    impl<'a> netstack3_core::routes::RoutesVisitor<'a, BindingsCtx> for DualIpVisitor {
        type VisitResult = ();
        fn visit<'b, I: Ip>(
            &mut self,
            per_route: impl Iterator<Item = &'b netstack3_core::routes::Entry<I::Addr, DeviceId<BindingsCtx>>>
                + 'b,
        ) -> Self::VisitResult
        where
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
    let core_ctx = ctx.core_ctx();
    let mut visitor = DualIpVisitor::new();
    netstack3_core::routes::with_routes::<Ipv4, BindingsCtx, _>(core_ctx, &mut visitor);
    netstack3_core::routes::with_routes::<Ipv6, BindingsCtx, _>(core_ctx, &mut visitor);
    visitor.inspector
}

pub(crate) fn devices(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    impl device::DevicesVisitor<BindingsCtx> for Visitor {
        fn visit_devices(
            &self,
            devices: impl Iterator<Item = device::InspectDeviceState<BindingsCtx>>,
        ) {
            use crate::bindings::DeviceIdExt as _;
            let Self(inspector) = self;
            for device::InspectDeviceState { device_id, addresses } in devices {
                let external_state = device_id.external_state();
                let DeviceIdAndName { id: binding_id, name } = device_id.bindings_id();
                inspector.root().record_child(format!("{binding_id}"), |node| {
                    node.record_string("Name", &name);
                    node.record_uint("InterfaceId", (*binding_id).into());
                    let ip_addresses = node.create_string_array("IpAddresses", addresses.len());
                    for (j, address) in addresses.iter().enumerate() {
                        ip_addresses.set(j, address.to_string());
                    }
                    node.record(ip_addresses);
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
                            info @ NetdeviceInfo {
                                mac,
                                dynamic: _,
                                handler: _,
                                static_common_info: _,
                            },
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
                    }
                })
            }
        }
    }
    let core_ctx = ctx.core_ctx();
    let visitor = Visitor::new();
    device::inspect_devices::<BindingsCtx, _>(core_ctx, &visitor);
    let Visitor(inspector) = visitor;
    inspector
}

pub(crate) fn neighbors(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    impl device::NeighborVisitor<BindingsCtx, StackTime> for Visitor {
        fn visit_neighbors<LinkAddress: fmt::Debug>(
            &self,
            device: DeviceId<BindingsCtx>,
            neighbors: impl Iterator<
                Item = netstack3_core::neighbor::NeighborStateInspect<LinkAddress, StackTime>,
            >,
        ) {
            let Self(inspector) = self;
            let name = &device.bindings_id().name;
            inspector.root().record_child(name, |node| {
                for (i, neighbor) in neighbors.enumerate() {
                    let netstack3_core::neighbor::NeighborStateInspect {
                        state,
                        ip_address,
                        link_address,
                        last_confirmed_at,
                    } = neighbor;
                    node.record_child(format!("{i}"), |node| {
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
            });
        }
    }
    let core_ctx = ctx.core_ctx();
    let visitor = Visitor::new();
    device::inspect_neighbors::<BindingsCtx, _>(core_ctx, &visitor);
    let Visitor(inspector) = visitor;
    inspector
}

pub(crate) fn counters(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    impl netstack3_core::inspect::CounterVisitor for Visitor {
        fn visit_counters(&self, counters: netstack3_core::inspect::StackCounters<'_>) {
            let Self(inspector) = self;
            inspector.root().record_child("Devices", |node| {
                node.record_child("Ethernet", |node| {
                    node.record_child("Rx", |node| {
                        node.record_uint(
                            "TotalFrames",
                            counters.devices.ethernet.common.recv_frame.get(),
                        );
                        node.record_uint(
                            "Malformed",
                            counters.devices.ethernet.common.recv_parse_error.get(),
                        );
                        node.record_uint(
                            "NonLocalDstAddr",
                            counters.devices.ethernet.recv_other_dest.get(),
                        );
                        node.record_uint(
                            "UnsupportedEthertype",
                            counters.devices.ethernet.common.recv_unsupported_ethertype.get(),
                        );
                        node.record_uint(
                            "ArpDelivered",
                            counters.devices.ethernet.recv_arp_delivered.get(),
                        );
                        node.record_uint(
                            "IpDelivered",
                            counters.devices.ethernet.common.recv_ip_delivered.get(),
                        );
                    });
                    node.record_child("Tx", |node| {
                        node.record_uint(
                            "TotalFrames",
                            counters.devices.ethernet.common.send_total_frames.get(),
                        );
                        node.record_uint("Sent", counters.devices.ethernet.common.send_frame.get());
                        node.record_uint("NoQueue", counters.devices.ethernet.send_no_queue.get());
                        node.record_uint(
                            "QueueFull",
                            counters.devices.ethernet.common.send_queue_full.get(),
                        );
                        node.record_uint(
                            "SerializeError",
                            counters.devices.ethernet.common.send_serialize_error.get(),
                        );
                    });
                });
                node.record_child("Loopback", |node| {
                    node.record_child("Rx", |node| {
                        node.record_uint(
                            "TotalFrames",
                            counters.devices.loopback.common.recv_frame.get(),
                        );
                        node.record_uint(
                            "Malformed",
                            counters.devices.loopback.common.recv_parse_error.get(),
                        );
                        node.record_uint(
                            "NoEthertype",
                            counters.devices.loopback.recv_no_ethertype.get(),
                        );
                        node.record_uint(
                            "UnsupportedEthertype",
                            counters.devices.loopback.common.recv_unsupported_ethertype.get(),
                        );
                        node.record_uint(
                            "IpDelivered",
                            counters.devices.loopback.common.recv_ip_delivered.get(),
                        );
                    });
                    node.record_child("Tx", |node| {
                        node.record_uint(
                            "TotalFrames",
                            counters.devices.loopback.common.send_total_frames.get(),
                        );
                        node.record_uint("Sent", counters.devices.loopback.common.send_frame.get());
                        node.record_uint(
                            "QueueFull",
                            counters.devices.loopback.common.send_queue_full.get(),
                        );
                        node.record_uint(
                            "SerializeError",
                            counters.devices.loopback.common.send_serialize_error.get(),
                        );
                    });
                });
            });
            inspector.root().record_child("Arp", |node| {
                node.record_child("Rx", |node| {
                    node.record_uint("TotalPackets", counters.arp.rx_packets.get());
                    node.record_uint("Requests", counters.arp.rx_requests.get());
                    node.record_uint("Responses", counters.arp.rx_responses.get());
                    node.record_uint("Malformed", counters.arp.rx_malformed_packets.get());
                    node.record_uint(
                        "NonLocalDstAddr",
                        counters.arp.rx_dropped_non_local_target.get(),
                    );
                });
                node.record_child("Tx", |node| {
                    node.record_uint("Requests", counters.arp.tx_requests.get());
                    node.record_uint(
                        "RequestsNonLocalSrcAddr",
                        counters.arp.tx_requests_dropped_no_local_addr.get(),
                    );
                    node.record_uint("Responses", counters.arp.tx_responses.get());
                });
            });
            inspector.root().record_child("ICMP", |node| {
                node.record_child("V4", |node| {
                    node.record_child("Rx", |node| {
                        node.record_uint("EchoRequest", counters.icmpv4_rx.echo_request.get());
                        node.record_uint("EchoReply", counters.icmpv4_rx.echo_reply.get());
                        node.record_uint(
                            "TimestampRequest",
                            counters.icmpv4_rx.timestamp_request.get(),
                        );
                        node.record_uint(
                            "DestUnreachable",
                            counters.icmpv4_rx.dest_unreachable.get(),
                        );
                        node.record_uint("TimeExceeded", counters.icmpv4_rx.time_exceeded.get());
                        node.record_uint(
                            "ParameterProblem",
                            counters.icmpv4_rx.parameter_problem.get(),
                        );
                        node.record_uint("PacketTooBig", counters.icmpv4_rx.packet_too_big.get());
                        node.record_uint("Error", counters.icmpv4_rx.error.get());
                    });
                    node.record_child("Tx", |node| {
                        node.record_uint("Reply", counters.icmpv4_tx.reply.get());
                        node.record_uint(
                            "ProtocolUnreachable",
                            counters.icmpv4_tx.protocol_unreachable.get(),
                        );
                        node.record_uint(
                            "PortUnreachable",
                            counters.icmpv4_tx.port_unreachable.get(),
                        );
                        node.record_uint(
                            "NetUnreachable",
                            counters.icmpv4_tx.net_unreachable.get(),
                        );
                        node.record_uint("TtlExpired", counters.icmpv4_tx.ttl_expired.get());
                        node.record_uint("PacketTooBig", counters.icmpv4_tx.packet_too_big.get());
                        node.record_uint(
                            "ParameterProblem",
                            counters.icmpv4_tx.parameter_problem.get(),
                        );
                        node.record_uint(
                            "DestUnreachable",
                            counters.icmpv4_tx.dest_unreachable.get(),
                        );
                        node.record_uint("Error", counters.icmpv4_tx.error.get());
                    });
                });
                node.record_child("V6", |node| {
                    node.record_child("Rx", |node| {
                        node.record_uint("EchoRequest", counters.icmpv6_rx.echo_request.get());
                        node.record_uint("EchoReply", counters.icmpv6_rx.echo_reply.get());
                        node.record_uint(
                            "TimestampRequest",
                            counters.icmpv6_rx.timestamp_request.get(),
                        );
                        node.record_uint(
                            "DestUnreachable",
                            counters.icmpv6_rx.dest_unreachable.get(),
                        );
                        node.record_uint("TimeExceeded", counters.icmpv6_rx.time_exceeded.get());
                        node.record_uint(
                            "ParameterProblem",
                            counters.icmpv6_rx.parameter_problem.get(),
                        );
                        node.record_uint("PacketTooBig", counters.icmpv6_rx.packet_too_big.get());
                        node.record_uint("Error", counters.icmpv6_rx.error.get());
                        node.record_child("NDP", |node| {
                            node.record_uint(
                                "NeighborSolicitation",
                                counters.ndp.rx_neighbor_solicitation.get(),
                            );
                            node.record_uint(
                                "NeighborAdvertisement",
                                counters.ndp.rx_neighbor_advertisement.get(),
                            );
                            node.record_uint(
                                "RouterSolicitation",
                                counters.ndp.rx_router_solicitation.get(),
                            );
                            node.record_uint(
                                "RouterAdvertisement",
                                counters.ndp.rx_router_advertisement.get(),
                            );
                        });
                    });
                    node.record_child("Tx", |node| {
                        node.record_uint("Reply", counters.icmpv6_tx.reply.get());
                        node.record_uint(
                            "ProtocolUnreachable",
                            counters.icmpv6_tx.protocol_unreachable.get(),
                        );
                        node.record_uint(
                            "PortUnreachable",
                            counters.icmpv6_tx.port_unreachable.get(),
                        );
                        node.record_uint(
                            "NetUnreachable",
                            counters.icmpv6_tx.net_unreachable.get(),
                        );
                        node.record_uint("TtlExpired", counters.icmpv6_tx.ttl_expired.get());
                        node.record_uint("PacketTooBig", counters.icmpv6_tx.packet_too_big.get());
                        node.record_uint(
                            "ParameterProblem",
                            counters.icmpv6_tx.parameter_problem.get(),
                        );
                        node.record_uint(
                            "DestUnreachable",
                            counters.icmpv6_tx.dest_unreachable.get(),
                        );
                        node.record_uint("Error", counters.icmpv6_tx.error.get());
                        node.record_child("NDP", |node| {
                            node.record_uint(
                                "NeighborAdvertisement",
                                counters.ndp.tx_neighbor_advertisement.get(),
                            );
                            node.record_uint(
                                "NeighborSolicitation",
                                counters.ndp.tx_neighbor_solicitation.get(),
                            );
                        });
                    });
                });
            });
            inspector.root().record_child("IPv4", |node| {
                node.record_uint("PacketTx", counters.ipv4_common.send_ip_packet.get());
                node.record_child("PacketRx", |node| {
                    node.record_uint("Received", counters.ipv4_common.receive_ip_packet.get());
                    node.record_uint(
                        "Dispatched",
                        counters.ipv4_common.dispatch_receive_ip_packet.get(),
                    );
                    node.record_uint("Delivered", counters.ipv4.deliver.get());
                    node.record_uint(
                        "OtherHost",
                        counters.ipv4_common.dispatch_receive_ip_packet_other_host.get(),
                    );
                    node.record_uint(
                        "ParameterProblem",
                        counters.ipv4_common.parameter_problem.get(),
                    );
                    node.record_uint(
                        "UnspecifiedDst",
                        counters.ipv4_common.unspecified_destination.get(),
                    );
                    node.record_uint(
                        "UnspecifiedSrc",
                        counters.ipv4_common.unspecified_source.get(),
                    );
                    node.record_uint("Dropped", counters.ipv4_common.dropped.get());
                });
                node.record_child("Forwarding", |node| {
                    node.record_uint("Forwarded", counters.ipv4_common.forward.get());
                    node.record_uint("NoRouteToHost", counters.ipv4_common.no_route_to_host.get());
                    node.record_uint("MtuExceeded", counters.ipv4_common.mtu_exceeded.get());
                    node.record_uint("TtlExpired", counters.ipv4_common.ttl_expired.get());
                });
                node.record_uint("RxIcmpError", counters.ipv4_common.receive_icmp_error.get());
                node.record_child("Fragments", |node| {
                    node.record_uint(
                        "ReassemblyError",
                        counters.ipv4_common.fragment_reassembly_error.get(),
                    );
                    node.record_uint(
                        "NeedMoreFragments",
                        counters.ipv4_common.need_more_fragments.get(),
                    );
                    node.record_uint(
                        "InvalidFragment",
                        counters.ipv4_common.invalid_fragment.get(),
                    );
                    node.record_uint("CacheFull", counters.ipv4_common.fragment_cache_full.get());
                });
            });
            inspector.root().record_child("IPv6", |node| {
                node.record_uint("PacketTx", counters.ipv6_common.send_ip_packet.get());
                node.record_child("PacketRx", |node| {
                    node.record_uint("Received", counters.ipv6_common.receive_ip_packet.get());
                    node.record_uint(
                        "Dispatched",
                        counters.ipv6_common.dispatch_receive_ip_packet.get(),
                    );
                    node.record_uint("DeliveredMulticast", counters.ipv6.deliver_multicast.get());
                    node.record_uint("DeliveredUnicast", counters.ipv6.deliver_unicast.get());
                    node.record_uint(
                        "OtherHost",
                        counters.ipv6_common.dispatch_receive_ip_packet_other_host.get(),
                    );
                    node.record_uint(
                        "ParameterProblem",
                        counters.ipv6_common.parameter_problem.get(),
                    );
                    node.record_uint(
                        "UnspecifiedDst",
                        counters.ipv6_common.unspecified_destination.get(),
                    );
                    node.record_uint(
                        "UnspecifiedSrc",
                        counters.ipv6_common.unspecified_source.get(),
                    );
                    node.record_uint("Dropped", counters.ipv6_common.dropped.get());
                    node.record_uint("DroppedTentativeDst", counters.ipv6.drop_for_tentative.get());
                    node.record_uint(
                        "DroppedNonUnicastSrc",
                        counters.ipv6.non_unicast_source.get(),
                    );
                    node.record_uint(
                        "DroppedExtensionHeader",
                        counters.ipv6.extension_header_discard.get(),
                    );
                });
                node.record_child("Forwarding", |node| {
                    node.record_uint("Forwarded", counters.ipv6_common.forward.get());
                    node.record_uint("NoRouteToHost", counters.ipv6_common.no_route_to_host.get());
                    node.record_uint("MtuExceeded", counters.ipv6_common.mtu_exceeded.get());
                    node.record_uint("TtlExpired", counters.ipv6_common.ttl_expired.get());
                });
                node.record_uint("RxIcmpError", counters.ipv6_common.receive_icmp_error.get());
                node.record_child("Fragments", |node| {
                    node.record_uint(
                        "ReassemblyError",
                        counters.ipv6_common.fragment_reassembly_error.get(),
                    );
                    node.record_uint(
                        "NeedMoreFragments",
                        counters.ipv6_common.need_more_fragments.get(),
                    );
                    node.record_uint(
                        "InvalidFragment",
                        counters.ipv6_common.invalid_fragment.get(),
                    );
                    node.record_uint("CacheFull", counters.ipv6_common.fragment_cache_full.get());
                });
            });
            inspector.root().record_child("UDP", |node| {
                node.record_child("V4", |node| {
                    node.record_child("Rx", |node| {
                        node.record_uint("Received", counters.udpv4.rx.get());
                        node.record_child("Errors", |node| {
                            node.record_uint("MappedAddr", counters.udpv4.rx_mapped_addr.get());
                            node.record_uint(
                                "UnknownDstPort",
                                counters.udpv4.rx_unknown_dest_port.get(),
                            );
                            node.record_uint("Malformed", counters.udpv4.rx_malformed.get());
                        });
                    });
                    node.record_child("Tx", |node| {
                        node.record_uint("Sent", counters.udpv4.tx.get());
                        node.record_uint("Errors", counters.udpv4.tx_error.get());
                    });
                    node.record_uint("IcmpErrors", counters.udpv4.rx_icmp_error.get());
                });
                node.record_child("V6", |node| {
                    node.record_child("Rx", |node| {
                        node.record_uint("Received", counters.udpv6.rx.get());
                        node.record_child("Errors", |node| {
                            node.record_uint("MappedAddr", counters.udpv4.rx_mapped_addr.get());
                            node.record_uint(
                                "UnknownDstPort",
                                counters.udpv6.rx_unknown_dest_port.get(),
                            );
                            node.record_uint("Malformed", counters.udpv6.rx_malformed.get());
                        });
                    });
                    node.record_child("Tx", |node| {
                        node.record_uint("Sent", counters.udpv6.tx.get());
                        node.record_uint("Errors", counters.udpv6.tx_error.get());
                    });
                    node.record_uint("IcmpErrors", counters.udpv6.rx_icmp_error.get());
                });
            });
        }
    }
    let core_ctx = ctx.core_ctx();
    let visitor = Visitor::new();
    netstack3_core::inspect::inspect_counters::<_, _>(core_ctx, &visitor);
    let Visitor(inspector) = visitor;
    inspector
}
