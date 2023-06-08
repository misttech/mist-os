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

/// Allows conversion into an error code suitable for a (N)Ack message.
pub(crate) trait IntoAckErrorCode {
    /// Converts `self` into a value suitable for a (N)Ack message's error code
    /// field.
    fn into_code(self) -> i32;
}

/// An implementation of [`IntoAckErrorCode`] that always returns an error code
/// indicating success.
#[derive(Copy, Clone, Debug)]
pub(crate) struct AckErrorCode;

impl IntoAckErrorCode for AckErrorCode {
    fn into_code(self) -> i32 {
        ACK_ERROR_CODE
    }
}

/// An implementation of [`IntoAckErrorCode`] that always returns an error code
/// indicating failure.
///
/// Essentially a non-zero `i32` type.
#[derive(Copy, Clone, Debug)]
pub(crate) struct NackErrorCode(i32);

impl NackErrorCode {
    pub(crate) const INVALID: Self = const_unwrap::const_unwrap_option(Self::new(libc::EINVAL));

    const fn new(code: i32) -> Option<Self> {
        if code == ACK_ERROR_CODE {
            None
        } else {
            Some(Self(code))
        }
    }
}

impl IntoAckErrorCode for NackErrorCode {
    fn into_code(self) -> i32 {
        let Self(code) = self;
        assert_ne!(code, ACK_ERROR_CODE);
        code
    }
}

impl<E: IntoAckErrorCode> IntoAckErrorCode for Result<(), E> {
    fn into_code(self) -> i32 {
        match self {
            Ok(()) => ACK_ERROR_CODE,
            Err(e) => e.into_code(),
        }
    }
}

/// Returns an `Ack` message.
pub(crate) fn new_ack<T: NetlinkSerializable>(
    code: impl IntoAckErrorCode,
    req_header: NetlinkHeader,
) -> NetlinkMessage<T> {
    let payload = NetlinkPayload::<T>::Ack(crate::netlink_packet::new_error_message(
        code.into_code(),
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
    use test_case::test_case;

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

    struct TestAckCode(i32);

    impl IntoAckErrorCode for TestAckCode {
        fn into_code(self) -> i32 {
            let Self(code) = self;
            code
        }
    }

    #[test_case(0, None; "0")]
    #[test_case(1, Some(1); "1")]
    #[test_case(i32::MAX, Some(i32::MAX); "max")]
    #[test_case(i32::MIN, Some(i32::MIN); "min")]
    fn test_nack_error_code(code: i32, expected_code: Option<i32>) {
        assert_eq!(NackErrorCode::new(code).map(IntoAckErrorCode::into_code), expected_code);
    }

    #[test_case(AckErrorCode, 0; "ack_error_code")]
    #[test_case(NackErrorCode::new(1).unwrap(), 1; "nack_error_code_1")]
    #[test_case(NackErrorCode::new(i32::MAX).unwrap(), i32::MAX; "nack_error_code_max")]
    #[test_case(NackErrorCode::new(i32::MIN).unwrap(), i32::MIN; "nack_error_code_min")]
    #[test_case(TestAckCode(0), 0; "test_ack_code_0")]
    #[test_case(TestAckCode(1), 1; "test_ack_code_1")]
    #[test_case(TestAckCode(i32::MAX), i32::MAX; "test_ack_code_i32_max")]
    #[test_case(TestAckCode(i32::MIN), i32::MIN; "test_ack_code_i32_min")]
    fn test_new_ack(code: impl IntoAckErrorCode, expected_code: i32) {
        // Header with arbitrary values
        let mut expected_header = NetlinkHeader::default();
        expected_header.length = 0x01234567;
        expected_header.message_type = 0x89AB;
        expected_header.flags = 0xCDEF;
        expected_header.sequence_number = 0x55555555;
        expected_header.port_number = 0x00000000;

        let ack = new_ack::<RtnlMessage>(code, expected_header);
        // `serialize` will panic if the message is malformed.
        let mut buf = vec![0; ack.buffer_len()];
        ack.serialize(&mut buf);

        let (header, payload) = ack.into_parts();
        assert_eq!(header.message_type, NLMSG_ERROR);
        assert_matches!(
            payload,
            NetlinkPayload::Ack(ErrorMessage{ code, header, .. }) => {
                assert_eq!(code, expected_code);
                assert_eq!(
                    NetlinkHeader::parse(&NetlinkBuffer::new(&header)).unwrap(),
                    expected_header,
                );
            }
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
