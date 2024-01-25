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
    ip::{Ip, IpAddress, IpVersion, Ipv4, Ipv6},
    Witness as _,
};
use netstack3_core::{
    device::{ArpCounters, DeviceCounters, DeviceId, EthernetLinkDevice, WeakDeviceId},
    inspect::StackCounters,
    ip::{
        CommonIpCounters, IcmpRxCounters, IcmpTxCounters, Ipv4Counters, Ipv6Counters, NdpCounters,
    },
    neighbor, tcp,
    udp::UdpCounters,
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

/// A visitor for diagnostics data.
struct Visitor<'a>(&'a Node);

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

/// Publishes netstack3 socket diagnostics data to Inspect.
pub(crate) fn sockets(ctx: &mut Ctx) -> fuchsia_inspect::Inspector {
    impl<'a, I: Ip> tcp::InfoVisitor<I, WeakDeviceId<BindingsCtx>> for DualIpVisitor<'a> {
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
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let mut visitor = DualIpVisitor::new(inspector.root());
    ctx.api().tcp::<Ipv4>().with_info(&mut visitor);
    ctx.api().tcp::<Ipv6>().with_info(&mut visitor);
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

pub(crate) fn counters(ctx: &Ctx) -> fuchsia_inspect::Inspector {
    impl<'a> netstack3_core::inspect::CounterVisitor for Visitor<'a> {
        fn visit_counters(
            &self,
            StackCounters {
                ipv4_common,
                ipv6_common,
                ipv4,
                ipv6,
                arp,
                udpv4,
                udpv6,
                icmpv4_rx,
                icmpv4_tx,
                icmpv6_rx,
                icmpv6_tx,
                ndp,
                devices,
            }: StackCounters<'_>,
        ) {
            let Self(node) = self;
            node.record_child("Device", |node| {
                let DeviceCounters {
                    recv_arp_delivered,
                    recv_frame,
                    recv_ip_delivered,
                    recv_no_ethertype,
                    recv_ethernet_other_dest,
                    recv_parse_error,
                    recv_unsupported_ethertype,
                    send_frame,
                    send_ipv4_frame,
                    send_ipv6_frame,
                    send_dropped_no_queue,
                    send_queue_full,
                    send_serialize_error,
                    send_total_frames,
                } = devices;
                node.record_child("Rx", |node| {
                    node.record_uint("TotalFrames", recv_frame.get());
                    node.record_uint("Malformed", recv_parse_error.get());
                    node.record_uint("NonLocalDstAddr", recv_ethernet_other_dest.get());
                    node.record_uint("NoEthertype", recv_no_ethertype.get());
                    node.record_uint("UnsupportedEthertype", recv_unsupported_ethertype.get());
                    node.record_uint("ArpDelivered", recv_arp_delivered.get());
                    node.record_uint("IpDelivered", recv_ip_delivered.get());
                });
                node.record_child("Tx", |node| {
                    node.record_uint("TotalFrames", send_total_frames.get());
                    node.record_uint("Sent", send_frame.get());
                    node.record_uint("SendIpv4Frame", send_ipv4_frame.get());
                    node.record_uint("SendIpv6Frame", send_ipv6_frame.get());
                    node.record_uint("NoQueue", send_dropped_no_queue.get());
                    node.record_uint("QueueFull", send_queue_full.get());
                    node.record_uint("SerializeError", send_serialize_error.get());
                });
            });
            node.record_child("Arp", |node| {
                let ArpCounters {
                    rx_dropped_non_local_target,
                    rx_malformed_packets,
                    rx_packets,
                    rx_requests,
                    rx_responses,
                    tx_requests,
                    tx_requests_dropped_no_local_addr,
                    tx_responses,
                } = arp;
                node.record_child("Rx", |node| {
                    node.record_uint("TotalPackets", rx_packets.get());
                    node.record_uint("Requests", rx_requests.get());
                    node.record_uint("Responses", rx_responses.get());
                    node.record_uint("Malformed", rx_malformed_packets.get());
                    node.record_uint("NonLocalDstAddr", rx_dropped_non_local_target.get());
                });
                node.record_child("Tx", |node| {
                    node.record_uint("Requests", tx_requests.get());
                    node.record_uint(
                        "RequestsNonLocalSrcAddr",
                        tx_requests_dropped_no_local_addr.get(),
                    );
                    node.record_uint("Responses", tx_responses.get());
                });
            });
            node.record_child("ICMP", |node| {
                node.record_child("V4", |node| {
                    node.record_child("Rx", |node| {
                        record_icmp_rx(node, icmpv4_rx.get());
                    });
                    node.record_child("Tx", |node| {
                        record_icmp_tx(node, icmpv4_tx.get());
                    });
                });
                node.record_child("V6", |node| {
                    let NdpCounters {
                        rx_neighbor_solicitation,
                        rx_neighbor_advertisement,
                        rx_router_advertisement,
                        rx_router_solicitation,
                        tx_neighbor_advertisement,
                        tx_neighbor_solicitation,
                    } = ndp;
                    node.record_child("Rx", |node| {
                        record_icmp_rx(node, icmpv6_rx.get());
                        node.record_child("NDP", |node| {
                            node.record_uint(
                                "NeighborSolicitation",
                                rx_neighbor_solicitation.get(),
                            );
                            node.record_uint(
                                "NeighborAdvertisement",
                                rx_neighbor_advertisement.get(),
                            );
                            node.record_uint("RouterSolicitation", rx_router_solicitation.get());
                            node.record_uint("RouterAdvertisement", rx_router_advertisement.get());
                        });
                    });
                    node.record_child("Tx", |node| {
                        record_icmp_tx(node, icmpv6_tx.get());
                        node.record_child("NDP", |node| {
                            node.record_uint(
                                "NeighborAdvertisement",
                                tx_neighbor_advertisement.get(),
                            );
                            node.record_uint(
                                "NeighborSolicitation",
                                tx_neighbor_solicitation.get(),
                            );
                        });
                    });
                });
            });
            node.record_child("IPv4", |node| {
                record_ip(node, ipv4_common.get(), |node| {
                    let Ipv4Counters { deliver } = ipv4;
                    node.record_uint("Delivered", deliver.get());
                });
            });
            node.record_child("IPv6", |node| {
                record_ip(node, ipv6_common.get(), |node| {
                    let Ipv6Counters {
                        deliver_multicast,
                        deliver_unicast,
                        drop_for_tentative,
                        non_unicast_source,
                        extension_header_discard,
                    } = ipv6;
                    node.record_uint("DeliveredMulticast", deliver_multicast.get());
                    node.record_uint("DeliveredUnicast", deliver_unicast.get());
                    node.record_uint("DroppedTentativeDst", drop_for_tentative.get());
                    node.record_uint("DroppedNonUnicastSrc", non_unicast_source.get());
                    node.record_uint("DroppedExtensionHeader", extension_header_discard.get());
                });
            });
            node.record_child("UDP", |node| {
                node.record_child("V4", |node| {
                    record_udp(node, udpv4.get());
                });
                node.record_child("V6", |node| {
                    record_udp(node, udpv6.get());
                });
            });
        }
    }
    let inspector = fuchsia_inspect::Inspector::new(Default::default());
    let core_ctx = ctx.core_ctx();
    netstack3_core::inspect::inspect_counters::<_, _>(core_ctx, &Visitor(inspector.root()));
    inspector
}

fn record_icmp_rx(
    node: &Node,
    IcmpRxCounters {
        error,
        error_delivered_to_transport_layer,
        error_delivered_to_socket,
        echo_request,
        echo_reply,
        timestamp_request,
        dest_unreachable,
        time_exceeded,
        parameter_problem,
        packet_too_big,
    }: &IcmpRxCounters,
) {
    node.record_uint("EchoRequest", echo_request.get());
    node.record_uint("EchoReply", echo_reply.get());
    node.record_uint("TimestampRequest", timestamp_request.get());
    node.record_uint("DestUnreachable", dest_unreachable.get());
    node.record_uint("TimeExceeded", time_exceeded.get());
    node.record_uint("ParameterProblem", parameter_problem.get());
    node.record_uint("PacketTooBig", packet_too_big.get());
    node.record_uint("Error", error.get());
    node.record_uint("ErrorDeliveredToTransportLayer", error_delivered_to_transport_layer.get());
    node.record_uint("ErrorDeliveredToSocket", error_delivered_to_socket.get());
}

fn record_icmp_tx(
    node: &Node,
    IcmpTxCounters {
        reply,
        protocol_unreachable,
        port_unreachable,
        net_unreachable,
        ttl_expired,
        packet_too_big,
        parameter_problem,
        dest_unreachable,
        error,
    }: &IcmpTxCounters,
) {
    node.record_uint("Reply", reply.get());
    node.record_uint("ProtocolUnreachable", protocol_unreachable.get());
    node.record_uint("PortUnreachable", port_unreachable.get());
    node.record_uint("NetUnreachable", net_unreachable.get());
    node.record_uint("TtlExpired", ttl_expired.get());
    node.record_uint("PacketTooBig", packet_too_big.get());
    node.record_uint("ParameterProblem", parameter_problem.get());
    node.record_uint("DestUnreachable", dest_unreachable.get());
    node.record_uint("Error", error.get());
}

fn record_ip<F: FnOnce(&Node)>(
    node: &Node,
    CommonIpCounters {
        dispatch_receive_ip_packet,
        dispatch_receive_ip_packet_other_host,
        receive_ip_packet,
        send_ip_packet,
        forwarding_disabled,
        forward,
        no_route_to_host,
        mtu_exceeded,
        ttl_expired,
        receive_icmp_error,
        fragment_reassembly_error,
        need_more_fragments,
        invalid_fragment,
        fragment_cache_full,
        parameter_problem,
        unspecified_destination,
        unspecified_source,
        dropped,
    }: &CommonIpCounters,
    record_version_specific_rx: F,
) {
    node.record_uint("PacketTx", send_ip_packet.get());
    node.record_child("PacketRx", |node| {
        node.record_uint("Received", receive_ip_packet.get());
        node.record_uint("Dispatched", dispatch_receive_ip_packet.get());
        node.record_uint("OtherHost", dispatch_receive_ip_packet_other_host.get());
        node.record_uint("ParameterProblem", parameter_problem.get());
        node.record_uint("UnspecifiedDst", unspecified_destination.get());
        node.record_uint("UnspecifiedSrc", unspecified_source.get());
        node.record_uint("Dropped", dropped.get());
        record_version_specific_rx(node);
    });
    node.record_child("Forwarding", |node| {
        node.record_uint("Forwarded", forward.get());
        node.record_uint("ForwardingDisabled", forwarding_disabled.get());
        node.record_uint("NoRouteToHost", no_route_to_host.get());
        node.record_uint("MtuExceeded", mtu_exceeded.get());
        node.record_uint("TtlExpired", ttl_expired.get());
    });
    node.record_uint("RxIcmpError", receive_icmp_error.get());
    node.record_child("Fragments", |node| {
        node.record_uint("ReassemblyError", fragment_reassembly_error.get());
        node.record_uint("NeedMoreFragments", need_more_fragments.get());
        node.record_uint("InvalidFragment", invalid_fragment.get());
        node.record_uint("CacheFull", fragment_cache_full.get());
    });
}

fn record_udp(node: &Node, counters: &UdpCounters) {
    let UdpCounters {
        rx_icmp_error,
        rx,
        rx_mapped_addr,
        rx_unknown_dest_port,
        rx_malformed,
        tx,
        tx_error,
    } = counters;
    node.record_child("Rx", |node| {
        node.record_uint("Received", rx.get());
        node.record_child("Errors", |node| {
            node.record_uint("MappedAddr", rx_mapped_addr.get());
            node.record_uint("UnknownDstPort", rx_unknown_dest_port.get());
            node.record_uint("Malformed", rx_malformed.get());
        });
    });
    node.record_child("Tx", |node| {
        node.record_uint("Sent", tx.get());
        node.record_uint("Errors", tx_error.get());
    });
    node.record_uint("IcmpErrors", rx_icmp_error.get());
}
