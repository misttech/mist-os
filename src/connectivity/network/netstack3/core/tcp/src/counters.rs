// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities for tracking the counts of various TCP events.

use net_types::ip::{Ip, IpMarked};
use netstack3_base::{Counter, CounterContext, Inspectable, Inspector, InspectorExt as _};

/// TCP Counters.
///
/// Accrued for the entire stack, rather than on a per connection basis.
///
/// Note that for dual stack sockets, all events will be attributed to the IPv6
/// counters.
pub type TcpCounters<I> = IpMarked<I, TcpCountersInner>;

/// The IP agnostic version of [`TcpCounters`].
///
/// The counter type `C` is generic to facilitate testing.
#[derive(Default)]
#[cfg_attr(test, derive(Debug, PartialEq))]
// TODO(https://fxbug.dev/42052878): Add counters for SYN cookies.
// TODO(https://fxbug.dev/42078221): Add counters for SACK.
pub struct TcpCountersInner<C = Counter> {
    /// Count of received IP packets that were dropped because they had
    /// unexpected IP addresses (either src or dst).
    pub invalid_ip_addrs_received: C,
    /// Count of received TCP segments that were dropped because they could not
    /// be parsed.
    pub invalid_segments_received: C,
    /// Count of received TCP segments that were valid.
    pub valid_segments_received: C,
    /// Count of received TCP segments that were successfully dispatched to a
    /// socket.
    pub received_segments_dispatched: C,
    /// Count of received TCP segments that were not associated with any
    /// existing sockets.
    pub received_segments_no_dispatch: C,
    /// Count of received TCP segments that were dropped because the listener
    /// queue was full.
    pub listener_queue_overflow: C,
    /// Count of TCP segments that failed to send.
    pub segment_send_errors: C,
    /// Count of TCP segments that were sent.
    pub segments_sent: C,
    /// Count of passive open attempts that failed because the stack doesn't
    /// have route to the peer.
    pub passive_open_no_route_errors: C,
    /// Count of passive connections that have been opened.
    pub passive_connection_openings: C,
    /// Count of active open attempts that have failed because the stack doesn't
    /// have a route to the peer.
    pub active_open_no_route_errors: C,
    /// Count of active connections that have been opened.
    pub active_connection_openings: C,
    /// Count of all failed connection attempts, including both passive and
    /// active opens.
    pub failed_connection_attempts: C,
    /// Count of port reservation attempts that failed.
    pub failed_port_reservations: C,
    /// Count of received segments whose checksums were invalid.
    pub checksum_errors: C,
    /// Count of received segments with the RST flag set.
    pub resets_received: C,
    /// Count of sent segments with the RST flag set.
    pub resets_sent: C,
    /// Count of received segments with the SYN flag set.
    pub syns_received: C,
    /// Count of sent segments with the SYN flag set.
    pub syns_sent: C,
    /// Count of received segments with the FIN flag set.
    pub fins_received: C,
    /// Count of sent segments with the FIN flag set.
    pub fins_sent: C,
    /// Count of retransmission timeouts.
    pub timeouts: C,
    /// Count of retransmissions of segments.
    pub retransmits: C,
    /// Count of retransmissions of segments while in slow start.
    pub slow_start_retransmits: C,
    /// Count of retransmissions of segments while in fast recovery.
    pub fast_retransmits: C,
    /// Count of times fast recovery was initiated to recover from packet loss.
    pub fast_recovery: C,
    /// Count of times an established TCP connection transitioned to CLOSED.
    pub established_closed: C,
    /// Count of times an established TCP connection transitioned to CLOSED due
    /// to a RST segment.
    pub established_resets: C,
    /// Count of times an established TCP connection transitioned to CLOSED due
    /// to a timeout (e.g. a keep-alive or retransmit timeout).
    pub established_timedout: C,
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

/// Holds references to the stack-wide and per-socket counters.
///
/// This is used to easily increment both counters in contexts that don't have
/// access to `ResourceCounterContext`. This is currently used by the TCP state
/// machine, which does not operate on a core context so that it can remain
/// IP agnostic (and thus avoid duplicate code generation).
pub(crate) struct TcpCountersRefs<'a> {
    pub(crate) stack_wide: &'a TcpCountersInner,
    // TODO(https://fxbug.dev/42081990): Add per-socket counters.
}

impl<'a> TcpCountersRefs<'a> {
    pub(crate) fn from_ctx<I: Ip, CC: CounterContext<TcpCounters<I>>>(ctx: &'a CC) -> Self {
        TcpCountersRefs { stack_wide: ctx.counters() }
    }

    pub(crate) fn increment<F: Fn(&TcpCountersInner) -> &Counter>(&self, cb: F) {
        let Self { stack_wide } = self;
        cb(stack_wide).increment();
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    pub(crate) type CounterExpectations = TcpCountersInner<u64>;

    impl From<&TcpCountersInner> for CounterExpectations {
        fn from(counters: &TcpCountersInner) -> CounterExpectations {
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
            } = counters;
            TcpCountersInner {
                invalid_ip_addrs_received: invalid_ip_addrs_received.get(),
                invalid_segments_received: invalid_segments_received.get(),
                valid_segments_received: valid_segments_received.get(),
                received_segments_dispatched: received_segments_dispatched.get(),
                received_segments_no_dispatch: received_segments_no_dispatch.get(),
                listener_queue_overflow: listener_queue_overflow.get(),
                segment_send_errors: segment_send_errors.get(),
                segments_sent: segments_sent.get(),
                passive_open_no_route_errors: passive_open_no_route_errors.get(),
                passive_connection_openings: passive_connection_openings.get(),
                active_open_no_route_errors: active_open_no_route_errors.get(),
                active_connection_openings: active_connection_openings.get(),
                failed_connection_attempts: failed_connection_attempts.get(),
                failed_port_reservations: failed_port_reservations.get(),
                checksum_errors: checksum_errors.get(),
                resets_received: resets_received.get(),
                resets_sent: resets_sent.get(),
                syns_received: syns_received.get(),
                syns_sent: syns_sent.get(),
                fins_received: fins_received.get(),
                fins_sent: fins_sent.get(),
                timeouts: timeouts.get(),
                retransmits: retransmits.get(),
                slow_start_retransmits: slow_start_retransmits.get(),
                fast_retransmits: fast_retransmits.get(),
                fast_recovery: fast_recovery.get(),
                established_closed: established_closed.get(),
                established_resets: established_resets.get(),
                established_timedout: established_timedout.get(),
            }
        }
    }
}
