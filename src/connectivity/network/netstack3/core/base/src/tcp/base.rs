// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Transmission Control Protocol (TCP).

use core::num::{NonZeroU16, TryFromIntError};
use core::ops::Range;

use alloc::borrow::ToOwned as _;
use alloc::vec::Vec;
use const_unwrap::const_unwrap_option;
use net_types::ip::{Ip, IpVersion};
use packet::InnerPacketBuilder;
use packet_formats::ip::IpExt;

use crate::ip::Mms;
use crate::tcp::segment::{Payload, PayloadLen};

/// Control flags that can alter the state of a TCP control block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
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
    pub fn has_sequence_no(self) -> bool {
        match self {
            Control::SYN | Control::FIN => true,
            Control::RST => false,
        }
    }
}

const TCP_HEADER_LEN: u32 = packet_formats::tcp::HDR_PREFIX_LEN as u32;

/// Maximum segment size, that is the maximum TCP payload one segment can carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Mss(pub NonZeroU16);

impl Mss {
    /// Creates MSS from the maximum message size of the IP layer.
    pub fn from_mms<I: IpExt>(mms: Mms) -> Option<Self> {
        NonZeroU16::new(
            u16::try_from(mms.get().get().saturating_sub(TCP_HEADER_LEN)).unwrap_or(u16::MAX),
        )
        .map(Self)
    }

    /// Create a new [`Mss`] with the IP-version default value, as defined by RFC 9293.
    pub const fn default<I: Ip>() -> Self {
        // Per RFC 9293 Section 3.7.1:
        //  If an MSS Option is not received at connection setup, TCP
        //  implementations MUST assume a default send MSS of 536 (576 - 40) for
        //  IPv4 or 1220 (1280 - 60) for IPv6 (MUST-15).
        match I::VERSION {
            IpVersion::V4 => Mss(const_unwrap_option(NonZeroU16::new(536))),
            IpVersion::V6 => Mss(const_unwrap_option(NonZeroU16::new(1220))),
        }
    }

    /// Gets the numeric value of the MSS.
    pub const fn get(&self) -> NonZeroU16 {
        let Self(mss) = *self;
        mss
    }
}

impl From<Mss> for u32 {
    fn from(Mss(mss): Mss) -> Self {
        u32::from(mss.get())
    }
}

/// A type for the payload being sent.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SendPayload<'a> {
    /// The payload is contained in a single chunk of memory.
    Contiguous(&'a [u8]),
    /// The payload straddles across two chunks of memory.
    Straddle(&'a [u8], &'a [u8]),
}

impl SendPayload<'_> {
    /// Convert this [`SendPayload`] into an owned `Vec`.
    pub fn to_vec(self) -> Vec<u8> {
        match self {
            SendPayload::Contiguous(p) => p.to_owned(),
            SendPayload::Straddle(p1, p2) => [p1, p2].concat(),
        }
    }
}

impl PayloadLen for SendPayload<'_> {
    fn len(&self) -> usize {
        match self {
            SendPayload::Contiguous(p) => p.len(),
            SendPayload::Straddle(p1, p2) => p1.len() + p2.len(),
        }
    }
}

impl Payload for SendPayload<'_> {
    fn slice(self, range: Range<u32>) -> Self {
        match self {
            SendPayload::Contiguous(p) => SendPayload::Contiguous(p.slice(range)),
            SendPayload::Straddle(p1, p2) => {
                let Range { start, end } = range;
                let start = usize::try_from(start).unwrap_or_else(|TryFromIntError { .. }| {
                    panic!(
                        "range start index {} out of range for slice of length {}",
                        start,
                        self.len()
                    )
                });
                let end = usize::try_from(end).unwrap_or_else(|TryFromIntError { .. }| {
                    panic!(
                        "range end index {} out of range for slice of length {}",
                        end,
                        self.len()
                    )
                });
                assert!(start <= end);
                let first_len = p1.len();
                if start < first_len && end > first_len {
                    SendPayload::Straddle(&p1[start..first_len], &p2[0..end - first_len])
                } else if start >= first_len {
                    SendPayload::Contiguous(&p2[start - first_len..end - first_len])
                } else {
                    SendPayload::Contiguous(&p1[start..end])
                }
            }
        }
    }

    fn partial_copy(&self, offset: usize, dst: &mut [u8]) {
        match self {
            SendPayload::Contiguous(p) => p.partial_copy(offset, dst),
            SendPayload::Straddle(p1, p2) => {
                if offset < p1.len() {
                    let first_len = dst.len().min(p1.len() - offset);
                    p1.partial_copy(offset, &mut dst[..first_len]);
                    if dst.len() > first_len {
                        p2.partial_copy(0, &mut dst[first_len..]);
                    }
                } else {
                    p2.partial_copy(offset - p1.len(), dst);
                }
            }
        }
    }
}

impl InnerPacketBuilder for SendPayload<'_> {
    fn bytes_len(&self) -> usize {
        match self {
            SendPayload::Contiguous(p) => p.len(),
            SendPayload::Straddle(p1, p2) => p1.len() + p2.len(),
        }
    }

    fn serialize(&self, buffer: &mut [u8]) {
        self.partial_copy(0, buffer);
    }
}
