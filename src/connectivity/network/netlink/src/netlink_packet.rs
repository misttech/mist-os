// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for interacting with the `netlink-packet-*` suite 3p crates.

use netlink_packet_core::{
    buffer::NETLINK_HEADER_LEN, constants::NLM_F_MULTIPART, ErrorBuffer, ErrorMessage,
    NetlinkHeader, NetlinkMessage, NetlinkPayload, NetlinkSerializable,
};
use netlink_packet_utils::{Emitable as _, Parseable as _};

/// The error code used by `Ack` messages.
const ACK_ERROR_CODE: i32 = 0;

/// Returns a newly created [`ErrorMessage`] with the given error code.
// Note this is essentially a constructor for `ErrorMessage`, because the
// `netlink-packet-core` crate does not expose a public method for constructing
// the type. This isn't all that surprising, as we're probably the only user of
// the crate acting as a "server" and typically, clients won't need to construct
// an error. Adding such a constructor upstream may be worth looking into at a
// later date.
#[allow(dead_code)]
pub fn new_error_message(code: i32, header: NetlinkHeader) -> ErrorMessage {
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

/// Returns an `Ack` message.
pub(crate) fn new_ack<T: NetlinkSerializable>(req_header: NetlinkHeader) -> NetlinkMessage<T> {
    let payload = NetlinkPayload::<T>::Ack(crate::netlink_packet::new_error_message(
        ACK_ERROR_CODE,
        req_header,
    ));
    // Note that the following header fields are unset as they don't appear to
    // be used by any of our clients: `flags`, `sequence_number`, `port_number`.
    let mut message = NetlinkMessage::new(NetlinkHeader::default(), payload);
    // Sets the header `length` and `message_type` based on the payload.
    message.finalize();
    message
}

// Returns a `Done` message.
pub(crate) fn new_done<T: NetlinkSerializable>() -> NetlinkMessage<T> {
    let payload = NetlinkPayload::<T>::Done;
    let mut resp_header = NetlinkHeader::default();
    // Note that the following header fields are unset as they don't appear to
    // be used by any of our clients: `sequence_number`, `port_number`.
    resp_header.flags = NLM_F_MULTIPART;
    let mut message = NetlinkMessage::new(resp_header, payload);
    // Sets the header `length` and `message_type` based on the payload.
    message.finalize();
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use netlink_packet_core::{constants::NLM_F_MULTIPART, NetlinkBuffer, NLMSG_DONE, NLMSG_ERROR};
    use netlink_packet_route::RtnlMessage;

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

    #[test]
    fn test_new_ack() {
        // Header with arbitrary values
        let mut expected_header = NetlinkHeader::default();
        expected_header.length = 0x01234567;
        expected_header.message_type = 0x89AB;
        expected_header.flags = 0xCDEF;
        expected_header.sequence_number = 0x55555555;
        expected_header.port_number = 0x00000000;

        let ack = new_ack::<RtnlMessage>(expected_header);
        // `serialize` will panic if the message is malformed.
        let mut buf = vec![0; ack.buffer_len()];
        ack.serialize(&mut buf);

        let (header, payload) = ack.into_parts();
        assert_eq!(header.message_type, NLMSG_ERROR);
        assert_matches!(
            payload,
            NetlinkPayload::Ack(ErrorMessage{code, header, ..}) if (
                code == ACK_ERROR_CODE &&
                NetlinkHeader::parse(&NetlinkBuffer::new(&header)).unwrap() == expected_header
            )
        );
    }

    #[test]
    fn test_new_done() {
        let done = new_done::<RtnlMessage>();
        // `serialize` will panic if the message is malformed.
        let mut buf = vec![0; done.buffer_len()];
        done.serialize(&mut buf);

        let (header, payload) = done.into_parts();
        assert_eq!(header.message_type, NLMSG_DONE);
        assert_eq!(header.flags, NLM_F_MULTIPART);
        assert_eq!(payload, NetlinkPayload::Done);
    }
}
