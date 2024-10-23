// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Network Address Translation.

use core::fmt::Debug;
use core::num::NonZeroU16;
use core::ops::{ControlFlow, RangeInclusive};

use log::{error, warn};
use net_types::SpecifiedAddr;
use netstack3_base::{Inspectable, Inspector as _};
use once_cell::sync::OnceCell;
use packet_formats::ip::IpExt;
use rand::Rng as _;

use crate::conntrack::{Connection, ConnectionDirection, Table, TransportProtocol, Tuple};
use crate::context::{FilterBindingsContext, FilterBindingsTypes, NatContext};
use crate::logic::{IngressVerdict, Interfaces, RoutineResult, Verdict};
use crate::packets::{IpPacket, MaybeTransportPacketMut as _, TransportPacketMut as _};
use crate::state::Hook;

/// The NAT configuration for a given conntrack connection.
///
/// Each type of NAT (source and destination) is configured exactly once for a
/// given connection, for the first packet encountered on that connection. This
/// is not to say that all connections are NATed: the configuration can be
/// either `true` (NAT the connection) or `false` (do not NAT), but the
/// `OnceCell` containing the configuration should always be initialized by the
/// time a connection is inserted in the conntrack table.
#[derive(Default, Debug)]
pub struct NatConfig {
    destination: OnceCell<bool>,
    source: OnceCell<bool>,
}

impl<I: IpExt, BT: FilterBindingsTypes> Connection<I, BT, NatConfig> {
    pub fn destination_nat(&self) -> bool {
        self.external_data().destination.get().copied().unwrap_or(false)
    }
}

/// A type of NAT that is performed on a given conntrack connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NatType {
    /// Destination NAT is performed.
    Destination,
    /// Source NAT is performed.
    Source,
}

impl Inspectable for NatConfig {
    fn record<I: netstack3_base::Inspector>(&self, inspector: &mut I) {
        let Self { source, destination } = self;
        let nat_type = |config: &OnceCell<bool>| match config.get() {
            None => "Unconfigured",
            Some(false) => "No-op",
            Some(true) => "NAT",
        };
        inspector.record_child("NAT", |inspector| {
            inspector.record_str("Destination", nat_type(destination));
            inspector.record_str("Source", nat_type(source));
        });
    }
}

pub(crate) trait NatHook<I: IpExt> {
    type Verdict: FilterVerdict;

    const NAT_TYPE: NatType;

    /// Evaluate the result of a given routine and returning the resulting control
    /// flow.
    fn evaluate_result<P, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, CC::DeviceId>,
        result: RoutineResult<I>,
    ) -> ControlFlow<ConfigureNatResult<Self::Verdict>>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BC>,
        BC: FilterBindingsContext;

    fn redirect_addr<P, CC, BT>(
        core_ctx: &mut CC,
        packet: &P,
        ingress: Option<&CC::DeviceId>,
    ) -> Option<I::Addr>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BT>,
        BT: FilterBindingsTypes;
}

pub(crate) trait FilterVerdict: From<Verdict> + Debug + PartialEq {
    fn behavior(&self) -> ControlFlow<()>;
}

impl FilterVerdict for Verdict {
    fn behavior(&self) -> ControlFlow<()> {
        match self {
            Self::Accept => ControlFlow::Continue(()),
            Self::Drop => ControlFlow::Break(()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct ConfigureNatResult<V> {
    verdict: V,
    should_nat: bool,
}

impl<V: From<Verdict>> ConfigureNatResult<V> {
    fn drop_packet() -> Self {
        Self { verdict: Verdict::Drop.into(), should_nat: false }
    }
}

pub(crate) enum IngressHook {}

impl<I: IpExt> NatHook<I> for IngressHook {
    type Verdict = IngressVerdict<I>;

    const NAT_TYPE: NatType = NatType::Destination;

    fn evaluate_result<P, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, CC::DeviceId>,
        result: RoutineResult<I>,
    ) -> ControlFlow<ConfigureNatResult<Self::Verdict>>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BC>,
        BC: FilterBindingsContext,
    {
        match result {
            RoutineResult::Accept | RoutineResult::Return => ControlFlow::Continue(()),
            RoutineResult::Drop => ControlFlow::Break(ConfigureNatResult::drop_packet()),
            RoutineResult::TransparentLocalDelivery { addr, port } => {
                ControlFlow::Break(ConfigureNatResult {
                    verdict: IngressVerdict::TransparentLocalDelivery { addr, port },
                    should_nat: false,
                })
            }
            RoutineResult::Redirect { dst_port } => {
                ControlFlow::Break(configure_redirect_nat::<Self, _, _, _, _>(
                    core_ctx,
                    bindings_ctx,
                    table,
                    reply_tuple,
                    packet,
                    interfaces,
                    dst_port,
                ))
            }
            result @ RoutineResult::Masquerade { .. } => {
                unreachable!("SNAT not supported in INGRESS; got {result:?}")
            }
        }
    }

    fn redirect_addr<P, CC, BT>(
        core_ctx: &mut CC,
        packet: &P,
        ingress: Option<&CC::DeviceId>,
    ) -> Option<I::Addr>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BT>,
        BT: FilterBindingsTypes,
    {
        let interface = ingress.expect("must have ingress interface in ingress hook");
        core_ctx
            .get_local_addr_for_remote(interface, SpecifiedAddr::new(packet.src_addr()))
            .map(|addr| addr.addr())
            .or_else(|| {
                warn!(
                    "cannot redirect because there is no address assigned to the incoming \
                    interface {interface:?}; dropping packet",
                );
                None
            })
    }
}

impl<I: IpExt> FilterVerdict for IngressVerdict<I> {
    fn behavior(&self) -> ControlFlow<()> {
        match self {
            Self::Verdict(v) => v.behavior(),
            Self::TransparentLocalDelivery { .. } => ControlFlow::Break(()),
        }
    }
}

pub(crate) enum LocalEgressHook {}

impl<I: IpExt> NatHook<I> for LocalEgressHook {
    type Verdict = Verdict;

    const NAT_TYPE: NatType = NatType::Destination;

    fn evaluate_result<P, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, CC::DeviceId>,
        result: RoutineResult<I>,
    ) -> ControlFlow<ConfigureNatResult<Self::Verdict>>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BC>,
        BC: FilterBindingsContext,
    {
        match result {
            RoutineResult::Accept | RoutineResult::Return => ControlFlow::Continue(()),
            RoutineResult::Drop => ControlFlow::Break(ConfigureNatResult::drop_packet()),
            result @ RoutineResult::TransparentLocalDelivery { .. } => {
                unreachable!(
                    "transparent local delivery is only valid in INGRESS hook; got {result:?}"
                )
            }
            result @ RoutineResult::Masquerade { .. } => {
                unreachable!("SNAT not supported in LOCAL_EGRESS; got {result:?}")
            }
            RoutineResult::Redirect { dst_port } => {
                ControlFlow::Break(configure_redirect_nat::<Self, _, _, _, _>(
                    core_ctx,
                    bindings_ctx,
                    table,
                    reply_tuple,
                    packet,
                    interfaces,
                    dst_port,
                ))
            }
        }
    }

    fn redirect_addr<P, CC, BT>(_: &mut CC, _: &P, _: Option<&CC::DeviceId>) -> Option<I::Addr>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BT>,
        BT: FilterBindingsTypes,
    {
        Some(*I::LOOPBACK_ADDRESS)
    }
}

pub(crate) enum LocalIngressHook {}

impl<I: IpExt> NatHook<I> for LocalIngressHook {
    type Verdict = Verdict;

    const NAT_TYPE: NatType = NatType::Source;

    fn evaluate_result<P, CC, BC>(
        _core_ctx: &mut CC,
        _bindings_ctx: &mut BC,
        _table: &Table<I, BC, NatConfig>,
        _reply_tuple: &mut Tuple<I>,
        _packet: &P,
        _interfaces: &Interfaces<'_, CC::DeviceId>,
        result: RoutineResult<I>,
    ) -> ControlFlow<ConfigureNatResult<Self::Verdict>>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BC>,
        BC: FilterBindingsContext,
    {
        match result {
            RoutineResult::Accept | RoutineResult::Return => ControlFlow::Continue(()),
            RoutineResult::Drop => ControlFlow::Break(ConfigureNatResult::drop_packet()),
            result @ RoutineResult::Masquerade { .. } => {
                unreachable!("Masquerade not supported in LOCAL_INGRESS; got {result:?}")
            }
            result @ RoutineResult::TransparentLocalDelivery { .. } => {
                unreachable!(
                    "transparent local delivery is only valid in INGRESS hook; got {result:?}"
                )
            }
            result @ RoutineResult::Redirect { .. } => {
                unreachable!("DNAT not supported in LOCAL_INGRESS; got {result:?}")
            }
        }
    }

    fn redirect_addr<P, CC, BT>(_: &mut CC, _: &P, _: Option<&CC::DeviceId>) -> Option<I::Addr>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BT>,
        BT: FilterBindingsTypes,
    {
        unreachable!("DNAT not supported in LOCAL_INGRESS; cannot perform redirect action")
    }
}

pub(crate) enum EgressHook {}

impl<I: IpExt> NatHook<I> for EgressHook {
    type Verdict = Verdict;

    const NAT_TYPE: NatType = NatType::Source;

    fn evaluate_result<P, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, CC::DeviceId>,
        result: RoutineResult<I>,
    ) -> ControlFlow<ConfigureNatResult<Self::Verdict>>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BC>,
        BC: FilterBindingsContext,
    {
        match result {
            RoutineResult::Accept | RoutineResult::Return => ControlFlow::Continue(()),
            RoutineResult::Drop => ControlFlow::Break(ConfigureNatResult::drop_packet()),
            RoutineResult::Masquerade { src_port } => {
                ControlFlow::Break(configure_masquerade_nat::<_, _, _, _>(
                    core_ctx,
                    bindings_ctx,
                    table,
                    reply_tuple,
                    packet,
                    interfaces,
                    src_port,
                ))
            }
            result @ RoutineResult::TransparentLocalDelivery { .. } => {
                unreachable!(
                    "transparent local delivery is only valid in INGRESS hook; got {result:?}"
                )
            }
            result @ RoutineResult::Redirect { .. } => {
                unreachable!("DNAT not supported in EGRESS; got {result:?}")
            }
        }
    }

    fn redirect_addr<P, CC, BT>(_: &mut CC, _: &P, _: Option<&CC::DeviceId>) -> Option<I::Addr>
    where
        P: IpPacket<I>,
        CC: NatContext<I, BT>,
        BT: FilterBindingsTypes,
    {
        unreachable!("DNAT not supported in EGRESS; cannot perform redirect action")
    }
}

impl<I: IpExt, BT: FilterBindingsTypes> Connection<I, BT, NatConfig> {
    fn relevant_config(
        &self,
        hook_nat_type: NatType,
        direction: ConnectionDirection,
    ) -> (&OnceCell<bool>, NatType) {
        let NatConfig { source, destination } = self.external_data();
        match (hook_nat_type, direction) {
            // If either this is a DNAT hook and we are looking at a packet in the
            // "original" direction, or this is an SNAT hook and we are looking at a reply
            // packet, then we want to decide whether to NAT based on whether the connection
            // has DNAT configured.
            (NatType::Destination, ConnectionDirection::Original)
            | (NatType::Source, ConnectionDirection::Reply) => (destination, NatType::Destination),
            // If either this is an SNAT hook and we are looking at a packet in the
            // "original" direction, or this is a DNAT hook and we are looking at a reply
            // packet, then we want to decide whether to NAT based on whether the connection
            // has SNAT configured.
            (NatType::Source, ConnectionDirection::Original)
            | (NatType::Destination, ConnectionDirection::Reply) => (source, NatType::Source),
        }
    }

    fn nat_config(&self, hook_nat_type: NatType, direction: ConnectionDirection) -> Option<bool> {
        let (config, _name) = self.relevant_config(hook_nat_type, direction);
        config.get().copied()
    }

    fn set_nat_config(
        &mut self,
        hook_nat_type: NatType,
        direction: ConnectionDirection,
        value: bool,
    ) -> Result<(), (bool, NatType)> {
        let (config, nat_type) = self.relevant_config(hook_nat_type, direction);
        config.set(value).map_err(|e| (e, nat_type))
    }
}

/// The entry point for NAT logic from an IP layer filtering hook.
///
/// This function configures NAT, if it has not yet been configured for the
/// connection, and performs NAT on the provided packet based on the
/// connection's NAT type.
pub(crate) fn perform_nat<N, I, P, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    conn: &mut Connection<I, BC, NatConfig>,
    hook: &Hook<I, BC::DeviceClass, ()>,
    packet: &mut P,
    interfaces: Interfaces<'_, CC::DeviceId>,
) -> N::Verdict
where
    N: NatHook<I>,
    I: IpExt,
    P: IpPacket<I>,
    CC: NatContext<I, BC>,
    BC: FilterBindingsContext,
{
    let Some(tuple) = Tuple::from_packet(packet) else {
        return Verdict::Accept.into();
    };
    let Some(direction) = conn.direction(&tuple) else {
        // If the packet does not match the connection, that likely means that it's been
        // NATed already.
        //
        // TODO(https://fxbug.dev/371017876): retrieve the packet's tuple and/or
        // direction from its IP layer metadata, once it is cached there, so that we can
        // reliably tell the direction of a packet with respect to its flow even after
        // it has had NAT performed on it.
        return Verdict::Accept.into();
    };

    let should_nat_conn = if let Some(nat) = conn.nat_config(N::NAT_TYPE, direction) {
        nat
    } else {
        // NAT has not yet been configured for this connection; traverse the installed
        // NAT routines in order to configure NAT.
        let ConfigureNatResult { verdict, should_nat } = match (&mut *conn, direction) {
            (Connection::Exclusive(_), ConnectionDirection::Reply) => {
                // This is the first packet in the flow (hence the connection being exclusive),
                // yet the packet is determined to be in the "reply" direction. This means that
                // this is a self-connected flow. When a connection's original tuple is the same
                // as its reply tuple, every packet on the connection is considered to be in the
                // reply direction, which is an implementation quirk that allows self-connected
                // flows to be considered immediately "established".
                //
                // Handle this by just configuring the connection not to be NATed. It does not
                // make sense to NAT a self-connected flow since the original and reply
                // directions are indistinguishable.
                ConfigureNatResult { verdict: Verdict::Accept.into(), should_nat: false }
            }
            (Connection::Shared(_), _) => {
                // TODO(https://fxbug.dev/371017876): once we cache the packet's tuple and/or
                // direction in its IP layer metadata, replace this logic with `unreachable!`.
                //
                // NAT ideally should be configured for every connection before it is inserted
                // in the conntrack table, at which point it becomes a shared connection. (This
                // should apply whether or not NAT will actually be performed; the configuration
                // could be `false`, i.e. "do not perform NAT for this connection".) However,
                // because the connection direction for a packet is recalculated at every NAT
                // hook, once a packet has had NAT performed on it, it may no longer match
                // either the original or reply tuple of the connection. In this case, we bail
                // at the top of this function.
                //
                // This can result in some connections not having both source and destination
                // NAT configured by the time they're finalized. To handle this, just configure
                // the connection not to be NATed.
                ConfigureNatResult { verdict: Verdict::Accept.into(), should_nat: false }
            }
            (Connection::Exclusive(conn), ConnectionDirection::Original) => {
                // TODO(https://fxbug.dev/368131272): remap source ports for all outgoing
                // traffic by default to prevent forwarded masqueraded traffic from clashing
                // with locally-generated traffic.
                configure_nat::<N, _, _, _, _>(
                    core_ctx,
                    bindings_ctx,
                    table,
                    conn.reply_tuple_mut(),
                    hook,
                    packet,
                    interfaces,
                )
            }
        };
        conn.set_nat_config(N::NAT_TYPE, direction, should_nat).unwrap_or_else(
            |(value, nat_type)| {
                unreachable!(
                    "{nat_type:?} NAT should not have been configured yet, but found {value:?}"
                )
            },
        );
        if let ControlFlow::Break(()) = verdict.behavior() {
            return verdict;
        }
        should_nat
    };

    if !should_nat_conn {
        return Verdict::Accept.into();
    }
    rewrite_packet(conn, direction, N::NAT_TYPE, packet).into()
}

/// Configure NAT by rewriting the provided reply tuple of a connection.
///
/// Evaluates the NAT routines at the provided hook and, on finding a rule that
/// matches the provided packet, configures NAT based on the rule's action. Note
/// that because NAT routines can contain a superset of the rules filter
/// routines can, it's possible for this packet to hit a non-NAT action.
fn configure_nat<N, I, P, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    reply_tuple: &mut Tuple<I>,
    hook: &Hook<I, BC::DeviceClass, ()>,
    packet: &P,
    interfaces: Interfaces<'_, CC::DeviceId>,
) -> ConfigureNatResult<N::Verdict>
where
    N: NatHook<I>,
    I: IpExt,
    P: IpPacket<I>,
    CC: NatContext<I, BC>,
    BC: FilterBindingsContext,
{
    let Hook { routines } = hook;
    for routine in routines {
        let result = super::check_routine(&routine, packet, &interfaces);
        match N::evaluate_result(
            core_ctx,
            bindings_ctx,
            table,
            reply_tuple,
            packet,
            &interfaces,
            result,
        ) {
            ControlFlow::Break(result) => return result,
            ControlFlow::Continue(()) => {}
        }
    }
    ConfigureNatResult { verdict: Verdict::Accept.into(), should_nat: false }
}

/// Configure Redirect NAT, a special case of DNAT that redirects the packet to
/// the local host.
fn configure_redirect_nat<N, I, P, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    reply_tuple: &mut Tuple<I>,
    packet: &P,
    interfaces: &Interfaces<'_, CC::DeviceId>,
    dst_port_range: Option<RangeInclusive<NonZeroU16>>,
) -> ConfigureNatResult<N::Verdict>
where
    N: NatHook<I>,
    I: IpExt,
    P: IpPacket<I>,
    CC: NatContext<I, BC>,
    BC: FilterBindingsContext,
{
    match N::NAT_TYPE {
        NatType::Source => panic!("DNAT action called from SNAT-only hook"),
        NatType::Destination => {}
    }

    // Choose an appropriate new destination address and, optionally, port. Then
    // rewrite the source address/port of the reply tuple for the connection to use
    // as the guide for future packet rewriting.
    //
    // If we are in INGRESS, use the primary address of the incoming interface; if
    // we are in LOCAL_EGRESS, use the loopback address.
    let Some(new_dst_addr) = N::redirect_addr(core_ctx, packet, interfaces.ingress) else {
        return ConfigureNatResult::drop_packet();
    };
    reply_tuple.src_addr = new_dst_addr;

    let verdict = dst_port_range
        .map(|range| {
            rewrite_tuple_port(bindings_ctx, table, reply_tuple, ReplyTuplePort::Source, range)
        })
        .unwrap_or(Verdict::Accept);

    ConfigureNatResult {
        verdict: verdict.into(),
        should_nat: match verdict {
            Verdict::Drop => false,
            Verdict::Accept => true,
        },
    }
}

/// Configure Masquerade NAT, a special case of SNAT that rewrites the source IP
/// address of the packet to an address that is assigned to the outgoing
/// interface.
fn configure_masquerade_nat<I, P, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    reply_tuple: &mut Tuple<I>,
    packet: &P,
    interfaces: &Interfaces<'_, CC::DeviceId>,
    src_port_range: Option<RangeInclusive<NonZeroU16>>,
) -> ConfigureNatResult<Verdict>
where
    I: IpExt,
    P: IpPacket<I>,
    CC: NatContext<I, BC>,
    BC: FilterBindingsContext,
{
    // Choose an appropriate new source address and, optionally, port. Then rewrite
    // the destination address/port of the reply tuple for the connection to use as
    // the guide for future packet rewriting.
    let interface = interfaces.egress.expect(
        "must have egress interface in EGRESS hook; Masquerade NAT is only valid in EGRESS",
    );
    let Some(new_src_addr) = core_ctx
        .get_local_addr_for_remote(interface, SpecifiedAddr::new(packet.dst_addr()))
        .map(|addr| addr.addr())
    else {
        // TODO(https://fxbug.dev/372549231): add a counter for this scenario.
        warn!(
            "cannot masquerade because there is no address assigned to the outgoing interface \
            {interface:?}; dropping packet",
        );
        return ConfigureNatResult::drop_packet();
    };
    reply_tuple.dst_addr = new_src_addr;

    // Rewrite the source port if necessary to avoid conflicting with existing
    // tracked connections. If a source port range was specified, rewrite into that
    // range; otherwise, attempt to rewrite into a "similar" range to the current
    // value.
    let range = src_port_range
        .or_else(|| rewrite_port_or_id_range(reply_tuple.protocol, reply_tuple.dst_port_or_id));
    let Some(range) = range else {
        return ConfigureNatResult::drop_packet();
    };
    let verdict =
        rewrite_tuple_port(bindings_ctx, table, reply_tuple, ReplyTuplePort::Destination, range);

    ConfigureNatResult {
        verdict,
        should_nat: match verdict {
            Verdict::Drop => false,
            Verdict::Accept => true,
        },
    }
}

/// Rewrite the transport-layer port or ID to a "similar" value -- that is, a
/// value that is likely to be a similar range to the original value with
/// respect to privilege (or lack thereof).
///
/// The heuristics used in this function are chosen to roughly match those used
/// by Netstack2/gVisor and Linux.
fn rewrite_port_or_id_range(
    protocol: TransportProtocol,
    port_or_id: u16,
) -> Option<RangeInclusive<NonZeroU16>> {
    match protocol {
        TransportProtocol::Tcp | TransportProtocol::Udp => Some(match port_or_id {
            _ if port_or_id < 512 => NonZeroU16::MIN..=NonZeroU16::new(511).unwrap(),
            _ if port_or_id < 1024 => NonZeroU16::MIN..=NonZeroU16::new(1023).unwrap(),
            _ => NonZeroU16::new(1024).unwrap()..=NonZeroU16::MAX,
        }),
        // TODO(https://fxbug.dev/341128580): allow rewriting ICMP echo ID to zero.
        TransportProtocol::Icmp => Some(NonZeroU16::MIN..=NonZeroU16::MAX),
        TransportProtocol::Other(p) => {
            error!(
                "cannot rewrite port or ID of unsupported transport protocol {p}; dropping packet"
            );
            None
        }
    }
}

#[derive(Clone, Copy)]
enum ReplyTuplePort {
    Source,
    Destination,
}

/// Attempt to rewrite the source port of the provided tuple such that it fits
/// in the specified range and results in a new unique tuple.
fn rewrite_tuple_port<I: IpExt, BC: FilterBindingsContext>(
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    tuple: &mut Tuple<I>,
    which_port: ReplyTuplePort,
    port_range: RangeInclusive<NonZeroU16>,
) -> Verdict {
    // If the current port is already in the specified range, and the resulting
    // reply tuple is already unique, then there is no need to change the port.
    let current_port = match which_port {
        ReplyTuplePort::Source => tuple.src_port_or_id,
        ReplyTuplePort::Destination => tuple.dst_port_or_id,
    };
    if NonZeroU16::new(current_port).map(|port| port_range.contains(&port)).unwrap_or(false)
        && !table.contains_tuple(&tuple)
    {
        return Verdict::Accept;
    }

    // Attempt to find a new port in the provided range that results in a unique
    // tuple. Start by selecting a random offset into the target range, and from
    // there increment the candidate port, wrapping around until all ports in
    // the range have been checked (or MAX_ATTEMPTS has been exceeded).
    //
    // As soon as we find a port that would result in a unique tuple, stop and
    // accept the packet. If we search the entire range and fail to find a port
    // that creates a unique tuple, drop the packet.
    const MAX_ATTEMPTS: u16 = 128;
    let len = port_range.end().get() - port_range.start().get() + 1;
    let mut rng = bindings_ctx.rng();
    let start = rng.gen_range(port_range.start().get()..=port_range.end().get());
    for i in 0..core::cmp::min(MAX_ATTEMPTS, len) {
        // `offset` is <= the size of `port_range`, which is a range of `NonZerou16`, so
        // `port_range.start()` + `offset` is guaranteed to fit in a `NonZeroU16`.
        let offset = (start + i) % len;
        let new_port = port_range.start().checked_add(offset).unwrap();
        let port_mut = match which_port {
            ReplyTuplePort::Source => &mut tuple.src_port_or_id,
            ReplyTuplePort::Destination => &mut tuple.dst_port_or_id,
        };
        *port_mut = new_port.get();
        if !table.contains_tuple(&tuple) {
            return Verdict::Accept;
        }
    }

    Verdict::Drop
}

/// Perform NAT on a packet, using its connection in the conntrack table as a
/// guide.
fn rewrite_packet<I, P, BT>(
    conn: &Connection<I, BT, NatConfig>,
    direction: ConnectionDirection,
    nat: NatType,
    packet: &mut P,
) -> Verdict
where
    I: IpExt,
    P: IpPacket<I>,
    BT: FilterBindingsTypes,
{
    // If this packet is in the "original" direction of the connection, rewrite its
    // address and port from the connection's reply tuple. If this is a reply
    // packet, rewrite it using the *original* tuple so the traffic is seen to
    // originate from or be destined to the expected peer.
    //
    // The reply tuple functions both as a way to mark what we expect to see coming
    // in, *and* a way to stash what the NAT remapping should be.
    let tuple = match direction {
        ConnectionDirection::Original => conn.reply_tuple(),
        ConnectionDirection::Reply => conn.original_tuple(),
    };
    match nat {
        NatType::Destination => {
            let (new_dst_addr, new_dst_port) = (tuple.src_addr, tuple.src_port_or_id);

            packet.set_dst_addr(new_dst_addr);
            let proto = packet.protocol();
            let mut transport = packet.transport_packet_mut();
            let Some(mut transport) = transport.transport_packet_mut() else {
                return Verdict::Accept;
            };
            let Some(new_dst_port) = NonZeroU16::new(new_dst_port) else {
                // TODO(https://fxbug.dev/341128580): allow rewriting port to zero if allowed by
                // the transport-layer protocol.
                error!("cannot rewrite dst port to unspecified; dropping {proto} packet");
                return Verdict::Drop;
            };
            transport.set_dst_port(new_dst_port);
        }
        NatType::Source => {
            let (new_src_addr, new_src_port) = (tuple.dst_addr, tuple.dst_port_or_id);

            packet.set_src_addr(new_src_addr);
            let proto = packet.protocol();
            let mut transport = packet.transport_packet_mut();
            let Some(mut transport) = transport.transport_packet_mut() else {
                return Verdict::Accept;
            };
            let Some(new_src_port) = NonZeroU16::new(new_src_port) else {
                // TODO(https://fxbug.dev/341128580): allow rewriting port to zero if allowed by
                // the transport-layer protocol.
                error!("cannot rewrite src port to unspecified; dropping {proto} packet");
                return Verdict::Drop;
            };
            transport.set_src_port(new_src_port);
        }
    }
    Verdict::Accept
}

#[cfg(test)]
mod tests {
    use alloc::collections::HashMap;
    use alloc::vec;
    use core::marker::PhantomData;

    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_types::ip::Ipv4;
    use netstack3_base::{IntoCoreTimerCtx, IpDeviceAddr};
    use test_case::test_case;

    use super::*;
    use crate::context::testutil::{FakeBindingsCtx, FakeNatCtx};
    use crate::matchers::testutil::{ethernet_interface, FakeDeviceId};
    use crate::matchers::PacketMatcher;
    use crate::packets::testutil::internal::{
        ArbitraryValue, FakeIpPacket, FakeUdpPacket, TestIpExt,
    };
    use crate::state::{Action, Routine, Rule, TransparentProxy};

    impl<V: From<Verdict>> ConfigureNatResult<V> {
        fn accept_packet() -> Self {
            Self { verdict: Verdict::Accept.into(), should_nat: false }
        }
    }

    #[test]
    fn accept_by_default_if_no_matching_rules_in_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &Hook::default(),
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult::accept_packet()
        );
    }

    #[test]
    fn accept_by_default_if_return_from_routine() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let hook = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(PacketMatcher::default(), Action::Return)],
            }],
        };
        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult::accept_packet()
        );
    }

    #[test]
    fn accept_terminal_for_installed_routine() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        // The first installed routine should terminate at its `Accept` result.
        let routine = Routine {
            rules: vec![
                // Accept all traffic.
                Rule::new(PacketMatcher::default(), Action::Accept),
                // Drop all traffic.
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };
        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &Hook { routines: vec![routine.clone()] },
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult::accept_packet()
        );

        // The first installed routine should terminate at its `Accept` result, but the
        // hook should terminate at the `Drop` result in the second routine.
        let hook = Hook {
            routines: vec![
                routine,
                Routine {
                    rules: vec![
                        // Drop all traffic.
                        Rule::new(PacketMatcher::default(), Action::Drop),
                    ],
                },
            ],
        };
        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult::drop_packet()
        );
    }

    #[test]
    fn drop_terminal_for_entire_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let hook = Hook {
            routines: vec![
                Routine {
                    rules: vec![
                        // Drop all traffic.
                        Rule::new(PacketMatcher::default(), Action::Drop),
                    ],
                },
                Routine {
                    rules: vec![
                        // Accept all traffic.
                        Rule::new(PacketMatcher::default(), Action::Accept),
                    ],
                },
            ],
        };

        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult::drop_packet()
        );
    }

    #[test]
    fn transparent_proxy_terminal_for_entire_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let ingress = Hook {
            routines: vec![
                Routine {
                    rules: vec![Rule::new(
                        PacketMatcher::default(),
                        Action::TransparentProxy(TransparentProxy::LocalPort(LOCAL_PORT)),
                    )],
                },
                Routine {
                    rules: vec![
                        // Accept all traffic.
                        Rule::new(PacketMatcher::default(), Action::Accept),
                    ],
                },
            ],
        };

        assert_eq!(
            configure_nat::<IngressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &ingress,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult {
                verdict: IngressVerdict::TransparentLocalDelivery {
                    addr: Ipv4::DST_IP,
                    port: LOCAL_PORT
                },
                should_nat: false,
            }
        );
    }

    #[test]
    fn redirect_terminal_for_entire_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let hook = Hook {
            routines: vec![
                Routine {
                    rules: vec![
                        // Redirect all traffic.
                        Rule::new(PacketMatcher::default(), Action::Redirect { dst_port: None }),
                    ],
                },
                Routine {
                    rules: vec![
                        // Drop all traffic.
                        Rule::new(PacketMatcher::default(), Action::Drop),
                    ],
                },
            ],
        };

        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            ConfigureNatResult { verdict: Verdict::Accept.into(), should_nat: true }
        );
    }

    #[ip_test(I)]
    fn masquerade_terminal_for_entire_hook<I: TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx {
            device_addrs: HashMap::from([(
                ethernet_interface(),
                IpDeviceAddr::new(I::DST_IP_2).unwrap(),
            )]),
        };
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let hook = Hook {
            routines: vec![
                Routine {
                    rules: vec![
                        // Masquerade all traffic.
                        Rule::new(PacketMatcher::default(), Action::Masquerade { src_port: None }),
                    ],
                },
                Routine {
                    rules: vec![
                        // Drop all traffic.
                        Rule::new(PacketMatcher::default(), Action::Drop),
                    ],
                },
            ],
        };

        assert_eq!(
            configure_nat::<EgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: Some(&ethernet_interface()) },
            ),
            ConfigureNatResult { verdict: Verdict::Accept.into(), should_nat: true }
        );
    }

    #[test]
    fn redirect_ingress_drops_packet_if_no_assigned_address() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let hook = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(
                    PacketMatcher::default(),
                    Action::Redirect { dst_port: None },
                )],
            }],
        };

        assert_eq!(
            configure_nat::<IngressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: Some(&ethernet_interface()), egress: None },
            ),
            ConfigureNatResult::drop_packet()
        );
    }

    #[test]
    fn masquerade_egress_drops_packet_if_no_assigned_address() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();

        let hook = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(
                    PacketMatcher::default(),
                    Action::Masquerade { src_port: None },
                )],
            }],
        };

        assert_eq!(
            configure_nat::<EgressHook, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: Some(&ethernet_interface()) },
            ),
            ConfigureNatResult::drop_packet()
        );
    }

    trait NatHookExt {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId>;
    }

    impl NatHookExt for IngressHook {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: Some(interface), egress: None }
        }
    }

    impl NatHookExt for LocalEgressHook {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: None, egress: Some(interface) }
        }
    }

    impl NatHookExt for EgressHook {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: None, egress: Some(interface) }
        }
    }

    #[ip_test(I)]
    fn nat_disabled_for_self_connected_flows<I: TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();

        let mut packet = FakeIpPacket::<I, _> {
            src_ip: I::SRC_IP,
            dst_ip: I::SRC_IP,
            body: FakeUdpPacket { src_port: 22222, dst_port: 22222 },
        };
        let mut conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");

        // Even with a Redirect NAT rule in LOCAL_EGRESS, and a Masquerade NAT rule in
        // EGRESS, DNAT and SNAT should both be disabled for the connection because it
        // is self-connected.
        let verdict = perform_nat::<LocalEgressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &Hook {
                routines: vec![Routine {
                    rules: vec![Rule::new(
                        PacketMatcher::default(),
                        Action::Redirect { dst_port: None },
                    )],
                }],
            },
            &mut packet,
            LocalEgressHook::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());

        let verdict = perform_nat::<EgressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &Hook {
                routines: vec![Routine {
                    rules: vec![Rule::new(
                        PacketMatcher::default(),
                        Action::Masquerade { src_port: None },
                    )],
                }],
            },
            &mut packet,
            EgressHook::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());

        assert_eq!(conn.external_data().destination.get().copied(), Some(false));
        assert_eq!(conn.external_data().source.get().copied(), Some(false));
    }

    #[ip_test(I)]
    fn nat_disabled_if_not_configured_before_connection_finalized<I: TestIpExt>() {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();

        // With a Redirect NAT rule configured in LOCAL_EGRESS, send an outgoing packet
        // through LOCAL_EGRESS and EGRESS, and finalize the packet's connection in
        // conntrack.
        let mut packet = FakeIpPacket::<I, FakeUdpPacket>::arbitrary_value();
        let mut conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");

        let nat_routines = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(
                    PacketMatcher::default(),
                    Action::Redirect { dst_port: None },
                )],
            }],
        };
        let verdict = perform_nat::<LocalEgressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut packet,
            LocalEgressHook::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());
        assert_eq!(conn.external_data().destination.get().copied(), Some(true));
        assert_eq!(conn.external_data().source.get().copied(), None);

        // DNAT was configured in the LOCAL_EGRESS hook, but SNAT is not configured as
        // it typically should be in EGRESS because the post-NAT packet no longer
        // matches its connection, so it's ignored.
        //
        // TODO(https://fxbug.dev/371017876): expect both DNAT and SNAT to be
        // configured.
        let verdict = perform_nat::<EgressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &Hook::default(),
            &mut packet,
            EgressHook::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());
        assert_eq!(conn.external_data().destination.get().copied(), Some(true));
        assert_eq!(conn.external_data().source.get().copied(), None);

        let (inserted, _weak) = conntrack
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection should not conflict");
        assert!(inserted);

        // Now, when a reply comes in to INGRESS, expect that SNAT will be configured as
        // `false` given it has not already been configured for the connection.
        let mut reply = packet.reply();
        let mut conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &reply)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        let verdict = perform_nat::<IngressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &Hook::default(),
            &mut reply,
            IngressHook::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());
        assert_eq!(conn.external_data().destination.get().copied(), Some(true));
        assert_eq!(conn.external_data().source.get().copied(), Some(false));
    }

    const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(55555));

    #[ip_test(I)]
    #[test_case(
        PhantomData::<IngressHook>, PhantomData::<EgressHook>, None;
        "redirect INGRESS"
    )]
    #[test_case(
        PhantomData::<IngressHook>, PhantomData::<EgressHook>, Some(LOCAL_PORT);
        "redirect INGRESS to local port"
    )]
    #[test_case(
        PhantomData::<LocalEgressHook>, PhantomData::<EgressHook>, None;
        "redirect LOCAL_EGRESS"
    )]
    #[test_case(
        PhantomData::<LocalEgressHook>, PhantomData::<EgressHook>, Some(LOCAL_PORT);
        "redirect LOCAL_EGRESS to local port"
    )]
    fn redirect<I: TestIpExt, Original: NatHookExt + NatHook<I>, Reply: NatHookExt + NatHook<I>>(
        _original_nat_hook: PhantomData<Original>,
        _reply_nat_hook: PhantomData<Reply>,
        dst_port: Option<NonZeroU16>,
    ) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx {
            device_addrs: HashMap::from([(
                ethernet_interface(),
                IpDeviceAddr::new(I::DST_IP_2).unwrap(),
            )]),
        };

        // Create a packet and get the corresponding connection from conntrack.
        let mut packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();
        let pre_nat_packet = packet.clone();
        let mut conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        let original = conn.original_tuple().clone();

        // Perform NAT at the first hook where we'd encounter this packet (either
        // INGRESS, if it's an incoming packet, or LOCAL_EGRESS, if it's an outgoing
        // packet).
        let nat_routines = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(
                    PacketMatcher::default(),
                    Action::Redirect { dst_port: dst_port.map(|port| port..=port) },
                )],
            }],
        };
        let verdict = perform_nat::<Original, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut packet,
            Original::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());

        // The packet's destination should be rewritten, and DNAT should be configured
        // for the packet; the reply tuple's source should be rewritten to match the new
        // destination.
        let redirect_addr = Original::redirect_addr(
            &mut core_ctx,
            &packet,
            Original::interfaces(&ethernet_interface()).ingress,
        )
        .expect("get redirect addr for NAT hook");
        let expected = FakeIpPacket::<_, FakeUdpPacket> {
            src_ip: packet.src_ip,
            dst_ip: redirect_addr,
            body: FakeUdpPacket {
                src_port: packet.body.src_port,
                dst_port: dst_port.map(NonZeroU16::get).unwrap_or(packet.body.dst_port),
            },
        };
        assert_eq!(packet, expected);
        assert!(conn.external_data().destination.get().expect("DNAT should be configured"));
        assert_eq!(conn.external_data().source.get(), None, "SNAT should not be configured");
        assert_eq!(conn.original_tuple(), &original);
        let mut reply = Tuple { src_addr: redirect_addr, ..original.invert() };
        if let Some(port) = dst_port {
            reply.src_port_or_id = port.get();
        }
        assert_eq!(conn.reply_tuple(), &reply);

        // When a reply to the original packet arrives at the corresponding hook, it
        // should have reverse DNAT applied, i.e. its source should be rewritten to
        // match the original destination of the connection.
        let mut reply_packet = packet.reply();
        // Install a NAT routine that simply drops all packets. This should have no
        // effect, because only the first packet for a given connection traverses NAT
        // routines.
        let nat_routines = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(PacketMatcher::default(), Action::Drop)],
            }],
        };
        let verdict = perform_nat::<Reply, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut reply_packet,
            Reply::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());
        assert_eq!(reply_packet, pre_nat_packet.reply());
    }

    #[ip_test(I)]
    #[test_case(None; "masquerade")]
    #[test_case(Some(LOCAL_PORT); "masquerade to specified port")]
    fn masquerade<I: TestIpExt>(src_port: Option<NonZeroU16>) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx {
            device_addrs: HashMap::from([(
                ethernet_interface(),
                IpDeviceAddr::new(I::SRC_IP_2).unwrap(),
            )]),
        };

        // Create a packet and get the corresponding connection from conntrack.
        let mut packet = FakeIpPacket::<_, FakeUdpPacket>::arbitrary_value();
        let pre_nat_packet = packet.clone();
        let mut conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        let original = conn.original_tuple().clone();

        // Perform Masquerade NAT at EGRESS.
        let nat_routines = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(
                    PacketMatcher::default(),
                    Action::Masquerade { src_port: src_port.map(|port| port..=port) },
                )],
            }],
        };
        let verdict = perform_nat::<EgressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut packet,
            Interfaces { ingress: None, egress: Some(&ethernet_interface()) },
        );
        assert_eq!(verdict, Verdict::Accept.into());

        // The packet's source address should be rewritten, and SNAT should be
        // configured for the packet; the reply tuple's destination should be rewritten
        // to match the new source.
        let expected = FakeIpPacket::<_, FakeUdpPacket> {
            src_ip: I::SRC_IP_2,
            dst_ip: packet.dst_ip,
            body: FakeUdpPacket {
                src_port: src_port.map(NonZeroU16::get).unwrap_or(packet.body.src_port),
                dst_port: packet.body.dst_port,
            },
        };
        assert_eq!(packet, expected);
        assert!(conn.external_data().source.get().expect("SNAT should be configured"));
        assert_eq!(conn.external_data().destination.get(), None, "DNAT should not be configured");
        assert_eq!(conn.original_tuple(), &original);
        let mut reply = Tuple { dst_addr: I::SRC_IP_2, ..original.invert() };
        if let Some(port) = src_port {
            reply.dst_port_or_id = port.get();
        }
        assert_eq!(conn.reply_tuple(), &reply);

        // When a reply to the original packet arrives at INGRESS, it should have
        // reverse SNAT applied, i.e. its destination should be rewritten to match the
        // original source of the connection.
        let mut reply_packet = packet.reply();
        // Install a NAT routine that simply drops all packets. This should have no
        // effect, because only the first packet for a given connection traverses NAT
        // routines.
        let nat_routines = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(PacketMatcher::default(), Action::Drop)],
            }],
        };
        let verdict = perform_nat::<IngressHook, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut reply_packet,
            Interfaces { ingress: Some(&ethernet_interface()), egress: None },
        );
        assert_eq!(verdict, Verdict::Accept.into());
        assert_eq!(reply_packet, pre_nat_packet.reply());
    }

    #[ip_test(I)]
    #[test_case(22, 1..=511)]
    #[test_case(853, 1..=1023)]
    #[test_case(11111, 1024..=u16::MAX)]
    fn masquerade_reply_tuple_dst_port_rewritten_even_if_target_range_unspecified<I: TestIpExt>(
        src_port: u16,
        expected_range: RangeInclusive<u16>,
    ) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx {
            device_addrs: HashMap::from([(
                ethernet_interface(),
                IpDeviceAddr::new(I::SRC_IP_2).unwrap(),
            )]),
        };
        let packet = FakeIpPacket {
            body: FakeUdpPacket { src_port, ..ArbitraryValue::arbitrary_value() },
            ..ArbitraryValue::arbitrary_value()
        };

        // First, insert a connection in conntrack with the same the source address the
        // packet will be masqueraded to, and the same source port, to cause a conflict.
        let reply = FakeIpPacket { src_ip: I::SRC_IP_2, ..packet.clone() };
        let conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &reply)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(
            conntrack
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection should not conflict"),
            (true, Some(_))
        );

        // Now, configure Masquerade NAT for a new connection that conflicts with the
        // existing one, but do not specify a port range to which the source port should
        // be rewritten.
        let mut reply_tuple = Tuple::from_packet(&packet).unwrap().invert();
        let ConfigureNatResult { verdict, should_nat } = configure_masquerade_nat(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut reply_tuple,
            &packet,
            &Interfaces { ingress: None, egress: Some(&ethernet_interface()) },
            /* src_port */ None,
        );

        // The destination address of the reply tuple should have been rewritten to the
        // new source address, and the destination port should also have been rewritten
        // (to a "similar" value), even though a rewrite range was not specified,
        // because otherwise it would conflict with the existing connection in the
        // table.
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(should_nat, true);
        assert_eq!(reply_tuple.dst_addr, I::SRC_IP_2);
        assert_ne!(reply_tuple.dst_port_or_id, src_port);
        assert!(expected_range.contains(&reply_tuple.dst_port_or_id));
    }

    fn packet_with_port(which: ReplyTuplePort, port: u16) -> FakeIpPacket<Ipv4, FakeUdpPacket> {
        let mut packet = FakeIpPacket::<Ipv4, FakeUdpPacket>::arbitrary_value();
        match which {
            ReplyTuplePort::Source => packet.body.src_port = port,
            ReplyTuplePort::Destination => packet.body.dst_port = port,
        }
        packet
    }

    fn tuple_with_port(which: ReplyTuplePort, port: u16) -> Tuple<Ipv4> {
        Tuple::from_packet(&packet_with_port(which, port)).unwrap()
    }

    #[test_case(ReplyTuplePort::Source)]
    #[test_case(ReplyTuplePort::Destination)]
    fn rewrite_port_noop_if_in_range(which: ReplyTuplePort) {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut tuple = tuple_with_port(which, LOCAL_PORT.get());

        // If the port is already in the specified range, rewriting should succeed and
        // be a no-op.
        let original = tuple.clone();
        let verdict = rewrite_tuple_port(
            &mut bindings_ctx,
            &table,
            &mut tuple,
            which,
            LOCAL_PORT..=LOCAL_PORT,
        );
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(tuple, original);
    }

    #[test_case(ReplyTuplePort::Source)]
    #[test_case(ReplyTuplePort::Destination)]
    fn rewrite_port_succeeds_if_available_port_in_range(which: ReplyTuplePort) {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut tuple = tuple_with_port(which, LOCAL_PORT.get());

        // If the port is not in the specified range, but there is an available port,
        // rewriting should succeed.
        const NEW_PORT: NonZeroU16 = const_unwrap_option(LOCAL_PORT.checked_add(1));
        let verdict =
            rewrite_tuple_port(&mut bindings_ctx, &table, &mut tuple, which, NEW_PORT..=NEW_PORT);
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(tuple, tuple_with_port(which, NEW_PORT.get()));
    }

    #[test_case(ReplyTuplePort::Source)]
    #[test_case(ReplyTuplePort::Destination)]
    fn rewrite_port_fails_if_no_available_port_in_range(which: ReplyTuplePort) {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        // If there is no port available in the specified range that does not conflict
        // with a tuple already in the table, rewriting should fail and the packet
        // should be dropped.
        let packet = packet_with_port(which, LOCAL_PORT.get());
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert_matches!(
            table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection should not conflict"),
            (true, Some(_))
        );

        let mut tuple = tuple_with_port(which, LOCAL_PORT.get());
        let verdict = rewrite_tuple_port(
            &mut bindings_ctx,
            &table,
            &mut tuple,
            which,
            LOCAL_PORT..=LOCAL_PORT,
        );
        assert_eq!(verdict, Verdict::Drop);
    }

    #[test_case(ReplyTuplePort::Source)]
    #[test_case(ReplyTuplePort::Destination)]
    fn port_rewritten_to_ensure_unique_tuple_even_if_in_range(which: ReplyTuplePort) {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        // Fill the conntrack table with tuples such that there is only one tuple that
        // does not conflict with an existing one and which has a port in the specified
        // range.
        const MAX_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(LOCAL_PORT.get() + 100));
        for port in LOCAL_PORT.get()..=MAX_PORT.get() {
            let packet = packet_with_port(which, port);
            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &packet)
                .expect("packet should be valid")
                .expect("packet should be trackable");
            assert_matches!(
                table
                    .finalize_connection(&mut bindings_ctx, conn)
                    .expect("connection should not conflict"),
                (true, Some(_))
            );
        }

        // If the port is in the specified range, but results in a non-unique tuple,
        // rewriting should succeed as long as some port exists in the range that
        // results in a unique tuple.
        let mut tuple = tuple_with_port(which, LOCAL_PORT.get());
        const MIN_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(LOCAL_PORT.get() - 1));
        let verdict =
            rewrite_tuple_port(&mut bindings_ctx, &table, &mut tuple, which, MIN_PORT..=MAX_PORT);
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(tuple, tuple_with_port(which, MIN_PORT.get()));
    }
}
