// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::borrow::Borrow;
use core::convert::Infallible as Never;
use core::fmt::Debug;
use core::num::NonZeroU16;

use net_types::ip::{
    GenericOverIp, Ip, IpAddress, IpInvariant, IpVersionMarker, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
};
use netstack3_base::{Options, PayloadLen, SegmentHeader};
use packet::records::options::OptionSequenceBuilder;
use packet::{
    Buf, Buffer, BufferAlloc, BufferMut, BufferProvider, BufferViewMut, EitherSerializer, EmptyBuf,
    GrowBufferMut, InnerSerializer, Nested, PacketConstraints, ParsablePacket, ParseBuffer,
    ParseMetadata, ReusableBuffer, SerializeError, Serializer, SliceBufViewMut,
    TruncatingSerializer,
};
use packet_formats::icmp::mld::{
    MulticastListenerDone, MulticastListenerQuery, MulticastListenerQueryV2,
    MulticastListenerReport, MulticastListenerReportV2,
};
use packet_formats::icmp::ndp::options::NdpOptionBuilder;
use packet_formats::icmp::ndp::{
    NeighborAdvertisement, NeighborSolicitation, Redirect, RouterAdvertisement, RouterSolicitation,
};
use packet_formats::icmp::{
    self, IcmpDestUnreachable, IcmpEchoReply, IcmpEchoRequest, IcmpPacketBuilder, IcmpPacketRaw,
    IcmpPacketTypeRaw as _, IcmpTimeExceeded, Icmpv4MessageType, Icmpv4PacketRaw,
    Icmpv4ParameterProblem, Icmpv4Redirect, Icmpv4TimestampReply, Icmpv4TimestampRequest,
    Icmpv6MessageType, Icmpv6PacketRaw, Icmpv6PacketTooBig, Icmpv6ParameterProblem,
};
use packet_formats::igmp::messages::IgmpMembershipReportV3Builder;
use packet_formats::igmp::{self, IgmpPacketBuilder};
use packet_formats::ip::{IpExt, IpPacketBuilder, IpProto, Ipv4Proto, Ipv6Proto};
use packet_formats::ipv4::{Ipv4Header, Ipv4Packet, Ipv4PacketRaw};
use packet_formats::ipv6::{Ipv6Header, Ipv6Packet, Ipv6PacketRaw};
use packet_formats::tcp::options::TcpOption;
use packet_formats::tcp::{TcpSegmentBuilderWithOptions, TcpSegmentRaw};
use packet_formats::udp::{UdpPacketBuilder, UdpPacketRaw};
use zerocopy::{SplitByteSlice, SplitByteSliceMut};

use crate::conntrack;

/// An IP extension trait for the filtering crate.
pub trait FilterIpExt: IpExt {
    /// A marker type to add an [`IpPacket`] bound to [`Self::Packet`].
    type FilterIpPacket<B: SplitByteSliceMut>: IpPacket<Self>;

    /// A marker type to add an [`IpPacket`] bound to
    /// [`Self::PacketRaw`].
    type FilterIpPacketRaw<B: SplitByteSliceMut>: IpPacket<Self>;

    /// A no-op conversion to help the compiler identify that [`Self::Packet`]
    /// actually implements [`IpPacket`].
    fn as_filter_packet<B: SplitByteSliceMut>(
        packet: &mut Self::Packet<B>,
    ) -> &mut Self::FilterIpPacket<B>;

    /// The same as [`FilterIpExt::as_filter_packet`], but for owned values.
    fn as_filter_packet_owned<B: SplitByteSliceMut>(
        packet: Self::Packet<B>,
    ) -> Self::FilterIpPacket<B>;

    /// The same as [`FilterIpExt::as_filter_packet_owned`], but for owned raw
    /// values.
    fn as_filter_packet_raw_owned<B: SplitByteSliceMut>(
        packet: Self::PacketRaw<B>,
    ) -> Self::FilterIpPacketRaw<B>;
}

impl FilterIpExt for Ipv4 {
    type FilterIpPacket<B: SplitByteSliceMut> = Ipv4Packet<B>;
    type FilterIpPacketRaw<B: SplitByteSliceMut> = Ipv4PacketRaw<B>;

    #[inline]
    fn as_filter_packet<B: SplitByteSliceMut>(packet: &mut Ipv4Packet<B>) -> &mut Ipv4Packet<B> {
        packet
    }

    #[inline]
    fn as_filter_packet_owned<B: SplitByteSliceMut>(
        packet: Self::Packet<B>,
    ) -> Self::FilterIpPacket<B> {
        packet
    }

    #[inline]
    fn as_filter_packet_raw_owned<B: SplitByteSliceMut>(
        packet: Self::PacketRaw<B>,
    ) -> Self::FilterIpPacketRaw<B> {
        packet
    }
}

impl FilterIpExt for Ipv6 {
    type FilterIpPacket<B: SplitByteSliceMut> = Ipv6Packet<B>;
    type FilterIpPacketRaw<B: SplitByteSliceMut> = Ipv6PacketRaw<B>;

    #[inline]
    fn as_filter_packet<B: SplitByteSliceMut>(packet: &mut Ipv6Packet<B>) -> &mut Ipv6Packet<B> {
        packet
    }

    #[inline]
    fn as_filter_packet_owned<B: SplitByteSliceMut>(
        packet: Self::Packet<B>,
    ) -> Self::FilterIpPacket<B> {
        packet
    }

    #[inline]
    fn as_filter_packet_raw_owned<B: SplitByteSliceMut>(
        packet: Self::PacketRaw<B>,
    ) -> Self::FilterIpPacketRaw<B> {
        packet
    }
}

/// An IP packet that provides header inspection.
pub trait IpPacket<I: FilterIpExt> {
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

    /// The type that provides access to IP- and transport-layer information
    /// within an ICMP error packet, if this IP packet contains one.
    type IcmpError<'a>: MaybeIcmpErrorPayload<I>
    where
        Self: 'a;

    /// The type that provides mutable access to the message within an ICMP
    /// error packet, if this IP packet contains one.
    type IcmpErrorMut<'a>: MaybeIcmpErrorMut<I>
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
    fn protocol(&self) -> Option<I::Proto>;

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

    /// Returns a type that provides the ability to access the IP- and
    /// transport-layer headers contained within the body of the ICMP error
    /// message, if one exists in this packet.
    ///
    /// NOTE: See the note on [`IpPacket::maybe_transport_packet`].
    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a>;

    /// Returns a type that provides the ability to modify the IP- and
    /// transport-layer headers contained within the body of the ICMP error
    /// message, if one exists in this packet.
    ///
    /// NOTE: See the note on [`IpPacket::transport_packet_mut`].
    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a>;

    /// The header information to be used for connection tracking.
    ///
    /// For the transport header, this currently returns the same information as
    /// [`IpPacket::maybe_transport_packet`], but may be different for packets
    /// such as ICMP errors. In that case, we care about the inner IP packet for
    /// connection tracking, but use the outer header for filtering.
    ///
    /// Subtlety: For ICMP packets, only request/response messages will have
    /// a transport packet defined (and currently only ECHO messages do). This
    /// gets us basic tracking for free, and lets us implicitly ignore ICMP
    /// errors, which are not meant to be tracked.
    ///
    /// If other ICMP message types eventually have TransportPacket impls, then
    /// this would lead to multiple message types being mapped to the same tuple
    /// if they happen to have the same ID.
    fn conntrack_packet(&self) -> Option<conntrack::PacketMetadata<I>> {
        if let Some(payload) = self.maybe_icmp_error().icmp_error_payload() {
            // Checks whether it's reasonable that `payload` is a payload inside
            // an ICMP error with tuple `outer`.
            //
            // An ICMP error can be returned from any router between the sender
            // and the receiver, from the receiver itself, or from the netstack
            // on send (for synthetic errors). Therefore, an originating packet
            // (A -> B) could end up in ICMP errors that look like:
            //
            // B -> A | A -> B
            // R -> A | A -> B
            // A -> A | A -> B
            //
            // where R is some router along the path from A to B.
            //
            // Notice that in both of these cases the destination address is
            // always A. There's no more check we can make, even if we had
            // access to conntrack data for the payload tuple. It's valid for us
            // to compare addresses from the outer packet and the inner payload,
            // even in the presence of NAT, because we can think of the payload
            // and outer tuples as having come from the same "side" of the NAT,
            // so we can pretend that NAT isn't occurring.
            //
            // Even if the tuples are compatible, it's not necessarily the
            // case that conntrack will find a corresponding connection for
            // the packet. That would require the payload tuple to belong to a
            // preexisting conntrack connection.
            (self.dst_addr() == payload.src_ip).then(|| {
                conntrack::PacketMetadata::new_from_icmp_error(
                    payload.src_ip,
                    payload.dst_ip,
                    payload.src_port,
                    payload.dst_port,
                    I::map_ip(payload.proto, |proto| proto.into(), |proto| proto.into()),
                )
            })
        } else {
            self.maybe_transport_packet().transport_packet_data().and_then(|transport_data| {
                Some(conntrack::PacketMetadata::new(
                    self.src_addr(),
                    self.dst_addr(),
                    I::map_ip(self.protocol()?, |proto| proto.into(), |proto| proto.into()),
                    transport_data,
                ))
            })
        }
    }
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

/// A payload of an ICMP error packet that may contain an IP packet.
///
/// See also the note on [`MaybeTransportPacket`].
pub trait MaybeIcmpErrorPayload<I: IpExt> {
    /// Optionally returns a type that provides access to the payload of this
    /// ICMP error.
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>>;
}

/// A payload of an IP packet that may be a valid modifiable ICMP error message
/// (i.e., one that contains the prefix of an IP packet in its payload).
pub trait MaybeIcmpErrorMut<I: FilterIpExt> {
    type IcmpErrorMut<'a>: IcmpErrorMut<I>
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>>;
}

/// A serializer that may also be a valid transport layer packet.
pub trait TransportPacketSerializer<I: FilterIpExt>:
    Serializer
    + MaybeTransportPacket
    + MaybeTransportPacketMut<I>
    + MaybeIcmpErrorPayload<I>
    + MaybeIcmpErrorMut<I>
{
}

impl<I, S> TransportPacketSerializer<I> for S
where
    I: FilterIpExt,
    S: Serializer
        + MaybeTransportPacket
        + MaybeTransportPacketMut<I>
        + MaybeIcmpErrorPayload<I>
        + MaybeIcmpErrorMut<I>,
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

impl<T: ?Sized, I: IpExt> MaybeIcmpErrorPayload<I> for &T
where
    T: MaybeIcmpErrorPayload<I>,
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        (**self).icmp_error_payload()
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

impl<I: FilterIpExt, T: ?Sized> MaybeIcmpErrorMut<I> for &mut T
where
    T: MaybeIcmpErrorMut<I>,
{
    type IcmpErrorMut<'a>
        = T::IcmpErrorMut<'a>
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        (**self).icmp_error_mut()
    }
}

impl<I: FilterIpExt, T: ?Sized> IcmpErrorMut<I> for &mut T
where
    T: IcmpErrorMut<I>,
{
    type InnerPacket<'a>
        = T::InnerPacket<'a>
    where
        Self: 'a;

    fn recalculate_checksum(&mut self) -> bool {
        (**self).recalculate_checksum()
    }

    fn inner_packet<'a>(&'a mut self) -> Option<Self::InnerPacket<'a>> {
        (**self).inner_packet()
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

impl<I: FilterIpExt, T> MaybeIcmpErrorMut<I> for Option<T>
where
    T: IcmpErrorMut<I>,
{
    type IcmpErrorMut<'a>
        = &'a mut T
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
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

/// An ICMP error packet that provides mutable access to the contained IP
/// packet.
pub trait IcmpErrorMut<I: FilterIpExt> {
    type InnerPacket<'a>: IpPacket<I>
    where
        Self: 'a;

    /// Fully recalculate the checksum of this ICMP packet.
    ///
    /// Returns whether the checksum was successfully written.
    ///
    /// Must be called after modifying the IP packet contained within this ICMP
    /// error to ensure the checksum stays correct.
    fn recalculate_checksum(&mut self) -> bool;

    /// Returns an [`IpPacket`] of the packet contained within this error, if
    /// one is present.
    fn inner_packet<'a>(&'a mut self) -> Option<Self::InnerPacket<'a>>;
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
    type IcmpError<'a>
        = &'a Self
    where
        Self: 'a;
    type IcmpErrorMut<'a>
        = Option<ParsedIcmpErrorMut<'a, Ipv4>>
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

        self.set_dst_ip_and_update_checksum(addr);
    }

    fn protocol(&self) -> Option<Ipv4Proto> {
        Some(self.proto())
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        ParsedTransportHeaderMut::parse_in_ipv4_packet(
            self.proto(),
            SliceBufViewMut::new(self.body_mut()),
        )
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
        ParsedIcmpErrorMut::parse_in_ipv4_packet(
            self.src_addr(),
            self.dst_addr(),
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

impl<B: SplitByteSlice> MaybeIcmpErrorPayload<Ipv4> for Ipv4Packet<B> {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<Ipv4>> {
        ParsedIcmpErrorPayload::parse_in_outer_ipv4_packet(self.proto(), Buf::new(self.body(), ..))
    }
}

impl<B: SplitByteSliceMut> IpPacket<Ipv4> for Ipv4PacketRaw<B> {
    type TransportPacket<'a>
        = &'a Self
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Option<ParsedTransportHeaderMut<'a, Ipv4>>
    where
        B: 'a;
    type IcmpError<'a>
        = &'a Self
    where
        Self: 'a;
    type IcmpErrorMut<'a>
        = Option<ParsedIcmpErrorMut<'a, Ipv4>>
    where
        B: 'a;

    fn src_addr(&self) -> Ipv4Addr {
        self.src_ip()
    }

    fn set_src_addr(&mut self, addr: Ipv4Addr) {
        let old = self.src_ip();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }

        self.set_src_ip_and_update_checksum(addr);
    }

    fn dst_addr(&self) -> Ipv4Addr {
        self.dst_ip()
    }

    fn set_dst_addr(&mut self, addr: Ipv4Addr) {
        let old = self.dst_ip();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }

        self.set_dst_ip_and_update_checksum(addr);
    }

    fn protocol(&self) -> Option<Ipv4Proto> {
        Some(self.proto())
    }

    fn maybe_transport_packet<'a>(&'a self) -> Self::TransportPacket<'a> {
        self
    }

    fn transport_packet_mut<'a>(&'a mut self) -> Self::TransportPacketMut<'a> {
        ParsedTransportHeaderMut::parse_in_ipv4_packet(
            self.proto(),
            SliceBufViewMut::new(self.body_mut()),
        )
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
        ParsedIcmpErrorMut::parse_in_ipv4_packet(
            self.src_addr(),
            self.dst_addr(),
            self.proto(),
            SliceBufViewMut::new(self.body_mut()),
        )
    }
}

impl<B: SplitByteSlice> MaybeTransportPacket for Ipv4PacketRaw<B> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        parse_transport_header_in_ipv4_packet(
            self.src_ip(),
            self.dst_ip(),
            self.proto(),
            // We don't particularly care whether we have the full packet, since
            // we're only looking at transport headers.
            self.body().into_inner(),
        )
    }
}

impl<B: SplitByteSlice> MaybeIcmpErrorPayload<Ipv4> for Ipv4PacketRaw<B> {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<Ipv4>> {
        ParsedIcmpErrorPayload::parse_in_outer_ipv4_packet(
            self.proto(),
            // We don't particularly care whether we have the full packet, since
            // we're only looking at transport headers.
            Buf::new(self.body().into_inner(), ..),
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
    type IcmpError<'a>
        = &'a Self
    where
        Self: 'a;
    type IcmpErrorMut<'a>
        = Option<ParsedIcmpErrorMut<'a, Ipv6>>
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

        self.set_dst_ip(addr);
    }

    fn protocol(&self) -> Option<Ipv6Proto> {
        Some(self.proto())
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        ParsedTransportHeaderMut::parse_in_ipv6_packet(
            self.proto(),
            SliceBufViewMut::new(self.body_mut()),
        )
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
        ParsedIcmpErrorMut::parse_in_ipv6_packet(
            self.src_addr(),
            self.dst_addr(),
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

impl<B: SplitByteSlice> MaybeIcmpErrorPayload<Ipv6> for Ipv6Packet<B> {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<Ipv6>> {
        ParsedIcmpErrorPayload::parse_in_outer_ipv6_packet(self.proto(), Buf::new(self.body(), ..))
    }
}

impl<B: SplitByteSliceMut> IpPacket<Ipv6> for Ipv6PacketRaw<B> {
    type TransportPacket<'a>
        = &'a Self
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Option<ParsedTransportHeaderMut<'a, Ipv6>>
    where
        B: 'a;
    type IcmpError<'a>
        = &'a Self
    where
        Self: 'a;
    type IcmpErrorMut<'a>
        = Option<ParsedIcmpErrorMut<'a, Ipv6>>
    where
        B: 'a;

    fn src_addr(&self) -> Ipv6Addr {
        self.src_ip()
    }

    fn set_src_addr(&mut self, addr: Ipv6Addr) {
        let old = self.src_ip();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_src_addr(old, addr);
        }

        self.set_src_ip(addr);
    }

    fn dst_addr(&self) -> Ipv6Addr {
        self.dst_ip()
    }

    fn set_dst_addr(&mut self, addr: Ipv6Addr) {
        let old = self.dst_ip();
        if let Some(packet) = self.transport_packet_mut().transport_packet_mut() {
            packet.update_pseudo_header_dst_addr(old, addr);
        }

        self.set_dst_ip(addr);
    }

    fn protocol(&self) -> Option<Ipv6Proto> {
        self.proto().ok()
    }

    fn maybe_transport_packet<'a>(&'a self) -> Self::TransportPacket<'a> {
        self
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        let proto = self.proto().ok()?;
        let body = self.body_mut()?;
        ParsedTransportHeaderMut::parse_in_ipv6_packet(proto, SliceBufViewMut::new(body))
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
        let src_addr = self.src_addr();
        let dst_addr = self.dst_addr();
        let proto = self.proto().ok()?;
        let body = self.body_mut()?;

        ParsedIcmpErrorMut::parse_in_ipv6_packet(
            src_addr,
            dst_addr,
            proto,
            SliceBufViewMut::new(body),
        )
    }
}

impl<B: SplitByteSlice> MaybeTransportPacket for Ipv6PacketRaw<B> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        let (body, proto) = self.body_proto().ok()?;
        parse_transport_header_in_ipv6_packet(
            self.src_ip(),
            self.dst_ip(),
            proto,
            body.into_inner(),
        )
    }
}

impl<B: SplitByteSlice> MaybeIcmpErrorPayload<Ipv6> for Ipv6PacketRaw<B> {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<Ipv6>> {
        let (body, proto) = self.body_proto().ok()?;
        ParsedIcmpErrorPayload::parse_in_outer_ipv6_packet(proto, Buf::new(body.into_inner(), ..))
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

impl<I: FilterIpExt, S: TransportPacketSerializer<I>> IpPacket<I> for TxPacket<'_, I, S> {
    type TransportPacket<'a>
        = &'a S
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = &'a mut S
    where
        Self: 'a;
    type IcmpError<'a>
        = &'a S
    where
        Self: 'a;
    type IcmpErrorMut<'a>
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

    fn protocol(&self) -> Option<I::Proto> {
        Some(self.protocol)
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self.serializer
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        self.serializer
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self.serializer
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
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

impl<I: FilterIpExt, B: BufferMut> IpPacket<I> for ForwardedPacket<I, B> {
    type TransportPacket<'a>
        = &'a Self
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Option<ParsedTransportHeaderMut<'a, I>>
    where
        Self: 'a;
    type IcmpError<'a>
        = &'a Self
    where
        Self: 'a;

    type IcmpErrorMut<'a>
        = Option<ParsedIcmpErrorMut<'a, I>>
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

        self.dst_addr = addr;
    }

    fn protocol(&self) -> Option<I::Proto> {
        Some(self.protocol)
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        let ForwardedPacket { src_addr: _, dst_addr: _, protocol, buffer, transport_header_offset } =
            self;
        ParsedTransportHeaderMut::<I>::parse_in_ip_packet(
            *protocol,
            SliceBufViewMut::new(&mut buffer.as_mut()[*transport_header_offset..]),
        )
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
        let ForwardedPacket { src_addr, dst_addr, protocol, buffer, transport_header_offset } =
            self;

        ParsedIcmpErrorMut::<I>::parse_in_ip_packet(
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

impl<I: IpExt, B: BufferMut> MaybeIcmpErrorPayload<I> for ForwardedPacket<I, B> {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        let Self { src_addr: _, dst_addr: _, protocol, transport_header_offset, buffer } = self;
        ParsedIcmpErrorPayload::parse_in_outer_ip_packet(
            *protocol,
            Buf::new(&buffer.as_ref()[*transport_header_offset..], ..),
        )
    }
}

impl<I: FilterIpExt, S: TransportPacketSerializer<I>, B: IpPacketBuilder<I>> IpPacket<I>
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
    type IcmpError<'a>
        = &'a S
    where
        Self: 'a;
    type IcmpErrorMut<'a>
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

    fn protocol(&self) -> Option<I::Proto> {
        Some(self.outer().proto())
    }

    fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
        self.inner()
    }

    fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
        self.inner_mut()
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        self.inner()
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
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

impl<I: FilterIpExt> IpPacket<I> for Never {
    type TransportPacket<'a>
        = Never
    where
        Self: 'a;
    type TransportPacketMut<'a>
        = Never
    where
        Self: 'a;
    type IcmpError<'a>
        = Never
    where
        Self: 'a;
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn src_addr(&self) -> I::Addr {
        match *self {}
    }

    fn set_src_addr(&mut self, _addr: I::Addr) {
        match *self {}
    }

    fn dst_addr(&self) -> I::Addr {
        match *self {}
    }

    fn protocol(&self) -> Option<I::Proto> {
        match *self {}
    }

    fn set_dst_addr(&mut self, _addr: I::Addr) {
        match *self {}
    }

    fn maybe_transport_packet<'a>(&'a self) -> Self::TransportPacket<'a> {
        match *self {}
    }

    fn transport_packet_mut<'a>(&'a mut self) -> Self::TransportPacketMut<'a> {
        match *self {}
    }

    fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
        match *self {}
    }

    fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
        match *self {}
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

impl<I: IpExt> MaybeIcmpErrorPayload<I> for Never {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        match *self {}
    }
}

impl<I: FilterIpExt> MaybeIcmpErrorMut<I> for Never {
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        match *self {}
    }
}

impl<I: FilterIpExt> IcmpErrorMut<I> for Never {
    type InnerPacket<'a>
        = Never
    where
        Self: 'a;

    fn inner_packet<'a>(&'a mut self) -> Option<Self::InnerPacket<'a>> {
        match *self {}
    }

    fn recalculate_checksum(&mut self) -> bool {
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

impl<A: IpAddress, I: IpExt, Inner> MaybeIcmpErrorPayload<I>
    for Nested<Inner, UdpPacketBuilder<A>>
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        None
    }
}

impl<A: IpAddress, I: FilterIpExt, Inner> MaybeIcmpErrorMut<I>
    for Nested<Inner, UdpPacketBuilder<A>>
{
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        None
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

impl<A: IpAddress, I: IpExt, Inner, O> MaybeIcmpErrorPayload<I>
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, O>>
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        None
    }
}

impl<A: IpAddress, I: FilterIpExt, Inner, O> MaybeIcmpErrorMut<I>
    for Nested<Inner, TcpSegmentBuilderWithOptions<A, O>>
{
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        None
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
        if M::IS_REWRITABLE {
            let _: u16 = self.message_mut().update_icmp_id(id.get());
        }
    }

    fn set_dst_port(&mut self, id: NonZeroU16) {
        if M::IS_REWRITABLE {
            let _: u16 = self.message_mut().update_icmp_id(id.get());
        }
    }

    fn update_pseudo_header_src_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.set_src_ip(new);
    }

    fn update_pseudo_header_dst_addr(&mut self, _old: I::Addr, new: I::Addr) {
        self.set_dst_ip(new);
    }
}

impl<Inner, I: IpExt> MaybeIcmpErrorPayload<I>
    for Nested<Inner, IcmpPacketBuilder<I, IcmpEchoRequest>>
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        None
    }
}

impl<Inner, I: FilterIpExt> MaybeIcmpErrorMut<I>
    for Nested<Inner, IcmpPacketBuilder<I, IcmpEchoRequest>>
{
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        None
    }
}

impl<Inner, I: IpExt> MaybeIcmpErrorPayload<I>
    for Nested<Inner, IcmpPacketBuilder<I, IcmpEchoReply>>
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        None
    }
}

impl<Inner, I: FilterIpExt> MaybeIcmpErrorMut<I>
    for Nested<Inner, IcmpPacketBuilder<I, IcmpEchoReply>>
{
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        None
    }
}

/// An ICMP message type that may allow for transport-layer packet inspection.
pub trait IcmpMessage<I: IpExt>: icmp::IcmpMessage<I> + MaybeTransportPacket {
    /// Whether this ICMP message supports rewriting the ID.
    const IS_REWRITABLE: bool;

    /// The same as [`IcmpMessage::IS_REWRITABLE`], but for when you have an
    /// object, rather than a type.
    fn is_rewritable(&self) -> bool {
        Self::IS_REWRITABLE
    }

    /// Sets the ICMP ID for the message, returning the previous value.
    ///
    /// The ICMP ID is both the *src* AND *dst* ports for conntrack entries.
    fn update_icmp_id(&mut self, id: u16) -> u16;
}

// TODO(https://fxbug.dev/341128580): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl MaybeTransportPacket for IcmpEchoReply {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        Some(TransportPacketData::Generic { src_port: self.id(), dst_port: self.id() })
    }
}

impl<I: IpExt> IcmpMessage<I> for IcmpEchoReply {
    const IS_REWRITABLE: bool = true;

    fn update_icmp_id(&mut self, id: u16) -> u16 {
        let old = self.id();
        self.set_id(id);
        old
    }
}

// TODO(https://fxbug.dev/341128580): connection tracking will probably want to
// special case ICMP echo packets to ensure that a new connection is only ever
// created from an echo request, and not an echo response. We need to provide a
// way for conntrack to differentiate between the two.
impl MaybeTransportPacket for IcmpEchoRequest {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        Some(TransportPacketData::Generic { src_port: self.id(), dst_port: self.id() })
    }
}

impl<I: IpExt> IcmpMessage<I> for IcmpEchoRequest {
    const IS_REWRITABLE: bool = true;

    fn update_icmp_id(&mut self, id: u16) -> u16 {
        let old = self.id();
        self.set_id(id);
        old
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
                const IS_REWRITABLE: bool = false;

                fn update_icmp_id(&mut self, _: u16) -> u16 {
                    unreachable!("non-echo ICMP packets should never be rewritten")
                }
            }
        )+
    };
}

unsupported_icmp_message_type!(Icmpv4TimestampRequest, Ipv4);
unsupported_icmp_message_type!(Icmpv4TimestampReply, Ipv4);
unsupported_icmp_message_type!(NeighborSolicitation, Ipv6);
unsupported_icmp_message_type!(NeighborAdvertisement, Ipv6);
unsupported_icmp_message_type!(RouterSolicitation, Ipv6);
unsupported_icmp_message_type!(MulticastListenerDone, Ipv6);
unsupported_icmp_message_type!(MulticastListenerReport, Ipv6);
unsupported_icmp_message_type!(MulticastListenerReportV2, Ipv6);
unsupported_icmp_message_type!(MulticastListenerQuery, Ipv6);
unsupported_icmp_message_type!(MulticastListenerQueryV2, Ipv6);
unsupported_icmp_message_type!(RouterAdvertisement, Ipv6);
// This isn't considered an error because, unlike ICMPv4, an ICMPv6 Redirect
// message doesn't contain an IP packet payload (RFC 2461 Section 4.5).
unsupported_icmp_message_type!(Redirect, Ipv6);

/// Implement For ICMP message that aren't errors.
macro_rules! non_error_icmp_message_type {
    ($message:ty, $ip:ty) => {
        impl<Inner> MaybeIcmpErrorPayload<$ip> for Nested<Inner, IcmpPacketBuilder<$ip, $message>> {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<$ip>> {
                None
            }
        }

        impl<Inner> MaybeIcmpErrorMut<$ip> for Nested<Inner, IcmpPacketBuilder<$ip, $message>> {
            type IcmpErrorMut<'a>
                = Never
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                None
            }
        }
    };
}

non_error_icmp_message_type!(Icmpv4TimestampRequest, Ipv4);
non_error_icmp_message_type!(Icmpv4TimestampReply, Ipv4);
non_error_icmp_message_type!(RouterSolicitation, Ipv6);
non_error_icmp_message_type!(RouterAdvertisement, Ipv6);
non_error_icmp_message_type!(NeighborSolicitation, Ipv6);
non_error_icmp_message_type!(NeighborAdvertisement, Ipv6);
non_error_icmp_message_type!(MulticastListenerReport, Ipv6);
non_error_icmp_message_type!(MulticastListenerDone, Ipv6);
non_error_icmp_message_type!(MulticastListenerReportV2, Ipv6);

macro_rules! icmp_error_message {
    ($message:ty, $($ips:ty),+) => {
        impl MaybeTransportPacket for $message {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                None
            }
        }

        $(
            impl IcmpMessage<$ips> for $message {
                const IS_REWRITABLE: bool = false;

                fn update_icmp_id(&mut self, _: u16) -> u16 {
                    unreachable!("non-echo ICMP packets should never be rewritten")
                }
            }
        )+
    };
}

icmp_error_message!(IcmpDestUnreachable, Ipv4, Ipv6);
icmp_error_message!(IcmpTimeExceeded, Ipv4, Ipv6);
icmp_error_message!(Icmpv4ParameterProblem, Ipv4);
icmp_error_message!(Icmpv4Redirect, Ipv4);
icmp_error_message!(Icmpv6ParameterProblem, Ipv6);
icmp_error_message!(Icmpv6PacketTooBig, Ipv6);

macro_rules! icmpv4_error_message {
    ($message: ty) => {
        impl<Inner: AsRef<[u8]>> MaybeIcmpErrorPayload<Ipv4>
            for Nested<Inner, IcmpPacketBuilder<Ipv4, $message>>
        {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<Ipv4>> {
                ParsedIcmpErrorPayload::parse_in_icmpv4_error(Buf::new(self.inner(), ..))
            }
        }

        impl<Inner: BufferMut> MaybeIcmpErrorMut<Ipv4>
            for Nested<Inner, IcmpPacketBuilder<Ipv4, $message>>
        {
            type IcmpErrorMut<'a>
                = &'a mut Self
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                Some(self)
            }
        }

        impl<Inner: BufferMut> IcmpErrorMut<Ipv4>
            for Nested<Inner, IcmpPacketBuilder<Ipv4, $message>>
        {
            type InnerPacket<'a>
                = Ipv4PacketRaw<&'a mut [u8]>
            where
                Self: 'a;

            fn recalculate_checksum(&mut self) -> bool {
                // Checksum is calculated during serialization.
                true
            }

            fn inner_packet<'a>(&'a mut self) -> Option<Self::InnerPacket<'a>> {
                let packet =
                    Ipv4PacketRaw::parse_mut(SliceBufViewMut::new(self.inner_mut().as_mut()), ())
                        .ok()?;

                Some(packet)
            }
        }
    };
}

icmpv4_error_message!(IcmpDestUnreachable);
icmpv4_error_message!(Icmpv4Redirect);
icmpv4_error_message!(IcmpTimeExceeded);
icmpv4_error_message!(Icmpv4ParameterProblem);

macro_rules! icmpv6_error_message {
    ($message: ty) => {
        impl<Inner: Buffer> MaybeIcmpErrorPayload<Ipv6>
            for Nested<TruncatingSerializer<Inner>, IcmpPacketBuilder<Ipv6, $message>>
        {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<Ipv6>> {
                ParsedIcmpErrorPayload::parse_in_icmpv6_error(Buf::new(self.inner().buffer(), ..))
            }
        }

        impl<Inner: BufferMut> MaybeIcmpErrorMut<Ipv6>
            for Nested<TruncatingSerializer<Inner>, IcmpPacketBuilder<Ipv6, $message>>
        {
            type IcmpErrorMut<'a>
                = &'a mut Self
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                Some(self)
            }
        }

        impl<Inner: BufferMut> IcmpErrorMut<Ipv6>
            for Nested<TruncatingSerializer<Inner>, IcmpPacketBuilder<Ipv6, $message>>
        {
            type InnerPacket<'a>
                = Ipv6PacketRaw<&'a mut [u8]>
            where
                Self: 'a;

            fn recalculate_checksum(&mut self) -> bool {
                // Checksum is calculated during serialization.
                true
            }

            fn inner_packet<'a>(&'a mut self) -> Option<Self::InnerPacket<'a>> {
                let packet = Ipv6PacketRaw::parse_mut(
                    SliceBufViewMut::new(self.inner_mut().buffer_mut().as_mut()),
                    (),
                )
                .ok()?;

                Some(packet)
            }
        }
    };
}

icmpv6_error_message!(IcmpDestUnreachable);
icmpv6_error_message!(Icmpv6PacketTooBig);
icmpv6_error_message!(IcmpTimeExceeded);
icmpv6_error_message!(Icmpv6ParameterProblem);

impl<I: FilterIpExt, M: igmp::MessageType<EmptyBuf>> MaybeIcmpErrorMut<I>
    for InnerSerializer<IgmpPacketBuilder<EmptyBuf, M>, EmptyBuf>
{
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        None
    }
}

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

impl<I: IpExt, M: igmp::MessageType<EmptyBuf>> MaybeIcmpErrorPayload<I>
    for InnerSerializer<IgmpPacketBuilder<EmptyBuf, M>, EmptyBuf>
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        None
    }
}

impl<I> MaybeTransportPacket for InnerSerializer<IgmpMembershipReportV3Builder<I>, EmptyBuf> {
    fn transport_packet_data(&self) -> Option<TransportPacketData> {
        None
    }
}

impl<I> MaybeTransportPacketMut<Ipv4>
    for InnerSerializer<IgmpMembershipReportV3Builder<I>, EmptyBuf>
{
    type TransportPacketMut<'a>
        = Never
    where
        I: 'a;

    fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
        None
    }
}

impl<I: IpExt, II, B> MaybeIcmpErrorPayload<I>
    for InnerSerializer<IgmpMembershipReportV3Builder<II>, B>
{
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        None
    }
}

impl<I: FilterIpExt, II, B> MaybeIcmpErrorMut<I>
    for InnerSerializer<IgmpMembershipReportV3Builder<II>, B>
{
    type IcmpErrorMut<'a>
        = Never
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
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
        let RawIpBody { protocol, src_addr: _, dst_addr: _, body, transport_packet_data: _ } = self;
        ParsedTransportHeaderMut::<I>::parse_in_ip_packet(
            *protocol,
            SliceBufViewMut::new(body.as_mut()),
        )
    }
}

impl<I: IpExt, B: ParseBuffer> MaybeIcmpErrorPayload<I> for RawIpBody<I, B> {
    fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
        ParsedIcmpErrorPayload::parse_in_outer_ip_packet(self.protocol, Buf::new(&self.body, ..))
    }
}

impl<I: FilterIpExt, B: BufferMut> MaybeIcmpErrorMut<I> for RawIpBody<I, B> {
    type IcmpErrorMut<'a>
        = ParsedIcmpErrorMut<'a, I>
    where
        Self: 'a;

    fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
        let RawIpBody { protocol, src_addr, dst_addr, body, transport_packet_data: _ } = self;

        ParsedIcmpErrorMut::parse_in_ip_packet(
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
    let options = Options::from_iter(builder.syn_set(), options.iter());

    let segment = SegmentHeader::from_builder_options(&builder, options).ok()?;

    Some(TransportPacketData::Tcp {
        src_port: builder.src_port().map(NonZeroU16::get).unwrap_or(0),
        dst_port: builder.dst_port().map(NonZeroU16::get).unwrap_or(0),
        segment,
        payload_len: body.len(),
    })
}

fn parse_icmpv4_header<B: ParseBuffer>(mut body: B) -> Option<TransportPacketData> {
    match icmp::peek_message_type(body.as_ref()).ok()? {
        Icmpv4MessageType::EchoRequest => {
            let packet = body.parse::<IcmpPacketRaw<Ipv4, _, IcmpEchoRequest>>().ok()?;
            packet.message().transport_packet_data()
        }
        Icmpv4MessageType::EchoReply => {
            let packet = body.parse::<IcmpPacketRaw<Ipv4, _, IcmpEchoReply>>().ok()?;
            packet.message().transport_packet_data()
        }
        // ICMP errors have a separate parsing path.
        Icmpv4MessageType::DestUnreachable
        | Icmpv4MessageType::Redirect
        | Icmpv4MessageType::TimeExceeded
        | Icmpv4MessageType::ParameterProblem => None,
        // NOTE: If these are parsed, then without further work, conntrack won't
        // be able to differentiate between these and ECHO message with the same
        // ID.
        Icmpv4MessageType::TimestampRequest | Icmpv4MessageType::TimestampReply => None,
    }
}

fn parse_icmpv6_header<B: ParseBuffer>(mut body: B) -> Option<TransportPacketData> {
    match icmp::peek_message_type(body.as_ref()).ok()? {
        Icmpv6MessageType::EchoRequest => {
            let packet = body.parse::<IcmpPacketRaw<Ipv6, _, IcmpEchoRequest>>().ok()?;
            packet.message().transport_packet_data()
        }
        Icmpv6MessageType::EchoReply => {
            let packet = body.parse::<IcmpPacketRaw<Ipv6, _, IcmpEchoReply>>().ok()?;
            packet.message().transport_packet_data()
        }
        // ICMP errors have a separate parsing path.
        Icmpv6MessageType::DestUnreachable
        | Icmpv6MessageType::PacketTooBig
        | Icmpv6MessageType::TimeExceeded
        | Icmpv6MessageType::ParameterProblem => None,
        Icmpv6MessageType::RouterSolicitation
        | Icmpv6MessageType::RouterAdvertisement
        | Icmpv6MessageType::NeighborSolicitation
        | Icmpv6MessageType::NeighborAdvertisement
        | Icmpv6MessageType::Redirect
        | Icmpv6MessageType::MulticastListenerQuery
        | Icmpv6MessageType::MulticastListenerReport
        | Icmpv6MessageType::MulticastListenerDone
        | Icmpv6MessageType::MulticastListenerReportV2 => None,
    }
}

/// A transport header that has been parsed from a byte buffer and provides
/// mutable access to its contents.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub enum ParsedTransportHeaderMut<'a, I: IpExt> {
    Tcp(TcpSegmentRaw<&'a mut [u8]>),
    Udp(UdpPacketRaw<&'a mut [u8]>),
    Icmp(I::IcmpPacketTypeRaw<&'a mut [u8]>),
}

impl<'a> ParsedTransportHeaderMut<'a, Ipv4> {
    fn parse_in_ipv4_packet<BV: BufferViewMut<&'a mut [u8]>>(
        proto: Ipv4Proto,
        body: BV,
    ) -> Option<Self> {
        match proto {
            Ipv4Proto::Proto(IpProto::Udp) => {
                Some(Self::Udp(UdpPacketRaw::parse_mut(body, IpVersionMarker::<Ipv4>::new()).ok()?))
            }
            Ipv4Proto::Proto(IpProto::Tcp) => {
                Some(Self::Tcp(TcpSegmentRaw::parse_mut(body, ()).ok()?))
            }
            Ipv4Proto::Icmp => Some(Self::Icmp(Icmpv4PacketRaw::parse_mut(body, ()).ok()?)),
            Ipv4Proto::Proto(IpProto::Reserved) | Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
        }
    }
}

impl<'a> ParsedTransportHeaderMut<'a, Ipv6> {
    fn parse_in_ipv6_packet<BV: BufferViewMut<&'a mut [u8]>>(
        proto: Ipv6Proto,
        body: BV,
    ) -> Option<Self> {
        match proto {
            Ipv6Proto::Proto(IpProto::Udp) => {
                Some(Self::Udp(UdpPacketRaw::parse_mut(body, IpVersionMarker::<Ipv6>::new()).ok()?))
            }
            Ipv6Proto::Proto(IpProto::Tcp) => {
                Some(Self::Tcp(TcpSegmentRaw::parse_mut(body, ()).ok()?))
            }
            Ipv6Proto::Icmpv6 => Some(Self::Icmp(Icmpv6PacketRaw::parse_mut(body, ()).ok()?)),
            Ipv6Proto::Proto(IpProto::Reserved) | Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => {
                None
            }
        }
    }
}

impl<'a, I: IpExt> ParsedTransportHeaderMut<'a, I> {
    fn parse_in_ip_packet<BV: BufferViewMut<&'a mut [u8]>>(
        proto: I::Proto,
        body: BV,
    ) -> Option<Self> {
        I::map_ip(
            (proto, IpInvariant(body)),
            |(proto, IpInvariant(body))| {
                ParsedTransportHeaderMut::<'a, Ipv4>::parse_in_ipv4_packet(proto, body)
            },
            |(proto, IpInvariant(body))| {
                ParsedTransportHeaderMut::<'a, Ipv6>::parse_in_ipv6_packet(proto, body)
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

/// An inner IP packet contained within an ICMP error.
#[derive(Debug, PartialEq, Eq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct ParsedIcmpErrorPayload<I: IpExt> {
    src_ip: I::Addr,
    dst_ip: I::Addr,
    // Hold the ports directly instead of TransportPacketData. In case of an
    // ICMP error, we don't update conntrack connection state, so there's no
    // reason to keep the extra information.
    src_port: u16,
    dst_port: u16,
    proto: I::Proto,
}

impl ParsedIcmpErrorPayload<Ipv4> {
    fn parse_in_outer_ipv4_packet<B>(protocol: Ipv4Proto, mut body: B) -> Option<Self>
    where
        B: ParseBuffer,
    {
        match protocol {
            Ipv4Proto::Proto(_) | Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
            Ipv4Proto::Icmp => {
                let message = body.parse::<Icmpv4PacketRaw<_>>().ok()?;
                let message_body = match &message {
                    Icmpv4PacketRaw::EchoRequest(_)
                    | Icmpv4PacketRaw::EchoReply(_)
                    | Icmpv4PacketRaw::TimestampRequest(_)
                    | Icmpv4PacketRaw::TimestampReply(_) => return None,

                    Icmpv4PacketRaw::DestUnreachable(inner) => inner.message_body(),
                    Icmpv4PacketRaw::Redirect(inner) => inner.message_body(),
                    Icmpv4PacketRaw::TimeExceeded(inner) => inner.message_body(),
                    Icmpv4PacketRaw::ParameterProblem(inner) => inner.message_body(),
                };

                Self::parse_in_icmpv4_error(Buf::new(message_body, ..))
            }
        }
    }

    fn parse_in_icmpv4_error<B>(mut body: B) -> Option<Self>
    where
        B: ParseBuffer,
    {
        let packet = body.parse::<Ipv4PacketRaw<_>>().ok()?;

        let src_ip = packet.get_header_prefix().src_ip();
        let dst_ip = packet.get_header_prefix().dst_ip();
        let proto = packet.proto();
        let transport_data = parse_transport_header_in_ipv4_packet(
            src_ip,
            dst_ip,
            proto,
            packet.body().into_inner(),
        )?;
        Some(Self {
            src_ip,
            dst_ip,
            src_port: transport_data.src_port(),
            dst_port: transport_data.dst_port(),
            proto,
        })
    }
}

impl ParsedIcmpErrorPayload<Ipv6> {
    fn parse_in_outer_ipv6_packet<B>(protocol: Ipv6Proto, mut body: B) -> Option<Self>
    where
        B: ParseBuffer,
    {
        match protocol {
            Ipv6Proto::NoNextHeader | Ipv6Proto::Proto(_) | Ipv6Proto::Other(_) => None,

            Ipv6Proto::Icmpv6 => {
                let message = body.parse::<Icmpv6PacketRaw<_>>().ok()?;
                let message_body = match &message {
                    Icmpv6PacketRaw::EchoRequest(_)
                    | Icmpv6PacketRaw::EchoReply(_)
                    | Icmpv6PacketRaw::Ndp(_)
                    | Icmpv6PacketRaw::Mld(_) => return None,

                    Icmpv6PacketRaw::DestUnreachable(inner) => inner.message_body(),
                    Icmpv6PacketRaw::PacketTooBig(inner) => inner.message_body(),
                    Icmpv6PacketRaw::TimeExceeded(inner) => inner.message_body(),
                    Icmpv6PacketRaw::ParameterProblem(inner) => inner.message_body(),
                };

                Self::parse_in_icmpv6_error(Buf::new(message_body, ..))
            }
        }
    }

    fn parse_in_icmpv6_error<B>(mut body: B) -> Option<Self>
    where
        B: ParseBuffer,
    {
        let packet = body.parse::<Ipv6PacketRaw<_>>().ok()?;

        let src_ip = packet.get_fixed_header().src_ip();
        let dst_ip = packet.get_fixed_header().dst_ip();
        let proto = packet.proto().ok()?;
        let transport_data = parse_transport_header_in_ipv6_packet(
            src_ip,
            dst_ip,
            proto,
            packet.body().ok()?.into_inner(),
        )?;
        Some(Self {
            src_ip,
            dst_ip,
            src_port: transport_data.src_port(),
            dst_port: transport_data.dst_port(),
            proto,
        })
    }
}

impl<I: IpExt> ParsedIcmpErrorPayload<I> {
    fn parse_in_outer_ip_packet<B>(proto: I::Proto, body: B) -> Option<Self>
    where
        B: ParseBuffer,
    {
        I::map_ip(
            (proto, IpInvariant(body)),
            |(proto, IpInvariant(body))| {
                ParsedIcmpErrorPayload::<Ipv4>::parse_in_outer_ipv4_packet(proto, body)
            },
            |(proto, IpInvariant(body))| {
                ParsedIcmpErrorPayload::<Ipv6>::parse_in_outer_ipv6_packet(proto, body)
            },
        )
    }
}

/// An ICMP error packet that provides mutable access to the contained IP
/// packet.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct ParsedIcmpErrorMut<'a, I: IpExt> {
    src_ip: I::Addr,
    dst_ip: I::Addr,
    message: I::IcmpPacketTypeRaw<&'a mut [u8]>,
}

impl<'a> ParsedIcmpErrorMut<'a, Ipv4> {
    fn parse_in_ipv4_packet<BV: BufferViewMut<&'a mut [u8]>>(
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        proto: Ipv4Proto,
        body: BV,
    ) -> Option<Self> {
        match proto {
            Ipv4Proto::Proto(_) | Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
            Ipv4Proto::Icmp => {
                let message = Icmpv4PacketRaw::parse_mut(body, ()).ok()?;
                match message {
                    Icmpv4PacketRaw::EchoRequest(_)
                    | Icmpv4PacketRaw::EchoReply(_)
                    | Icmpv4PacketRaw::TimestampRequest(_)
                    | Icmpv4PacketRaw::TimestampReply(_) => None,

                    Icmpv4PacketRaw::DestUnreachable(_)
                    | Icmpv4PacketRaw::Redirect(_)
                    | Icmpv4PacketRaw::TimeExceeded(_)
                    | Icmpv4PacketRaw::ParameterProblem(_) => {
                        Some(Self { src_ip, dst_ip, message })
                    }
                }
            }
        }
    }
}

impl<'a> ParsedIcmpErrorMut<'a, Ipv6> {
    fn parse_in_ipv6_packet<BV: BufferViewMut<&'a mut [u8]>>(
        src_ip: Ipv6Addr,
        dst_ip: Ipv6Addr,
        proto: Ipv6Proto,
        body: BV,
    ) -> Option<Self> {
        match proto {
            Ipv6Proto::NoNextHeader | Ipv6Proto::Proto(_) | Ipv6Proto::Other(_) => None,

            Ipv6Proto::Icmpv6 => {
                let message = Icmpv6PacketRaw::parse_mut(body, ()).ok()?;
                match message {
                    Icmpv6PacketRaw::EchoRequest(_)
                    | Icmpv6PacketRaw::EchoReply(_)
                    | Icmpv6PacketRaw::Ndp(_)
                    | Icmpv6PacketRaw::Mld(_) => None,

                    Icmpv6PacketRaw::DestUnreachable(_)
                    | Icmpv6PacketRaw::PacketTooBig(_)
                    | Icmpv6PacketRaw::TimeExceeded(_)
                    | Icmpv6PacketRaw::ParameterProblem(_) => {
                        Some(Self { src_ip, dst_ip, message })
                    }
                }
            }
        }
    }
}

impl<'a, I: FilterIpExt> ParsedIcmpErrorMut<'a, I> {
    fn parse_in_ip_packet<BV: BufferViewMut<&'a mut [u8]>>(
        src_ip: I::Addr,
        dst_ip: I::Addr,
        proto: I::Proto,
        body: BV,
    ) -> Option<Self> {
        I::map_ip(
            (src_ip, dst_ip, proto, IpInvariant(body)),
            |(src_ip, dst_ip, proto, IpInvariant(body))| {
                ParsedIcmpErrorMut::<'a, Ipv4>::parse_in_ipv4_packet(src_ip, dst_ip, proto, body)
            },
            |(src_ip, dst_ip, proto, IpInvariant(body))| {
                ParsedIcmpErrorMut::<'a, Ipv6>::parse_in_ipv6_packet(src_ip, dst_ip, proto, body)
            },
        )
    }
}

impl<'a, I: FilterIpExt> IcmpErrorMut<I> for ParsedIcmpErrorMut<'a, I> {
    type InnerPacket<'b>
        = I::FilterIpPacketRaw<&'b mut [u8]>
    where
        Self: 'b;

    fn inner_packet<'b>(&'b mut self) -> Option<Self::InnerPacket<'b>> {
        Some(I::as_filter_packet_raw_owned(
            I::PacketRaw::parse_mut(SliceBufViewMut::new(self.message.message_body_mut()), ())
                .ok()?,
        ))
    }

    fn recalculate_checksum(&mut self) -> bool {
        let Self { src_ip, dst_ip, message } = self;
        message.try_write_checksum(*src_ip, *dst_ip)
    }
}

/// A helper trait to extract [`IcmpMessage`] impls from parsed ICMP messages.
trait IcmpMessageImplHelper<I: IpExt> {
    fn message_impl_mut(&mut self) -> &mut impl IcmpMessage<I>;
}

impl<I: IpExt, B: SplitByteSliceMut, M: IcmpMessage<I>> IcmpMessageImplHelper<I>
    for IcmpPacketRaw<I, B, M>
{
    fn message_impl_mut(&mut self) -> &mut impl IcmpMessage<I> {
        self.message_mut()
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
                        packet_formats::icmpv4_dispatch!(
                            packet: raw,
                            p => {
                                let message = p.message_impl_mut();
                                if  message.is_rewritable() {
                                    let old = message.update_icmp_id(port.get());
                                    p.update_checksum_header_field_u16(old, port.get())
                                }
                            }
                        );
                    },
                    |packet| {
                        packet_formats::icmpv6_dispatch!(
                            packet: raw,
                            p => {
                                let message = p.message_impl_mut();
                                if  message.is_rewritable() {
                                    let old = message.update_icmp_id(port.get());
                                    p.update_checksum_header_field_u16(old, port.get())
                                }
                            }
                        );
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
                        packet_formats::icmpv4_dispatch!(
                            packet:raw,
                            p => {
                                let message = p.message_impl_mut();
                                if  message.is_rewritable() {
                                    let old = message.update_icmp_id(port.get());
                                    p.update_checksum_header_field_u16(old, port.get())
                                }
                            }
                        );
                    },
                    |packet| {
                        packet_formats::icmpv6_dispatch!(
                            packet:raw,
                            p => {
                                let message = p.message_impl_mut();
                                if  message.is_rewritable() {
                                    let old = message.update_icmp_id(port.get());
                                    p.update_checksum_header_field_u16(old, port.get())
                                }
                            }
                        );
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

    impl<I: IpExt, B: BufferMut> MaybeIcmpErrorPayload<I> for Nested<B, ()> {
        fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
            unimplemented!()
        }
    }

    impl<I: FilterIpExt, B: BufferMut> MaybeIcmpErrorMut<I> for Nested<B, ()> {
        type IcmpErrorMut<'a>
            = Never
        where
            Self: 'a;

        fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
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

    impl<I: IpExt> MaybeIcmpErrorPayload<I> for InnerSerializer<&[u8], EmptyBuf> {
        fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
            None
        }
    }

    impl<I: FilterIpExt> MaybeIcmpErrorMut<I> for InnerSerializer<&[u8], EmptyBuf> {
        type IcmpErrorMut<'a>
            = Never
        where
            Self: 'a;

        fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
            None
        }
    }

    #[cfg(test)]
    pub(crate) mod internal {
        use alloc::vec::Vec;
        use net_declare::{net_ip_v4, net_ip_v6, net_subnet_v4, net_subnet_v6};
        use net_types::ip::Subnet;
        use netstack3_base::{SeqNum, UnscaledWindowSize};
        use packet::TruncateDirection;
        use packet_formats::icmp::{Icmpv4DestUnreachableCode, Icmpv6DestUnreachableCode};

        use super::*;

        pub trait TestIpExt: FilterIpExt {
            const SRC_IP: Self::Addr;
            const SRC_PORT: u16 = 1234;
            const DST_IP: Self::Addr;
            const DST_PORT: u16 = 9876;
            const SRC_IP_2: Self::Addr;
            const DST_IP_2: Self::Addr;
            const SRC_IP_3: Self::Addr;
            const DST_IP_3: Self::Addr;
            const IP_OUTSIDE_SUBNET: Self::Addr;
            const SUBNET: Subnet<Self::Addr>;
        }

        impl TestIpExt for Ipv4 {
            const SRC_IP: Self::Addr = net_ip_v4!("192.0.2.1");
            const DST_IP: Self::Addr = net_ip_v4!("192.0.2.2");
            const SRC_IP_2: Self::Addr = net_ip_v4!("192.0.2.3");
            const DST_IP_2: Self::Addr = net_ip_v4!("192.0.2.4");
            const SRC_IP_3: Self::Addr = net_ip_v4!("192.0.2.5");
            const DST_IP_3: Self::Addr = net_ip_v4!("192.0.2.6");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v4!("192.0.3.1");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v4!("192.0.2.0/24");
        }

        impl TestIpExt for Ipv6 {
            const SRC_IP: Self::Addr = net_ip_v6!("2001:db8::1");
            const DST_IP: Self::Addr = net_ip_v6!("2001:db8::2");
            const SRC_IP_2: Self::Addr = net_ip_v6!("2001:db8::3");
            const DST_IP_2: Self::Addr = net_ip_v6!("2001:db8::4");
            const SRC_IP_3: Self::Addr = net_ip_v6!("2001:db8::5");
            const DST_IP_3: Self::Addr = net_ip_v6!("2001:db8::6");
            const IP_OUTSIDE_SUBNET: Self::Addr = net_ip_v6!("2001:db8:ffff::1");
            const SUBNET: Subnet<Self::Addr> = net_subnet_v6!("2001:db8::/64");
        }

        #[derive(Clone, Debug, PartialEq)]
        pub struct FakeIpPacket<I: FilterIpExt, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
        {
            pub src_ip: I::Addr,
            pub dst_ip: I::Addr,
            pub body: T,
        }

        impl<I: FilterIpExt> FakeIpPacket<I, FakeUdpPacket> {
            pub(crate) fn reply(&self) -> Self {
                Self { src_ip: self.dst_ip, dst_ip: self.src_ip, body: self.body.reply() }
            }
        }

        pub trait TransportPacketExt<I: IpExt>:
            MaybeTransportPacket + MaybeIcmpErrorPayload<I>
        {
            fn proto() -> Option<I::Proto>;
        }

        impl<I: FilterIpExt, T> IpPacket<I> for FakeIpPacket<I, T>
        where
            for<'a> &'a T: TransportPacketExt<I>,
            for<'a> &'a mut T: MaybeTransportPacketMut<I> + MaybeIcmpErrorMut<I>,
        {
            type TransportPacket<'a>
                = &'a T
            where
                T: 'a;
            type TransportPacketMut<'a>
                = &'a mut T
            where
                T: 'a;
            type IcmpError<'a>
                = &'a T
            where
                T: 'a;
            type IcmpErrorMut<'a>
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

            fn protocol(&self) -> Option<I::Proto> {
                <&T>::proto()
            }

            fn maybe_transport_packet(&self) -> Self::TransportPacket<'_> {
                &self.body
            }

            fn transport_packet_mut(&mut self) -> Self::TransportPacketMut<'_> {
                &mut self.body
            }

            fn maybe_icmp_error<'a>(&'a self) -> Self::IcmpError<'a> {
                &self.body
            }

            fn icmp_error_mut<'a>(&'a mut self) -> Self::IcmpErrorMut<'a> {
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

        impl<I: FilterIpExt> TransportPacketExt<I> for &FakeTcpSegment {
            fn proto() -> Option<I::Proto> {
                Some(I::map_ip_out(
                    (),
                    |()| Ipv4Proto::Proto(IpProto::Tcp),
                    |()| Ipv6Proto::Proto(IpProto::Tcp),
                ))
            }
        }

        impl MaybeTransportPacket for &FakeTcpSegment {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                Some(TransportPacketData::Tcp {
                    src_port: self.src_port,
                    dst_port: self.dst_port,
                    segment: self.segment.clone(),
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

        impl<I: IpExt> MaybeIcmpErrorPayload<I> for FakeTcpSegment {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
                None
            }
        }

        impl<I: FilterIpExt> MaybeIcmpErrorMut<I> for FakeTcpSegment {
            type IcmpErrorMut<'a>
                = Never
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                None
            }
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

        impl<I: FilterIpExt> TransportPacketExt<I> for &FakeUdpPacket {
            fn proto() -> Option<I::Proto> {
                Some(I::map_ip_out(
                    (),
                    |()| Ipv4Proto::Proto(IpProto::Udp),
                    |()| Ipv6Proto::Proto(IpProto::Udp),
                ))
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

        impl<I: IpExt> MaybeIcmpErrorPayload<I> for FakeUdpPacket {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
                None
            }
        }

        impl<I: FilterIpExt> MaybeIcmpErrorMut<I> for FakeUdpPacket {
            type IcmpErrorMut<'a>
                = Never
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                None
            }
        }

        #[derive(Clone, Debug, PartialEq)]
        pub struct FakeNullPacket;

        impl<I: IpExt> TransportPacketExt<I> for &FakeNullPacket {
            fn proto() -> Option<I::Proto> {
                None
            }
        }

        impl MaybeTransportPacket for &FakeNullPacket {
            fn transport_packet_data(&self) -> Option<TransportPacketData> {
                None
            }
        }

        impl<I: IpExt> MaybeTransportPacketMut<I> for FakeNullPacket {
            type TransportPacketMut<'a> = Never;

            fn transport_packet_mut(&mut self) -> Option<Self::TransportPacketMut<'_>> {
                None
            }
        }

        impl<I: IpExt> MaybeIcmpErrorPayload<I> for FakeNullPacket {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
                None
            }
        }

        impl<I: FilterIpExt> MaybeIcmpErrorMut<I> for FakeNullPacket {
            type IcmpErrorMut<'a>
                = Never
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                None
            }
        }

        pub struct FakeIcmpEchoRequest {
            pub id: u16,
        }

        impl<I: FilterIpExt> TransportPacketExt<I> for &FakeIcmpEchoRequest {
            fn proto() -> Option<I::Proto> {
                Some(I::map_ip_out((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6))
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

        impl<I: IpExt> MaybeIcmpErrorPayload<I> for FakeIcmpEchoRequest {
            fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
                None
            }
        }

        impl<I: FilterIpExt> MaybeIcmpErrorMut<I> for FakeIcmpEchoRequest {
            type IcmpErrorMut<'a>
                = Never
            where
                Self: 'a;

            fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
                None
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

        impl ArbitraryValue for FakeNullPacket {
            fn arbitrary_value() -> Self {
                FakeNullPacket
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
                    wnd: UnscaledWindowSize::from(1234),
                    ..Default::default()
                }
            }
        }

        pub(crate) trait IcmpErrorMessage<I: FilterIpExt> {
            type Serializer: TransportPacketSerializer<I, Buffer: packet::ReusableBuffer> + Debug;

            fn proto() -> I::Proto {
                I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
            }

            fn make_serializer(
                src_ip: I::Addr,
                dst_ip: I::Addr,
                inner: Vec<u8>,
            ) -> Self::Serializer;

            fn make_serializer_truncated(
                src_ip: I::Addr,
                dst_ip: I::Addr,
                mut payload: Vec<u8>,
                truncate_payload: Option<usize>,
            ) -> Self::Serializer {
                if let Some(len) = truncate_payload {
                    payload.truncate(len);
                }

                Self::make_serializer(src_ip, dst_ip, payload)
            }
        }

        pub(crate) struct Icmpv4DestUnreachableError;

        impl IcmpErrorMessage<Ipv4> for Icmpv4DestUnreachableError {
            type Serializer = Nested<Buf<Vec<u8>>, IcmpPacketBuilder<Ipv4, IcmpDestUnreachable>>;

            fn make_serializer(
                src_ip: Ipv4Addr,
                dst_ip: Ipv4Addr,
                payload: Vec<u8>,
            ) -> Self::Serializer {
                Buf::new(payload, ..).encapsulate(
                    IcmpPacketBuilder::<Ipv4, IcmpDestUnreachable>::new(
                        src_ip,
                        dst_ip,
                        Icmpv4DestUnreachableCode::DestHostUnreachable,
                        IcmpDestUnreachable::default(),
                    ),
                )
            }
        }

        pub(crate) struct Icmpv6DestUnreachableError;

        impl IcmpErrorMessage<Ipv6> for Icmpv6DestUnreachableError {
            type Serializer = Nested<
                TruncatingSerializer<Buf<Vec<u8>>>,
                IcmpPacketBuilder<Ipv6, IcmpDestUnreachable>,
            >;

            fn make_serializer(
                src_ip: Ipv6Addr,
                dst_ip: Ipv6Addr,
                payload: Vec<u8>,
            ) -> Self::Serializer {
                TruncatingSerializer::new(Buf::new(payload, ..), TruncateDirection::DiscardBack)
                    .encapsulate(IcmpPacketBuilder::<Ipv6, IcmpDestUnreachable>::new(
                        src_ip,
                        dst_ip,
                        Icmpv6DestUnreachableCode::AddrUnreachable,
                        IcmpDestUnreachable::default(),
                    ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::fmt::Debug;
    use core::marker::PhantomData;
    use netstack3_base::{SeqNum, UnscaledWindowSize};

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use packet::{InnerPacketBuilder as _, ParseBufferMut};
    use packet_formats::icmp::IcmpZeroCode;
    use packet_formats::tcp::TcpSegmentBuilder;
    use test_case::{test_case, test_matrix};

    use crate::conntrack;

    use super::testutil::internal::{
        IcmpErrorMessage, Icmpv4DestUnreachableError, Icmpv6DestUnreachableError, TestIpExt,
    };
    use super::*;

    const SRC_PORT: NonZeroU16 = NonZeroU16::new(11111).unwrap();
    const DST_PORT: NonZeroU16 = NonZeroU16::new(22222).unwrap();
    const SRC_PORT_2: NonZeroU16 = NonZeroU16::new(44444).unwrap();
    const DST_PORT_2: NonZeroU16 = NonZeroU16::new(55555).unwrap();

    const SEQ_NUM: u32 = 1;
    const ACK_NUM: Option<u32> = Some(2);
    const WINDOW_SIZE: u16 = 3u16;

    trait Protocol {
        type Serializer<'a, I: FilterIpExt>: TransportPacketSerializer<I, Buffer: packet::ReusableBuffer>
            + MaybeTransportPacketMut<I>
            + Debug
            + PartialEq;

        fn proto<I: IpExt>() -> I::Proto;

        fn make_serializer_with_ports_data<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
            data: &'a [u8],
        ) -> Self::Serializer<'a, I>;

        fn make_serializer_with_ports<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
        ) -> Self::Serializer<'a, I> {
            Self::make_serializer_with_ports_data(src_ip, dst_ip, src_port, dst_port, &[1, 2, 3])
        }

        fn make_serializer<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
        ) -> Self::Serializer<'a, I> {
            Self::make_serializer_with_ports(src_ip, dst_ip, SRC_PORT, DST_PORT)
        }

        fn make_packet<I: FilterIpExt>(src_ip: I::Addr, dst_ip: I::Addr) -> Vec<u8> {
            Self::make_packet_with_ports::<I>(src_ip, dst_ip, SRC_PORT, DST_PORT)
        }

        fn make_packet_with_ports<I: FilterIpExt>(
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

        fn make_ip_packet_with_ports_data<I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
            data: &[u8],
        ) -> Vec<u8> {
            Self::make_serializer_with_ports_data::<I>(src_ip, dst_ip, src_port, dst_port, data)
                .encapsulate(I::PacketBuilder::new(src_ip, dst_ip, u8::MAX, Self::proto::<I>()))
                .serialize_vec_outer()
                .expect("serialize packet")
                .unwrap_b()
                .into_inner()
        }
    }

    struct Udp;

    impl Protocol for Udp {
        type Serializer<'a, I: FilterIpExt> =
            Nested<InnerSerializer<&'a [u8], EmptyBuf>, UdpPacketBuilder<I::Addr>>;

        fn proto<I: IpExt>() -> I::Proto {
            IpProto::Udp.into()
        }

        fn make_serializer_with_ports_data<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
            data: &'a [u8],
        ) -> Self::Serializer<'a, I> {
            data.into_serializer().encapsulate(UdpPacketBuilder::new(
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

    impl<A: IpAddress, I: IpExt, Inner> MaybeIcmpErrorPayload<I>
        for Nested<Inner, TcpSegmentBuilder<A>>
    {
        fn icmp_error_payload(&self) -> Option<ParsedIcmpErrorPayload<I>> {
            None
        }
    }

    impl<A: IpAddress, I: FilterIpExt, Inner> MaybeIcmpErrorMut<I>
        for Nested<Inner, TcpSegmentBuilder<A>>
    {
        type IcmpErrorMut<'a>
            = Never
        where
            Self: 'a;

        fn icmp_error_mut<'a>(&'a mut self) -> Option<Self::IcmpErrorMut<'a>> {
            None
        }
    }

    struct Tcp;

    impl Protocol for Tcp {
        type Serializer<'a, I: FilterIpExt> =
            Nested<InnerSerializer<&'a [u8], EmptyBuf>, TcpSegmentBuilder<I::Addr>>;

        fn proto<I: IpExt>() -> I::Proto {
            IpProto::Tcp.into()
        }

        fn make_serializer_with_ports_data<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
            data: &'a [u8],
        ) -> Self::Serializer<'a, I> {
            data.into_serializer().encapsulate(TcpSegmentBuilder::new(
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
        type Serializer<'a, I: FilterIpExt> = Nested<
            InnerSerializer<&'a [u8], EmptyBuf>,
            IcmpPacketBuilder<I, icmp::IcmpEchoRequest>,
        >;

        fn proto<I: IpExt>() -> I::Proto {
            I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
        }

        fn make_serializer_with_ports_data<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            _dst_port: NonZeroU16,
            data: &'a [u8],
        ) -> Self::Serializer<'a, I> {
            data.into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
                src_ip,
                dst_ip,
                IcmpZeroCode,
                icmp::IcmpEchoRequest::new(/* id */ src_port.get(), /* seq */ 0),
            ))
        }
    }

    struct IcmpEchoReply;

    impl Protocol for IcmpEchoReply {
        type Serializer<'a, I: FilterIpExt> =
            Nested<InnerSerializer<&'a [u8], EmptyBuf>, IcmpPacketBuilder<I, icmp::IcmpEchoReply>>;

        fn proto<I: IpExt>() -> I::Proto {
            I::map_ip((), |()| Ipv4Proto::Icmp, |()| Ipv6Proto::Icmpv6)
        }

        fn make_serializer_with_ports_data<'a, I: FilterIpExt>(
            src_ip: I::Addr,
            dst_ip: I::Addr,
            _src_port: NonZeroU16,
            dst_port: NonZeroU16,
            data: &'a [u8],
        ) -> Self::Serializer<'a, I> {
            data.into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
                src_ip,
                dst_ip,
                IcmpZeroCode,
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

        fn make_ip_packet_with_ports_data<I: TestIpExt>(
            &self,
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: NonZeroU16,
            dst_port: NonZeroU16,
            data: &[u8],
        ) -> Vec<u8> {
            match self {
                TransportPacketDataProtocol::Tcp => Tcp::make_ip_packet_with_ports_data::<I>(
                    src_ip, dst_ip, src_port, dst_port, data,
                ),
                TransportPacketDataProtocol::Udp => Udp::make_ip_packet_with_ports_data::<I>(
                    src_ip, dst_ip, src_port, dst_port, data,
                ),
                TransportPacketDataProtocol::IcmpEchoRequest => {
                    IcmpEchoRequest::make_ip_packet_with_ports_data::<I>(
                        src_ip, dst_ip, src_port, dst_port, data,
                    )
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
                    wnd: UnscaledWindowSize::from(WINDOW_SIZE),
                    ..Default::default()
                },
                payload_len: 3,
            },
            TransportPacketDataProtocol::Udp => {
                TransportPacketData::Generic { src_port: SRC_PORT.get(), dst_port: DST_PORT.get() }
            }
            TransportPacketDataProtocol::IcmpEchoRequest => {
                TransportPacketData::Generic { src_port: SRC_PORT.get(), dst_port: SRC_PORT.get() }
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

    enum PacketType {
        FullyParsed,
        Raw,
    }

    #[ip_test(I)]
    #[test_matrix(
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            PacketType::FullyParsed,
            PacketType::Raw
        ]
    )]
    fn conntrack_packet_data_from_ip_packet<I: TestIpExt>(
        proto: TransportPacketDataProtocol,
        packet_type: PacketType,
    ) where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
        for<'a> I::PacketRaw<&'a mut [u8]>: IpPacket<I>,
    {
        let expected_data = match proto {
            TransportPacketDataProtocol::Tcp => conntrack::PacketMetadata::new(
                I::SRC_IP,
                I::DST_IP,
                conntrack::TransportProtocol::Tcp,
                TransportPacketData::Tcp {
                    src_port: SRC_PORT.get(),
                    dst_port: DST_PORT.get(),
                    segment: SegmentHeader {
                        seq: SeqNum::new(SEQ_NUM),
                        ack: ACK_NUM.map(SeqNum::new),
                        wnd: UnscaledWindowSize::from(WINDOW_SIZE),
                        ..Default::default()
                    },
                    payload_len: 3,
                },
            ),
            TransportPacketDataProtocol::Udp => conntrack::PacketMetadata::new(
                I::SRC_IP,
                I::DST_IP,
                conntrack::TransportProtocol::Udp,
                TransportPacketData::Generic { src_port: SRC_PORT.get(), dst_port: DST_PORT.get() },
            ),
            TransportPacketDataProtocol::IcmpEchoRequest => conntrack::PacketMetadata::new(
                I::SRC_IP,
                I::DST_IP,
                conntrack::TransportProtocol::Icmp,
                TransportPacketData::Generic { src_port: SRC_PORT.get(), dst_port: SRC_PORT.get() },
            ),
        };

        let mut buf = proto.make_ip_packet_with_ports_data::<I>(
            I::SRC_IP,
            I::DST_IP,
            SRC_PORT,
            DST_PORT,
            &[1, 2, 3],
        );

        let parsed_data = match packet_type {
            PacketType::FullyParsed => {
                let packet = I::Packet::parse_mut(SliceBufViewMut::new(buf.as_mut()), ())
                    .expect("parse IP packet");
                packet.conntrack_packet().expect("packet should be trackable")
            }
            PacketType::Raw => {
                let packet = I::PacketRaw::parse_mut(SliceBufViewMut::new(buf.as_mut()), ())
                    .expect("parse IP packet");
                packet.conntrack_packet().expect("packet should be trackable")
            }
        };

        assert_eq!(parsed_data, expected_data);
    }

    #[ip_test(I)]
    #[test_case(Udp)]
    #[test_case(Tcp)]
    #[test_case(IcmpEchoRequest)]
    fn update_pseudo_header_address_updates_checksum<I: TestIpExt, P: Protocol>(_proto: P) {
        let mut buf = P::make_packet::<I>(I::SRC_IP, I::DST_IP);
        let view = SliceBufViewMut::new(&mut buf);

        let mut packet = ParsedTransportHeaderMut::<I>::parse_in_ip_packet(P::proto::<I>(), view)
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

        let mut packet = ParsedTransportHeaderMut::<I>::parse_in_ip_packet(P::proto::<I>(), view)
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
            IcmpZeroCode,
            icmp::IcmpEchoRequest::new(SRC_PORT.get(), /* seq */ 0),
        ));
        serializer
            .transport_packet_mut()
            .expect("packet should support rewriting")
            .set_src_port(SRC_PORT_2);

        let equivalent = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpZeroCode,
            icmp::IcmpEchoRequest::new(SRC_PORT_2.get(), /* seq */ 0),
        ));

        assert_eq!(equivalent, serializer);
    }

    #[ip_test(I)]
    fn icmp_echo_reply_update_id_port_updates_checksum<I: TestIpExt>() {
        let mut serializer = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpZeroCode,
            icmp::IcmpEchoReply::new(SRC_PORT.get(), /* seq */ 0),
        ));
        serializer
            .transport_packet_mut()
            .expect("packet should support rewriting")
            .set_dst_port(SRC_PORT_2);

        let equivalent = [].into_serializer().encapsulate(IcmpPacketBuilder::<I, _>::new(
            I::SRC_IP,
            I::DST_IP,
            IcmpZeroCode,
            icmp::IcmpEchoReply::new(SRC_PORT_2.get(), /* seq */ 0),
        ));

        assert_eq!(equivalent, serializer);
    }

    fn ip_packet<I: FilterIpExt, P: Protocol>(src: I::Addr, dst: I::Addr) -> Buf<Vec<u8>> {
        Buf::new(P::make_packet::<I>(src, dst), ..)
            .encapsulate(I::PacketBuilder::new(src, dst, /* ttl */ u8::MAX, P::proto::<I>()))
            .serialize_vec_outer()
            .expect("serialize IP packet")
            .unwrap_b()
    }

    #[ip_test(I)]
    #[test_matrix(
        [
            PhantomData::<Udp>,
            PhantomData::<Tcp>,
            PhantomData::<IcmpEchoRequest>,
        ],
        [
            PacketType::FullyParsed,
            PacketType::Raw
        ]
    )]
    fn ip_packet_set_src_dst_addr_updates_checksums<I: TestIpExt, P: Protocol>(
        _proto: PhantomData<P>,
        packet_type: PacketType,
    ) where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
        for<'a> I::PacketRaw<&'a mut [u8]>: IpPacket<I>,
    {
        let mut buf = ip_packet::<I, P>(I::SRC_IP, I::DST_IP).into_inner();

        match packet_type {
            PacketType::FullyParsed => {
                let mut packet = I::Packet::parse_mut(SliceBufViewMut::new(&mut buf), ())
                    .expect("parse IP packet");
                packet.set_src_addr(I::SRC_IP_2);
                packet.set_dst_addr(I::DST_IP_2);
            }
            PacketType::Raw => {
                let mut packet = I::PacketRaw::parse_mut(SliceBufViewMut::new(&mut buf), ())
                    .expect("parse IP packet");
                packet.set_src_addr(I::SRC_IP_2);
                packet.set_dst_addr(I::DST_IP_2);
            }
        }

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

    #[ip_test(I)]
    #[test_matrix(
         [
             PhantomData::<Udp>,
             PhantomData::<Tcp>,
             PhantomData::<IcmpEchoRequest>,
         ],
         [
             PacketType::FullyParsed,
             PacketType::Raw
         ]
     )]
    fn no_icmp_error_for_normal_ip_packet<I: TestIpExt, P: Protocol>(
        _proto: PhantomData<P>,
        packet_type: PacketType,
    ) where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
        for<'a> I::PacketRaw<&'a mut [u8]>: IpPacket<I>,
    {
        let mut buf = ip_packet::<I, P>(I::SRC_IP, I::DST_IP).into_inner();
        let icmp_error = match packet_type {
            PacketType::FullyParsed => {
                let packet = I::Packet::parse_mut(SliceBufViewMut::new(&mut buf), ())
                    .expect("parse IP packet");
                let icmp_payload = packet.maybe_icmp_error().icmp_error_payload();

                icmp_payload
            }
            PacketType::Raw => {
                let packet = I::PacketRaw::parse_mut(SliceBufViewMut::new(&mut buf), ())
                    .expect("parse IP packet");
                let icmp_payload = packet.maybe_icmp_error().icmp_error_payload();

                icmp_payload
            }
        };

        assert_matches!(icmp_error, None);
    }

    #[ip_test(I)]
    #[test_matrix(
         [
             PhantomData::<Udp>,
             PhantomData::<Tcp>,
             PhantomData::<IcmpEchoRequest>,
         ],
         [
             PacketType::FullyParsed,
             PacketType::Raw
         ]
     )]
    fn no_icmp_error_mut_for_normal_ip_packet<I: TestIpExt, P: Protocol>(
        _proto: PhantomData<P>,
        packet_type: PacketType,
    ) where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
        for<'a> I::PacketRaw<&'a mut [u8]>: IpPacket<I>,
    {
        let mut buf = ip_packet::<I, P>(I::SRC_IP, I::DST_IP).into_inner();
        match packet_type {
            PacketType::FullyParsed => {
                let mut packet = I::Packet::parse_mut(SliceBufViewMut::new(&mut buf), ())
                    .expect("parse IP packet");
                assert!(packet.icmp_error_mut().icmp_error_mut().is_none());
            }
            PacketType::Raw => {
                let mut packet = I::PacketRaw::parse_mut(SliceBufViewMut::new(&mut buf), ())
                    .expect("parse IP packet");
                assert!(packet.icmp_error_mut().icmp_error_mut().is_none());
            }
        }
    }

    #[ip_test(I)]
    #[test_case(TransportPacketDataProtocol::Udp)]
    #[test_case(TransportPacketDataProtocol::Tcp)]
    #[test_case(TransportPacketDataProtocol::IcmpEchoRequest)]
    fn no_icmp_error_for_normal_bytes<I: TestIpExt>(proto: TransportPacketDataProtocol) {
        let buf = proto.make_packet::<I>(I::SRC_IP, I::DST_IP);

        assert_matches!(
            ParsedIcmpErrorPayload::<I>::parse_in_outer_ip_packet(
                proto.proto::<I>(),
                buf.as_slice(),
            ),
            None
        );
    }

    #[ip_test(I)]
    #[test_case(TransportPacketDataProtocol::Udp)]
    #[test_case(TransportPacketDataProtocol::Tcp)]
    #[test_case(TransportPacketDataProtocol::IcmpEchoRequest)]
    fn no_icmp_error_mut_for_normal_bytes<I: TestIpExt>(proto: TransportPacketDataProtocol) {
        let mut buf = proto.make_packet::<I>(I::SRC_IP, I::DST_IP);

        assert!(ParsedIcmpErrorMut::<I>::parse_in_ip_packet(
            I::SRC_IP,
            I::DST_IP,
            proto.proto::<I>(),
            SliceBufViewMut::new(&mut buf),
        )
        .is_none());
    }

    #[ip_test(I)]
    #[test_case(PhantomData::<Udp>)]
    #[test_case(PhantomData::<Tcp>)]
    #[test_case(PhantomData::<IcmpEchoRequest>)]
    fn no_icmp_error_for_normal_serializer<I: TestIpExt, P: Protocol>(_proto: PhantomData<P>) {
        let serializer =
            P::make_serializer_with_ports::<I>(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT);

        assert_matches!(serializer.icmp_error_payload(), None);
    }

    #[ip_test(I)]
    #[test_case(PhantomData::<Udp>)]
    #[test_case(PhantomData::<Tcp>)]
    #[test_case(PhantomData::<IcmpEchoRequest>)]
    fn no_icmp_error_mut_for_normal_serializer<I: TestIpExt, P: Protocol>(_proto: PhantomData<P>) {
        let mut serializer =
            P::make_serializer_with_ports::<I>(I::SRC_IP, I::DST_IP, SRC_PORT, DST_PORT);

        assert!(serializer.icmp_error_mut().is_none());
    }

    #[test_matrix(
        [
            PhantomData::<Icmpv4DestUnreachableError>,
            PhantomData::<Icmpv6DestUnreachableError>,
        ],
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            PacketType::FullyParsed,
            PacketType::Raw,
        ],
        [
            false,
            true,
        ]
    )]
    fn icmp_error_from_bytes<I: TestIpExt, IE: IcmpErrorMessage<I>>(
        _icmp_error: PhantomData<IE>,
        proto: TransportPacketDataProtocol,
        packet_type: PacketType,
        truncate_message: bool,
    ) {
        let serializer = IE::make_serializer_truncated(
            I::DST_IP_2,
            I::SRC_IP,
            proto.make_ip_packet_with_ports_data::<I>(
                I::SRC_IP,
                I::DST_IP,
                SRC_PORT,
                DST_PORT,
                &[0xAB; 5000],
            ),
            // Try with a truncated and full body to make sure we don't fail
            // when a partial payload is present. In these cases, the ICMP error
            // payload checksum can't be validated, though we want to be sure
            // it's updated as if it were correct.
            truncate_message.then_some(1280),
        )
        .encapsulate(I::PacketBuilder::new(I::DST_IP_2, I::SRC_IP, u8::MAX, IE::proto()));

        let mut bytes: Buf<Vec<u8>> = serializer.serialize_vec_outer().unwrap().unwrap_b();
        let icmp_payload = match packet_type {
            PacketType::FullyParsed => {
                let packet = I::as_filter_packet_owned(bytes.parse_mut::<I::Packet<_>>().unwrap());
                let icmp_payload =
                    packet.maybe_icmp_error().icmp_error_payload().expect("no ICMP error found");

                icmp_payload
            }
            PacketType::Raw => {
                let packet =
                    I::as_filter_packet_raw_owned(bytes.parse_mut::<I::PacketRaw<_>>().unwrap());
                let icmp_payload =
                    packet.maybe_icmp_error().icmp_error_payload().expect("no ICMP error found");

                icmp_payload
            }
        };

        let expected = match proto {
            TransportPacketDataProtocol::Tcp | TransportPacketDataProtocol::Udp => {
                ParsedIcmpErrorPayload {
                    src_ip: I::SRC_IP,
                    dst_ip: I::DST_IP,
                    src_port: SRC_PORT.get(),
                    dst_port: DST_PORT.get(),
                    proto: proto.proto::<I>(),
                }
            }
            TransportPacketDataProtocol::IcmpEchoRequest => {
                ParsedIcmpErrorPayload {
                    src_ip: I::SRC_IP,
                    dst_ip: I::DST_IP,
                    // NOTE: These are intentionally the same because of how
                    // ICMP tracking works.
                    src_port: SRC_PORT.get(),
                    dst_port: SRC_PORT.get(),
                    proto: proto.proto::<I>(),
                }
            }
        };

        assert_eq!(icmp_payload, expected);
    }

    #[test_matrix(
        [
            PhantomData::<Icmpv4DestUnreachableError>,
            PhantomData::<Icmpv6DestUnreachableError>,
        ],
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            false,
            true,
        ]
    )]
    fn icmp_error_from_serializer<I: TestIpExt, IE: IcmpErrorMessage<I>>(
        _icmp_error: PhantomData<IE>,
        proto: TransportPacketDataProtocol,
        truncate_message: bool,
    ) {
        let serializer = IE::make_serializer_truncated(
            I::DST_IP_2,
            I::SRC_IP,
            proto.make_ip_packet_with_ports_data::<I>(
                I::SRC_IP,
                I::DST_IP,
                SRC_PORT,
                DST_PORT,
                &[0xAB; 5000],
            ),
            // Try with a truncated and full body to make sure we don't fail
            // when a partial payload is present. In these cases, the ICMP error
            // payload checksum can't be validated, though we want to be sure
            // it's updated as if it were correct.
            truncate_message.then_some(1280),
        );

        let actual =
            serializer.icmp_error_payload().expect("serializer should contain an IP packet");

        let expected = match proto {
            TransportPacketDataProtocol::Tcp | TransportPacketDataProtocol::Udp => {
                ParsedIcmpErrorPayload::<I> {
                    src_ip: I::SRC_IP,
                    dst_ip: I::DST_IP,
                    src_port: SRC_PORT.get(),
                    dst_port: DST_PORT.get(),
                    proto: proto.proto::<I>(),
                }
            }
            TransportPacketDataProtocol::IcmpEchoRequest => ParsedIcmpErrorPayload::<I> {
                src_ip: I::SRC_IP,
                dst_ip: I::DST_IP,
                // NOTE: These are intentionally the same because of how ICMP
                // tracking works.
                src_port: SRC_PORT.get(),
                dst_port: SRC_PORT.get(),
                proto: proto.proto::<I>(),
            },
        };

        assert_eq!(actual, expected);
    }

    #[test_matrix(
        [
            PhantomData::<Icmpv4DestUnreachableError>,
            PhantomData::<Icmpv6DestUnreachableError>,
        ],
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            PacketType::FullyParsed,
            PacketType::Raw,
        ],
        [
            false,
            true,
        ]
    )]
    fn conntrack_packet_icmp_error_from_bytes<I: TestIpExt, IE: IcmpErrorMessage<I>>(
        _icmp_error: PhantomData<IE>,
        proto: TransportPacketDataProtocol,
        packet_type: PacketType,
        truncate_message: bool,
    ) {
        let serializer = IE::make_serializer_truncated(
            I::DST_IP_2,
            I::SRC_IP,
            proto.make_ip_packet_with_ports_data::<I>(
                I::SRC_IP,
                I::DST_IP,
                SRC_PORT,
                DST_PORT,
                &[0xAB; 5000],
            ),
            // Try with a truncated and full body to make sure we don't fail
            // when a partial payload is present. In these cases, the ICMP error
            // payload checksum can't be validated, though we want to be sure
            // it's updated as if it were correct.
            truncate_message.then_some(1280),
        )
        .encapsulate(I::PacketBuilder::new(I::DST_IP_2, I::SRC_IP, u8::MAX, IE::proto()));

        let mut bytes: Buf<Vec<u8>> = serializer.serialize_vec_outer().unwrap().unwrap_b();

        let conntrack_packet = match packet_type {
            PacketType::FullyParsed => {
                let packet = I::as_filter_packet_owned(bytes.parse_mut::<I::Packet<_>>().unwrap());
                packet.conntrack_packet().unwrap()
            }
            PacketType::Raw => {
                let packet =
                    I::as_filter_packet_raw_owned(bytes.parse_mut::<I::PacketRaw<_>>().unwrap());
                packet.conntrack_packet().unwrap()
            }
        };

        let expected = match proto {
            TransportPacketDataProtocol::Tcp | TransportPacketDataProtocol::Udp => {
                conntrack::PacketMetadata::new_from_icmp_error(
                    I::SRC_IP,
                    I::DST_IP,
                    SRC_PORT.get(),
                    DST_PORT.get(),
                    I::map_ip(proto.proto::<I>(), |proto| proto.into(), |proto| proto.into()),
                )
            }
            TransportPacketDataProtocol::IcmpEchoRequest => {
                conntrack::PacketMetadata::new_from_icmp_error(
                    I::SRC_IP,
                    I::DST_IP,
                    // NOTE: These are intentionally the same because of how
                    // ICMP tracking works.
                    SRC_PORT.get(),
                    SRC_PORT.get(),
                    I::map_ip(proto.proto::<I>(), |proto| proto.into(), |proto| proto.into()),
                )
            }
        };

        assert_eq!(conntrack_packet, expected);
    }

    #[test_matrix(
        [
            PhantomData::<Icmpv4DestUnreachableError>,
            PhantomData::<Icmpv6DestUnreachableError>,
        ],
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            PacketType::FullyParsed,
            PacketType::Raw,
        ],
        [
            false,
            true,
        ]
    )]
    fn no_conntrack_packet_for_incompatible_outer_and_payload<
        I: TestIpExt,
        IE: IcmpErrorMessage<I>,
    >(
        _icmp_error: PhantomData<IE>,
        proto: TransportPacketDataProtocol,
        packet_type: PacketType,
        truncate_message: bool,
    ) {
        // In order for the outer packet to have the tuple (DST_IP_2, SRC_IP_2),
        // the host sending the error must have seen a packet with a source
        // address of SRC_IP_2, but we know that can't be right because the
        // payload of the packet contains a packet with a source address of
        // SRC_IP.
        let serializer = IE::make_serializer_truncated(
            I::DST_IP_2,
            I::SRC_IP_2,
            proto.make_ip_packet_with_ports_data::<I>(
                I::SRC_IP,
                I::DST_IP,
                SRC_PORT,
                DST_PORT,
                &[0xAB; 5000],
            ),
            // Try with a truncated and full body to make sure we don't fail
            // when a partial payload is present. In these cases, the ICMP error
            // payload checksum can't be validated, though we want to be sure
            // it's updated as if it were correct.
            truncate_message.then_some(1280),
        )
        .encapsulate(I::PacketBuilder::new(I::DST_IP_2, I::SRC_IP_2, u8::MAX, IE::proto()));

        let mut bytes: Buf<Vec<u8>> = serializer.serialize_vec_outer().unwrap().unwrap_b();

        let conntrack_packet = match packet_type {
            PacketType::FullyParsed => {
                let packet = I::as_filter_packet_owned(bytes.parse_mut::<I::Packet<_>>().unwrap());
                packet.conntrack_packet()
            }
            PacketType::Raw => {
                let packet =
                    I::as_filter_packet_raw_owned(bytes.parse_mut::<I::PacketRaw<_>>().unwrap());
                packet.conntrack_packet()
            }
        };

        // Because the outer and payload tuples aren't compatible, we shouldn't
        // get a conntrack packet back.
        assert_matches!(conntrack_packet, None);
    }

    #[test_matrix(
        [
            PhantomData::<Icmpv4DestUnreachableError>,
            PhantomData::<Icmpv6DestUnreachableError>,
        ],
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            false,
            true,
        ]
    )]
    fn icmp_error_mut_from_serializer<I: TestIpExt, IE: IcmpErrorMessage<I>>(
        _icmp_error: PhantomData<IE>,
        proto: TransportPacketDataProtocol,
        truncate_message: bool,
    ) where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
    {
        const LEN: usize = 5000;

        let mut payload_bytes = proto.make_ip_packet_with_ports_data::<I>(
            I::SRC_IP,
            I::DST_IP,
            SRC_PORT,
            DST_PORT,
            &[0xAB; LEN],
        );

        // Try with a truncated and full body to make sure we don't fail when a
        // partial payload is present.
        if truncate_message {
            payload_bytes.truncate(1280);
        }

        let mut serializer = IE::make_serializer(I::SRC_IP, I::DST_IP, payload_bytes)
            .encapsulate(I::PacketBuilder::new(I::SRC_IP, I::DST_IP, u8::MAX, IE::proto()));

        {
            let mut icmp_packet = serializer
                .icmp_error_mut()
                .icmp_error_mut()
                .expect("couldn't find an inner ICMP error");

            {
                let mut inner_packet = icmp_packet.inner_packet().expect("no inner packet");

                inner_packet.set_src_addr(I::SRC_IP_2);
                inner_packet.set_dst_addr(I::DST_IP_2);
            }

            // Since this is just a serializer, there's no thing to be recalculated,
            // but this should still never fail.
            assert!(icmp_packet.recalculate_checksum());
        }

        let mut expected_payload_bytes = proto.make_ip_packet_with_ports_data::<I>(
            I::SRC_IP_2,
            I::DST_IP_2,
            SRC_PORT,
            DST_PORT,
            &[0xAB; LEN],
        );

        // Try with a truncated and full body to make sure we don't fail when a
        // partial payload is present.
        if truncate_message {
            expected_payload_bytes.truncate(1280);
        }

        let expected_serializer = IE::make_serializer(I::SRC_IP, I::DST_IP, expected_payload_bytes)
            // We never updated the outer IPs, so they should still be
            // their original values.
            .encapsulate(I::PacketBuilder::new(I::SRC_IP, I::DST_IP, u8::MAX, IE::proto()));

        let actual_bytes = serializer.serialize_vec_outer().unwrap().unwrap_b();
        let expected_bytes = expected_serializer.serialize_vec_outer().unwrap().unwrap_b();

        assert_eq!(actual_bytes, expected_bytes);
    }

    #[test_matrix(
        [
            PhantomData::<Icmpv4DestUnreachableError>,
            PhantomData::<Icmpv6DestUnreachableError>,
        ],
        [
            TransportPacketDataProtocol::Udp,
            TransportPacketDataProtocol::Tcp,
            TransportPacketDataProtocol::IcmpEchoRequest,
        ],
        [
            PacketType::FullyParsed,
            PacketType::Raw,
        ],
        [
            false,
            true,
        ]
    )]
    fn icmp_error_mut_from_bytes<I: TestIpExt, IE: IcmpErrorMessage<I>>(
        _icmp_error: PhantomData<IE>,
        proto: TransportPacketDataProtocol,
        packet_type: PacketType,
        truncate_message: bool,
    ) where
        for<'a> I::Packet<&'a mut [u8]>: IpPacket<I>,
    {
        const LEN: usize = 5000;

        let mut payload_bytes = proto.make_ip_packet_with_ports_data::<I>(
            I::SRC_IP,
            I::DST_IP,
            SRC_PORT,
            DST_PORT,
            &[0xAB; LEN],
        );

        // Try with a truncated and full body to make sure we don't fail when a
        // partial payload is present.
        if truncate_message {
            payload_bytes.truncate(1280);
        }

        let serializer = IE::make_serializer(I::SRC_IP, I::DST_IP, payload_bytes)
            .encapsulate(I::PacketBuilder::new(I::SRC_IP, I::DST_IP, u8::MAX, IE::proto()));

        let mut bytes = serializer.serialize_vec_outer().unwrap().unwrap_b().into_inner();

        {
            fn modify_packet<I: TestIpExt, P: IpPacket<I>>(mut packet: P) {
                let mut icmp_error = packet.icmp_error_mut();
                let mut icmp_error =
                    icmp_error.icmp_error_mut().expect("couldn't find an inner ICMP error");

                {
                    let mut inner_packet = icmp_error.inner_packet().expect("no inner packet");

                    inner_packet.set_src_addr(I::SRC_IP_2);
                    inner_packet.set_dst_addr(I::DST_IP_2);
                }

                assert!(icmp_error.recalculate_checksum());
            }

            let mut bytes = Buf::new(&mut bytes, ..);

            match packet_type {
                PacketType::FullyParsed => {
                    let packet =
                        I::as_filter_packet_owned(bytes.parse_mut::<I::Packet<_>>().unwrap());
                    modify_packet(packet);
                }
                PacketType::Raw => {
                    let packet = I::as_filter_packet_raw_owned(
                        bytes.parse_mut::<I::PacketRaw<_>>().unwrap(),
                    );
                    modify_packet(packet);
                }
            }
        }

        let mut expected_payload_bytes = proto.make_ip_packet_with_ports_data::<I>(
            I::SRC_IP_2,
            I::DST_IP_2,
            SRC_PORT,
            DST_PORT,
            &[0xAB; LEN],
        );

        if truncate_message {
            expected_payload_bytes.truncate(1280);
        }

        let expected_serializer = IE::make_serializer(I::SRC_IP, I::DST_IP, expected_payload_bytes)
            // We never updated the outer IPs, so they should still be
            // their original values.
            .encapsulate(I::PacketBuilder::new(I::SRC_IP, I::DST_IP, u8::MAX, IE::proto()));

        let expected_bytes =
            expected_serializer.serialize_vec_outer().unwrap().unwrap_b().into_inner();

        assert_eq!(bytes, expected_bytes);
    }
}
