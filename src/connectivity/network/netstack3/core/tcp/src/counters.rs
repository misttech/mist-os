// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities for tracking the counts of various TCP events.

use net_types::ip::IpMarked;
use netstack3_base::{Counter, Inspectable, Inspector, InspectorExt as _};

/// TCP Counters.
///
/// Accrued for the entire stack, rather than on a per connection basis.
///
/// Note that for dual stack sockets, all events will be attributed to the IPv6
/// counters.
pub type TcpCounters<I> = IpMarked<I, TcpCountersInner>;

/// The IP agnostic version of [`TcpCounters`].
#[derive(Default)]
// TODO(https://fxbug.dev/42052878): Add counters for SYN cookies.
// TODO(https://fxbug.dev/42078221): Add counters for SACK.
pub struct TcpCountersInner {
    /// Count of received IP packets that were dropped because they had
    /// unexpected IP addresses (either src or dst).
    pub invalid_ip_addrs_received: Counter,
    /// Count of received TCP segments that were dropped because they could not
    /// be parsed.
    pub invalid_segments_received: Counter,
    /// Count of received TCP segments that were valid.
    pub valid_segments_received: Counter,
    /// Count of received TCP segments that were successfully dispatched to a
    /// socket.
    pub received_segments_dispatched: Counter,
    /// Count of received TCP segments that were not associated with any
    /// existing sockets.
    pub received_segments_no_dispatch: Counter,
    /// Count of received TCP segments that were dropped because the listener
    /// queue was full.
    pub listener_queue_overflow: Counter,
    /// Count of TCP segments that failed to send.
    pub segment_send_errors: Counter,
    /// Count of TCP segments that were sent.
    pub segments_sent: Counter,
    /// Count of passive open attempts that failed because the stack doesn't
    /// have route to the peer.
    pub passive_open_no_route_errors: Counter,
    /// Count of passive connections that have been opened.
    pub passive_connection_openings: Counter,
    /// Count of active open attempts that have failed because the stack doesn't
    /// have a route to the peer.
    pub active_open_no_route_errors: Counter,
    /// Count of active connections that have been opened.
    pub active_connection_openings: Counter,
    /// Count of all failed connection attempts, including both passive and
    /// active opens.
    pub failed_connection_attempts: Counter,
    /// Count of port reservation attempts that failed.
    pub failed_port_reservations: Counter,
    /// Count of received segments whose checksums were invalid.
    pub checksum_errors: Counter,
    /// Count of received segments with the RST flag set.
    pub resets_received: Counter,
    /// Count of sent segments with the RST flag set.
    pub resets_sent: Counter,
    /// Count of received segments with the SYN flag set.
    pub syns_received: Counter,
    /// Count of sent segments with the SYN flag set.
    pub syns_sent: Counter,
    /// Count of received segments with the FIN flag set.
    pub fins_received: Counter,
    /// Count of sent segments with the FIN flag set.
    pub fins_sent: Counter,
    /// Count of retransmission timeouts.
    pub timeouts: Counter,
    /// Count of retransmissions of segments.
    pub retransmits: Counter,
    /// Count of retransmissions of segments while in slow start.
    pub slow_start_retransmits: Counter,
    /// Count of retransmissions of segments while in fast recovery.
    pub fast_retransmits: Counter,
    /// Count of times fast recovery was initiated to recover from packet loss.
    pub fast_recovery: Counter,
    /// Count of times an established TCP connection transitioned to CLOSED.
    pub established_closed: Counter,
    /// Count of times an established TCP connection transitioned to CLOSED due
    /// to a RST segment.
    pub established_resets: Counter,
    /// Count of times an established TCP connection transitioned to CLOSED due
    /// to a timeout (e.g. a keep-alive or retransmit timeout).
    pub established_timedout: Counter,
}

impl Inspectable for TcpCountersInner {
    fn record<I: Inspector>(&self, inspector: &mut I) {
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
        } = self;
        inspector.record_child("Rx", |inspector| {
            inspector.record_counter("ValidSegmentsReceived", valid_segments_received);
            inspector.record_counter("ReceivedSegmentsDispatched", received_segments_dispatched);
            inspector.record_counter("ResetsReceived", resets_received);
            inspector.record_counter("SynsReceived", syns_received);
            inspector.record_counter("FinsReceived", fins_received);
            inspector.record_child("Errors", |inspector| {
                inspector.record_counter("InvalidIpAddrsReceived", invalid_ip_addrs_received);
                inspector.record_counter("InvalidSegmentsReceived", invalid_segments_received);
                inspector
                    .record_counter("ReceivedSegmentsNoDispatch", received_segments_no_dispatch);
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
}
