// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for interacting with the `netlink-packet-*` suite 3p crates.

use netlink_packet_core::{buffer::NETLINK_HEADER_LEN, ErrorBuffer, ErrorMessage, NetlinkHeader};
use netlink_packet_utils::{Emitable as _, Parseable as _};

/// Returns a newly created [`ErrorMessage`] with the given error code.
// Note this is essentially a constructor for `ErrorMessage`, because the
// `netlink-packet-core` crate does not expose a public method for constructing
// the type. This isn't all that surprising, as we're probably the only user of
// the crate acting as a "server" and typically, clients won't need to construct
// an error. Adding such a constructor upstream may be worth looking into at a
// later date.
// TODO(https://issuetracker.google.com/283136408): Use this to send Acks.
#[allow(dead_code)]
pub(crate) fn new_error_message(code: i32, header: NetlinkHeader) -> ErrorMessage {
    assert_eq!(header.buffer_len(), NETLINK_HEADER_LEN);
    let mut buffer = [0; NETLINK_HEADER_LEN];
    header.emit(&mut buffer);
    let buffer = code.to_ne_bytes().into_iter().chain(buffer.into_iter()).collect::<Vec<_>>();
    ErrorMessage::parse(
        &ErrorBuffer::new_checked(&buffer)
            .expect("buffer should have a valid `ErrorBuffer` format"),
    )
    .expect("buffer should have a valid `ErrorMessage` format")
}

#[cfg(test)]
mod tests {
    use super::*;

    use netlink_packet_core::NetlinkBuffer;

    #[test]
    fn test_new_error_message() {
        // Arbitrary value with different bits in each octet, which ensures the
        // test exercises endianness.
        let expected_code: i32 = 0x7FEDBCA9;
        let mut expected_header = NetlinkHeader::default();
        expected_header.length = 0x01234567;
        expected_header.message_type = 0x89AB;
        expected_header.flags = 0xCDEF;
        expected_header.sequence_number = 0x55555555;
        expected_header.port_number = 0x00000000;

        let ErrorMessage { code, header, .. } = new_error_message(expected_code, expected_header);
        assert_eq!(expected_code, code);
        assert_eq!(expected_header, NetlinkHeader::parse(&NetlinkBuffer::new(&header)).unwrap());
    }
}
