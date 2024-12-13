// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for interacting with the `netlink-packet-*` suite 3p crates.

use std::num::NonZeroI32;

use net_types::ip::{Ip, Ipv4Addr, Ipv6Addr};
use netlink_packet_core::buffer::NETLINK_HEADER_LEN;
use netlink_packet_core::constants::NLM_F_MULTIPART;
use netlink_packet_core::{
    DoneMessage, ErrorMessage, NetlinkHeader, NetlinkMessage, NetlinkPayload, NetlinkSerializable,
};
use netlink_packet_route::route::RouteAddress;
use netlink_packet_utils::Emitable as _;
use tracing::warn;

use crate::netlink_packet::errno::Errno;

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

/// Produces an `I::Addr` from the given `RouteAddress`
pub(crate) fn ip_addr_from_route<I: Ip>(route_addr: &RouteAddress) -> Result<I::Addr, Errno> {
    I::map_ip(
        (),
        |()| match route_addr {
            RouteAddress::Inet(v4_addr) => Ok(Ipv4Addr::new(v4_addr.octets())),
            RouteAddress::Inet6(_) => {
                warn!("expected IPv4 address from route but got an IPv6 address");
                Err(Errno::EINVAL)
            }
            RouteAddress::Mpls(_) | RouteAddress::Other(_) | _ => Err(Errno::ENOTSUP),
        },
        |()| match route_addr {
            RouteAddress::Inet6(v6_addr) => Ok(Ipv6Addr::new(v6_addr.segments())),
            RouteAddress::Inet(_) => {
                warn!("expected IPv6 address from route but got an IPv4 address");
                Err(Errno::EINVAL)
            }
            RouteAddress::Mpls(_) | RouteAddress::Other(_) | _ => Err(Errno::ENOTSUP),
        },
    )
}

pub(crate) mod errno {
    use net_types::ip::GenericOverIp;

    use super::*;

    /// Represents a Netlink Error code.
    ///
    /// Netlink errors are expected to be negative Errnos, with 0 used for ACKs.
    /// This type enforces that the contained code is NonZero & Negative.
    #[derive(Copy, Clone, Debug, PartialEq, GenericOverIp)]
    #[generic_over_ip()]
    pub(crate) struct Errno(i32);

    impl Errno {
        pub(crate) const EADDRNOTAVAIL: Errno = Errno::new(-libc::EADDRNOTAVAIL).unwrap();
        pub(crate) const EAFNOSUPPORT: Errno = Errno::new(-libc::EAFNOSUPPORT).unwrap();
        pub(crate) const EBUSY: Errno = Errno::new(-libc::EBUSY).unwrap();
        pub(crate) const EEXIST: Errno = Errno::new(-libc::EEXIST).unwrap();
        pub(crate) const EINVAL: Errno = Errno::new(-libc::EINVAL).unwrap();
        pub(crate) const ENODEV: Errno = Errno::new(-libc::ENODEV).unwrap();
        pub(crate) const ENOENT: Errno = Errno::new(-libc::ENOENT).unwrap();
        pub(crate) const ENOTSUP: Errno = Errno::new(-libc::ENOTSUP).unwrap();
        pub(crate) const ESRCH: Errno = Errno::new(-libc::ESRCH).unwrap();
        pub(crate) const ETOOMANYREFS: Errno = Errno::new(-libc::ETOOMANYREFS).unwrap();

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
    use netlink_packet_core::{NetlinkBuffer, NLMSG_DONE, NLMSG_ERROR};
    use netlink_packet_route::RouteNetlinkMessage;
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

        let error = new_error::<RouteNetlinkMessage>(expected_error, expected_header);
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

        let done = new_done::<RouteNetlinkMessage>(req_header);
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
