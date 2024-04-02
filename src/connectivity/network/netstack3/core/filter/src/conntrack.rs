// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use packet_formats::ip::IpExt;

use crate::{packets::TransportPacket, IpPacket, MaybeTransportPacket};

/// A tuple for a flow in a single direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuple<I: IpExt> {
    protocol: I::Proto,
    src_addr: I::Addr,
    dst_addr: I::Addr,
    src_port_or_id: u16,
    dst_port_or_id: u16,
}

#[allow(dead_code)]
impl<I: IpExt> Tuple<I> {
    /// Creates a `Tuple` from an `IpPacket`, if possible.
    ///
    /// Returns `None` if the packet doesn't have an inner transport packet.
    pub(crate) fn from_packet<'a, P: IpPacket<I>>(packet: &'a P) -> Option<Self> {
        let maybe_transport_packet = packet.transport_packet();
        let transport_packet = maybe_transport_packet.transport_packet()?;

        Some(Self {
            protocol: packet.protocol(),
            src_addr: packet.src_addr(),
            dst_addr: packet.dst_addr(),
            src_port_or_id: transport_packet.src_port(),
            dst_port_or_id: transport_packet.dst_port(),
        })
    }

    /// Returns the inverted version of the tuple.
    ///
    /// This means the src and dst addresses are swapped. For TCP and UDP, the
    /// ports are reversed, but for ICMP, where the ports stand in for other
    /// information, things are more complicated.
    pub(crate) fn invert(self) -> Tuple<I> {
        // TODO(https://fxbug.dev/328064082): Support ICMP properly. The
        // request/response message have different ICMP types.
        Self {
            protocol: self.protocol,
            src_addr: self.dst_addr,
            dst_addr: self.src_addr,
            src_port_or_id: self.dst_port_or_id,
            dst_port_or_id: self.src_port_or_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible as Never;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use packet_formats::ip::IpProto;
    use test_case::test_case;

    use super::*;
    use crate::packets::testutil::internal::{FakeIpPacket, FakeTcpSegment, TransportPacketExt};

    trait TestIpExt: Ip {
        const SRC_ADDR: Self::Addr;
        const SRC_PORT: u16 = 1234;
        const DST_ADDR: Self::Addr;
        const DST_PORT: u16 = 9876;
    }

    impl TestIpExt for Ipv4 {
        const SRC_ADDR: Self::Addr = net_ip_v4!("192.168.1.1");
        const DST_ADDR: Self::Addr = net_ip_v4!("192.168.254.254");
    }

    impl TestIpExt for Ipv6 {
        const SRC_ADDR: Self::Addr = net_ip_v6!("2001:db8::1");
        const DST_ADDR: Self::Addr = net_ip_v6!("2001:db8::ffff");
    }

    struct NoTransportPacket;

    impl MaybeTransportPacket for &NoTransportPacket {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            None
        }
    }

    impl<I: IpExt> TransportPacketExt<I> for &NoTransportPacket {
        fn proto() -> I::Proto {
            I::Proto::from(IpProto::Tcp)
        }
    }

    #[ip_test]
    #[test_case(IpProto::Udp)]
    #[test_case(IpProto::Tcp)]
    fn tuple_invert_udp_tcp<I: Ip + IpExt + TestIpExt>(protocol: IpProto) {
        let orig_tuple = Tuple::<I> {
            protocol: protocol.into(),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };

        let expected = Tuple::<I> {
            protocol: protocol.into(),
            src_addr: I::DST_ADDR,
            dst_addr: I::SRC_ADDR,
            src_port_or_id: I::DST_PORT,
            dst_port_or_id: I::SRC_PORT,
        };

        let inverted = orig_tuple.invert();

        assert_eq!(inverted, expected);
    }

    #[ip_test]
    fn tuple_from_tcp_packet<I: Ip + IpExt + TestIpExt>() {
        let expected = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        };

        let tuple = Tuple::from_packet(&packet).expect("valid TCP packet should return a tuple");
        assert_eq!(tuple, expected);
    }

    #[ip_test]
    fn tuple_from_packet_no_body<I: Ip + IpExt + TestIpExt>() {
        let packet = FakeIpPacket::<I, NoTransportPacket> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: NoTransportPacket {},
        };

        let tuple = Tuple::from_packet(&packet);
        assert_matches!(tuple, None);
    }
}
