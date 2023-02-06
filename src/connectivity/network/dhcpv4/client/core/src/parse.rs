// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Parsing and serialization of DHCP messages

#[todo_unused("https://fxbug.dev/81593")]
use packet::{InnerPacketBuilder, ParseBuffer as _, Serializer};
#[todo_unused("https://fxbug.dev/81593")]
use packet_formats::ip::IpPacket as _;
#[todo_unused("https://fxbug.dev/81593")]
use std::num::NonZeroU16;
use todo_unused::todo_unused;

#[todo_unused("https://fxbug.dev/81593")]
#[derive(thiserror::Error, Debug)]
enum ParseError {
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

#[todo_unused("https://fxbug.dev/81593")]
/// Parses a DHCP message from the bytes of an IP packet. This function does not
/// expect to parse a packet with link-layer headers; the buffer may only
/// include bytes for the IP layer and above.
/// NOTE: does not handle IP fragmentation.
fn parse_dhcp_message_from_ip_packet(
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

#[todo_unused("https://fxbug.dev/81593")]
const DEFAULT_TTL: u8 = 64;

#[todo_unused("https://fxbug.dev/81593")]
/// Serializes a DHCP message to the bytes of an IP packet. Includes IP header
/// but not link-layer headers.
fn serialize_dhcp_message_to_ip_packet(
    message: dhcp_protocol::Message,
    src_ip: net_types::ip::Ipv4Addr,
    src_port: NonZeroU16,
    dst_ip: net_types::ip::Ipv4Addr,
    dst_port: NonZeroU16,
) -> impl AsRef<[u8]> {
    let message = message.serialize();

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
                packet::SerializeError::Mtu => unreachable!("no MTU constraints on serializer"),
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;
    use net_declare::{net_ip_v4, net_mac};
    use std::net::Ipv4Addr;

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
            Ipv4Addr::UNSPECIFIED.into(),
            CLIENT_PORT,
            Ipv4Addr::BROADCAST.into(),
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
}
