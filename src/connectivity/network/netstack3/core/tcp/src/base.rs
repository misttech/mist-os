// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Transmission Control Protocol (TCP).

use core::num::NonZeroU8;
use core::time::Duration;

use net_types::ip::{GenericOverIp, Ip, IpMarked, Mtu};
use net_types::SpecifiedAddr;
use netstack3_base::{
    Counter, IcmpErrorCode, Icmpv4ErrorCode, Icmpv6ErrorCode, IpExt, UnscaledWindowSize,
    WeakDeviceIdentifier, WindowSize,
};
use netstack3_ip::socket::{RouteResolutionOptions, SendOptions};
use netstack3_ip::Marks;
use packet_formats::icmp::{Icmpv4DestUnreachableCode, Icmpv6DestUnreachableCode};
use packet_formats::ip::DscpAndEcn;
use packet_formats::utils::NonZeroDuration;
use rand::Rng;

use crate::internal::socket::isn::IsnGenerator;
use crate::internal::socket::{DualStackIpExt, Sockets, TcpBindingsTypes};
use crate::internal::state::DEFAULT_MAX_SYN_RETRIES;

/// Default lifetime for a orphaned connection in FIN_WAIT2.
pub const DEFAULT_FIN_WAIT2_TIMEOUT: Duration = Duration::from_secs(60);

/// Errors surfaced to the user.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    /// The connection was refused, RST segment received while in SYN_SENT state.
    ConnectionRefused,
    /// The connection was reset because of a RST segment.
    ConnectionReset,
    /// The connection was closed because the network is unreachable.
    NetworkUnreachable,
    /// The connection was closed because the host is unreachable.
    HostUnreachable,
    /// The connection was closed because the protocol is unreachable.
    ProtocolUnreachable,
    /// The connection was closed because the port is unreachable.
    PortUnreachable,
    /// The connection was closed because the host is down.
    DestinationHostDown,
    /// The connection was closed because the source route failed.
    SourceRouteFailed,
    /// The connection was closed because the source host is isolated.
    SourceHostIsolated,
    /// The connection was closed because of a time out.
    TimedOut,
}

impl ConnectionError {
    // Notes: the following mappings are guided by the packetimpact test here:
    // https://cs.opensource.google/gvisor/gvisor/+/master:test/packetimpact/tests/tcp_network_unreachable_test.go;drc=611e6e1247a0691f5fd198f411c68b3bc79d90af
    pub(crate) fn try_from_icmp_error(err: IcmpErrorCode) -> Option<Self> {
        match err {
            IcmpErrorCode::V4(Icmpv4ErrorCode::DestUnreachable(code)) => match code {
                Icmpv4DestUnreachableCode::DestNetworkUnreachable => {
                    Some(ConnectionError::NetworkUnreachable)
                }
                Icmpv4DestUnreachableCode::DestHostUnreachable => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv4DestUnreachableCode::DestProtocolUnreachable => {
                    Some(ConnectionError::ProtocolUnreachable)
                }
                Icmpv4DestUnreachableCode::DestPortUnreachable => {
                    Some(ConnectionError::PortUnreachable)
                }
                Icmpv4DestUnreachableCode::FragmentationRequired => None,
                Icmpv4DestUnreachableCode::SourceRouteFailed => {
                    Some(ConnectionError::SourceRouteFailed)
                }
                Icmpv4DestUnreachableCode::DestNetworkUnknown => {
                    Some(ConnectionError::NetworkUnreachable)
                }
                Icmpv4DestUnreachableCode::DestHostUnknown => {
                    Some(ConnectionError::DestinationHostDown)
                }
                Icmpv4DestUnreachableCode::SourceHostIsolated => {
                    Some(ConnectionError::SourceHostIsolated)
                }
                Icmpv4DestUnreachableCode::NetworkAdministrativelyProhibited => {
                    Some(ConnectionError::NetworkUnreachable)
                }
                Icmpv4DestUnreachableCode::HostAdministrativelyProhibited => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv4DestUnreachableCode::NetworkUnreachableForToS => {
                    Some(ConnectionError::NetworkUnreachable)
                }
                Icmpv4DestUnreachableCode::HostUnreachableForToS => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv4DestUnreachableCode::CommAdministrativelyProhibited => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv4DestUnreachableCode::HostPrecedenceViolation => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv4DestUnreachableCode::PrecedenceCutoffInEffect => {
                    Some(ConnectionError::HostUnreachable)
                }
            },
            // TODO(https://fxbug.dev/42052672): Map the following ICMP messages.
            IcmpErrorCode::V4(
                Icmpv4ErrorCode::ParameterProblem(_)
                | Icmpv4ErrorCode::Redirect(_)
                | Icmpv4ErrorCode::TimeExceeded(_),
            ) => None,
            IcmpErrorCode::V6(Icmpv6ErrorCode::DestUnreachable(code)) => match code {
                Icmpv6DestUnreachableCode::NoRoute => Some(ConnectionError::NetworkUnreachable),
                Icmpv6DestUnreachableCode::CommAdministrativelyProhibited => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv6DestUnreachableCode::BeyondScope => Some(ConnectionError::NetworkUnreachable),
                Icmpv6DestUnreachableCode::AddrUnreachable => {
                    Some(ConnectionError::HostUnreachable)
                }
                Icmpv6DestUnreachableCode::PortUnreachable => {
                    Some(ConnectionError::PortUnreachable)
                }
                Icmpv6DestUnreachableCode::SrcAddrFailedPolicy => {
                    Some(ConnectionError::SourceRouteFailed)
                }
                Icmpv6DestUnreachableCode::RejectRoute => Some(ConnectionError::NetworkUnreachable),
            },
            // TODO(https://fxbug.dev/42052672): Map the following ICMP messages.
            IcmpErrorCode::V6(
                Icmpv6ErrorCode::PacketTooBig
                | Icmpv6ErrorCode::ParameterProblem(_)
                | Icmpv6ErrorCode::TimeExceeded(_),
            ) => None,
        }
    }
}

/// Stack wide state supporting TCP.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct TcpState<I: DualStackIpExt, D: WeakDeviceIdentifier, BT: TcpBindingsTypes> {
    /// The initial sequence number generator.
    pub isn_generator: IsnGenerator<BT::Instant>,
    /// TCP sockets state.
    pub sockets: Sockets<I, D, BT>,
    /// TCP counters.
    pub counters: TcpCounters<I>,
}

impl<I: DualStackIpExt, D: WeakDeviceIdentifier, BT: TcpBindingsTypes> TcpState<I, D, BT> {
    /// Creates a new TCP stack state.
    pub fn new(now: BT::Instant, rng: &mut impl Rng) -> Self {
        Self {
            isn_generator: IsnGenerator::new(now, rng),
            sockets: Sockets::new(),
            counters: Default::default(),
        }
    }
}

/// Named tuple for holding sizes of buffers for a socket.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct BufferSizes {
    /// The size of the send buffer.
    pub send: usize,
    /// The size of the receive buffer.
    pub receive: usize,
}
/// Sensible defaults only for testing.
#[cfg(any(test, feature = "testutils"))]
impl Default for BufferSizes {
    fn default() -> Self {
        BufferSizes { send: WindowSize::DEFAULT.into(), receive: WindowSize::DEFAULT.into() }
    }
}

impl BufferSizes {
    pub(crate) fn rwnd(&self) -> WindowSize {
        let Self { send: _, receive } = *self;
        WindowSize::new(receive).unwrap_or(WindowSize::MAX)
    }

    pub(crate) fn rwnd_unscaled(&self) -> UnscaledWindowSize {
        let Self { send: _, receive } = *self;
        UnscaledWindowSize::from(u16::try_from(receive).unwrap_or(u16::MAX))
    }
}

/// A mutable reference to buffer configuration.
pub(crate) enum BuffersRefMut<'a, R, S> {
    /// All buffers are dropped.
    NoBuffers,
    /// Buffer sizes are configured but not instantiated yet.
    Sizes(&'a mut BufferSizes),
    /// Buffers are instantiated and mutable references are provided.
    Both { send: &'a mut S, recv: &'a mut R },
    /// Only the send buffer is still instantiated, which happens in Closing
    /// states.
    SendOnly(&'a mut S),
    /// Only the receive buffer is still instantiated, which happens in Finwait
    /// states.
    RecvOnly(&'a mut R),
}

impl<'a, R, S> BuffersRefMut<'a, R, S> {
    pub(crate) fn into_send_buffer(self) -> Option<&'a mut S> {
        match self {
            Self::NoBuffers | Self::Sizes(_) | Self::RecvOnly(_) => None,
            Self::Both { send, recv: _ } | Self::SendOnly(send) => Some(send),
        }
    }

    pub(crate) fn into_receive_buffer(self) -> Option<&'a mut R> {
        match self {
            Self::NoBuffers | Self::Sizes(_) | Self::SendOnly(_) => None,
            Self::Both { send: _, recv } | Self::RecvOnly(recv) => Some(recv),
        }
    }
}

/// The IP sock options used by TCP.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TcpIpSockOptions {
    /// Socket marks used for routing.
    pub marks: Marks,
}

impl<I: Ip> RouteResolutionOptions<I> for TcpIpSockOptions {
    fn marks(&self) -> &Marks {
        &self.marks
    }

    fn transparent(&self) -> bool {
        false
    }
}

impl<I: IpExt> SendOptions<I> for TcpIpSockOptions {
    fn hop_limit(&self, _destination: &SpecifiedAddr<I::Addr>) -> Option<NonZeroU8> {
        None
    }

    fn multicast_loop(&self) -> bool {
        false
    }

    fn allow_broadcast(&self) -> Option<I::BroadcastMarker> {
        None
    }

    fn dscp_and_ecn(&self) -> DscpAndEcn {
        DscpAndEcn::default()
    }

    fn mtu(&self) -> Mtu {
        Mtu::no_limit()
    }
}

/// TCP socket options.
///
/// This only stores options that are trivial to get and set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketOptions {
    /// Socket options that control TCP keep-alive mechanism, see [`KeepAlive`].
    pub keep_alive: KeepAlive,
    /// Switch to turn nagle algorithm on/off.
    pub nagle_enabled: bool,
    /// The period of time after which the connection should be aborted if no
    /// ACK is received.
    pub user_timeout: Option<NonZeroDuration>,
    /// Switch to turn delayed ACK on/off.
    pub delayed_ack: bool,
    /// The period of time after with a dangling FIN_WAIT2 state should be
    /// reclaimed.
    pub fin_wait2_timeout: Option<Duration>,
    /// The maximum SYN retransmissions before aborting a connection.
    pub max_syn_retries: NonZeroU8,
    /// Ip socket options.
    pub ip_options: TcpIpSockOptions,
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            keep_alive: KeepAlive::default(),
            // RFC 9293 Section 3.7.4:
            //   A TCP implementation SHOULD implement the Nagle algorithm to
            //   coalesce short segments
            nagle_enabled: true,
            user_timeout: None,
            // RFC 9293 Section 4.2:
            //   The delayed ACK algorithm specified in [RFC1122] SHOULD be used
            //   by a TCP receiver.
            // Delayed acks have *bad* performance for connections that are not
            // interactive, especially when combined with the Nagle algorithm.
            // We disable it by default here because:
            //   1. RFC does not say MUST;
            //   2. Common implementations like Linux has it turned off by
            //   default.
            // More context: https://news.ycombinator.com/item?id=10607422
            delayed_ack: false,
            fin_wait2_timeout: Some(DEFAULT_FIN_WAIT2_TIMEOUT),
            max_syn_retries: DEFAULT_MAX_SYN_RETRIES,
            ip_options: TcpIpSockOptions::default(),
        }
    }
}

/// Options that are related to TCP keep-alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeepAlive {
    /// The amount of time for an idle connection to wait before sending out
    /// probes.
    pub idle: NonZeroDuration,
    /// Interval between consecutive probes.
    pub interval: NonZeroDuration,
    /// Maximum number of probes we send before considering the connection dead.
    ///
    /// `u8` is enough because if a connection doesn't hear back from the peer
    /// after 256 probes, then chances are that the connection is already dead.
    pub count: NonZeroU8,
    /// Only send probes if keep-alive is enabled.
    pub enabled: bool,
}

impl Default for KeepAlive {
    fn default() -> Self {
        // Default values inspired by Linux's TCP implementation:
        // https://github.com/torvalds/linux/blob/0326074ff4652329f2a1a9c8685104576bd8d131/include/net/tcp.h#L155-L157
        const DEFAULT_IDLE_DURATION: NonZeroDuration =
            NonZeroDuration::from_secs(2 * 60 * 60).unwrap();
        const DEFAULT_INTERVAL: NonZeroDuration = NonZeroDuration::from_secs(75).unwrap();
        const DEFAULT_COUNT: NonZeroU8 = NonZeroU8::new(9).unwrap();

        Self {
            idle: DEFAULT_IDLE_DURATION,
            interval: DEFAULT_INTERVAL,
            count: DEFAULT_COUNT,
            // Per RFC 9293(https://datatracker.ietf.org/doc/html/rfc9293#section-3.8.4):
            //   ... they MUST default to off.
            enabled: false,
        }
    }
}

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

#[cfg(test)]
pub(crate) mod testutil {
    use netstack3_base::Mss;
    /// Per RFC 879 section 1 (https://tools.ietf.org/html/rfc879#section-1):
    ///
    /// THE TCP MAXIMUM SEGMENT SIZE IS THE IP MAXIMUM DATAGRAM SIZE MINUS
    /// FORTY.
    ///   The default IP Maximum Datagram Size is 576.
    ///   The default TCP Maximum Segment Size is 536.
    pub(crate) const DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE: usize = 536;
    pub(crate) const DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE: Mss =
        Mss(core::num::NonZeroU16::new(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE as u16).unwrap());
}
