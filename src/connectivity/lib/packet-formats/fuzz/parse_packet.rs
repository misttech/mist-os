// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use arbitrary::{Arbitrary, Unstructured};
use fuzz::fuzz;
use fuzz_util::Fuzzed;
use mdns::protocol::Message as MdnsMessage;
use net_types::{
    ethernet::Mac,
    ip::{Ipv4Addr, Ipv6Addr},
};
use netsvc_proto::{debuglog::DebugLogPacket, netboot::NetbootPacket, tftp::TftpPacket};
use packet::{BufferView, ParsablePacket};
use packet_formats::{
    arp::ArpPacket,
    ethernet::{EthernetFrame, EthernetFrameLengthCheck},
    icmp::{IcmpParseArgs, Icmpv4Packet, Icmpv6Packet},
    igmp::messages::IgmpPacket,
    ipv4::Ipv4Packet,
    ipv6::Ipv6Packet,
    tcp::{TcpParseArgs, TcpSegment},
    udp::{UdpPacket, UdpParseArgs},
};
use packet_formats_dhcp::v6::Message as Dhcpv6Message;
use tracing::Subscriber;
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields},
        FmtContext,
    },
    registry::LookupSpan,
};
use zerocopy::ByteSlice;

/// Packet formats whose parsers are provided by the [`packet_formats_dhcp`]
/// crate.
#[derive(Arbitrary)]
enum DhcpPacketType {
    Dhcpv4Message,
    Dhcpv6Message,
}

/// Packet formats whose parsers are provided by the [`mdns`] crate.
#[derive(Arbitrary)]
enum MdnsPacketType {
    MdnsMessage,
}

/// Packet formats whose parsers are provided by the [`netsvc_proto`] crate.
#[derive(Arbitrary)]
enum NetsvcPacketType {
    DebugLogPacket,
    NetbootPacket,
    TftpPacket,
}

/// Packet formats whose parsers are provided by the [`ppp_packet`] crate.
#[derive(Arbitrary)]
enum PppPacketType {
    ConfigurationPacket,
    ControlProtocolPacket,
    CodeRejectPacket,
    PppPacket,
    EchoDiscardPacket,
    ProtocolRejectPacket,
    TerminationPacket,
}

/// Packet formats whose parsers are provided by the [`packet_formats`] crate.
#[derive(Arbitrary)]
enum PacketFormatsPacketType {
    ArpPacket,
    Ethernet(Fuzzed<EthernetFrameLengthCheck>),
    Icmpv4Packet(Fuzzed<IcmpParseArgs<Ipv4Addr>>),
    Icmpv6Packet(Fuzzed<IcmpParseArgs<Ipv6Addr>>),
    IgmpPacket,
    Ipv4,
    Ipv6,
    TcpSegmentv4(Fuzzed<TcpParseArgs<Ipv4Addr>>),
    TcpSegmentv6(Fuzzed<TcpParseArgs<Ipv6Addr>>),
    UdpPacketv4(Fuzzed<UdpParseArgs<Ipv4Addr>>),
    UdpPacketv6(Fuzzed<UdpParseArgs<Ipv6Addr>>),
}

trait ParseAndIgnore<A, B: ByteSlice> {
    /// Parse the provided value to check for crashes and ignore the result.
    fn parse_and_ignore<BV: BufferView<B>>(input: BV, args: A);
}

impl<B, A, T> ParseAndIgnore<A, B> for T
where
    B: ByteSlice,
    T: ParsablePacket<B, A>,
{
    fn parse_and_ignore<BV: BufferView<B>>(input: BV, args: A) {
        let _: Result<_, _> = Self::parse(input, args);
    }
}

fn init_logging() {
    static LOGGER_ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);
    if LOGGER_ONCE.swap(false, core::sync::atomic::Ordering::AcqRel) {
        struct LogEventFormatter;
        impl<S, N> FormatEvent<S, N> for LogEventFormatter
        where
            S: Subscriber + for<'a> LookupSpan<'a>,
            N: for<'a> FormatFields<'a> + 'static,
        {
            fn format_event(
                &self,
                ctx: &FmtContext<'_, S, N>,
                mut writer: format::Writer<'_>,
                event: &tracing::Event<'_>,
            ) -> std::fmt::Result {
                let level = *event.metadata().level();
                let path = event.metadata().module_path().unwrap_or("_unknown_");
                write!(writer, "[{path}][{level}]: ")?;
                ctx.field_format().format_fields(writer.by_ref(), event)?;
                writeln!(writer)
            }
        }

        let subscriber = tracing_subscriber::fmt()
            .event_format(LogEventFormatter)
            .with_writer(std::io::stderr)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("Unable to set global default")
    }
}

#[fuzz]
fn fuzz_parse_packet(input: &[u8]) {
    init_logging();

    let mut unstructured = Unstructured::new(input);

    #[derive(Arbitrary)]
    enum SupportedPacketType {
        Packet(PacketFormatsPacketType),
        DhcpPacket(DhcpPacketType),
        MdnsPacket(MdnsPacketType),
        NetsvcPacket(NetsvcPacketType),
        PppPacket(PppPacketType),
    }

    let parse_as = match unstructured.arbitrary() {
        Ok(t) => t,
        Err(_) => return,
    };
    let mut input = unstructured.take_rest();

    match parse_as {
        SupportedPacketType::Packet(packet_type) => match packet_type {
            PacketFormatsPacketType::ArpPacket => {
                ArpPacket::<_, Mac, Ipv4Addr>::parse_and_ignore(&mut input, ());
            }
            PacketFormatsPacketType::Ethernet(args) => {
                EthernetFrame::parse_and_ignore(&mut input, args.into());
            }
            PacketFormatsPacketType::Icmpv4Packet(args) => {
                Icmpv4Packet::parse_and_ignore(&mut input, args.into());
            }
            PacketFormatsPacketType::Icmpv6Packet(args) => {
                Icmpv6Packet::parse_and_ignore(&mut input, args.into());
            }
            PacketFormatsPacketType::IgmpPacket => {
                IgmpPacket::parse_and_ignore(&mut input, ());
            }
            PacketFormatsPacketType::Ipv4 => {
                Ipv4Packet::parse_and_ignore(&mut input, ());
            }
            PacketFormatsPacketType::Ipv6 => {
                Ipv6Packet::parse_and_ignore(&mut input, ());
            }
            PacketFormatsPacketType::TcpSegmentv4(args) => {
                TcpSegment::parse_and_ignore(&mut input, args.into());
            }
            PacketFormatsPacketType::TcpSegmentv6(args) => {
                TcpSegment::parse_and_ignore(&mut input, args.into());
            }
            PacketFormatsPacketType::UdpPacketv4(args) => {
                UdpPacket::parse_and_ignore(&mut input, args.into());
            }
            PacketFormatsPacketType::UdpPacketv6(args) => {
                UdpPacket::parse_and_ignore(&mut input, args.into());
            }
        },
        SupportedPacketType::DhcpPacket(dhcp_type) => match dhcp_type {
            DhcpPacketType::Dhcpv6Message => {
                Dhcpv6Message::parse_and_ignore(&mut input, ());
            }
            DhcpPacketType::Dhcpv4Message => {
                // TODO(https://fxbug.dev/42073571): Migrate dhcp_protocol to packet-formats APIs
                // and make this use `parse_and_ignore` instead.
                let _: Result<dhcp_protocol::Message, dhcp_protocol::ProtocolError> =
                    dhcp_protocol::Message::from_buffer(input);
            }
        },
        SupportedPacketType::MdnsPacket(mdns_type) => match mdns_type {
            MdnsPacketType::MdnsMessage => {
                MdnsMessage::parse_and_ignore(&mut input, ());
            }
        },
        SupportedPacketType::NetsvcPacket(netsvc_type) => match netsvc_type {
            NetsvcPacketType::DebugLogPacket => {
                DebugLogPacket::parse_and_ignore(&mut input, ());
            }
            NetsvcPacketType::NetbootPacket => {
                NetbootPacket::parse_and_ignore(&mut input, ());
            }
            NetsvcPacketType::TftpPacket => {
                TftpPacket::parse_and_ignore(&mut input, ());
            }
        },
        SupportedPacketType::PppPacket(ppp_type) => {
            use ppp_packet::{
                CodeRejectPacket, ConfigurationPacket, ControlProtocolPacket, EchoDiscardPacket,
                PppPacket, ProtocolRejectPacket, TerminationPacket,
            };

            match ppp_type {
                PppPacketType::ConfigurationPacket => {
                    ConfigurationPacket::parse_and_ignore(&mut input, ());
                }
                PppPacketType::CodeRejectPacket => {
                    CodeRejectPacket::parse_and_ignore(&mut input, ());
                }
                PppPacketType::ControlProtocolPacket => {
                    ControlProtocolPacket::parse_and_ignore(&mut input, ());
                }
                PppPacketType::EchoDiscardPacket => {
                    EchoDiscardPacket::parse_and_ignore(&mut input, ());
                }
                PppPacketType::PppPacket => {
                    PppPacket::parse_and_ignore(&mut input, ());
                }
                PppPacketType::ProtocolRejectPacket => {
                    ProtocolRejectPacket::parse_and_ignore(&mut input, ());
                }
                PppPacketType::TerminationPacket => {
                    TerminationPacket::parse_and_ignore(&mut input, ());
                }
            }
        }
    }
}
