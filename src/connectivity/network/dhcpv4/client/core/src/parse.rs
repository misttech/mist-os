// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Parsing and serialization of DHCP messages

use packet::{InnerPacketBuilder, ParseBuffer as _, Serializer};
use packet_formats::ip::IpPacket as _;
use std::{
    collections::BTreeSet,
    net::Ipv4Addr,
    num::{NonZeroU16, NonZeroU32},
};

#[derive(thiserror::Error, Debug)]
pub(crate) enum ParseError {
    #[error("parsing IPv4 packet")]
    Ipv4(packet_formats::error::IpParseError<net_types::ip::Ipv4>),
    #[error("IPv4 packet protocol was not UDP")]
    NotUdp,
    #[error("parsing UDP datagram")]
    Udp(packet_formats::error::ParseError),
    #[error("incoming packet destined for wrong port")]
    WrongPort(NonZeroU16),
    #[error("parsing DHCP message")]
    Dhcp(dhcp_protocol::ProtocolError),
}

/// Parses a DHCP message from the bytes of an IP packet. This function does not
/// expect to parse a packet with link-layer headers; the buffer may only
/// include bytes for the IP layer and above.
/// NOTE: does not handle IP fragmentation.
pub(crate) fn parse_dhcp_message_from_ip_packet(
    mut bytes: &[u8],
    expected_dst_port: NonZeroU16,
) -> Result<dhcp_protocol::Message, ParseError> {
    let ip_packet =
        bytes.parse::<packet_formats::ipv4::Ipv4Packet<_>>().map_err(ParseError::Ipv4)?;
    match ip_packet.proto() {
        packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp) => (),
        packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Tcp)
        | packet_formats::ip::Ipv4Proto::Icmp
        | packet_formats::ip::Ipv4Proto::Igmp
        | packet_formats::ip::Ipv4Proto::Other(_) => return Err(ParseError::NotUdp),
    };
    let mut ip_packet_body = ip_packet.body();

    let udp_packet = ip_packet_body
        .parse_with::<_, packet_formats::udp::UdpPacket<_>>(packet_formats::udp::UdpParseArgs::new(
            ip_packet.src_ip(),
            ip_packet.dst_ip(),
        ))
        .map_err(ParseError::Udp)?;
    let dst_port = udp_packet.dst_port();
    if dst_port != expected_dst_port {
        return Err(ParseError::WrongPort(dst_port));
    }
    dhcp_protocol::Message::from_buffer(udp_packet.body()).map_err(ParseError::Dhcp)
}

const DEFAULT_TTL: u8 = 64;

/// Serializes a DHCP message to the bytes of an IP packet. Includes IP header
/// but not link-layer headers.
pub(crate) fn serialize_dhcp_message_to_ip_packet(
    message: dhcp_protocol::Message,
    src_ip: impl Into<net_types::ip::Ipv4Addr>,
    src_port: NonZeroU16,
    dst_ip: impl Into<net_types::ip::Ipv4Addr>,
    dst_port: NonZeroU16,
) -> impl AsRef<[u8]> {
    let message = message.serialize();
    let src_ip = src_ip.into();
    let dst_ip = dst_ip.into();

    let udp_builder =
        packet_formats::udp::UdpPacketBuilder::new(src_ip, dst_ip, Some(src_port), dst_port);

    let ipv4_builder = packet_formats::ipv4::Ipv4PacketBuilder::new(
        src_ip,
        dst_ip,
        DEFAULT_TTL,
        packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp),
    );

    match message
        .into_serializer()
        .encapsulate(udp_builder)
        .encapsulate(ipv4_builder)
        .serialize_vec_outer()
    {
        Ok(buf) => buf,
        Err(e) => {
            let (e, _serializer) = e;
            match e {
                packet::SerializeError::Alloc(infallible) => match infallible {},
                packet::SerializeError::SizeLimitExceeded => {
                    unreachable!("no MTU constraints on serializer")
                }
            }
        }
    }
}

/// Reasons that an incoming DHCP message might be discarded during Selecting
/// state.
#[derive(thiserror::Error, Debug, PartialEq)]
pub(crate) enum SelectingIncomingMessageError {
    #[error("got op = {0}, want op = BOOTREPLY")]
    NotBootReply(dhcp_protocol::OpCode),
    #[error("no DHCP message type")]
    NoDhcpMessageType,
    #[error("got DHCP message type = {0}, wanted DHCPOFFER")]
    NotDhcpOffer(dhcp_protocol::MessageType),
    #[error("saw two DHCP options with same option code: {0}")]
    DuplicateOption(dhcp_protocol::OptionCode),
    #[error("missing field: {0}")]
    BuilderMissingField(&'static str),
    #[error("server identifier was the unspecified address")]
    UnspecifiedServerIdentifier,
    #[error("yiaddr was the unspecified address")]
    UnspecifiedYiaddr,
}

impl From<derive_builder::UninitializedFieldError> for SelectingIncomingMessageError {
    fn from(value: derive_builder::UninitializedFieldError) -> Self {
        // `derive_builder::UninitializedFieldError` cannot be destructured
        // because its fields are private.
        Self::BuilderMissingField(value.field_name())
    }
}

/// Extracts the fields from a DHCP message incoming during Selecting state that
/// should be used during Requesting state.
pub(crate) fn fields_to_retain_from_selecting(
    message: dhcp_protocol::Message,
) -> Result<FieldsFromOfferToUseInRequest, SelectingIncomingMessageError> {
    let dhcp_protocol::Message {
        op,
        xid: _,
        secs: _,
        bdcast_flag: _,
        ciaddr: _,
        yiaddr,
        siaddr: _,
        giaddr: _,
        chaddr: _,
        sname: _,
        file: _,
        options,
    } = message;

    match op {
        dhcp_protocol::OpCode::BOOTREQUEST => {
            // Per the table RFC 2131 section 4.3.1, the DHCPOFFER 'op' field
            // must be BOOTREPLY.
            return Err(SelectingIncomingMessageError::NotBootReply(op));
        }
        dhcp_protocol::OpCode::BOOTREPLY => {}
    };

    let mut found_message_type: Option<dhcp_protocol::MessageType> = None;
    let mut builder = FieldsFromOfferToUseInRequestBuilder::default();
    builder.ip_address_to_request(yiaddr)?;

    let mut seen_options = BTreeSet::new();

    for option in options {
        let option_code = option.code();
        let newly_inserted = seen_options.insert(option_code);
        if !newly_inserted {
            return Err(SelectingIncomingMessageError::DuplicateOption(option_code));
        }
        match option {
            dhcp_protocol::DhcpOption::ServerIdentifier(addr) => {
                builder.server_identifier(addr)?;
            }
            dhcp_protocol::DhcpOption::IpAddressLeaseTime(time_in_secs) => {
                match NonZeroU32::try_from(time_in_secs) {
                    Err(e) => {
                        let _: std::num::TryFromIntError = e;
                    }
                    Ok(time_in_secs) => {
                        builder.ip_address_lease_time_secs(time_in_secs).ignore_unused_result();
                    }
                }
            }
            dhcp_protocol::DhcpOption::DhcpMessageType(message_type) => match message_type {
                dhcp_protocol::MessageType::DHCPOFFER => {
                    let _: &mut dhcp_protocol::MessageType =
                        found_message_type.insert(message_type);
                }
                dhcp_protocol::MessageType::DHCPDISCOVER
                | dhcp_protocol::MessageType::DHCPREQUEST
                | dhcp_protocol::MessageType::DHCPDECLINE
                | dhcp_protocol::MessageType::DHCPACK
                | dhcp_protocol::MessageType::DHCPNAK
                | dhcp_protocol::MessageType::DHCPRELEASE
                | dhcp_protocol::MessageType::DHCPINFORM => {
                    return Err(SelectingIncomingMessageError::NotDhcpOffer(message_type))
                }
            },
            dhcp_protocol::DhcpOption::Pad()
            | dhcp_protocol::DhcpOption::End()
            | dhcp_protocol::DhcpOption::SubnetMask(_)
            | dhcp_protocol::DhcpOption::TimeOffset(_)
            | dhcp_protocol::DhcpOption::Router(_)
            | dhcp_protocol::DhcpOption::TimeServer(_)
            | dhcp_protocol::DhcpOption::NameServer(_)
            | dhcp_protocol::DhcpOption::DomainNameServer(_)
            | dhcp_protocol::DhcpOption::LogServer(_)
            | dhcp_protocol::DhcpOption::CookieServer(_)
            | dhcp_protocol::DhcpOption::LprServer(_)
            | dhcp_protocol::DhcpOption::ImpressServer(_)
            | dhcp_protocol::DhcpOption::ResourceLocationServer(_)
            | dhcp_protocol::DhcpOption::HostName(_)
            | dhcp_protocol::DhcpOption::BootFileSize(_)
            | dhcp_protocol::DhcpOption::MeritDumpFile(_)
            | dhcp_protocol::DhcpOption::DomainName(_)
            | dhcp_protocol::DhcpOption::SwapServer(_)
            | dhcp_protocol::DhcpOption::RootPath(_)
            | dhcp_protocol::DhcpOption::ExtensionsPath(_)
            | dhcp_protocol::DhcpOption::IpForwarding(_)
            | dhcp_protocol::DhcpOption::NonLocalSourceRouting(_)
            | dhcp_protocol::DhcpOption::PolicyFilter(_)
            | dhcp_protocol::DhcpOption::MaxDatagramReassemblySize(_)
            | dhcp_protocol::DhcpOption::DefaultIpTtl(_)
            | dhcp_protocol::DhcpOption::PathMtuAgingTimeout(_)
            | dhcp_protocol::DhcpOption::PathMtuPlateauTable(_)
            | dhcp_protocol::DhcpOption::InterfaceMtu(_)
            | dhcp_protocol::DhcpOption::AllSubnetsLocal(_)
            | dhcp_protocol::DhcpOption::BroadcastAddress(_)
            | dhcp_protocol::DhcpOption::PerformMaskDiscovery(_)
            | dhcp_protocol::DhcpOption::MaskSupplier(_)
            | dhcp_protocol::DhcpOption::PerformRouterDiscovery(_)
            | dhcp_protocol::DhcpOption::RouterSolicitationAddress(_)
            | dhcp_protocol::DhcpOption::StaticRoute(_)
            | dhcp_protocol::DhcpOption::TrailerEncapsulation(_)
            | dhcp_protocol::DhcpOption::ArpCacheTimeout(_)
            | dhcp_protocol::DhcpOption::EthernetEncapsulation(_)
            | dhcp_protocol::DhcpOption::TcpDefaultTtl(_)
            | dhcp_protocol::DhcpOption::TcpKeepaliveInterval(_)
            | dhcp_protocol::DhcpOption::TcpKeepaliveGarbage(_)
            | dhcp_protocol::DhcpOption::NetworkInformationServiceDomain(_)
            | dhcp_protocol::DhcpOption::NetworkInformationServers(_)
            | dhcp_protocol::DhcpOption::NetworkTimeProtocolServers(_)
            | dhcp_protocol::DhcpOption::VendorSpecificInformation(_)
            | dhcp_protocol::DhcpOption::NetBiosOverTcpipNameServer(_)
            | dhcp_protocol::DhcpOption::NetBiosOverTcpipDatagramDistributionServer(_)
            | dhcp_protocol::DhcpOption::NetBiosOverTcpipNodeType(_)
            | dhcp_protocol::DhcpOption::NetBiosOverTcpipScope(_)
            | dhcp_protocol::DhcpOption::XWindowSystemFontServer(_)
            | dhcp_protocol::DhcpOption::XWindowSystemDisplayManager(_)
            | dhcp_protocol::DhcpOption::NetworkInformationServicePlusDomain(_)
            | dhcp_protocol::DhcpOption::NetworkInformationServicePlusServers(_)
            | dhcp_protocol::DhcpOption::MobileIpHomeAgent(_)
            | dhcp_protocol::DhcpOption::SmtpServer(_)
            | dhcp_protocol::DhcpOption::Pop3Server(_)
            | dhcp_protocol::DhcpOption::NntpServer(_)
            | dhcp_protocol::DhcpOption::DefaultWwwServer(_)
            | dhcp_protocol::DhcpOption::DefaultFingerServer(_)
            | dhcp_protocol::DhcpOption::DefaultIrcServer(_)
            | dhcp_protocol::DhcpOption::StreetTalkServer(_)
            | dhcp_protocol::DhcpOption::StreetTalkDirectoryAssistanceServer(_)
            | dhcp_protocol::DhcpOption::RequestedIpAddress(_)
            | dhcp_protocol::DhcpOption::OptionOverload(_)
            | dhcp_protocol::DhcpOption::TftpServerName(_)
            | dhcp_protocol::DhcpOption::BootfileName(_)
            | dhcp_protocol::DhcpOption::ParameterRequestList(_)
            | dhcp_protocol::DhcpOption::Message(_)
            | dhcp_protocol::DhcpOption::MaxDhcpMessageSize(_)
            | dhcp_protocol::DhcpOption::RenewalTimeValue(_)
            | dhcp_protocol::DhcpOption::RebindingTimeValue(_)
            | dhcp_protocol::DhcpOption::VendorClassIdentifier(_)
            | dhcp_protocol::DhcpOption::ClientIdentifier(_) => {}
        }
    }

    if found_message_type.is_none() {
        return Err(SelectingIncomingMessageError::NoDhcpMessageType);
    }

    builder.build()
}

#[derive(Debug, Clone, PartialEq, derive_builder::Builder)]
#[builder(build_fn(error = "SelectingIncomingMessageError"))]
/// Fields from a DHCPOFFER that should be used while building a DHCPREQUEST.
pub(crate) struct FieldsFromOfferToUseInRequest {
    #[builder(setter(custom))]
    pub(crate) server_identifier: net_types::SpecifiedAddr<net_types::ip::Ipv4Addr>,
    #[builder(setter(strip_option), default)]
    pub(crate) ip_address_lease_time_secs: Option<NonZeroU32>,
    #[builder(setter(custom))]
    pub(crate) ip_address_to_request: net_types::SpecifiedAddr<net_types::ip::Ipv4Addr>,
}

impl FieldsFromOfferToUseInRequestBuilder {
    fn ignore_unused_result(&mut self) {}

    fn server_identifier(&mut self, addr: Ipv4Addr) -> Result<(), SelectingIncomingMessageError> {
        self.server_identifier = Some(
            net_types::ip::Ipv4Addr::from(addr)
                .try_into()
                .map_err(|()| SelectingIncomingMessageError::UnspecifiedServerIdentifier)?,
        );
        Ok(())
    }

    fn ip_address_to_request(
        &mut self,
        addr: Ipv4Addr,
    ) -> Result<(), SelectingIncomingMessageError> {
        self.ip_address_to_request = Some(
            net_types::ip::Ipv4Addr::from(addr)
                .try_into()
                .map_err(|()| SelectingIncomingMessageError::UnspecifiedYiaddr)?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use net_declare::{net_ip_v4, net_mac, std_ip_v4};
    use std::net::Ipv4Addr;
    use test_case::test_case;

    const SERVER_PORT: NonZeroU16 = nonzero_ext::nonzero!(dhcp_protocol::SERVER_PORT);
    const CLIENT_PORT: NonZeroU16 = nonzero_ext::nonzero!(dhcp_protocol::CLIENT_PORT);

    #[test]
    fn serialize_parse_roundtrip() {
        let make_message = || dhcp_protocol::Message {
            op: dhcp_protocol::OpCode::BOOTREQUEST,
            xid: 124,
            secs: 99,
            bdcast_flag: false,
            ciaddr: net_ip_v4!("1.2.3.4").into(),
            yiaddr: net_ip_v4!("5.6.7.8").into(),
            siaddr: net_ip_v4!("9.10.11.12").into(),
            giaddr: net_ip_v4!("13.14.15.16").into(),
            chaddr: net_mac!("17:18:19:20:21:22"),
            sname: "this is a sname".to_owned(),
            file: "this is the boot filename".to_owned(),
            options: vec![
                dhcp_protocol::DhcpOption::DhcpMessageType(
                    dhcp_protocol::MessageType::DHCPDISCOVER,
                ),
                dhcp_protocol::DhcpOption::RequestedIpAddress(net_ip_v4!("5.6.7.8").into()),
            ],
        };
        let packet = serialize_dhcp_message_to_ip_packet(
            make_message(),
            Ipv4Addr::UNSPECIFIED,
            CLIENT_PORT,
            Ipv4Addr::BROADCAST,
            SERVER_PORT,
        );
        let parsed_message = parse_dhcp_message_from_ip_packet(packet.as_ref(), SERVER_PORT);

        assert_eq!(make_message(), parsed_message.unwrap());
    }

    #[test]
    fn nonsense() {
        assert_matches!(
            parse_dhcp_message_from_ip_packet(
                &[0xD, 0xE, 0xA, 0xD, 0xB, 0xE, 0xE, 0xF],
                nonzero_ext::nonzero!(1u16)
            ),
            Err(ParseError::Ipv4(parse_error)) => {
                assert_eq!(parse_error, packet_formats::error::IpParseError::Parse { error: packet_formats::error::ParseError::Format })
            }
        )
    }

    #[test]
    fn not_udp() {
        let src_ip = Ipv4Addr::UNSPECIFIED.into();
        let dst_ip = Ipv4Addr::BROADCAST.into();
        let tcp_builder: packet_formats::tcp::TcpSegmentBuilder<net_types::ip::Ipv4Addr> =
            packet_formats::tcp::TcpSegmentBuilder::new(
                src_ip,
                dst_ip,
                CLIENT_PORT,
                SERVER_PORT,
                0,
                None,
                0,
            );
        let ipv4_builder = packet_formats::ipv4::Ipv4PacketBuilder::new(
            src_ip,
            dst_ip,
            DEFAULT_TTL,
            packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Tcp),
        );
        let bytes = vec![1, 2, 3, 4, 5]
            .into_serializer()
            .encapsulate(tcp_builder)
            .encapsulate(ipv4_builder)
            .serialize_vec_outer()
            .expect("serialize error");

        assert_matches!(
            parse_dhcp_message_from_ip_packet(bytes.as_ref(), nonzero_ext::nonzero!(1u16)),
            Err(ParseError::NotUdp)
        );
    }

    #[test]
    fn wrong_port() {
        let src_ip = Ipv4Addr::UNSPECIFIED.into();
        let dst_ip = Ipv4Addr::BROADCAST.into();

        let udp_builder: packet_formats::udp::UdpPacketBuilder<net_types::ip::Ipv4Addr> =
            packet_formats::udp::UdpPacketBuilder::new(
                src_ip,
                dst_ip,
                Some(CLIENT_PORT),
                SERVER_PORT,
            );
        let ipv4_builder = packet_formats::ipv4::Ipv4PacketBuilder::new(
            src_ip,
            dst_ip,
            DEFAULT_TTL,
            packet_formats::ip::Ipv4Proto::Proto(packet_formats::ip::IpProto::Udp),
        );

        let bytes = "hello_world"
            .bytes()
            .collect::<Vec<_>>()
            .into_serializer()
            .encapsulate(udp_builder)
            .encapsulate(ipv4_builder)
            .serialize_vec_outer()
            .expect("serialize error");

        let result = parse_dhcp_message_from_ip_packet(bytes.as_ref(), CLIENT_PORT);
        assert_matches!(result, Err(ParseError::WrongPort(port)) => assert_eq!(port, SERVER_PORT));
    }

    #[derive(Debug)]
    enum FieldsFromOfferCase {
        AcceptedWithLeaseTime,
        AcceptedWithNoLeaseTime,
        UnspecifiedServerIdentifier,
        NoServerIdentifier,
        UnspecifiedYiaddr,
        NotBootReply,
        WrongDhcpMessageType,
        NoDhcpMessageType,
        DuplicateOption,
    }

    #[test_case(FieldsFromOfferCase::AcceptedWithLeaseTime ; "accepts good offer with lease time")]
    #[test_case(FieldsFromOfferCase::AcceptedWithNoLeaseTime ; "accepts good offer without lease time")]
    #[test_case(FieldsFromOfferCase::UnspecifiedServerIdentifier ; "rejects offer with unspecified server identifier")]
    #[test_case(FieldsFromOfferCase::NoServerIdentifier ; "rejects offer with no server identifier option")]
    #[test_case(FieldsFromOfferCase::UnspecifiedYiaddr ; "rejects offer with unspecified yiaddr")]
    #[test_case(FieldsFromOfferCase::NotBootReply ; "rejects offer with that isn't a bootreply")]
    #[test_case(FieldsFromOfferCase::WrongDhcpMessageType ; "rejects offer with wrong DHCP message type")]
    #[test_case(FieldsFromOfferCase::NoDhcpMessageType ; "rejects offer with no DHCP message type option")]
    #[test_case(FieldsFromOfferCase::DuplicateOption ; "rejects offer with duplicate DHCP option")]
    fn fields_from_offer_to_use_in_request(case: FieldsFromOfferCase) {
        use super::fields_to_retain_from_selecting as fields;
        const SERVER_IP: Ipv4Addr = std_ip_v4!("192.168.1.1");
        const LEASE_LENGTH_SECS: u32 = 100;
        const YIADDR: Ipv4Addr = std_ip_v4!("192.168.1.5");

        let message = dhcp_protocol::Message {
            op: match case {
                FieldsFromOfferCase::NotBootReply => dhcp_protocol::OpCode::BOOTREQUEST,
                _ => dhcp_protocol::OpCode::BOOTREPLY,
            },
            xid: 1,
            secs: 0,
            bdcast_flag: false,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: match case {
                FieldsFromOfferCase::UnspecifiedYiaddr => Ipv4Addr::UNSPECIFIED,
                _ => YIADDR,
            },
            siaddr: Ipv4Addr::UNSPECIFIED,
            giaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: net_mac!("01:02:03:04:05:06"),
            sname: String::new(),
            file: String::new(),
            options: (match case {
                FieldsFromOfferCase::WrongDhcpMessageType => Some(
                    dhcp_protocol::DhcpOption::DhcpMessageType(dhcp_protocol::MessageType::DHCPACK),
                ),
                FieldsFromOfferCase::NoDhcpMessageType => None,
                _ => Some(dhcp_protocol::DhcpOption::DhcpMessageType(
                    dhcp_protocol::MessageType::DHCPOFFER,
                )),
            })
            .into_iter()
            .chain(
                match case {
                    FieldsFromOfferCase::NoServerIdentifier => None,
                    FieldsFromOfferCase::UnspecifiedServerIdentifier => Some(Ipv4Addr::UNSPECIFIED),
                    _ => Some(SERVER_IP),
                }
                .map(dhcp_protocol::DhcpOption::ServerIdentifier),
            )
            .chain(match case {
                FieldsFromOfferCase::AcceptedWithLeaseTime => {
                    Some(dhcp_protocol::DhcpOption::IpAddressLeaseTime(LEASE_LENGTH_SECS))
                }
                _ => None,
            })
            .chain(
                match case {
                    FieldsFromOfferCase::DuplicateOption => Some([
                        dhcp_protocol::DhcpOption::DomainName("example.com".to_owned()),
                        dhcp_protocol::DhcpOption::DomainName("example.com".to_owned()),
                    ]),
                    _ => None,
                }
                .into_iter()
                .flatten(),
            )
            .collect(),
        };

        let fields_from_offer_result = fields(message);

        let expected = match case {
            FieldsFromOfferCase::AcceptedWithLeaseTime => Ok(FieldsFromOfferToUseInRequest {
                server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                    .try_into()
                    .expect("should be specified"),
                ip_address_lease_time_secs: Some(nonzero_ext::nonzero!(LEASE_LENGTH_SECS)),
                ip_address_to_request: net_types::ip::Ipv4Addr::from(YIADDR)
                    .try_into()
                    .expect("should be specified"),
            }),
            FieldsFromOfferCase::AcceptedWithNoLeaseTime => Ok(FieldsFromOfferToUseInRequest {
                server_identifier: net_types::ip::Ipv4Addr::from(SERVER_IP)
                    .try_into()
                    .expect("should be specified"),
                ip_address_lease_time_secs: None,
                ip_address_to_request: net_types::ip::Ipv4Addr::from(YIADDR)
                    .try_into()
                    .expect("should be specified"),
            }),
            FieldsFromOfferCase::UnspecifiedServerIdentifier => {
                Err(SelectingIncomingMessageError::UnspecifiedServerIdentifier)
            }
            FieldsFromOfferCase::NoServerIdentifier => {
                Err(SelectingIncomingMessageError::BuilderMissingField("server_identifier"))
            }
            FieldsFromOfferCase::UnspecifiedYiaddr => {
                Err(SelectingIncomingMessageError::UnspecifiedYiaddr)
            }
            FieldsFromOfferCase::NotBootReply => {
                Err(SelectingIncomingMessageError::NotBootReply(dhcp_protocol::OpCode::BOOTREQUEST))
            }
            FieldsFromOfferCase::WrongDhcpMessageType => Err(
                SelectingIncomingMessageError::NotDhcpOffer(dhcp_protocol::MessageType::DHCPACK),
            ),
            FieldsFromOfferCase::NoDhcpMessageType => {
                Err(SelectingIncomingMessageError::NoDhcpMessageType)
            }
            FieldsFromOfferCase::DuplicateOption => {
                Err(SelectingIncomingMessageError::DuplicateOption(
                    dhcp_protocol::OptionCode::DomainName,
                ))
            }
        };

        assert_eq!(fields_from_offer_result, expected);
    }
}
