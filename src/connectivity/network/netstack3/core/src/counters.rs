// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for working with and exposing packet statistic counters.

use core::sync::atomic::{AtomicU64, Ordering};
use net_types::ip::{Ip, Ipv4, Ipv6};

use crate::{
    base::ContextPair,
    context::CounterContext,
    device::{arp::ArpCounters, DeviceCounters},
    inspect::Inspector,
    ip::{
        icmp::{
            IcmpRxCounters, IcmpRxCountersInner, IcmpTxCounters, IcmpTxCountersInner, NdpCounters,
            NdpRxCounters, NdpTxCounters,
        },
        IpCounters, IpLayerIpExt,
    },
    transport::udp::{UdpCounters, UdpCountersInner},
};

/// An atomic counter for packet statistics, e.g. IPv4 packets received.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub(crate) fn increment(&self) {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        let _: u64 = v.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically retrieves the counter value as a `u64`.
    pub fn get(&self) -> u64 {
        // Use relaxed ordering since we do not use packet counter values to
        // synchronize other accesses.  See:
        // https://doc.rust-lang.org/nomicon/atomics.html#relaxed
        let Self(v) = self;
        v.load(Ordering::Relaxed)
    }
}

/// An API struct for accessing all stack counters.
pub struct CountersApi<C>(C);

impl<C> CountersApi<C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

impl<C> CountersApi<C>
where
    C: ContextPair,
    C::CoreContext: CounterContext<IpCounters<Ipv4>>
        + CounterContext<IpCounters<Ipv6>>
        + CounterContext<UdpCounters<Ipv4>>
        + CounterContext<UdpCounters<Ipv6>>
        + CounterContext<IcmpRxCounters<Ipv4>>
        + CounterContext<IcmpRxCounters<Ipv6>>
        + CounterContext<IcmpTxCounters<Ipv4>>
        + CounterContext<IcmpTxCounters<Ipv4>>
        + CounterContext<IcmpTxCounters<Ipv6>>
        + CounterContext<NdpCounters>
        + CounterContext<ArpCounters>
        + CounterContext<DeviceCounters>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair) = self;
        pair.core_ctx()
    }

    /// Exposes all of the stack wide counters through `inspector`.
    pub fn inspect_stack_counters<I: Inspector>(&mut self, inspector: &mut I) {
        inspector.record_child("Device", |inspector| {
            self.core_ctx().with_counters(|device| inspect_device_counters(inspector, device));
        });
        inspector.record_child("Arp", |inspector| {
            self.core_ctx().with_counters(|arp| inspect_arp_counters(inspector, arp));
        });
        inspector.record_child("ICMP", |inspector| {
            inspector.record_child("V4", |inspector| {
                inspector.record_child("Rx", |inspector| {
                    self.core_ctx()
                        .with_counters(|icmp| inspect_icmp_rx_counters::<Ipv4>(inspector, icmp));
                });
                inspector.record_child("Tx", |inspector| {
                    self.core_ctx()
                        .with_counters(|icmp| inspect_icmp_tx_counters::<Ipv4>(inspector, icmp));
                });
            });
            inspector.record_child("V6", |inspector| {
                inspector.record_child("Rx", |inspector| {
                    self.core_ctx()
                        .with_counters(|icmp| inspect_icmp_rx_counters::<Ipv6>(inspector, icmp));
                    inspector.record_child("NDP", |inspector| {
                        self.core_ctx().with_counters(|NdpCounters { rx, tx: _ }| {
                            inspect_ndp_rx_counters(inspector, rx)
                        });
                    })
                });
                inspector.record_child("Tx", |inspector| {
                    self.core_ctx()
                        .with_counters(|icmp| inspect_icmp_tx_counters::<Ipv6>(inspector, icmp));
                    inspector.record_child("NDP", |inspector| {
                        self.core_ctx().with_counters(|NdpCounters { rx: _, tx }| {
                            inspect_ndp_tx_counters(inspector, tx)
                        });
                    })
                });
            });
        });
        inspector.record_child("IPv4", |inspector| {
            self.core_ctx().with_counters(|ip| inspect_ip_counters::<Ipv4>(inspector, ip))
        });
        inspector.record_child("IPv6", |inspector| {
            self.core_ctx().with_counters(|ip| inspect_ip_counters::<Ipv6>(inspector, ip))
        });
        inspector.record_child("UDP", |inspector| {
            inspector.record_child("V4", |inspector| {
                self.core_ctx().with_counters(|udp| inspect_udp_counters::<Ipv4>(inspector, udp))
            });
            inspector.record_child("V6", |inspector| {
                self.core_ctx().with_counters(|udp| inspect_udp_counters::<Ipv6>(inspector, udp))
            });
        });
    }
}

fn inspect_device_counters(inspector: &mut impl Inspector, counters: &DeviceCounters) {
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
    } = counters;
    inspector.record_child("Rx", |inspector| {
        inspector.record_counter("TotalFrames", recv_frame);
        inspector.record_counter("Malformed", recv_parse_error);
        inspector.record_counter("NonLocalDstAddr", recv_ethernet_other_dest);
        inspector.record_counter("NoEthertype", recv_no_ethertype);
        inspector.record_counter("UnsupportedEthertype", recv_unsupported_ethertype);
        inspector.record_counter("ArpDelivered", recv_arp_delivered);
        inspector.record_counter("IpDelivered", recv_ip_delivered);
    });
    inspector.record_child("Tx", |inspector| {
        inspector.record_counter("TotalFrames", send_total_frames);
        inspector.record_counter("Sent", send_frame);
        inspector.record_counter("SendIpv4Frame", send_ipv4_frame);
        inspector.record_counter("SendIpv6Frame", send_ipv6_frame);
        inspector.record_counter("NoQueue", send_dropped_no_queue);
        inspector.record_counter("QueueFull", send_queue_full);
        inspector.record_counter("SerializeError", send_serialize_error);
    });
}

fn inspect_arp_counters(inspector: &mut impl Inspector, counters: &ArpCounters) {
    let ArpCounters {
        rx_dropped_non_local_target,
        rx_malformed_packets,
        rx_packets,
        rx_requests,
        rx_responses,
        tx_requests,
        tx_requests_dropped_no_local_addr,
        tx_responses,
    } = counters;
    inspector.record_child("Rx", |inspector| {
        inspector.record_counter("TotalPackets", rx_packets);
        inspector.record_counter("Requests", rx_requests);
        inspector.record_counter("Responses", rx_responses);
        inspector.record_counter("Malformed", rx_malformed_packets);
        inspector.record_counter("NonLocalDstAddr", rx_dropped_non_local_target);
    });
    inspector.record_child("Tx", |inspector| {
        inspector.record_counter("Requests", tx_requests);
        inspector.record_counter("RequestsNonLocalSrcAddr", tx_requests_dropped_no_local_addr);
        inspector.record_counter("Responses", tx_responses);
    });
}

fn inspect_icmp_rx_counters<I: Ip>(inspector: &mut impl Inspector, counters: &IcmpRxCounters<I>) {
    let IcmpRxCountersInner {
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
    } = counters.as_ref();
    inspector.record_counter("EchoRequest", echo_request);
    inspector.record_counter("EchoReply", echo_reply);
    inspector.record_counter("TimestampRequest", timestamp_request);
    inspector.record_counter("DestUnreachable", dest_unreachable);
    inspector.record_counter("TimeExceeded", time_exceeded);
    inspector.record_counter("ParameterProblem", parameter_problem);
    inspector.record_counter("PacketTooBig", packet_too_big);
    inspector.record_counter("Error", error);
    inspector.record_counter("ErrorDeliveredToTransportLayer", error_delivered_to_transport_layer);
    inspector.record_counter("ErrorDeliveredToSocket", error_delivered_to_socket);
}

fn inspect_icmp_tx_counters<I: Ip>(inspector: &mut impl Inspector, counters: &IcmpTxCounters<I>) {
    let IcmpTxCountersInner {
        reply,
        protocol_unreachable,
        port_unreachable,
        net_unreachable,
        ttl_expired,
        packet_too_big,
        parameter_problem,
        dest_unreachable,
        error,
    } = counters.as_ref();
    inspector.record_counter("Reply", reply);
    inspector.record_counter("ProtocolUnreachable", protocol_unreachable);
    inspector.record_counter("PortUnreachable", port_unreachable);
    inspector.record_counter("NetUnreachable", net_unreachable);
    inspector.record_counter("TtlExpired", ttl_expired);
    inspector.record_counter("PacketTooBig", packet_too_big);
    inspector.record_counter("ParameterProblem", parameter_problem);
    inspector.record_counter("DestUnreachable", dest_unreachable);
    inspector.record_counter("Error", error);
}

fn inspect_ndp_tx_counters(inspector: &mut impl Inspector, counters: &NdpTxCounters) {
    let NdpTxCounters { neighbor_advertisement, neighbor_solicitation } = counters;
    inspector.record_counter("NeighborAdvertisement", neighbor_advertisement);
    inspector.record_counter("NeighborSolicitation", neighbor_solicitation);
}

fn inspect_ndp_rx_counters(inspector: &mut impl Inspector, counters: &NdpRxCounters) {
    let NdpRxCounters {
        neighbor_solicitation,
        neighbor_advertisement,
        router_advertisement,
        router_solicitation,
    } = counters;
    inspector.record_counter("NeighborSolicitation", neighbor_solicitation);
    inspector.record_counter("NeighborAdvertisement", neighbor_advertisement);
    inspector.record_counter("RouterSolicitation", router_solicitation);
    inspector.record_counter("RouterAdvertisement", router_advertisement);
}

fn inspect_ip_counters<I: IpLayerIpExt>(inspector: &mut impl Inspector, counters: &IpCounters<I>) {
    let IpCounters {
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
        version_rx,
    } = counters;
    inspector.record_counter("PacketTx", send_ip_packet);
    inspector.record_child("PacketRx", |inspector| {
        inspector.record_counter("Received", receive_ip_packet);
        inspector.record_counter("Dispatched", dispatch_receive_ip_packet);
        inspector.record_counter("OtherHost", dispatch_receive_ip_packet_other_host);
        inspector.record_counter("ParameterProblem", parameter_problem);
        inspector.record_counter("UnspecifiedDst", unspecified_destination);
        inspector.record_counter("UnspecifiedSrc", unspecified_source);
        inspector.record_counter("Dropped", dropped);
        inspector.delegate_inspectable(version_rx);
    });
    inspector.record_child("Forwarding", |inspector| {
        inspector.record_counter("Forwarded", forward);
        inspector.record_counter("ForwardingDisabled", forwarding_disabled);
        inspector.record_counter("NoRouteToHost", no_route_to_host);
        inspector.record_counter("MtuExceeded", mtu_exceeded);
        inspector.record_counter("TtlExpired", ttl_expired);
    });
    inspector.record_counter("RxIcmpError", receive_icmp_error);
    inspector.record_child("Fragments", |inspector| {
        inspector.record_counter("ReassemblyError", fragment_reassembly_error);
        inspector.record_counter("NeedMoreFragments", need_more_fragments);
        inspector.record_counter("InvalidFragment", invalid_fragment);
        inspector.record_counter("CacheFull", fragment_cache_full);
    });
}

fn inspect_udp_counters<I: Ip>(inspector: &mut impl Inspector, counters: &UdpCounters<I>) {
    let UdpCountersInner {
        rx_icmp_error,
        rx,
        rx_mapped_addr,
        rx_unknown_dest_port,
        rx_malformed,
        tx,
        tx_error,
    } = counters.as_ref();
    inspector.record_child("Rx", |inspector| {
        inspector.record_counter("Received", rx);
        inspector.record_child("Errors", |inspector| {
            inspector.record_counter("MappedAddr", rx_mapped_addr);
            inspector.record_counter("UnknownDstPort", rx_unknown_dest_port);
            inspector.record_counter("Malformed", rx_malformed);
        });
    });
    inspector.record_child("Tx", |inspector| {
        inspector.record_counter("Sent", tx);
        inspector.record_counter("Errors", tx_error);
    });
    inspector.record_counter("IcmpErrors", rx_icmp_error);
}
