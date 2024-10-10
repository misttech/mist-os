// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Network Address Translation.

use core::fmt::Debug;
use core::num::NonZeroU16;
use core::ops::{ControlFlow, RangeInclusive};

use log::{error, warn};
use net_types::SpecifiedAddr;
use netstack3_base::Inspectable;
use once_cell::sync::OnceCell;
use packet_formats::ip::IpExt;
use rand::Rng as _;

use crate::conntrack::{Connection, ConnectionDirection, Table, Tuple};
use crate::context::{FilterBindingsContext, FilterBindingsTypes, NatContext};
use crate::logic::{IngressVerdict, Interfaces, RoutineResult, Verdict};
use crate::packets::{IpPacket, MaybeTransportPacketMut as _, TransportPacketMut as _};
use crate::state::Hook;

/// The NAT configuration for a given conntrack connection.
///
/// NAT is configured exactly once for a given connection, for the first packet
/// encountered on that connection. This is not to say that all connections are
/// NATed: the configuration can be either `true` (NAT the connection) or
/// `false` (do not NAT), but the `OnceCell` containing the configuration
/// should always be initialized by the time a connection is inserted in the
/// conntrack table.
#[derive(Default, Debug)]
pub struct NatConfig {
    pub(crate) destination: OnceCell<bool>,
}

impl<I: IpExt, BT: FilterBindingsTypes> Connection<I, BT, NatConfig> {
    pub fn destination_nat(&self) -> bool {
        self.external_data().destination.get().copied().unwrap_or(false)
    }
}

/// The type of NAT that is performed on a given conntrack connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NatType {
    /// Destination NAT is performed on this connection.
    Destination,
    /// Source NAT is performed on this connection.
    //
    // TODO(https://fxbug.dev/341771631): support configuring SNAT for new
    // connections.
    //
    // Once we support SNAT of any kind, we will also need to remap source ports for
    // all non-NATed traffic by default to prevent locally-generated and forwarded
    // traffic from stepping on each other's toes.
    #[allow(dead_code)]
    Source,
}

impl Inspectable for NatConfig {
    fn record<I: netstack3_base::Inspector>(&self, inspector: &mut I) {
        let Self { destination } = self;
        let value = match destination.get() {
            None => "Unconfigured",
            Some(false) => "No-op",
            Some(true) => "Destination",
        };
        inspector.record_str("NAT", value);
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
                unreachable!("masquerade NAT is only valid in EGRESS hook; got {result:?}")
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
                unreachable!("masquerade NAT is only valid in EGRESS hook; got {result:?}")
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
        unreachable!("SNAT is not supported and should never be configured; got {result:?}")
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
        unreachable!("SNAT is not supported and should never be configured; got {result:?}")
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

    let NatConfig { destination } = conn.external_data();
    let relevant_config = match (N::NAT_TYPE, direction) {
        // If either this is a DNAT hook and we are looking at a packet in the
        // "original" direction, or this is an SNAT hook and we are looking at a reply
        // packet, then we want to decide whether to NAT based on whether the connection
        // has DNAT configured.
        (NatType::Destination, ConnectionDirection::Original)
        | (NatType::Source, ConnectionDirection::Reply) => destination,
        // TODO(https://fxbug.dev/341771631): when SNAT is supported, consult the SNAT
        // configuration here.
        (NatType::Source, ConnectionDirection::Original)
        | (NatType::Destination, ConnectionDirection::Reply) => return Verdict::Accept.into(),
    };

    let should_nat_conn = if let Some(nat) = relevant_config.get() {
        *nat
    } else {
        // NAT has not yet been configured for this connection; traverse the installed
        // NAT routines in order to configure NAT.
        let conn = match conn {
            Connection::Exclusive(conn) => conn,
            Connection::Shared(_) => {
                // NAT is configured for every connection before it is inserted in the conntrack
                // table, at which point it becomes a shared connection. (This is true whether
                // or not NAT will actually be performed; the configuration could be `None`.)
                unreachable!(
                    "connections always have NAT configured before they are inserted in conntrack"
                )
            }
        };
        match N::NAT_TYPE {
            NatType::Source => {
                // TODO(https://fxbug.dev/341771631): support SNAT.
                //
                // The way this function is written, we should really not be ending up this
                // branch, because:
                //  (1) NAT has not yet been configured for this connection, so this should be
                //      the first packet in the flow and therefore be in the original direction
                //  (2) We bail early at the top of the function if this is an SNAT hook and the
                //      packet is in the original direction.
                //
                // However, it is currently possible to end up in this branch when dealing with
                // self- connected flows. Typically, the first packet in a flow will be in the
                // original direction. However, when a connection's original tuple is the same
                // as its reply tuple, every packet on the connection is considered to be in the
                // reply direction, which is an implementation quirk that allows self-connected
                // flows to be considered immediately "established".
                //
                // This means that when we see the first packet in a self-connected flow in an
                // SNAT hook, no NAT will have been configured for it, and yet it would appear
                // to be a reply packet, so we would attempt to traverse the NAT routines to
                // configure SNAT, which is not yet supported. Instead, just configure the
                // connection not to be NATed, and bail.
                conn.external_data()
                    .destination
                    .set(false)
                    .expect("DNAT should not have been configured yet");
                return Verdict::Accept.into();
            }
            NatType::Destination => {}
        }
        let ConfigureNatResult { verdict, should_nat } = configure_nat::<N, _, _, _, _>(
            core_ctx,
            bindings_ctx,
            table,
            conn.reply_tuple_mut(),
            hook,
            packet,
            interfaces,
        );
        if let ControlFlow::Break(()) = verdict.behavior() {
            return verdict;
        }
        conn.external_data()
            .destination
            .set(should_nat)
            .expect("DNAT should not have been configured yet");
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
    dst_port: Option<RangeInclusive<NonZeroU16>>,
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

    let verdict = dst_port
        .map(|range| rewrite_tuple_src_port(bindings_ctx, table, reply_tuple, range))
        .unwrap_or(Verdict::Accept);

    ConfigureNatResult {
        verdict: verdict.into(),
        should_nat: match verdict {
            Verdict::Drop => false,
            Verdict::Accept => true,
        },
    }
}

/// Attempt to rewrite the source port of the provided tuple such that it fits
/// in the specified range and results in a new unique tuple.
fn rewrite_tuple_src_port<I: IpExt, BC: FilterBindingsContext>(
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    tuple: &mut Tuple<I>,
    port_range: RangeInclusive<NonZeroU16>,
) -> Verdict {
    // If the current source port is already in the specified range, and the
    // resulting reply tuple is already unique, then there is no need to change
    // the port.
    if NonZeroU16::new(tuple.src_port_or_id).map(|port| port_range.contains(&port)).unwrap_or(false)
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
        tuple.src_port_or_id = new_port.get();
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
                error!("cannot rewrite dst port to unspecified; dropping {proto} packet",);
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
                error!("cannot rewrite src port to unspecified; dropping {proto} packet",);
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

    trait NatHookExt<I: TestIpExt>: NatHook<I> {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId>;
    }

    impl<I: TestIpExt> NatHookExt<I> for IngressHook {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: Some(interface), egress: None }
        }
    }

    impl<I: TestIpExt> NatHookExt<I> for LocalEgressHook {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: None, egress: Some(interface) }
        }
    }

    impl<I: TestIpExt> NatHookExt<I> for EgressHook {
        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: None, egress: Some(interface) }
        }
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
    fn redirect<I: TestIpExt, Original: NatHookExt<I>, Reply: NatHookExt<I>>(
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
        assert_eq!(
            *conn.external_data().destination.get().expect("DNAT should be configured"),
            true
        );
        assert_eq!(conn.original_tuple(), &original);
        let mut reply = Tuple { src_addr: redirect_addr, ..original.invert() };
        if let Some(port) = dst_port {
            reply.src_port_or_id = port.get();
        }
        assert_eq!(conn.reply_tuple(), &reply);

        // When a reply to the original packet arrives at the corresponding hook, it
        // should have reverse DNAT applied, i.e. it's source should be rewritten to
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

    fn packet_with_src_port(src_port: u16) -> FakeIpPacket<Ipv4, FakeUdpPacket> {
        FakeIpPacket {
            body: FakeUdpPacket { src_port, ..ArbitraryValue::arbitrary_value() },
            ..ArbitraryValue::arbitrary_value()
        }
    }

    #[test]
    fn rewrite_src_port_noop_if_in_range() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let packet = packet_with_src_port(LOCAL_PORT.get());
        let mut tuple = Tuple::from_packet(&packet).unwrap();

        // If the port is already in the specified range, rewriting should succeed and
        // be a no-op.
        let original = tuple.clone();
        let verdict =
            rewrite_tuple_src_port(&mut bindings_ctx, &table, &mut tuple, LOCAL_PORT..=LOCAL_PORT);
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(tuple, original);
    }

    #[test]
    fn rewrite_src_port_succeeds_if_available_port_in_range() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let packet = packet_with_src_port(LOCAL_PORT.get());
        let mut tuple = Tuple::from_packet(&packet).unwrap();
        let original = tuple.clone();

        // If the port is not in the specified range, but there is an available port,
        // rewriting should succeed.
        const NEW_PORT: NonZeroU16 = const_unwrap_option(LOCAL_PORT.checked_add(1));
        let verdict =
            rewrite_tuple_src_port(&mut bindings_ctx, &table, &mut tuple, NEW_PORT..=NEW_PORT);
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(tuple, Tuple { src_port_or_id: NEW_PORT.get(), ..original });
    }

    #[test]
    fn rewrite_src_port_fails_if_no_available_port_in_range() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let packet = packet_with_src_port(LOCAL_PORT.get());
        let mut tuple = Tuple::from_packet(&packet).unwrap();

        // If there is no port available in the specified range that does not conflict
        // with a tuple already in the table, rewriting should fail and the packet
        // should be dropped.
        let conn = table
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
            .expect("packet should be valid")
            .expect("packet should be trackable");
        assert!(table
            .finalize_connection(&mut bindings_ctx, conn)
            .expect("connection should not conflict"));
        let verdict =
            rewrite_tuple_src_port(&mut bindings_ctx, &table, &mut tuple, LOCAL_PORT..=LOCAL_PORT);
        assert_eq!(verdict, Verdict::Drop);
    }

    #[test]
    fn src_port_rewritten_to_ensure_unique_tuple_even_if_in_range() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let table = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);

        // Fill the conntrack table with tuples such that there is only one tuple that
        // does not conflict with an existing one and which has a port in the specified
        // range.
        const MAX_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(LOCAL_PORT.get() + 100));
        for port in LOCAL_PORT.get()..=MAX_PORT.get() {
            let packet = packet_with_src_port(port);
            let conn = table
                .get_connection_for_packet_and_update(&bindings_ctx, &packet)
                .expect("packet should be valid")
                .expect("packet should be trackable");
            assert!(table
                .finalize_connection(&mut bindings_ctx, conn)
                .expect("connection should not conflict"));
        }

        // If the port is in the specified range, but results in a non-unique tuple,
        // rewriting should succeed as long as some port exists in the range that
        // results in a unique tuple.
        let packet = packet_with_src_port(LOCAL_PORT.get());
        let mut tuple = Tuple::from_packet(&packet).unwrap();
        let original = tuple.clone();
        const MIN_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(LOCAL_PORT.get() - 1));
        let verdict =
            rewrite_tuple_src_port(&mut bindings_ctx, &table, &mut tuple, MIN_PORT..=MAX_PORT);
        assert_eq!(verdict, Verdict::Accept);
        assert_eq!(tuple, Tuple { src_port_or_id: MIN_PORT.get(), ..original });
    }
}
