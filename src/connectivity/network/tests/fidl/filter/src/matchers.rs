// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::{fmt::Debug, num::NonZeroU64, ops::RangeInclusive};

use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext::{
    AddressMatcher, AddressMatcherType, DeviceClass, InterfaceMatcher, Matchers, PortMatcher,
    TransportProtocolMatcher,
};
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use net_types::ip::{Ip, IpInvariant};

use crate::ip_hooks::{
    IcmpSocket, Interfaces, IrrelevantToTest, Ports, SocketType, Subnets, TcpSocket, UdpSocket,
};

pub(crate) trait Matcher: Copy + Debug {
    type SocketType: SocketType;

    async fn matcher<I: Ip>(
        &self,
        interfaces: Interfaces<'_>,
        subnets: Subnets,
        ports: Ports,
    ) -> Matchers;
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Inversion {
    Default,
    InverseMatch,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AllTraffic;

impl Matcher for AllTraffic {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        Matchers::default()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InterfaceId;

impl Matcher for InterfaceId {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        let Interfaces { ingress, egress } = interfaces;
        Matchers {
            in_interface: ingress.map(|interface| {
                InterfaceMatcher::Id(
                    NonZeroU64::new(interface.id()).expect("interface ID should be nonzero"),
                )
            }),
            out_interface: egress.map(|interface| {
                InterfaceMatcher::Id(
                    NonZeroU64::new(interface.id()).expect("interface ID should be nonzero"),
                )
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InterfaceName;

impl Matcher for InterfaceName {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        async fn get_interface_name(interface: &netemul::TestInterface<'_>) -> InterfaceMatcher {
            InterfaceMatcher::Name(
                interface.get_interface_name().await.expect("get interface name"),
            )
        }

        let Interfaces { ingress, egress } = interfaces;
        let in_interface = match ingress {
            Some(ingress) => Some(get_interface_name(ingress).await),
            None => None,
        };
        let out_interface = match egress {
            Some(egress) => Some(get_interface_name(egress).await),
            None => None,
        };
        Matchers { in_interface, out_interface, ..Default::default() }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InterfaceDeviceClass;

impl Matcher for InterfaceDeviceClass {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        async fn get_device_class(interface: &netemul::TestInterface<'_>) -> InterfaceMatcher {
            InterfaceMatcher::DeviceClass(
                match interface.get_device_class().await.expect("get device class") {
                    fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {}) => {
                        DeviceClass::Loopback
                    }
                    fnet_interfaces::DeviceClass::Device(device) => DeviceClass::Device(device),
                },
            )
        }

        let Interfaces { ingress, egress } = interfaces;
        let in_interface = match ingress {
            Some(ingress) => Some(get_device_class(ingress).await),
            None => None,
        };
        let out_interface = match egress {
            Some(egress) => Some(get_device_class(egress).await),
            None => None,
        };
        Matchers { in_interface, out_interface, ..Default::default() }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SrcAddressSubnet(pub(crate) Inversion);

impl Matcher for SrcAddressSubnet {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Subnets { src, other, dst: _ } = subnets;

        Matchers {
            src_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        fnet_ext::apply_subnet_mask(src)
                            .try_into()
                            .expect("subnet should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        other.try_into().expect("subnet should be valid"),
                    ),
                    invert: true,
                },
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SrcAddressRange(pub(crate) Inversion);

impl Matcher for SrcAddressRange {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Subnets { src, other, dst: _ } = subnets;

        Matchers {
            src_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange { start: src.addr, end: src.addr }
                            .try_into()
                            .expect("address range should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange { start: other.addr, end: other.addr }
                            .try_into()
                            .expect("address range should be valid"),
                    ),
                    invert: true,
                },
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DstAddressSubnet(pub(crate) Inversion);

impl Matcher for DstAddressSubnet {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Subnets { dst, other, src: _ } = subnets;

        Matchers {
            dst_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        fnet_ext::apply_subnet_mask(dst)
                            .try_into()
                            .expect("subnet should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        other.try_into().expect("subnet should be valid"),
                    ),
                    invert: true,
                },
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DstAddressRange(pub(crate) Inversion);

impl Matcher for DstAddressRange {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Subnets { dst, other, src: _ } = subnets;

        Matchers {
            dst_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange { start: dst.addr, end: dst.addr }
                            .try_into()
                            .expect("address range should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange { start: other.addr, end: other.addr }
                            .try_into()
                            .expect("address range should be valid"),
                    ),
                    invert: true,
                },
            }),
            ..Default::default()
        }
    }
}

fn unique_ephemeral_port(exclude: &[u16]) -> u16 {
    // RFC 6335 section 6 defines 49152-65535 as the ephemeral port range.
    const RANGE: RangeInclusive<u16> = 49152..=65535;
    for port in RANGE {
        if !exclude.contains(&port) {
            return port;
        }
    }
    panic!("could not find an available port in the ephemeral range")
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Tcp;

impl Matcher for Tcp {
    type SocketType = TcpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: None,
                dst_port: None,
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TcpSrcPort(pub(crate) Inversion);

impl Matcher for TcpSrcPort {
    type SocketType = TcpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Ports { src, dst } = ports;

        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(src, src, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        let port = unique_ephemeral_port(&[src, dst]);
                        PortMatcher::new(port, port, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
                dst_port: None,
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TcpDstPort(pub(crate) Inversion);

impl Matcher for TcpDstPort {
    type SocketType = TcpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Ports { src, dst } = ports;

        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: None,
                dst_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(dst, dst, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        let port = unique_ephemeral_port(&[src, dst]);
                        PortMatcher::new(port, port, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Udp;

impl Matcher for Udp {
    type SocketType = UdpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Udp {
                src_port: None,
                dst_port: None,
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UdpSrcPort(pub(crate) Inversion);

impl Matcher for UdpSrcPort {
    type SocketType = UdpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Ports { src, dst } = ports;

        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Udp {
                src_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(src, src, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        let port = unique_ephemeral_port(&[src, dst]);
                        PortMatcher::new(port, port, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
                dst_port: None,
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UdpDstPort(pub(crate) Inversion);

impl Matcher for UdpDstPort {
    type SocketType = UdpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        ports: Ports,
    ) -> Matchers {
        let Self(inversion) = self;
        let Ports { src, dst } = ports;

        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Udp {
                src_port: None,
                dst_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(dst, dst, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        let port = unique_ephemeral_port(&[src, dst]);
                        PortMatcher::new(port, port, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Icmp;

impl Matcher for Icmp {
    type SocketType = IcmpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interfaces: Interfaces<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        Matchers {
            transport_protocol: Some({
                let IpInvariant(matcher) = I::map_ip(
                    (),
                    |()| IpInvariant(TransportProtocolMatcher::Icmp),
                    |()| IpInvariant(TransportProtocolMatcher::Icmpv6),
                );
                matcher
            }),
            ..Default::default()
        }
    }
}
