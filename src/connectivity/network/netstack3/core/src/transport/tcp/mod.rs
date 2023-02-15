// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Transmission Control Protocol (TCP).

pub mod buffer;
mod congestion;
mod rtt;
pub mod segment;
mod seqnum;
pub mod socket;
pub mod state;

use core::num::{NonZeroU16, NonZeroU64, NonZeroU8};

use const_unwrap::const_unwrap_option;
use net_types::ip::{Ip, IpVersion};
use packet_formats::utils::NonZeroDuration;
use rand::RngCore;

use crate::{
    ip::{socket::Mms, IpDeviceId, IpExt},
    sync::Mutex,
    transport::tcp::{
        self,
        seqnum::WindowSize,
        socket::{isn::IsnGenerator, Sockets},
    },
};

/// Control flags that can alter the state of a TCP control block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    /// Corresponds to the SYN bit in a TCP segment.
    SYN,
    /// Corresponds to the FIN bit in a TCP segment.
    FIN,
    /// Corresponds to the RST bit in a TCP segment.
    RST,
}

impl Control {
    /// Returns whether the control flag consumes one byte from the sequence
    /// number space.
    fn has_sequence_no(self) -> bool {
        match self {
            Control::SYN | Control::FIN => true,
            Control::RST => false,
        }
    }
}

/// Errors surfaced to the user.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) enum UserError {
    /// The connection was reset because of a RST segment.
    ConnectionReset,
    /// The connection was closed because of a user request.
    ConnectionClosed,
}

pub(crate) struct TcpState<I: IpExt, D: IpDeviceId, C: tcp::socket::NonSyncContext> {
    pub(crate) isn_generator: IsnGenerator<C::Instant>,
    pub(crate) sockets: Mutex<Sockets<I, D, C>>,
}

impl<I: IpExt, D: IpDeviceId, C: tcp::socket::NonSyncContext> TcpState<I, D, C> {
    pub(crate) fn new(now: C::Instant, rng: &mut impl RngCore) -> Self {
        Self { isn_generator: IsnGenerator::new(now, rng), sockets: Mutex::new(Sockets::new(rng)) }
    }
}

const TCP_HEADER_LEN: u32 = packet_formats::tcp::HDR_PREFIX_LEN as u32;

/// Maximum segment size, that is the maximum TCP payload one segment can carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub(crate) struct Mss(NonZeroU16);

impl Mss {
    /// Creates MSS from the maximum message size of the IP layer.
    fn from_mms<I: IpExt>(mms: Mms) -> Option<Self> {
        NonZeroU16::new(
            u16::try_from(mms.get().get().saturating_sub(TCP_HEADER_LEN)).unwrap_or(u16::MAX),
        )
        .map(Self)
    }

    const fn default<I: Ip>() -> Self {
        // Per RFC 9293 Section 3.7.1:
        //  If an MSS Option is not received at connection setup, TCP
        //  implementations MUST assume a default send MSS of 536 (576 - 40) for
        //  IPv4 or 1220 (1280 - 60) for IPv6 (MUST-15).
        match I::VERSION {
            IpVersion::V4 => Mss(nonzero_ext::nonzero!(536_u16)),
            IpVersion::V6 => Mss(nonzero_ext::nonzero!(1220_u16)),
        }
    }

    /// Gets the numeric value of the MSS.
    const fn get(&self) -> NonZeroU16 {
        let Self(mss) = *self;
        mss
    }
}

impl From<Mss> for u32 {
    fn from(Mss(mss): Mss) -> Self {
        u32::from(mss.get())
    }
}

/// Named tuple for holding sizes of buffers for a socket.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct BufferSizes {
    /// The size of the send buffer.
    pub send: usize,
}

impl Default for BufferSizes {
    fn default() -> Self {
        let send = WindowSize::DEFAULT.into();
        Self { send }
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
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            keep_alive: KeepAlive::default(),
            // RFC 9293 Section 3.7.4:
            //   A TCP implementation SHOULD implement the Nagle algorithm to
            //   coalesce short segments
            nagle_enabled: true,
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
        Self {
            // Default values inspired by Linux's TCP implementation:
            // https://github.com/torvalds/linux/blob/0326074ff4652329f2a1a9c8685104576bd8d131/include/net/tcp.h#L155-L157
            idle: NonZeroDuration::from_nonzero_secs(const_unwrap_option(NonZeroU64::new(
                2 * 60 * 60,
            ))),
            interval: NonZeroDuration::from_nonzero_secs(const_unwrap_option(NonZeroU64::new(75))),
            count: const_unwrap_option(NonZeroU8::new(9)),
            // Per RFC 9293(https://datatracker.ietf.org/doc/html/rfc9293#section-3.8.4):
            //   ... they MUST default to off.
            enabled: false,
        }
    }
}

#[cfg(test)]
mod testutil {
    use super::Mss;
    /// Per RFC 879 section 1 (https://tools.ietf.org/html/rfc879#section-1):
    ///
    /// THE TCP MAXIMUM SEGMENT SIZE IS THE IP MAXIMUM DATAGRAM SIZE MINUS
    /// FORTY.
    ///   The default IP Maximum Datagram Size is 576.
    ///   The default TCP Maximum Segment Size is 536.
    pub(super) const DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE: usize = 536;
    pub(super) const DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE: Mss =
        Mss(nonzero_ext::nonzero!(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE as u16));
}
