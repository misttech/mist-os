// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declare types related to the per-socket filters of raw IP sockets.

use net_types::ip::{GenericOverIp, Ip, IpVersionMarker};
use packet_formats::icmp::IcmpIpExt;

/// An ICMP filter installed on a raw IP socket.
#[derive(Debug, GenericOverIp, PartialEq, Clone)]
#[generic_over_ip(I, Ip)]
pub struct RawIpSocketIcmpFilter<I: IcmpIpExt> {
    _marker: IpVersionMarker<I>,
    /// The raw 256-bit filter. If bit `n` is set, ICMP messages with type `n`
    /// will be filtered.
    ///
    /// Note: if bit `n` is an invalid message type, the packet will be dropped
    /// regardless of if the bit is set or not.
    filter: [u8; 32],
}

impl<I: IcmpIpExt> RawIpSocketIcmpFilter<I> {
    /// An ICMP filter that allows all message types to be delivered.
    pub const ALLOW_ALL: Self = Self::new([0; 32]);

    /// An ICMP filter that prevents all message types from being delivered.
    pub const DENY_ALL: Self = Self::new([u8::MAX; 32]);

    /// Construct a `RawIpSocketIcmpFilter` from the raw bytes.
    ///
    /// The array is expected to be little endian. E.g. byte 0 in the array is
    /// used to control filters for types 0-7.
    pub const fn new(filter: [u8; 32]) -> RawIpSocketIcmpFilter<I> {
        RawIpSocketIcmpFilter { _marker: IpVersionMarker::new(), filter }
    }

    /// Convert the `RawIpSocketIcmpFilter` into the raw bytes.
    ///
    /// The array is returned in little endian format.
    pub fn into_bytes(self) -> [u8; 32] {
        let RawIpSocketIcmpFilter { _marker, filter } = self;
        filter
    }

    /// True if this `RawIpSocketIcmpFilter` allows ICMP messages of the given type.
    pub(super) fn allows_type(&self, message_type: I::IcmpMessageType) -> bool {
        let message_type: u8 = message_type.into();
        let byte: u8 = message_type / 8;
        let bit: u8 = message_type % 8;
        // NB: message_type has a max value of 255 (u8::MAX); once divided by 8
        // it's maximum value becomes 31, so `byte` cannot exceed the array
        // bounds on `self.filter`, which has a length of 32.
        (self.filter[usize::from(byte)] & (1 << bit)) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ip_test_macro::ip_test;
    use net_types::ip::{Ipv4, Ipv6};
    use test_case::test_case;

    /// Builds a filter to precisely allow/disallow a given message type.
    ///
    /// E.g. when allow is true, the filter will be all 1s, except for the bit
    /// at message type. The filter will have the opposite value when allow is
    /// false.
    fn build_precise_filter<I: IcmpIpExt>(
        message_type: u8,
        allow: bool,
    ) -> RawIpSocketIcmpFilter<I> {
        let fill_byte: u8 = allow.then_some(u8::MAX).unwrap_or(0);
        let mut raw_filter = [fill_byte; 32];
        raw_filter[usize::from(message_type / 8)] = fill_byte ^ (1 << (message_type % 8));
        RawIpSocketIcmpFilter::new(raw_filter)
    }

    // NB: the test helper is complex enough to warrant a test of it's own.
    #[test_case(21, true, [
        0xFF, 0xFF, 0xDF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]; "allow_21")]
    #[test_case(21, false, [
        0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]; "deny_21")]
    fn build_filter(message_type: u8, allow: bool, expected: [u8; 32]) {
        assert_eq!(build_precise_filter::<Ipv4>(message_type, allow).into_bytes(), expected);
        assert_eq!(build_precise_filter::<Ipv6>(message_type, allow).into_bytes(), expected);
    }

    #[ip_test]
    fn icmp_filter_allows_type<I: Ip + IcmpIpExt>() {
        for i in 0..u8::MAX {
            match I::IcmpMessageType::try_from(i) {
                // This isn't a valid message type; skip testing it.
                Err(_) => continue,
                Ok(message_type) => {
                    let pass_filter = build_precise_filter::<I>(i, true);
                    let deny_filter = build_precise_filter::<I>(i, false);
                    assert!(pass_filter.allows_type(message_type), "Should allow MessageType:{i}");
                    assert!(!deny_filter.allows_type(message_type), "Should deny MessageType:{i}");
                }
            }
        }
    }
}
