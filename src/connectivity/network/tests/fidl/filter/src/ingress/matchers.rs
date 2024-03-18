// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::num::NonZeroU64;

use fidl_fuchsia_net_ext as fnet_ext;
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext::{
    AddressMatcher, AddressMatcherType, DeviceClass, InterfaceMatcher, Matchers, PortMatcher,
    TransportProtocolMatcher,
};
use fidl_fuchsia_net_interfaces as fnet_interfaces;
use net_types::ip::{Ip, IpInvariant};

use crate::ingress::{
    IcmpSocket, IrrelevantToTest, Ports, SocketType, Subnets, TcpSocket, UdpSocket,
};

pub(crate) trait Matcher: Copy {
    type SocketType: SocketType;

    async fn matcher<I: Ip>(
        &self,
        interface: &netemul::TestInterface<'_>,
        subnets: Subnets,
        ports: Ports,
    ) -> Matchers;
}

#[derive(Clone, Copy)]
pub(crate) enum Inversion {
    Default,
    InverseMatch,
}

#[derive(Clone, Copy)]
pub(crate) struct AllTraffic;

impl Matcher for AllTraffic {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        Matchers::default()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InterfaceId;

impl Matcher for InterfaceId {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        interface: &netemul::TestInterface<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        // NOTE: these tests are for the INGRESS and LOCAL_INGRESS hooks, so we are
        // doing interface matching on the input interface.
        Matchers {
            in_interface: Some(InterfaceMatcher::Id(
                NonZeroU64::new(interface.id()).expect("interface ID should be nonzero"),
            )),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InterfaceName;

impl Matcher for InterfaceName {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        interface: &netemul::TestInterface<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        // NOTE: these tests are for the INGRESS and LOCAL_INGRESS hooks, so we are
        // doing interface matching on the input interface.
        Matchers {
            in_interface: Some(InterfaceMatcher::Name(
                interface.get_interface_name().await.expect("get interface name"),
            )),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InterfaceDeviceClass;

impl Matcher for InterfaceDeviceClass {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        interface: &netemul::TestInterface<'_>,
        _subnets: Subnets,
        _ports: Ports,
    ) -> Matchers {
        Matchers {
            // NOTE: these tests are for the INGRESS and LOCAL_INGRESS hooks, so we are
            // doing interface matching on the input interface.
            in_interface: Some(InterfaceMatcher::DeviceClass(
                match interface.get_device_class().await.expect("get device class") {
                    fnet_interfaces::DeviceClass::Loopback(fnet_interfaces::Empty {}) => {
                        DeviceClass::Loopback
                    }
                    fnet_interfaces::DeviceClass::Device(device) => DeviceClass::Device(device),
                },
            )),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SrcAddressSubnet(pub(crate) Inversion);

impl Matcher for SrcAddressSubnet {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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

#[derive(Clone, Copy)]
pub(crate) struct SrcAddressRange(pub(crate) Inversion);

impl Matcher for SrcAddressRange {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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

#[derive(Clone, Copy)]
pub(crate) struct DstAddressSubnet(pub(crate) Inversion);

impl Matcher for DstAddressSubnet {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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

#[derive(Clone, Copy)]
pub(crate) struct DstAddressRange(pub(crate) Inversion);

impl Matcher for DstAddressRange {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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

#[derive(Clone, Copy)]
pub(crate) struct Tcp;

impl Matcher for Tcp {
    type SocketType = TcpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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

#[derive(Clone, Copy)]
pub(crate) struct TcpSrcPort(pub(crate) Inversion);

impl Matcher for TcpSrcPort {
    type SocketType = TcpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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
                        PortMatcher::new(dst, dst, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
                dst_port: None,
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TcpDstPort(pub(crate) Inversion);

impl Matcher for TcpDstPort {
    type SocketType = TcpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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
                        PortMatcher::new(src, src, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Udp;

impl Matcher for Udp {
    type SocketType = UdpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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

#[derive(Clone, Copy)]
pub(crate) struct UdpSrcPort(pub(crate) Inversion);

impl Matcher for UdpSrcPort {
    type SocketType = UdpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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
                        PortMatcher::new(dst, dst, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
                dst_port: None,
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct UdpDstPort(pub(crate) Inversion);

impl Matcher for UdpDstPort {
    type SocketType = UdpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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
                        PortMatcher::new(src, src, /* invert */ true)
                            .expect("should be valid port range")
                    }
                }),
            }),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Icmp;

impl Matcher for Icmp {
    type SocketType = IcmpSocket;

    async fn matcher<I: Ip>(
        &self,
        _interface: &netemul::TestInterface<'_>,
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
