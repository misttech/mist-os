// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities for tracking the counts of various IP events.

use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};
use netstack3_base::{Counter, Inspectable, Inspector, InspectorExt as _};

use crate::internal::fragmentation::FragmentationCounters;

/// An IP extension trait supporting counters at the IP layer.
pub trait IpCountersIpExt: Ip {
    /// Receive counters.
    type RxCounters: Default + Inspectable;
}

impl IpCountersIpExt for Ipv4 {
    type RxCounters = Ipv4RxCounters;
}

impl IpCountersIpExt for Ipv6 {
    type RxCounters = Ipv6RxCounters;
}

/// Ip layer counters.
#[derive(Default, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpCounters<I: IpCountersIpExt> {
    /// Count of incoming IP unicast packets delivered.
    pub deliver_unicast: Counter,
    /// Count of incoming IP multicast packets delivered.
    pub deliver_multicast: Counter,
    /// Count of incoming IP packets that are dispatched to the appropriate protocol.
    pub dispatch_receive_ip_packet: Counter,
    /// Count of incoming IP packets destined to another host.
    pub dispatch_receive_ip_packet_other_host: Counter,
    /// Count of incoming IP packets received by the stack.
    pub receive_ip_packet: Counter,
    /// Count of sent outgoing IP packets.
    pub send_ip_packet: Counter,
    /// Count of packets to be forwarded which are instead dropped because
    /// forwarding is disabled.
    pub forwarding_disabled: Counter,
    /// Count of incoming packets forwarded to another host.
    pub forward: Counter,
    /// Count of incoming packets which cannot be forwarded because there is no
    /// route to the destination host.
    pub no_route_to_host: Counter,
    /// Count of incoming packets which cannot be forwarded because the MTU has
    /// been exceeded.
    pub mtu_exceeded: Counter,
    /// Count of incoming packets which cannot be forwarded because the TTL has
    /// expired.
    pub ttl_expired: Counter,
    /// Count of ICMP error messages received.
    pub receive_icmp_error: Counter,
    /// Count of IP fragment reassembly errors.
    pub fragment_reassembly_error: Counter,
    /// Count of IP fragments that could not be reassembled because more
    /// fragments were needed.
    pub need_more_fragments: Counter,
    /// Count of IP fragments that could not be reassembled because the fragment
    /// was invalid.
    pub invalid_fragment: Counter,
    /// Count of IP fragments that could not be reassembled because the stack's
    /// per-IP-protocol fragment cache was full.
    pub fragment_cache_full: Counter,
    /// Count of incoming IP packets not delivered because of a parameter problem.
    pub parameter_problem: Counter,
    /// Count of incoming IP packets with an unspecified destination address.
    pub unspecified_destination: Counter,
    /// Count of incoming IP packets with an unspecified source address.
    pub unspecified_source: Counter,
    /// Count of incoming IP packets dropped.
    pub dropped: Counter,
    /// Number of frames rejected because they'd cause illegal loopback
    /// addresses on the wire.
    pub tx_illegal_loopback_address: Counter,
    /// Version specific rx counters.
    pub version_rx: I::RxCounters,
    /// Count of incoming IP multicast packets that were dropped because
    /// The stack doesn't have any sockets that belong to the multicast group,
    /// and the stack isn't configured to forward the multicast packet.
    pub multicast_no_interest: Counter,
    /// Count of looped-back packets that held a cached conntrack entry that could
    /// not be downcasted to the expected type. This would happen if, for example, a
    /// packet was modified to a different IP version between EGRESS and INGRESS.
    pub invalid_cached_conntrack_entry: Counter,
    /// IP fragmentation counters.
    pub fragmentation: FragmentationCounters,
}

impl<I: IpCountersIpExt> Inspectable for IpCounters<I> {
    fn record<II: Inspector>(&self, inspector: &mut II) {
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
        } = self;
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
            inspector
                .record_counter("ErrorInnerSizeLimitExceeded", error_inner_size_limit_exceeded);
            inspector.record_counter("ErrorFragmentedSerializer", error_fragmented_serializer);
        });
    }
}

/// IPv4-specific Rx counters.
#[derive(Default)]
pub struct Ipv4RxCounters {
    /// Count of incoming broadcast IPv4 packets delivered.
    pub deliver_broadcast: Counter,
}

impl Inspectable for Ipv4RxCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { deliver_broadcast } = self;
        inspector.record_counter("DeliveredBroadcast", deliver_broadcast);
    }
}

/// IPv6-specific Rx counters.
#[derive(Default)]
pub struct Ipv6RxCounters {
    /// Count of incoming IPv6 packets dropped because the destination address
    /// is only tentatively assigned to the device.
    pub drop_for_tentative: Counter,
    /// Count of incoming IPv6 packets dropped due to a non-unicast source address.
    pub non_unicast_source: Counter,
    /// Count of incoming IPv6 packets discarded while processing extension
    /// headers.
    pub extension_header_discard: Counter,
    /// Count of incoming neighbor solicitations discarded as looped-back
    /// DAD probes.
    pub drop_looped_back_dad_probe: Counter,
}

impl Inspectable for Ipv6RxCounters {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self {
            drop_for_tentative,
            non_unicast_source,
            extension_header_discard,
            drop_looped_back_dad_probe,
        } = self;
        inspector.record_counter("DroppedTentativeDst", drop_for_tentative);
        inspector.record_counter("DroppedNonUnicastSrc", non_unicast_source);
        inspector.record_counter("DroppedExtensionHeader", extension_header_discard);
        inspector.record_counter("DroppedLoopedBackDadProbe", drop_looped_back_dad_probe);
    }
}
