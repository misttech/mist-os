// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use packet_formats::ip::IpExt;

use crate::{
    context::{FilterBindingsTypes, FilterIpContext},
    matchers::InterfaceProperties,
    packets::IpPacket,
    state::{Action, Hook, Routine, Rule},
};

/// The result of packet processing at a given filtering hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// The packet should continue traversing the stack.
    Accept,
    /// The packet should be dropped immediately.
    Drop,
}

pub(crate) struct Interfaces<'a, D> {
    pub ingress: Option<&'a D>,
    pub egress: Option<&'a D>,
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
    for Routine { rules } in routines {
        for Rule { matcher, action, validation_info: () } in rules {
            if matcher.matches(packet, &interfaces) {
                match action {
                    Action::Accept => break,
                    Action::Drop => return Verdict::Drop,
                    Action::Jump(_) | Action::Return => {
                        todo!("https://fxbug.dev/318718273: implement jumping and returning")
                    }
                }
            }
        }
    }
    Verdict::Accept
}

/// An implementation of packet filtering logic, providing entry points at
/// various stages of packet processing.
pub trait FilterHandler<I: IpExt, BT: FilterBindingsTypes> {
    /// The ingress hook intercepts incoming traffic before a routing decision
    /// has been made.
    fn ingress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>;

    /// The local ingress hook intercepts incoming traffic that is destined for
    /// the local host.
    fn local_ingress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>;

    /// The forwarding hook intercepts incoming traffic that is destined for
    /// another host.
    fn forwarding_hook<P, D>(
        &mut self,
        packet: &mut P,
        in_interface: &D,
        out_interface: &D,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>;

    /// The local egress hook intercepts locally-generated traffic before a
    /// routing decision has been made.
    fn local_egress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>;

    /// The egress hook intercepts all outgoing traffic after a routing decision
    /// has been made.
    fn egress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>;
}

/// The "production" implementation of packet filtering.
///
/// Provides an implementation of [`FilterHandler`] for any `CC` that implements
/// [`FilterIpContext`].
pub struct FilterImpl<'a, CC>(pub &'a mut CC);

impl<I: IpExt, BT: FilterBindingsTypes, CC: FilterIpContext<I, BT>> FilterHandler<I, BT>
    for FilterImpl<'_, CC>
{
    fn ingress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.get().ip_routines.ingress,
                packet,
                Interfaces { ingress: Some(interface), egress: None },
            )
        })
    }

    fn local_ingress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.get().ip_routines.local_ingress,
                packet,
                Interfaces { ingress: Some(interface), egress: None },
            )
        })
    }

    fn forwarding_hook<P, D>(
        &mut self,
        packet: &mut P,
        in_interface: &D,
        out_interface: &D,
    ) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.get().ip_routines.forwarding,
                packet,
                Interfaces { ingress: Some(in_interface), egress: Some(out_interface) },
            )
        })
    }

    fn local_egress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.get().ip_routines.local_egress,
                packet,
                Interfaces { ingress: None, egress: Some(interface) },
            )
        })
    }

    fn egress_hook<P, D>(&mut self, packet: &mut P, interface: &D) -> Verdict
    where
        P: IpPacket<I>,
        D: InterfaceProperties<BT::DeviceClass>,
    {
        let Self(this) = self;
        this.with_filter_state(|state| {
            check_routines_for_hook(
                &state.get().ip_routines.egress,
                packet,
                Interfaces { ingress: None, egress: Some(interface) },
            )
        })
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
        fn ingress_hook<P, D>(&mut self, _: &mut P, _: &D) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
        {
            Verdict::Accept
        }

        fn local_ingress_hook<P, D>(&mut self, _: &mut P, _: &D) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
        {
            Verdict::Accept
        }

        fn forwarding_hook<P, D>(&mut self, _: &mut P, _: &D, _: &D) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
        {
            Verdict::Accept
        }

        fn local_egress_hook<P, D>(&mut self, _: &mut P, _: &D) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
        {
            Verdict::Accept
        }

        fn egress_hook<P, D>(&mut self, _: &mut P, _: &D) -> Verdict
        where
            P: IpPacket<I>,
            D: InterfaceProperties<BT::DeviceClass>,
        {
            Verdict::Accept
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
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
        state::IpRoutines,
    };

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
    fn accept_verdict_terminal_for_single_routine() {
        let hook = Hook {
            routines: vec![Routine {
                rules: vec![
                    // Accept all traffic.
                    Rule {
                        matcher: PacketMatcher::default(),
                        action: Action::Accept,
                        validation_info: (),
                    },
                    // Drop all traffic.
                    Rule {
                        matcher: PacketMatcher::default(),
                        action: Action::Drop,
                        validation_info: (),
                    },
                ],
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
    fn drop_verdict_terminal_for_entire_hook() {
        let hook = Hook {
            routines: vec![
                Routine {
                    rules: vec![
                        // Accept all traffic.
                        Rule {
                            matcher: PacketMatcher::default(),
                            action: Action::Accept,
                            validation_info: (),
                        },
                    ],
                },
                Routine {
                    rules: vec![
                        // Drop all traffic.
                        Rule {
                            matcher: PacketMatcher::default(),
                            action: Action::Drop,
                            validation_info: (),
                        },
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

    #[ip_test]
    fn filter_handler_implements_ip_hooks_correctly<I: Ip + TestIpExt>() {
        fn drop_all_traffic<I: TestIpExt>(
            matcher: PacketMatcher<I, FakeDeviceClass>,
        ) -> Hook<I, FakeDeviceClass, ()> {
            Hook {
                routines: vec![Routine {
                    rules: vec![Rule { matcher, action: Action::Drop, validation_info: () }],
                }],
            }
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
                &wlan_interface()
            ),
            Verdict::Drop
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
                &wlan_interface()
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
                &ethernet_interface()
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
                &wlan_interface()
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
            FilterImpl(&mut ctx).egress_hook(
                &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                &wlan_interface()
            ),
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
            Rule {
                matcher: PacketMatcher {
                    transport_protocol: Some(TransportProtocolMatcher {
                        proto: <&FakeTcpSegment as TransportPacketExt<I>>::proto(),
                        src_port,
                        dst_port,
                    }),
                    ..Default::default()
                },
                action,
                validation_info: (),
            }
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
                rules: vec![Rule {
                    matcher: PacketMatcher {
                        in_interface: Some(InterfaceMatcher::Id(wlan_interface().id)),
                        ..Default::default()
                    },
                    action: Action::Drop,
                    validation_info: (),
                }],
            }
        }

        let mut ctx = FakeCtx::with_ip_routines(IpRoutines {
            local_ingress: Hook { routines: vec![drop_wlan_traffic()] },
            ..Default::default()
        });

        FilterImpl(&mut ctx).local_ingress_hook(
            &mut FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
            &interface,
        )
    }
}
