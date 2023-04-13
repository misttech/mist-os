// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Parsing and serialization of DHCP messages

use dhcp_protocol::{AtLeast, AtMostBytes};
use packet::{InnerPacketBuilder, ParseBuffer as _, Serializer};
use packet_formats::ip::IpPacket as _;
use std::{
    net::Ipv4Addr,
    num::{NonZeroU16, NonZeroU32, TryFromIntError},
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

#[derive(derive_builder::Builder, Debug, PartialEq)]
#[builder(private, build_fn(error = "CommonIncomingMessageError"))]
struct CommonIncomingMessageFields {
    op_code: dhcp_protocol::OpCode,
    message_type: dhcp_protocol::MessageType,
    #[builder(setter(custom), default)]
    server_identifier: Option<net_types::SpecifiedAddr<net_types::ip::Ipv4Addr>>,
    #[builder(setter(custom), default)]
    yiaddr: Option<net_types::SpecifiedAddr<net_types::ip::Ipv4Addr>>,
    #[builder(setter(strip_option), default)]
    ip_address_lease_time_secs: Option<NonZeroU32>,
    // While it's nonsensical to have a 0-valued lease time, it's somewhat more
    // reasonable to set the renewal time value to 0 (prompting the client to
    // begin the renewal process immediately).
    #[builder(setter(strip_option), default)]
    renewal_time_value_secs: Option<u32>,
    // Same holds for the rebinding time.
    #[builder(setter(strip_option), default)]
    rebinding_time_value_secs: Option<u32>,
    #[builder(default)]
    parameters: Vec<dhcp_protocol::DhcpOption>,
    #[builder(setter(strip_option), default)]
    message: Option<String>,
    #[builder(setter(strip_option), default)]
    client_identifier: Option<AtLeast<2, AtMostBytes<{ dhcp_protocol::U8_MAX_AS_USIZE }, Vec<u8>>>>,
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub(crate) enum CommonIncomingMessageError {
    #[error("server identifier was the unspecified address")]
    UnspecifiedServerIdentifier,
    #[error("missing: {0}")]
    BuilderMissingField(&'static str),
    #[error("duplicate option: {0:?}")]
    DuplicateOption(dhcp_protocol::OptionCode),
    #[error("option's inclusion violates protocol: {0:?}")]
    IllegallyIncludedOption(dhcp_protocol::OptionCode),
}

impl From<derive_builder::UninitializedFieldError> for CommonIncomingMessageError {
    fn from(value: derive_builder::UninitializedFieldError) -> Self {
        // `derive_builder::UninitializedFieldError` cannot be destructured
        // because its fields are private.
        Self::BuilderMissingField(value.field_name())
    }
}

impl CommonIncomingMessageFieldsBuilder {
    fn ignore_unused_result(&mut self) {}

    fn add_requested_parameter(&mut self, option: dhcp_protocol::DhcpOption) {
        let parameters = self.parameters.get_or_insert_with(Default::default);
        parameters.push(option)
    }

    fn server_identifier(&mut self, addr: Ipv4Addr) -> Result<(), CommonIncomingMessageError> {
        self.server_identifier = Some(Some(
            net_types::SpecifiedAddr::new(net_types::ip::Ipv4Addr::from(addr))
                .ok_or(CommonIncomingMessageError::UnspecifiedServerIdentifier)?,
        ));
        Ok(())
    }

    fn yiaddr(&mut self, addr: Ipv4Addr) {
        match net_types::SpecifiedAddr::new(net_types::ip::Ipv4Addr::from(addr)) {
            None => {
                // Unlike with the Server Identifier option, it is not an error
                // to set `yiaddr` to the unspecified address, as there is no
                // other way to indicate its absence (it has its own field in
                // the DHCP message rather than appearing in the list of
                // options).
            }
            Some(specified_addr) => {
                self.yiaddr = Some(Some(specified_addr));
            }
        }
    }
}

/// Represents a set of OptionCodes as an array of booleans.
struct OptionCodeSet {
    inner: [bool; dhcp_protocol::U8_MAX_AS_USIZE],
}

impl OptionCodeSet {
    fn new() -> Self {
        OptionCodeSet { inner: [false; dhcp_protocol::U8_MAX_AS_USIZE] }
    }

    /// Inserts `option_code` into the set, returning whether it was newly added.
    fn insert(&mut self, option_code: dhcp_protocol::OptionCode) -> bool {
        let option_code_index = usize::from(u8::from(option_code));
        if self.inner[option_code_index] {
            return false;
        }

        self.inner[option_code_index] = true;
        true
    }

    fn contains(&self, option_code: dhcp_protocol::OptionCode) -> bool {
        self.inner[usize::from(u8::from(option_code))]
    }
}

fn collect_common_fields(
    requested_parameters: &OptionCodeSet,
    dhcp_protocol::Message {
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
    }: dhcp_protocol::Message,
) -> Result<CommonIncomingMessageFields, CommonIncomingMessageError> {
    use dhcp_protocol::DhcpOption;

    let mut builder = CommonIncomingMessageFieldsBuilder::default();
    builder.op_code(op).yiaddr(yiaddr);

    let mut seen_options = OptionCodeSet::new();

    for option in options {
        let newly_seen = seen_options.insert(option.code());
        if !newly_seen {
            return Err(CommonIncomingMessageError::DuplicateOption(option.code()));
        }

        // From RFC 2131 section 4.3.1:
        // """
        // Option                    DHCPOFFER    DHCPACK               DHCPNAK
        // ------                    ---------    -------               -------
        // Requested IP address      MUST NOT     MUST NOT              MUST NOT
        // IP address lease time     MUST         MUST (DHCPREQUEST)    MUST NOT
        //                                        MUST NOT (DHCPINFORM)
        // Use 'file'/'sname' fields MAY          MAY                   MUST NOT
        // DHCP message type         DHCPOFFER    DHCPACK               DHCPNAK
        // Parameter request list    MUST NOT     MUST NOT              MUST NOT
        // Message                   SHOULD       SHOULD                SHOULD
        // Client identifier         MUST NOT     MUST NOT              MAY
        // Vendor class identifier   MAY          MAY                   MAY
        // Server identifier         MUST         MUST                  MUST
        // Maximum message size      MUST NOT     MUST NOT              MUST NOT
        // All others                MAY          MAY                   MUST NOT
        //
        //            Table 3:  Fields and options used by DHCP servers
        // """

        match &option {
            DhcpOption::IpAddressLeaseTime(value) => match NonZeroU32::try_from(*value) {
                Err(e) => {
                    let _: TryFromIntError = e;
                    tracing::warn!("dropping 0 lease time");
                }
                Ok(value) => {
                    builder.ip_address_lease_time_secs(value).ignore_unused_result();
                }
            },
            DhcpOption::DhcpMessageType(message_type) => {
                builder.message_type(*message_type).ignore_unused_result()
            }
            DhcpOption::ServerIdentifier(value) => {
                builder.server_identifier(*value)?;
            }
            DhcpOption::Message(message) => builder.message(message.clone()).ignore_unused_result(),
            DhcpOption::RenewalTimeValue(value) => {
                builder.renewal_time_value_secs(*value).ignore_unused_result()
            }
            DhcpOption::RebindingTimeValue(value) => {
                builder.rebinding_time_value_secs(*value).ignore_unused_result()
            }
            DhcpOption::ClientIdentifier(value) => {
                builder.client_identifier(value.clone()).ignore_unused_result();
            }
            DhcpOption::ParameterRequestList(_)
            | DhcpOption::RequestedIpAddress(_)
            | DhcpOption::MaxDhcpMessageSize(_) => {
                return Err(CommonIncomingMessageError::IllegallyIncludedOption(option.code()))
            }
            DhcpOption::Pad()
            | DhcpOption::End()
            | DhcpOption::SubnetMask(_)
            | DhcpOption::TimeOffset(_)
            | DhcpOption::Router(_)
            | DhcpOption::TimeServer(_)
            | DhcpOption::NameServer(_)
            | DhcpOption::DomainNameServer(_)
            | DhcpOption::LogServer(_)
            | DhcpOption::CookieServer(_)
            | DhcpOption::LprServer(_)
            | DhcpOption::ImpressServer(_)
            | DhcpOption::ResourceLocationServer(_)
            | DhcpOption::HostName(_)
            | DhcpOption::BootFileSize(_)
            | DhcpOption::MeritDumpFile(_)
            | DhcpOption::DomainName(_)
            | DhcpOption::SwapServer(_)
            | DhcpOption::RootPath(_)
            | DhcpOption::ExtensionsPath(_)
            | DhcpOption::IpForwarding(_)
            | DhcpOption::NonLocalSourceRouting(_)
            | DhcpOption::PolicyFilter(_)
            | DhcpOption::MaxDatagramReassemblySize(_)
            | DhcpOption::DefaultIpTtl(_)
            | DhcpOption::PathMtuAgingTimeout(_)
            | DhcpOption::PathMtuPlateauTable(_)
            | DhcpOption::InterfaceMtu(_)
            | DhcpOption::AllSubnetsLocal(_)
            | DhcpOption::BroadcastAddress(_)
            | DhcpOption::PerformMaskDiscovery(_)
            | DhcpOption::MaskSupplier(_)
            | DhcpOption::PerformRouterDiscovery(_)
            | DhcpOption::RouterSolicitationAddress(_)
            | DhcpOption::StaticRoute(_)
            | DhcpOption::TrailerEncapsulation(_)
            | DhcpOption::ArpCacheTimeout(_)
            | DhcpOption::EthernetEncapsulation(_)
            | DhcpOption::TcpDefaultTtl(_)
            | DhcpOption::TcpKeepaliveInterval(_)
            | DhcpOption::TcpKeepaliveGarbage(_)
            | DhcpOption::NetworkInformationServiceDomain(_)
            | DhcpOption::NetworkInformationServers(_)
            | DhcpOption::NetworkTimeProtocolServers(_)
            | DhcpOption::VendorSpecificInformation(_)
            | DhcpOption::NetBiosOverTcpipNameServer(_)
            | DhcpOption::NetBiosOverTcpipDatagramDistributionServer(_)
            | DhcpOption::NetBiosOverTcpipNodeType(_)
            | DhcpOption::NetBiosOverTcpipScope(_)
            | DhcpOption::XWindowSystemFontServer(_)
            | DhcpOption::XWindowSystemDisplayManager(_)
            | DhcpOption::NetworkInformationServicePlusDomain(_)
            | DhcpOption::NetworkInformationServicePlusServers(_)
            | DhcpOption::MobileIpHomeAgent(_)
            | DhcpOption::SmtpServer(_)
            | DhcpOption::Pop3Server(_)
            | DhcpOption::NntpServer(_)
            | DhcpOption::DefaultWwwServer(_)
            | DhcpOption::DefaultFingerServer(_)
            | DhcpOption::DefaultIrcServer(_)
            | DhcpOption::StreetTalkServer(_)
            | DhcpOption::StreetTalkDirectoryAssistanceServer(_)
            | DhcpOption::OptionOverload(_)
            | DhcpOption::TftpServerName(_)
            | DhcpOption::BootfileName(_)
            | DhcpOption::VendorClassIdentifier(_) => (),
        };

        if requested_parameters.contains(option.code()) {
            builder.add_requested_parameter(option);
        }
    }
    builder.build()
}

/// Reasons that an incoming DHCP message might be discarded during Selecting
/// state.
#[derive(thiserror::Error, Debug, PartialEq)]
pub(crate) enum SelectingIncomingMessageError {
    #[error("{0}")]
    CommonError(#[from] CommonIncomingMessageError),
    #[error("no server identifier")]
    NoServerIdentifier,
    #[error("got op = {0}, want op = BOOTREPLY")]
    NotBootReply(dhcp_protocol::OpCode),
    #[error("got DHCP message type = {0}, wanted DHCPOFFER")]
    NotDhcpOffer(dhcp_protocol::MessageType),
    #[error("yiaddr was the unspecified address")]
    UnspecifiedYiaddr,
}

/// Extracts the fields from a DHCP message incoming during Selecting state that
/// should be used during Requesting state.
pub(crate) fn fields_to_retain_from_selecting(
    message: dhcp_protocol::Message,
) -> Result<FieldsFromOfferToUseInRequest, SelectingIncomingMessageError> {
    let CommonIncomingMessageFields {
        op_code,
        message_type,
        server_identifier,
        yiaddr,
        ip_address_lease_time_secs,
        renewal_time_value_secs: _,
        rebinding_time_value_secs: _,
        parameters: _,
        message: _,
        client_identifier: _,
    } = collect_common_fields(&OptionCodeSet::new(), message)?;

    match op_code {
        dhcp_protocol::OpCode::BOOTREQUEST => {
            return Err(SelectingIncomingMessageError::NotBootReply(op_code))
        }
        dhcp_protocol::OpCode::BOOTREPLY => (),
    };

    match message_type {
        dhcp_protocol::MessageType::DHCPOFFER => (),
        dhcp_protocol::MessageType::DHCPDISCOVER
        | dhcp_protocol::MessageType::DHCPREQUEST
        | dhcp_protocol::MessageType::DHCPDECLINE
        | dhcp_protocol::MessageType::DHCPACK
        | dhcp_protocol::MessageType::DHCPNAK
        | dhcp_protocol::MessageType::DHCPRELEASE
        | dhcp_protocol::MessageType::DHCPINFORM => {
            return Err(SelectingIncomingMessageError::NotDhcpOffer(message_type))
        }
    };

    Ok(FieldsFromOfferToUseInRequest {
        server_identifier: server_identifier
            .ok_or(SelectingIncomingMessageError::NoServerIdentifier)?,
        ip_address_lease_time_secs,
        ip_address_to_request: yiaddr.ok_or(SelectingIncomingMessageError::UnspecifiedYiaddr)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
/// Fields from a DHCPOFFER that should be used while building a DHCPREQUEST.
pub(crate) struct FieldsFromOfferToUseInRequest {
    pub(crate) server_identifier: net_types::SpecifiedAddr<net_types::ip::Ipv4Addr>,
    pub(crate) ip_address_lease_time_secs: Option<NonZeroU32>,
    pub(crate) ip_address_to_request: net_types::SpecifiedAddr<net_types::ip::Ipv4Addr>,
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use dhcp_protocol::{CLIENT_PORT, SERVER_PORT};
    use net_declare::{net_ip_v4, net_mac, std_ip_v4};
    use std::net::Ipv4Addr;
    use test_case::test_case;

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
                Err(SelectingIncomingMessageError::CommonError(
                    CommonIncomingMessageError::UnspecifiedServerIdentifier,
                ))
            }
            FieldsFromOfferCase::NoServerIdentifier => {
                Err(SelectingIncomingMessageError::NoServerIdentifier)
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
                Err(SelectingIncomingMessageError::CommonError(
                    CommonIncomingMessageError::BuilderMissingField("message_type"),
                ))
            }
            FieldsFromOfferCase::DuplicateOption => {
                Err(SelectingIncomingMessageError::CommonError(
                    CommonIncomingMessageError::DuplicateOption(
                        dhcp_protocol::OptionCode::DomainName,
                    ),
                ))
            }
        };

        assert_eq!(fields_from_offer_result, expected);
    }
}
