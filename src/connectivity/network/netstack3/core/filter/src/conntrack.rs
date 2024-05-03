// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{collections::HashMap, sync::Arc};
use core::hash::Hash;

use derivative::Derivative;
use packet_formats::ip::IpExt;

use crate::{
    context::FilterBindingsContext, packets::TransportPacket, FilterBindingsTypes, IpPacket,
    MaybeTransportPacket,
};
use netstack3_base::sync::Mutex;

/// Implements a connection tracking subsystem.
///
/// The `E` parameter is for external data that is stored in the [`Connection`]
/// struct and can be extracted with the [`Connection::external_data()`]
/// function.
#[allow(dead_code)]
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct Table<I: IpExt, BT: FilterBindingsTypes, E> {
    /// A connection is inserted into the map twice: once for the original
    /// tuple, and once for the reply tuple.
    map: Mutex<HashMap<Tuple<I>, Arc<ConnectionShared<I, BT, E>>>>,
}

#[allow(dead_code)]
impl<I: IpExt, BT: FilterBindingsTypes, E> Table<I, BT, E> {
    pub(crate) fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    /// Returns whether the table contains a connection for the specified tuple.
    ///
    /// This is for NAT to determine whether a generated tuple will clash with
    /// one already in the map. While it might seem inefficient, to require
    /// locking in a loop, taking an uncontested lock is going to be
    /// significantly faster than the RNG used to allocate NAT parameters.
    pub(crate) fn contains_tuple(&self, tuple: &Tuple<I>) -> bool {
        self.map.lock().contains_key(tuple)
    }

    /// Attempts to insert the `Connection` into the table.
    ///
    /// To be called once a packet for the connection has passed all filtering.
    /// The boolean return value represents whether the connection was newly
    /// added to the connection tracking state.
    ///
    /// This is on [`Table`] instead of [`Connection`] because conntrack needs
    /// to be able to manipulate its internal map.
    pub(crate) fn finalize_connection(
        &self,
        connection: Connection<I, BT, E>,
    ) -> Result<bool, FinalizeConnectionError> {
        let exclusive = match connection {
            Connection::Exclusive(c) => c,
            // Given that make_shared is private, the only way for us to receive
            // a shared connection is if it was already present in the map. This
            // is far and away the most common case under normal operation.
            Connection::Shared(_) => return Ok(false),
        };

        let mut guard = self.map.lock();

        // The expected case here is that there isn't a conflict.
        //
        // Normally, we'd want to use the entry API to reduce the number of map
        // lookups, but this setup allows us to completely avoid any heap
        // allocations until we're sure that the insertion will succeed. This
        // wastes a little CPU in the common case to avoid pathological behavior
        // in degenerate cases.
        //
        // NOTE: It's theoretically possible for the first two packets (or more)
        // in the same flow to create ExclusiveConnections. In this case,
        // subsequent packets will be reported as conflicts. However, it should
        // be the case that packets for the same flow are handled sequentially,
        // so each subsequent packet should see the connection created by the
        // first one.
        if guard.contains_key(&exclusive.inner.original_tuple)
            || guard.contains_key(&exclusive.inner.reply_tuple)
        {
            Err(FinalizeConnectionError::Conflict)
        } else {
            let shared = exclusive.make_shared();

            let res = guard.insert(shared.inner.original_tuple.clone(), shared.clone());
            debug_assert!(res.is_none());

            let res = guard.insert(shared.inner.reply_tuple.clone(), shared);
            debug_assert!(res.is_none());

            Ok(true)
        }
    }
}

#[allow(dead_code)]
impl<I: IpExt, BC: FilterBindingsContext, E: Default> Table<I, BC, E> {
    /// Returns a [`Connection`] for the packet's flow. If a connection does not
    /// currently exist, a new one is created.
    ///
    /// At the same time, process the packet for the connection, updating
    /// internal connection state.
    ///
    /// After processing is complete, you must call
    /// [`finalize_connection`](Table::finalize_connection) with this
    /// connection.
    pub(crate) fn get_connection_for_packet_and_update<P: IpPacket<I>>(
        &self,
        bindings_ctx: &BC,
        packet: &P,
    ) -> Option<Connection<I, BC, E>> {
        let tuple = Tuple::from_packet(packet)?;

        let mut connection = match self.map.lock().get(&tuple) {
            Some(connection) => Connection::Shared(connection.clone()),
            None => {
                Connection::Exclusive(ConnectionExclusive::from_tuple(bindings_ctx, tuple.clone()))
            }
        };

        match connection.update(bindings_ctx, &tuple) {
            Ok(()) => Some(connection),
            Err(e) => match e {
                ConnectionUpdateError::NonMatchingTuple => {
                    panic!(
                        "Tuple didn't match. tuple={:?}, conn.original={:?}, conn.reply={:?}",
                        &tuple,
                        connection.original_tuple(),
                        connection.reply_tuple()
                    );
                }
            },
        }
    }
}

/// A tuple for a flow in a single direction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// An error returned from [`Table::finalize_connection`].
#[derive(Debug)]
pub(crate) enum FinalizeConnectionError {
    /// There is a conflicting connection already tracked by conntrack. The
    /// to-be-finalized connection was not inserted into the table.
    Conflict,
}

/// An error returned from [`Connection::update`].
#[derive(Debug)]
pub(crate) enum ConnectionUpdateError {
    /// The provided tuple doesn't belong to the connection being updated.
    NonMatchingTuple,
}

/// A `Connection` contains all of the information about a single connection
/// tracked by conntrack.
#[derive(Debug)]
#[allow(dead_code)]
pub enum Connection<I: IpExt, BT: FilterBindingsTypes, E> {
    /// A connection that is directly owned by the packet that originated the
    /// connection and no others. All fields are modifiable.
    Exclusive(ConnectionExclusive<I, BT, E>),

    /// This is an existing connection, and there are possibly many other
    /// packets that are concurrently modifying it.
    Shared(Arc<ConnectionShared<I, BT, E>>),
}

#[allow(dead_code)]
impl<I: IpExt, BT: FilterBindingsTypes, E> Connection<I, BT, E> {
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

    /// Returns a copy of the internal connection state
    pub(crate) fn state(&self) -> ConnectionState<BT> {
        match self {
            Connection::Exclusive(c) => c.state.clone(),
            Connection::Shared(c) => c.state.lock().clone(),
        }
    }
}

impl<I: IpExt, BC: FilterBindingsContext, E> Connection<I, BC, E> {
    fn update(&mut self, bindings_ctx: &BC, tuple: &Tuple<I>) -> Result<(), ConnectionUpdateError> {
        let direction = match self.direction(tuple) {
            Some(d) => d,
            None => return Err(ConnectionUpdateError::NonMatchingTuple),
        };

        let now = bindings_ctx.now();

        match self {
            Connection::Exclusive(c) => c.state.update(direction, now),
            Connection::Shared(c) => c.state.lock().update(direction, now),
        }
    }
}

/// Fields common to both [`ConnectionExclusive`] and [`ConnectionShared`].
#[derive(Debug)]
pub struct ConnectionCommon<I: IpExt, E> {
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

/// Dynamic per-connection state.
#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""))]
pub(crate) struct ConnectionState<BT: FilterBindingsTypes> {
    /// Whether this connection has seen packets in both directions.
    established: bool,

    /// The time the last packet was seen for this connection (in either of the
    /// original or reply directions).
    last_packet_time: BT::Instant,
}

impl<BT: FilterBindingsTypes> ConnectionState<BT> {
    fn update(
        &mut self,
        dir: ConnectionDirection,
        now: BT::Instant,
    ) -> Result<(), ConnectionUpdateError> {
        match dir {
            ConnectionDirection::Original => (),
            ConnectionDirection::Reply => self.established = true,
        };

        if self.last_packet_time < now {
            self.last_packet_time = now;
        }

        Ok(())
    }
}

/// A conntrack connection with single ownership.
///
/// Because of this, many fields may be updated without synchronization. There
/// is no chance of messing with other packets for this connection or ending up
/// out-of-sync with the table (e.g. by changing the tuples once the connection
/// has been inserted).
#[derive(Debug)]
pub struct ConnectionExclusive<I: IpExt, BT: FilterBindingsTypes, E> {
    pub(crate) inner: ConnectionCommon<I, E>,
    pub(crate) state: ConnectionState<BT>,
}

#[allow(dead_code)]
impl<I: IpExt, BT: FilterBindingsTypes, E> ConnectionExclusive<I, BT, E> {
    /// Turn this exclusive connection into a shared one. This is required in
    /// order to insert into the [`Table`] table.
    fn make_shared(self) -> Arc<ConnectionShared<I, BT, E>> {
        Arc::new(ConnectionShared { inner: self.inner, state: Mutex::new(self.state) })
    }
}

impl<I: IpExt, BC: FilterBindingsContext, E: Default> ConnectionExclusive<I, BC, E> {
    fn from_tuple(bindings_ctx: &BC, original_tuple: Tuple<I>) -> Self {
        let reply_tuple = original_tuple.clone().invert();

        Self {
            inner: ConnectionCommon { original_tuple, reply_tuple, external_data: E::default() },
            state: ConnectionState { established: false, last_packet_time: bindings_ctx.now() },
        }
    }
}

/// A conntrack connection with shared ownership.
///
/// All fields are private, because other packets, and the conntrack table
/// itself, will be depending on them not to change. Fields must be accessed
/// through the associated methods.
#[derive(Debug)]
pub struct ConnectionShared<I: IpExt, BT: FilterBindingsTypes, E> {
    inner: ConnectionCommon<I, E>,
    state: Mutex<ConnectionState<BT>>,
}

#[cfg(test)]
mod tests {
    use core::{convert::Infallible as Never, time::Duration};

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use packet_formats::ip::IpProto;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::FakeBindingsCtx,
        packets::testutil::internal::{FakeIpPacket, FakeTcpSegment, TransportPacketExt},
    };

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
        let bindings_ctx = FakeBindingsCtx::new();

        let original_tuple = Tuple::<I> {
            protocol: protocol.into(),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let connection =
            ConnectionExclusive::<_, _, ()>::from_tuple(&bindings_ctx, original_tuple.clone());

        assert_eq!(&connection.inner.original_tuple, &original_tuple);
        assert_eq!(&connection.inner.reply_tuple, &reply_tuple);
    }

    #[ip_test]
    fn connection_make_shared_has_same_underlying_info<I: Ip + IpExt + TestIpExt>() {
        let bindings_ctx = FakeBindingsCtx::new();

        let original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let mut connection = ConnectionExclusive::from_tuple(&bindings_ctx, original_tuple.clone());
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
        let bindings_ctx = FakeBindingsCtx::new();

        let original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };
        let reply_tuple = original_tuple.clone().invert();

        let mut connection = ConnectionExclusive::from_tuple(&bindings_ctx, original_tuple.clone());
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
        let bindings_ctx = FakeBindingsCtx::new();

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

        let connection =
            ConnectionExclusive::<_, _, ()>::from_tuple(&bindings_ctx, original_tuple.clone());
        let connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_matches!(connection.direction(&original_tuple), Some(ConnectionDirection::Original));
        assert_matches!(connection.direction(&reply_tuple), Some(ConnectionDirection::Reply));
        assert_matches!(connection.direction(&other_tuple), None);
    }

    #[ip_test]
    #[test_case(ConnectionKind::Exclusive)]
    #[test_case(ConnectionKind::Shared)]
    fn connection_update<I: Ip + IpExt + TestIpExt>(connection_kind: ConnectionKind) {
        let mut bindings_ctx = FakeBindingsCtx::new();
        bindings_ctx.set_time_elapsed(Duration::from_secs(12));

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

        let connection =
            ConnectionExclusive::<_, _, ()>::from_tuple(&bindings_ctx, original_tuple.clone());
        let mut connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_matches!(connection.update(&bindings_ctx, &original_tuple), Ok(()));
        let state = connection.state();
        assert!(!state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(12));

        // Tuple in reply direction should set established to true and obviously
        // update last packet time.
        bindings_ctx.set_time_elapsed(Duration::from_secs(13));
        assert_matches!(connection.update(&bindings_ctx, &reply_tuple), Ok(()));
        let state = connection.state();
        assert!(state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(13));

        // Unrelated connection should return an error and otherwise not touch
        // anything.
        bindings_ctx.set_time_elapsed(Duration::from_secs(14));
        assert_matches!(
            connection.update(&bindings_ctx, &other_tuple),
            Err(ConnectionUpdateError::NonMatchingTuple)
        );
        assert!(state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(13));
    }

    #[ip_test]
    fn table_get_exclusive_connection_and_finalize_shared<I: Ip + IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        bindings_ctx.set_time_elapsed(Duration::from_secs(12));
        let table = Table::<_, _, ()>::new();

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        };

        let reply_packet = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeTcpSegment { src_port: I::DST_PORT, dst_port: I::SRC_PORT },
        };

        let original_tuple = Tuple::from_packet(&packet).expect("packet should be valid");
        let reply_tuple = Tuple::from_packet(&reply_packet).expect("packet should be valid");

        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid");
        let state = conn.state();
        assert!(!state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(12));

        // Since the connection isn't present in the map, we should get a
        // freshly-allocated exclusive connection and the map should not have
        // been touched.
        assert_matches!(conn, Connection::Exclusive(_));
        assert!(!table.contains_tuple(&original_tuple));
        assert!(!table.contains_tuple(&reply_tuple));

        // Once we finalize the connection, it should be present in the map.
        assert_matches!(table.finalize_connection(conn), Ok(true));
        assert!(table.contains_tuple(&original_tuple));
        assert!(table.contains_tuple(&reply_tuple));

        // We should now get a shared connection back for packets in either
        // direction now that the connection is present in the table.
        bindings_ctx.set_time_elapsed(Duration::from_secs(13));
        let conn = table.get_connection_for_packet_and_update(&bindings_ctx, &packet);
        let conn = assert_matches!(conn, Some(conn) => conn);
        let state = conn.state();
        assert!(!state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(13));
        let conn = assert_matches!(conn, Connection::Shared(conn) => conn);

        bindings_ctx.set_time_elapsed(Duration::from_secs(14));
        let reply_conn = table.get_connection_for_packet_and_update(&bindings_ctx, &reply_packet);
        let reply_conn = assert_matches!(reply_conn, Some(conn) => conn);
        let state = reply_conn.state();
        assert!(state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(14));
        let reply_conn = assert_matches!(reply_conn, Connection::Shared(conn) => conn);

        // We should be getting the same connection in both directions.
        assert!(Arc::ptr_eq(&conn, &reply_conn));

        // Inserting the connection a second time shouldn't change the map.
        let conn = table.get_connection_for_packet_and_update(&bindings_ctx, &packet).unwrap();
        assert_matches!(table.finalize_connection(conn), Ok(false));
        assert!(table.contains_tuple(&original_tuple));
        assert!(table.contains_tuple(&reply_tuple));
    }

    #[ip_test]
    fn table_conflict<I: Ip + IpExt + TestIpExt>() {
        let bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new();

        let original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };

        let nated_original_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT + 1,
            dst_port_or_id: I::DST_PORT + 1,
        };

        let nated_reply_tuple = Tuple::<I> {
            protocol: I::Proto::from(IpProto::Tcp),
            src_addr: I::DST_ADDR,
            dst_addr: I::SRC_ADDR,
            src_port_or_id: I::DST_PORT + 1,
            dst_port_or_id: I::SRC_PORT + 1,
        };

        let conn1 = Connection::Exclusive(ConnectionExclusive::<_, _, ()>::from_tuple(
            &bindings_ctx,
            original_tuple.clone(),
        ));

        // Fake NAT that ends up allocating the same reply tuple as an existing
        // connection.
        let mut conn2 =
            ConnectionExclusive::<_, _, ()>::from_tuple(&bindings_ctx, original_tuple.clone());
        conn2.inner.original_tuple = nated_original_tuple.clone();
        let conn2 = Connection::Exclusive(conn2);

        // Fake NAT that ends up allocating the same original tuple as an
        // existing connection.
        let mut conn3 =
            ConnectionExclusive::<_, _, ()>::from_tuple(&bindings_ctx, original_tuple.clone());
        conn3.inner.reply_tuple = nated_reply_tuple.clone();
        let conn3 = Connection::Exclusive(conn3);

        assert_matches!(table.finalize_connection(conn1), Ok(true));
        assert_matches!(table.finalize_connection(conn2), Err(FinalizeConnectionError::Conflict));
        assert_matches!(table.finalize_connection(conn3), Err(FinalizeConnectionError::Conflict));
    }
}
