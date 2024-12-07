// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::borrow::Borrow;
use core::convert::Infallible as Never;
use core::num::NonZeroU16;

use net_types::ip::{GenericOverIp, Ip, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use netstack3_base::{Options, PayloadLen, SegmentHeader};
use packet::records::options::OptionSequenceBuilder;
use packet::{
    Buf, BufferAlloc, BufferMut, BufferProvider, BufferViewMut, EitherSerializer, EmptyBuf,
    GrowBufferMut, InnerSerializer, Nested, PacketConstraints, ParsablePacket, ParseBuffer,
    ParseMetadata, ReusableBuffer, SerializeError, Serializer, SliceBufViewMut,
};
use packet_formats::icmp::mld::{MulticastListenerDone, MulticastListenerReport};
use packet_formats::icmp::ndp::options::NdpOptionBuilder;
use packet_formats::icmp::ndp::{NeighborAdvertisement, NeighborSolicitation, RouterSolicitation};
use packet_formats::icmp::{
    self, IcmpDestUnreachable, IcmpEchoReply, IcmpEchoRequest, IcmpPacketBuilder, IcmpPacketRaw,
    IcmpPacketTypeRaw as _, IcmpTimeExceeded, Icmpv4MessageType, Icmpv4PacketRaw,
    Icmpv4ParameterProblem, Icmpv4TimestampReply, Icmpv6MessageType, Icmpv6PacketRaw,
    Icmpv6PacketTooBig, Icmpv6ParameterProblem,
};
use packet_formats::igmp::{self, IgmpPacketBuilder};
use packet_formats::ip::{IpExt, IpPacket as _, IpPacketBuilder, IpProto, Ipv4Proto, Ipv6Proto};
use packet_formats::ipv4::{Ipv4Packet, Ipv4PacketRaw};
use packet_formats::ipv6::{Ipv6Packet, Ipv6PacketRaw};
use packet_formats::tcp::options::TcpOption;
use packet_formats::tcp::{TcpParseArgs, TcpSegment, TcpSegmentBuilderWithOptions, TcpSegmentRaw};
use packet_formats::udp::{UdpPacket, UdpPacketBuilder, UdpPacketRaw, UdpParseArgs};
use zerocopy::{SplitByteSlice, SplitByteSliceMut};

/// An IP extension trait for the filtering crate.
pub trait FilterIpExt: IpExt {
    /// A marker type to add an [`IpPacket`] bound to [`Self::Packet`].
    type FilterIpPacket<B: SplitByteSliceMut>: IpPacket<Self>;

    /// A no-op conversion to help the compiler identify that [`Self::Packet`]
    /// actually implements [`IpPacket`].
    fn as_filter_packet<B: SplitByteSliceMut>(
        packet: &mut Self::Packet<B>,
    ) -> &mut Self::FilterIpPacket<B>;
}

impl FilterIpExt for Ipv4 {
    type FilterIpPacket<B: SplitByteSliceMut> = Ipv4Packet<B>;

    #[inline]
    fn as_filter_packet<B: SplitByteSliceMut>(packet: &mut Ipv4Packet<B>) -> &mut Ipv4Packet<B> {
        packet
    }
}

impl FilterIpExt for Ipv6 {
    type FilterIpPacket<B: SplitByteSliceMut> = Ipv6Packet<B>;

    #[inline]
    fn as_filter_packet<B: SplitByteSliceMut>(packet: &mut Ipv6Packet<B>) -> &mut Ipv6Packet<B> {
        packet
    }
}

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

    /// Sets the source IP address of the packet.
    fn set_src_addr(&mut self, addr: I::Addr);

    /// The destination IP address of the packet.
    fn dst_addr(&self) -> I::Addr;

    /// Sets the destination IP address of the packet.
    fn set_dst_addr(&mut self, addr: I::Addr);

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
    fn maybe_transport_packet<'a>(&'a self) -> Self::TransportPacket<'a>;

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
    /// Optionally returns a type that provides access to this transport-layer
    /// packet.
    fn transport_packet_data(&self) -> Option<TransportPacketData>;
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
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        (**self).transport_packet_data()
    }
}

impl<T: ?Sized> MaybeTransportPacket for &mut T
where
    T: MaybeTransportPacket,
{
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        (**self).transport_packet_data()
    }
}

impl<I: IpExt, T: ?Sized> MaybeTransportPacketMut<I> for &mut T
where
    T: MaybeTransportPacketMut<I>,
{
    type TransportPacketMut<'a>
        = T::TransportPacketMut<'a>
    where
        Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        (**self).transport_packet_mut()
    }
}

impl<I: IpExt, T: TransportPacketMut<I>> MaybeTransportPacketMut<I> for Option<T> {
    type TransportPacketMut<'a>
        = &'a mut T
    where
        Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        self.as_mut()
    }
}

/// A concrete enum to hold all of the transport packet data that could possibly be usefully
/// extracted from a packet.
#[derive(Debug, Clone, GenericOverIp, PartialEq, Eq)]
#[generic_over_ip()]
pub enum TransportPacketData {
    Tcp { src_port: u16, dst_port: u16, segment: SegmentHeader, payload_len: usize },
    Generic { src_port: u16, dst_port: u16 },
}

impl TransportPacketData {
    pub fn src_port(&self) -> u16 {
        match self {
            TransportPacketData::Tcp { src_port, .. }
            | TransportPacketData::Generic { src_port, .. } => *src_port,
        }
    }

    pub fn dst_port(&self) -> u16 {
        match self {
            TransportPacketData::Tcp { dst_port, .. }
            | TransportPacketData::Generic { dst_port, .. } => *dst_port,
        }
    }

    pub fn tcp_segment_and_len(&self) -> Option<(&SegmentHeader, usize)> {
        match self {
            TransportPacketData::Tcp { segment, payload_len, .. } => Some((&segment, *payload_len)),
            TransportPacketData::Generic { .. } => None,
        }
    }

    fn parse_in_ip_packet<I: IpExt, B: ParseBuffer>(
        src_ip: I::Addr,
        dst_ip: I::Addr,
        proto: I::Proto,
        body: B,
    ) -> Option<TransportPacketData> {
        I::map_ip(
            (src_ip, dst_ip, proto, IpInvariant(body)),
            |(src_ip, dst_ip, proto, IpInvariant(body))| {
                parse_transport_header_in_ipv4_packet(src_ip, dst_ip, proto, body)
            },
            |(src_ip, dst_ip, proto, IpInvariant(body))| {
                parse_transport_header_in_ipv6_packet(src_ip, dst_ip, proto, body)
            },
        )
    }
}

/// A transport layer packet that provides header modification.
//
// TODO(https://fxbug.dev/341128580): make this trait more expressive for the
// differences between transport protocols.
pub trait TransportPacketMut<I: IpExt> {
    /// Set the source port or identifier of the packet.
    fn set_src_port(&mut self, port: NonZeroU16);

    /// Set the destination port or identifier of the packet.
    fn set_dst_port(&mut self, port: NonZeroU16);

    /// Update the source IP address in the pseudo header.
    fn update_pseudo_header_src_addr(&mut self, old: I::Addr, new: I::Addr);

    /// Update the destination IP address in the pseudo header.
    fn update_pseudo_header_dst_addr(&mut self, old: I::Addr, new: I::Addr);
}

impl<B: SplitByteSliceMut> IpPacket<Ipv4> for Ipv4Packet<B> {
    type TransportPacket<'a>
        = &'a Self
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Option<ParsedTransportHeaderMut<'a, Ipv4>>
    where
        B: 'a;

    fn src_addr(&self) -> Ipv4Addr {
        self.src_ip()
    }

    fn set_src_addr(&mut self, addr: Ipv4Addr) {
        let old = self.src_addr();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }
        // NB: it is important that we do not update the IP layer header until
        // after parsing the transport header, because if we change the source
        // address in the IP header and then attempt to parse the transport
        // header using that address for checksum validation, parsing will fail.
        //
        // TODO(https://fxbug.dev/341340810): use raw packet parsing for all
        // transport header updates, which is less expensive and would allow
        // updating the IP and transport headers to be order-independent.
        self.set_src_ip_and_update_checksum(addr);
    }

    fn dst_addr(&self) -> Ipv4Addr {
        self.dst_ip()
    }

    fn set_dst_addr(&mut self, addr: Ipv4Addr) {
        let old = self.dst_addr();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }
        // NB: it is important that we do not update the IP layer header until
        // after parsing the transport header, because if we change the
        // destination address in the IP header and then attempt to parse the
        // transport header using that address for checksum validation, parsing
        // will fail.
        //
        // TODO(https://fxbug.dev/341340810): use raw packet parsing for all
        // transport header updates, which is less expensive and would allow
        // updating the IP and transport headers to be order-independent.
        self.set_dst_ip_and_update_checksum(addr);
    }

    fn protocol(&self) -> Ipv4Proto {
        self.proto()
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self
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

impl<B: SplitByteSlice> MaybeTransportPacket for Ipv4Packet<B> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        parse_transport_header_in_ipv4_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            self.body(),
        )
    }
}

impl<B: SplitByteSliceMut> IpPacket<Ipv6> for Ipv6Packet<B> {
    type TransportPacket<'a>
        = &'a Self
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Option<ParsedTransportHeaderMut<'a, Ipv6>>
    where
        B: 'a;

    fn src_addr(&self) -> Ipv6Addr {
        self.src_ip()
    }

    fn set_src_addr(&mut self, addr: Ipv6Addr) {
        let old = self.src_addr();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }
        // NB: it is important that we do not update the IP layer header until
        // after parsing the transport header, because if we change the source
        // address in the IP header and then attempt to parse the transport
        // header using that address for checksum validation, parsing will fail.
        //
        // TODO(https://fxbug.dev/341340810): use raw packet parsing for all
        // transport header updates, which is less expensive and would allow
        // updating the IP and transport headers to be order-independent.
        self.set_src_ip(addr);
    }

    fn dst_addr(&self) -> Ipv6Addr {
        self.dst_ip()
    }

    fn set_dst_addr(&mut self, addr: Ipv6Addr) {
        let old = self.dst_addr();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }
        // NB: it is important that we do not update the IP layer header until
        // after parsing the transport header, because if we change the
        // destination address in the IP header and then attempt to parse the
        // transport header using that address for checksum validation, parsing
        // will fail.
        //
        // TODO(https://fxbug.dev/341340810): use raw packet parsing for all
        // transport header updates, which is less expensive and would allow
        // updating the IP and transport headers to be order-independent.
        self.set_dst_ip(addr);
    }

    fn protocol(&self) -> Ipv6Proto {
        self.proto()
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self
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

impl<B: SplitByteSlice> MaybeTransportPacket for Ipv6Packet<B> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        parse_transport_header_in_ipv6_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            self.body(),
        )
    }
}

/// An outgoing IP packet that has not yet been wrapped into an outer serializer
/// type.
#[derive(Debug, PartialEq, GenericOverIp)]
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

    /// The source IP address of the packet.
    pub fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    /// The destination IP address of the packet.
    pub fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }
}

impl<I: IpExt, S: TransportPacketSerializer<I>> IpPacket<I> for TxPacket<'_, I, S> {
    type TransportPacket<'a>
        = &'a S
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = &'a mut S
    where
        Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn set_src_addr(&mut self, addr: I::Addr) {
        let old = core::mem::replace(&mut self.src_addr, addr);
        if let Some(mut packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn set_dst_addr(&mut self, addr: I::Addr) {
        let old = core::mem::replace(&mut self.dst_addr, addr);
        if let Some(mut packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self.serializer
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        self.serializer
    }
}

/// An incoming IP packet that is being forwarded.
#[derive(Debug, PartialEq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct ForwardedPacket<I: IpExt, B> {
    src_addr: I::Addr,
    dst_addr: I::Addr,
    protocol: I::Proto,
    transport_header_offset: usize,
    buffer: B,
}

impl<I: IpExt, B: BufferMut> ForwardedPacket<I, B> {
    /// Create a new [`ForwardedPacket`] from its IP header fields and payload.
    ///
    /// `meta` is used to revert `buffer` back to the IP header for further
    /// serialization, and to mark where the transport header starts in
    /// `buffer`. It _must_ have originated from a previously parsed IP packet
    /// on `buffer`.
    pub fn new(
        src_addr: I::Addr,
        dst_addr: I::Addr,
        protocol: I::Proto,
        meta: ParseMetadata,
        mut buffer: B,
    ) -> Self {
        let transport_header_offset = meta.header_len();
        buffer.undo_parse(meta);
        Self { src_addr, dst_addr, protocol, transport_header_offset, buffer }
    }

    /// Discard the metadata carried by the [`ForwardedPacket`] and return the
    /// inner buffer.
    ///
    /// The returned buffer is guaranteed to contain a valid IP frame, the
    /// start of the buffer points at the start of the IP header.
    pub fn into_buffer(self) -> B {
        self.buffer
    }

    /// Returns a reference to the forwarded buffer.
    ///
    /// The returned reference is guaranteed to contain a valid IP frame, the
    /// start of the buffer points at the start of the IP header.
    pub fn buffer(&self) -> &B {
        &self.buffer
    }
}

impl<I: IpExt, B: BufferMut> Serializer for ForwardedPacket<I, B> {
    type Buffer = <B as Serializer>::Buffer;

    fn serialize<G: packet::GrowBufferMut, P: packet::BufferProvider<Self::Buffer, G>>(
        self,
        outer: packet::PacketConstraints,
        provider: P,
    ) -> Result<G, (packet::SerializeError<P::Error>, Self)> {
        let Self { src_addr, dst_addr, protocol, transport_header_offset, buffer } = self;
        buffer.serialize(outer, provider).map_err(|(err, buffer)| {
            (err, Self { src_addr, dst_addr, protocol, transport_header_offset, buffer })
        })
    }

    fn serialize_new_buf<BB: packet::ReusableBuffer, A: packet::BufferAlloc<BB>>(
        &self,
        outer: packet::PacketConstraints,
        alloc: A,
    ) -> Result<BB, packet::SerializeError<A::Error>> {
        self.buffer.serialize_new_buf(outer, alloc)
    }
}

impl<I: IpExt, B: BufferMut> IpPacket<I> for ForwardedPacket<I, B> {
    type TransportPacket<'a>
        = &'a Self
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Option<ParsedTransportHeaderMut<'a, I>>
    where
        Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.src_addr
    }

    fn set_src_addr(&mut self, addr: I::Addr) {
        // Re-parse the IP header so we can modify it in place.
        I::map_ip::<_, ()>(
            (IpInvariant(self.buffer.as_mut()), addr),
            |(IpInvariant(buffer), addr)| {
                let mut packet = Ipv4PacketRaw::parse_mut(SliceBufViewMut::new(buffer), ())
                    .expect("ForwardedPacket must have been created from a valid IP packet");
                packet.set_src_ip_and_update_checksum(addr);
            },
            |(IpInvariant(buffer), addr)| {
                let mut packet = Ipv6PacketRaw::parse_mut(SliceBufViewMut::new(buffer), ())
                    .expect("ForwardedPacket must have been created from a valid IP packet");
                packet.set_src_ip(addr);
            },
        );

        let old = self.src_addr;
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }
        // NB: it is important that we do not update this until after parsing
        // the transport header, because if we change the source address and
        // then attempt to parse the transport header using that address for
        // checksum validation, parsing will fail.
        //
        // TODO(https://fxbug.dev/341340810): use raw packet parsing for all
        // transport header updates, which is less expensive and would allow
        // updating the IP and transport headers to be order-independent.
        self.src_addr = addr;
    }

    fn dst_addr(&self) -> I::Addr {
        self.dst_addr
    }

    fn set_dst_addr(&mut self, addr: I::Addr) {
        // Re-parse the IP header so we can modify it in place.
        I::map_ip::<_, ()>(
            (IpInvariant(self.buffer.as_mut()), addr),
            |(IpInvariant(buffer), addr)| {
                let mut packet = Ipv4PacketRaw::parse_mut(SliceBufViewMut::new(buffer), ())
                    .expect("ForwardedPacket must have been created from a valid IP packet");
                packet.set_dst_ip_and_update_checksum(addr);
            },
            |(IpInvariant(buffer), addr)| {
                let mut packet = Ipv6PacketRaw::parse_mut(SliceBufViewMut::new(buffer), ())
                    .expect("ForwardedPacket must have been created from a valid IP packet");
                packet.set_dst_ip(addr);
            },
        );

        let old = self.dst_addr;
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }
        // NB: it is important that we do not update this until after parsing
        // the transport header, because if we change the destination address
        // and then attempt to parse the transport header using that address for
        // checksum validation, parsing will fail.
        //
        // TODO(https://fxbug.dev/341340810): use raw packet parsing for all
        // transport header updates, which is less expensive and would allow
        // updating the IP and transport headers to be order-independent.
        self.dst_addr = addr;
    }

    fn protocol(&self) -> I::Proto {
        self.protocol
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        let ForwardedPacket { src_addr, dst_addr, protocol, buffer, transport_header_offset } =
            self;
        ParsedTransportHeaderMut::<I>::parse_in_ip_packet(
            *src_addr,
            *dst_addr,
            *protocol,
            SliceBufViewMut::new(&mut buffer.as_mut()[*transport_header_offset..]),
        )
    }
}

impl<I: IpExt, B: BufferMut> MaybeTransportPacket for ForwardedPacket<I, B> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        let ForwardedPacket { protocol, buffer, src_addr, dst_addr, transport_header_offset } =
            self;
        TransportPacketData::parse_in_ip_packet::<I, _>(
            *src_addr,
            *dst_addr,
            *protocol,
            Buf::new(&buffer.as_ref()[*transport_header_offset..], ..),
        )
    }
}

impl<I: IpExt, S: TransportPacketSerializer<I>, B: IpPacketBuilder<I>> IpPacket<I>
    for Nested<S, B>
{
    type TransportPacket<'a>
        = &'a S
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = &'a mut S
    where
        Self: 'a;

    fn src_addr(&self) -> I::Addr {
        self.outer().src_ip()
    }

    fn set_src_addr(&mut self, addr: I::Addr) {
        let old = self.outer().src_ip();
        self.outer_mut().set_src_ip(addr);
        if let Some(mut packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }
    }

    fn dst_addr(&self) -> I::Addr {
        self.outer().dst_ip()
    }

    fn set_dst_addr(&mut self, addr: I::Addr) {
        let old = self.outer().dst_ip();
        self.outer_mut().set_dst_ip(addr);
        if let Some(mut packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }
    }

    fn protocol(&self) -> I::Proto {
        self.outer().proto()
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self.inner()
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        self.inner_mut()
    }
}

impl<I: IpExt, T: ?Sized> TransportPacketMut<I> for &mut T
where
    T: TransportPacketMut<I>,
{
    fn set_src_port(&mut self, port: NonZeroU16) {
        (*self).set_src_port(port);
    }

    fn set_dst_port(&mut self, port: NonZeroU16) {
        (*self).set_dst_port(port);
    }

    fn update_pseudo_header_src_addr(&mut self, old: I::Addr, new: I::Addr) {
        (*self).update_pseudo_header_src_addr(old, new);
    }

    fn update_pseudo_header_dst_addr(&mut self, old: I::Addr, new: I::Addr) {
        (*self).update_pseudo_header_dst_addr(old, new);
    }
}

impl MaybeTransportPacket for Never {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        match *self {}
    }
}

impl<I: IpExt> MaybeTransportPacketMut<I> for Never {
    type TransportPacketMut<'a>
        = Never
    where
        Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        match *self {}
    }
}

impl<I: IpExt> TransportPacketMut<I> for Never {
    fn set_src_port(&mut self, _: NonZeroU16) {
        match *self {}
    }

    fn set_dst_port(&mut self, _: NonZeroU16) {
        match *self {}
    }

    fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {
        match *self {}
    }

    fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {
        match *self {}
    }
}

impl<A: IpAddress, Inner> MaybeTransportPacket for Nested<Inner, UdpPacketBuilder<A>> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        Some(TransportPacketData::Generic {
            src_port: self.outer().src_port().map_or(0, NonZeroU16::get),
            dst_port: self.outer().dst_port().map_or(0, NonZeroU16::get),
        })
    }
}

impl<I: IpExt, Inner> MaybeTransportPacketMut<I> for Nested<Inner, UdpPacketBuilder<I::Addr>> {
    type TransportPacketMut<'a>
        = &'a mut Self
    where
        Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        Some(self)
    }
}

impl<I: IpExt, Inner> TransportPacketMut<I> for Nested<Inner, UdpPacketBuilder<I::Addr>> {
    fn set_src_port(&mut self, port: NonZeroU16) {
        self.outer_mut().set_src_port(port.get());
    }

    fn set_dst_port(&mut self, port: NonZeroU16) {
        self.outer_mut().set_dst_port(port);
    }

    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_dst_ip(new);
    }
}

impl<'a, A: IpAddress, Inner: PayloadLen, I> MaybeTransportPacket
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, OptionSequenceBuilder<TcpOption<'a>, I>>>
where
    I: Iterator + Clone,
    I::Item: Borrow<TcpOption<'a>>,
{
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        Some(TransportPacketData::Tcp {
            src_port: self.outer().src_port().map_or(0, NonZeroU16::get),
            dst_port: self.outer().dst_port().map_or(0, NonZeroU16::get),
            segment: self.outer().try_into().ok()?,
            payload_len: self.inner().len(),
        })
    }
}

impl<I: IpExt, Outer, Inner> MaybeTransportPacketMut<I>
    for Nested<Inner, TcpSegmentBuilderWithOptions<I::Addr, Outer>>
{
    type TransportPacketMut<'a>
        = &'a mut Self
    where
        Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        Some(self)
    }
}

impl<I: IpExt, Outer, Inner> TransportPacketMut<I>
    for Nested<Inner, TcpSegmentBuilderWithOptions<I::Addr, Outer>>
{
    fn set_src_port(&mut self, port: NonZeroU16) {
        self.outer_mut().set_src_port(port);
    }

    fn set_dst_port(&mut self, port: NonZeroU16) {
        self.outer_mut().set_dst_port(port);
    }

    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.outer_mut().set_dst_ip(new);
    }
}

impl<I: IpExt, Inner, M: IcmpMessage<I>> MaybeTransportPacket
    for Nested<Inner, IcmpPacketBuilder<I, M>>
{
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        self.outer().message().transport_packet_data()
    }
}

impl<I: IpExt, Inner, M: IcmpMessage<I>> MaybeTransportPacketMut<I>
    for Nested<Inner, IcmpPacketBuilder<I, M>>
{
    type TransportPacketMut<'a>
        = &'a mut IcmpPacketBuilder<I, M>
    where
        M: 'a,
        Inner: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        Some(self.outer_mut())
    }
}

impl<I: IpExt, M: IcmpMessage<I>> TransportPacketMut<I> for IcmpPacketBuilder<I, M> {
    fn set_src_port(&mut self, id: NonZeroU16) {
        self.message_mut().set_src_port(id.get());
    }

    fn set_dst_port(&mut self, id: NonZeroU16) {
        self.message_mut().set_dst_port(id.get());
    }

    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.set_dst_ip(new);
    }
}

/// An ICMP message type that may allow for transport-layer packet inspection.
pub trait IcmpMessage<I: IpExt>: icmp::IcmpMessage<I> + MaybeTransportPacket {
    /// Set the notional source port of the ICMP message, which for ICMP Echo
    /// Requests is the ID.
    ///
    /// # Panics
    ///
    /// Panics if this is called on an ICMP message that does not support packet
    /// rewriting (currently all non-echo message types), or on an ICMP echo
    /// reply.
    fn set_src_port(&mut self, id: u16);

    /// Set the notional destination port of the ICMP message, which for ICMP
    /// Echo Replies is the ID.
    ///
    /// # Panics
    ///
    /// Panics if this is called on an ICMP message that does not support packet
    /// rewriting (currently all non-echo message types), or on an ICMP echo
    /// request.
    fn set_dst_port(&mut self, id: u16);
}

// TODO(https://fxbug.dev/341128580): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl MaybeTransportPacket for IcmpEchoReply {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        Some(TransportPacketData::Generic { src_port: 0, dst_port: self.id() })
    }
}

impl<I: IpExt> IcmpMessage<I> for IcmpEchoReply {
    fn set_src_port(&mut self, _: u16) {
        panic!("cannot set source port for ICMP echo reply")
    }

    fn set_dst_port(&mut self, id: u16) {
        self.set_id(id);
    }
}

// TODO(https://fxbug.dev/341128580): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl MaybeTransportPacket for IcmpEchoRequest {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        Some(TransportPacketData::Generic { src_port: self.id(), dst_port: 0 })
    }
}

impl<I: IpExt> IcmpMessage<I> for IcmpEchoRequest {
    fn set_src_port(&mut self, id: u16) {
        self.set_id(id);
    }

    fn set_dst_port(&mut self, _: u16) {
        panic!("cannot set destination port for ICMP echo request")
    }
}

macro_rules! unsupported_icmp_message_type {
    ($message:ty, $($ips:ty),+) => {
        impl MaybeTransportPacket for $message {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                None
            }
        }

        $(
            impl IcmpMessage<$ips> for $message {
                fn set_src_port(&mut self, _: u16) {
                    unreachable!("non-echo ICMP packets should never be rewritten")
                }

                fn set_dst_port(&mut self, _: u16) {
                    unreachable!("non-echo ICMP packets should never be rewritten")
                }
            }
        )+
    };
}

unsupported_icmp_message_type!(Icmpv4TimestampReply, Ipv4);
unsupported_icmp_message_type!(NeighborSolicitation, Ipv6);
unsupported_icmp_message_type!(NeighborAdvertisement, Ipv6);
unsupported_icmp_message_type!(RouterSolicitation, Ipv6);
unsupported_icmp_message_type!(MulticastListenerDone, Ipv6);
unsupported_icmp_message_type!(MulticastListenerReport, Ipv6);

// Transport layer packet inspection is not currently supported for any ICMP
// error message types.
//
// TODO(https://fxbug.dev/328057704): parse the IP packet contained in the ICMP
// error message payload so NAT can be applied to it.
macro_rules! icmp_error_message {
    ($message:ty, $($ips:ty),+) => {
        unsupported_icmp_message_type!($message, $( $ips ),+);
    };
}

icmp_error_message!(IcmpDestUnreachable, Ipv4, Ipv6);
icmp_error_message!(IcmpTimeExceeded, Ipv4, Ipv6);
icmp_error_message!(Icmpv4ParameterProblem, Ipv4);
icmp_error_message!(Icmpv6ParameterProblem, Ipv6);
icmp_error_message!(Icmpv6PacketTooBig, Ipv6);

impl<M: igmp::MessageType<EmptyBuf>> MaybeTransportPacket
    for InnerSerializer<IgmpPacketBuilder<EmptyBuf, M>, EmptyBuf>
{
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        None
    }
}

impl<M: igmp::MessageType<EmptyBuf>> MaybeTransportPacketMut<Ipv4>
    for InnerSerializer<IgmpPacketBuilder<EmptyBuf, M>, EmptyBuf>
{
    type TransportPacketMut<'a>
        = Never
    where
        M: 'a;

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
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        None
    }
}

/// An unsanitized IP packet body.
///
/// Allows packets from raw IP sockets (with a user provided IP body), to be
/// tracked from the filtering module.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct RawIpBody<I: IpExt, B: ParseBuffer> {
    /// The IANA protocol of the inner message. This may be, but is not required
    /// to be, a transport protocol.
    protocol: I::Proto,
    /// The source IP addr of the packet. Required by
    /// [`ParsedTransportHeaderMut`] to recompute checksums.
    src_addr: I::Addr,
    /// The destination IP addr of the packet. Required by
    /// [`ParsedTransportHeaderMut`] to recompute checksums.
    dst_addr: I::Addr,
    /// The body of the IP packet. The body is expected to be a message of type
    /// `protocol`, but is not guaranteed to be valid.
    body: B,
    /// The parsed transport data contained within `body`. Only `Some` if body
    /// is a valid transport header.
    transport_packet_data: Option<TransportPacketData>,
}

impl<I: IpExt, B: ParseBuffer> RawIpBody<I, B> {
    /// Construct a new [`RawIpBody`] from it's parts.
    pub fn new(
        protocol: I::Proto,
        src_addr: I::Addr,
        dst_addr: I::Addr,
        body: B,
    ) -> RawIpBody<I, B> {
        let transport_packet_data = TransportPacketData::parse_in_ip_packet::<I, _>(
            src_addr,
            dst_addr,
            protocol,
            Buf::new(&body, ..),
        );
        RawIpBody { protocol, src_addr, dst_addr, body, transport_packet_data }
    }
}

impl<I: IpExt, B: ParseBuffer> MaybeTransportPacket for RawIpBody<I, B> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        self.transport_packet_data.clone()
    }
}

impl<I: IpExt, B: BufferMut> MaybeTransportPacketMut<I> for RawIpBody<I, B> {
    type TransportPacketMut<'a>
        = ParsedTransportHeaderMut<'a, I>
    where
        Self: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        let RawIpBody { protocol, src_addr, dst_addr, body, transport_packet_data: _ } = self;
        ParsedTransportHeaderMut::<I>::parse_in_ip_packet(
            *src_addr,
            *dst_addr,
            *protocol,
            SliceBufViewMut::new(body.as_mut()),
        )
    }
}

impl<I: IpExt, B: BufferMut> Serializer for RawIpBody<I, B> {
    type Buffer = <B as Serializer>::Buffer;

    fn serialize<G: GrowBufferMut, P: BufferProvider<Self::Buffer, G>>(
        self,
        outer: PacketConstraints,
        provider: P,
    ) -> Result<G, (SerializeError<P::Error>, Self)> {
        let Self { protocol, src_addr, dst_addr, body, transport_packet_data } = self;
        body.serialize(outer, provider).map_err(|(err, body)| {
            (err, Self { protocol, src_addr, dst_addr, body, transport_packet_data })
        })
    }

    fn serialize_new_buf<BB: ReusableBuffer, A: BufferAlloc<BB>>(
        &self,
        outer: PacketConstraints,
        alloc: A,
    ) -> Result<BB, SerializeError<A::Error>> {
        self.body.serialize_new_buf(outer, alloc)
    }
}

fn parse_transport_header_in_ipv4_packet<B: ParseBuffer>(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    proto: Ipv4Proto,
    body: B,
) -> Option<TransportPacketData> {
    match proto {
        Ipv4Proto::Proto(IpProto::Udp) => parse_udp_header::<_, Ipv4>(body),
        Ipv4Proto::Proto(IpProto::Tcp) => parse_tcp_header::<_, Ipv4>(body, src_ip, dst_ip),
        Ipv4Proto::Icmp => parse_icmpv4_header(body),
        Ipv4Proto::Proto(IpProto::Reserved) | Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
    }
}

fn parse_transport_header_in_ipv6_packet<B: ParseBuffer>(
    src_ip: Ipv6Addr,
    dst_ip: Ipv6Addr,
    proto: Ipv6Proto,
    body: B,
) -> Option<TransportPacketData> {
    match proto {
        Ipv6Proto::Proto(IpProto::Udp) => parse_udp_header::<_, Ipv6>(body),
        Ipv6Proto::Proto(IpProto::Tcp) => parse_tcp_header::<_, Ipv6>(body, src_ip, dst_ip),
        Ipv6Proto::Icmpv6 => parse_icmpv6_header(body),
        Ipv6Proto::Proto(IpProto::Reserved) | Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => None,
    }
}

fn parse_udp_header<B: ParseBuffer, I: Ip>(mut body: B) -> Option<TransportPacketData> {
    let packet = body.parse_with::<_, UdpPacketRaw<_>>(I::VERSION_MARKER).ok()?;
    Some(TransportPacketData::Generic {
        src_port: packet.src_port().map(NonZeroU16::get).unwrap_or(0),
        // NB: UDP packets must have a specified (nonzero) destination port, so
        // if this packet has a destination port of 0, it is malformed.
        dst_port: packet.dst_port()?.get(),
    })
}

fn parse_tcp_header<B: ParseBuffer, I: IpExt>(
    mut body: B,
    src_ip: I::Addr,
    dst_ip: I::Addr,
) -> Option<TransportPacketData> {
    // NOTE: By using TcpSegmentRaw here, we're opting into getting invalid data
    // (for example, if the checksum isn't valid). As a team, we've decided
    // that's okay for now, since the worst that happens is we filter or
    // conntrack a packet incorrectly and the end host rejects it.
    //
    // This will be fixed at some point as part of a larger effort to ensure
    // that checksums are validated exactly once (and hopefully via checksum
    // offloading).
    let packet = body.parse::<TcpSegmentRaw<_>>().ok()?;

    let (builder, options, body) = packet.into_builder_options(src_ip, dst_ip)?;
    let options = Options::from_iter(options.iter());

    let segment = SegmentHeader::from_builder_options(&builder, options).ok()?;

    Some(TransportPacketData::Tcp {
        src_port: builder.src_port().map(NonZeroU16::get).unwrap_or(0),
        dst_port: builder.dst_port().map(NonZeroU16::get).unwrap_or(0),
        segment,
        payload_len: body.len(),
    })
}

fn parse_icmpv4_header<B: ParseBuffer>(mut body: B) -> Option<TransportPacketData> {
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
    Some(TransportPacketData::Generic { src_port, dst_port })
}

fn parse_icmpv6_header<B: ParseBuffer>(mut body: B) -> Option<TransportPacketData> {
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
    Some(TransportPacketData::Generic { src_port, dst_port })
}

/// A transport header that has been parsed from a byte buffer and provides
/// mutable access to its contents.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub enum ParsedTransportHeaderMut<'a, I: IpExt> {
    Tcp(TcpSegment<&'a mut [u8]>),
    Udp(UdpPacket<&'a mut [u8]>),
    Icmp(I::IcmpPacketTypeRaw<&'a mut [u8]>),
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
            Ipv4Proto::Icmp => Some(Self::Icmp(Icmpv4PacketRaw::parse_mut(body, ()).ok()?)),
            Ipv4Proto::Proto(IpProto::Reserved) | Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
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
            Ipv6Proto::Icmpv6 => Some(Self::Icmp(Icmpv6PacketRaw::parse_mut(body, ()).ok()?)),
            Ipv6Proto::Proto(IpProto::Reserved) | Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => {
                None
            }
        }
    }
}

impl<'a, I: IpExt> ParsedTransportHeaderMut<'a, I> {
    fn parse_in_ip_packet<BV: BufferViewMut<&'a mut [u8]>>(
        src_ip: I::Addr,
        dst_ip: I::Addr,
        proto: I::Proto,
        body: BV,
    ) -> Option<Self> {
        I::map_ip(
            (src_ip, dst_ip, proto, IpInvariant(body)),
            |(src_ip, dst_ip, proto, IpInvariant(body))| {
                ParsedTransportHeaderMut::<'a, Ipv4>::parse_in_ipv4_packet(
                    src_ip, dst_ip, proto, body,
                )
            },
            |(src_ip, dst_ip, proto, IpInvariant(body))| {
                ParsedTransportHeaderMut::<'a, Ipv6>::parse_in_ipv6_packet(
                    src_ip, dst_ip, proto, body,
                )
            },
        )
    }

    fn update_pseudo_header_address(&mut self, old: I::Addr, new: I::Addr) {
        match self {
            Self::Tcp(segment) => segment.update_checksum_pseudo_header_address(old, new),
            Self::Udp(packet) => {
                packet.update_checksum_pseudo_header_address(old, new);
            }
            Self::Icmp(packet) => {
                packet.update_checksum_pseudo_header_address(old, new);
            }
        }
    }
}

impl<'a, I: IpExt> TransportPacketMut<I> for ParsedTransportHeaderMut<'a, I> {
    fn set_src_port(&mut self, port: NonZeroU16) {
        match self {
            ParsedTransportHeaderMut::Tcp(segment) => segment.set_src_port(port),
            ParsedTransportHeaderMut::Udp(packet) => packet.set_src_port(port.get()),
            ParsedTransportHeaderMut::Icmp(packet) => {
                I::map_ip::<_, ()>(
                    packet,
                    |packet| {
                        use Icmpv4PacketRaw::*;
                        match packet {
                            EchoRequest(packet) => packet.set_id(port.get()),
                            EchoReply(_) => panic!("cannot set source port for ICMP echo reply"),
                            DestUnreachable(_) | Redirect(_) | TimeExceeded(_)
                            | ParameterProblem(_) | TimestampRequest(_) | TimestampReply(_) => {
                                unreachable!("non-echo ICMP packets should never be rewritten")
                            }
                        }
                    },
                    |packet| {
                        use Icmpv6PacketRaw::*;
                        match packet {
                            EchoRequest(packet) => packet.set_id(port.get()),
                            EchoReply(_) => panic!("cannot set source port for ICMPv6 echo reply"),
                            DestUnreachable(_) | PacketTooBig(_) | TimeExceeded(_)
                            | ParameterProblem(_) | Ndp(_) | Mld(_) => {
                                unreachable!("non-echo ICMPv6 packets should never be rewritten")
                            }
                        }
                    },
                );
            }
        }
    }

    fn set_dst_port(&mut self, port: NonZeroU16) {
        match self {
            ParsedTransportHeaderMut::Tcp(ref mut segment) => segment.set_dst_port(port),
            ParsedTransportHeaderMut::Udp(ref mut packet) => packet.set_dst_port(port),
            ParsedTransportHeaderMut::Icmp(packet) => {
                I::map_ip::<_, ()>(
                    packet,
                    |packet| {
                        use Icmpv4PacketRaw::*;
                        match packet {
                            EchoReply(packet) => packet.set_id(port.get()),
                            EchoRequest(_) => {
                                panic!("cannot set destination port for ICMP echo request")
                            }
                            DestUnreachable(_) | Redirect(_) | TimeExceeded(_)
                            | ParameterProblem(_) | TimestampRequest(_) | TimestampReply(_) => {
                                unreachable!("non-echo ICMP packets should never be rewritten")
                            }
                        }
                    },
                    |packet| {
                        use Icmpv6PacketRaw::*;
                        match packet {
                            EchoReply(packet) => packet.set_id(port.get()),
                            EchoRequest(_) => {
                                panic!("cannot set destination port for ICMPv6 echo request")
                            }
                            DestUnreachable(_) | PacketTooBig(_) | TimeExceeded(_)
                            | ParameterProblem(_) | Ndp(_) | Mld(_) => {
                                unreachable!("non-echo ICMPv6 packets should never be rewritten")
                            }
                        }
                    },
                );
            }
        }
    }

    fn update_pseudo_header_src_addr(&mut self, old: I::Addr, new: I::Addr) {
        self.update_pseudo_header_address(old, new);
    }

    fn update_pseudo_header_dst_addr(&mut self, old: I::Addr, new: I::Addr) {
        self.update_pseudo_header_address(old, new);
    }
}

#[cfg(any(test, feature = "testutils"))]
pub mod testutil {
    use super::*;

    // Note that we could choose to implement `MaybeTransportPacket` for these
    // opaque byte buffer types by parsing them as we do incoming buffers, but since
    // these implementations are only for use in netstack3_core unit tests, there is
    // no expectation that filtering or connection tracking actually be performed.
    // If that changes at some point, we could replace these with "real"
    // implementations.

    impl<B: BufferMut> MaybeTransportPacket for Nested<B, ()> {
        fn transport_packet_data(&self) -> Option<TransportPacketData> {
            unimplemented!()
        }
    }

    impl<I: IpExt, B: BufferMut> MaybeTransportPacketMut<I> for Nested<B, ()> {
        type TransportPacketMut<'a>
            = Never
        where
            B: 'a;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
            unimplemented!()
        }
    }

    impl MaybeTransportPacket for InnerSerializer<&[u8], EmptyBuf> {
        fn transport_packet_data(&self) -> Option<TransportPacketData> {
            None
        }
    }

    impl<I: IpExt> MaybeTransportPacketMut<I> for InnerSerializer<&[u8], EmptyBuf> {
        type TransportPacketMut<'a>
            = Never
        where
            Self: 'a;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
            None
        }
    }

    #[cfg(test)]
    pub(crate) mod internal {
        use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
        use net_types::ip::Subnet;
        use netstack3_base::{SeqNum, UnscaledWindowSize};

        use super::*;

        pub trait TestIpExt: IpExt {
            const SRC_IP: Self::Addr;
            const DST_IP: Self::Addr;
            const SRC_IP_2: Self::Addr;
            const DST_IP_2: Self::Addr;
            const IP_OUTSIDE_SUBNET: Self::Addr;
            const SUBNET: Subnet<Self::Addr>;
        }

        impl TestIpExt for Ipv4 {
            const SRC_IP: Self::Addr = net_ip_v4!("192.0.2.1");
            const DST_IP: Self::Addr = net_ip_v4!("192.0.2.2");
            const SRC_IP_2: Self::Addr = net_ip_v4!("192.0.2.3");
            const DST_IP_2: Self::Addr = net_ip_v4!("192.0.2.4");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v4!("192.0.3.1");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v4!("192.0.2.0/24");
        }

        impl TestIpExt for Ipv6 {
            const SRC_IP: Self::Addr = net_ip_v6!("2001:db8::1");
            const DST_IP: Self::Addr = net_ip_v6!("2001:db8::2");
            const SRC_IP_2: Self::Addr = net_ip_v6!("2001:db8::3");
            const DST_IP_2: Self::Addr = net_ip_v6!("2001:db8::4");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v6!("2001:db8:ffff::1");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v6!("2001:db8::/64");
        }

        #[derive(Clone, Debug, PartialEq)]
        pub struct FakeIpPacket<I: IpExt, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
        {
            pub src_ip: I::Addr,
            pub dst_ip: I::Addr,
            pub body: T,
        }

        impl<I: IpExt> FakeIpPacket<I, FakeUdpPacket> {
            pub(crate) fn reply(&self) -> Self {
                Self { src_ip: self.dst_ip, dst_ip: self.src_ip, body: self.body.reply() }
            }
        }

        pub trait TransportPacketExt<I: IpExt>: MaybeTransportPacket {
            fn proto() -> I::Proto;
        }

        impl<I: IpExt, T> IpPacket<I> for FakeIpPacket<I, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
            for<'a> &'a mut T: MaybeTransportPacketMut<I>,
        {
            type TransportPacket<'a>
                = &'a T
            where
                T: 'a;
            type TransportPacketMut<'a>
                = &'a mut T
            where
                T: 'a;

            fn src_addr(&self) -> I::Addr {
                self.src_ip
            }

            fn set_src_addr(&mut self, addr: I::Addr) {
                self.src_ip = addr;
            }

            fn dst_addr(&self) -> I::Addr {
                self.dst_ip
            }

            fn set_dst_addr(&mut self, addr: I::Addr) {
                self.dst_ip = addr;
            }

            fn protocol(&self) -> I::Proto {
                <&T>::proto()
            }

            fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
                &self.body
            }

            fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
                &mut self.body
            }
        }

        #[derive(Clone, Debug, PartialEq)]
        pub struct FakeTcpSegment {
            pub src_port: u16,
            pub dst_port: u16,
            pub segment: SegmentHeader,
            pub payload_len: usize,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeTcpSegment {
            fn proto() -> I::Proto {
                I::map_ip_out(
                    (),
                    |()| Ipv4Proto::Proto(IpProto::Tcp),
                    |()| Ipv6Proto::Proto(IpProto::Tcp),
                )
            }
        }

        impl MaybeTransportPacket for &FakeTcpSegment {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                Some(TransportPacketData::Tcp {
                    src_port: self.src_port,
                    dst_port: self.dst_port,
                    segment: self.segment,
                    payload_len: self.payload_len,
                })
            }
        }

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeTcpSegment {
            type TransportPacketMut<'a> = &'a mut Self;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                Some(self)
            }
        }

        impl<I: IpExt> TransportPacketMut<I> for FakeTcpSegment {
            fn set_src_port(&mut self, port: NonZeroU16) {
                self.src_port = port.get();
            }

            fn set_dst_port(&mut self, port: NonZeroU16) {
                self.dst_port = port.get();
            }

            fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {}

            fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {}
        }

        #[derive(Clone, Debug, PartialEq)]
        pub struct FakeUdpPacket {
            pub src_port: u16,
            pub dst_port: u16,
        }

        impl FakeUdpPacket {
            fn reply(&self) -> Self {
                Self { src_port: self.dst_port, dst_port: self.src_port }
            }
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeUdpPacket {
            fn proto() -> I::Proto {
                I::map_ip_out(
                    (),
                    |()| Ipv4Proto::Proto(IpProto::Udp),
                    |()| Ipv6Proto::Proto(IpProto::Udp),
                )
            }
        }

        impl MaybeTransportPacket for &FakeUdpPacket {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                Some(TransportPacketData::Generic {
                    src_port: self.src_port,
                    dst_port: self.dst_port,
                })
            }
        }

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeUdpPacket {
            type TransportPacketMut<'a> = &'a mut Self;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                Some(self)
            }
        }

        impl<I: IpExt> TransportPacketMut<I> for FakeUdpPacket {
            fn set_src_port(&mut self, port: NonZeroU16) {
                self.src_port = port.get();
            }

            fn set_dst_port(&mut self, port: NonZeroU16) {
                self.dst_port = port.get();
            }

            fn update_pseudo_header_src_addr(&mut self, _: I::Addr, _: I::Addr) {}

            fn update_pseudo_header_dst_addr(&mut self, _: I::Addr, _: I::Addr) {}
        }

        pub struct FakeIcmpEchoRequest {
            pub id: u16,
        }

        impl<I: IpExt> TransportPacketExt<I> for &FakeIcmpEchoRequest {
            fn proto() -> I::Proto {
                I::map_ip_out((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
            }
        }

        impl MaybeTransportPacket for &FakeIcmpEchoRequest {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                Some(TransportPacketData::Generic { src_port: self.id, dst_port: 0 })
            }
        }

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeIcmpEchoRequest {
            type TransportPacketMut<'a> = &'a mut Self;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                Some(self)
            }
        }

        impl<I: IpExt> TransportPacketMut<I> for FakeIcmpEchoRequest {
            fn set_src_port(&mut self, port: NonZeroU16) {
                self.id = port.get();
            }

            fn set_dst_port(&mut self, _: NonZeroU16) {
                panic!("cannot set destination port for ICMP echo request")
            }

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
                FakeTcpSegment {
                    src_port: 33333,
                    dst_port: 44444,
                    segment: SegmentHeader::arbitrary_value(),
                    payload_len: 8888,
                }
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

        impl ArbitraryValue for SegmentHeader {
            fn arbitrary_value() -> Self {
                SegmentHeader {
                    seq: SeqNum::new(55555),
                    ack: None,
                    control: None,
                    wnd: UnscaledWindowSize::from(1234),
                    options: Options::default(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::fmt::Debug;
    use netstack3_base::{SeqNum, UnscaledWindowSize};

    use const_unwrap::const_unwrap_option;
    use ip_test_macro::ip_test;
    use packet::InnerPacketBuilder as _;
    use packet_formats::icmp::IcmpUnusedCode;
    use packet_formats::tcp::TcpSegmentBuilder;
    use test_case::test_case;

    use super::testutil::internal::TestIpExt;
    use super::*;

    const SRC_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(11111));
    const DST_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(22222));
    const SRC_PORT_2: NonZeroU16 = const_unwrap_option(NonZeroU16::new(44444));
    const DST_PORT_2: NonZeroU16 = const_unwrap_option(NonZeroU16::new(55555));

    const SEQ_NUM: u32 = 1;
    const ACK_NUM: Option<u32> = Some(2);
    const WINDOW_SIZE: u16 = 3u16;

    trait Protocol {
        type Serializer<'a, I: IpExt>: TransportPacketSerializer<I, Buffer: packet::ReusableBuffer>
            + MaybeTransportPacketMut<I>
            + Debug
            + PartialEq;

        fn proto<I: IpExt>() -> I::Proto;

        fn make_serializer_with_ports<'a, I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
        ) -> Self::Serializer<'a, I>;

        fn make_serializer<'a, I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
        ) -> Self::Serializer<'a, I> {
            Self::make_serializer_with_ports(src_ip, dst_ip, SRC_PORT, DST_PORT)
        }

        fn make_packet<I: IpExt>(src_ip: I::Addr, dst_ip: I::Addr) -> Vec<u8> {
            Self::make_packet_with_ports::<I>(src_ip, dst_ip, SRC_PORT, DST_PORT)
        }

        fn make_packet_with_ports<I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
        ) -> Vec<u8> {
            Self::make_serializer_with_ports::<I>(src_ip, dst_ip, src_port, dst_port)
                .serialize_vec_outer()
                .expect("serialize packet")
                .unwrap_b()
                .into_inner()
        }
    }

    struct Udp;

    impl Protocol for Udp {
        type Serializer<'a, I: IpExt> =
            Nested<InnerSerializer<&'a [u8], EmptyBuf>, UdpPacketBuilder<I::Addr>>;

        fn proto<I: IpExt>() -> I::Proto {
            IpProto::Udp.into()
        }

        fn make_serializer_with_ports<'a, I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
        ) -> Self::Serializer<'a, I> {
            [].into_serializer().encapsulate(UdpPacketBuilder::new(
                src_ip,
                dst_ip,
                Some(src_port),
                dst_port,
            ))
        }
    }

    // The `TcpSegmentBuilder` impls are test-only on purpose, and removing this
    // restriction should be thought through.
    //
    // TCP state tracking depends on being able to read TCP options, but
    // TcpSegmentBuilder does not have this information. If a TcpSegmentBuilder
    // passes through filtering with options tracked separately, then these will
    // not be seen by conntrack and could lead to state desynchronization.
    impl<A: IpAddress, Inner: PayloadLen> MaybeTransportPacket for Nested<Inner, TcpSegmentBuilder<A>> {
        fn transport_packet_data(&self) -> Option<TransportPacketData> {
            Some(TransportPacketData::Tcp {
                src_port: TcpSegmentBuilder::src_port(self.outer()).map_or(0, NonZeroU16::get),
                dst_port: TcpSegmentBuilder::dst_port(self.outer()).map_or(0, NonZeroU16::get),
                segment: self.outer().try_into().ok()?,
                payload_len: self.inner().len(),
            })
        }
    }

    impl<I: IpExt, Inner> MaybeTransportPacketMut<I> for Nested<Inner, TcpSegmentBuilder<I::Addr>> {
        type TransportPacketMut<'a>
            = &'a mut Self
        where
            Self: 'a;

        fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
            Some(self)
        }
    }

    impl<I: IpExt, Inner> TransportPacketMut<I> for Nested<Inner, TcpSegmentBuilder<I::Addr>> {
        fn set_src_port(&mut self, port: NonZeroU16) {
            self.outer_mut().set_src_port(port);
        }

        fn set_dst_port(&mut self, port: NonZeroU16) {
            self.outer_mut().set_dst_port(port);
        }

        fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
            self.outer_mut().set_src_ip(new);
        }

        fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
            self.outer_mut().set_dst_ip(new);
        }
    }

    struct Tcp;

    impl Protocol for Tcp {
        type Serializer<'a, I: IpExt> =
            Nested<InnerSerializer<&'a [u8], EmptyBuf>, TcpSegmentBuilder<I::Addr>>;

        fn proto<I: IpExt>() -> I::Proto {
            IpProto::Tcp.into()
        }

        fn make_serializer_with_ports<'a, I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
        ) -> Self::Serializer<'a, I> {
            [1, 2, 3].into_serializer().encapsulate(TcpSegmentBuilder::new(
                src_ip,
                dst_ip,
                src_port,
                dst_port,
                SEQ_NUM,
                ACK_NUM,
                WINDOW_SIZE,
            ))
        }
    }

    struct IcmpEchoRequest;

    impl Protocol for IcmpEchoRequest {
        type Serializer<'a, I: IpExt> = Nested<
            InnerSerializer<&'a [u8], EmptyBuf>,
            IcmpPacketBuilder<I, icmp::IcmpEchoRequest>,
        >;

        fn proto<I: IpExt>() -> I::Proto {
            I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
        }

        fn make_serializer_with_ports<'a, I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            _dst_port: NonZeroU16,
        ) -> Self::Serializer<'a, I> {
            [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
                src_ip,
                dst_ip,
                IcmpUnusedCode,
                icmp::IcmpEchoRequest::new(/* id */ src_port.get(), /* seq */ 0),
            ))
        }
    }

    struct IcmpEchoReply;

    impl Protocol for IcmpEchoReply {
        type Serializer<'a, I: IpExt> =
            Nested<InnerSerializer<&'a [u8], EmptyBuf>, IcmpPacketBuilder<I, icmp::IcmpEchoReply>>;

        fn proto<I: IpExt>() -> I::Proto {
            I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
        }

        fn make_serializer_with_ports<'a, I: IpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            _src_port: NonZeroU16,
            dst_port: NonZeroU16,
        ) -> Self::Serializer<'a, I> {
            [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
                src_ip,
                dst_ip,
                IcmpUnusedCode,
                icmp::IcmpEchoReply::new(/* id */ dst_port.get(), /* seq */ 0),
            ))
        }
    }

    enum TransportPacketDataProtocol {
        Tcp,
        Udp,
        IcmpEchoRequest,
    }

    impl TransportPacketDataProtocol {
        fn make_packet<I: TestIpExt>(&self, src_ip: I::Addr, dst_ip: I::Addr) -> Vec<u8> {
            match self {
                TransportPacketDataProtocol::Tcp => Tcp::make_packet::<I>(src_ip, dst_ip),
                TransportPacketDataProtocol::Udp => Udp::make_packet::<I>(src_ip, dst_ip),
                TransportPacketDataProtocol::IcmpEchoRequest => {
                    IcmpEchoRequest::make_packet::<I>(src_ip, dst_ip)
                }
            }
        }

        fn proto<I: TestIpExt>(&self) -> I::Proto {
            match self {
                TransportPacketDataProtocol::Tcp => Tcp::proto::<I>(),
                TransportPacketDataProtocol::Udp => Udp::proto::<I>(),
                TransportPacketDataProtocol::IcmpEchoRequest => IcmpEchoRequest::proto::<I>(),
            }
        }
    }

    #[ip_test(I)]
    #[test_case(TransportPacketDataProtocol::Udp)]
    #[test_case(TransportPacketDataProtocol::Tcp)]
    #[test_case(TransportPacketDataProtocol::IcmpEchoRequest)]
    fn transport_packet_data_from_serialized<I: TestIpExt>(proto: TransportPacketDataProtocol) {
        let expected_data = match proto {
            TransportPacketDataProtocol::Tcp => TransportPacketData::Tcp {
                src_port: SRC_PORT.get(),
                dst_port: DST_PORT.get(),
                segment: SegmentHeader {
                    seq: SeqNum::new(SEQ_NUM),
                    ack: ACK_NUM.map(SeqNum::new),
                    control: None,
                    wnd: UnscaledWindowSize::from(WINDOW_SIZE),
                    options: Options::default(),
                },
                payload_len: 3,
            },
            TransportPacketDataProtocol::Udp => {
                TransportPacketData::Generic { src_port: SRC_PORT.get(), dst_port: DST_PORT.get() }
            }
            TransportPacketDataProtocol::IcmpEchoRequest => {
                TransportPacketData::Generic { src_port: SRC_PORT.get(), dst_port: 0 }
            }
        };

        let buf = proto.make_packet::<I>(I::SRC_IP, I::DST_IP);
        let parsed_data = TransportPacketData::parse_in_ip_packet::<I, _>(
            I::SRC_IP,
            I::DST_IP,
            proto.proto::<I>(),
            buf.as_slice(),
        )
        .expect("failed to parse transport packet data");

        assert_eq!(parsed_data, expected_data);
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    #[test_case(IcmpEchoRequest)]
    fn update_pseudo_header_address_updates_checksum<I: TestIpExt, P: Protocol>(_proto: P) {
        let mut buf = P::make_packet::<I>(I::SRC_IP, I::DST_IP);
        let view = SliceBufViewMut::new(&mut buf);

        let mut packet = ParsedTransportHeaderMut::<I>::parse_in_ip_packet(
            I::SRC_IP,
            I::DST_IP,
            P::proto::<I>(),
            view,
        )
        .expect("parse transport header");
        packet.update_pseudo_header_src_addr(I::SRC_IP, I::SRC_IP_2);
        packet.update_pseudo_header_dst_addr(I::DST_IP, I::DST_IP_2);
        // Drop the packet because it's holding a mutable borrow of `buf` which
        // we need to assert equality later.
        drop(packet);

        let equivalent = P::make_packet::<I>(I::SRC_IP_2, I::DST_IP_2);

        assert_eq!(equivalent, buf);
    }

    #[ip_test(I)]
    #[test_case(Udp, true, true)]
    #[test_case(Tcp, true, true)]
    #[test_case(IcmpEchoRequest, true, false)]
    #[test_case(IcmpEchoReply, false, true)]
    fn parsed_packet_update_src_dst_port_updates_checksum<I: TestIpExt, P: Protocol>(
        _proto: P,
        update_src_port: bool,
        update_dst_port: bool,
    ) {
        let mut buf = P::make_packet_with_ports::<I>(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT);
        let view = SliceBufViewMut::new(&mut buf);

        let mut packet = ParsedTransportHeaderMut::<I>::parse_in_ip_packet(
            I::SRC_IP,
            I::DST_IP,
            P::proto::<I>(),
            view,
        )
        .expect("parse transport header");
        let expected_src_port = if update_src_port {
            packet.set_src_port(SRC_PORT_2);
            SRC_PORT_2
        } else {
            SRC_PORT
        };
        let expected_dst_port = if update_dst_port {
            packet.set_dst_port(DST_PORT_2);
            DST_PORT_2
        } else {
            DST_PORT
        };
        drop(packet);

        let equivalent = P::make_packet_with_ports::<I>(
            I::SRC_IP,
            I::DST_IP,
            expected_src_port,
            expected_dst_port,
        );

        assert_eq!(equivalent, buf);
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    fn serializer_update_src_dst_port_updates_checksum<I: TestIpExt, P: Protocol>(_proto: P) {
        let mut serializer =
            P::make_serializer_with_ports::<I>(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT);
        let mut packet =
            serializer.transport_packet_mut().expect("packet should support rewriting");
        packet.set_src_port(SRC_PORT_2);
        packet.set_dst_port(DST_PORT_2);
        drop(packet);

        let equivalent =
            P::make_serializer_with_ports::<I>(I::SRC_IP, I::DST_IP, SRC_PORT_2, DST_PORT_2);

        assert_eq!(equivalent, serializer);
    }

    #[ip_test(I)]
    fn icmp_echo_request_update_id_port_updates_checksum<I: TestIpExt>() {
        let mut serializer = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpUnusedCode,
            icmp::IcmpEchoRequest::new(SRC_PORT.get(), /* seq */ 0),
        ));
        serializer
            .transport_packet_mut()
            .expect("packet should support rewriting")
            .set_src_port(SRC_PORT_2);

        let equivalent = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpUnusedCode,
            icmp::IcmpEchoRequest::new(SRC_PORT_2.get(), /* seq */ 0),
        ));

        assert_eq!(equivalent, serializer);
    }

    #[ip_test(I)]
    fn icmp_echo_reply_update_id_port_updates_checksum<I: TestIpExt>() {
        let mut serializer = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpUnusedCode,
            icmp::IcmpEchoReply::new(SRC_PORT.get(), /* seq */ 0),
        ));
        serializer
            .transport_packet_mut()
            .expect("packet should support rewriting")
            .set_dst_port(SRC_PORT_2);

        let equivalent = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpUnusedCode,
            icmp::IcmpEchoReply::new(SRC_PORT_2.get(), /* seq */ 0),
        ));

        assert_eq!(equivalent, serializer);
    }

    #[test]
    #[should_panic]
    fn icmp_serializer_set_port_panics_on_unsupported_type() {
        let mut serializer = [].into_serializer().encapsulate(IcmpPacketBuilder::new(
            Ipv4::SRC_IP,
            Ipv4::DST_IP,
            IcmpUnusedCode,
            icmp::Icmpv4TimestampRequest::new(
                /* origin_timestamp */ 0,
                /* id */ SRC_PORT.get(),
                /* seq */ 0,
            )
            .reply(/* recv_timestamp */ 0, /* tx_timestamp */ 0),
        ));
        let Some(packet) = serializer.transport_packet_mut() else {
            // We expect this method to always return Some, and this test is expected to
            // panic, so *do not* panic in order to fail the test if this method returns
            // None.
            return;
        };
        packet.set_src_port(SRC_PORT_2);
    }

    #[ip_test(I)]
    #[should_panic]
    fn icmp_echo_request_set_dst_port_panics<I: TestIpExt>() {
        let mut serializer = IcmpEchoRequest::make_serializer::<I>(I::SRC_IP, I::DST_IP);
        let Some(packet) = serializer.transport_packet_mut() else {
            // We expect this method to always return Some, and this test is expected to
            // panic, so *do not* panic in order to fail the test if this method returns
            // None.
            return;
        };
        packet.set_dst_port(SRC_PORT_2);
    }

    #[ip_test(I)]
    #[should_panic]
    fn icmp_echo_reply_set_src_port_panics<I: TestIpExt>() {
        let mut serializer = IcmpEchoReply::make_serializer::<I>(I::SRC_IP, I::DST_IP);
        let Some(packet) = serializer.transport_packet_mut() else {
            // We expect this method to always return Some, and this test is expected to
            // panic, so *do not* panic in order to fail the test if this method returns
            // None.
            return;
        };
        packet.set_src_port(SRC_PORT_2);
    }

    fn ip_packet<I: IpExt, P: Protocol>(src: I::Addr, dst: I::Addr) -> Buf<Vec<u8>> {
        Buf::new(P::make_packet::<I>(src, dst), ..)
            .encapsulate(I::PacketBuilder::new(src, dst, /* ttl */ u8::MAX, P::proto::<I>()))
            .serialize_vec_outer()
            .expect("serialize IP packet")
            .unwrap_b()
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    #[test_case(IcmpEchoRequest)]
    fn ip_packet_set_src_dst_addr_updates_checksums<I: TestIpExt, P: Protocol>(_proto: P)
    where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
    {
        let mut buf = ip_packet::<I, P>(I::SRC_IP, I::DST_IP).into_inner();

        let mut packet =
            I::Packet::parse_mut(SliceBufViewMut::new(&mut buf), ()).expect("parse IP packet");
        packet.set_src_addr(I::SRC_IP_2);
        packet.set_dst_addr(I::DST_IP_2);
        drop(packet);

        let equivalent = ip_packet::<I, P>(I::SRC_IP_2, I::DST_IP_2).into_inner();

        assert_eq!(equivalent, buf);
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    #[test_case(IcmpEchoRequest)]
    fn forwarded_packet_set_src_dst_addr_updates_checksums<I: TestIpExt, P: Protocol>(_proto: P) {
        let mut buffer = ip_packet::<I, P>(I::SRC_IP, I::DST_IP);
        let meta = buffer.parse::<I::Packet<_>>().expect("parse IP packet").parse_metadata();
        let mut packet =
            ForwardedPacket::<I, _>::new(I::SRC_IP, I::DST_IP, P::proto::<I>(), meta, buffer);
        packet.set_src_addr(I::SRC_IP_2);
        packet.set_dst_addr(I::DST_IP_2);

        let mut buffer = ip_packet::<I, P>(I::SRC_IP_2, I::DST_IP_2);
        let meta = buffer.parse::<I::Packet<_>>().expect("parse IP packet").parse_metadata();
        let equivalent =
            ForwardedPacket::<I, _>::new(I::SRC_IP_2, I::DST_IP_2, P::proto::<I>(), meta, buffer);

        assert_eq!(equivalent, packet);
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    #[test_case(IcmpEchoRequest)]
    fn tx_packet_set_src_dst_addr_updates_checksums<I: TestIpExt, P: Protocol>(_proto: P) {
        let mut body = P::make_serializer::<I>(I::SRC_IP, I::DST_IP);
        let mut packet = TxPacket::<I, _>::new(I::SRC_IP, I::DST_IP, P::proto::<I>(), &mut body);
        packet.set_src_addr(I::SRC_IP_2);
        packet.set_dst_addr(I::DST_IP_2);

        let mut equivalent_body = P::make_serializer::<I>(I::SRC_IP_2, I::DST_IP_2);
        let equivalent =
            TxPacket::new(I::SRC_IP_2, I::DST_IP_2, P::proto::<I>(), &mut equivalent_body);

        assert_eq!(equivalent, packet);
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    #[test_case(IcmpEchoRequest)]
    fn nested_serializer_set_src_dst_addr_updates_checksums<I: TestIpExt, P: Protocol>(_proto: P) {
        let mut packet = P::make_serializer::<I>(I::SRC_IP, I::DST_IP).encapsulate(
            I::PacketBuilder::new(I::SRC_IP, I::DST_IP, /* ttl */ u8::MAX, P::proto::<I>()),
        );
        packet.set_src_addr(I::SRC_IP_2);
        packet.set_dst_addr(I::DST_IP_2);

        let equivalent =
            P::make_serializer::<I>(I::SRC_IP_2, I::DST_IP_2).encapsulate(I::PacketBuilder::new(
                I::SRC_IP_2,
                I::DST_IP_2,
                /* ttl */ u8::MAX,
                P::proto::<I>(),
            ));

        assert_eq!(equivalent, packet);
    }
}
