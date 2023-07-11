// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for interacting with the `netlink-packet-*` suite 3p crates.

use std::num::NonZeroI32;

use netlink_packet_core::{
    buffer::NETLINK_HEADER_LEN, constants::NLM_F_MULTIPART, DoneMessage, ErrorMessage,
    NetlinkHeader, NetlinkMessage, NetlinkPayload, NetlinkSerializable,
};
use netlink_packet_utils::Emitable as _;

pub(crate) const UNSPECIFIED_SEQUENCE_NUMBER: u32 = 0;

/// The error code used by `Done` messages.
const DONE_ERROR_CODE: i32 = 0;

/// Returns a `Done` message.
pub(crate) fn new_done<T: NetlinkSerializable>(req_header: NetlinkHeader) -> NetlinkMessage<T> {
    let mut done = DoneMessage::default();
    done.code = DONE_ERROR_CODE;
    let payload = NetlinkPayload::<T>::Done(done);
    let mut resp_header = NetlinkHeader::default();
    resp_header.sequence_number = req_header.sequence_number;
    resp_header.flags |= NLM_F_MULTIPART;
    let mut message = NetlinkMessage::new(resp_header, payload);
    // Sets the header `length` and `message_type` based on the payload.
    message.finalize();
    message
}

pub(crate) mod errno {
    use super::*;

    /// Represents a Netlink Error code.
    ///
    /// Netlink errors are expected to be negative Errnos, with 0 used for ACKs.
    /// This type enforces that the contained code is NonZero & Negative.
    #[derive(Copy, Clone, Debug)]
    pub(crate) struct Errno(i32);

    impl Errno {
        pub(crate) const EADDRNOTAVAIL: Errno =
            const_unwrap::const_unwrap_option(Errno::new(-libc::EADDRNOTAVAIL));
        pub(crate) const EBUSY: Errno = const_unwrap::const_unwrap_option(Errno::new(-libc::EBUSY));
        pub(crate) const EEXIST: Errno =
            const_unwrap::const_unwrap_option(Errno::new(-libc::EEXIST));
        pub(crate) const EINVAL: Errno =
            const_unwrap::const_unwrap_option(Errno::new(-libc::EINVAL));
        pub(crate) const ENODEV: Errno =
            const_unwrap::const_unwrap_option(Errno::new(-libc::ENODEV));
        pub(crate) const ENOTSUP: Errno =
            const_unwrap::const_unwrap_option(Errno::new(-libc::ENOTSUP));

        /// Construct a new [`Errno`] from the given negative integer.
        ///
        /// Returns `None` when the code is non-negative (which includes 0).
        const fn new(code: i32) -> Option<Self> {
            if code.is_negative() {
                Some(Errno(code))
            } else {
                None
            }
        }
    }

    impl From<Errno> for NonZeroI32 {
        fn from(Errno(code): Errno) -> Self {
            NonZeroI32::new(code).expect("Errno's code must be non-zero")
        }
    }

    impl From<Errno> for i32 {
        fn from(Errno(code): Errno) -> Self {
            code
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use test_case::test_case;

        #[test_case(i32::MIN, Some(i32::MIN); "min")]
        #[test_case(-10, Some(-10); "negative")]
        #[test_case(0, None; "zero")]
        #[test_case(10, None; "positive")]
        #[test_case(i32::MAX, None; "max")]
        fn test_new_errno(raw_code: i32, expected_code: Option<i32>) {
            assert_eq!(Errno::new(raw_code).map(Into::<i32>::into), expected_code)
        }
    }
}

/// Returns an `Error` message.
///
/// `Ok(())` represents an ACK while `Err(Errno)` represents a NACK.
pub(crate) fn new_error<T: NetlinkSerializable>(
    error: Result<(), errno::Errno>,
    req_header: NetlinkHeader,
) -> NetlinkMessage<T> {
    let error = {
        assert_eq!(req_header.buffer_len(), NETLINK_HEADER_LEN);
        let mut buffer = vec![0; NETLINK_HEADER_LEN];
        req_header.emit(&mut buffer);

        let code = match error {
            Ok(()) => None,
            Err(e) => Some(e.into()),
        };

        let mut error = ErrorMessage::default();
        error.code = code;
        error.header = buffer;
        error
    };

    let payload = NetlinkPayload::<T>::Error(error);
    // Note that the following header fields are unset as they don't appear to
    // be used by any of our clients: `flags`.
    let mut resp_header = NetlinkHeader::default();
    resp_header.sequence_number = req_header.sequence_number;
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
    use netlink_packet_utils::Parseable as _;
    use test_case::test_case;

    use crate::netlink_packet::errno::Errno;

    #[test_case(0, Ok(()); "ACK")]
    #[test_case(0, Err(Errno::EINVAL); "EINVAL")]
    #[test_case(1, Err(Errno::ENODEV); "ENODEV")]
    fn test_new_error(sequence_number: u32, expected_error: Result<(), Errno>) {
        // Header with arbitrary values
        let mut expected_header = NetlinkHeader::default();
        expected_header.length = 0x01234567;
        expected_header.message_type = 0x89AB;
        expected_header.flags = 0xCDEF;
        expected_header.sequence_number = sequence_number;
        expected_header.port_number = 0x00000000;

        let error = new_error::<RtnlMessage>(expected_error, expected_header);
        // `serialize` will panic if the message is malformed.
        let mut buf = vec![0; error.buffer_len()];
        error.serialize(&mut buf);

        let (header, payload) = error.into_parts();
        assert_eq!(header.message_type, NLMSG_ERROR);
        assert_eq!(header.sequence_number, sequence_number);
        assert_matches!(
            payload,
            NetlinkPayload::Error(ErrorMessage{ code, header, .. }) => {
                let expected_code = match expected_error {
                    Ok(()) => None,
                    Err(e) => Some(e.into()),
                };
                assert_eq!(code, expected_code);
                assert_eq!(
                    NetlinkHeader::parse(&NetlinkBuffer::new(&header)).unwrap(),
                    expected_header,
                );
            }
        );
    }

    #[test_case(0; "seq_0")]
    #[test_case(1; "seq_1")]
    fn test_new_done(sequence_number: u32) {
        let mut req_header = NetlinkHeader::default();
        req_header.sequence_number = sequence_number;

        let done = new_done::<RtnlMessage>(req_header);
        // `serialize` will panic if the message is malformed.
        let mut buf = vec![0; done.buffer_len()];
        done.serialize(&mut buf);

        let (header, payload) = done.into_parts();
        assert_eq!(header.sequence_number, sequence_number);
        assert_eq!(header.message_type, NLMSG_DONE);
        assert_eq!(header.flags, NLM_F_MULTIPART);
        assert_matches!(
            payload,
            NetlinkPayload::Done(DoneMessage {code, extended_ack, ..}) => {
                assert_eq!(code, DONE_ERROR_CODE);
                assert_eq!(extended_ack, vec![]);
            }
        );
    }
}
