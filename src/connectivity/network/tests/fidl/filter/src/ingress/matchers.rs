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
use net_types::ip::IpInvariant;

use crate::ingress::{
    IcmpSocket, IrrelevantToTest, Ports, SocketType, TcpSocket, TestIpExt, TestRealm, UdpSocket,
};

pub(crate) trait Matcher: Copy {
    type SocketType: SocketType;

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, ports: Ports) -> Matchers;
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        Matchers::default()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InterfaceId;

impl Matcher for InterfaceId {
    type SocketType = IrrelevantToTest;

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let TestRealm { interface, .. } = realm;

        // NOTE: these tests are for the INGRESS hook, so we are doing interface
        // matching on the input interface.
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

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let TestRealm { interface, .. } = realm;

        // NOTE: these tests are for the INGRESS hook, so we are doing interface
        // matching on the input interface.
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

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let TestRealm { interface, .. } = realm;

        Matchers {
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

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let TestRealm { remote_subnet, .. } = realm;

        // For source address, match on the remote's subnet, because the traffic that
        // will be arriving on the INGRESS hook will be coming from that subnet.
        Matchers {
            src_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        fnet_ext::apply_subnet_mask(*remote_subnet)
                            .try_into()
                            .expect("subnet should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        I::OTHER_SUBNET.try_into().expect("subnet should be valid"),
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

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let TestRealm { remote_subnet, .. } = realm;

        // For source address, match on the remote's subnet, because the traffic that
        // will be arriving on the INGRESS hook will be coming from that subnet.
        Matchers {
            src_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange {
                            start: remote_subnet.addr,
                            end: remote_subnet.addr,
                        }
                        .try_into()
                        .expect("address range should be avlid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange {
                            start: I::OTHER_SUBNET.addr,
                            end: I::OTHER_SUBNET.addr,
                        }
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

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let TestRealm { local_subnet, .. } = realm;

        // For destination address, match on the local subnet, because the traffic that
        // will be arriving on the INGRESS hook will be destined to the local subnet.
        Matchers {
            dst_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        fnet_ext::apply_subnet_mask(*local_subnet)
                            .try_into()
                            .expect("subnet should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Subnet(
                        I::OTHER_SUBNET.try_into().expect("subnet should be valid"),
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

    async fn matcher<I: TestIpExt>(&self, realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let TestRealm { local_subnet, .. } = realm;

        // For destination address, match on the local subnet, because the traffic that
        // will be arriving on the INGRESS hook will be destined to the local subnet.
        Matchers {
            dst_addr: Some(match inversion {
                Inversion::Default => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange {
                            start: local_subnet.addr,
                            end: local_subnet.addr,
                        }
                        .try_into()
                        .expect("address range should be valid"),
                    ),
                    invert: false,
                },
                Inversion::InverseMatch => AddressMatcher {
                    matcher: AddressMatcherType::Range(
                        fnet_filter::AddressRange {
                            start: I::OTHER_SUBNET.addr,
                            end: I::OTHER_SUBNET.addr,
                        }
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let Ports { local, remote } = ports;

        // For source port, match on the remote port, because that is the port from
        // which traffic arriving on the INGRESS hook will be coming. If we are doing an
        // inverse match, match on the local port instead because that will never match
        // incoming traffic (and therefore will meet the criteria for an inverse match).
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(remote, remote, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        PortMatcher::new(local, local, /* invert */ true)
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let Ports { local, remote } = ports;

        // For destination port, match on the local port, because that is the port to
        // which traffic arriving on the INGRESS hook will be destined. If we are doing
        // an inverse match, match on the remote port instead because that will never
        // match incoming traffic (and therefore will meet the criteria for an inverse
        // match).
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: None,
                dst_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(local, local, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        PortMatcher::new(remote, remote, /* invert */ true)
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let Ports { local, remote } = ports;

        // For source port, match on the remote port, because that is the port from
        // which traffic arriving on the INGRESS hook will be coming. If we are doing an
        // inverse match, match on the local port instead because that will never match
        // incoming traffic (and therefore will meet the criteria for an inverse match).
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Udp {
                src_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(remote, remote, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        PortMatcher::new(local, local, /* invert */ true)
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, ports: Ports) -> Matchers {
        let Self(inversion) = self;
        let Ports { local, remote } = ports;

        // For destination port, match on the local port, because that is the port to
        // which traffic arriving on the INGRESS hook will be destined. If we are doing
        // an inverse match, match on the remote port instead because that will never
        // match incoming traffic (and therefore will meet the criteria for an inverse
        // match).
        Matchers {
            transport_protocol: Some(TransportProtocolMatcher::Udp {
                src_port: None,
                dst_port: Some(match inversion {
                    Inversion::Default => {
                        PortMatcher::new(local, local, /* invert */ false)
                            .expect("should be valid port range")
                    }
                    Inversion::InverseMatch => {
                        PortMatcher::new(remote, remote, /* invert */ true)
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

    async fn matcher<I: TestIpExt>(&self, _realm: &TestRealm<'_>, _ports: Ports) -> Matchers {
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
