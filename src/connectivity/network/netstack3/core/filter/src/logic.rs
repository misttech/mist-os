// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::num::NonZeroU16;
use packet_formats::ip::IpExt;
use tracing::error;

use crate::{
    context::{FilterBindingsTypes, FilterIpContext},
    matchers::InterfaceProperties,
    packets::{IpPacket, TransportPacket},
    state::{Action, Hook, Routine, Rule, TransparentProxy},
    FilterIpMetadata, MaybeTransportPacket,
};

/// The final result of packet processing at a given filtering hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// The packet should continue traversing the stack.
    Accept,
    /// The packet should be dropped immediately.
    Drop,
}

/// The final result of packet processing at the INGRESS hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IngressVerdict<I: IpExt> {
    /// A verdict that is valid at any hook.
    Verdict(Verdict),
    /// The packet should be immediately redirected to a local socket without its
    /// header being changed in any way.
    TransparentLocalDelivery {
        /// The bound address of the local socket to redirect the packet to.
        addr: I::Addr,
        /// The bound port of the local socket to redirect the packet to.
        port: NonZeroU16,
    },
}

impl<I: IpExt> From<Verdict> for IngressVerdict<I> {
    fn from(verdict: Verdict) -> Self {
        IngressVerdict::Verdict(verdict)
    }
}

/// A witness type to indicate that the egress filtering hook has been run.
#[derive(Debug)]
pub struct ProofOfEgressCheck {
    _private_field_to_prevent_construction_outside_of_module: (),
}

pub(crate) struct Interfaces<'a, D> {
    pub ingress: Option<&'a D>,
    pub egress: Option<&'a D>,
}

/// The result of packet processing for a given routine.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum RoutineResult<I: IpExt> {
    /// The packet should stop traversing the rest of the current installed
    /// routine, but continue travsering other routines installed in the hook.
    Accept,
    /// The packet should continue at the next rule in the calling chain.
    Return,
    /// The packet should be dropped immediately.
    Drop,
    /// The packet should be immediately redirected to a local socket without its
    /// header being changed in any way.
    TransparentLocalDelivery {
        /// The bound address of the local socket to redirect the packet to.
        addr: I::Addr,
        /// The bound port of the local socket to redirect the packet to.
        port: NonZeroU16,
    },
}

fn check_routine<I, P, D, DeviceClass>(
    Routine { rules }: &Routine<I, DeviceClass, ()>,
    packet: &mut P,
    interfaces: &Interfaces<'_, D>,
) -> RoutineResult<I>
where
    I: IpExt,
    P: IpPacket<I>,
    D: InterfaceProperties<DeviceClass>,
{
    for Rule { matcher, action, validation_info: () } in rules {
        if matcher.matches(packet, &interfaces) {
            match action {
                Action::Accept => return RoutineResult::Accept,
                Action::Return => return RoutineResult::Return,
                Action::Drop => return RoutineResult::Drop,
                // TODO(https://fxbug.dev/332739892): enforce some kind of maximum depth on the
                // routine graph to prevent a stack overflow here.
                Action::Jump(target) => match check_routine(target.get(), packet, interfaces) {
                    result @ (RoutineResult::Accept
                    | RoutineResult::Drop
                    | RoutineResult::TransparentLocalDelivery { .. }) => return result,
                    RoutineResult::Return => continue,
                },
                Action::TransparentProxy(proxy) => {
                    let (addr, port) = match proxy {
                        TransparentProxy::LocalPort(port) => (packet.dst_addr(), *port),
                        TransparentProxy::LocalAddr(addr) => {
                            let maybe_transport_packet = packet.transport_packet();
                            let Some(transport_packet) = maybe_transport_packet.transport_packet()
                            else {
                                // We ensure that TransparentProxy rules are always accompanied by a
                                // TCP or UDP matcher when filtering state is provided to Core, but
                                // given this invariant is enforced far from here, we log an error
                                // and drop the packet, which would likely happen at the transport
                                // layer anyway.
                                error!(
                                    "transparent proxy action is only valid on a rule that matches \
                                    on transport protocol, but this packet has no transport header",
                                );
                                return RoutineResult::Drop;
                            };
                            let port = NonZeroU16::new(transport_packet.dst_port())
                                .expect("TCP and UDP destination port is always non-zero");
                            (*addr, port)
                        }
                        TransparentProxy::LocalAddrAndPort(addr, port) => (*addr, *port),
                    };
                    return RoutineResult::TransparentLocalDelivery { addr, port };
                }
            }
        }
    }
    RoutineResult::Return
}

fn check_routines_for_hook<I, P, D, DeviceClass>(
    hook: &Hook<I, DeviceClass, ()>,
    packet: &mut P,
    interfaces: Interfaces<'_, D>,
) -> Verdict
where
    I: IpExt,
    P: IpPacket<I>,
    D: InterfaceProperties<DeviceClass>,
{
    let Hook { routines } = hook;
    for routine in routines {
        match check_routine(&routine, packet, &interfaces) {
            RoutineResult::Accept | RoutineResult::Return => {}
            RoutineResult::Drop => return Verdict::Drop,
            result @ RoutineResult::TransparentLocalDelivery { .. } => {
                unreachable!(
                    "immediate local delivery is only valid in INGRESS hook; got {result:?}"
                )
            }
        }
    }
    Verdict::Accept
}

fn check_routines_for_ingress<I, P, D, DeviceClass>(
    hook: &Hook<I, DeviceClass, ()>,
    packet: &mut P,
    interfaces: Interfaces<'_, D>,
) -> IngressVerdict<I>
where
    I: IpExt,
    P: IpPacket<I>,
    D: InterfaceProperties<DeviceClass>,
{
    let Hook { routines } = hook;
    for routine in routines {
        match check_routine(&routine, packet, &interfaces) {
            RoutineResult::Accept | RoutineResult::Return => {}
            RoutineResult::Drop => return Verdict::Drop.into(),
            RoutineResult::TransparentLocalDelivery { addr, port } => {
                return IngressVerdict::TransparentLocalDelivery { addr, port };
            }
        }
    }
    Verdict::Accept.into()
}

/// An implementation of packet filtering logic, providing entry points at
/// various stages of packet processing.
pub trait FilterHandler<I: IpExt, BT: FilterBindingsTypes> {
    /// The ingress hook intercepts incoming traffic before a routing decision
    /// has been made.
    fn ingress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        metadata: &mut M,
    ) -> IngressVerdict<I>
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>;

    /// The local ingress hook intercepts incoming traffic that is destined for
    /// the local host.
    fn local_ingress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        metadata: &mut M,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>;

    /// The forwarding hook intercepts incoming traffic that is destined for
    /// another host.
    fn forwarding_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        in_interface: &D,
        out_interface: &D,
        metadata: &mut M,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>;

    /// The local egress hook intercepts locally-generated traffic before a
    /// routing decision has been made.
    fn local_egress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        metadata: &mut M,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>;

    /// The egress hook intercepts all outgoing traffic after a routing decision
    /// has been made.
    fn egress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        metadata: &mut M,
    ) -> (Verdict, ProofOfEgressCheck)
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>;
}

/// The "production" implementation of packet filtering.
///
/// Provides an implementation of [`FilterHandler`] for any `CC` that implements
/// [`FilterIpContext`].
pub struct FilterImpl<'a, CC>(pub &'a mut CC);

impl<I: IpExt, BT: FilterBindingsTypes, CC: FilterIpContext<I, BT>> FilterHandler<I, BT>
    for FilterImpl<'_, CC>
{
    fn ingress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        _metadata: &mut M,
    ) -> IngressVerdict<I>
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_ingress(
                &state.installed_routines.get().ip.ingress,
                packet,
                Interfaces { ingress: Some(interface), egress: None },
            )
        })
    }

    fn local_ingress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        _metadata: &mut M,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.installed_routines.get().ip.local_ingress,
                packet,
                Interfaces { ingress: Some(interface), egress: None },
            )
        })
    }

    fn forwarding_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        in_interface: &D,
        out_interface: &D,
        _metadata: &mut M,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.installed_routines.get().ip.forwarding,
                packet,
                Interfaces { ingress: Some(in_interface), egress: Some(out_interface) },
            )
        })
    }

    fn local_egress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        _metadata: &mut M,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.installed_routines.get().ip.local_egress,
                packet,
                Interfaces { ingress: None, egress: Some(interface) },
            )
        })
    }

    fn egress_hook<P, D, M>(
        &mut self,
        packet: &mut P,
        interface: &D,
        _metadata: &mut M,
    ) -> (Verdict, ProofOfEgressCheck)
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
        M: FilterIpMetadata<I, BT>,
    {
        let Self(this) = self;
        (
            this.with_filter_state(|state| {
                check_routines_for_hook(
                    &state.installed_routines.get().ip.egress,
                    packet,
                    Interfaces { ingress: None, egress: Some(interface) },
                )
            }),
            ProofOfEgressCheck { _private_field_to_prevent_construction_outside_of_module: () },
        )
    }
}

#[cfg(feature = "testutils")]
pub mod testutil {
    use super::*;

    /// A no-op implementation of packet filtering that accepts any packet that
    /// passes through it, useful for unit tests of other modules where trait bounds
    /// require that a `FilterHandler` is available but no filtering logic is under
    /// test.
    ///
    /// Provides an implementation of [`FilterHandler`].
    pub struct NoopImpl;

    impl<I: IpExt, BT: FilterBindingsTypes> FilterHandler<I, BT> for NoopImpl {
        fn ingress_hook<P, D, M>(&mut self, _: &mut P, _: &D, _: &mut M) -> IngressVerdict<I>
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
            M: FilterIpMetadata<I, BT>,
        {
            Verdict::Accept.into()
        }

        fn local_ingress_hook<P, D, M>(&mut self, _: &mut P, _: &D, _: &mut M) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
            M: FilterIpMetadata<I, BT>,
        {
            Verdict::Accept
        }

        fn forwarding_hook<P, D, M>(&mut self, _: &mut P, _: &D, _: &D, _: &mut M) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
            M: FilterIpMetadata<I, BT>,
        {
            Verdict::Accept
        }

        fn local_egress_hook<P, D, M>(&mut self, _: &mut P, _: &D, _: &mut M) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
            M: FilterIpMetadata<I, BT>,
        {
            Verdict::Accept
        }

        fn egress_hook<P, D, M>(
            &mut self,
            _: &mut P,
            _: &D,
            _: &mut M,
        ) -> (Verdict, ProofOfEgressCheck)
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
            M: FilterIpMetadata<I, BT>,
        {
            (Verdict::Accept, ProofOfEgressCheck::forge_proof_for_test())
        }
    }

    impl ProofOfEgressCheck {
        /// For tests where it's not feasible to run the egress hook.
        pub(crate) fn forge_proof_for_test() -> Self {
            ProofOfEgressCheck { _private_field_to_prevent_construction_outside_of_module: () }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};
    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::{FakeCtx, FakeDeviceClass},
        matchers::{
            testutil::{ethernet_interface, wlan_interface, FakeDeviceId},
            InterfaceMatcher, PacketMatcher, PortMatcher, TransportProtocolMatcher,
        },
        packets::testutil::internal::{
            ArbitraryValue, FakeIpPacket, FakeTcpSegment, TestIpExt, TransportPacketExt,
        },
        state::{ConntrackExternalData, IpRoutines, UninstalledRoutine},
    };

    impl<I: IpExt> Rule<I, FakeDeviceClass, ()> {
        fn new(
            matcher: PacketMatcher<I, FakeDeviceClass>,
            action: Action<I, FakeDeviceClass, ()>,
        ) -> Self {
            Rule { matcher, action, validation_info: () }
        }
    }

    #[test]
    fn return_by_default_if_no_matching_rules_in_routine() {
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &Routine { rules: Vec::new() },
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Return
        );

        // A subroutine should also yield `Return` if no rules match, allowing
        // the calling routine to continue execution after the `Jump`.
        let routine = Routine {
            rules: vec![
                Rule::new(
                    PacketMatcher::default(),
                    Action::Jump(UninstalledRoutine::new(Vec::new(), 0)),
                ),
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Drop
        );
    }

    struct NullMetadata {}

    impl<I: IpExt, BT: FilterBindingsTypes> FilterIpMetadata<I, BT> for NullMetadata {
        fn take_conntrack_connection(
            &mut self,
        ) -> Option<crate::conntrack::Connection<I, BT, ConntrackExternalData>> {
            unimplemented!();
        }

        fn replace_conntrack_connection(
            &mut self,
            _conn: crate::conntrack::Connection<I, BT, ConntrackExternalData>,
        ) -> Option<crate::conntrack::Connection<I, BT, ConntrackExternalData>> {
            unimplemented!();
        }
    }

    #[test]
    fn accept_by_default_if_no_matching_rules_in_hook() {
        assert_eq!(
            check_routines_for_hook::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &Hook::default(),
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                Interfaces { ingress: None, egress: None },
            ),
            Verdict::Accept
        );
    }

    #[test]
    fn accept_by_default_if_return_from_routine() {
        let hook = Hook {
            routines: vec![Routine {
                rules: vec![Rule::new(PacketMatcher::default(), Action::Return)],
            }],
        };

        assert_eq!(
            check_routines_for_hook::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &hook,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                Interfaces { ingress: None, egress: None },
            ),
            Verdict::Accept
        );
    }

    #[test]
    fn accept_terminal_for_installed_routine() {
        let routine = Routine {
            rules: vec![
                // Accept all traffic.
                Rule::new(PacketMatcher::default(), Action::Accept),
                // Drop all traffic.
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Accept
        );

        // `Accept` should also be propagated from subroutines.
        let routine = Routine {
            rules: vec![
                // Jump to a routine that accepts all traffic.
                Rule::new(
                    PacketMatcher::default(),
                    Action::Jump(UninstalledRoutine::new(
                        vec![Rule::new(PacketMatcher::default(), Action::Accept)],
                        0,
                    )),
                ),
                // Drop all traffic.
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Accept
        );

        // Now put that routine in a hook that also includes *another* installed
        // routine which drops all traffic. The first installed routine should
        // terminate at its `Accept` result, but the hook should terminate at
        // the `Drop` result in the second routine.
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
            check_routines_for_hook::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &hook,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                Interfaces { ingress: None, egress: None },
            ),
            Verdict::Drop
        );
    }

    #[test]
    fn drop_terminal_for_entire_hook() {
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
            check_routines_for_hook::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &hook,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                Interfaces { ingress: None, egress: None },
            ),
            Verdict::Drop
        );
    }

    #[test]
    fn transparent_proxy_terminal_for_entire_hook() {
        const TPROXY_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(8080));

        let ingress = Hook {
            routines: vec![
                Routine {
                    rules: vec![Rule::new(
                        PacketMatcher::default(),
                        Action::TransparentProxy(TransparentProxy::LocalPort(TPROXY_PORT)),
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
            check_routines_for_ingress::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &ingress,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                Interfaces { ingress: None, egress: None },
            ),
            IngressVerdict::TransparentLocalDelivery { addr: Ipv4::DST_IP, port: TPROXY_PORT }
        );
    }

    #[test]
    fn jump_recursively_evaluates_target_routine() {
        // Drop result from a target routine is propagated to the calling
        // routine.
        let routine = Routine {
            rules: vec![Rule::new(
                PacketMatcher::default(),
                Action::Jump(UninstalledRoutine::new(
                    vec![Rule::new(PacketMatcher::default(), Action::Drop)],
                    0,
                )),
            )],
        };
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Drop
        );

        // Accept result from a target routine is also propagated to the calling
        // routine.
        let routine = Routine {
            rules: vec![
                Rule::new(
                    PacketMatcher::default(),
                    Action::Jump(UninstalledRoutine::new(
                        vec![Rule::new(PacketMatcher::default(), Action::Accept)],
                        0,
                    )),
                ),
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Accept
        );

        // Return from a target routine results in continued evaluation of the
        // calling routine.
        let routine = Routine {
            rules: vec![
                Rule::new(
                    PacketMatcher::default(),
                    Action::Jump(UninstalledRoutine::new(
                        vec![Rule::new(PacketMatcher::default(), Action::Return)],
                        0,
                    )),
                ),
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };
        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Drop
        );
    }

    #[test]
    fn return_terminal_for_single_routine() {
        let routine = Routine {
            rules: vec![
                Rule::new(PacketMatcher::default(), Action::Return),
                // Drop all traffic.
                Rule::new(PacketMatcher::default(), Action::Drop),
            ],
        };

        assert_eq!(
            check_routine::<Ipv4, _, FakeDeviceId, FakeDeviceClass>(
                &routine,
                &mut FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            RoutineResult::Return
        );
    }

    #[ip_test]
    fn filter_handler_implements_ip_hooks_correctly<I: Ip + TestIpExt>() {
        fn drop_all_traffic<I: TestIpExt>(
            matcher: PacketMatcher<I, FakeDeviceClass>,
        ) -> Hook<I, FakeDeviceClass, ()> {
            Hook { routines: vec![Routine { rules: vec![Rule::new(matcher, Action::Drop)] }] }
        }

        // Ingress hook should use ingress routines and check the input
        // interface.
        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            ingress: drop_all_traffic(PacketMatcher {
                in_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Wlan)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            FilterImpl(&mut ctx).ingress_hook(
                &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                &wlan_interface(),
                &mut NullMetadata {},
            ),
            Verdict::Drop.into()
        );

        // Local ingress hook should use local ingress routines and check the
        // input interface.
        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            local_ingress: drop_all_traffic(PacketMatcher {
                in_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Wlan)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            FilterImpl(&mut ctx).local_ingress_hook(
                &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                &wlan_interface(),
                &mut NullMetadata {},
            ),
            Verdict::Drop
        );

        // Forwarding hook should use forwarding routines and check both the
        // input and output interfaces.
        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            forwarding: drop_all_traffic(PacketMatcher {
                in_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Wlan)),
                out_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Ethernet)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            FilterImpl(&mut ctx).forwarding_hook(
                &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                &wlan_interface(),
                &ethernet_interface(),
                &mut NullMetadata {},
            ),
            Verdict::Drop
        );

        // Local egress hook should use local egress routines and check the
        // output interface.
        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            local_egress: drop_all_traffic(PacketMatcher {
                out_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Wlan)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            FilterImpl(&mut ctx).local_egress_hook(
                &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                &wlan_interface(),
                &mut NullMetadata {},
            ),
            Verdict::Drop
        );

        // Egress hook should use egress routines and check the output
        // interface.
        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            egress: drop_all_traffic(PacketMatcher {
                out_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Wlan)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            FilterImpl(&mut ctx)
                .egress_hook(
                    &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                    &wlan_interface(),
                    &mut NullMetadata {},
                )
                .0,
            Verdict::Drop
        );
    }

    #[ip_test]
    #[test_case(22 => Verdict::Accept; "port 22 allowed for SSH")]
    #[test_case(80 => Verdict::Accept; "port 80 allowed for HTTP")]
    #[test_case(1024 => Verdict::Accept; "ephemeral port 1024 allowed")]
    #[test_case(65535 => Verdict::Accept; "ephemeral port 65535 allowed")]
    #[test_case(1023 => Verdict::Drop; "privileged port 1023 blocked")]
    #[test_case(53 => Verdict::Drop; "privileged port 53 blocked")]
    fn block_privileged_ports_except_ssh_http<I: Ip + TestIpExt>(port: u16) -> Verdict {
        fn tcp_port_rule<I: IpExt>(
            src_port: Option<PortMatcher>,
            dst_port: Option<PortMatcher>,
            action: Action<I, FakeDeviceClass, ()>,
        ) -> Rule<I, FakeDeviceClass, ()> {
            Rule::new(
                PacketMatcher {
                    transport_protocol: Some(TransportProtocolMatcher {
                        proto: <&FakeTcpSegment as TransportPacketExt<I>>::proto(),
                        src_port,
                        dst_port,
                    }),
                    ..Default::default()
                },
                action,
            )
        }

        fn default_filter_rules<I: IpExt>() -> Routine<I, FakeDeviceClass, ()> {
            Routine {
                rules: vec![
                    // pass in proto tcp to port 22;
                    tcp_port_rule(
                        /* src_port */ None,
                        Some(PortMatcher { range: 22..=22, invert: false }),
                        Action::Accept,
                    ),
                    // pass in proto tcp to port 80;
                    tcp_port_rule(
                        /* src_port */ None,
                        Some(PortMatcher { range: 80..=80, invert: false }),
                        Action::Accept,
                    ),
                    // pass in proto tcp to range 1024:65535;
                    tcp_port_rule(
                        /* src_port */ None,
                        Some(PortMatcher { range: 1024..=65535, invert: false }),
                        Action::Accept,
                    ),
                    // drop in proto tcp to range 1:6553;
                    tcp_port_rule(
                        /* src_port */ None,
                        Some(PortMatcher { range: 1..=65535, invert: false }),
                        Action::Drop,
                    ),
                ],
            }
        }

        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            local_ingress: Hook { routines: vec![default_filter_rules()] },
            ..Default::default()
        });

        FilterImpl(&mut ctx).local_ingress_hook(
            &mut FakeIpPacket::<I, _> {
                body: FakeTcpSegment { dst_port: port, src_port: 11111 },
                ..ArbitraryValue::arbitrary_value()
            },
            &wlan_interface(),
            &mut NullMetadata {},
        )
    }

    #[ip_test]
    #[test_case(
        ethernet_interface() => Verdict::Accept;
        "allow incoming traffic on ethernet interface"
    )]
    #[test_case(wlan_interface() => Verdict::Drop; "drop incoming traffic on wlan interface")]
    fn filter_on_wlan_only<I: Ip + TestIpExt>(interface: FakeDeviceId) -> Verdict {
        fn drop_wlan_traffic<I: IpExt>() -> Routine<I, FakeDeviceClass, ()> {
            Routine {
                rules: vec![Rule::new(
                    PacketMatcher {
                        in_interface: Some(InterfaceMatcher::Id(wlan_interface().id)),
                        ..Default::default()
                    },
                    Action::Drop,
                )],
            }
        }

        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            local_ingress: Hook { routines: vec![drop_wlan_traffic()] },
            ..Default::default()
        });

        FilterImpl(&mut ctx).local_ingress_hook(
            &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
            &interface,
            &mut NullMetadata {},
        )
    }
}
