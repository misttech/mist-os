// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types for working with and exposing packet statistic counters.

use net_types::ip::{Ip, Ipv4, Ipv6};
use netstack3_base::{ContextPair, CounterContext, Inspector};
use netstack3_device::ethernet::EthernetDeviceCounters;
use netstack3_device::{ArpCounters, DeviceCounters};
use netstack3_ip::icmp::{
    IcmpRxCounters, IcmpRxCountersInner, IcmpTxCounters, IcmpTxCountersInner, NdpCounters,
    NdpRxCounters, NdpTxCounters,
};
use netstack3_ip::multicast_forwarding::MulticastForwardingCounters;
use netstack3_ip::nud::{NudCounters, NudCountersInner};
use netstack3_ip::raw::RawIpSocketCounters;
use netstack3_ip::{FragmentationCounters, IpCounters, IpLayerIpExt};
use netstack3_tcp::{TcpCounters, TcpCountersInner};
use netstack3_udp::{UdpCounters, UdpCountersInner};

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
        + CounterContext<MulticastForwardingCounters<Ipv4>>
        + CounterContext<MulticastForwardingCounters<Ipv6>>
        + CounterContext<RawIpSocketCounters<Ipv4>>
        + CounterContext<RawIpSocketCounters<Ipv6>>
        + CounterContext<UdpCounters<Ipv4>>
        + CounterContext<UdpCounters<Ipv6>>
        + CounterContext<TcpCounters<Ipv4>>
        + CounterContext<TcpCounters<Ipv6>>
        + CounterContext<IcmpRxCounters<Ipv4>>
        + CounterContext<IcmpRxCounters<Ipv6>>
        + CounterContext<IcmpTxCounters<Ipv4>>
        + CounterContext<IcmpTxCounters<Ipv4>>
        + CounterContext<IcmpTxCounters<Ipv6>>
        + CounterContext<NudCounters<Ipv4>>
        + CounterContext<NudCounters<Ipv6>>
        + CounterContext<NdpCounters>
        + CounterContext<ArpCounters>
        + CounterContext<DeviceCounters>
        + CounterContext<EthernetDeviceCounters>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair) = self;
        pair.core_ctx()
    }

    /// Exposes all of the stack wide counters through `inspector`.
    pub fn inspect_stack_counters<I: Inspector>(&mut self, inspector: &mut I) {
        inspector.record_child("Device", |inspector| {
            self.core_ctx().with_counters(|counters: &DeviceCounters| {
                inspector.delegate_inspectable(counters)
            });
            self.core_ctx().with_counters(|counters: &EthernetDeviceCounters| {
                inspector.delegate_inspectable(counters)
            });
        });
        inspector.record_child("Arp", |inspector| {
            self.core_ctx().with_counters(|arp| inspect_arp_counters(inspector, arp));
        });
        inspector.record_child("NUD", |inspector| {
            inspector.record_child("V4", |inspector| {
                self.core_ctx().with_counters(|nud| inspect_nud_counters::<Ipv4>(inspector, nud));
            });
            inspector.record_child("V6", |inspector| {
                self.core_ctx().with_counters(|nud| inspect_nud_counters::<Ipv6>(inspector, nud));
            });
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
        inspector.record_child("MulticastForwarding", |inspector| {
            inspector.record_child("V4", |inspector| {
                self.core_ctx().with_counters(|counters: &MulticastForwardingCounters<Ipv4>| {
                    inspector.delegate_inspectable(counters)
                })
            });
            inspector.record_child("V6", |inspector| {
                self.core_ctx().with_counters(|counters: &MulticastForwardingCounters<Ipv6>| {
                    inspector.delegate_inspectable(counters)
                })
            });
        });
        inspector.record_child("RawIpSockets", |inspector| {
            inspector.record_child("V4", |inspector| {
                self.core_ctx().with_counters(|counters: &RawIpSocketCounters<Ipv4>| {
                    inspector.delegate_inspectable(counters)
                })
            });
            inspector.record_child("V6", |inspector| {
                self.core_ctx().with_counters(|counters: &RawIpSocketCounters<Ipv6>| {
                    inspector.delegate_inspectable(counters)
                })
            });
        });
        inspector.record_child("UDP", |inspector| {
            inspector.record_child("V4", |inspector| {
                self.core_ctx().with_counters(|udp| inspect_udp_counters::<Ipv4>(inspector, udp))
            });
            inspector.record_child("V6", |inspector| {
                self.core_ctx().with_counters(|udp| inspect_udp_counters::<Ipv6>(inspector, udp))
            });
        });
        inspector.record_child("TCP", |inspector| {
            inspector.record_child("V4", |inspector| {
                self.core_ctx().with_counters(|tcp| inspect_tcp_counters::<Ipv4>(inspector, tcp))
            });
            inspector.record_child("V6", |inspector| {
                self.core_ctx().with_counters(|tcp| inspect_tcp_counters::<Ipv6>(inspector, tcp))
            });
        });
    }
}

fn inspect_nud_counters<I: Ip>(inspector: &mut impl Inspector, counters: &NudCounters<I>) {
    let NudCountersInner { icmp_dest_unreachable_dropped } = counters.as_ref();
    inspector.record_counter("IcmpDestUnreachableDropped", icmp_dest_unreachable_dropped);
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
        address_unreachable,
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
    inspector.record_counter("AddressUnreachable", address_unreachable);
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
        deliver_unicast,
        deliver_multicast,
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
        tx_illegal_loopback_address,
        version_rx,
        multicast_no_interest,
        invalid_cached_conntrack_entry,
        fragmentation,
    } = counters;
    inspector.record_child("PacketTx", |inspector| {
        inspector.record_counter("Sent", send_ip_packet);
        inspector.record_counter("IllegalLoopbackAddress", tx_illegal_loopback_address);
    });
    inspector.record_child("PacketRx", |inspector| {
        inspector.record_counter("Received", receive_ip_packet);
        inspector.record_counter("Dispatched", dispatch_receive_ip_packet);
        inspector.record_counter("OtherHost", dispatch_receive_ip_packet_other_host);
        inspector.record_counter("ParameterProblem", parameter_problem);
        inspector.record_counter("UnspecifiedDst", unspecified_destination);
        inspector.record_counter("UnspecifiedSrc", unspecified_source);
        inspector.record_counter("Dropped", dropped);
        inspector.record_counter("MulticastNoInterest", multicast_no_interest);
        inspector.record_counter("DeliveredUnicast", deliver_unicast);
        inspector.record_counter("DeliveredMulticast", deliver_multicast);
        inspector.record_counter("InvalidCachedConntrackEntry", invalid_cached_conntrack_entry);
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
    inspector.record_child("FragmentsRx", |inspector| {
        inspector.record_counter("ReassemblyError", fragment_reassembly_error);
        inspector.record_counter("NeedMoreFragments", need_more_fragments);
        inspector.record_counter("InvalidFragment", invalid_fragment);
        inspector.record_counter("CacheFull", fragment_cache_full);
    });
    inspector.record_child("FragmentsTx", |inspector| {
        let FragmentationCounters {
            fragmentation_required,
            fragments,
            error_not_allowed,
            error_mtu_too_small,
            error_body_too_long,
            error_inner_size_limit_exceeded,
            error_fragmented_serializer,
        } = fragmentation;
        inspector.record_counter("FragmentationRequired", fragmentation_required);
        inspector.record_counter("Fragments", fragments);
        inspector.record_counter("ErrorNotAllowed", error_not_allowed);
        inspector.record_counter("ErrorMtuTooSmall", error_mtu_too_small);
        inspector.record_counter("ErrorBodyTooLong", error_body_too_long);
        inspector.record_counter("ErrorInnerSizeLimitExceeded", error_inner_size_limit_exceeded);
        inspector.record_counter("ErrorFragmentedSerializer", error_fragmented_serializer);
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

fn inspect_tcp_counters<I: Ip>(inspector: &mut impl Inspector, counters: &TcpCounters<I>) {
    let TcpCountersInner {
        invalid_ip_addrs_received,
        invalid_segments_received,
        valid_segments_received,
        received_segments_dispatched,
        received_segments_no_dispatch,
        listener_queue_overflow,
        segment_send_errors,
        segments_sent,
        passive_open_no_route_errors,
        passive_connection_openings,
        active_open_no_route_errors,
        active_connection_openings,
        failed_connection_attempts,
        failed_port_reservations,
        checksum_errors,
        resets_received,
        resets_sent,
        syns_received,
        syns_sent,
        fins_received,
        fins_sent,
        timeouts,
        retransmits,
        slow_start_retransmits,
        fast_retransmits,
        fast_recovery,
        established_closed,
        established_resets,
        established_timedout,
    } = counters.as_ref();
    inspector.record_child("Rx", |inspector| {
        inspector.record_counter("ValidSegmentsReceived", valid_segments_received);
        inspector.record_counter("ReceivedSegmentsDispatched", received_segments_dispatched);
        inspector.record_counter("ResetsReceived", resets_received);
        inspector.record_counter("SynsReceived", syns_received);
        inspector.record_counter("FinsReceived", fins_received);
        inspector.record_child("Errors", |inspector| {
            inspector.record_counter("InvalidIpAddrsReceived", invalid_ip_addrs_received);
            inspector.record_counter("InvalidSegmentsReceived", invalid_segments_received);
            inspector.record_counter("ReceivedSegmentsNoDispatch", received_segments_no_dispatch);
            inspector.record_counter("ListenerQueueOverflow", listener_queue_overflow);
            inspector.record_counter("PassiveOpenNoRouteErrors", passive_open_no_route_errors);
            inspector.record_counter("ChecksumErrors", checksum_errors);
        })
    });
    inspector.record_child("Tx", |inspector| {
        inspector.record_counter("SegmentsSent", segments_sent);
        inspector.record_counter("ResetsSent", resets_sent);
        inspector.record_counter("SynsSent", syns_sent);
        inspector.record_counter("FinsSent", fins_sent);
        inspector.record_counter("Timeouts", timeouts);
        inspector.record_counter("Retransmits", retransmits);
        inspector.record_counter("SlowStartRetransmits", slow_start_retransmits);
        inspector.record_counter("FastRetransmits", fast_retransmits);
        inspector.record_child("Errors", |inspector| {
            inspector.record_counter("SegmentSendErrors", segment_send_errors);
            inspector.record_counter("ActiveOpenNoRouteErrors", active_open_no_route_errors);
        });
    });
    inspector.record_counter("PassiveConnectionOpenings", passive_connection_openings);
    inspector.record_counter("ActiveConnectionOpenings", active_connection_openings);
    inspector.record_counter("FastRecovery", fast_recovery);
    inspector.record_counter("EstablishedClosed", established_closed);
    inspector.record_counter("EstablishedResets", established_resets);
    inspector.record_counter("EstablishedTimedout", established_timedout);
    inspector.record_child("Errors", |inspector| {
        inspector.record_counter("FailedConnectionOpenings", failed_connection_attempts);
        inspector.record_counter("FailedPortReservations", failed_port_reservations);
    })
}
