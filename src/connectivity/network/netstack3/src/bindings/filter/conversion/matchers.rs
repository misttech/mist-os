// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext as fnet_filter_ext;
use net_types::ip::{GenericOverIp, Ip, IpInvariant};
use packet_formats::ip::{IpExt, IpProto, Ipv4Proto, Ipv6Proto};

use super::{ConversionResult, IpVersionMismatchError, IpVersionStrictness, TryConvertToCoreState};

impl TryConvertToCoreState for fnet_filter_ext::Matchers {
    type CoreState<I: IpExt> = netstack3_core::filter::PacketMatcher<I, fnet_filter::DeviceClass>;

    fn try_convert<I: IpExt>(
        self,
        ip_version_strictness: IpVersionStrictness,
    ) -> Result<ConversionResult<Self::CoreState<I>>, IpVersionMismatchError> {
        let Self { in_interface, out_interface, src_addr, dst_addr, transport_protocol } = self;

        let in_interface = match in_interface
            .map(|matcher| matcher.try_convert::<I>(ip_version_strictness))
            .transpose()?
        {
            Some(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
            Some(ConversionResult::State(matcher)) => Some(matcher),
            None => None,
        };
        let out_interface = match out_interface
            .map(|matcher| matcher.try_convert::<I>(ip_version_strictness))
            .transpose()?
        {
            Some(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
            Some(ConversionResult::State(matcher)) => Some(matcher),
            None => None,
        };
        let src_address = match src_addr
            .map(|matcher| matcher.try_convert::<I>(ip_version_strictness))
            .transpose()?
        {
            Some(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
            Some(ConversionResult::State(matcher)) => Some(matcher),
            None => None,
        };
        let dst_address = match dst_addr
            .map(|matcher| matcher.try_convert::<I>(ip_version_strictness))
            .transpose()?
        {
            Some(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
            Some(ConversionResult::State(matcher)) => Some(matcher),
            None => None,
        };
        let transport_protocol = match transport_protocol
            .map(|matcher| matcher.try_convert::<I>(ip_version_strictness))
            .transpose()?
        {
            Some(ConversionResult::Omit) => return Ok(ConversionResult::Omit),
            Some(ConversionResult::State(matcher)) => Some(matcher),
            None => None,
        };

        Ok(ConversionResult::State(netstack3_core::filter::PacketMatcher {
            in_interface,
            out_interface,
            src_address,
            dst_address,
            transport_protocol,
        }))
    }
}

impl TryConvertToCoreState for fnet_filter_ext::InterfaceMatcher {
    type CoreState<I: IpExt> = netstack3_core::filter::InterfaceMatcher<fnet_filter::DeviceClass>;

    fn try_convert<I: IpExt>(
        self,
        _ip_version_strictness: IpVersionStrictness,
    ) -> Result<ConversionResult<Self::CoreState<I>>, IpVersionMismatchError> {
        let matcher = match self {
            Self::Id(id) => netstack3_core::filter::InterfaceMatcher::Id(id),
            Self::Name(name) => netstack3_core::filter::InterfaceMatcher::Name(name),
            Self::DeviceClass(device_class) => {
                netstack3_core::filter::InterfaceMatcher::DeviceClass(device_class.into())
            }
        };
        Ok(ConversionResult::State(matcher))
    }
}

impl TryConvertToCoreState for fnet_filter_ext::AddressMatcher {
    type CoreState<I: IpExt> = netstack3_core::filter::AddressMatcher<I::Addr>;

    fn try_convert<I: IpExt>(
        self,
        ip_version_strictness: IpVersionStrictness,
    ) -> Result<ConversionResult<Self::CoreState<I>>, IpVersionMismatchError> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<I: IpExt>(
            Result<
                ConversionResult<netstack3_core::filter::AddressMatcherType<I::Addr>>,
                IpVersionMismatchError,
            >,
        );

        let Self { matcher, invert } = self;
        let matcher = match matcher {
            fnet_filter_ext::AddressMatcherType::Subnet(subnet) => {
                let fnet::Subnet { addr, prefix_len } = subnet.into();
                let addr = addr.into_ext();
                let Wrap(result) = I::map_ip::<_, Wrap<I>>(
                    (),
                    |()| match addr {
                        net_types::ip::IpAddr::V4(addr) => {
                            let subnet = net_types::ip::Subnet::new(addr, prefix_len)
                                .expect("subnet should be validated when change is pushed");
                            Wrap(Ok(ConversionResult::State(
                                netstack3_core::filter::AddressMatcherType::Subnet(subnet),
                            )))
                        }
                        net_types::ip::IpAddr::V6(_) => {
                            Wrap(ip_version_strictness.mismatch_result())
                        }
                    },
                    |()| match addr {
                        net_types::ip::IpAddr::V4(_) => {
                            Wrap(ip_version_strictness.mismatch_result())
                        }
                        net_types::ip::IpAddr::V6(addr) => {
                            let subnet = net_types::ip::Subnet::new(addr, prefix_len)
                                .expect("subnet should be validated when change is pushed");
                            Wrap(Ok(ConversionResult::State(
                                netstack3_core::filter::AddressMatcherType::Subnet(subnet),
                            )))
                        }
                    },
                );
                match result? {
                    ConversionResult::State(matcher) => matcher,
                    ConversionResult::Omit => return Ok(ConversionResult::Omit),
                }
            }
            fnet_filter_ext::AddressMatcherType::Range(range) => {
                let (start, end) = (range.start().into_ext(), range.end().into_ext());
                let Wrap(result) = I::map_ip::<_, Wrap<I>>(
                    (),
                    |()| match (start, end) {
                        (net_types::ip::IpAddr::V4(start), net_types::ip::IpAddr::V4(end)) => {
                            Wrap(Ok(ConversionResult::State(
                                netstack3_core::filter::AddressMatcherType::Range(start..=end),
                            )))
                        }
                        (net_types::ip::IpAddr::V6(_), net_types::ip::IpAddr::V6(_)) => {
                            Wrap(ip_version_strictness.mismatch_result())
                        }
                        _ => {
                            panic!("address range should be validated when change is pushed")
                        }
                    },
                    |()| match (start, end) {
                        (net_types::ip::IpAddr::V4(_), net_types::ip::IpAddr::V4(_)) => {
                            Wrap(ip_version_strictness.mismatch_result())
                        }
                        (net_types::ip::IpAddr::V6(start), net_types::ip::IpAddr::V6(end)) => {
                            Wrap(Ok(ConversionResult::State(
                                netstack3_core::filter::AddressMatcherType::Range(start..=end),
                            )))
                        }
                        _ => {
                            panic!("address range should be validated when change is pushed")
                        }
                    },
                );
                match result? {
                    ConversionResult::State(matcher) => matcher,
                    ConversionResult::Omit => return Ok(ConversionResult::Omit),
                }
            }
        };
        Ok(ConversionResult::State(netstack3_core::filter::AddressMatcher { matcher, invert }))
    }
}

impl TryConvertToCoreState for fnet_filter_ext::TransportProtocolMatcher {
    type CoreState<I: IpExt> = netstack3_core::filter::TransportProtocolMatcher<I::Proto>;

    fn try_convert<I: IpExt>(
        self,
        ip_version_strictness: IpVersionStrictness,
    ) -> Result<ConversionResult<Self::CoreState<I>>, IpVersionMismatchError> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        pub(super) struct Wrap<I: IpExt>(
            Result<ConversionResult<I::Proto>, IpVersionMismatchError>,
        );

        let into_core_port_matcher =
            |matcher: fnet_filter_ext::PortMatcher| netstack3_core::filter::PortMatcher {
                range: matcher.range().clone(),
                invert: matcher.invert,
            };

        let matcher = match self {
            fnet_filter_ext::TransportProtocolMatcher::Tcp { src_port, dst_port } => {
                netstack3_core::filter::TransportProtocolMatcher {
                    proto: I::map_ip(
                        IpInvariant(()),
                        |IpInvariant(())| Ipv4Proto::Proto(IpProto::Tcp),
                        |IpInvariant(())| Ipv6Proto::Proto(IpProto::Tcp),
                    ),
                    src_port: src_port.map(into_core_port_matcher),
                    dst_port: dst_port.map(into_core_port_matcher),
                }
            }
            fnet_filter_ext::TransportProtocolMatcher::Udp { src_port, dst_port } => {
                netstack3_core::filter::TransportProtocolMatcher {
                    proto: I::map_ip(
                        IpInvariant(()),
                        |IpInvariant(())| Ipv4Proto::Proto(IpProto::Udp),
                        |IpInvariant(())| Ipv6Proto::Proto(IpProto::Udp),
                    ),
                    src_port: src_port.map(into_core_port_matcher),
                    dst_port: dst_port.map(into_core_port_matcher),
                }
            }
            fnet_filter_ext::TransportProtocolMatcher::Icmp => {
                let Wrap(result) = I::map_ip::<_, Wrap<I>>(
                    IpInvariant(()),
                    |IpInvariant(())| Wrap(Ok(ConversionResult::State(Ipv4Proto::Icmp))),
                    |IpInvariant(())| Wrap(ip_version_strictness.mismatch_result()),
                );
                let proto = match result? {
                    ConversionResult::State(proto) => proto,
                    ConversionResult::Omit => return Ok(ConversionResult::Omit),
                };
                netstack3_core::filter::TransportProtocolMatcher {
                    proto,
                    src_port: None,
                    dst_port: None,
                }
            }
            fnet_filter_ext::TransportProtocolMatcher::Icmpv6 => {
                let Wrap(result) = I::map_ip::<_, Wrap<I>>(
                    IpInvariant(()),
                    |IpInvariant(())| Wrap(ip_version_strictness.mismatch_result()),
                    |IpInvariant(())| Wrap(Ok(ConversionResult::State(Ipv6Proto::Icmpv6))),
                );
                let proto = match result? {
                    ConversionResult::State(proto) => proto,
                    ConversionResult::Omit => return Ok(ConversionResult::Omit),
                };
                netstack3_core::filter::TransportProtocolMatcher {
                    proto,
                    src_port: None,
                    dst_port: None,
                }
            }
        };
        Ok(ConversionResult::State(matcher))
    }
}
