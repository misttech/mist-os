// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Network Address Translation.

// TODO(https://fxbug.dev/333419001): call into this module from the IP
// filtering hooks.
#![allow(dead_code)]

use core::{
    fmt::Debug,
    num::NonZeroU16,
    ops::{ControlFlow, RangeInclusive},
};

use net_types::{SpecifiedAddr, Witness as _};
use netstack3_base::Inspectable;
use once_cell::sync::OnceCell;
use packet_formats::ip::IpExt;
use rand::Rng as _;
use tracing::error;

use crate::{
    conntrack::{Connection, ConnectionDirection, Table, Tuple},
    context::{FilterBindingsContext, FilterBindingsTypes, NatContext},
    logic::{IngressVerdict, Interfaces, RoutineResult, Verdict},
    matchers::InterfaceProperties,
    packets::{IpPacket, MaybeTransportPacketMut as _, TransportPacketMut as _},
    state::Hook,
};

#[derive(Default)]
pub struct NatConfig {
    /// NAT is configured exactly once for a given connection, for the first
    /// packet encountered on that connection. This is not to say that all
    /// connections are NATed: the configuration could be `None`, but this field
    /// will always be initialized by the time a connection is inserted in the
    /// conntrack table.
    config: OnceCell<Option<NatType>>,
}

// TODO(https://fxbug.dev/341771631): perform SNAT.
//
// Once we support SNAT of any kind, we will also need to remap source ports for
// all non-NATed traffic by default to prevent locally-generated and forwarded
// traffic from stepping on each other's toes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum NatType {
    Destination,
}

impl Inspectable for NatConfig {
    fn record<I: netstack3_base::Inspector>(&self, inspector: &mut I) {
        inspector.record_debug("NAT", self.config.get())
    }
}

pub(crate) trait NatHook<I: IpExt> {
    type Verdict: FilterVerdict;

    const NAT_TYPE: NatType;

    /// Evaluate the result of a given routine and returning the resulting control
    /// flow.
    fn evaluate_result<P, D, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, D>,
        result: RoutineResult<I>,
    ) -> ControlFlow<(Self::Verdict, Option<NatType>)>
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BC::DeviceClass>,
        CC: NatContext<I, DeviceId = D>,
        BC: FilterBindingsContext;

    fn redirect_addr<P, D, CC>(
        core_ctx: &mut CC,
        packet: &P,
        ingress: Option<&D>,
    ) -> Option<I::Addr>
    where
        P: IpPacket<I>,
        CC: NatContext<I, DeviceId = D>;
}

pub(crate) trait FilterVerdict: From<Verdict> + Debug + PartialEq {
    fn behavior(&self) -> ControlFlow<()>;
}

pub(crate) enum IngressHook {}

impl<I: IpExt> NatHook<I> for IngressHook {
    type Verdict = IngressVerdict<I>;

    const NAT_TYPE: NatType = NatType::Destination;

    fn evaluate_result<P, D, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, D>,
        result: RoutineResult<I>,
    ) -> ControlFlow<(Self::Verdict, Option<NatType>)>
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BC::DeviceClass>,
        CC: NatContext<I, DeviceId = D>,
        BC: FilterBindingsContext,
    {
        match result {
            RoutineResult::Accept | RoutineResult::Return => ControlFlow::Continue(()),
            RoutineResult::Drop => ControlFlow::Break((Verdict::Drop.into(), None)),
            RoutineResult::TransparentLocalDelivery { addr, port } => {
                ControlFlow::Break((IngressVerdict::TransparentLocalDelivery { addr, port }, None))
            }
            RoutineResult::Redirect { dst_port } => {
                let (verdict, nat_type) = configure_redirect_nat::<_, _, _, _, _, Self>(
                    core_ctx,
                    bindings_ctx,
                    table,
                    reply_tuple,
                    packet,
                    interfaces,
                    dst_port,
                );
                ControlFlow::Break((verdict.into(), nat_type))
            }
        }
    }

    fn redirect_addr<P, D, CC>(
        core_ctx: &mut CC,
        packet: &P,
        ingress: Option<&D>,
    ) -> Option<I::Addr>
    where
        I: IpExt,
        P: IpPacket<I>,
        CC: NatContext<I, DeviceId = D>,
    {
        let interface = ingress.expect("must have ingress interface in ingress hook");
        core_ctx
            .get_local_addr_for_remote(interface, SpecifiedAddr::new(packet.src_addr()))
            .map(|addr| addr.get())
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

    fn evaluate_result<P, D, CC, BC>(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        table: &Table<I, BC, NatConfig>,
        reply_tuple: &mut Tuple<I>,
        packet: &P,
        interfaces: &Interfaces<'_, D>,
        result: RoutineResult<I>,
    ) -> ControlFlow<(Self::Verdict, Option<NatType>)>
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BC::DeviceClass>,
        CC: NatContext<I, DeviceId = D>,
        BC: FilterBindingsContext,
    {
        match result {
            RoutineResult::Accept | RoutineResult::Return => ControlFlow::Continue(()),
            RoutineResult::Drop => ControlFlow::Break((Verdict::Drop.into(), None)),
            result @ RoutineResult::TransparentLocalDelivery { .. } => {
                unreachable!(
                    "transparent local delivery is only valid in INGRESS hook; got {result:?}"
                )
            }
            RoutineResult::Redirect { dst_port } => {
                ControlFlow::Break(configure_redirect_nat::<_, _, _, _, _, Self>(
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

    fn redirect_addr<P, D, CC>(_: &mut CC, _: &P, _: Option<&D>) -> Option<I::Addr>
    where
        I: IpExt,
        P: IpPacket<I>,
        CC: NatContext<I, DeviceId = D>,
    {
        Some(*I::LOOPBACK_ADDRESS)
    }
}

impl FilterVerdict for Verdict {
    fn behavior(&self) -> ControlFlow<()> {
        match self {
            Self::Accept => ControlFlow::Continue(()),
            Self::Drop => ControlFlow::Break(()),
        }
    }
}

/// The entry point for NAT logic from an IP layer filtering hook.
///
/// This function configures NAT, if it has not yet been configured for the
/// connection, and performs NAT on the provided packet based on the
/// connection's NAT type.
pub(crate) fn perform_nat<N, I, P, D, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    conn: &mut Connection<I, BC, NatConfig>,
    hook: &Hook<I, BC::DeviceClass, ()>,
    packet: &mut P,
    interfaces: Interfaces<'_, D>,
) -> N::Verdict
where
    N: NatHook<I>,
    I: IpExt,
    P: IpPacket<I>,
    D: InterfaceProperties<BC::DeviceClass>,
    CC: NatContext<I, DeviceId = D>,
    BC: FilterBindingsContext,
{
    let NatConfig { config } = conn.external_data();
    let conn_nat = if let Some(config) = config.get() {
        *config
    } else {
        // NAT has not yet been configured for this connection; traverse the installed
        // NAT routines in order to configure NAT.
        let conn = match conn {
            Connection::Exclusive(conn) => conn,
            Connection::Shared(_) => {
                // NAT is configured for every connection before it becomes a shared connection
                // and is inserted in the conntrack table. (This is true whether or not NAT will
                // actually be performed; the configuration could be `None`.)
                unreachable!(
                    "connections always have NAT configured before they are inserted in conntrack"
                )
            }
        };
        let (reply_tuple, external_data) = conn.reply_tuple_and_external_data_mut();
        let (verdict, config) = configure_nat::<N, _, _, _, _, _>(
            core_ctx,
            bindings_ctx,
            table,
            reply_tuple,
            hook,
            packet,
            interfaces,
        );
        if let ControlFlow::Break(()) = verdict.behavior() {
            return verdict;
        }
        external_data.config.set(config).expect("NAT should not have been configured yet");
        config
    };

    match (conn_nat, N::NAT_TYPE) {
        (None, _) => Verdict::Accept.into(),
        (Some(NatType::Destination), NatType::Destination) => {
            rewrite_packet_dnat(conn, packet).into()
        }
    }
}

/// Configure NAT by rewriting the provided reply tuple of a connection.
///
/// Evaluates the NAT routines at the provided hook and, on finding a rule that
/// matches the provided packet, configures NAT based on the rule's action. Note
/// that because NAT routines can contain a superset of the rules filter
/// routines can, it's possible for this packet to hit a non-NAT action.
fn configure_nat<N, I, P, D, CC, BC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    reply_tuple: &mut Tuple<I>,
    hook: &Hook<I, BC::DeviceClass, ()>,
    packet: &P,
    interfaces: Interfaces<'_, D>,
) -> (N::Verdict, Option<NatType>)
where
    N: NatHook<I>,
    I: IpExt,
    P: IpPacket<I>,
    D: InterfaceProperties<BC::DeviceClass>,
    CC: NatContext<I, DeviceId = D>,
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
    (Verdict::Accept.into(), None)
}

/// Configure Redirect NAT, a special case of DNAT that redirects the packet to
/// the local host.
fn configure_redirect_nat<I, P, D, CC, BC, N>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    table: &Table<I, BC, NatConfig>,
    reply_tuple: &mut Tuple<I>,
    packet: &P,
    interfaces: &Interfaces<'_, D>,
    dst_port: Option<RangeInclusive<NonZeroU16>>,
) -> (Verdict, Option<NatType>)
where
    I: IpExt,
    P: IpPacket<I>,
    D: InterfaceProperties<BC::DeviceClass>,
    CC: NatContext<I, DeviceId = D>,
    BC: FilterBindingsContext,
    N: NatHook<I>,
{
    // Choose an appropriate new destination address and, optionally, port. Then
    // rewrite the source address/port of the reply tuple for the connection to use
    // as the guide for future packet rewriting.

    // TODO(https://fxbug.dev/341771631): when we support source NAT, we should
    // panic if hook_type is Source; it would mean we've hit a DNAT action from an
    // SNAT-only hook, which would indicate a validation error (to ensure no DNAT
    // actions are reachable from SNAT hooks), or a caller error (to provide an
    // SNAT hook with DNAT hook_type).
    match N::NAT_TYPE {
        NatType::Destination => {}
    }

    // If we are in INGRESS, use the primary address of the incoming interface; if
    // we are in LOCAL_EGRESS, use the loopback address.
    let Some(new_src_addr) = N::redirect_addr(core_ctx, packet, interfaces.ingress) else {
        return (Verdict::Drop, None);
    };
    reply_tuple.src_addr = new_src_addr;

    let verdict = dst_port
        .map(|range| rewrite_tuple_src_port(bindings_ctx, table, reply_tuple, range))
        .unwrap_or(Verdict::Accept);
    let nat_type = match verdict {
        Verdict::Drop => None,
        Verdict::Accept => Some(NatType::Destination),
    };
    (verdict, nat_type)
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

/// Perform Destination NAT (DNAT) on a packet, using its connection in the
/// conntrack table as a guide.
fn rewrite_packet_dnat<I, P, BT>(conn: &Connection<I, BT, NatConfig>, packet: &mut P) -> Verdict
where
    I: IpExt,
    P: IpPacket<I>,
    BT: FilterBindingsTypes,
{
    // If this packet is in the "original" direction of the connection, rewrite its
    // destination address and port using the source address and port from the
    // connection's reply tuple. If this is a reply packet, rewrite the *source*
    // using the original tuple's destination so the traffic is seen to come from
    // the expected source.
    //
    // The reply tuple functions both as a way to mark what we expect to see coming
    // in, *and* a way to stash what the NAT remapping should be.
    let Some(tuple) = Tuple::from_packet(packet) else {
        return Verdict::Accept;
    };
    let direction = conn.direction(&tuple).expect("packet should match connection");
    match direction {
        ConnectionDirection::Original => {
            let tuple = conn.reply_tuple();
            let (new_dst_addr, new_dst_port) = (tuple.src_addr, tuple.src_port_or_id);

            packet.set_dst_addr(new_dst_addr);
            let mut transport = packet.transport_packet_mut();
            let Some(mut packet) = transport.transport_packet_mut() else {
                return Verdict::Accept;
            };
            let Some(new_dst_port) = NonZeroU16::new(new_dst_port) else {
                // TODO(https://fxbug.dev/341128580): allow rewriting port to zero if allowed by
                // the transport-layer protocol.
                error!("cannot rewrite dst port to unspecified; dropping packet");
                return Verdict::Drop;
            };
            packet.set_dst_port(new_dst_port);
        }
        ConnectionDirection::Reply => {
            let tuple = conn.original_tuple();
            let (new_src_addr, new_src_port) = (tuple.dst_addr, tuple.dst_port_or_id);

            packet.set_src_addr(new_src_addr);
            let mut transport = packet.transport_packet_mut();
            let Some(mut packet) = transport.transport_packet_mut() else {
                return Verdict::Accept;
            };
            let Some(new_src_port) = NonZeroU16::new(new_src_port) else {
                // TODO(https://fxbug.dev/341128580): allow rewriting port to zero if allowed by
                // the transport-layer protocol.
                error!("cannot rewrite src port to unspecified; dropping packet");
                return Verdict::Drop;
            };
            packet.set_src_port(new_src_port);
        }
    }
    Verdict::Accept
}

#[cfg(test)]
mod tests {
    use alloc::{collections::HashMap, vec};
    use core::marker::PhantomData;

    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_types::{
        ip::{Ip, Ipv4, Ipv6},
        NonMappedAddr,
    };
    use netstack3_base::IntoCoreTimerCtx;
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::{FakeBindingsCtx, FakeNatCtx},
        matchers::testutil::{ethernet_interface, FakeDeviceId},
        matchers::PacketMatcher,
        packets::testutil::internal::{ArbitraryValue, FakeIpPacket, FakeTcpSegment, TestIpExt},
        state::{Action, Routine, Rule, TransparentProxy},
    };

    #[test]
    fn accept_by_default_if_no_matching_rules_in_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &Hook::default(),
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (Verdict::Accept, None)
        );
    }

    #[test]
    fn accept_by_default_if_return_from_routine() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

        let hook = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(PacketMatcher::default(), Action::Return)],
            }],
        };
        assert_eq!(
            configure_nat::<LocalEgressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (Verdict::Accept, None)
        );
    }

    #[test]
    fn accept_terminal_for_installed_routine() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

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
            configure_nat::<LocalEgressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &Hook { routines: vec![routine.clone()] },
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (Verdict::Accept, None)
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
            configure_nat::<LocalEgressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (Verdict::Drop, None)
        );
    }

    #[test]
    fn drop_terminal_for_entire_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

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
            configure_nat::<LocalEgressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (Verdict::Drop, None)
        );
    }

    #[test]
    fn transparent_proxy_terminal_for_entire_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

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
            configure_nat::<IngressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &ingress,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (
                IngressVerdict::TransparentLocalDelivery { addr: Ipv4::DST_IP, port: LOCAL_PORT },
                None
            )
        );
    }

    #[test]
    fn redirect_terminal_for_entire_hook() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

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
            configure_nat::<LocalEgressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: None, egress: None },
            ),
            (Verdict::Accept, Some(NatType::Destination))
        );
    }

    #[test]
    fn redirect_ingress_drops_packet_if_no_assigned_address() {
        let mut bindings_ctx = FakeBindingsCtx::<Ipv4>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx::default();
        let packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();

        let hook = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(
                    PacketMatcher::default(),
                    Action::Redirect { dst_port: None },
                )],
            }],
        };

        assert_eq!(
            configure_nat::<IngressHook, _, _, _, _, _>(
                &mut core_ctx,
                &mut bindings_ctx,
                &conntrack,
                &mut Tuple::from_packet(&packet).unwrap(),
                &hook,
                &packet,
                Interfaces { ingress: Some(&ethernet_interface()), egress: None },
            ),
            (Verdict::Drop.into(), None)
        );
    }

    trait NatHookExt<I: TestIpExt>: NatHook<I> {
        type ReplyHook: NatHookExt<I>;

        fn redirect_dst() -> I::Addr;

        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId>;
    }

    impl<I: TestIpExt> NatHookExt<I> for IngressHook {
        type ReplyHook = LocalEgressHook;

        fn redirect_dst() -> I::Addr {
            I::DST_IP_2
        }

        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: Some(interface), egress: None }
        }
    }

    impl<I: TestIpExt> NatHookExt<I> for LocalEgressHook {
        type ReplyHook = IngressHook;

        fn redirect_dst() -> I::Addr {
            *I::LOOPBACK_ADDRESS
        }

        fn interfaces<'a>(interface: &'a FakeDeviceId) -> Interfaces<'a, FakeDeviceId> {
            Interfaces { ingress: None, egress: Some(interface) }
        }
    }

    const LOCAL_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(55555));

    #[ip_test]
    #[test_case(PhantomData::<IngressHook>, None; "redirect INGRESS")]
    #[test_case(PhantomData::<IngressHook>, Some(LOCAL_PORT); "redirect INGRESS to local port")]
    #[test_case(PhantomData::<LocalEgressHook>, None; "redirect LOCAL_EGRESS")]
    #[test_case(PhantomData::<LocalEgressHook>, Some(LOCAL_PORT); "redirect LOCAL_EGRESS to local port")]
    fn redirect<I: Ip + TestIpExt, N: NatHookExt<I>>(
        _hook: PhantomData<N>,
        dst_port: Option<NonZeroU16>,
    ) {
        let mut bindings_ctx = FakeBindingsCtx::<I>::new();
        let conntrack = Table::new::<IntoCoreTimerCtx>(&mut bindings_ctx);
        let mut core_ctx = FakeNatCtx {
            device_addrs: HashMap::from([(
                ethernet_interface(),
                NonMappedAddr::new(SpecifiedAddr::new(I::DST_IP_2).unwrap()).unwrap(),
            )]),
        };

        // Create a packet and get the corresponding connection from conntrack.
        let mut packet = FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value();
        let pre_nat_packet = packet.clone();
        let mut conn = conntrack
            .get_connection_for_packet_and_update(&bindings_ctx, &packet)
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
        let verdict = perform_nat::<N, _, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut packet,
            N::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());

        // The packet's destination should be rewritten, and DNAT should be configured
        // for the packet; the reply tuple's source should be rewritten to match the new
        // destination.
        let expected = FakeIpPacket::<_, FakeTcpSegment> {
            src_ip: packet.src_ip,
            dst_ip: N::redirect_dst(),
            body: FakeTcpSegment {
                src_port: packet.body.src_port,
                dst_port: dst_port.map(NonZeroU16::get).unwrap_or(packet.body.dst_port),
            },
        };
        assert_eq!(packet, expected);
        assert_eq!(
            conn.external_data().config.get().expect("NAT should be configured"),
            &Some(NatType::Destination)
        );
        assert_eq!(conn.original_tuple(), &original);
        let mut reply = Tuple { src_addr: N::redirect_dst(), ..original.invert() };
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
        let verdict = perform_nat::<N::ReplyHook, _, _, _, _, _>(
            &mut core_ctx,
            &mut bindings_ctx,
            &conntrack,
            &mut conn,
            &nat_routines,
            &mut reply_packet,
            N::interfaces(&ethernet_interface()),
        );
        assert_eq!(verdict, Verdict::Accept.into());
        assert_eq!(reply_packet, pre_nat_packet.reply());
    }

    fn packet_with_src_port(src_port: u16) -> FakeIpPacket<Ipv4, FakeTcpSegment> {
        FakeIpPacket {
            body: FakeTcpSegment { src_port, ..ArbitraryValue::arbitrary_value() },
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
