// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::string::String;
use core::{num::NonZeroU64, ops::RangeInclusive};

use derivative::Derivative;
use net_types::ip::{IpAddress, Subnet};
use packet_formats::ip::IpExt;

use crate::{
    logic::Interfaces,
    packets::{IpPacket, MaybeTransportPacket, TransportPacket},
};

/// Matches on metadata of packets that come through the filtering framework.
trait Matcher<T> {
    /// Returns whether the provided value matches.
    fn matches(&self, actual: &T) -> bool;

    /// Returns whether the provided value is set and matches.
    fn required_matches(&self, actual: Option<&T>) -> bool {
        actual.map_or(false, |actual| self.matches(actual))
    }
}

/// Implement `Matcher` for optional matchers, so that if a matcher is left
/// unspecified, it matches all packets by default.
impl<T, O> Matcher<T> for Option<O>
where
    O: Matcher<T>,
{
    fn matches(&self, actual: &T) -> bool {
        self.as_ref().map_or(true, |expected| expected.matches(actual))
    }

    fn required_matches(&self, actual: Option<&T>) -> bool {
        self.as_ref().map_or(true, |expected| expected.required_matches(actual))
    }
}

/// A matcher for network interfaces.
#[derive(Clone, Debug)]
pub enum InterfaceMatcher<DeviceClass> {
    /// The ID of the interface as assigned by the netstack.
    Id(NonZeroU64),
    /// The name of the interface.
    Name(String),
    /// The device class of the interface.
    DeviceClass(DeviceClass),
}

/// Allows filtering code to match on properties of an interface (ID, name, and
/// device class) without Netstack3 Core (or Bindings, in the case of the device
/// class) having to specifically expose that state.
pub trait InterfaceProperties<DeviceClass> {
    /// Returns whether the provided ID matches the interface.
    fn id_matches(&self, id: &NonZeroU64) -> bool;

    /// Returns whether the provided name matches the interface.
    fn name_matches(&self, name: &str) -> bool;

    /// Returns whether the provided device class matches the interface.
    fn device_class_matches(&self, device_class: &DeviceClass) -> bool;
}

impl<DeviceClass, I: InterfaceProperties<DeviceClass>> Matcher<I>
    for InterfaceMatcher<DeviceClass>
{
    fn matches(&self, actual: &I) -> bool {
        match self {
            InterfaceMatcher::Id(id) => actual.id_matches(id),
            InterfaceMatcher::Name(name) => actual.name_matches(name),
            InterfaceMatcher::DeviceClass(device_class) => {
                actual.device_class_matches(device_class)
            }
        }
    }
}

/// A matcher for IP addresses.
#[derive(Clone, Debug)]
pub enum AddressMatcherType<A: IpAddress> {
    /// A subnet that must contain the address.
    Subnet(Subnet<A>),
    /// An inclusive range of IP addresses that must contain the address.
    Range(RangeInclusive<A>),
}

impl<A: IpAddress> Matcher<A> for AddressMatcherType<A> {
    fn matches(&self, actual: &A) -> bool {
        match self {
            Self::Subnet(subnet) => subnet.contains(actual),
            Self::Range(range) => range.contains(actual),
        }
    }
}

/// A matcher for IP addresses.
#[derive(Clone, Debug)]
pub struct AddressMatcher<A: IpAddress> {
    /// The type of the address matcher.
    pub matcher: AddressMatcherType<A>,
    /// Whether to check for an "inverse" or "negative" match (in which case,
    /// if the matcher criteria do *not* apply, it *is* considered a match, and
    /// vice versa).
    pub invert: bool,
}

impl<A: IpAddress> Matcher<A> for AddressMatcher<A> {
    fn matches(&self, addr: &A) -> bool {
        let Self { matcher, invert } = self;
        matcher.matches(addr) ^ *invert
    }
}

/// A matcher for transport-layer port numbers.
#[derive(Clone, Debug)]
pub struct PortMatcher {
    /// The range of port numbers in which the tested port number must fall.
    pub range: RangeInclusive<u16>,
    /// Whether to check for an "inverse" or "negative" match (in which case,
    /// if the matcher criteria do *not* apply, it *is* considered a match, and
    /// vice versa).
    pub invert: bool,
}

impl Matcher<u16> for PortMatcher {
    fn matches(&self, actual: &u16) -> bool {
        let Self { range, invert } = self;
        range.contains(actual) ^ *invert
    }
}

/// A matcher for transport-layer protocol or port numbers.
#[derive(Debug, Clone)]
pub struct TransportProtocolMatcher<P> {
    /// The transport-layer protocol.
    pub proto: P,
    /// If set, the matcher for the source port or identifier of the transport
    /// header.
    pub src_port: Option<PortMatcher>,
    /// If set, the matcher for the destination port or identifier of the
    /// transport header.
    pub dst_port: Option<PortMatcher>,
}

impl<P: PartialEq, T: MaybeTransportPacket> Matcher<(P, T)> for TransportProtocolMatcher<P> {
    fn matches(&self, actual: &(P, T)) -> bool {
        let Self { proto, src_port, dst_port } = self;
        let (packet_proto, packet) = actual;

        proto == packet_proto
            && src_port.required_matches(
                packet.transport_packet().as_ref().map(TransportPacket::src_port).as_ref(),
            )
            && dst_port.required_matches(
                packet.transport_packet().as_ref().map(TransportPacket::dst_port).as_ref(),
            )
    }
}

/// Top-level matcher for IP packets.
#[derive(Derivative, Debug, Clone)]
#[derivative(Default(bound = ""))]
pub struct PacketMatcher<I: IpExt, DeviceClass> {
    /// The interface on which the packet entered the stack.
    ///
    /// Only available in `INGRESS`, `LOCAL_INGRESS`, and `FORWARDING`.
    pub in_interface: Option<InterfaceMatcher<DeviceClass>>,
    /// The interface through which the packet exits the stack.
    ///
    /// Only available in `FORWARDING`, `LOCAL_EGRESS`, and `EGRESS`.
    pub out_interface: Option<InterfaceMatcher<DeviceClass>>,
    /// Matcher for the source IP address.
    pub src_address: Option<AddressMatcher<I::Addr>>,
    /// Matcher for the destination IP address.
    pub dst_address: Option<AddressMatcher<I::Addr>>,
    /// Matchers for the transport layer.
    pub transport_protocol: Option<TransportProtocolMatcher<I::Proto>>,
}

impl<I: IpExt, DeviceClass> PacketMatcher<I, DeviceClass> {
    pub(crate) fn matches<P: IpPacket<I>, D: InterfaceProperties<DeviceClass>>(
        &self,
        packet: &P,
        interfaces: &Interfaces<'_, D>,
    ) -> bool {
        let Self { in_interface, out_interface, src_address, dst_address, transport_protocol } =
            self;
        let Interfaces { ingress: in_if, egress: out_if } = interfaces;

        // If no fields are specified, match all traffic by default.
        in_interface.required_matches(*in_if)
            && out_interface.required_matches(*out_if)
            && src_address.matches(&packet.src_addr())
            && dst_address.matches(&packet.dst_addr())
            && transport_protocol.matches(&(packet.protocol(), packet.transport_packet()))
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use const_unwrap::const_unwrap_option;

    use super::*;
    use crate::context::testutil::FakeDeviceClass;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct FakeDeviceId {
        pub id: NonZeroU64,
        pub name: String,
        pub class: FakeDeviceClass,
    }

    impl InterfaceProperties<FakeDeviceClass> for FakeDeviceId {
        fn id_matches(&self, id: &NonZeroU64) -> bool {
            &self.id == id
        }

        fn name_matches(&self, name: &str) -> bool {
            &self.name == name
        }

        fn device_class_matches(&self, class: &FakeDeviceClass) -> bool {
            &self.class == class
        }
    }

    pub fn wlan_interface() -> FakeDeviceId {
        FakeDeviceId {
            id: const_unwrap_option(NonZeroU64::new(1)),
            name: String::from("wlan"),
            class: FakeDeviceClass::Wlan,
        }
    }

    pub fn ethernet_interface() -> FakeDeviceId {
        FakeDeviceId {
            id: const_unwrap_option(NonZeroU64::new(2)),
            name: String::from("eth"),
            class: FakeDeviceClass::Ethernet,
        }
    }
}

#[cfg(test)]
mod tests {
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
    use packet_formats::ip::{IpProto, Ipv4Proto};
    use test_case::test_case;

    use super::{testutil::*, *};
    use crate::{
        context::testutil::FakeDeviceClass,
        packets::testutil::internal::{
            ArbitraryValue, FakeIcmpEchoRequest, FakeIpPacket, FakeTcpSegment, FakeUdpPacket,
            TestIpExt, TransportPacketExt,
        },
    };

    #[test_case(InterfaceMatcher::Id(wlan_interface().id))]
    #[test_case(InterfaceMatcher::Name(wlan_interface().name.clone()))]
    #[test_case(InterfaceMatcher::DeviceClass(wlan_interface().class))]
    fn match_on_interface_properties(matcher: InterfaceMatcher<FakeDeviceClass>) {
        let matcher = PacketMatcher {
            in_interface: Some(matcher.clone()),
            out_interface: Some(matcher),
            ..Default::default()
        };

        assert_eq!(
            matcher.matches(
                &FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: Some(&wlan_interface()), egress: Some(&wlan_interface()) },
            ),
            true
        );
        assert_eq!(
            matcher.matches(
                &FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value(),
                &Interfaces {
                    ingress: Some(&ethernet_interface()),
                    egress: Some(&ethernet_interface())
                },
            ),
            false
        );
    }

    #[test_case(InterfaceMatcher::Id(wlan_interface().id))]
    #[test_case(InterfaceMatcher::Name(wlan_interface().name.clone()))]
    #[test_case(InterfaceMatcher::DeviceClass(wlan_interface().class))]
    fn interface_matcher_specified_but_not_available_in_hook_does_not_match(
        matcher: InterfaceMatcher<FakeDeviceClass>,
    ) {
        let matcher = PacketMatcher {
            in_interface: Some(matcher.clone()),
            out_interface: Some(matcher),
            ..Default::default()
        };

        assert_eq!(
            matcher.matches(
                &FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: Some(&wlan_interface()) },
            ),
            false
        );
        assert_eq!(
            matcher.matches(
                &FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: Some(&wlan_interface()), egress: None },
            ),
            false
        );
        assert_eq!(
            matcher.matches(
                &FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: Some(&wlan_interface()), egress: Some(&wlan_interface()) },
            ),
            true
        );
    }

    enum AddressMatcherTestCase {
        Subnet,
        Range,
    }

    #[ip_test]
    #[test_case(AddressMatcherTestCase::Subnet, /* invert */ false)]
    #[test_case(AddressMatcherTestCase::Subnet, /* invert */ true)]
    #[test_case(AddressMatcherTestCase::Range, /* invert */ false)]
    #[test_case(AddressMatcherTestCase::Range, /* invert */ true)]
    fn match_on_subnet_or_address_range<I: Ip + TestIpExt>(
        test_case: AddressMatcherTestCase,
        invert: bool,
    ) {
        let matcher = AddressMatcher {
            matcher: match test_case {
                AddressMatcherTestCase::Subnet => AddressMatcherType::Subnet(I::SUBNET),
                AddressMatcherTestCase::Range => {
                    // Generate the inclusive address range that is equivalent to the subnet.
                    let start = I::SUBNET.network();
                    let end = I::map_ip(
                        start,
                        |start| {
                            let range_size = 2_u32.pow(32 - u32::from(I::SUBNET.prefix())) - 1;
                            let end = u32::from_be_bytes(start.ipv4_bytes()) + range_size;
                            Ipv4Addr::from(end.to_be_bytes())
                        },
                        |start| {
                            let range_size = 2_u128.pow(128 - u32::from(I::SUBNET.prefix())) - 1;
                            let end = u128::from_be_bytes(start.ipv6_bytes()) + range_size;
                            Ipv6Addr::from(end.to_be_bytes())
                        },
                    );
                    AddressMatcherType::Range(start..=end)
                }
            },
            invert,
        };

        for matcher in [
            PacketMatcher { src_address: Some(matcher.clone()), ..Default::default() },
            PacketMatcher { dst_address: Some(matcher), ..Default::default() },
        ] {
            assert_ne!(
                matcher.matches::<_, FakeDeviceId>(
                    &FakeIpPacket::<I, FakeTcpSegment>::arbitrary_value(),
                    &Interfaces { ingress: None, egress: None },
                ),
                invert
            );
            assert_eq!(
                matcher.matches::<_, FakeDeviceId>(
                    &FakeIpPacket {
                        src_ip: I::IP_OUTSIDE_SUBNET,
                        dst_ip: I::IP_OUTSIDE_SUBNET,
                        body: FakeTcpSegment::arbitrary_value(),
                    },
                    &Interfaces { ingress: None, egress: None },
                ),
                invert
            );
        }
    }

    enum Protocol {
        Tcp,
        Udp,
        Icmp,
    }

    impl Protocol {
        fn ip_proto<I: IpExt>(&self) -> I::Proto {
            match self {
                Self::Tcp => <&FakeTcpSegment as TransportPacketExt<I>>::proto(),
                Self::Udp => <&FakeUdpPacket as TransportPacketExt<I>>::proto(),
                Self::Icmp => <&FakeIcmpEchoRequest as TransportPacketExt<I>>::proto(),
            }
        }
    }

    #[test_case(Protocol::Tcp, FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value() => true)]
    #[test_case(Protocol::Tcp, FakeIpPacket::<Ipv4, FakeUdpPacket>::arbitrary_value() => false)]
    #[test_case(
        Protocol::Tcp,
        FakeIpPacket::<Ipv4, FakeIcmpEchoRequest>::arbitrary_value()
        => false
    )]
    #[test_case(Protocol::Udp, FakeIpPacket::<Ipv4, FakeUdpPacket>::arbitrary_value() => true)]
    #[test_case(Protocol::Udp, FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value()=> false)]
    #[test_case(
        Protocol::Udp,
        FakeIpPacket::<Ipv4, FakeIcmpEchoRequest>::arbitrary_value()
        => false
    )]
    #[test_case(
        Protocol::Icmp,
        FakeIpPacket::<Ipv4, FakeIcmpEchoRequest>::arbitrary_value()
        => true
    )]
    #[test_case(
        Protocol::Icmp,
        FakeIpPacket::<Ipv6, FakeIcmpEchoRequest>::arbitrary_value()
        => true
    )]
    #[test_case(Protocol::Icmp, FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value() => false)]
    #[test_case(Protocol::Icmp, FakeIpPacket::<Ipv4, FakeUdpPacket>::arbitrary_value() => false)]
    fn match_on_transport_protocol<I: TestIpExt, P: IpPacket<I>>(
        protocol: Protocol,
        packet: P,
    ) -> bool {
        let matcher = PacketMatcher {
            transport_protocol: Some(TransportProtocolMatcher {
                proto: protocol.ip_proto::<I>(),
                src_port: None,
                dst_port: None,
            }),
            ..Default::default()
        };

        matcher.matches::<_, FakeDeviceId>(&packet, &Interfaces { ingress: None, egress: None })
    }

    #[test_case(
        Some(PortMatcher { range: 1024..=65535, invert: false }), None, (11111, 80), true;
        "matching src port"
    )]
    #[test_case(
        Some(PortMatcher { range: 1024..=65535, invert: true }), None, (11111, 80), false;
        "invert match src port"
    )]
    #[test_case(
        Some(PortMatcher { range: 1024..=65535, invert: false }), None, (53, 80), false;
        "non-matching src port"
    )]
    #[test_case(
        None, Some(PortMatcher { range: 22..=22, invert: false }), (11111, 22), true;
        "match dst port"
    )]
    #[test_case(
        None, Some(PortMatcher { range: 22..=22, invert: true }), (11111, 22), false;
        "invert match dst port"
    )]
    #[test_case(
        None, Some(PortMatcher { range: 22..=22, invert: false }), (11111, 80), false;
        "non-matching dst port"
    )]
    fn match_on_port_range(
        src_port: Option<PortMatcher>,
        dst_port: Option<PortMatcher>,
        transport_header: (u16, u16),
        expect_match: bool,
    ) {
        // TCP
        let matcher = PacketMatcher {
            transport_protocol: Some(TransportProtocolMatcher {
                proto: Ipv4Proto::Proto(IpProto::Tcp),
                src_port: src_port.clone(),
                dst_port: dst_port.clone(),
            }),
            ..Default::default()
        };
        let (src, dst) = transport_header;
        assert_eq!(
            matcher.matches::<_, FakeDeviceId>(
                &FakeIpPacket::<Ipv4, _> {
                    body: FakeTcpSegment { src_port: src, dst_port: dst },
                    ..ArbitraryValue::arbitrary_value()
                },
                &Interfaces { ingress: None, egress: None },
            ),
            expect_match
        );

        // UDP
        let matcher = PacketMatcher {
            transport_protocol: Some(TransportProtocolMatcher {
                proto: Ipv4Proto::Proto(IpProto::Udp),
                src_port,
                dst_port,
            }),
            ..Default::default()
        };
        let (src, dst) = transport_header;
        assert_eq!(
            matcher.matches::<_, FakeDeviceId>(
                &FakeIpPacket::<Ipv4, _> {
                    body: FakeUdpPacket { src_port: src, dst_port: dst },
                    ..ArbitraryValue::arbitrary_value()
                },
                &Interfaces { ingress: None, egress: None },
            ),
            expect_match
        );
    }

    #[ip_test]
    fn packet_must_match_all_provided_matchers<I: Ip + TestIpExt>() {
        let matcher = PacketMatcher::<I, FakeDeviceClass> {
            src_address: Some(AddressMatcher {
                matcher: AddressMatcherType::Subnet(I::SUBNET),
                invert: false,
            }),
            dst_address: Some(AddressMatcher {
                matcher: AddressMatcherType::Subnet(I::SUBNET),
                invert: false,
            }),
            ..Default::default()
        };

        assert_eq!(
            matcher.matches::<_, FakeDeviceId>(
                &FakeIpPacket::<_, FakeTcpSegment> {
                    src_ip: I::IP_OUTSIDE_SUBNET,
                    ..ArbitraryValue::arbitrary_value()
                },
                &Interfaces { ingress: None, egress: None },
            ),
            false
        );
        assert_eq!(
            matcher.matches::<_, FakeDeviceId>(
                &FakeIpPacket::<_, FakeTcpSegment> {
                    dst_ip: I::IP_OUTSIDE_SUBNET,
                    ..ArbitraryValue::arbitrary_value()
                },
                &Interfaces { ingress: None, egress: None },
            ),
            false
        );
        assert_eq!(
            matcher.matches::<_, FakeDeviceId>(
                &FakeIpPacket::<_, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            true
        );
    }

    #[test]
    fn match_by_default_if_no_specified_matchers() {
        assert_eq!(
            PacketMatcher::<_, FakeDeviceClass>::default().matches::<_, FakeDeviceId>(
                &FakeIpPacket::<Ipv4, FakeTcpSegment>::arbitrary_value(),
                &Interfaces { ingress: None, egress: None },
            ),
            true
        );
    }
}
