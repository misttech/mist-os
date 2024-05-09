// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::{convert::Infallible as Never, num::NonZeroU16};

use net_types::ip::{GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use packet::{
    Buf, BufferMut, EitherSerializer, EmptyBuf, InnerSerializer, Nested, ParseBuffer,
    ParseMetadata, Serializer,
};
use packet_formats::{
    icmp::{
        self,
        mld::{MulticastListenerDone, MulticastListenerReport},
        ndp::{
            options::NdpOptionBuilder, NeighborAdvertisement, NeighborSolicitation,
            RouterSolicitation,
        },
        IcmpDestUnreachable, IcmpEchoReply, IcmpEchoRequest, IcmpMessage, IcmpPacketBuilder,
        IcmpPacketRaw, IcmpTimeExceeded, Icmpv4MessageType, Icmpv4ParameterProblem,
        Icmpv4TimestampReply, Icmpv6MessageType, Icmpv6PacketTooBig, Icmpv6ParameterProblem,
    },
    igmp::{self, IgmpPacketBuilder},
    ip::{IpExt, IpPacket as _, IpPacketBuilder, IpProto, Ipv4Proto, Ipv6Proto},
    ipv4::Ipv4Packet,
    ipv6::Ipv6Packet,
    tcp::{TcpSegmentBuilderWithOptions, TcpSegmentRaw},
    udp::{UdpPacketBuilder, UdpPacketRaw},
};
use zerocopy::ByteSliceMut;

/// An IP packet that provides header inspection.
//
// TODO(https://fxbug.dev/321013529): provide the necessary methods and associated
// type for packet header modification.
pub trait IpPacket<I: IpExt> {
    /// The type that provides access to transport-layer header inspection, if a
    /// transport header is contained in the body of the IP packet.
    type TransportPacket<'a>: MaybeTransportPacket
    where
        Self: 'a;

    /// The source IP address of the packet.
    fn src_addr(&self) -> I::Addr;

    /// The destination IP address of the packet.
    fn dst_addr(&self) -> I::Addr;

    /// The IP protocol of the packet.
    fn protocol(&self) -> I::Proto;

    /// Returns a type that provides access to the transport-layer packet contained
    /// in the body of the IP packet, if one exists.
    ///
    /// This method returns an owned type parameterized on a lifetime that is tied
    /// to the lifetime of Self, rather than, for example, a reference to a
    /// non-parameterized type (`&Self::TransportPacket`). This is because
    /// implementors may need to parse the transport header from the body of the IP
    /// packet and materialize the results into a new type when this is called, but
    /// that type may also need to retain a reference to the backing buffer in order
    /// to modify the transport header.
    fn transport_packet<'a>(&'a self) -> Self::TransportPacket<'a>;
}

/// A payload of an IP packet that may be a valid transport layer packet.
///
/// This trait exists to allow bubbling up the trait bound that a serializer
/// type implement `MaybeTransportPacket` from the IP socket layer to, for
/// example, the ICMP layer, where it can be implemented separately on each
/// concrete ICMP message type depending on whether it supports packet header
/// inspection.
pub trait MaybeTransportPacket {
    /// The type that provides access to transport-layer header inspection, if this
    /// is indeed a valid transport packet.
    type TransportPacket: TransportPacket;

    /// Optionally returns a type that provides access to this transport-layer
    /// packet.
    fn transport_packet(&self) -> Option<&Self::TransportPacket>;
}

/// A serializer that may also be a valid transport layer packet.
pub trait TransportPacketSerializer: Serializer + MaybeTransportPacket {}

impl<S: Serializer + MaybeTransportPacket> TransportPacketSerializer for S {}

impl<T: ?Sized> MaybeTransportPacket for &T
where
    T: MaybeTransportPacket,
{
    type TransportPacket = T::TransportPacket;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        (*self).transport_packet()
    }
}

impl<T: TransportPacket> MaybeTransportPacket for Option<T> {
    type TransportPacket = T;

    fn transport_packet(&self) -> Option<&T> {
        self.as_ref()
    }
}

/// A transport layer packet that provides header inspection.
//
// TODO(https://fxbug.dev/321013529): provide the necessary methods and associated
// type for packet header modification.
pub trait TransportPacket {
    /// The source port or identifier of the packet.
    fn src_port(&self) -> u16;

    /// The destination port or identifier of the packet.
    fn dst_port(&self) -> u16;
}

impl<B: ByteSliceMut + ParseBuffer> IpPacket<Ipv4> for Ipv4Packet<B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;

    fn src_addr(&self) -> Ipv4Addr {
        self.src_ip()
    }

    fn dst_addr(&self) -> Ipv4Addr {
        self.dst_ip()
    }

    fn protocol(&self) -> Ipv4Proto {
        self.proto()
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        parse_transport_header_in_ipv4_packet(self.proto(), self.body())
    }
}

impl<B: ByteSliceMut + ParseBuffer> IpPacket<Ipv6> for Ipv6Packet<B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;

    fn src_addr(&self) -> Ipv6Addr {
        self.src_ip()
    }

    fn dst_addr(&self) -> Ipv6Addr {
        self.dst_ip()
    }

    fn protocol(&self) -> Ipv6Proto {
        self.proto()
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        parse_transport_header_in_ipv6_packet(self.proto(), self.body())
    }
}

/// An incoming IP packet that has been parsed into its constituent parts for
/// either local delivery or forwarding.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct RxPacket<'a, I: IpExt, B> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    body: &'a B,
}

impl<'a, I: IpExt, B: ParseBuffer> RxPacket<'a, I, B> {
    /// Create a new [`RxPacket`] from its IP header fields and payload.
    pub fn new(src_addr: I::Addr, dst_addr: I::Addr, protocol: I::Proto, body: &'a B) -> Self {
        Self { src_addr, dst_addr, protocol, body }
    }
}

impl<I: IpExt, B: ParseBuffer> IpPacket<I> for RxPacket<'_, I, B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        I::map_ip(
            self,
            |RxPacket { protocol, body, .. }| {
                parse_transport_header_in_ipv4_packet(*protocol, Buf::new(body, ..))
            },
            |RxPacket { protocol, body, .. }| {
                parse_transport_header_in_ipv6_packet(*protocol, Buf::new(body, ..))
            },
        )
    }
}

/// An outgoing IP packet that has not yet been wrapped into an outer serializer
/// type.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct TxPacket<'a, I: IpExt, S: MaybeTransportPacket> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    serializer: &'a S,
}

impl<'a, I: IpExt, S: MaybeTransportPacket> TxPacket<'a, I, S> {
    /// Create a new [`TxPacket`] from its IP header fields and payload.
    pub fn new(
        src_addr: I::Addr,
        dst_addr: I::Addr,
        protocol: I::Proto,
        serializer: &'a S,
    ) -> Self {
        Self { src_addr, dst_addr, protocol, serializer }
    }
}

impl<I: IpExt, S: MaybeTransportPacket> IpPacket<I> for TxPacket<'_, I, S> {
    type TransportPacket<'a> = &'a S where Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        self.serializer
    }
}

/// An incoming IP packet that is being forwarded.
///
/// NB: this type is distinct from [`RxPacket`] (used on ingress) and
/// [`TxPacket`] (used on egress) in that it implements `Serializer` by holding
/// the parse metadata from parsing the IP header and undoing that parsing when
/// it is serialized. This allows the buffer to be reused on the egress path in
/// its entirety.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct ForwardedPacket<I: IpExt, B> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    meta: ParseMetadata,
    body: B,
}

impl<I: IpExt, B: BufferMut> ForwardedPacket<I, B> {
    /// Create a new [`ForwardedPacket`] from its IP header fields and payload.
    pub fn new(
        src_addr: I::Addr,
        dst_addr: I::Addr,
        protocol: I::Proto,
        meta: ParseMetadata,
        body: B,
    ) -> Self {
        Self { src_addr, dst_addr, protocol, meta, body }
    }

    /// Discard the metadata carried by the [`ForwardedPacket`] and return the
    /// inner buffer.
    pub fn into_buffer(self) -> B {
        self.body
    }
}

impl<I: IpExt, B: BufferMut> Serializer for ForwardedPacket<I, B> {
    type Buffer = <B as Serializer>::Buffer;

    fn serialize<G: packet::GrowBufferMut, P: packet::BufferProvider<Self::Buffer, G>>(
        self,
        outer: packet::PacketConstraints,
        provider: P,
    ) -> Result<G, (packet::SerializeError<P::Error>, Self)> {
        let Self { src_addr, dst_addr, protocol, meta, mut body } = self;
        body.undo_parse(meta);
        body.serialize(outer, provider)
            .map_err(|(err, body)| (err, Self { src_addr, dst_addr, protocol, meta, body }))
    }
}

impl<I: IpExt, B: BufferMut> IpPacket<I> for ForwardedPacket<I, B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader>  where Self : 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        I::map_ip(
            self,
            |ForwardedPacket { protocol, body, .. }| {
                parse_transport_header_in_ipv4_packet(*protocol, Buf::new(body, ..))
            },
            |ForwardedPacket { protocol, body, .. }| {
                parse_transport_header_in_ipv6_packet(*protocol, Buf::new(body, ..))
            },
        )
    }
}

impl<I: IpExt, S: MaybeTransportPacket, B: IpPacketBuilder<I>> IpPacket<I> for Nested<S, B> {
    type TransportPacket<'a> = &'a S where Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.outer().src_ip()
    }

    fn dst_addr(&self) -> I::Addr {
        self.outer().dst_ip()
    }

    fn protocol(&self) -> I::Proto {
        self.outer().proto()
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        self.inner()
    }
}

/// A nested serializer whose inner serializer implements `IpPacket`.
///
/// [`packet::Nested`] is often used to represent an outer IP packet serializer
/// and an inner payload contained by the IP packet. However, in some cases, a
/// type that implements `IpPacket` may itself be wrapped in an outer
/// serializer, such as (for example) a [`packet::LimitedSizePacketBuilder`] for
/// enforcing an MTU. In that scenario, the `Nested` serializer can be wrapped
/// in this type to indicate that the `IpPacket` implementation should be
/// delegated to the inner serializer.
pub struct NestedWithInnerIpPacket<Inner, Outer>(Nested<Inner, Outer>);

impl<Inner, Outer> NestedWithInnerIpPacket<Inner, Outer> {
    /// Creates a new [`NestedWithInnerIpPacket`] wrapping the provided nested
    /// serializer.
    pub fn new(nested: Nested<Inner, Outer>) -> Self {
        Self(nested)
    }

    /// Calls [`Nested::into_inner`] on the contained nested serializer.
    pub fn into_inner(self) -> Inner {
        let Self(nested) = self;
        nested.into_inner()
    }

    fn inner(&self) -> &Inner {
        let Self(nested) = self;
        nested.inner()
    }
}

impl<Inner, Outer> Serializer for NestedWithInnerIpPacket<Inner, Outer>
where
    Nested<Inner, Outer>: Serializer,
{
    type Buffer = <Nested<Inner, Outer> as Serializer>::Buffer;

    fn serialize<G: packet::GrowBufferMut, P: packet::BufferProvider<Self::Buffer, G>>(
        self,
        outer: packet::PacketConstraints,
        provider: P,
    ) -> Result<G, (packet::SerializeError<P::Error>, Self)> {
        let Self(nested) = self;
        nested.serialize(outer, provider).map_err(|(err, nested)| (err, Self(nested)))
    }
}

impl<I: IpExt, Inner: IpPacket<I>, Outer> IpPacket<I> for NestedWithInnerIpPacket<Inner, Outer> {
    type TransportPacket<'a> = Inner::TransportPacket<'a> where Inner: 'a, Outer: 'a;

    fn src_addr(&self) -> I::Addr {
        self.inner().src_addr()
    }

    fn dst_addr(&self) -> I::Addr {
        self.inner().dst_addr()
    }

    fn protocol(&self) -> I::Proto {
        self.inner().protocol()
    }

    fn transport_packet(&self) -> Self::TransportPacket<'_> {
        self.inner().transport_packet()
    }
}

impl<T: ?Sized> TransportPacket for &T
where
    T: TransportPacket,
{
    fn src_port(&self) -> u16 {
        (*self).src_port()
    }

    fn dst_port(&self) -> u16 {
        (*self).dst_port()
    }
}

impl MaybeTransportPacket for Never {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        match *self {}
    }
}

impl TransportPacket for Never {
    fn src_port(&self) -> u16 {
        match *self {}
    }

    fn dst_port(&self) -> u16 {
        match *self {}
    }
}

impl<A: IpAddress, Inner> MaybeTransportPacket for Nested<Inner, UdpPacketBuilder<A>> {
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

impl<A: IpAddress, Inner> TransportPacket for Nested<Inner, UdpPacketBuilder<A>> {
    fn src_port(&self) -> u16 {
        self.outer().src_port().map_or(0, NonZeroU16::get)
    }

    fn dst_port(&self) -> u16 {
        self.outer().dst_port().map_or(0, NonZeroU16::get)
    }
}

impl<A: IpAddress, O, Inner> MaybeTransportPacket
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, O>>
{
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

impl<A: IpAddress, O, Inner> TransportPacket for Nested<Inner, TcpSegmentBuilderWithOptions<A, O>> {
    fn src_port(&self) -> u16 {
        TcpSegmentBuilderWithOptions::src_port(self.outer()).map_or(0, NonZeroU16::get)
    }

    fn dst_port(&self) -> u16 {
        TcpSegmentBuilderWithOptions::dst_port(self.outer()).map_or(0, NonZeroU16::get)
    }
}

impl<I: IpExt, Inner, M: IcmpMessage<I> + MaybeTransportPacket> MaybeTransportPacket
    for Nested<Inner, IcmpPacketBuilder<I, M>>
{
    type TransportPacket = M::TransportPacket;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        self.outer().message().transport_packet()
    }
}

impl MaybeTransportPacket for IcmpEchoReply {
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

// TODO(https://fxbug.dev/328064082): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl TransportPacket for IcmpEchoReply {
    fn src_port(&self) -> u16 {
        0
    }

    fn dst_port(&self) -> u16 {
        self.id()
    }
}

impl MaybeTransportPacket for IcmpEchoRequest {
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

// TODO(https://fxbug.dev/328064082): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl TransportPacket for IcmpEchoRequest {
    fn src_port(&self) -> u16 {
        self.id()
    }

    fn dst_port(&self) -> u16 {
        0
    }
}

// TODO(https://fxbug.dev/328057704): parse the IP packet contained in the ICMP
// error message payload so NAT can be applied to it.
impl MaybeTransportPacket for Icmpv4TimestampReply {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

// Transport layer packet inspection is not currently supported for any ICMP
// error message types.
//
// TODO(https://fxbug.dev/328057704): parse the IP packet contained in the ICMP
// error message payload so NAT can be applied to it.
macro_rules! icmp_error_message {
    ($message:ty) => {
        impl MaybeTransportPacket for $message {
            type TransportPacket = Never;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                None
            }
        }
    };
}

icmp_error_message!(IcmpDestUnreachable);

icmp_error_message!(IcmpTimeExceeded);

icmp_error_message!(Icmpv4ParameterProblem);

icmp_error_message!(Icmpv6ParameterProblem);

icmp_error_message!(Icmpv6PacketTooBig);

impl MaybeTransportPacket for NeighborSolicitation {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

impl MaybeTransportPacket for NeighborAdvertisement {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

impl MaybeTransportPacket for RouterSolicitation {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

impl MaybeTransportPacket for MulticastListenerDone {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

impl MaybeTransportPacket for MulticastListenerReport {
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

impl<M: igmp::MessageType<EmptyBuf>> MaybeTransportPacket
    for InnerSerializer<IgmpPacketBuilder<EmptyBuf, M>, EmptyBuf>
{
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

impl<I> MaybeTransportPacket
    for EitherSerializer<
        EmptyBuf,
        InnerSerializer<packet::records::RecordSequenceBuilder<NdpOptionBuilder<'_>, I>, EmptyBuf>,
    >
{
    type TransportPacket = Never;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        None
    }
}

#[derive(GenericOverIp)]
#[generic_over_ip()]
pub struct ParsedTransportHeader {
    src_port: u16,
    dst_port: u16,
}

impl TransportPacket for ParsedTransportHeader {
    fn src_port(&self) -> u16 {
        self.src_port
    }

    fn dst_port(&self) -> u16 {
        self.dst_port
    }
}

fn parse_transport_header_in_ipv4_packet<B: ParseBuffer>(
    proto: Ipv4Proto,
    body: B,
) -> Option<ParsedTransportHeader> {
    match proto {
        Ipv4Proto::Proto(IpProto::Udp) => parse_udp_header::<_, Ipv4>(body),
        Ipv4Proto::Proto(IpProto::Tcp) => parse_tcp_header(body),
        Ipv4Proto::Icmp => parse_icmpv4_header(body),
        Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
    }
}

fn parse_transport_header_in_ipv6_packet<B: ParseBuffer>(
    proto: Ipv6Proto,
    body: B,
) -> Option<ParsedTransportHeader> {
    match proto {
        Ipv6Proto::Proto(IpProto::Udp) => parse_udp_header::<_, Ipv6>(body),
        Ipv6Proto::Proto(IpProto::Tcp) => parse_tcp_header(body),
        Ipv6Proto::Icmpv6 => parse_icmpv6_header(body),
        Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => None,
    }
}

fn parse_udp_header<B: ParseBuffer, I: Ip>(mut body: B) -> Option<ParsedTransportHeader> {
    let packet = body.parse_with::<_, UdpPacketRaw<_>>(I::VERSION_MARKER).ok()?;
    Some(ParsedTransportHeader {
        src_port: packet.src_port().map(NonZeroU16::get).unwrap_or(0),
        // NB: UDP packets must have a specified (nonzero) destination port, so
        // if this packet has a destination port of 0, it is malformed.
        dst_port: packet.dst_port()?.get(),
    })
}

fn parse_tcp_header<B: ParseBuffer>(mut body: B) -> Option<ParsedTransportHeader> {
    let packet = body.parse::<TcpSegmentRaw<_>>().ok()?;
    let (src_port, dst_port) = packet.flow_header().src_dst();
    Some(ParsedTransportHeader { src_port, dst_port })
}

fn parse_icmpv4_header<B: ParseBuffer>(mut body: B) -> Option<ParsedTransportHeader> {
    let (src_port, dst_port) = match icmp::peek_message_type(body.as_ref()).ok()? {
        Icmpv4MessageType::EchoRequest => {
            let packet = body.parse::<IcmpPacketRaw<Ipv4, _, IcmpEchoRequest>>().ok()?;
            Some((packet.message().id(), 0))
        }
        Icmpv4MessageType::EchoReply => {
            let packet = body.parse::<IcmpPacketRaw<Ipv4, _, IcmpEchoReply>>().ok()?;
            Some((0, packet.message().id()))
        }
        // TODO(https://fxbug.dev/328057704): parse packet contained in ICMP error
        // message payload so NAT can be applied to it.
        Icmpv4MessageType::DestUnreachable
        | Icmpv4MessageType::Redirect
        | Icmpv4MessageType::TimeExceeded
        | Icmpv4MessageType::ParameterProblem
        | Icmpv4MessageType::TimestampRequest
        | Icmpv4MessageType::TimestampReply => None,
    }?;
    Some(ParsedTransportHeader { src_port, dst_port })
}

fn parse_icmpv6_header<B: ParseBuffer>(mut body: B) -> Option<ParsedTransportHeader> {
    let (src_port, dst_port) = match icmp::peek_message_type(body.as_ref()).ok()? {
        Icmpv6MessageType::EchoRequest => {
            let packet = body.parse::<IcmpPacketRaw<Ipv6, _, IcmpEchoRequest>>().ok()?;
            Some((packet.message().id(), 0))
        }
        Icmpv6MessageType::EchoReply => {
            let packet = body.parse::<IcmpPacketRaw<Ipv6, _, IcmpEchoReply>>().ok()?;
            Some((0, packet.message().id()))
        }
        // TODO(https://fxbug.dev/328057704): parse packet contained in ICMP error
        // message payload so NAT can be applied to it.
        Icmpv6MessageType::DestUnreachable
        | Icmpv6MessageType::PacketTooBig
        | Icmpv6MessageType::TimeExceeded
        | Icmpv6MessageType::ParameterProblem
        | Icmpv6MessageType::RouterSolicitation
        | Icmpv6MessageType::RouterAdvertisement
        | Icmpv6MessageType::NeighborSolicitation
        | Icmpv6MessageType::NeighborAdvertisement
        | Icmpv6MessageType::Redirect
        | Icmpv6MessageType::MulticastListenerQuery
        | Icmpv6MessageType::MulticastListenerReport
        | Icmpv6MessageType::MulticastListenerDone
        | Icmpv6MessageType::MulticastListenerReportV2 => None,
    }?;
    Some(ParsedTransportHeader { src_port, dst_port })
}

#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    use alloc::vec::Vec;

    use super::*;

    // Note that we could choose to implement `MaybeTransportPacket` for these
    // opaque byte buffer types by parsing them as we do incoming buffers, but since
    // these implementations are only for use in netstack3_core unit tests, there is
    // no expectation that filtering or connection tracking actually be performed.
    // If that changes at some point, we could replace these with "real"
    // implementations.

    impl<B: BufferMut> MaybeTransportPacket for Nested<B, ()> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            unimplemented!()
        }
    }

    impl MaybeTransportPacket for InnerSerializer<&[u8], EmptyBuf> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            unimplemented!()
        }
    }

    impl MaybeTransportPacket for Buf<Vec<u8>> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            unimplemented!()
        }
    }

    #[cfg(test)]
    pub(crate) mod internal {
        use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
        use net_types::ip::{IpInvariant, Subnet};

        use super::*;

        pub trait TestIpExt: IpExt {
            const SRC_IP: Self::Addr;
            const DST_IP: Self::Addr;
            const IP_OUTSIDE_SUBNET: Self::Addr;
            const SUBNET: Subnet<Self::Addr>;
        }

        impl TestIpExt for Ipv4 {
            const SRC_IP: Self::Addr = net_ip_v4!("192.0.2.1");
            const DST_IP: Self::Addr = net_ip_v4!("192.0.2.2");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v4!("192.0.2.4");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v4!("192.0.2.0/30");
        }

        impl TestIpExt for Ipv6 {
            const SRC_IP: Self::Addr = net_ip_v6!("2001:db8::1");
            const DST_IP: Self::Addr = net_ip_v6!("2001:db8::2");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v6!("2001:db8::4");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v6!("2001:db8::/126");
        }

        pub struct FakeIpPacket<I: IpExt, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
        {
            pub src_ip: I::Addr,
            pub dst_ip: I::Addr,
            pub body: T,
        }

        pub trait TransportPacketExt<I: IpExt>: MaybeTransportPacket {
            fn proto() -> I::Proto;
        }

        impl<I: IpExt, T> IpPacket<I> for FakeIpPacket<I, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
        {
            type TransportPacket<'a> = &'a T where T: 'a;

            fn src_addr(&self) -> I::Addr {
                self.src_ip
            }

            fn dst_addr(&self) -> I::Addr {
                self.dst_ip
            }

            fn protocol(&self) -> I::Proto {
                <&T>::proto()
            }

            fn transport_packet(&self) -> Self::TransportPacket<'_> {
                &self.body
            }
        }

        pub struct FakeTcpSegment {
            pub src_port: u16,
            pub dst_port: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeTcpSegment {
            fn proto() -> I::Proto {
                I::map_ip(
                    IpInvariant(()),
                    |IpInvariant(())| Ipv4Proto::Proto(IpProto::Tcp),
                    |IpInvariant(())| Ipv6Proto::Proto(IpProto::Tcp),
                )
            }
        }

        impl MaybeTransportPacket for &FakeTcpSegment {
            type TransportPacket = Self;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                Some(self)
            }
        }

        impl TransportPacket for &FakeTcpSegment {
            fn src_port(&self) -> u16 {
                self.src_port
            }

            fn dst_port(&self) -> u16 {
                self.dst_port
            }
        }

        pub struct FakeUdpPacket {
            pub src_port: u16,
            pub dst_port: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeUdpPacket {
            fn proto() -> I::Proto {
                I::map_ip(
                    IpInvariant(()),
                    |IpInvariant(())| Ipv4Proto::Proto(IpProto::Udp),
                    |IpInvariant(())| Ipv6Proto::Proto(IpProto::Udp),
                )
            }
        }

        impl MaybeTransportPacket for &FakeUdpPacket {
            type TransportPacket = Self;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                Some(self)
            }
        }

        impl TransportPacket for &FakeUdpPacket {
            fn src_port(&self) -> u16 {
                self.src_port
            }

            fn dst_port(&self) -> u16 {
                self.dst_port
            }
        }

        pub struct FakeIcmpEchoRequest {
            pub id: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeIcmpEchoRequest {
            fn proto() -> I::Proto {
                I::map_ip(
                    IpInvariant(()),
                    |IpInvariant(())| Ipv4Proto::Icmp,
                    |IpInvariant(())| Ipv6Proto::Icmpv6,
                )
            }
        }

        impl MaybeTransportPacket for &FakeIcmpEchoRequest {
            type TransportPacket = Self;

            fn transport_packet(&self) -> Option<&Self::TransportPacket> {
                Some(self)
            }
        }

        impl TransportPacket for &FakeIcmpEchoRequest {
            fn src_port(&self) -> u16 {
                self.id
            }

            fn dst_port(&self) -> u16 {
                0
            }
        }

        pub trait ArbitraryValue {
            fn arbitrary_value() -> Self;
        }

        impl<I, T> ArbitraryValue for FakeIpPacket<I, T>
        where
            I: TestIpExt,
            T: ArbitraryValue,
            for<'a> &'a T: TransportPacketExt<I>,
        {
            fn arbitrary_value() -> Self {
                FakeIpPacket { src_ip: I::SRC_IP, dst_ip: I::DST_IP, body: T::arbitrary_value() }
            }
        }

        impl ArbitraryValue for FakeTcpSegment {
            fn arbitrary_value() -> Self {
                FakeTcpSegment { src_port: 33333, dst_port: 44444 }
            }
        }

        impl ArbitraryValue for FakeUdpPacket {
            fn arbitrary_value() -> Self {
                FakeUdpPacket { src_port: 33333, dst_port: 44444 }
            }
        }

        impl ArbitraryValue for FakeIcmpEchoRequest {
            fn arbitrary_value() -> Self {
                FakeIcmpEchoRequest { id: 1 }
            }
        }
    }
}
