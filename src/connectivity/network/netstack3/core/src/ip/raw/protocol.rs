// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Declares types related to the protocols of raw IP sockets.

use net_types::ip::{GenericOverIp, Ip};
use packet_formats::ip::{IpProto, IpProtoExt, Ipv4Proto, Ipv6Proto};

/// A witness type enforcing that the contained protocol is not the
/// "reserved" IANA Internet protocol.
#[derive(Clone, Copy, Debug, GenericOverIp, PartialEq)]
#[generic_over_ip(I, Ip)]
pub struct Protocol<I: IpProtoExt>(I::Proto);

impl<I: IpProtoExt> Protocol<I> {
    fn new(proto: I::Proto) -> Option<Protocol<I>> {
        I::map_ip(
            proto,
            |v4_proto| match v4_proto {
                Ipv4Proto::Proto(IpProto::Reserved) => None,
                _ => Some(Protocol(v4_proto)),
            },
            |v6_proto| match v6_proto {
                Ipv6Proto::Proto(IpProto::Reserved) => None,
                _ => Some(Protocol(v6_proto)),
            },
        )
    }
}

/// The supported protocols of raw IP sockets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RawIpSocketProtocol<I: IpProtoExt> {
    /// Analogous to `IPPROTO_RAW` on Linux.
    ///
    /// This configures the socket as send only (no packets will be received).
    /// When sending the provided message must include the IP header, as when
    /// the `IP_HDRINCL` socket option is set.
    Raw,
    /// An IANA Internet Protocol.
    Proto(Protocol<I>),
}

impl<I: IpProtoExt> RawIpSocketProtocol<I> {
    /// Construct a new [`RawIpSocketProtocol`] from the given IP protocol.
    pub fn new(proto: I::Proto) -> RawIpSocketProtocol<I> {
        Protocol::new(proto).map_or(RawIpSocketProtocol::Raw, RawIpSocketProtocol::Proto)
    }

    /// Extract the plain IP protocol from this [`RawIpSocketProtocol`].
    pub fn proto(&self) -> I::Proto {
        match self {
            RawIpSocketProtocol::Raw => IpProto::Reserved.into(),
            RawIpSocketProtocol::Proto(Protocol(proto)) => *proto,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ipv4, Ipv6};
    use test_case::test_case;

    #[ip_test]
    #[test_case(IpProto::Udp, Some(Protocol(IpProto::Udp.into())); "valid")]
    #[test_case(IpProto::Reserved, None; "reserved")]
    fn new_protocol<I: Ip + IpProtoExt>(p: IpProto, expected: Option<Protocol<I>>) {
        assert_eq!(Protocol::new(p.into()), expected);
    }

    #[ip_test]
    #[test_case(IpProto::Udp, RawIpSocketProtocol::Proto(Protocol(IpProto::Udp.into())); "valid")]
    #[test_case(IpProto::Reserved, RawIpSocketProtocol::Raw; "reserved")]
    fn new_raw_ip_socket_protocol<I: Ip + IpProtoExt>(
        p: IpProto,
        expected: RawIpSocketProtocol<I>,
    ) {
        assert_eq!(RawIpSocketProtocol::new(p.into()), expected);
    }
}
