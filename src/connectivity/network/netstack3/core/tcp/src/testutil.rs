// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ops::Range;

use netstack3_base::{Mss, SackBlock, SackBlocks, SeqNum};

/// Per RFC 879 section 1 (https://tools.ietf.org/html/rfc879#section-1):
///
/// THE TCP MAXIMUM SEGMENT SIZE IS THE IP MAXIMUM DATAGRAM SIZE MINUS
/// FORTY.
///   The default IP Maximum Datagram Size is 576.
///   The default TCP Maximum Segment Size is 536.
pub(crate) const DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE: usize = 536;
pub(crate) const DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE: Mss =
    Mss(core::num::NonZeroU16::new(DEFAULT_IPV4_MAXIMUM_SEGMENT_SIZE_USIZE as u16).unwrap());

/// Per [RFC 9293 section 3.7.1]
///
/// > [...] or 1220 (1280 - 60) for IPv6.
///
/// [RFC 9293 section 3.7.1]: https://datatracker.ietf.org/doc/html/rfc9293#section-3.7.1
pub(crate) const DEFAULT_IPV6_MAXIMUM_SEGMENT_SIZE_USIZE: usize = 1220;
pub(crate) const DEFAULT_IPV6_MAXIMUM_SEGMENT_SIZE: Mss =
    Mss(core::num::NonZeroU16::new(DEFAULT_IPV6_MAXIMUM_SEGMENT_SIZE_USIZE as u16).unwrap());

/// Creates a [`SackBlocks`] from the sequence number ranges represented as
/// `u32`s.
pub(crate) fn sack_blocks(iter: impl IntoIterator<Item = Range<u32>>) -> SackBlocks {
    iter.into_iter()
        .map(|Range { start, end }| {
            SackBlock::try_new(SeqNum::new(start), SeqNum::new(end)).unwrap()
        })
        .collect()
}
