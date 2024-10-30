// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod tcp;

use alloc::collections::HashMap;
use alloc::fmt::Debug;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use assert_matches::assert_matches;
use core::any::Any;
use core::fmt::Display;
use core::hash::Hash;
use core::time::Duration;

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip, IpVersionMarker};
use packet_formats::ip::{IpExt, IpProto, Ipv4Proto, Ipv6Proto};

use crate::context::{FilterBindingsContext, FilterBindingsTypes};
use crate::logic::FilterTimerId;
use crate::packets::{IpPacket, MaybeTransportPacket, TransportPacketData};
use netstack3_base::sync::Mutex;
use netstack3_base::{CoreTimerContext, Inspectable, Inspector, Instant, TimerContext};

/// The time from the end of one GC cycle to the beginning of the next.
const GC_INTERVAL: Duration = Duration::from_secs(10);

/// The time since the last seen packet after which an established UDP
/// connection will be considered expired and is eligible for garbage
/// collection.
///
/// This was taken from RFC 4787 REQ-5.
const CONNECTION_EXPIRY_TIME_UDP: Duration = Duration::from_secs(120);

/// The time since the last seen packet after which a generic connection will be
/// considered expired and is eligible for garbage collection.
const CONNECTION_EXPIRY_OTHER: Duration = Duration::from_secs(30);

/// The maximum number of connections in the conntrack table.
pub(crate) const MAXIMUM_CONNECTIONS: usize = 50_000;

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
    /// The number of connections in the table.
    ///
    /// We can't use the size of the HashMap because connections that have
    /// identical original and reply tuples (e.g. self-connected sockets) can
    /// only be inserted into the table once.
    num_connections: usize,
    /// A timer for triggering garbage collection events.
    gc_timer: BT::Timer,
    /// The number of times the table size limit was hit.
    table_limit_hits: u32,
    /// Of the times the table limit was hit, the number of times we had to drop
    /// a packet because we couldn't make space in the table.
    table_limit_drops: u32,
}

impl<I: IpExt, BT: FilterBindingsTypes, E> Table<I, BT, E> {
    /// Returns whether the table contains a connection for the specified tuple.
    ///
    /// This is for NAT to determine whether a generated tuple will clash with
    /// one already in the map. While it might seem inefficient, to require
    /// locking in a loop, taking an uncontested lock is going to be
    /// significantly faster than the RNG used to allocate NAT parameters.
    pub fn contains_tuple(&self, tuple: &Tuple<I>) -> bool {
        self.inner.lock().table.contains_key(tuple)
    }

    /// Returns a [`Connection`] for the flow indexed by `tuple`, if one exists.
    pub(crate) fn get_shared_connection(
        &self,
        tuple: &Tuple<I>,
    ) -> Option<Arc<ConnectionShared<I, BT, E>>> {
        let guard = self.inner.lock();
        let conn = guard.table.get(&tuple)?;
        Some(conn.clone())
    }

    /// Returns a [`Connection`] for the flow indexed by `tuple`, if one exists.
    pub fn get_connection(&self, tuple: &Tuple<I>) -> Option<Connection<I, BT, E>> {
        let guard = self.inner.lock();
        let conn = guard.table.get(&tuple)?;
        Some(Connection::Shared(conn.clone()))
    }

    /// Returns the number of connections in the table.
    #[cfg(feature = "testutils")]
    pub fn num_connections(&self) -> usize {
        self.inner.lock().num_connections
    }

    /// Removes the [`Connection`] for the flow indexed by `tuple`, if one exists,
    /// and returns it to the caller.
    #[cfg(feature = "testutils")]
    pub fn remove_connection(&mut self, tuple: &Tuple<I>) -> Option<Connection<I, BT, E>> {
        let mut guard = self.inner.lock();

        // Remove the entry indexed by the tuple.
        let conn = guard.table.remove(&tuple)?;
        let (original, reply) = (&conn.inner.original_tuple, &conn.inner.reply_tuple);

        // If this is not a self-connected flow, we need to remove the other tuple on
        // which the connection is indexed.
        if original != reply {
            if tuple == original {
                assert!(guard.table.remove(reply).is_some());
            } else {
                assert!(guard.table.remove(original).is_some());
            }
        }

        guard.num_connections -= 1;

        Some(Connection::Shared(conn))
    }
}

fn schedule_gc<BC>(bindings_ctx: &mut BC, timer: &mut BC::Timer)
where
    BC: TimerContext,
{
    let _ = bindings_ctx.schedule_timer(GC_INTERVAL, timer);
}

impl<
        I: IpExt,
        BC: FilterBindingsContext,
        E: Default + Send + Sync + Debug + PartialEq + CompatibleWith + 'static,
    > Table<I, BC, E>
{
    pub(crate) fn new<CC: CoreTimerContext<FilterTimerId<I>, BC>>(bindings_ctx: &mut BC) -> Self {
        Self {
            inner: Mutex::new(TableInner {
                table: HashMap::new(),
                gc_timer: CC::new_timer(
                    bindings_ctx,
                    FilterTimerId::ConntrackGc(IpVersionMarker::<I>::new()),
                ),
                num_connections: 0,
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
    pub(crate) fn finalize_connection(
        &self,
        bindings_ctx: &mut BC,
        connection: Connection<I, BC, E>,
    ) -> Result<(bool, Option<Arc<ConnectionShared<I, BC, E>>>), FinalizeConnectionError> {
        let exclusive = match connection {
            Connection::Exclusive(c) => c,
            // Given that make_shared is private, the only way for us to receive
            // a shared connection is if it was already present in the map. This
            // is far and away the most common case under normal operation.
            Connection::Shared(inner) => return Ok((false, Some(inner))),
        };

        if exclusive.do_not_insert {
            return Ok((false, None));
        }

        let mut guard = self.inner.lock();

        // We multiply the table size limit because each connection is inserted
        // into the table twice, once for the original tuple and again for the
        // reply tuple.
        if guard.num_connections >= MAXIMUM_CONNECTIONS {
            guard.table_limit_hits = guard.table_limit_hits.saturating_add(1);
            if let Some((original_tuple, reply_tuple)) = guard
                .table
                .iter()
                .filter_map(|(_, conn)| match conn.state.lock().establishment_lifecycle {
                    EstablishmentLifecycle::SeenOriginal => {
                        Some((conn.inner.original_tuple.clone(), conn.inner.reply_tuple.clone()))
                    }
                    EstablishmentLifecycle::SeenReply | EstablishmentLifecycle::Established => None,
                })
                .next()
            {
                assert!(guard.table.remove(&original_tuple).is_some());
                if original_tuple != reply_tuple {
                    assert!(guard.table.remove(&reply_tuple).is_some());
                }

                guard.num_connections -= 1;
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
        if guard.table.contains_key(&exclusive.inner.original_tuple)
            || guard.table.contains_key(&exclusive.inner.reply_tuple)
        {
            // NOTE: It's possible for the first two packets (or more) in the
            // same flow to create ExclusiveConnections. Typically packets for
            // the same flow are handled sequentically, so each subsequent
            // packet should see the connection created by the first one.
            // However, it is possible (e.g. if these two packets arrive on
            // different interfaces) for them to race.
            //
            // In this case, subsequent packets would be reported as conflicts.
            // To avoid this race condition, we check whether the conflicting
            // connection in the table is actually the same as the connection
            // that we are attempting to finalize; if so, we can simply adopt
            // the already-finalized connection.
            let conn = if let Some(conn) = guard.table.get(&exclusive.inner.original_tuple) {
                conn
            } else {
                guard
                    .table
                    .get(&exclusive.inner.reply_tuple)
                    .expect("checked that tuple is in table and table is locked")
            };
            if conn.compatible_with(&exclusive) {
                return Ok((false, Some(conn.clone())));
            }

            // TODO(https://fxbug.dev/372549231): add a counter for this error.
            Err(FinalizeConnectionError::Conflict)
        } else {
            let shared = exclusive.make_shared();
            let clone = Arc::clone(&shared);

            let res = guard.table.insert(shared.inner.original_tuple.clone(), shared.clone());
            debug_assert!(res.is_none());

            if shared.inner.reply_tuple != shared.inner.original_tuple {
                let res = guard.table.insert(shared.inner.reply_tuple.clone(), shared);
                debug_assert!(res.is_none());
            }

            guard.num_connections += 1;

            // For the most part, this will only schedule the timer once, when
            // the first packet hits the netstack. However, since the GC timer
            // is only rescheduled during GC when the table has entries, it's
            // possible that this will be called again if the table ever becomes
            // empty.
            if bindings_ctx.scheduled_instant(&mut guard.gc_timer).is_none() {
                schedule_gc(bindings_ctx, &mut guard.gc_timer);
            }

            Ok((true, Some(clone)))
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
    pub(crate) fn get_connection_for_packet_and_update<P: IpPacket<I>>(
        &self,
        bindings_ctx: &BC,
        packet: &P,
    ) -> Result<Option<Connection<I, BC, E>>, GetConnectionError<I, BC, E>> {
        let Some(packet) = PacketMetadata::new(packet) else {
            return Ok(None);
        };

        let mut connection = match self.inner.lock().table.get(&packet.tuple) {
            Some(connection) => Connection::Shared(connection.clone()),
            None => Connection::Exclusive(
                match ConnectionExclusive::from_deconstructed_packet(bindings_ctx, &packet) {
                    None => return Ok(None),
                    Some(c) => c,
                },
            ),
        };

        match connection.update(bindings_ctx, &packet) {
            Ok(ConnectionUpdateAction::NoAction) => Ok(Some(connection)),
            Ok(ConnectionUpdateAction::RemoveEntry) => match connection {
                Connection::Exclusive(mut conn) => {
                    conn.do_not_insert = true;
                    Ok(Some(Connection::Exclusive(conn)))
                }
                Connection::Shared(conn) => {
                    // RACE: It's possible that GC already removed the
                    // connection from the table, since we released the table
                    // lock while updating the connection.
                    let mut guard = self.inner.lock();
                    let _ = guard.table.remove(&conn.inner.original_tuple);
                    let _ = guard.table.remove(&conn.inner.reply_tuple);

                    Ok(Some(Connection::Shared(conn)))
                }
            },
            Err(e) => match e {
                ConnectionUpdateError::NonMatchingTuple => {
                    panic!(
                        "Tuple didn't match. tuple={:?}, conn.original={:?}, conn.reply={:?}",
                        &packet.tuple,
                        connection.original_tuple(),
                        connection.reply_tuple()
                    );
                }
                ConnectionUpdateError::InvalidPacket => {
                    Err(GetConnectionError::InvalidPacket(connection))
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

        guard.num_connections -= to_remove.len();
        for (original_tuple, reply_tuple) in to_remove {
            assert!(guard.table.remove(&original_tuple).is_some());
            if reply_tuple != original_tuple {
                assert!(guard.table.remove(&reply_tuple).is_some());
            }
        }

        // The table is only expected to be empty in exceptional cases, or
        // during tests. The test case especially important, because some tests
        // will wait for core to quiesce by waiting for timers to stop firing.
        // By only rescheduling when there are still entries in the table, we
        // ensure that we won't enter an infinite timer firing/scheduling loop.
        if guard.num_connections > 0 {
            schedule_gc(bindings_ctx, &mut guard.gc_timer);
        }
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E: Inspectable> Inspectable for Table<I, BT, E> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let guard = self.inner.lock();

        inspector.record_usize("num_connections", guard.num_connections);
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct Tuple<I: IpExt> {
    /// The IP protocol number of the flow.
    pub protocol: TransportProtocol,
    /// The source IP address of the flow.
    pub src_addr: I::Addr,
    /// The destination IP address of the flow.
    pub dst_addr: I::Addr,
    /// The transport-layer source port or ID of the flow.
    pub src_port_or_id: u16,
    /// The transport-layer destination port or ID of the flow.
    pub dst_port_or_id: u16,
}

impl<I: IpExt> Tuple<I> {
    /// Creates a `Tuple` from an `IpPacket`, if possible.
    ///
    /// Returns `None` if the packet doesn't have an inner transport packet.
    pub(crate) fn from_packet<'a, P: IpPacket<I>>(packet: &'a P) -> Option<Self> {
        // Subtlety: For ICMP packets, only request/response messages will have
        // a transport packet defined (and currently only ECHO messages do).
        // This gets us basic tracking for free, and lets us implicitly ignore
        // ICMP errors, which are not meant to be tracked.
        //
        // If other message types eventually have TransportPacket impls, then
        // this would lead to confusing different message types that happen to
        // have the same ID.
        let transport_packet_data = packet.maybe_transport_packet().transport_packet_data()?;
        Some(Self::from_packet_and_transport_data(packet, &transport_packet_data))
    }

    fn from_packet_and_transport_data<'a, P: IpPacket<I>>(
        packet: &'a P,
        transport_packet_data: &TransportPacketData,
    ) -> Self {
        let protocol = I::map_ip(packet.protocol(), |proto| proto.into(), |proto| proto.into());

        let (src_port, dst_port) = match transport_packet_data {
            TransportPacketData::Tcp { src_port, dst_port, .. }
            | TransportPacketData::Generic { src_port, dst_port } => (*src_port, *dst_port),
        };

        Self {
            protocol: protocol,
            src_addr: packet.src_addr(),
            dst_addr: packet.dst_addr(),
            src_port_or_id: src_port,
            dst_port_or_id: dst_port,
        }
    }

    /// Returns the inverted version of the tuple.
    ///
    /// This means the src and dst addresses are swapped. For TCP and UDP, the
    /// ports are reversed, but for ICMP, where the ports stand in for other
    /// information, things are more complicated.
    pub(crate) fn invert(self) -> Tuple<I> {
        // TODO(https://fxbug.dev/328064082): Support tracking different ICMP
        // request/response types.
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
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
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

/// Type to track additional processing required after updating a connection.
#[derive(Debug, PartialEq, Eq)]
enum ConnectionUpdateAction {
    /// Processing completed and no further action necessary.
    NoAction,

    /// The entry for this connection should be removed from the conntrack table.
    RemoveEntry,
}

/// An error returned from [`Connection::update`].
#[derive(Debug, PartialEq, Eq)]
enum ConnectionUpdateError {
    /// The provided tuple doesn't belong to the connection being updated.
    NonMatchingTuple,

    /// The packet was invalid. The caller may decide whether to drop this
    /// packet or not.
    InvalidPacket,
}

/// An error returned from [`Table::get_connection_for_packet_and_update`].
#[derive(Derivative)]
#[derivative(Debug(bound = "E: Debug"))]
pub(crate) enum GetConnectionError<I: IpExt, BT: FilterBindingsTypes, E> {
    /// The packet was invalid. The caller may decide whether to drop it or not.
    InvalidPacket(Connection<I, BT, E>),
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

/// An error when attempting to retrieve the underlying conntrack entry from a
/// weak handle to it.
#[derive(Debug)]
pub enum WeakConnectionError {
    /// The entry was removed from the table after the weak handle was created.
    EntryRemoved,
    /// The entry does not match the type that is expected (due to an IP version
    /// mismatch, for example).
    InvalidEntry,
}

/// A type-erased weak handle to a connection tracking entry.
///
/// We use type erasure here to get rid of the parameterization on IP version;
/// this handle is meant to be able to transit the device layer and at that
/// point things are not parameterized on IP version (IPv4 and IPv6 packets both
/// end up in the same device queue, for example).
///
/// When this is received for incoming packets, [`WeakConnection::into_inner`]
/// can be used to downcast to the expected concrete [`Connection`] type.
#[derive(Debug, Clone)]
pub struct WeakConnection(pub(crate) Weak<dyn Any + Send + Sync>);

impl WeakConnection {
    /// Creates a new type-erased weak handle to the provided conntrack entry,
    /// provided it is a shared entry.
    pub fn new<I: IpExt, BT: FilterBindingsTypes + 'static, E: Send + Sync + 'static>(
        conn: &Connection<I, BT, E>,
    ) -> Option<Self> {
        let shared = match conn {
            Connection::Exclusive(_) => return None,
            Connection::Shared(shared) => shared,
        };
        let weak = Arc::downgrade(shared);
        Some(Self(weak))
    }

    /// Attempts to upgrade the provided weak handle to the conntrack entry and
    /// downcast it to the specified concrete [`Connection`] type.
    ///
    /// Fails if either the weak handle cannot be upgraded (because the conntrack
    /// entry has since been removed), or the type-erased handle cannot be downcast
    /// to the concrete type (because the packet was modified after the creation of
    /// this handle such that it no longer matches, e.g. the IP version of the
    /// connection).
    pub fn into_inner<I: IpExt, BT: FilterBindingsTypes + 'static, E: Send + Sync + 'static>(
        self,
    ) -> Result<Connection<I, BT, E>, WeakConnectionError> {
        let Self(inner) = self;
        let shared = inner
            .upgrade()
            .ok_or(WeakConnectionError::EntryRemoved)?
            .downcast()
            .map_err(|_err: Arc<_>| WeakConnectionError::InvalidEntry)?;
        Ok(Connection::Shared(shared))
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E> Connection<I, BT, E> {
    /// Returns the tuple of the original direction of this connection.
    pub fn original_tuple(&self) -> &Tuple<I> {
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
    pub fn external_data(&self) -> &E {
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

        // The ordering here is sadly mildly load-bearing. For self-connected
        // sockets, the first comparison will be true, so having the original
        // tuple first would mean that the connection is never marked
        // established.
        //
        // This ordering means that all self-connected connections will be
        // marked as established immediately upon receiving the first packet.
        if tuple == reply {
            Some(ConnectionDirection::Reply)
        } else if tuple == original {
            Some(ConnectionDirection::Original)
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
    fn update(
        &mut self,
        bindings_ctx: &BC,
        packet: &PacketMetadata<I>,
    ) -> Result<ConnectionUpdateAction, ConnectionUpdateError> {
        let direction = match self.direction(&packet.tuple) {
            Some(d) => d,
            None => return Err(ConnectionUpdateError::NonMatchingTuple),
        };

        let now = bindings_ctx.now();

        match self {
            Connection::Exclusive(c) => c.state.update(direction, &packet.transport_data, now),
            Connection::Shared(c) => c.state.lock().update(direction, &packet.transport_data, now),
        }
    }
}

/// Fields common to both [`ConnectionExclusive`] and [`ConnectionShared`].
#[derive(Derivative)]
#[derivative(Debug(bound = "E: Debug"), PartialEq(bound = "E: PartialEq"))]
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

#[derive(Debug, Clone)]
enum ProtocolState {
    Tcp(tcp::Connection),
    Udp {
        /// Whether this connection has seen packets in both directions.
        established: bool,
    },
    Other {
        /// Whether this connection has seen packets in both directions.
        established: bool,
    },
}

impl ProtocolState {
    fn update(
        &mut self,
        dir: ConnectionDirection,
        transport_data: &TransportPacketData,
    ) -> Result<ConnectionUpdateAction, ConnectionUpdateError> {
        match self {
            ProtocolState::Tcp(tcp_conn) => {
                let (segment, payload_len) = assert_matches!(
                    transport_data,
                    TransportPacketData::Tcp { segment, payload_len, .. } => (segment, payload_len)
                );
                tcp_conn.update(&segment, *payload_len, dir)
            }
            ProtocolState::Udp { established } | ProtocolState::Other { established } => {
                match dir {
                    ConnectionDirection::Original => (),
                    ConnectionDirection::Reply => *established = true,
                }

                Ok(ConnectionUpdateAction::NoAction)
            }
        }
    }
}

/// The lifecycle of the connection getting to being established.
///
/// To mimic Linux behavior, we require seeing three packets in order to mark a
/// connection established.
/// 1. Original
/// 2. Reply
/// 3. Original
///
/// The first packet is implicit in the creation of the connection when the
/// first packet is seen.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum EstablishmentLifecycle {
    SeenOriginal,
    SeenReply,
    Established,
}

impl EstablishmentLifecycle {
    fn update(self, dir: ConnectionDirection) -> Self {
        match self {
            EstablishmentLifecycle::SeenOriginal => match dir {
                ConnectionDirection::Original => self,
                ConnectionDirection::Reply => EstablishmentLifecycle::SeenReply,
            },
            EstablishmentLifecycle::SeenReply => match dir {
                ConnectionDirection::Original => EstablishmentLifecycle::Established,
                ConnectionDirection::Reply => self,
            },
            EstablishmentLifecycle::Established => self,
        }
    }
}

/// Dynamic per-connection state.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Debug(bound = ""))]
pub(crate) struct ConnectionState<BT: FilterBindingsTypes> {
    /// The time the last packet was seen for this connection (in either of the
    /// original or reply directions).
    last_packet_time: BT::Instant,

    /// Where in the generic establishment lifecycle the current connection is.
    establishment_lifecycle: EstablishmentLifecycle,

    /// State that is specific to a given protocol (e.g. TCP or UDP).
    protocol_state: ProtocolState,
}

impl<BT: FilterBindingsTypes> ConnectionState<BT> {
    fn update(
        &mut self,
        dir: ConnectionDirection,
        transport_data: &TransportPacketData,
        now: BT::Instant,
    ) -> Result<ConnectionUpdateAction, ConnectionUpdateError> {
        if self.last_packet_time < now {
            self.last_packet_time = now;
        }

        self.establishment_lifecycle = self.establishment_lifecycle.update(dir);

        self.protocol_state.update(dir, transport_data)
    }
}

impl<BT: FilterBindingsTypes> Inspectable for ConnectionState<BT> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        inspector.record_bool(
            "established",
            match self.establishment_lifecycle {
                EstablishmentLifecycle::SeenOriginal | EstablishmentLifecycle::SeenReply => false,
                EstablishmentLifecycle::Established => true,
            },
        );
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

    /// When true, do not insert the connection into the conntrack table.
    ///
    /// This allows the stack to still operate against the connection (e.g. for
    /// NAT), while guaranteeing that it won't make it into the table.
    do_not_insert: bool,
}

impl<I: IpExt, BT: FilterBindingsTypes, E> ConnectionExclusive<I, BT, E> {
    /// Turn this exclusive connection into a shared one. This is required in
    /// order to insert into the [`Table`] table.
    fn make_shared(self) -> Arc<ConnectionShared<I, BT, E>> {
        Arc::new(ConnectionShared { inner: self.inner, state: Mutex::new(self.state) })
    }

    pub(crate) fn reply_tuple_mut(&mut self) -> &mut Tuple<I> {
        &mut self.inner.reply_tuple
    }
}

impl<I: IpExt, BC: FilterBindingsContext, E: Default> ConnectionExclusive<I, BC, E> {
    pub(crate) fn from_deconstructed_packet(
        bindings_ctx: &BC,
        PacketMetadata { tuple, transport_data }: &PacketMetadata<I>,
    ) -> Option<Self> {
        let reply_tuple = tuple.clone().invert();
        let self_connected = reply_tuple == *tuple;

        Some(Self {
            inner: ConnectionCommon {
                original_tuple: tuple.clone(),
                reply_tuple,
                external_data: E::default(),
            },
            state: ConnectionState {
                last_packet_time: bindings_ctx.now(),
                establishment_lifecycle: EstablishmentLifecycle::SeenOriginal,
                protocol_state: match tuple.protocol {
                    TransportProtocol::Tcp => {
                        let (segment, payload_len) = transport_data
                            .tcp_segment_and_len()
                            .expect("protocol was TCP, so transport data should have TCP info");

                        ProtocolState::Tcp(tcp::Connection::new(
                            segment,
                            payload_len,
                            self_connected,
                        )?)
                    }
                    TransportProtocol::Udp => ProtocolState::Udp { established: false },
                    TransportProtocol::Icmp | TransportProtocol::Other(_) => {
                        ProtocolState::Other { established: false }
                    }
                },
            },
            do_not_insert: false,
        })
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

/// The IP-agnostic transport protocol of a packet.
#[allow(missing_docs)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, GenericOverIp)]
#[generic_over_ip()]
pub enum TransportProtocol {
    Tcp,
    Udp,
    Icmp,
    Other(u8),
}

impl From<Ipv4Proto> for TransportProtocol {
    fn from(value: Ipv4Proto) -> Self {
        match value {
            Ipv4Proto::Proto(IpProto::Tcp) => TransportProtocol::Tcp,
            Ipv4Proto::Proto(IpProto::Udp) => TransportProtocol::Udp,
            Ipv4Proto::Icmp => TransportProtocol::Icmp,
            v => TransportProtocol::Other(v.into()),
        }
    }
}

impl From<Ipv6Proto> for TransportProtocol {
    fn from(value: Ipv6Proto) -> Self {
        match value {
            Ipv6Proto::Proto(IpProto::Tcp) => TransportProtocol::Tcp,
            Ipv6Proto::Proto(IpProto::Udp) => TransportProtocol::Udp,
            Ipv6Proto::Icmpv6 => TransportProtocol::Icmp,
            v => TransportProtocol::Other(v.into()),
        }
    }
}

impl From<IpProto> for TransportProtocol {
    fn from(value: IpProto) -> Self {
        match value {
            IpProto::Tcp => TransportProtocol::Tcp,
            IpProto::Udp => TransportProtocol::Udp,
            v @ IpProto::Reserved => TransportProtocol::Other(v.into()),
        }
    }
}

impl Display for TransportProtocol {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TransportProtocol::Tcp => write!(f, "TCP"),
            TransportProtocol::Udp => write!(f, "UDP"),
            TransportProtocol::Icmp => write!(f, "ICMP"),
            TransportProtocol::Other(n) => write!(f, "Other({n})"),
        }
    }
}

impl Debug for TransportProtocol {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self, f)
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E> ConnectionShared<I, BT, E> {
    fn is_expired(&self, now: BT::Instant) -> bool {
        let state = self.state.lock().clone();
        let duration = now.saturating_duration_since(state.last_packet_time);

        let expiry_duration = match state.protocol_state {
            ProtocolState::Tcp(tcp_conn) => tcp_conn.expiry_duration(state.establishment_lifecycle),
            ProtocolState::Udp { .. } => CONNECTION_EXPIRY_TIME_UDP,
            // ICMP ends up here. The ICMP messages we track are simple
            // request/response protocols, so we always expect to get a response
            // quickly (within 2 RTT). Any followup messages (e.g. if making
            // periodic ECHO requests) should reuse this existing connection.
            ProtocolState::Other { .. } => CONNECTION_EXPIRY_OTHER,
        };

        duration >= expiry_duration
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E: CompatibleWith> ConnectionShared<I, BT, E> {
    /// Returns whether the provided exclusive connection is compatible with this
    /// one, to the extent that a shared reference to this tracked connection could
    /// be adopted in place of the exclusive connection.
    pub(crate) fn compatible_with(&self, conn: &ConnectionExclusive<I, BT, E>) -> bool {
        self.inner.original_tuple == conn.inner.original_tuple
            && self.inner.reply_tuple == conn.inner.reply_tuple
            && self.inner.external_data.compatible_with(&conn.inner.external_data)
    }
}

impl<I: IpExt, BT: FilterBindingsTypes, E: Inspectable> Inspectable for ConnectionShared<I, BT, E> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        inspector.delegate_inspectable(&self.inner);
        inspector.delegate_inspectable(&*self.state.lock());
    }
}

/// Allows a caller to check whether a given connection tracking entry (or some
/// configuration owned by that entry) is compatible with another.
pub trait CompatibleWith {
    /// Returns whether the provided entity is compatible with this entity in the
    /// context of connection tracking.
    fn compatible_with(&self, other: &Self) -> bool;
}

/// A struct containing relevant fields extracted from the IP and transport
/// headers that means we only have to touch the incoming IpPacket once. Also
/// acts as a witness type that the tuple and transport data have the same
/// transport protocol.
pub(crate) struct PacketMetadata<I: IpExt> {
    tuple: Tuple<I>,
    transport_data: TransportPacketData,
}

impl<I: IpExt> PacketMetadata<I> {
    pub(crate) fn new<P: IpPacket<I>>(packet: &P) -> Option<Self> {
        let transport_packet_data = packet.maybe_transport_packet().transport_packet_data()?;

        let tuple = Tuple::from_packet_and_transport_data(packet, &transport_packet_data);

        match tuple.protocol {
            TransportProtocol::Tcp => {
                assert_matches!(transport_packet_data, TransportPacketData::Tcp { .. })
            }
            TransportProtocol::Udp | TransportProtocol::Icmp | TransportProtocol::Other(_) => {
                assert_matches!(transport_packet_data, TransportPacketData::Generic { .. })
            }
        }

        Some(Self { tuple, transport_data: transport_packet_data })
    }
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible as Never;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_declare::{net_ip_v4, net_ip_v6};
    use net_types::ip::{Ipv4, Ipv6};
    use netstack3_base::testutil::FakeTimerCtxExt;
    use netstack3_base::{
        Control, IntoCoreTimerCtx, Options, SegmentHeader, SeqNum, UnscaledWindowSize,
    };
    use packet_formats::ip::IpProto;
    use test_case::test_case;

    use super::*;
    use crate::context::testutil::{FakeBindingsCtx, FakeCtx};
    use crate::packets::testutil::internal::{
        ArbitraryValue, FakeIpPacket, FakeTcpSegment, FakeUdpPacket, TransportPacketExt,
    };
    use crate::packets::MaybeTransportPacketMut;
    use crate::state::IpRoutines;

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
        fn transport_packet_data(&self) -> Option<TransportPacketData> {
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

    impl CompatibleWith for () {
        fn compatible_with(&self, (): &()) -> bool {
            true
        }
    }

    #[test_case(
        EstablishmentLifecycle::SeenOriginal,
        ConnectionDirection::Original
          => EstablishmentLifecycle::SeenOriginal
    )]
    #[test_case(
        EstablishmentLifecycle::SeenOriginal,
        ConnectionDirection::Reply
          => EstablishmentLifecycle::SeenReply
    )]
    #[test_case(
        EstablishmentLifecycle::SeenReply,
        ConnectionDirection::Original
          => EstablishmentLifecycle::Established
    )]
    #[test_case(
        EstablishmentLifecycle::SeenReply,
        ConnectionDirection::Reply
          => EstablishmentLifecycle::SeenReply
    )]
    #[test_case(
        EstablishmentLifecycle::Established,
        ConnectionDirection::Original
          => EstablishmentLifecycle::Established
    )]
    #[test_case(
        EstablishmentLifecycle::Established,
        ConnectionDirection::Reply
          => EstablishmentLifecycle::Established
    )]
    fn establishment_lifecycle_test(
        lifecycle: EstablishmentLifecycle,
        dir: ConnectionDirection,
    ) -> EstablishmentLifecycle {
        lifecycle.update(dir)
    }

    #[ip_test(I)]
    #[test_case(TransportProtocol::Udp)]
    #[test_case(TransportProtocol::Tcp)]
    fn tuple_invert_udp_tcp<I: IpExt + TestIpExt>(protocol: TransportProtocol) {
        let orig_tuple = Tuple::<I> {
            protocol: protocol,
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };

        let expected = Tuple::<I> {
            protocol: protocol,
            src_addr: I::DST_ADDR,
            dst_addr: I::SRC_ADDR,
            src_port_or_id: I::DST_PORT,
            dst_port_or_id: I::SRC_PORT,
        };

        let inverted = orig_tuple.invert();

        assert_eq!(inverted, expected);
    }

    #[ip_test(I)]
    fn tuple_from_tcp_packet<I: IpExt + TestIpExt>() {
        let expected = Tuple::<I> {
            protocol: TransportProtocol::Tcp,
            src_addr: I::SRC_ADDR,
            dst_addr: I::DST_ADDR,
            src_port_or_id: I::SRC_PORT,
            dst_port_or_id: I::DST_PORT,
        };

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment {
                src_port: I::SRC_PORT,
                dst_port: I::DST_PORT,
                segment: SegmentHeader::arbitrary_value(),
                payload_len: 4,
            },
        };

        let tuple = Tuple::from_packet(&packet).expect("valid TCP packet should return a tuple");
        assert_eq!(tuple, expected);
    }

    #[ip_test(I)]
    fn tuple_from_packet_no_body<I: IpExt + TestIpExt>() {
        let packet = FakeIpPacket::<I, NoTransportPacket> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: NoTransportPacket {},
        };

        let tuple = Tuple::from_packet(&packet);
        assert_matches!(tuple, None);
    }

    #[ip_test(I)]
    fn connection_from_tuple<I: IpExt + TestIpExt>() {
        let bindings_ctx = FakeBindingsCtx::<I>::new();

        let packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        })
        .unwrap();
        let original_tuple = packet.tuple.clone();
        let reply_tuple = original_tuple.clone().invert();

        let connection =
            ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(&bindings_ctx, &packet)
                .unwrap();

        assert_eq!(&connection.inner.original_tuple, &original_tuple);
        assert_eq!(&connection.inner.reply_tuple, &reply_tuple);
    }

    #[ip_test(I)]
    fn connection_make_shared_has_same_underlying_info<I: IpExt + TestIpExt>() {
        let bindings_ctx = FakeBindingsCtx::<I>::new();

        let packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        })
        .unwrap();
        let original_tuple = packet.tuple.clone();
        let reply_tuple = original_tuple.clone().invert();

        let mut connection =
            ConnectionExclusive::from_deconstructed_packet(&bindings_ctx, &packet).unwrap();
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

    #[ip_test(I)]
    #[test_case(ConnectionKind::Exclusive)]
    #[test_case(ConnectionKind::Shared)]
    fn connection_getters<I: IpExt + TestIpExt>(connection_kind: ConnectionKind) {
        let bindings_ctx = FakeBindingsCtx::<I>::new();

        let packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        })
        .unwrap();
        let original_tuple = packet.tuple.clone();
        let reply_tuple = original_tuple.clone().invert();

        let mut connection =
            ConnectionExclusive::from_deconstructed_packet(&bindings_ctx, &packet).unwrap();
        connection.inner.external_data = 1234;

        let connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_eq!(connection.original_tuple(), &original_tuple);
        assert_eq!(connection.reply_tuple(), &reply_tuple);
        assert_eq!(connection.external_data(), &1234);
    }

    #[ip_test(I)]
    #[test_case(ConnectionKind::Exclusive)]
    #[test_case(ConnectionKind::Shared)]
    fn connection_direction<I: IpExt + TestIpExt>(connection_kind: ConnectionKind) {
        let bindings_ctx = FakeBindingsCtx::<I>::new();

        let packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        })
        .unwrap();
        let original_tuple = packet.tuple.clone();
        let reply_tuple = original_tuple.clone().invert();

        let mut other_tuple = original_tuple.clone();
        other_tuple.src_port_or_id += 1;

        let connection: ConnectionExclusive<_, _, ()> =
            ConnectionExclusive::from_deconstructed_packet(&bindings_ctx, &packet).unwrap();
        let connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_matches!(connection.direction(&original_tuple), Some(ConnectionDirection::Original));
        assert_matches!(connection.direction(&reply_tuple), Some(ConnectionDirection::Reply));
        assert_matches!(connection.direction(&other_tuple), None);
    }

    #[ip_test(I)]
    #[test_case(ConnectionKind::Exclusive)]
    #[test_case(ConnectionKind::Shared)]
    fn connection_update<I: IpExt + TestIpExt>(connection_kind: ConnectionKind) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        bindings_ctx.sleep(Duration::from_secs(1));

        let packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        })
        .unwrap();

        let reply_packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeUdpPacket { src_port: I::DST_PORT, dst_port: I::SRC_PORT },
        })
        .unwrap();

        let other_packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeUdpPacket { src_port: I::DST_PORT, dst_port: I::SRC_PORT + 1 },
        })
        .unwrap();

        let connection =
            ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(&bindings_ctx, &packet)
                .unwrap();
        let mut connection = match connection_kind {
            ConnectionKind::Exclusive => Connection::Exclusive(connection),
            ConnectionKind::Shared => Connection::Shared(connection.make_shared()),
        };

        assert_matches!(
            connection.update(&bindings_ctx, &packet),
            Ok(ConnectionUpdateAction::NoAction)
        );
        let state = connection.state();
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenOriginal);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(1));

        // Tuple in reply direction should set established to true and obviously
        // update last packet time.
        bindings_ctx.sleep(Duration::from_secs(1));
        assert_matches!(
            connection.update(&bindings_ctx, &reply_packet),
            Ok(ConnectionUpdateAction::NoAction)
        );
        let state = connection.state();
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenReply);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(2));

        // Unrelated connection should return an error and otherwise not touch
        // anything.
        bindings_ctx.sleep(Duration::from_secs(100));
        assert_matches!(
            connection.update(&bindings_ctx, &other_packet),
            Err(ConnectionUpdateError::NonMatchingTuple)
        );
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenReply);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(2));
    }

    #[ip_test(I)]
    fn table_get_exclusive_connection_and_finalize_shared<I: IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        bindings_ctx.sleep(Duration::from_secs(1));
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        };

        let reply_packet = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeUdpPacket { src_port: I::DST_PORT, dst_port: I::SRC_PORT },
        };

        let original_tuple = Tuple::from_packet(&packet).expect("packet should be valid");
        let reply_tuple = Tuple::from_packet(&reply_packet).expect("packet should be valid");

        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("connection should be present");
        let state = conn.state();
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenOriginal);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(1));

        // Since the connection isn't present in the map, we should get a
        // freshly-allocated exclusive connection and the map should not have
        // been touched.
        assert_matches!(conn, Connection::Exclusive(_));
        assert!(!table.contains_tuple(&original_tuple));
        assert!(!table.contains_tuple(&reply_tuple));

        // Once we finalize the connection, it should be present in the map.
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok((true, Some(_))));
        assert!(table.contains_tuple(&original_tuple));
        assert!(table.contains_tuple(&reply_tuple));

        // We should now get a shared connection back for packets in either
        // direction now that the connection is present in the table.
        bindings_ctx.sleep(Duration::from_secs(1));
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid");
        let conn = assert_matches!(conn, Some(conn) => conn);
        let state = conn.state();
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenOriginal);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(2));
        let conn = assert_matches!(conn, Connection::Shared(conn) => conn);

        bindings_ctx.sleep(Duration::from_secs(1));
        let reply_conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
            .expect("packet should be valid");
        let reply_conn = assert_matches!(reply_conn, Some(conn) => conn);
        let state = reply_conn.state();
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenReply);
        assert_eq!(state.last_packet_time.offset, Duration::from_secs(3));
        let reply_conn = assert_matches!(reply_conn, Connection::Shared(conn) => conn);

        // We should be getting the same connection in both directions.
        assert!(Arc::ptr_eq(&conn, &reply_conn));

        // Inserting the connection a second time shouldn't change the map.
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .unwrap();
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok((false, Some(_))));
        assert!(table.contains_tuple(&original_tuple));
        assert!(table.contains_tuple(&reply_tuple));
    }

    #[ip_test(I)]
    fn table_conflict<I: IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        let original_packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        })
        .unwrap();

        let nated_original_packet = PacketMetadata::new(&FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT + 1, dst_port: I::DST_PORT + 1 },
        })
        .unwrap();

        let conn1 = Connection::Exclusive(
            ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(
                &bindings_ctx,
                &original_packet,
            )
            .unwrap(),
        );

        // Fake NAT that ends up allocating the same reply tuple as an existing
        // connection.
        let mut conn2 = ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(
            &bindings_ctx,
            &original_packet,
        )
        .unwrap();
        conn2.inner.original_tuple = nated_original_packet.tuple.clone();
        let conn2 = Connection::Exclusive(conn2);

        // Fake NAT that ends up allocating the same original tuple as an
        // existing connection.
        let mut conn3 = ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(
            &bindings_ctx,
            &original_packet,
        )
        .unwrap();
        conn3.inner.reply_tuple = nated_original_packet.tuple.clone().invert();
        let conn3 = Connection::Exclusive(conn3);

        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn1), Ok((true, Some(_))));
        assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn2),
            Err(FinalizeConnectionError::Conflict)
        );
        assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn3),
            Err(FinalizeConnectionError::Conflict)
        );
    }

    #[ip_test(I)]
    fn table_conflict_identical_connection<
        I: IpExt + crate::packets::testutil::internal::TestIpExt,
    >() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        let original_packet =
            PacketMetadata::new(&FakeIpPacket::<I, FakeUdpPacket>::arbitrary_value()).unwrap();

        // Simulate a race where two packets in the same flow both end up
        // creating identical exclusive connections.

        let conn = Connection::Exclusive(
            ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(
                &bindings_ctx,
                &original_packet,
            )
            .unwrap(),
        );
        let finalized = assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn),
            Ok((true, Some(conn))) => conn
        );

        let conn = Connection::Exclusive(
            ConnectionExclusive::<_, _, ()>::from_deconstructed_packet(
                &bindings_ctx,
                &original_packet,
            )
            .unwrap(),
        );
        let conn = assert_matches!(
            table.finalize_connection(&mut bindings_ctx, conn),
            Ok((false, Some(conn))) => conn
        );
        assert!(Arc::ptr_eq(&finalized, &conn));
    }

    #[derive(Copy, Clone)]
    enum GcTrigger {
        /// Call [`perform_gc`] function directly, avoiding any timer logic.
        Direct,
        /// Trigger a timer expiry, which indirectly calls into [`perform_gc`].
        Timer,
    }

    #[ip_test(I)]
    #[test_case(GcTrigger::Direct)]
    #[test_case(GcTrigger::Timer)]
    fn garbage_collection<I: IpExt + TestIpExt>(gc_trigger: GcTrigger) {
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
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        };

        let second_packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT + 1, dst_port: I::DST_PORT },
        };
        let second_packet_reply = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeUdpPacket { src_port: I::DST_PORT, dst_port: I::SRC_PORT + 1 },
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
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(
            core_ctx
                .conntrack()
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"),
            (true, Some(_))
        );
        let conn = core_ctx
            .conntrack()
            .get_connection_for_packet_and_update(&bindings_ctx, &second_packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(
            core_ctx
                .conntrack()
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"),
            (true, Some(_))
        );
        assert!(core_ctx.conntrack().contains_tuple(&first_tuple));
        assert!(core_ctx.conntrack().contains_tuple(&second_tuple));
        assert_eq!(core_ctx.conntrack().inner.lock().num_connections, 2);

        // T=GC_INTERVAL: Triggering a GC does not clean up any connections,
        // because no connections are stale yet.
        bindings_ctx.sleep(GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().inner.lock().num_connections, 2);

        // T=GC_INTERVAL a packet for just the second connection comes in.
        let conn = core_ctx
            .conntrack()
            .get_connection_for_packet_and_update(&bindings_ctx, &second_packet_reply)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(conn.state().establishment_lifecycle, EstablishmentLifecycle::SeenReply);
        assert_matches!(
            core_ctx
                .conntrack()
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"),
            (false, Some(_))
        );
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().inner.lock().num_connections, 2);

        // The state in the table at this point is:
        // Connection 1:
        //   - Last packet seen at T=0
        //   - Expires after T=CONNECTION_EXPIRY_TIME_UDP
        // Connection 2:
        //   - Last packet seen at T=GC_INTERVAL
        //   - Expires after CONNECTION_EXPIRY_TIME_UDP + GC_INTERVAL

        // T=2*GC_INTERVAL: Triggering a GC does not clean up any connections.
        bindings_ctx.sleep(GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().inner.lock().num_connections, 2);

        // Time advances to expiry for the first packet
        // (T=CONNECTION_EXPIRY_TIME_UDP) trigger gc and note that the first
        // connection was cleaned up
        bindings_ctx.sleep(CONNECTION_EXPIRY_TIME_UDP - 2 * GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), true);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), true);
        assert_eq!(core_ctx.conntrack().inner.lock().num_connections, 1);

        // Advance time past the expiry time for the second connection
        // (T=CONNECTION_EXPIRY_TIME_UDP + GC_INTERVAL) and see that it is
        // cleaned up.
        bindings_ctx.sleep(GC_INTERVAL);
        perform_gc(&mut core_ctx, &mut bindings_ctx, gc_trigger);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&first_tuple_reply), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple), false);
        assert_eq!(core_ctx.conntrack().contains_tuple(&second_tuple_reply), false);
        assert_eq!(core_ctx.conntrack().inner.lock().num_connections, 0);
    }

    fn make_packets<I: IpExt + TestIpExt>(
        index: usize,
    ) -> (FakeIpPacket<I, FakeUdpPacket>, FakeIpPacket<I, FakeUdpPacket>) {
        // This ensures that, no matter what size MAXIMUM_CONNECTIONS is
        // (under 2^32, at least), we'll always have unique src and dst
        // ports, and thus unique connections.
        assert!(index < u32::MAX as usize);
        let src = (index % (u16::MAX as usize)) as u16;
        let dst = (index / (u16::MAX as usize)) as u16;

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: src, dst_port: dst },
        };
        let reply_packet = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeUdpPacket { src_port: dst, dst_port: src },
        };

        (packet, reply_packet)
    }

    #[ip_test(I)]
    #[test_case(true; "existing connections established")]
    #[test_case(false; "existing connections unestablished")]
    fn table_size_limit<I: IpExt + TestIpExt>(established: bool) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        bindings_ctx.sleep(Duration::from_secs(1));
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        // Fill up the table so that the next insertion will fail.
        for i in 0..MAXIMUM_CONNECTIONS {
            let (packet, reply_packet) = make_packets(i);
            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &packet)
                .expect("packet should be valid")
                .expect("packet should be trackable");
            assert_matches!(
                table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection finalize should succeed"),
                (true, Some(_))
            );

            // Whether to update the connection to be established by sending
            // through the reply packet.
            if established {
                let conn = table
                    .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
                    .expect("packet should be valid")
                    .expect("packet should be trackable");
                assert_matches!(
                    table
                        .finalize_connection(&mut bindings_ctx, conn)
                        .expect("connection finalize should succeed"),
                    (false, Some(_))
                );
            }
        }

        // The table should be full whether or not the connections are
        // established since finalize_connection always inserts the connection
        // under the original and reply tuples.
        assert_eq!(table.inner.lock().num_connections, MAXIMUM_CONNECTIONS);

        let (packet, _) = make_packets(MAXIMUM_CONNECTIONS);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
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
                .expect("packet should be valid")
                .expect("packet should be trackable");
            assert_matches!(
                table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection finalize should succeed"),
                (false, Some(_))
            );
        } else {
            assert_matches!(
                table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection finalize should succeed"),
                (true, Some(_))
            );
        }
    }

    #[cfg(target_os = "fuchsia")]
    #[ip_test(I)]
    fn inspect<I: IpExt + TestIpExt>() {
        use alloc::boxed::Box;
        use alloc::string::ToString;
        use netstack3_fuchsia::testutils::{assert_data_tree, Inspector};
        use netstack3_fuchsia::FuchsiaInspector;

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
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(conn.state().establishment_lifecycle, EstablishmentLifecycle::SeenOriginal);
        assert_matches!(
            table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"),
            (true, Some(_))
        );

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
                            "protocol": "UDP",
                            "src_addr": I::SRC_ADDR.to_string(),
                            "dst_addr": I::DST_ADDR.to_string(),
                            "src_port_or_id": 0u64,
                            "dst_port_or_id": 0u64,
                        },
                        "reply_tuple": {
                            "protocol": "UDP",
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
                .expect("packet should be valid")
                .expect("packet should be trackable");
            assert_matches!(
                table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection finalize should succeed"),
                (true, Some(_))
            );

            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
                .expect("packet should be valid")
                .unwrap();
            assert_matches!(
                table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection finalize should succeed"),
                (false, Some(_))
            );
        }

        assert_eq!(table.inner.lock().num_connections, MAXIMUM_CONNECTIONS);

        // This first one should succeed because it can evict the
        // non-established connection.
        let (packet, reply_packet) = make_packets(MAXIMUM_CONNECTIONS);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(
            table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"),
            (true, Some(_))
        );
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(
            table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection finalize should succeed"),
            (false, Some(_))
        );

        // This next one should fail because there are no connections left to
        // evict.
        let (packet, _) = make_packets(MAXIMUM_CONNECTIONS + 1);
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
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

    #[ip_test(I)]
    fn self_connected_socket<I: IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::SRC_PORT },
        };

        let tuple = Tuple::from_packet(&packet).expect("packet should be valid");
        let reply_tuple = tuple.clone().invert();

        assert_eq!(tuple, reply_tuple);

        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        let state = conn.state();
        // Since we can't differentiate between the original and reply tuple,
        // the connection ends up being marked established immediately.
        assert_matches!(state.establishment_lifecycle, EstablishmentLifecycle::SeenReply);

        assert_matches!(conn, Connection::Exclusive(_));
        assert!(!table.contains_tuple(&tuple));

        // Once we finalize the connection, it should be present in the map.
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok((true, Some(_))));
        assert!(table.contains_tuple(&tuple));

        // There should be a single connection in the table, despite there only
        // being a single tuple.
        assert_eq!(table.inner.lock().table.len(), 1);
        assert_eq!(table.inner.lock().num_connections, 1);

        bindings_ctx.sleep(CONNECTION_EXPIRY_TIME_UDP);
        table.perform_gc(&mut bindings_ctx);

        assert_eq!(table.inner.lock().table.len(), 0);
        assert_eq!(table.inner.lock().num_connections, 0);
    }

    #[ip_test(I)]
    fn remove_entry_on_update<I: IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        let original_packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeTcpSegment {
                src_port: I::SRC_PORT,
                dst_port: I::DST_PORT,
                segment: SegmentHeader {
                    seq: SeqNum::new(1024),
                    ack: None,
                    wnd: UnscaledWindowSize::from(16u16),
                    control: Some(Control::SYN),
                    options: Options::default(),
                },
                payload_len: 0,
            },
        };

        let reply_packet = FakeIpPacket::<I, _> {
            src_ip: I::DST_ADDR,
            dst_ip: I::SRC_ADDR,
            body: FakeTcpSegment {
                src_port: I::DST_PORT,
                dst_port: I::SRC_PORT,
                segment: SegmentHeader {
                    seq: SeqNum::new(0),
                    ack: Some(SeqNum::new(1025)),
                    wnd: UnscaledWindowSize::from(16u16),
                    control: Some(Control::RST),
                    options: Options::default(),
                },
                payload_len: 0,
            },
        };

        let tuple = Tuple::from_packet(&original_packet).expect("packet should be valid");
        let reply_tuple = tuple.clone().invert();

        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &original_packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok((true, Some(_))));

        assert!(table.contains_tuple(&tuple));
        assert!(table.contains_tuple(&reply_tuple));

        // Sending the reply RST through should result in the connection being
        // removed from the table.
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &reply_packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");

        assert!(!table.contains_tuple(&tuple));
        assert!(!table.contains_tuple(&reply_tuple));

        // The connection should not added back on finalization.
        assert_matches!(table.finalize_connection(&mut bindings_ctx, conn), Ok((false, Some(_))));

        assert!(!table.contains_tuple(&tuple));
        assert!(!table.contains_tuple(&reply_tuple));

        // GC should complete successfully.
        bindings_ctx.sleep(Duration::from_secs(60 * 60 * 24 * 6));
        table.perform_gc(&mut bindings_ctx);
    }

    #[ip_test(I)]
    fn do_not_insert<I: IpExt + TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::new();
        let table = Table::<_, _, ()>::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        let packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_ADDR,
            dst_ip: I::DST_ADDR,
            body: FakeUdpPacket { src_port: I::SRC_PORT, dst_port: I::DST_PORT },
        };

        let tuple = Tuple::from_packet(&packet).expect("packet should be valid");
        let reply_tuple = tuple.clone().invert();

        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        let mut conn = assert_matches!(conn, Connection::Exclusive(conn) => conn);
        conn.do_not_insert = true;
        assert_matches!(
            table.finalize_connection(&mut bindings_ctx, Connection::Exclusive(conn)),
            Ok((false, None))
        );

        assert!(!table.contains_tuple(&tuple));
        assert!(!table.contains_tuple(&reply_tuple));
    }
}
