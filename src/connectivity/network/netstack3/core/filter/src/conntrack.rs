// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{collections::HashMap, fmt::Debug, sync::Arc, vec::Vec};
use core::{hash::Hash, time::Duration};

use derivative::Derivative;
use net_types::ip::IpVersionMarker;
use packet_formats::ip::IpExt;

use crate::{
    context::FilterBindingsContext, logic::FilterTimerId, packets::TransportPacket,
    FilterBindingsTypes, IpPacket, MaybeTransportPacket,
};
use netstack3_base::{
    sync::Mutex, CoreTimerContext, Inspectable, Inspector, Instant, TimerContext,
};

/// The time from the end of one GC cycle to the beginning of the next.
const GC_INTERVAL: Duration = Duration::from_secs(10);

/// The time since the last seen packet after which an established connection
/// (one that has seen traffic in both directions) is considered expired and is
/// eligible for garbage collection.
///
/// Until we have TCP tracking, this is a large value to ensure that connections
/// that are still valid aren't cleaned up prematurely.
const CONNECTION_EXPIRY_TIME_ESTABLISHED: Duration = Duration::from_secs(12 * 60 * 60);

/// The time since the last seen packet after which an unestablished connection
/// (one that has only seen traffic in one direction) is considered expired and
/// is eligible for garbage collection.
///
/// This is lower than the one for established connections as an optimization to
/// prune unused connections from the conntrack table. We expect that
/// connections establish very quickly (e.g. a handful of milliseconds for a TCP
/// handshake or DNS query).
const CONNECTION_EXPIRY_TIME_UNESTABLISHED: Duration = Duration::from_secs(60);

/// The maximum number of connections in the conntrack table.
const MAXIMUM_CONNECTIONS: usize = 10_000;

/// The maximum size of the conntrack table. We double the table size limit
/// because each connection is inserted into the table twice, once for the
/// original tuple and again for the reply tuple.
const TABLE_SIZE_LIMIT: usize = MAXIMUM_CONNECTIONS * 2;

/// Implements a connection tracking subsystem.
///
/// The `E` parameter is for external data that is stored in the [`Connection`]
/// struct and can be extracted with the [`Connection::external_data()`]
/// function.
pub struct Table<I: IpExt, BT: FilterBindingsTypes, E> {
    inner: Mutex<TableInner<I, BT, E>>,
}

struct TableInner<I: IpExt, BT: FilterBindingsTypes, E> {
    /// A connection is inserted into the map twice: once for the original
    /// tuple, and once for the reply tuple.
    table: HashMap<Tuple<I>, Arc<ConnectionShared<I, BT, E>>>,
    /// A timer for triggering garbage collection events.
    gc_timer: BT::Timer,
    /// The number of times the table size limit was hit.
    table_limit_hits: u32,
    /// Of the times the table limit was hit, the number of times we had to drop
    /// a packet because we couldn't make space in the table.
    table_limit_drops: u32,
}

#[allow(dead_code)]
impl<I: IpExt, BT: FilterBindingsTypes, E> Table<I, BT, E> {
    /// Returns whether the table contains a connection for the specified tuple.
    ///
    /// This is for NAT to determine whether a generated tuple will clash with
    /// one already in the map. While it might seem inefficient, to require
    /// locking in a loop, taking an uncontested lock is going to be
    /// significantly faster than the RNG used to allocate NAT parameters.
    pub(crate) fn contains_tuple(&self, tuple: &Tuple<I>) -> bool {
        self.inner.lock().table.contains_key(tuple)
    }
}

fn schedule_gc<BC>(bindings_ctx: &mut BC, timer: &mut BC::Timer)
where
    BC: TimerContext,
{
    let _ = bindings_ctx.schedule_timer(GC_INTERVAL, timer);
}

impl<I: IpExt, BC: FilterBindingsContext, E: Default> Table<I, BC, E> {
    pub(crate) fn new<CC: CoreTimerContext<FilterTimerId<I>, BC>>(bindings_ctx: &mut BC) -> Self {
        Self {
            inner: Mutex::new(TableInner {
                table: HashMap::new(),
                gc_timer: CC::new_timer(
                    bindings_ctx,
                    FilterTimerId::ConntrackGc(IpVersionMarker::<I>::new()),
                ),
                table_limit_hits: 0,
                table_limit_drops: 0,
            }),
        }
    }

    /// Attempts to insert the `Connection` into the table.
    ///
    /// To be called once a packet for the connection has passed all filtering.
    /// The boolean return value represents whether the connection was newly
    /// added to the connection tracking state.
    ///
    /// This is on [`Table`] instead of [`Connection`] because conntrack needs
    /// to be able to manipulate its internal map.
    #[allow(dead_code)]
    pub(crate) fn finalize_connection(
        &self,
        bindings_ctx: &mut BC,
        connection: Connection<I, BC, E>,
    ) -> Result<bool, FinalizeConnectionError> {
        let exclusive = match connection {
            Connection::Exclusive(c) => c,
            // Given that make_shared is private, the only way for us to receive
            // a shared connection is if it was already present in the map. This
            // is far and away the most common case under normal operation.
            Connection::Shared(_) => return Ok(false),
        };

        let mut guard = self.inner.lock();

        // We multiply the table size limit because each connection is inserted
        // into the table twice, once for the original tuple and again for the
        // reply tuple.
        if guard.table.len() >= TABLE_SIZE_LIMIT {
            guard.table_limit_hits = guard.table_limit_hits.saturating_add(1);
            if let Some((original_tuple, reply_tuple)) = guard
                .table
                .iter()
                .filter_map(|(_, conn)| {
                    if conn.state.lock().established {
                        None
                    } else {
                        Some((conn.inner.original_tuple.clone(), conn.inner.reply_tuple.clone()))
                    }
                })
                .next()
            {
                assert!(guard.table.remove(&original_tuple).is_some());
                assert!(guard.table.remove(&reply_tuple).is_some());
            } else {
                guard.table_limit_drops = guard.table_limit_drops.saturating_add(1);
                return Err(FinalizeConnectionError::TableFull);
            }
        }

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
        if guard.table.contains_key(&exclusive.inner.original_tuple)
            || guard.table.contains_key(&exclusive.inner.reply_tuple)
        {
            Err(FinalizeConnectionError::Conflict)
        } else {
            let shared = exclusive.make_shared();

            let res = guard.table.insert(shared.inner.original_tuple.clone(), shared.clone());
            debug_assert!(res.is_none());

            let res = guard.table.insert(shared.inner.reply_tuple.clone(), shared);
            debug_assert!(res.is_none());

            // For the most part, this will only schedule the timer once, when
            // the first packet hits the netstack. However, since the GC timer
            // is only rescheduled during GC when the table has entries, it's
            // possible that this will be called again if the table ever becomes
            // empty.
            if bindings_ctx.scheduled_instant(&mut guard.gc_timer).is_none() {
                schedule_gc(bindings_ctx, &mut guard.gc_timer);
            }

            Ok(true)
        }
    }

    /// Returns a [`Connection`] for the packet's flow. If a connection does not
    /// currently exist, a new one is created.
    ///
    /// At the same time, process the packet for the connection, updating
    /// internal connection state.
    ///
    /// After processing is complete, you must call
    /// [`finalize_connection`](Table::finalize_connection) with this
    /// connection.
    #[allow(dead_code)]
    pub(crate) fn get_connection_for_packet_and_update<P: IpPacket<I>>(
        &self,
        bindings_ctx: &BC,
        packet: &P,
    ) -> Option<Connection<I, BC, E>> {
        let tuple = Tuple::from_packet(packet)?;

        let mut connection = match self.inner.lock().table.get(&tuple) {
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

    pub(crate) fn perform_gc(&self, bindings_ctx: &mut BC) {
        let now = bindings_ctx.now();
        let mut guard = self.inner.lock();

        // Sadly, we can't easily remove entries from the map in-place for two
        // reasons:
        // - HashMap::retain() will look at each connection twice, since it will
        // be inserted under both tuples. If a packet updates last_packet_time
        // between these two checks, we might remove one tuple of the connection
        // but not the other, leaving a single tuple in the table, which breaks
        // a core invariant.
        // - You can't modify a std::HashMap while iterating over it.
        let to_remove: Vec<_> = guard
            .table
            .iter()
            .filter_map(|(tuple, conn)| {
                if *tuple == conn.inner.original_tuple && conn.is_expired(now) {
                    Some((conn.inner.original_tuple.clone(), conn.inner.reply_tuple.clone()))
                } else {
                    None
                }
            })
            .collect();

        for (original_tuple, reply_tuple) in to_remove {
            assert!(guard.table.remove(&original_tuple).is_some());
            assert!(guard.table.remove(&reply_tuple).is_some());
        }

        // The table is only expected to be empty in exceptional cases, or
        // during tests. The test case especially important, because some tests
        // will wait for core to quiesce by waiting for timers to stop firing.
        // By only rescheduling when there are still entries in the table, we
        // ensure that we won't enter an infinite timer firing/scheduling loop.
        if !guard.table.is_empty() {
            schedule_gc(bindings_ctx, &mut guard.gc_timer);
        }
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E: Inspectable> Inspectable for Table<I, BT, E> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let guard = self.inner.lock();

        inspector.record_usize("num_connections", guard.table.len() / 2);
        inspector.record_uint("table_limit_hits", guard.table_limit_hits);
        inspector.record_uint("table_limit_drops", guard.table_limit_drops);

        inspector.record_child("connections", |inspector| {
            guard
                .table
                .iter()
                .filter_map(|(tuple, connection)| {
                    if *tuple == connection.inner.original_tuple {
                        Some(connection)
                    } else {
                        None
                    }
                })
                .for_each(|connection| {
                    inspector.record_unnamed_child(|inspector| {
                        inspector.delegate_inspectable(connection.as_ref())
                    });
                });
        });
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

impl<I: IpExt> Inspectable for Tuple<I> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        inspector.record_debug("protocol", self.protocol);
        inspector.record_ip_addr("src_addr", self.src_addr);
        inspector.record_ip_addr("dst_addr", self.dst_addr);
        inspector.record_usize("src_port_or_id", self.src_port_or_id);
        inspector.record_usize("dst_port_or_id", self.dst_port_or_id);
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

    /// The table has reached the hard size cap and no room could be made.
    TableFull,
}

/// An error returned from [`Connection::update`].
#[derive(Debug)]
pub(crate) enum ConnectionUpdateError {
    /// The provided tuple doesn't belong to the connection being updated.
    NonMatchingTuple,
}

/// A `Connection` contains all of the information about a single connection
/// tracked by conntrack.
#[derive(Derivative)]
#[derivative(Debug(bound = "E: Debug"))]
pub enum Connection<I: IpExt, BT: FilterBindingsTypes, E> {
    /// A connection that is directly owned by the packet that originated the
    /// connection and no others. All fields are modifiable.
    Exclusive(ConnectionExclusive<I, BT, E>),

    /// This is an existing connection, and there are possibly many other
    /// packets that are concurrently modifying it.
    Shared(Arc<ConnectionShared<I, BT, E>>),
}

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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
#[derive(Derivative)]
#[derivative(Debug(bound = "E: Debug"))]
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

impl<I: IpExt, E: Inspectable> Inspectable for ConnectionCommon<I, E> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        inspector.record_child("original_tuple", |inspector| {
            inspector.delegate_inspectable(&self.original_tuple);
        });

        inspector.record_child("reply_tuple", |inspector| {
            inspector.delegate_inspectable(&self.reply_tuple);
        });

        // We record external_data as an inspectable because that allows us to
        // prevent accidentally leaking data, which could happen if we just used
        // the Debug impl.
        inspector.record_child("external_data", |inspector| {
            inspector.delegate_inspectable(&self.external_data);
        });
    }
}

/// Dynamic per-connection state.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Debug(bound = ""))]
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

impl<BT: FilterBindingsTypes> Inspectable for ConnectionState<BT> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        inspector.record_bool("established", self.established);
        inspector.record_inspectable_value("last_packet_time", &self.last_packet_time);
    }
}

/// A conntrack connection with single ownership.
///
/// Because of this, many fields may be updated without synchronization. There
/// is no chance of messing with other packets for this connection or ending up
/// out-of-sync with the table (e.g. by changing the tuples once the connection
/// has been inserted).
#[derive(Derivative)]
#[derivative(Debug(bound = "E: Debug"))]
pub struct ConnectionExclusive<I: IpExt, BT: FilterBindingsTypes, E> {
    pub(crate) inner: ConnectionCommon<I, E>,
    pub(crate) state: ConnectionState<BT>,
}

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
#[derive(Derivative)]
#[derivative(Debug(bound = "E: Debug"))]
pub struct ConnectionShared<I: IpExt, BT: FilterBindingsTypes, E> {
    inner: ConnectionCommon<I, E>,
    state: Mutex<ConnectionState<BT>>,
}

impl<I: IpExt, BT: FilterBindingsTypes, E> ConnectionShared<I, BT, E> {
    fn is_expired(&self, now: BT::Instant) -> bool {
        let state = self.state.lock();

        let duration = now.duration_since(state.last_packet_time);

        if state.established {
            duration >= CONNECTION_EXPIRY_TIME_ESTABLISHED
        } else {
            duration >= CONNECTION_EXPIRY_TIME_UNESTABLISHED
        }
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E: Inspectable> Inspectable for ConnectionShared<I, BT, E> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        inspector.delegate_inspectable(&self.inner);
        inspector.delegate_inspectable(&*self.state.lock());
    }
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible as Never;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use netstack3_base::{testutil::FakeTimerCtxExt, IntoCoreTimerCtx};
    use packet_formats::ip::IpProto;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::{FakeBindingsCtx, FakeCtx},
        packets::{
            testutil::internal::{FakeIpPacket, FakeTcpSegment, TransportPacketExt},
            MaybeTransportPacketMut,
        },
        state::IpRoutines,
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

    impl<I: IpExt> MaybeTransportPacketMut<I> for NoTransportPacket {
        type TransportPacketMut<'a> = Never;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
            None
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
        let bindings_ctx = FakeBindingsCtx::<I>::new();

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
        let bindings_ctx = FakeBindingsCtx::<I>::new();

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
        let bindings_ctx = FakeBindingsCtx::<I>::new();

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
        let bindings_ctx = FakeBindingsCtx::<I>::new();

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
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        bindings_ctx.sleep(Duration::from_secs(1));

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
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(1));

        // Tuple in reply direction should set established to true and obviously
        // update last packet time.
        bindings_ctx.sleep(Duration::from_secs(1));
        assert_matches!(connection.update(&bindings_ctx, &reply_tuple), Ok(()));
        let state = connection.state();
        assert!(state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(2));

        // Unrelated connection should return an error and otherwise not touch
        // anything.
        bindings_ctx.sleep(Duration::from_secs(100));
        assert_matches!(
            connection.update(&bindings_ctx, &other_tuple),
            Err(ConnectionUpdateError::NonMatchingTuple)
        );
        assert!(state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(2));
    }

    #[ip_test]
    fn table_get_exclusive_connection_and_finalize_shared<I: Ip + IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        bindings_ctx.sleep(Duration::from_secs(1));
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

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
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(1));

        // Since the connection isn't present in the map, we should get a
        // freshly-allocated exclusive connection and the map should not have
        // been touched.
        assert_matches!(conn, Connection::Exclusive(_));
        assert!(!table.contains_tuple(&original_tuple));
        assert!(!table.contains_tuple(&reply_tuple));

        // Once we finalize the connection, it should be present in the map.
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok(true));
        assert!(table.contains_tuple(&original_tuple));
        assert!(table.contains_tuple(&reply_tuple));

        // We should now get a shared connection back for packets in either
        // direction now that the connection is present in the table.
        bindings_ctx.sleep(Duration::from_secs(1));
        let conn = table.get_connection_for_packet_and_update(&bindings_ctx, &packet);
        let conn = assert_matches!(conn, Some(conn) => conn);
        let state = conn.state();
        assert!(!state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(2));
        let conn = assert_matches!(conn, Connection::Shared(conn) => conn);

        bindings_ctx.sleep(Duration::from_secs(1));
        let reply_conn = table.get_connection_for_packet_and_update(&bindings_ctx, &reply_packet);
        let reply_conn = assert_matches!(reply_conn, Some(conn) => conn);
        let state = reply_conn.state();
        assert!(state.established);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(3));
        let reply_conn = assert_matches!(reply_conn, Connection::Shared(conn) => conn);

        // We should be getting the same connection in both directions.
        assert!(Arc::ptr_eq(&conn, &reply_conn));

        // Inserting the connection a second time shouldn't change the map.
        let conn = table.get_connection_for_packet_and_update(&bindings_ctx, &packet).unwrap();
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok(false));
        assert!(table.contains_tuple(&original_tuple));
        assert!(table.contains_tuple(&reply_tuple));
    }

    #[ip_test]
    fn table_conflict<I: Ip + IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

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

        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn1), Ok(true));
        assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn2),
            Err(FinalizeConnectionError::Conflict)
        );
        assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn3),
            Err(FinalizeConnectionError::Conflict)
        );
    }

    #[derive(Copy, Clone)]
    enum GcTrigger {
        /// Call [`perform_gc`] function directly, avoiding any timer logic.
        Direct,
        /// Trigger a timer expiry, which indirectly calls into [`perform_gc`].
        Timer,
    }

    #[ip_test]
    #[test_case(GcTrigger::Direct)]
    #[test_case(GcTrigger::Timer)]
    fn garbage_collection<I: Ip + IpExt + TestIpExt>(gc_trigger: GcTrigger) {
        fn perform_gc<I: IpExt>(
            core_ctx: &mut FakeCtx<I>,
            bindings_ctx: &mut FakeBindingsCtx<I>,
            gc_trigger: GcTrigger,
        ) {
            match gc_trigger {
                GcTrigger::Direct => core_ctx.conntrack().perform_gc(bindings_ctx),
                GcTrigger::Timer => {
                    for timer in bindings_ctx
                        .trigger_timers_until_instant(bindings_ctx.timer_ctx.instant.time, core_ctx)
                    {
                        assert_matches!(timer, FilterTimerId::ConntrackGc(_));
                    }
                }
            }
        }

        let mut bindings_ctx = FakeBindingsCtx::new();
        let mut core_ctx = FakeCtx::with_ip_routines(&mut bindings_ctx, IpRoutines::default());

        let first_packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        };

        let second_packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment { src_port: I::SRC_PORT + 1, dst_port: I::DST_PORT },
        };
        let second_packet_reply = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeTcpSegment { src_port: I::DST_PORT, dst_port: I::SRC_PORT + 1 },
        };

        let first_tuple = Tuple::from_packet(&first_packet).expect("packet should be valid");
        let first_tuple_reply = first_tuple.clone().invert();
        let second_tuple = Tuple::from_packet(&second_packet).expect("packet should be valid");
        let second_tuple_reply =
            Tuple::from_packet(&second_packet_reply).expect("packet should be valid");

        // T=0: Packets for two connections come in.
        let conn = core_ctx
            .conntrack()
            .get_connection_for_packet_and_update(&bindings_ctx, &first_packet)
            .expect("packet should be valid");
        assert!(core_ctx
            .conntrack()
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection finalize should succeed"));
        let conn = core_ctx
            .conntrack()
            .get_connection_for_packet_and_update(&bindings_ctx, &second_packet)
            .expect("packet should be valid");
        assert!(core_ctx
            .conntrack()
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection finalize should succeed"));
        assert!(core_ctx.conntrack().contains_tuple(&first_tuple));
        assert!(core_ctx.conntrack().contains_tuple(&second_tuple));

        // T=GC_INTERVAL: Triggering a GC does not clean up any connections,
        // because no connections are stale yet.
        bindings_ctx.sleep(GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);

        // T=GC_INTERVAL a packet for just the second connection comes in in the
        // reply direction, which causes the connection to be marked
        // established.
        let conn = core_ctx
            .conntrack()
            .get_connection_for_packet_and_update(&bindings_ctx, &second_packet_reply)
            .expect("packet should be valid");
        assert!(conn.state().established);
        assert!(!core_ctx
            .conntrack()
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection finalize should succeed"));
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);

        // The state in the table at this point is:
        // Connection 1
        //   - Not established
        //   - Last packet seen at T=0
        //   - Expires at T=CONNECTION_EXPIRY_TIME_UNESTABLISHED+GC_INTERVAL
        // Connection 2:
        //   - Established
        //   - Last packet seen at T=GC_INTERVAL
        //   - Expires at CONNECTION_EXPIRY_TIME_ESTABLISHED + GC_INTERVAL

        // T=2*GC_INTERVAL: Triggering a GC does not clean up any connections.
        bindings_ctx.sleep(GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);

        // Time advances to expiry for the first packet
        // (T=CONNECTION_EXPIRY_TIME_UNESTABLISHED + GC_INTERVAL) trigger gc and
        // note that the first connection was cleaned up
        bindings_ctx.sleep(CONNECTION_EXPIRY_TIME_UNESTABLISHED - GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);

        // Advance time past the expiry time for the second connection and see
        // that it is cleaned up.
        bindings_ctx
            .sleep(CONNECTION_EXPIRY_TIME_ESTABLISHED - CONNECTION_EXPIRY_TIME_UNESTABLISHED);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), false);
    }

    fn make_packets<I: IpExt + TestIpExt>(
        index: usize,
    ) -> (FakeIpPacket<I, FakeTcpSegment>, FakeIpPacket<I, FakeTcpSegment>) {
        // This ensures that, no matter what size MAXIMUM_CONNECTIONS is
        // (under 2^32, at least), we'll always have unique src and dst
        // ports, and thus unique connections.
        assert!(index < u32::MAX as usize);
        let src = (index % (u16::MAX as usize)) as u16;
        let dst = (index / (u16::MAX as usize)) as u16;

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment { src_port: src, dst_port: dst },
        };
        let reply_packet = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeTcpSegment { src_port: dst, dst_port: src },
        };

        (packet, reply_packet)
    }

    #[ip_test]
    #[test_case(true; "existing connections established")]
    #[test_case(false; "existing connections unestablished")]
    fn table_size_limit<I: Ip + IpExt + TestIpExt>(established: bool) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        bindings_ctx.sleep(Duration::from_secs(1));
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        // Fill up the table so that the next insertion will fail.
        for i in 0..MAXIMUM_CONNECTIONS {
            let (packet, reply_packet) = make_packets(i);
            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &packet)
                .expect("packet should be valid");
            assert!(table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"));

            // Whether to update the connection to be established by sending
            // through the reply packet.
            if established {
                let conn = table
                    .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
                    .expect("packet should be valid");
                assert!(!table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection finalize should succeed"));
            }
        }

        // The table should be full whether or not the connections are
        // established since finalize_connection always inserts the connection
        // under the original and reply tuples.
        assert_eq!(table.inner.lock().table.len(), TABLE_SIZE_LIMIT);

        let (packet, _) = make_packets(MAXIMUM_CONNECTIONS);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid");

        if established {
            // Inserting a new connection should fail because it would grow the
            // table.
            assert_matches!(
                table.finalize_connection(&mut bindings_ctx, conn),
                Err(FinalizeConnectionError::TableFull)
            );

            // Inserting an existing connection again should succeed because
            // it's not growing the table.
            let (packet, _) = make_packets(MAXIMUM_CONNECTIONS - 1);
            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &packet)
                .expect("packet should be valid");
            assert!(!table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"));
        } else {
            assert!(table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"));
        }
    }

    #[cfg(target_os = "fuchsia")]
    #[ip_test]
    fn inspect<I: Ip + IpExt + TestIpExt>() {
        use alloc::{boxed::Box, string::ToString};
        use netstack3_fuchsia::{
            testutils::{assert_data_tree, Inspector},
            FuchsiaInspector,
        };

        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        bindings_ctx.sleep(Duration::from_secs(1));
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        {
            let inspector = Inspector::new(Default::default());
            let mut bindings_inspector = FuchsiaInspector::<()>::new(inspector.root());
            bindings_inspector.delegate_inspectable(&table);

            assert_data_tree!(inspector, "root": {
                "table_limit_drops": 0u64,
                "table_limit_hits": 0u64,
                "num_connections": 0u64,
                "connections": {},
            });
        }

        // Insert the first connection into the table in an unestablished state.
        // This will later be evicted when the table fills up.
        let (packet, _) = make_packets::<I>(0);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid");
        assert!(!conn.state().established);
        assert!(table
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection finalize should succeed"));

        {
            let inspector = Inspector::new(Default::default());
            let mut bindings_inspector = FuchsiaInspector::<()>::new(inspector.root());
            bindings_inspector.delegate_inspectable(&table);

            assert_data_tree!(inspector, "root": {
                "table_limit_drops": 0u64,
                "table_limit_hits": 0u64,
                "num_connections": 1u64,
                "connections": {
                    "0": {
                        "original_tuple": {
                            "protocol": "TCP",
                            "src_addr": I::SRC_ADDR.to_string(),
                            "dst_addr": I::DST_ADDR.to_string(),
                            "src_port_or_id": 0u64,
                            "dst_port_or_id": 0u64,
                        },
                        "reply_tuple": {
                            "protocol": "TCP",
                            "src_addr": I::DST_ADDR.to_string(),
                            "dst_addr": I::SRC_ADDR.to_string(),
                            "src_port_or_id": 0u64,
                            "dst_port_or_id": 0u64,
                        },
                        "external_data": {},
                        "established": false,
                        "last_packet_time": 1_000_000_000u64,
                    }
                },
            });
        }

        // Fill the table up the rest of the way.
        for i in 1..MAXIMUM_CONNECTIONS {
            let (packet, reply_packet) = make_packets(i);
            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &packet)
                .expect("packet should be valid");
            assert!(table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"));

            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
                .expect("packet should be valid");
            assert!(!table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"));
        }

        assert_eq!(table.inner.lock().table.len(), TABLE_SIZE_LIMIT);

        // This first one should succeed because it can evict the
        // non-established connection.
        let (packet, reply_packet) = make_packets(MAXIMUM_CONNECTIONS);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid");
        assert!(table
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection finalize should succeed"));
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
            .expect("packet should be valid");
        assert!(!table
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection finalize should succeed"));

        // This next one should fail because there are no connections left to
        // evict.
        let (packet, _) = make_packets(MAXIMUM_CONNECTIONS + 1);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid");
        assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn),
            Err(FinalizeConnectionError::TableFull)
        );

        {
            let inspector = Inspector::new(Default::default());
            let mut bindings_inspector = FuchsiaInspector::<()>::new(inspector.root());
            bindings_inspector.delegate_inspectable(&table);

            assert_data_tree!(inspector, "root": contains {
                "table_limit_drops": 1u64,
                "table_limit_hits": 2u64,
                "num_connections": MAXIMUM_CONNECTIONS as u64,
            });
        }
    }
}
