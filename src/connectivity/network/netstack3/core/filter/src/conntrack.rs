// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::sync::Arc;

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

/// The direction of a packet when compared to the given connection.
#[derive(Debug)]
pub(crate) enum ConnectionDirection {
    /// The packet is traveling in the same direction as the first packet seen
    /// for the [`Connection`].
    Original,

    /// The packet is traveling in the opposite direction from the first packet
    /// seen for the [`Connection`].
    Reply,
}

/// A `Connection` contains all of the information about a single connection
/// tracked by conntrack.
#[derive(Debug)]
#[allow(dead_code)]
pub enum Connection<I: IpExt, E> {
    /// A connection that is directly owned by the packet that originated the
    /// connection and no others. All fields are modifiable.
    Exclusive(ConnectionExclusive<I, E>),

    /// This is an existing connection, and there are possibly many other
    /// packets that are concurrently modifying it.
    Shared(Arc<ConnectionShared<I, E>>),
}

#[allow(dead_code)]
impl<I: IpExt, E> Connection<I, E> {
    /// Returns the tuple of the original direction of this connection
    pub(crate) fn original_tuple(&self) -> &Tuple<I> {
        match self {
            Connection::Exclusive(c) => &c.inner.original_tuple,
            Connection::Shared(c) => &c.inner.original_tuple,
        }
    }

    /// Returns the tuple of the reply direction of this connection.
    pub(crate) fn reply_tuple(&self) -> &Tuple<I> {
        match self {
            Connection::Exclusive(c) => &c.inner.reply_tuple,
            Connection::Shared(c) => &c.inner.reply_tuple,
        }
    }

    /// Returns a reference to the [`Connection::external_data`] field.
    pub(crate) fn external_data(&self) -> &E {
        match self {
            Connection::Exclusive(c) => &c.inner.external_data,
            Connection::Shared(c) => &c.inner.external_data,
        }
    }

    /// Returns the direction the tuple represents with respect to the
    /// connection.
    pub(crate) fn direction(&self, tuple: &Tuple<I>) -> Option<ConnectionDirection> {
        let (original, reply) = match self {
            Connection::Exclusive(c) => (&c.inner.original_tuple, &c.inner.reply_tuple),
            Connection::Shared(c) => (&c.inner.original_tuple, &c.inner.reply_tuple),
        };

        if tuple == original {
            Some(ConnectionDirection::Original)
        } else if tuple == reply {
            Some(ConnectionDirection::Reply)
        } else {
            None
        }
    }
}

/// Fields common to both [`ConnectionExclusive`] and [`ConnectionShared`].
#[derive(Debug)]
pub(crate) struct ConnectionCommon<I: IpExt, E> {
    /// The 5-tuple for the connection in the original direction. This is
    /// arbitrary, and is just the direction where a packet was first seen.
    pub(crate) original_tuple: Tuple<I>,

    /// The 5-tuple for the connection in the reply direction. This is what's
    /// used for packet rewriting for NAT.
    pub(crate) reply_tuple: Tuple<I>,

    /// Extra information that is not needed by the conntrack module itself. In
    /// the case of NAT, we expect this to contain things such as the kind of
    /// rewriting that will occur (e.g. SNAT vs DNAT).
    pub(crate) external_data: E,
}

/// A conntrack connection with single ownership.
///
/// Because of this, many fields may be updated without synchronization. There
/// is no chance of messing with other packets for this connection or ending up
/// out-of-sync with the table (e.g. by changing the tuples once the connection
/// has been inserted).
#[derive(Debug)]
pub(crate) struct ConnectionExclusive<I: IpExt, E> {
    pub(crate) inner: ConnectionCommon<I, E>,
}

#[allow(dead_code)]
impl<I: IpExt, E> ConnectionExclusive<I, E> {
    /// Turn this exclusive connection into a shared one. This is required in
    /// order to insert into the [`Table`] table.
    fn make_shared(self) -> Arc<ConnectionShared<I, E>> {
        Arc::new(ConnectionShared { inner: self.inner })
    }
}

impl<I: IpExt, E: Default> From<Tuple<I>> for ConnectionExclusive<I, E> {
    fn from(original_tuple: Tuple<I>) -> Self {
        let reply_tuple = original_tuple.clone().invert();

        Self {
            inner: ConnectionCommon { original_tuple, reply_tuple, external_data: E::default() },
        }
    }
}

/// A conntrack connection with shared ownership.
///
/// All fields are private, because other packets, and the conntrack table
/// itself, will be depending on them not to change. Fields must be accessed
/// through the associated methods.
#[derive(Debug)]
pub(crate) struct ConnectionShared<I: IpExt, E> {
    inner: ConnectionCommon<I, E>,
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

    #[ip_test]
    #[test_case(IpProto::Udp)]
    #[test_case(IpProto::Tcp)]
    fn connection_from_tuple<I: Ip + IpExt + TestIpExt>(protocol: IpProto) {
        let original_tuple = Tuple::<I> {
            protocol: protocol.into(),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let connection: ConnectionExclusive<I, ()> = original_tuple.clone().into();

        assert_eq!(&connection.inner.original_tuple, &original_tuple);
        assert_eq!(&connection.inner.reply_tuple, &reply_tuple);
    }

    #[ip_test]
    fn connection_make_shared_has_same_underlying_info<I: Ip + IpExt + TestIpExt>() {
        let original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let mut connection: ConnectionExclusive<I, u32> = original_tuple.clone().into();
        connection.inner.external_data = 1234;
        let shared = connection.make_shared();

        assert_eq!(shared.inner.original_tuple, original_tuple);
        assert_eq!(shared.inner.reply_tuple, reply_tuple);
        assert_eq!(shared.inner.external_data, 1234);
    }

    enum ConnectionKind {
        Exclusive,
        Shared,
    }

    #[ip_test]
    #[test_case(ConnectionKind::Exclusive)]
    #[test_case(ConnectionKind::Shared)]
    fn connection_getters<I: Ip + IpExt + TestIpExt>(connection_kind: ConnectionKind) {
        let original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let mut connection: ConnectionExclusive<I, u32> = original_tuple.clone().into();
        connection.inner.external_data = 1234;

        let connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_eq!(connection.original_tuple(), &original_tuple);
        assert_eq!(connection.reply_tuple(), &reply_tuple);
        assert_eq!(connection.external_data(), &1234);
    }

    #[ip_test]
    #[test_case(ConnectionKind::Exclusive)]
    #[test_case(ConnectionKind::Shared)]
    fn connection_direction<I: Ip + IpExt + TestIpExt>(connection_kind: ConnectionKind) {
        let original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let mut other_tuple = original_tuple.clone();
        other_tuple.src_port_or_id += 1;

        let connection: ConnectionExclusive<I, ()> = original_tuple.clone().into();
        let connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_matches!(connection.direction(&original_tuple), Some(ConnectionDirection::Original));
        assert_matches!(connection.direction(&reply_tuple), Some(ConnectionDirection::Reply));
        assert_matches!(connection.direction(&other_tuple), None);
    }
}
