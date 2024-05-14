// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::{convert::Infallible as Never, num::NonZeroU16};

use net_types::ip::{GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use packet::{
    Buf, BufferMut, BufferViewMut, EitherSerializer, EmptyBuf, InnerSerializer, Nested,
    ParsablePacket, ParseBuffer, ParseMetadata, Serializer, SliceBufViewMut,
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
        IcmpPacketRaw, IcmpPacketType as _, IcmpParseArgs, IcmpTimeExceeded, Icmpv4MessageType,
        Icmpv4Packet, Icmpv4ParameterProblem, Icmpv4TimestampReply, Icmpv6MessageType,
        Icmpv6Packet, Icmpv6PacketTooBig, Icmpv6ParameterProblem,
    },
    igmp::{self, IgmpPacketBuilder},
    ip::{IpExt, IpPacket as _, IpPacketBuilder, IpProto, Ipv4Proto, Ipv6Proto},
    ipv4::Ipv4Packet,
    ipv6::Ipv6Packet,
    tcp::{TcpParseArgs, TcpSegment, TcpSegmentBuilderWithOptions, TcpSegmentRaw},
    udp::{UdpPacket, UdpPacketBuilder, UdpPacketRaw, UdpParseArgs},
};
use zerocopy::ByteSliceMut;

/// An IP packet that provides header inspection.
pub trait IpPacket<I: IpExt> {
    /// The type that provides access to transport-layer header inspection, if a
    /// transport header is contained in the body of the IP packet.
    type TransportPacket<'a>: MaybeTransportPacket
    where
        Self: 'a;

    /// The type that provides access to transport-layer header modification, if a
    /// transport header is contained in the body of the IP packet.
    type TransportPacketMut<'a>: MaybeTransportPacketMut<I>
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

    /// Returns a type that provides the ability to modify the transport-layer
    /// packet contained in the body of the IP packet, if one exists.
    ///
    /// This method returns an owned type parameterized on a lifetime that is tied
    /// to the lifetime of Self, rather than, for example, a reference to a
    /// non-parameterized type (`&Self::TransportPacketMut`). This is because
    /// implementors may need to parse the transport header from the body of the IP
    /// packet and materialize the results into a new type when this is called, but
    /// that type may also need to retain a reference to the backing buffer in order
    /// to modify the transport header.
    fn transport_packet_mut<'a>(&'a mut self) -> Self::TransportPacketMut<'a>;
}

/// A payload of an IP packet that may be a valid transport layer packet.
///
/// This trait exists to allow bubbling up the trait bound that a serializer
/// type implement `MaybeTransportPacket` from the IP socket layer to upper
/// layers, where it can be implemented separately on each concrete packet type
/// depending on whether it supports packet header inspection.
pub trait MaybeTransportPacket {
    /// The type that provides access to transport-layer header inspection, if this
    /// is indeed a valid transport packet.
    type TransportPacket: TransportPacket;

    /// Optionally returns a type that provides access to this transport-layer
    /// packet.
    fn transport_packet(&self) -> Option<&Self::TransportPacket>;
}

/// A payload of an IP packet that may be a valid modifiable transport layer
/// packet.
///
/// This trait exists to allow bubbling up the trait bound that a serializer
/// type implement `MaybeTransportPacketMut` from the IP socket layer to upper
/// layers, where it can be implemented separately on each concrete packet type
/// depending on whether it supports packet header modification.
pub trait MaybeTransportPacketMut<I: IpExt> {
    /// The type that provides access to transport-layer header modification, if
    /// this is indeed a valid transport packet.
    type TransportPacketMut<'a>: TransportPacketMut<I>
    where
        Self: 'a;

    /// Optionally returns a type that provides mutable access to this
    /// transport-layer packet.
    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>>;
}

/// A serializer that may also be a valid transport layer packet.
pub trait TransportPacketSerializer<I: IpExt>:
    Serializer + MaybeTransportPacket + MaybeTransportPacketMut<I>
{
}

impl<I, S> TransportPacketSerializer<I> for S
where
    I: IpExt,
    S: Serializer + MaybeTransportPacket + MaybeTransportPacketMut<I>,
{
}

impl<T: ?Sized> MaybeTransportPacket for &T
where
    T: MaybeTransportPacket,
{
    type TransportPacket = T::TransportPacket;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        (**self).transport_packet()
    }
}

impl<T: ?Sized> MaybeTransportPacket for &mut T
where
    T: MaybeTransportPacket,
{
    type TransportPacket = T::TransportPacket;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        (**self).transport_packet()
    }
}

impl<I: IpExt, T: ?Sized> MaybeTransportPacketMut<I> for &mut T
where
    T: MaybeTransportPacketMut<I>,
{
    type TransportPacketMut<'a> = T::TransportPacketMut<'a> where Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        (**self).transport_packet_mut()
    }
}

impl<T: TransportPacket> MaybeTransportPacket for Option<T> {
    type TransportPacket = T;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        self.as_ref()
    }
}

impl<I: IpExt, T: TransportPacketMut<I>> MaybeTransportPacketMut<I> for Option<T> {
    type TransportPacketMut<'a> = &'a mut T where Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        self.as_mut()
    }
}

/// A transport layer packet that provides header inspection.
pub trait TransportPacket {
    /// The source port or identifier of the packet.
    fn src_port(&self) -> u16;

    /// The destination port or identifier of the packet.
    fn dst_port(&self) -> u16;
}

/// A transport layer packet that provides header modification.
//
// TODO(https://fxbug.dev/321013529): provide methods to modify source and
// destination port.
pub trait TransportPacketMut<I: IpExt> {
    /// Update the source IP address in the pseudo header.
    fn update_pseudo_header_src_addr(&mut self, old: I::Addr, new: I::Addr);

    /// Update the destination IP address in the pseudo header.
    fn update_pseudo_header_dst_addr(&mut self, old: I::Addr, new: I::Addr);
}

impl<B: ByteSliceMut> IpPacket<Ipv4> for Ipv4Packet<B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;
    type TransportPacketMut<'a> = Option<ParsedTransportHeaderMut<'a, Ipv4>> where B: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        ParsedTransportHeaderMut::parse_in_ipv4_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            SliceBufViewMut::new(self.body_mut()),
        )
    }
}

impl<B: ByteSliceMut> IpPacket<Ipv6> for Ipv6Packet<B> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;
    type TransportPacketMut<'a> = Option<ParsedTransportHeaderMut<'a, Ipv6>> where B: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        ParsedTransportHeaderMut::parse_in_ipv6_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            SliceBufViewMut::new(self.body_mut()),
        )
    }
}

/// An incoming IP packet that has been parsed into its constituent parts for
/// either local delivery or forwarding.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct RxPacket<'a, I: IpExt> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    body: &'a mut [u8],
}

impl<'a, I: IpExt> RxPacket<'a, I> {
    /// Create a new [`RxPacket`] from its IP header fields and payload.
    pub fn new(
        src_addr: I::Addr,
        dst_addr: I::Addr,
        protocol: I::Proto,
        body: &'a mut [u8],
    ) -> Self {
        Self { src_addr, dst_addr, protocol, body }
    }
}

impl<I: IpExt> IpPacket<I> for RxPacket<'_, I> {
    type TransportPacket<'a> = Option<ParsedTransportHeader> where Self: 'a;
    type TransportPacketMut<'a> = Option<ParsedTransportHeaderMut<'a, I>> where Self: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        I::map_ip(
            self,
            |RxPacket { src_addr, dst_addr, protocol, body }| {
                ParsedTransportHeaderMut::parse_in_ipv4_packet(
                    *src_addr,
                    *dst_addr,
                    *protocol,
                    SliceBufViewMut::new(body),
                )
            },
            |RxPacket { src_addr, dst_addr, protocol, body }| {
                ParsedTransportHeaderMut::parse_in_ipv6_packet(
                    *src_addr,
                    *dst_addr,
                    *protocol,
                    SliceBufViewMut::new(body),
                )
            },
        )
    }
}

/// An outgoing IP packet that has not yet been wrapped into an outer serializer
/// type.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct TxPacket<'a, I: IpExt, S> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    serializer: &'a mut S,
}

impl<'a, I: IpExt, S> TxPacket<'a, I, S> {
    /// Create a new [`TxPacket`] from its IP header fields and payload.
    pub fn new(
        src_addr: I::Addr,
        dst_addr: I::Addr,
        protocol: I::Proto,
        serializer: &'a mut S,
    ) -> Self {
        Self { src_addr, dst_addr, protocol, serializer }
    }
}

impl<I: IpExt, S: TransportPacketSerializer<I>> IpPacket<I> for TxPacket<'_, I, S> {
    type TransportPacket<'a> = &'a S where Self: 'a;
    type TransportPacketMut<'a> = &'a mut S where Self: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
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
    type TransportPacket<'a> = Option<ParsedTransportHeader>  where Self: 'a;
    type TransportPacketMut<'a> = Option<ParsedTransportHeaderMut<'a, I>> where Self: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        I::map_ip(
            self,
            |ForwardedPacket { src_addr, dst_addr, protocol, body, meta: _ }| {
                ParsedTransportHeaderMut::parse_in_ipv4_packet(
                    *src_addr,
                    *dst_addr,
                    *protocol,
                    SliceBufViewMut::new(body.as_mut()),
                )
            },
            |ForwardedPacket { src_addr, dst_addr, protocol, body, meta: _ }| {
                ParsedTransportHeaderMut::parse_in_ipv6_packet(
                    *src_addr,
                    *dst_addr,
                    *protocol,
                    SliceBufViewMut::new(body.as_mut()),
                )
            },
        )
    }
}

impl<I: IpExt, S: TransportPacketSerializer<I>, B: IpPacketBuilder<I>> IpPacket<I>
    for Nested<S, B>
{
    type TransportPacket<'a> = &'a S where Self: 'a;
    type TransportPacketMut<'a> = &'a mut S where Self: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        self.inner_mut()
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

    fn inner_mut(&mut self) -> &mut Inner {
        let Self(nested) = self;
        nested.inner_mut()
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
    type TransportPacketMut<'a> = Inner::TransportPacketMut<'a> where Inner: 'a, Outer: 'a;

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

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        self.inner_mut().transport_packet_mut()
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

impl<I: IpExt, T: ?Sized> TransportPacketMut<I> for &mut T
where
    T: TransportPacketMut<I>,
{
    fn update_pseudo_header_src_addr(&mut self, old: I::Addr, new: I::Addr) {
        (*self).update_pseudo_header_src_addr(new, old);
    }

    fn update_pseudo_header_dst_addr(&mut self, old: I::Addr, new: I::Addr) {
        (*self).update_pseudo_header_dst_addr(new, old);
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

impl<I: IpExt> MaybeTransportPacketMut<I> for Never {
    type TransportPacketMut<'a> = Never where Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        match *self {}
    }
}

impl<I: IpExt> TransportPacketMut<I> for Never {
    fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {
        match *self {}
    }

    fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {
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

impl<I: IpExt, Inner> MaybeTransportPacketMut<I> for Nested<Inner, UdpPacketBuilder<I::Addr>> {
    type TransportPacketMut<'a> = &'a mut Self where Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        Some(self)
    }
}

impl<I: IpExt, Inner> TransportPacketMut<I> for Nested<Inner, UdpPacketBuilder<I::Addr>> {
    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_dst_ip(new);
    }
}

impl<A: IpAddress, Outer, Inner> MaybeTransportPacket
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, Outer>>
{
    type TransportPacket = Self;

    fn transport_packet(&self) -> Option<&Self::TransportPacket> {
        Some(self)
    }
}

impl<A: IpAddress, Outer, Inner> TransportPacket
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, Outer>>
{
    fn src_port(&self) -> u16 {
        TcpSegmentBuilderWithOptions::src_port(self.outer()).map_or(0, NonZeroU16::get)
    }

    fn dst_port(&self) -> u16 {
        TcpSegmentBuilderWithOptions::dst_port(self.outer()).map_or(0, NonZeroU16::get)
    }
}

impl<I: IpExt, Outer, Inner> MaybeTransportPacketMut<I>
    for Nested<Inner, TcpSegmentBuilderWithOptions<I::Addr, Outer>>
{
    type TransportPacketMut<'a> = &'a mut Self where Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        Some(self)
    }
}

impl<I: IpExt, Outer, Inner> TransportPacketMut<I>
    for Nested<Inner, TcpSegmentBuilderWithOptions<I::Addr, Outer>>
{
    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_dst_ip(new);
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

impl<I: IpExt, Inner, M: IcmpMessage<I>> MaybeTransportPacketMut<I>
    for Nested<Inner, IcmpPacketBuilder<I, M>>
{
    type TransportPacketMut<'a> = &'a mut IcmpPacketBuilder<I, M> where M: 'a, Inner: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        Some(self.outer_mut())
    }
}

impl<I: IpExt, M: IcmpMessage<I>> TransportPacketMut<I> for IcmpPacketBuilder<I, M> {
    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.set_dst_ip(new);
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

impl<M: igmp::MessageType<EmptyBuf>> MaybeTransportPacketMut<Ipv4>
    for InnerSerializer<IgmpPacketBuilder<EmptyBuf, M>, EmptyBuf>
{
    type TransportPacketMut<'a> = Never where M: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
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
        // NOTE: If these are parsed, then without further work, conntrack won't
        // be able to differentiate between these and ECHO message with the same
        // ID.
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

/// A transport header that has been parsed from a byte buffer and provides
/// mutable access to its contents.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub enum ParsedTransportHeaderMut<'a, I: IpExt> {
    Tcp(TcpSegment<&'a mut [u8]>),
    Udp(UdpPacket<&'a mut [u8]>),
    Icmp(I::IcmpPacketType<&'a mut [u8]>),
}

impl<'a, I: IpExt> ParsedTransportHeaderMut<'a, I> {
    fn update_pseudo_header_address(&mut self, old: I::Addr, new: I::Addr) {
        match self {
            Self::Tcp(segment) => segment.update_checksum_pseudo_header_address(old, new),
            Self::Udp(packet) => {
                if packet.checksummed() {
                    packet.update_checksum_pseudo_header_address(old, new);
                }
            }
            Self::Icmp(packet) => {
                packet.update_checksum_pseudo_header_address(old, new);
            }
        }
    }
}

impl<'a> ParsedTransportHeaderMut<'a, Ipv4> {
    fn parse_in_ipv4_packet<BV: BufferViewMut<&'a mut [u8]>>(
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        proto: Ipv4Proto,
        body: BV,
    ) -> Option<Self> {
        match proto {
            Ipv4Proto::Proto(IpProto::Udp) => {
                Some(Self::Udp(UdpPacket::parse_mut(body, UdpParseArgs::new(src_ip, dst_ip)).ok()?))
            }
            Ipv4Proto::Proto(IpProto::Tcp) => Some(Self::Tcp(
                TcpSegment::parse_mut(body, TcpParseArgs::new(src_ip, dst_ip)).ok()?,
            )),
            Ipv4Proto::Icmp => Some(Self::Icmp(
                Icmpv4Packet::parse_mut(body, IcmpParseArgs::new(src_ip, dst_ip)).ok()?,
            )),
            Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
        }
    }
}

impl<'a> ParsedTransportHeaderMut<'a, Ipv6> {
    fn parse_in_ipv6_packet<BV: BufferViewMut<&'a mut [u8]>>(
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        proto: Ipv6Proto,
        body: BV,
    ) -> Option<Self> {
        match proto {
            Ipv6Proto::Proto(IpProto::Udp) => {
                Some(Self::Udp(UdpPacket::parse_mut(body, UdpParseArgs::new(src_ip, dst_ip)).ok()?))
            }
            Ipv6Proto::Proto(IpProto::Tcp) => Some(Self::Tcp(
                TcpSegment::parse_mut(body, TcpParseArgs::new(src_ip, dst_ip)).ok()?,
            )),
            Ipv6Proto::Icmpv6 => Some(Self::Icmp(
                Icmpv6Packet::parse_mut(body, IcmpParseArgs::new(src_ip, dst_ip)).ok()?,
            )),
            Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => None,
        }
    }
}

impl<'a, I: IpExt> TransportPacketMut<I> for ParsedTransportHeaderMut<'a, I> {
    fn update_pseudo_header_src_addr(&mut self, old: I::Addr, new: I::Addr) {
        self.update_pseudo_header_address(old, new);
    }

    fn update_pseudo_header_dst_addr(&mut self, old: I::Addr, new: I::Addr) {
        self.update_pseudo_header_address(old, new);
    }
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

    impl<I: IpExt, B: BufferMut> MaybeTransportPacketMut<I> for Nested<B, ()> {
        type TransportPacketMut<'a> = Never where B: 'a;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
            unimplemented!()
        }
    }

    impl MaybeTransportPacket for InnerSerializer<&[u8], EmptyBuf> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            unimplemented!()
        }
    }

    impl<I: IpExt> MaybeTransportPacketMut<I> for InnerSerializer<&[u8], EmptyBuf> {
        type TransportPacketMut<'a> = Never where Self: 'a;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
            unimplemented!()
        }
    }

    impl MaybeTransportPacket for Buf<Vec<u8>> {
        type TransportPacket = Never;

        fn transport_packet(&self) -> Option<&Self::TransportPacket> {
            unimplemented!()
        }
    }

    impl<I: IpExt> MaybeTransportPacketMut<I> for Buf<Vec<u8>> {
        type TransportPacketMut<'a> = Never;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
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
            for<'a> &'a mut T: MaybeTransportPacketMut<I>,
        {
            type TransportPacket<'a> = &'a T where T: 'a;
            type TransportPacketMut<'a> = &'a mut T where T: 'a;

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

            fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
                &mut self.body
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

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeTcpSegment {
            type TransportPacketMut<'a> = &'a mut Self;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                Some(self)
            }
        }

        impl<I: IpExt> TransportPacketMut<I> for FakeTcpSegment {
            fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {}
            fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {}
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

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeUdpPacket {
            type TransportPacketMut<'a> = &'a mut Self;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                Some(self)
            }
        }

        impl<I: IpExt> TransportPacketMut<I> for FakeUdpPacket {
            fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {}
            fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {}
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

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeIcmpEchoRequest {
            type TransportPacketMut<'a> = &'a mut Self;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                Some(self)
            }
        }

        impl<I: IpExt> TransportPacketMut<I> for FakeIcmpEchoRequest {
            fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {}
            fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {}
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

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use net_types::ip::IpInvariant;
    use packet::InnerPacketBuilder as _;
    use packet_formats::{icmp::IcmpUnusedCode, tcp::TcpSegmentBuilder};
    use test_case::test_case;

    use super::{testutil::internal::TestIpExt, *};

    const SRC_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(11111));
    const DST_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(33333));

    enum Protocol {
        Udp,
        Tcp,
        Icmp,
    }

    impl Protocol {
        fn proto<I: IpExt>(&self) -> I::Proto {
            match self {
                Self::Udp => IpProto::Udp.into(),
                Self::Tcp => IpProto::Tcp.into(),
                Self::Icmp => I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6),
            }
        }

        fn make_packet<I: IpExt>(&self, src_ip: I::Addr, dst_ip: I::Addr) -> Vec<u8> {
            match self {
                Self::Udp => []
                    .into_serializer()
                    .encapsulate(UdpPacketBuilder::new(src_ip, dst_ip, Some(SRC_PORT), DST_PORT))
                    .serialize_vec_outer()
                    .expect("serialize UDP packet")
                    .unwrap_b()
                    .into_inner(),
                Self::Tcp => []
                    .into_serializer()
                    .encapsulate(TcpSegmentBuilder::new(
                        src_ip, dst_ip, SRC_PORT, DST_PORT, /* seq_num */ 0,
                        /* ack_num */ None, /* window_size */ 0,
                    ))
                    .serialize_vec_outer()
                    .expect("serialize TCP segment")
                    .unwrap_b()
                    .into_inner(),
                Self::Icmp => []
                    .into_serializer()
                    .encapsulate(IcmpPacketBuilder::<I, _>::new(
                        src_ip,
                        dst_ip,
                        IcmpUnusedCode,
                        IcmpEchoRequest::new(/* id */ SRC_PORT.get(), /* seq */ 0),
                    ))
                    .serialize_vec_outer()
                    .expect("serialize ICMP echo request")
                    .unwrap_b()
                    .into_inner(),
            }
        }
    }

    #[ip_test]
    #[test_case(Protocol::Udp)]
    #[test_case(Protocol::Tcp)]
    #[test_case(Protocol::Icmp)]
    fn update_pseudo_header_address_updates_checksum<I: Ip + TestIpExt>(proto: Protocol) {
        let mut buf = proto.make_packet::<I>(I::SRC_IP, I::DST_IP);
        let view = SliceBufViewMut::new(&mut buf);

        let mut packet = I::map_ip::<_, Option<ParsedTransportHeaderMut<'_, I>>>(
            (I::SRC_IP, I::DST_IP, proto.proto::<I>(), IpInvariant(view)),
            |(src, dst, proto, IpInvariant(view))| {
                ParsedTransportHeaderMut::parse_in_ipv4_packet(src, dst, proto, view)
            },
            |(src, dst, proto, IpInvariant(view))| {
                ParsedTransportHeaderMut::parse_in_ipv6_packet(src, dst, proto, view)
            },
        )
        .expect("parse transport header");
        packet.update_pseudo_header_src_addr(I::SRC_IP, I::DST_IP);
        packet.update_pseudo_header_dst_addr(I::DST_IP, I::SRC_IP);
        // Drop the packet because it's holding a mutable borrow of `buf` which
        // we need to assert equality later.
        drop(packet);

        let equivalent = proto.make_packet::<I>(I::DST_IP, I::SRC_IP);

        assert_eq!(equivalent, buf);
    }
}
