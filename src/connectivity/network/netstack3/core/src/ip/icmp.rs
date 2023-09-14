// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Internet Control Message Protocol (ICMP).

use core::{
    convert::TryInto as _,
    fmt::Debug,
    num::{NonZeroU16, NonZeroU8},
};

use assert_matches::assert_matches;
use derivative::Derivative;
use lock_order::{lock::UnlockedAccess, relation::LockBefore, Locked};
use net_types::{
    ip::{
        GenericOverIp, Ip, IpAddress, IpVersionMarker, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
        Ipv6SourceAddr, Mtu, SubnetError,
    },
    LinkLocalAddress, LinkLocalUnicastAddr, MulticastAddress, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{
    BufferMut, EmptyBuf, InnerPacketBuilder as _, ParseBuffer, Serializer, TruncateDirection,
    TruncatingSerializer,
};
use packet_formats::{
    icmp::{
        ndp::{
            options::{NdpOption, NdpOptionBuilder},
            NdpPacket, NeighborAdvertisement, NonZeroNdpLifetime, OptionSequenceBuilder,
        },
        peek_message_type, IcmpDestUnreachable, IcmpEchoRequest, IcmpMessage, IcmpMessageType,
        IcmpPacket, IcmpPacketBuilder, IcmpPacketRaw, IcmpParseArgs, IcmpTimeExceeded,
        IcmpUnusedCode, Icmpv4DestUnreachableCode, Icmpv4Packet, Icmpv4ParameterProblem,
        Icmpv4ParameterProblemCode, Icmpv4RedirectCode, Icmpv4TimeExceededCode,
        Icmpv6DestUnreachableCode, Icmpv6Packet, Icmpv6PacketTooBig, Icmpv6ParameterProblem,
        Icmpv6ParameterProblemCode, Icmpv6TimeExceededCode, MessageBody, OriginalPacket,
    },
    ip::{Ipv4Proto, Ipv6Proto},
    ipv4::{Ipv4FragmentType, Ipv4Header},
    ipv6::{ExtHdrParseError, Ipv6Header},
};
use thiserror::Error;
use tracing::{debug, error, trace};
use zerocopy::ByteSlice;

use crate::{
    algorithm::{PortAlloc, PortAllocImpl},
    context::{CounterContext, InstantContext, RngContext},
    data_structures::{id_map::EntryKey, socketmap::IterShadows as _, token_bucket::TokenBucket},
    device::{FrameDestination, Id, WeakId},
    ip::{
        device::{
            nud::{ConfirmationFlags, NudIpHandler},
            route_discovery::Ipv6DiscoveredRoute,
            IpAddressState, IpDeviceHandler, Ipv6DeviceHandler,
        },
        path_mtu::PmtuHandler,
        socket::{
            BufferIpSocketHandler, DefaultSendOptions, IpSock, IpSockCreationError,
            IpSockSendError, IpSocketHandler,
        },
        AddressStatus, AnyDevice, BufferIpLayerHandler, BufferIpTransportContext, DeviceIdContext,
        EitherDeviceId, IpDeviceStateContext, IpExt, IpTransportContext, Ipv6PresentAddressStatus,
        MulticastMembershipHandler, SendIpPacketMeta, TransportIpContext, TransportReceiveError,
        IPV6_DEFAULT_SUBNET,
    },
    socket::{
        self,
        address::{AddrIsMappedError, ConnAddr, ConnIpAddr, SocketIpAddr},
        datagram::{
            self, DatagramBoundStateContext, DatagramFlowId, DatagramSocketMapSpec,
            DatagramSocketSpec,
        },
        AddrVec, IncompatibleError, InsertError, ListenerAddrInfo, SocketMapAddrSpec,
        SocketMapAddrStateSpec, SocketMapConflictPolicy, SocketMapStateSpec,
    },
    sync::{Mutex, RwLock},
    BufferNonSyncContext, NonSyncContext, SyncCtx,
};

/// The IP packet hop limit for all NDP packets.
///
/// See [RFC 4861 section 4.1], [RFC 4861 section 4.2], [RFC 4861 section 4.2],
/// [RFC 4861 section 4.3], [RFC 4861 section 4.4], and [RFC 4861 section 4.5]
/// for more information.
///
/// [RFC 4861 section 4.1]: https://tools.ietf.org/html/rfc4861#section-4.1
/// [RFC 4861 section 4.2]: https://tools.ietf.org/html/rfc4861#section-4.2
/// [RFC 4861 section 4.3]: https://tools.ietf.org/html/rfc4861#section-4.3
/// [RFC 4861 section 4.4]: https://tools.ietf.org/html/rfc4861#section-4.4
/// [RFC 4861 section 4.5]: https://tools.ietf.org/html/rfc4861#section-4.5
pub(crate) const REQUIRED_NDP_IP_PACKET_HOP_LIMIT: u8 = 255;

/// The default number of ICMP error messages to send per second.
///
/// Beyond this rate, error messages will be silently dropped.
pub const DEFAULT_ERRORS_PER_SECOND: u64 = 2 << 16;

/// An ICMPv4 error type and code.
///
/// Each enum variant corresponds to a particular error type, and contains the
/// possible codes for that error type.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Icmpv4ErrorCode {
    DestUnreachable(Icmpv4DestUnreachableCode),
    Redirect(Icmpv4RedirectCode),
    TimeExceeded(Icmpv4TimeExceededCode),
    ParameterProblem(Icmpv4ParameterProblemCode),
}

impl<I: IcmpIpExt> GenericOverIp<I> for Icmpv4ErrorCode {
    type Type = I::ErrorCode;
}

/// An ICMPv6 error type and code.
///
/// Each enum variant corresponds to a particular error type, and contains the
/// possible codes for that error type.
#[derive(Copy, Clone, Debug, PartialEq)]
#[allow(missing_docs)]
pub enum Icmpv6ErrorCode {
    DestUnreachable(Icmpv6DestUnreachableCode),
    PacketTooBig,
    TimeExceeded(Icmpv6TimeExceededCode),
    ParameterProblem(Icmpv6ParameterProblemCode),
}

impl<I: IcmpIpExt> GenericOverIp<I> for Icmpv6ErrorCode {
    type Type = I::ErrorCode;
}

/// An ICMP error of either IPv4 or IPv6.
#[derive(Debug, Clone, Copy)]
pub enum IcmpErrorCode {
    /// ICMPv4 error.
    V4(Icmpv4ErrorCode),
    /// ICMPv6 error.
    V6(Icmpv6ErrorCode),
}

impl From<Icmpv4ErrorCode> for IcmpErrorCode {
    fn from(v4_err: Icmpv4ErrorCode) -> Self {
        IcmpErrorCode::V4(v4_err)
    }
}

impl From<Icmpv6ErrorCode> for IcmpErrorCode {
    fn from(v6_err: Icmpv6ErrorCode) -> Self {
        IcmpErrorCode::V6(v6_err)
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct IcmpSockets<I: IpExt + datagram::DualStackIpExt, D: WeakId> {
    // This will be used to store state for unbound sockets, like socket
    // options.
    state: datagram::SocketsState<I, D, Icmp>,
    bound: IcmpBoundSockets<I, D>,
    id_allocator: LocalIdAllocator<I, D>,
}

pub(crate) struct IcmpState<I: IpExt + datagram::DualStackIpExt, Instant, D: WeakId> {
    pub(crate) sockets: RwLock<IcmpSockets<I, D>>,
    pub(crate) error_send_bucket: Mutex<TokenBucket<Instant>>,
}

/// A builder for ICMPv4 state.
#[derive(Copy, Clone)]
pub struct Icmpv4StateBuilder {
    send_timestamp_reply: bool,
    errors_per_second: u64,
}

impl Default for Icmpv4StateBuilder {
    fn default() -> Icmpv4StateBuilder {
        Icmpv4StateBuilder {
            send_timestamp_reply: false,
            errors_per_second: DEFAULT_ERRORS_PER_SECOND,
        }
    }
}

impl Icmpv4StateBuilder {
    /// Enable or disable replying to ICMPv4 Timestamp Request messages with
    /// Timestamp Reply messages (default: disabled).
    ///
    /// Enabling this can introduce a very minor vulnerability in which an
    /// attacker can learn the system clock's time, which in turn can aid in
    /// attacks against time-based authentication systems.
    pub fn send_timestamp_reply(&mut self, send_timestamp_reply: bool) -> &mut Self {
        self.send_timestamp_reply = send_timestamp_reply;
        self
    }

    /// Configure the number of ICMPv4 error messages to send per second
    /// (default: [`DEFAULT_ERRORS_PER_SECOND`]).
    ///
    /// The number of ICMPv4 error messages sent by the stack will be rate
    /// limited to the given number of errors per second. Any messages that
    /// exceed this rate will be silently dropped.
    pub fn errors_per_second(&mut self, errors_per_second: u64) -> &mut Self {
        self.errors_per_second = errors_per_second;
        self
    }

    pub(crate) fn build<Instant, D: WeakId>(self) -> Icmpv4State<Instant, D> {
        Icmpv4State {
            inner: IcmpState {
                sockets: Default::default(),
                error_send_bucket: Mutex::new(TokenBucket::new(self.errors_per_second)),
            },
            send_timestamp_reply: self.send_timestamp_reply,
        }
    }
}

/// The state associated with the ICMPv4 protocol.
pub(crate) struct Icmpv4State<Instant, D: WeakId> {
    inner: IcmpState<Ipv4, Instant, D>,
    send_timestamp_reply: bool,
}

// Used by `receive_icmp_echo_reply`.
impl<Instant, D: WeakId> AsRef<IcmpState<Ipv4, Instant, D>> for Icmpv4State<Instant, D> {
    fn as_ref(&self) -> &IcmpState<Ipv4, Instant, D> {
        &self.inner
    }
}

// Used by `send_icmpv4_echo_request_inner`.
impl<Instant, D: WeakId> AsMut<IcmpState<Ipv4, Instant, D>> for Icmpv4State<Instant, D> {
    fn as_mut(&mut self) -> &mut IcmpState<Ipv4, Instant, D> {
        &mut self.inner
    }
}

/// A builder for ICMPv6 state.
#[derive(Copy, Clone)]
pub struct Icmpv6StateBuilder {
    errors_per_second: u64,
}

impl Default for Icmpv6StateBuilder {
    fn default() -> Icmpv6StateBuilder {
        Icmpv6StateBuilder { errors_per_second: DEFAULT_ERRORS_PER_SECOND }
    }
}

impl Icmpv6StateBuilder {
    /// Configure the number of ICMPv6 error messages to send per second
    /// (default: [`DEFAULT_ERRORS_PER_SECOND`]).
    ///
    /// The number of ICMPv6 error messages sent by the stack will be rate
    /// limited to the given number of errors per second. Any messages that
    /// exceed this rate will be silently dropped.
    ///
    /// This rate limit is required by [RFC 4443 Section 2.4] (f).
    ///
    /// [RFC 4443 Section 2.4]: https://tools.ietf.org/html/rfc4443#section-2.4
    pub fn errors_per_second(&mut self, errors_per_second: u64) -> &mut Self {
        self.errors_per_second = errors_per_second;
        self
    }

    pub(crate) fn build<Instant, D: WeakId>(self) -> Icmpv6State<Instant, D> {
        Icmpv6State {
            inner: IcmpState {
                sockets: Default::default(),
                error_send_bucket: Mutex::new(TokenBucket::new(self.errors_per_second)),
            },
        }
    }
}

/// The state associated with the ICMPv6 protocol.
pub(crate) struct Icmpv6State<Instant, D: WeakId> {
    inner: IcmpState<Ipv6, Instant, D>,
}

// Used by `receive_icmp_echo_reply`.
impl<Instant, D: WeakId> AsRef<IcmpState<Ipv6, Instant, D>> for Icmpv6State<Instant, D> {
    fn as_ref(&self) -> &IcmpState<Ipv6, Instant, D> {
        &self.inner
    }
}

// Used by `send_icmpv6_echo_request_inner`.
impl<Instant, D: WeakId> AsMut<IcmpState<Ipv6, Instant, D>> for Icmpv6State<Instant, D> {
    fn as_mut(&mut self) -> &mut IcmpState<Ipv6, Instant, D> {
        &mut self.inner
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) struct IcmpAddr<A: IpAddress> {
    local_addr: SpecifiedAddr<A>,
    remote_addr: SpecifiedAddr<A>,
    icmp_id: u16,
}

/// An identifier for an ICMP socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
pub struct SocketId<I: Ip>(usize, IpVersionMarker<I>);

impl<I: Ip> SocketId<I> {
    fn new(id: usize) -> SocketId<I> {
        SocketId(id, IpVersionMarker::default())
    }
}

impl<I: Ip> Into<usize> for SocketId<I> {
    fn into(self) -> usize {
        let Self(id, _marker) = self;
        id
    }
}

#[derive(Clone)]
pub(crate) struct IcmpConn<S> {
    icmp_id: u16,
    ip: S,
}

impl<'a, A: IpAddress, D> From<&'a IcmpConn<IpSock<A::Version, D, ()>>> for IcmpAddr<A>
where
    A::Version: IpExt,
{
    fn from(conn: &'a IcmpConn<IpSock<A::Version, D, ()>>) -> IcmpAddr<A> {
        IcmpAddr {
            local_addr: *conn.ip.local_ip(),
            remote_addr: *conn.ip.remote_ip(),
            icmp_id: conn.icmp_id,
        }
    }
}

/// An extension trait adding extra ICMP-related functionality to IP versions.
pub trait IcmpIpExt: packet_formats::ip::IpExt + packet_formats::icmp::IcmpIpExt {
    /// The type of error code for this version of ICMP - [`Icmpv4ErrorCode`] or
    /// [`Icmpv6ErrorCode`].
    type ErrorCode: Debug
        + Copy
        + PartialEq
        + GenericOverIp<Self, Type = Self::ErrorCode>
        + GenericOverIp<Ipv4, Type = Icmpv4ErrorCode>
        + GenericOverIp<Ipv6, Type = Icmpv6ErrorCode>
        + Into<IcmpErrorCode>;
}

impl IcmpIpExt for Ipv4 {
    type ErrorCode = Icmpv4ErrorCode;
}

impl IcmpIpExt for Ipv6 {
    type ErrorCode = Icmpv6ErrorCode;
}

/// An extension trait providing ICMP handler properties.
pub(crate) trait IcmpHandlerIpExt: Ip {
    type SourceAddress: Witness<Self::Addr>;
    type IcmpError;
}

impl IcmpHandlerIpExt for Ipv4 {
    type SourceAddress = SpecifiedAddr<Ipv4Addr>;
    type IcmpError = Icmpv4Error;
}

impl IcmpHandlerIpExt for Ipv6 {
    type SourceAddress = UnicastAddr<Ipv6Addr>;
    type IcmpError = Icmpv6ErrorKind;
}

/// A kind of ICMPv4 error.
pub(crate) enum Icmpv4ErrorKind {
    ParameterProblem {
        code: Icmpv4ParameterProblemCode,
        pointer: u8,
        fragment_type: Ipv4FragmentType,
    },
    TtlExpired {
        proto: Ipv4Proto,
        fragment_type: Ipv4FragmentType,
    },
    NetUnreachable {
        proto: Ipv4Proto,
        fragment_type: Ipv4FragmentType,
    },
    ProtocolUnreachable,
    PortUnreachable,
}

/// An ICMPv4 error.
pub(crate) struct Icmpv4Error {
    pub(super) kind: Icmpv4ErrorKind,
    pub(super) header_len: usize,
}

/// A kind of ICMPv6 error.
pub(crate) enum Icmpv6ErrorKind {
    ParameterProblem { code: Icmpv6ParameterProblemCode, pointer: u32, allow_dst_multicast: bool },
    TtlExpired { proto: Ipv6Proto, header_len: usize },
    NetUnreachable { proto: Ipv6Proto, header_len: usize },
    PacketTooBig { proto: Ipv6Proto, header_len: usize, mtu: Mtu },
    ProtocolUnreachable { header_len: usize },
    PortUnreachable,
}

/// The handler exposed by ICMP.
pub(crate) trait BufferIcmpHandler<I: IcmpHandlerIpExt, C, B: BufferMut>:
    DeviceIdContext<AnyDevice>
{
    /// Sends an error message in response to an incoming packet.
    ///
    /// `src_ip` and `dst_ip` are the source and destination addresses of the
    /// incoming packet.
    fn send_icmp_error_message(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        frame_dst: FrameDestination,
        src_ip: I::SourceAddress,
        dst_ip: SpecifiedAddr<I::Addr>,
        original_packet: B,
        error: I::IcmpError,
    );
}

impl<B: BufferMut, C: BufferIcmpNonSyncCtx<Ipv4, B>, SC: InnerBufferIcmpv4Context<C, B>>
    BufferIcmpHandler<Ipv4, C, B> for SC
{
    fn send_icmp_error_message(
        &mut self,
        ctx: &mut C,
        device: &SC::DeviceId,
        frame_dst: FrameDestination,
        src_ip: SpecifiedAddr<Ipv4Addr>,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        original_packet: B,
        Icmpv4Error { kind, header_len }: Icmpv4Error,
    ) {
        match kind {
            Icmpv4ErrorKind::ParameterProblem { code, pointer, fragment_type } => {
                send_icmpv4_parameter_problem(
                    self,
                    ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    code,
                    Icmpv4ParameterProblem::new(pointer),
                    original_packet,
                    header_len,
                    fragment_type,
                )
            }
            Icmpv4ErrorKind::TtlExpired { proto, fragment_type } => send_icmpv4_ttl_expired(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                original_packet,
                header_len,
                fragment_type,
            ),
            Icmpv4ErrorKind::NetUnreachable { proto, fragment_type } => {
                send_icmpv4_net_unreachable(
                    self,
                    ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    proto,
                    original_packet,
                    header_len,
                    fragment_type,
                )
            }
            Icmpv4ErrorKind::ProtocolUnreachable => send_icmpv4_protocol_unreachable(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                original_packet,
                header_len,
            ),
            Icmpv4ErrorKind::PortUnreachable => send_icmpv4_port_unreachable(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                original_packet,
                header_len,
            ),
        }
    }
}

impl<B: BufferMut, C: BufferIcmpNonSyncCtx<Ipv6, B>, SC: InnerBufferIcmpv6Context<C, B>>
    BufferIcmpHandler<Ipv6, C, B> for SC
{
    fn send_icmp_error_message(
        &mut self,
        ctx: &mut C,
        device: &SC::DeviceId,
        frame_dst: FrameDestination,
        src_ip: UnicastAddr<Ipv6Addr>,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        original_packet: B,
        error: Icmpv6ErrorKind,
    ) {
        match error {
            Icmpv6ErrorKind::ParameterProblem { code, pointer, allow_dst_multicast } => {
                send_icmpv6_parameter_problem(
                    self,
                    ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    code,
                    Icmpv6ParameterProblem::new(pointer),
                    original_packet,
                    allow_dst_multicast,
                )
            }
            Icmpv6ErrorKind::TtlExpired { proto, header_len } => send_icmpv6_ttl_expired(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                original_packet,
                header_len,
            ),
            Icmpv6ErrorKind::NetUnreachable { proto, header_len } => send_icmpv6_net_unreachable(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                original_packet,
                header_len,
            ),
            Icmpv6ErrorKind::PacketTooBig { proto, header_len, mtu } => send_icmpv6_packet_too_big(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                proto,
                mtu,
                original_packet,
                header_len,
            ),
            Icmpv6ErrorKind::ProtocolUnreachable { header_len } => {
                send_icmpv6_protocol_unreachable(
                    self,
                    ctx,
                    device,
                    frame_dst,
                    src_ip,
                    dst_ip,
                    original_packet,
                    header_len,
                )
            }
            Icmpv6ErrorKind::PortUnreachable => send_icmpv6_port_unreachable(
                self,
                ctx,
                device,
                frame_dst,
                src_ip,
                dst_ip,
                original_packet,
            ),
        }
    }
}

/// The context required by the ICMP layer in order to deliver events related to
/// ICMP sockets.
pub trait IcmpContext<I: IcmpIpExt> {
    /// Receives an ICMP error message related to a previously-sent ICMP echo
    /// request.
    ///
    /// `seq_num` is the sequence number of the original echo request that
    /// triggered the error, and `err` is the specific error identified by the
    /// incoming ICMP error message.
    fn receive_icmp_error(&mut self, conn: SocketId<I>, seq_num: u16, err: I::ErrorCode);
}

/// The context required by the ICMP layer in order to deliver packets on ICMP
/// sockets.
pub trait BufferIcmpContext<I: IcmpIpExt, B: BufferMut>: IcmpContext<I> {
    /// Receives an ICMP echo reply.
    fn receive_icmp_echo_reply(
        &mut self,
        conn: SocketId<I>,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        id: u16,
        seq_num: u16,
        data: B,
    );
}

/// The non-synchronized execution context shared by both ICMPv4 and ICMPv6.
pub(crate) trait IcmpNonSyncCtx<I: IcmpIpExt>:
    InstantContext + IcmpContext<I> + CounterContext + RngContext
{
}
impl<I: IcmpIpExt, C: InstantContext + IcmpContext<I> + CounterContext + RngContext>
    IcmpNonSyncCtx<I> for C
{
}

/// Empty trait to work around coherence issues.
///
/// This serves only to convince the coherence checker that a particular blanket
/// trait implementation could only possibly conflict with other blanket impls
/// in this crate. It can be safely implemented for any type.
/// TODO(https://github.com/rust-lang/rust/issues/97811): Remove this once the
/// coherence checker doesn't require it.
pub(crate) trait IcmpStateContext {}

/// The execution context shared by ICMP(v4) and ICMPv6 for the internal
/// operations of the IP stack.
///
/// Unlike [`IcmpContext`], `InnerIcmpContext` is not exposed outside of this
/// crate.
pub(crate) trait InnerIcmpContext<I: IcmpIpExt + IpExt, C: IcmpNonSyncCtx<I>>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback provided to methods.
    type IpSocketsCtx<'a>: TransportIpContext<I, C>
        + MulticastMembershipHandler<I, C>
        + DeviceIdContext<AnyDevice, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>;
    // TODO(joshlf): If we end up needing to respond to these messages with new
    // outbound packets, then perhaps it'd be worth passing the original buffer
    // so that it can be reused?
    //
    // NOTE(joshlf): We don't guarantee the packet body length here for two
    // reasons:
    // - It's possible that some IPv4 protocol does or will exist for which
    //   valid packets are less than 8 bytes in length. If we were to reject all
    //   packets with bodies of less than 8 bytes, we might silently discard
    //   legitimate error messages for such protocols.
    // - Even if we were to guarantee this, there's no good way to encode such a
    //   guarantee in the type system, and so the caller would have no recourse
    //   but to panic, and panics have a habit of becoming bugs or DoS
    //   vulnerabilities when invariants change.

    /// Receives an ICMP error message and demultiplexes it to a transport layer
    /// protocol.
    ///
    /// All arguments beginning with `original_` are fields from the IP packet
    /// that triggered the error. The `original_body` is provided here so that
    /// the error can be associated with a transport-layer socket. `device`
    /// identifies the device on which the packet was received.
    ///
    /// While ICMPv4 error messages are supposed to contain the first 8 bytes of
    /// the body of the offending packet, and ICMPv6 error messages are supposed
    /// to contain as much of the offending packet as possible without violating
    /// the IPv6 minimum MTU, the caller does NOT guarantee that either of these
    /// hold. It is `receive_icmp_error`'s responsibility to handle any length
    /// of `original_body`, and to perform any necessary validation.
    fn receive_icmp_error(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        original_proto: I::Proto,
        original_body: &[u8],
        err: I::ErrorCode,
    );

    /// Calls the function with an immutable reference to ICMP sockets.
    fn with_icmp_sockets<O, F: FnOnce(&IcmpSockets<I, Self::WeakDeviceId>) -> O>(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to `IpSocketsCtx` and
    /// a mutable reference to ICMP sockets.
    fn with_icmp_ctx_and_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut IcmpSockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to ICMP error send tocket
    /// bucket.
    fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<C::Instant>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// Uninstantiatable type for implementing [`DatagramSocketSpec`].
enum Icmp {}

impl DatagramSocketSpec for Icmp {
    type AddrSpec = IcmpAddrSpec;

    type SocketId<I: datagram::IpExt> = SocketId<I>;

    type OtherStackIpOptions<I: datagram::IpExt> = ();

    type ListenerSharingState = ();

    type SocketMapSpec<I: datagram::IpExt + datagram::DualStackIpExt, D: WeakId> = (Self, I, D);

    type UnboundSharingState<I: datagram::IpExt> = ();

    fn make_receiving_map_id<I: datagram::IpExt, D: WeakId>(
        s: Self::SocketId<I>,
    ) -> <Self::SocketMapSpec<I, D> as datagram::DatagramSocketMapSpec<
        I,
        D,
        Self::AddrSpec,
    >>::ReceivingId{
        s
    }

    type Serializer<I: datagram::IpExt, B: BufferMut> =
        packet::Nested<B, IcmpPacketBuilder<I, IcmpEchoRequest>>;

    fn make_packet<I: datagram::IpExt, B: BufferMut>(
        mut body: B,
        addr: &socket::address::ConnIpAddr<
            I::Addr,
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
            <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
        >,
    ) -> Self::Serializer<I, B> {
        let ConnIpAddr { local: (local_ip, id), remote: (remote_ip, ()) } = addr;
        // TODO(https://fxbug.dev/47321): Instead of panic, make this trait
        // method fallible so that the caller can return errors. This will
        // become necessary once we use the datagram module for sending.
        let icmp_echo: packet_formats::icmp::IcmpPacketRaw<I, &[u8], IcmpEchoRequest> =
            body.parse().unwrap();
        let icmp_builder = IcmpPacketBuilder::<I, _>::new(
            local_ip.addr(),
            remote_ip.addr(),
            packet_formats::icmp::IcmpUnusedCode,
            IcmpEchoRequest::new(id.get(), icmp_echo.message().seq()),
        );
        body.encapsulate(icmp_builder)
    }

    fn try_alloc_listen_identifier<I: datagram::IpExt, D: WeakId>(
        ctx: &mut impl RngContext,
        is_available: impl Fn(
            <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        ) -> Result<(), datagram::InUseError>,
    ) -> Option<<Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier> {
        let mut port = IcmpBoundSockets::<I, D>::rand_ephemeral(&mut ctx.rng());
        for _ in IcmpBoundSockets::<I, D>::EPHEMERAL_RANGE {
            // We can unwrap here because we know that the EPHEMERAL_RANGE doesn't
            // include 0.
            let tryport = NonZeroU16::new(port.get()).unwrap();
            match is_available(tryport) {
                Ok(()) => return Some(tryport),
                Err(datagram::InUseError {}) => port.next(),
            }
        }
        None
    }

    type ListenerIpAddr<I: datagram::IpExt> = socket::address::ListenerIpAddr<I::Addr, NonZeroU16>;

    type ConnIpAddr<I: datagram::IpExt> = ConnIpAddr<
        I::Addr,
        <Self::AddrSpec as SocketMapAddrSpec>::LocalIdentifier,
        <Self::AddrSpec as SocketMapAddrSpec>::RemoteIdentifier,
    >;

    type ConnState<I: datagram::IpExt, D: Debug + Eq + core::hash::Hash> =
        datagram::ConnState<I, D, Self, datagram::IpOptions<I, D, Self>>;

    fn conn_addr_from_state<I: datagram::IpExt, D: Clone + Debug + Eq + core::hash::Hash>(
        state: &Self::ConnState<I, D>,
    ) -> ConnAddr<Self::ConnIpAddr<I>, D> {
        let datagram::ConnState { shutdown: _, socket: _, addr, clear_device_on_disconnect: _ } =
            state;
        addr.clone()
    }
}

/// Uninstantiatable type for implementing [`SocketMapAddrSpec`].
enum IcmpAddrSpec {}

impl SocketMapAddrSpec for IcmpAddrSpec {
    type RemoteIdentifier = ();
    type LocalIdentifier = NonZeroU16;
}

type IcmpBoundSockets<I, D> = datagram::BoundSockets<I, D, IcmpAddrSpec, (Icmp, I, D)>;

impl<I: IpExt, D: WeakId> PortAllocImpl for IcmpBoundSockets<I, D> {
    const EPHEMERAL_RANGE: core::ops::RangeInclusive<u16> = 1..=u16::MAX;
    type Id = DatagramFlowId<I::Addr, ()>;

    fn is_port_available(&self, id: &Self::Id, port: u16) -> bool {
        // We can safely unwrap here, because the ports received in
        // `is_port_available` are guaranteed to be in `EPHEMERAL_RANGE`.
        let port = NonZeroU16::new(port).unwrap();
        let conn = ConnAddr {
            ip: ConnIpAddr { local: (id.local_ip, port), remote: (id.remote_ip, ()) },
            device: None,
        };

        // A port is free if there are no sockets currently using it, and if
        // there are no sockets that are shadowing it.
        AddrVec::from(conn).iter_shadows().all(|a| match &a {
            AddrVec::Listen(l) => self.listeners().get_by_addr(&l).is_none(),
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).is_none(),
        } && self.get_shadower_counts(&a) == 0)
    }
}

type LocalIdAllocator<I, D> = Option<PortAlloc<IcmpBoundSockets<I, D>>>;

impl<I: IpExt, D: WeakId, C: RngContext>
    datagram::LocalIdentifierAllocator<I, D, IcmpAddrSpec, C, (Icmp, I, D)>
    for LocalIdAllocator<I, D>
{
    fn try_alloc_local_id(
        &mut self,
        bound: &socket::BoundSocketMap<I, D, IcmpAddrSpec, (Icmp, I, D)>,
        ctx: &mut C,
        flow: datagram::DatagramFlowId<I::Addr, ()>,
    ) -> Option<NonZeroU16> {
        let mut rng = ctx.rng();
        // Lazily init port_alloc if it hasn't been inited yet.
        let port_alloc = self.get_or_insert_with(|| PortAlloc::new(&mut rng));
        port_alloc.try_alloc(&flow, bound).and_then(NonZeroU16::new)
    }
}

impl<I, C, SC> datagram::NonDualStackDatagramBoundStateContext<I, C, Icmp> for SC
where
    I: IpExt + datagram::DualStackIpExt,
    C: IcmpNonSyncCtx<I>,
    SC: InnerIcmpContext<I, C> + IcmpStateContext,
{
    type Converter = ();
    fn converter(&self) -> Self::Converter {
        ()
    }
}

impl<I, C, SC> DatagramBoundStateContext<I, C, Icmp> for SC
where
    I: IpExt + datagram::DualStackIpExt,
    C: IcmpNonSyncCtx<I>,
    SC: InnerIcmpContext<I, C> + IcmpStateContext,
    for<'a> SC::IpSocketsCtx<'a>: TransportIpContext<I::OtherVersion, C>,
{
    type IpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;

    // ICMP sockets doesn't support dual-stack operations.
    type DualStackContext = datagram::UninstantiableContext<I, Icmp, Self>;

    type NonDualStackContext = Self;

    type LocalIdAllocator = LocalIdAllocator<I, Self::WeakDeviceId>;

    fn with_bound_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &IcmpBoundSockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(self, |ctx, sockets| {
            cb(ctx, &sockets.bound)
        })
    }

    fn with_bound_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut IcmpBoundSockets<I, Self::WeakDeviceId>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(self, |ctx, sockets| {
            cb(ctx, &mut sockets.bound, &mut sockets.id_allocator)
        })
    }

    fn dual_stack_context(
        &mut self,
    ) -> datagram::MaybeDualStack<&mut Self::DualStackContext, &mut Self::NonDualStackContext> {
        datagram::MaybeDualStack::NotDualStack(self)
    }

    fn with_transport_context<O, F: FnOnce(&mut Self::IpSocketsCtx<'_>) -> O>(
        &mut self,
        cb: F,
    ) -> O {
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(self, |ctx, _sockets| cb(ctx))
    }
}

impl<I: IpExt, D: Id> SocketMapStateSpec for (Icmp, I, D) {
    type ListenerId = SocketId<I>;
    type ConnId = SocketId<I>;

    type AddrVecTag = ();

    type ListenerSharingState = ();
    type ConnSharingState = ();

    type ListenerAddrState = Self::ListenerId;

    type ConnAddrState = Self::ConnId;
    fn listener_tag(
        ListenerAddrInfo { has_device: _, specified_addr: _ }: ListenerAddrInfo,
        _state: &Self::ListenerAddrState,
    ) -> Self::AddrVecTag {
        ()
    }
    fn connected_tag(_has_device: bool, _state: &Self::ConnAddrState) -> Self::AddrVecTag {
        ()
    }
}

impl<I: IpExt> SocketMapAddrStateSpec for SocketId<I> {
    type Id = Self;

    type SharingState = ();

    type Inserter<'a> = core::convert::Infallible
    where
        Self: 'a;

    fn new(_new_sharing_state: &Self::SharingState, id: Self::Id) -> Self {
        id
    }

    fn contains_id(&self, id: &Self::Id) -> bool {
        self == id
    }

    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        _new_sharing_state: &'a Self::SharingState,
    ) -> Result<Self::Inserter<'b>, IncompatibleError> {
        Err(IncompatibleError)
    }

    fn could_insert(
        &self,
        _new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        Err(IncompatibleError)
    }

    fn remove_by_id(&mut self, _id: Self::Id) -> socket::RemoveResult {
        socket::RemoveResult::IsLast
    }
}

impl<I: IpExt, D: WeakId> DatagramSocketMapSpec<I, D, IcmpAddrSpec> for (Icmp, I, D) {
    type ReceivingId = SocketId<I>;
}

impl<AA, I: IpExt, D: WeakId> SocketMapConflictPolicy<AA, (), I, D, IcmpAddrSpec> for (Icmp, I, D)
where
    AA: Into<AddrVec<I, D, IcmpAddrSpec>> + Clone,
{
    fn check_insert_conflicts(
        _new_sharing_state: &(),
        addr: &AA,
        socketmap: &crate::data_structures::socketmap::SocketMap<
            AddrVec<I, D, IcmpAddrSpec>,
            socket::Bound<Self>,
        >,
    ) -> Result<(), socket::InsertError> {
        let addr: AddrVec<_, _, _> = addr.clone().into();
        // Having a value present at a shadowed address is disqualifying.
        if addr.iter_shadows().any(|a| socketmap.get(&a).is_some()) {
            return Err(InsertError::ShadowAddrExists);
        }

        // Likewise, the presence of a value that shadows the target address is
        // also disqualifying.
        if socketmap.descendant_counts(&addr).len() != 0 {
            return Err(InsertError::ShadowerExists);
        }
        Ok(())
    }
}

impl<I: Ip> From<usize> for SocketId<I> {
    fn from(value: usize) -> Self {
        Self(value, IpVersionMarker::default())
    }
}

impl<I: Ip> EntryKey for SocketId<I> {
    fn get_key_index(&self) -> usize {
        let Self(index, _marker) = self;
        *index
    }
}
/// The non-synchronized execution context shared by both ICMPv4 and ICMPv6,
/// with a buffer.
pub(crate) trait BufferIcmpNonSyncCtx<I: IcmpIpExt, B: BufferMut>:
    IcmpNonSyncCtx<I> + BufferIcmpContext<I, B>
{
}
impl<I: IcmpIpExt, B: BufferMut, C: IcmpNonSyncCtx<I> + BufferIcmpContext<I, B>>
    BufferIcmpNonSyncCtx<I, B> for C
{
}

/// The execution context shared by ICMP(v4) and ICMPv6 for the internal
/// operations of the IP stack when a buffer is required.
pub(crate) trait InnerBufferIcmpContext<
    I: IcmpIpExt + IpExt,
    C: BufferIcmpNonSyncCtx<I, B>,
    B: BufferMut,
>: InnerIcmpContext<I, C> + BufferIpSocketHandler<I, C, B>
{
}

impl<
        I: IcmpIpExt + IpExt,
        C: BufferIcmpNonSyncCtx<I, B>,
        B: BufferMut,
        SC: InnerIcmpContext<I, C> + BufferIpSocketHandler<I, C, B>,
    > InnerBufferIcmpContext<I, C, B> for SC
{
}

/// The execution context for ICMPv4.
///
/// `InnerIcmpv4Context` is a shorthand for a larger collection of traits.
pub(crate) trait InnerIcmpv4Context<C: IcmpNonSyncCtx<Ipv4>>:
    InnerIcmpContext<Ipv4, C>
{
    /// Returns true if a timestamp reply may be sent.
    fn should_send_timestamp_reply(&self) -> bool;
}

impl<C: NonSyncContext> UnlockedAccess<crate::lock_ordering::IcmpSendTimestampReply<Ipv4>>
    for SyncCtx<C>
{
    type Data = bool;
    type Guard<'l> = &'l bool where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.state.ipv4.icmp.send_timestamp_reply
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IcmpSockets<Ipv4>>>
    InnerIcmpv4Context<C> for Locked<&SyncCtx<C>, L>
{
    fn should_send_timestamp_reply(&self) -> bool {
        *self.unlocked_access::<crate::lock_ordering::IcmpSendTimestampReply<Ipv4>>()
    }
}

/// The execution context for ICMPv4 where a buffer is required.
///
/// `InnerBufferIcmpv4Context` is a shorthand for a larger collection of traits.
pub(crate) trait InnerBufferIcmpv4Context<C: BufferIcmpNonSyncCtx<Ipv4, B>, B: BufferMut>:
    InnerIcmpv4Context<C> + InnerBufferIcmpContext<Ipv4, C, B>
{
}

impl<
        B: BufferMut,
        C: BufferIcmpNonSyncCtx<Ipv4, B>,
        SC: InnerIcmpv4Context<C> + InnerBufferIcmpContext<Ipv4, C, B>,
    > InnerBufferIcmpv4Context<C, B> for SC
{
}

/// The execution context for ICMPv6.
///
/// `InnerIcmpv6Context` is a shorthand for a larger collection of traits.
pub(crate) trait InnerIcmpv6Context<C: IcmpNonSyncCtx<Ipv6>>:
    InnerIcmpContext<Ipv6, C>
{
}

impl<C: IcmpNonSyncCtx<Ipv6>, SC: InnerIcmpContext<Ipv6, C>> InnerIcmpv6Context<C> for SC {}

/// The execution context for ICMPv6 where a buffer is required.
///
/// `InnerBufferIcmpv6Context` is a shorthand for a larger collection of traits.
pub(crate) trait InnerBufferIcmpv6Context<C: BufferIcmpNonSyncCtx<Ipv6, B>, B: BufferMut>:
    InnerIcmpv6Context<C> + InnerBufferIcmpContext<Ipv6, C, B>
{
}

impl<
        B: BufferMut,
        C: BufferIcmpNonSyncCtx<Ipv6, B>,
        SC: InnerIcmpv6Context<C> + InnerBufferIcmpContext<Ipv6, C, B>,
    > InnerBufferIcmpv6Context<C, B> for SC
{
}

/// Attempt to send an ICMP or ICMPv6 error message, applying a rate limit.
///
/// `try_send_error!($sync_ctx, $ctx, $e)` attempts to consume a token from the
/// token bucket at `$sync_ctx.get_state_mut().error_send_bucket`. If it
/// succeeds, it invokes the expression `$e`, and otherwise does nothing. It
/// assumes that the type of `$e` is `Result<(), _>` and, in the case that the
/// rate limit is exceeded and it does not invoke `$e`, returns `Ok(())`.
///
/// [RFC 4443 Section 2.4] (f) requires that we MUST limit the rate of outbound
/// ICMPv6 error messages. To our knowledge, there is no similar requirement for
/// ICMPv4, but the same rationale applies, so we do it for ICMPv4 as well.
///
/// [RFC 4443 Section 2.4]: https://tools.ietf.org/html/rfc4443#section-2.4
macro_rules! try_send_error {
    ($sync_ctx:expr, $ctx:expr, $e:expr, $counter_name:ident) => {{
        // TODO(joshlf): Figure out a way to avoid querying for the current time
        // unconditionally. See the documentation on the `CachedInstantCtx` type
        // for more information.
        let instant_ctx = crate::context::new_cached_instant_context($ctx);
        let send = $sync_ctx.with_error_send_bucket_mut(|error_send_bucket| {
            error_send_bucket.try_take(&instant_ctx)
        });

        if send {
            $ctx.increment_debug_counter($counter_name);
            $e
        } else {
            trace!("ip::icmp::try_send_error!: dropping rate-limited ICMP error message");
            Ok(())
        }
    }};
}

/// An implementation of [`IpTransportContext`] for ICMP.
pub(crate) enum IcmpIpTransportContext {}

impl<I: IcmpIpExt + IpExt, C: IcmpNonSyncCtx<I>, SC: InnerIcmpContext<I, C>>
    IpTransportContext<I, C, SC> for IcmpIpTransportContext
{
    fn receive_icmp_error(
        sync_ctx: &mut SC,
        ctx: &mut C,
        _device: &SC::DeviceId,
        original_src_ip: Option<SpecifiedAddr<I::Addr>>,
        original_dst_ip: SpecifiedAddr<I::Addr>,
        mut original_body: &[u8],
        err: I::ErrorCode,
    ) {
        ctx.increment_debug_counter("IcmpIpTransportContext::receive_icmp_error");
        trace!("IcmpIpTransportContext::receive_icmp_error({:?})", err);

        let echo_request = if let Ok(echo_request) =
            original_body.parse::<IcmpPacketRaw<I, _, IcmpEchoRequest>>()
        {
            echo_request
        } else {
            // NOTE: This might just mean that the error message was in response
            // to a packet that we sent that wasn't an echo request, so we just
            // silently ignore it.
            return;
        };

        let original_src_ip = match original_src_ip {
            Some(ip) => ip,
            None => {
                trace!("IcmpIpTransportContext::receive_icmp_error: unspecified source IP address");
                return;
            }
        };
        let original_src_ip: SocketIpAddr<_> = match original_src_ip.try_into() {
            Ok(ip) => ip,
            Err(AddrIsMappedError {}) => {
                trace!("IcmpIpTransportContext::receive_icmp_error: mapped source IP address");
                return;
            }
        };
        let original_dst_ip: SocketIpAddr<_> = match original_dst_ip.try_into() {
            Ok(ip) => ip,
            Err(AddrIsMappedError {}) => {
                trace!("IcmpIpTransportContext::receive_icmp_error: mapped destination IP address");
                return;
            }
        };

        let id = echo_request.message().id();
        sync_ctx.with_icmp_sockets(|sockets| {
            if let Some(conn) = sockets.bound.conns().get_by_addr(&ConnAddr {
                ip: ConnIpAddr {
                    local: (original_src_ip, NonZeroU16::new(id).unwrap()),
                    remote: (original_dst_ip, ())
                },
                device: None,
            }) {
                let seq = echo_request.message().seq();
                ctx.increment_debug_counter("IcmpContext::receive_icmp_error");
                IcmpContext::receive_icmp_error(ctx, *conn, seq, err);
            } else {
                trace!("IcmpIpTransportContext::receive_icmp_error: Got ICMP error message for nonexistent ICMP echo socket; either the socket responsible has since been removed, or the error message was sent in error or corrupted");
            }
        })
    }
}

impl<
        B: BufferMut,
        C: BufferIcmpNonSyncCtx<Ipv4, B>,
        SC: InnerBufferIcmpv4Context<C, B> + PmtuHandler<Ipv4, C>,
    > BufferIpTransportContext<Ipv4, C, SC, B> for IcmpIpTransportContext
{
    fn receive_ip_packet(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        trace!(
            "<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet({}, {})",
            src_ip,
            dst_ip
        );
        let packet =
            match buffer.parse_with::<_, Icmpv4Packet<_>>(IcmpParseArgs::new(src_ip, dst_ip)) {
                Ok(packet) => packet,
                Err(_) => return Ok(()), // TODO(joshlf): Do something else here?
            };

        match packet {
            Icmpv4Packet::EchoRequest(echo_request) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::echo_request");

                if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
                    let req = *echo_request.message();
                    let code = echo_request.code();
                    let (local_ip, remote_ip) = (dst_ip, src_ip);
                    // TODO(joshlf): Do something if send_icmp_reply returns an
                    // error?
                    let _ = send_icmp_reply(sync_ctx, ctx,Some(device), remote_ip, local_ip, |src_ip| {
                        buffer.encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                            src_ip,
                            remote_ip,
                            code,
                            req.reply(),
                        ))
                    });
                } else {
                    trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received echo request with an unspecified source address");
                }
            }
            Icmpv4Packet::EchoReply(echo_reply) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::echo_reply");
                trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received an EchoReply message");
                let id = echo_reply.message().id();
                let seq = echo_reply.message().seq();
                receive_icmp_echo_reply(sync_ctx,ctx, src_ip, dst_ip, id, seq, buffer);
            }
            Icmpv4Packet::TimestampRequest(timestamp_request) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::timestamp_request");
                if let Some(src_ip) = SpecifiedAddr::new(src_ip) {
                    if sync_ctx.should_send_timestamp_reply() {
                        trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Responding to Timestamp Request message");
                        // We're supposed to respond with the time that we
                        // processed this message as measured in milliseconds
                        // since midnight UT. However, that would require that
                        // we knew the local time zone and had a way to convert
                        // `InstantContext::Instant` to a `u32` value. We can't
                        // do that, and probably don't want to introduce all of
                        // the machinery necessary just to support this one use
                        // case. Luckily, RFC 792 page 17 provides us with an
                        // out:
                        //
                        //   If the time is not available in miliseconds [sic]
                        //   or cannot be provided with respect to midnight UT
                        //   then any time can be inserted in a timestamp
                        //   provided the high order bit of the timestamp is
                        //   also set to indicate this non-standard value.
                        //
                        // Thus, we provide a zero timestamp with the high order
                        // bit set.
                        const NOW: u32 = 0x80000000;
                        let reply = timestamp_request.message().reply(NOW, NOW);
                        let (local_ip, remote_ip) = (dst_ip, src_ip);
                        // We don't actually want to use any of the _contents_
                        // of the buffer, but we would like to reuse it as
                        // scratch space. Eventually, `IcmpPacketBuilder` will
                        // implement `InnerPacketBuilder` for messages without
                        // bodies, but until that happens, we need to give it an
                        // empty buffer.
                        buffer.shrink_front_to(0);
                        // TODO(joshlf): Do something if send_icmp_reply returns
                        // an error?
                        let _ = send_icmp_reply(sync_ctx, ctx, Some(device), remote_ip, local_ip, |src_ip| {
                            buffer.encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                                src_ip,
                                remote_ip,
                                IcmpUnusedCode,
                                reply,
                            ))
                        });
                    } else {
                        trace!(
                            "<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Silently ignoring Timestamp Request message"
                        );
                    }
                } else {
                    trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received timestamp request with an unspecified source address");
                }
            }
            Icmpv4Packet::TimestampReply(_) => {
                // TODO(joshlf): Support sending Timestamp Requests and
                // receiving Timestamp Replies?
                debug!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received unsolicited Timestamp Reply message");
            }
            Icmpv4Packet::DestUnreachable(dest_unreachable) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::dest_unreachable");
                trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received a Destination Unreachable message");

                if dest_unreachable.code() == Icmpv4DestUnreachableCode::FragmentationRequired {
                    if let Some(next_hop_mtu) = dest_unreachable.message().next_hop_mtu() {
                        // We are updating the path MTU from the destination
                        // address of this `packet` (which is an IP address on
                        // this node) to some remote (identified by the source
                        // address of this `packet`).
                        //
                        // `update_pmtu_if_less` may return an error, but it
                        // will only happen if the Dest Unreachable message's
                        // MTU field had a value that was less than the IPv4
                        // minimum MTU (which as per IPv4 RFC 791, must not
                        // happen).
                        sync_ctx.update_pmtu_if_less(
                            ctx,
                            dst_ip.get(),
                            src_ip,
                            Mtu::new(u32::from(next_hop_mtu.get())),
                        );
                    } else {
                        // If the Next-Hop MTU from an incoming ICMP message is
                        // `0`, then we assume the source node of the ICMP
                        // message does not implement RFC 1191 and therefore
                        // does not actually use the Next-Hop MTU field and
                        // still considers it as an unused field.
                        //
                        // In this case, the only information we have is the
                        // size of the original IP packet that was too big (the
                        // original packet header should be included in the ICMP
                        // response). Here we will simply reduce our PMTU
                        // estimate to a value less than the total length of the
                        // original packet. See RFC 1191 Section 5.
                        //
                        // `update_pmtu_next_lower` may return an error, but it
                        // will only happen if no valid lower value exists from
                        // the original packet's length. It is safe to silently
                        // ignore the error when we have no valid lower PMTU
                        // value as the node from `src_ip` would not be IP RFC
                        // compliant and we expect this to be very rare (for
                        // IPv4, the lowest MTU value for a link can be 68
                        // bytes).
                        let original_packet_buf = dest_unreachable.body().bytes();
                        if original_packet_buf.len() >= 4 {
                            // We need the first 4 bytes as the total length
                            // field is at bytes 2/3 of the original packet
                            // buffer.
                            let total_len = u16::from_be_bytes(original_packet_buf[2..4].try_into().unwrap());

                            trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Next-Hop MTU is 0 so using the next best PMTU value from {}", total_len);

                            sync_ctx.update_pmtu_next_lower(ctx, dst_ip.get(), src_ip, Mtu::new(u32::from(total_len)));
                        } else {
                            // Ok to silently ignore as RFC 792 requires nodes
                            // to send the original IP packet header + 64 bytes
                            // of the original IP packet's body so the node
                            // itself is already violating the RFC.
                            trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Original packet buf is too small to get original packet len so ignoring");
                        }
                    }
                }

                receive_icmpv4_error(
                    sync_ctx,
                    ctx,
                    device,
                    &dest_unreachable,
                    Icmpv4ErrorCode::DestUnreachable(dest_unreachable.code()),
                );
            }
            Icmpv4Packet::TimeExceeded(time_exceeded) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::time_exceeded");
                trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received a Time Exceeded message");

                receive_icmpv4_error(
                    sync_ctx,
                    ctx,
                    device,
                    &time_exceeded,
                    Icmpv4ErrorCode::TimeExceeded(time_exceeded.code()),
                );
            }
            Icmpv4Packet::Redirect(_) => log_unimplemented!((), "<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::redirect"),
            Icmpv4Packet::ParameterProblem(parameter_problem) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::parameter_problem");
                trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet: Received a Parameter Problem message");

                receive_icmpv4_error(
                    sync_ctx,
                    ctx,
                    device,
                    &parameter_problem,
                    Icmpv4ErrorCode::ParameterProblem(parameter_problem.code()),
                );
            }
        }

        Ok(())
    }
}

pub(crate) fn send_ndp_packet<
    C,
    SC: BufferIpLayerHandler<Ipv6, C, EmptyBuf>,
    S: Serializer<Buffer = EmptyBuf>,
    M: IcmpMessage<Ipv6>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    src_ip: Option<SpecifiedAddr<Ipv6Addr>>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    body: S,
    code: M::Code,
    message: M,
) -> Result<(), S> {
    // TODO(https://fxbug.dev/95359): Send through ICMPv6 send path.
    BufferIpLayerHandler::<Ipv6, _, _>::send_ip_packet_from_device(
        sync_ctx,
        ctx,
        SendIpPacketMeta {
            device: device_id,
            src_ip,
            dst_ip,
            next_hop: dst_ip,
            ttl: NonZeroU8::new(REQUIRED_NDP_IP_PACKET_HOP_LIMIT),
            proto: Ipv6Proto::Icmpv6,
            mtu: None,
        },
        body.encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
            src_ip.map_or(Ipv6::UNSPECIFIED_ADDRESS, |a| a.get()),
            dst_ip.get(),
            code,
            message,
        )),
    )
    .map_err(|s| s.into_inner())
}

fn send_neighbor_advertisement<
    C,
    SC: Ipv6DeviceHandler<C> + IpDeviceHandler<Ipv6, C> + BufferIpLayerHandler<Ipv6, C, EmptyBuf>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    solicited: bool,
    device_addr: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
) {
    debug!("send_neighbor_advertisement from {:?} to {:?}", device_addr, dst_ip);
    // We currently only allow the destination address to be:
    // 1) a unicast address.
    // 2) a multicast destination but the message should be an unsolicited
    //    neighbor advertisement.
    // NOTE: this assertion may need change if more messages are to be allowed
    // in the future.
    debug_assert!(dst_ip.is_valid_unicast() || (!solicited && dst_ip.is_multicast()));

    // We must call into the higher level send_ip_packet_from_device function
    // because it is not guaranteed that we actually know the link-layer
    // address of the destination IP. Typically, the solicitation request will
    // carry that information, but it is not necessary. So it is perfectly valid
    // that trying to send this advertisement will end up triggering a neighbor
    // solicitation to be sent.
    let src_ll = sync_ctx.get_link_layer_addr_bytes(&device_id);

    // Nothing reasonable to do with the error.
    let advertisement = NeighborAdvertisement::new(
        sync_ctx.is_router_device(&device_id),
        solicited,
        false,
        device_addr.get(),
    );
    let _: Result<(), _> = send_ndp_packet(
        sync_ctx,
        ctx,
        &device_id,
        Some(device_addr.into_specified()),
        dst_ip,
        OptionSequenceBuilder::new(
            src_ll.as_ref().map(AsRef::as_ref).map(NdpOptionBuilder::TargetLinkLayerAddress).iter(),
        )
        .into_serializer(),
        IcmpUnusedCode,
        advertisement,
    );
}

fn receive_ndp_packet<
    B: ByteSlice,
    C: IcmpNonSyncCtx<Ipv6>,
    SC: InnerIcmpv6Context<C>
        + Ipv6DeviceHandler<C>
        + IpDeviceHandler<Ipv6, C>
        + IpDeviceStateContext<Ipv6, C>
        + NudIpHandler<Ipv6, C>
        + BufferIpLayerHandler<Ipv6, C, EmptyBuf>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    src_ip: Ipv6SourceAddr,
    packet: NdpPacket<B>,
) {
    // TODO(https://fxbug.dev/97319): Make sure IP's hop limit is set to 255 as
    // per RFC 4861 section 6.1.2.

    match packet {
        NdpPacket::RouterSolicitation(_) | NdpPacket::Redirect(_) => {}
        NdpPacket::NeighborSolicitation(ref p) => {
            let target_address = p.message().target_address();
            let target_address = match UnicastAddr::new(*target_address) {
                Some(a) => a,
                None => {
                    trace!(
                        "dropping NS from {} with non-unicast target={:?}",
                        src_ip,
                        target_address
                    );
                    return;
                }
            };

            ctx.increment_debug_counter("ndp::rx_neighbor_solicitation");

            match src_ip {
                Ipv6SourceAddr::Unspecified => {
                    // The neighbor is performing Duplicate address detection.
                    //
                    // As per RFC 4861 section 4.3,
                    //
                    //   Source Address
                    //       Either an address assigned to the interface from
                    //       which this message is sent or (if Duplicate Address
                    //       Detection is in progress [ADDRCONF]) the
                    //       unspecified address.
                    match Ipv6DeviceHandler::remove_duplicate_tentative_address(
                        sync_ctx,
                        ctx,
                        &device_id,
                        target_address,
                    ) {
                        IpAddressState::Assigned => {
                            // Address is assigned to us to we let the
                            // remote node performing DAD that we own the
                            // address.
                            send_neighbor_advertisement(
                                sync_ctx,
                                ctx,
                                &device_id,
                                false,
                                target_address,
                                Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS.into_specified(),
                            );
                        }
                        IpAddressState::Tentative => {
                            // Nothing further to do in response to DAD
                            // messages.
                        }
                        IpAddressState::Unavailable => {
                            // Nothing further to do for unassigned target
                            // addresses.
                        }
                    }

                    return;
                }
                Ipv6SourceAddr::Unicast(src_ip) => {
                    // Neighbor is performing link address resolution.
                    match sync_ctx
                        .address_status_for_device(target_address.into_specified(), device_id)
                    {
                        AddressStatus::Present(Ipv6PresentAddressStatus::UnicastAssigned) => {}
                        AddressStatus::Present(
                            Ipv6PresentAddressStatus::UnicastTentative
                            | Ipv6PresentAddressStatus::Multicast,
                        )
                        | AddressStatus::Unassigned => {
                            // Address is not considered assigned to us as a
                            // unicast so don't send a neighbor advertisement
                            // reply.
                            return;
                        }
                    }

                    let link_addr = p.body().iter().find_map(|o| match o {
                        NdpOption::SourceLinkLayerAddress(a) => Some(a),
                        NdpOption::TargetLinkLayerAddress(_)
                        | NdpOption::PrefixInformation(_)
                        | NdpOption::RedirectedHeader { .. }
                        | NdpOption::RecursiveDnsServer(_)
                        | NdpOption::RouteInformation(_)
                        | NdpOption::Mtu(_) => None,
                    });

                    if let Some(link_addr) = link_addr {
                        NudIpHandler::handle_neighbor_probe(
                            sync_ctx,
                            ctx,
                            &device_id,
                            src_ip.into_specified(),
                            link_addr,
                        );
                    }

                    send_neighbor_advertisement(
                        sync_ctx,
                        ctx,
                        &device_id,
                        true,
                        target_address,
                        src_ip.into_specified(),
                    );
                }
            }
        }
        NdpPacket::NeighborAdvertisement(ref p) => {
            // TODO(https://fxbug.dev/97311): Invalidate discovered routers when
            // neighbor entry's IsRouter field transitions to false.

            let target_address = p.message().target_address();

            let src_ip = match src_ip {
                Ipv6SourceAddr::Unicast(src_ip) => src_ip,
                Ipv6SourceAddr::Unspecified => {
                    trace!("dropping NA with unspecified source and target = {:?}", target_address);
                    return;
                }
            };

            let target_address = match UnicastAddr::new(*target_address) {
                Some(a) => a,
                None => {
                    trace!(
                        "dropping NA from {} with non-unicast target={:?}",
                        src_ip,
                        target_address
                    );
                    return;
                }
            };

            ctx.increment_debug_counter("ndp::rx_neighbor_advertisement");

            match Ipv6DeviceHandler::remove_duplicate_tentative_address(
                sync_ctx,
                ctx,
                &device_id,
                target_address,
            ) {
                IpAddressState::Assigned => {
                    // A neighbor is advertising that it owns an address
                    // that we also have assigned. This is out of scope
                    // for DAD.
                    //
                    // As per RFC 4862 section 5.4.4,
                    //
                    //   2.  If the target address matches a unicast address
                    //       assigned to the receiving interface, it would
                    //       possibly indicate that the address is a
                    //       duplicate but it has not been detected by the
                    //       Duplicate Address Detection procedure (recall
                    //       that Duplicate Address Detection is not
                    //       completely reliable). How to handle such a case
                    //       is beyond the scope of this document.
                    //
                    // TODO(https://fxbug.dev/36238): Signal to bindings
                    // that a duplicate address is detected.
                    error!(
                        "NA from {src_ip} with target address {target_address} that is also \
                        assigned on device {device_id:?}",
                    );
                }
                IpAddressState::Tentative => {
                    // Nothing further to do for an NA from a neighbor that
                    // targets an address we also have assigned.
                    return;
                }
                IpAddressState::Unavailable => {
                    // Address not targeting us so we know its for a neighbor.
                    //
                    // TODO(https://fxbug.dev/99830): Move NUD to IP.
                }
            }

            let link_addr = p.body().iter().find_map(|o| match o {
                NdpOption::TargetLinkLayerAddress(a) => Some(a),
                NdpOption::SourceLinkLayerAddress(_)
                | NdpOption::PrefixInformation(_)
                | NdpOption::RedirectedHeader { .. }
                | NdpOption::RecursiveDnsServer(_)
                | NdpOption::RouteInformation(_)
                | NdpOption::Mtu(_) => None,
            });
            let link_addr = match link_addr {
                Some(a) => a,
                None => {
                    trace!(
                        "dropping NA from {} targetting {} with no TLL option",
                        src_ip,
                        target_address
                    );
                    return;
                }
            };

            NudIpHandler::handle_neighbor_confirmation(
                sync_ctx,
                ctx,
                &device_id,
                target_address.into_specified(),
                link_addr,
                ConfirmationFlags {
                    solicited_flag: p.message().solicited_flag(),
                    override_flag: p.message().override_flag(),
                },
            );
        }
        NdpPacket::RouterAdvertisement(ref p) => {
            // As per RFC 4861 section 6.1.2,
            //
            //   A node MUST silently discard any received Router Advertisement
            //   messages that do not satisfy all of the following validity
            //   checks:
            //
            //      - IP Source Address is a link-local address.  Routers must
            //        use their link-local address as the source for Router
            //        Advertisement and Redirect messages so that hosts can
            //        uniquely identify routers.
            //
            //        ...
            let src_ip = match src_ip {
                Ipv6SourceAddr::Unicast(ip) => match LinkLocalUnicastAddr::new(ip) {
                    Some(ip) => ip,
                    None => return,
                },
                Ipv6SourceAddr::Unspecified => return,
            };

            ctx.increment_debug_counter("ndp::rx_router_advertisement");

            let ra = p.message();

            // As per RFC 4861 section 6.3.4,
            //   The RetransTimer variable SHOULD be copied from the Retrans
            //   Timer field, if it is specified.
            //
            // TODO(https://fxbug.dev/101357): Control whether or not we should
            // update the retransmit timer.
            if let Some(retransmit_timer) = ra.retransmit_timer() {
                Ipv6DeviceHandler::set_discovered_retrans_timer(
                    sync_ctx,
                    ctx,
                    &device_id,
                    retransmit_timer,
                );
            }

            // As per RFC 4861 section 6.3.4:
            //   If the received Cur Hop Limit value is specified, the host
            //   SHOULD set its CurHopLimit variable to the received value.
            //
            // TODO(https://fxbug.dev/101357): Control whether or not we should
            // update the default hop limit.
            if let Some(hop_limit) = ra.current_hop_limit() {
                trace!("receive_ndp_packet: NDP RA: updating device's hop limit to {:?} for router: {:?}", ra.current_hop_limit(), src_ip);
                IpDeviceHandler::set_default_hop_limit(sync_ctx, &device_id, hop_limit);
            }

            // TODO(https://fxbug.dev/126654): Support default router preference.
            Ipv6DeviceHandler::update_discovered_ipv6_route(
                sync_ctx,
                ctx,
                &device_id,
                Ipv6DiscoveredRoute { subnet: IPV6_DEFAULT_SUBNET, gateway: Some(src_ip) },
                p.message().router_lifetime().map(NonZeroNdpLifetime::Finite),
            );

            for option in p.body().iter() {
                match option {
                    NdpOption::TargetLinkLayerAddress(_)
                    | NdpOption::RedirectedHeader { .. }
                    | NdpOption::RecursiveDnsServer(_) => {}
                    NdpOption::SourceLinkLayerAddress(addr) => {
                        // As per RFC 4861 section 6.3.4,
                        //
                        //   If the advertisement contains a Source Link-Layer
                        //   Address option, the link-layer address SHOULD be
                        //   recorded in the Neighbor Cache entry for the router
                        //   (creating an entry if necessary) and the IsRouter
                        //   flag in the Neighbor Cache entry MUST be set to
                        //   TRUE. If no Source Link-Layer Address is included,
                        //   but a corresponding Neighbor Cache entry exists,
                        //   its IsRouter flag MUST be set to TRUE. The IsRouter
                        //   flag is used by Neighbor Unreachability Detection
                        //   to determine when a router changes to being a host
                        //   (i.e., no longer capable of forwarding packets).
                        //   If a Neighbor Cache entry is created for the
                        //   router, its reachability state MUST be set to STALE
                        //   as specified in Section 7.3.3.  If a cache entry
                        //   already exists and is updated with a different
                        //   link-layer address, the reachability state MUST
                        //   also be set to STALE.if a Neighbor Cache entry
                        //
                        // We do not yet support NUD as described in RFC 4861
                        // so for now we just record the link-layer address in
                        // our neighbor table.
                        //
                        // TODO(https://fxbug.dev/133436): Add support for routers in NUD.
                        NudIpHandler::handle_neighbor_probe(
                            sync_ctx,
                            ctx,
                            &device_id,
                            {
                                let src_ip: UnicastAddr<_> = src_ip.into_addr();
                                src_ip.into_specified()
                            },
                            addr,
                        );
                    }
                    NdpOption::PrefixInformation(prefix_info) => {
                        // As per RFC 4861 section 6.3.4,
                        //
                        //   For each Prefix Information option with the on-link
                        //   flag set, a host does the following:
                        //
                        //      - If the prefix is the link-local prefix,
                        //        silently ignore the Prefix Information option.
                        //
                        // Also as per RFC 4862 section 5.5.3,
                        //
                        //   For each Prefix-Information option in the Router
                        //   Advertisement:
                        //
                        //    ..
                        //
                        //    b)  If the prefix is the link-local prefix,
                        //        silently ignore the Prefix Information option.
                        if prefix_info.prefix().is_link_local() {
                            continue;
                        }

                        let subnet = match prefix_info.subnet() {
                            Ok(subnet) => subnet,
                            Err(err) => match err {
                                SubnetError::PrefixTooLong | SubnetError::HostBitsSet => continue,
                            },
                        };

                        match UnicastAddr::new(subnet.network()) {
                            Some(UnicastAddr { .. }) => {}
                            None => continue,
                        }

                        let valid_lifetime = prefix_info.valid_lifetime();

                        if prefix_info.on_link_flag() {
                            // TODO(https://fxbug.dev/126654): Support route preference.
                            Ipv6DeviceHandler::update_discovered_ipv6_route(
                                sync_ctx,
                                ctx,
                                &device_id,
                                Ipv6DiscoveredRoute { subnet, gateway: None },
                                valid_lifetime,
                            )
                        }

                        if prefix_info.autonomous_address_configuration_flag() {
                            Ipv6DeviceHandler::apply_slaac_update(
                                sync_ctx,
                                ctx,
                                &device_id,
                                subnet,
                                prefix_info.preferred_lifetime(),
                                valid_lifetime,
                            );
                        }
                    }
                    NdpOption::RouteInformation(rio) => {
                        // TODO(https://fxbug.dev/126654): Support route preference.
                        Ipv6DeviceHandler::update_discovered_ipv6_route(
                            sync_ctx,
                            ctx,
                            &device_id,
                            Ipv6DiscoveredRoute {
                                subnet: rio.prefix().clone(),
                                gateway: Some(src_ip),
                            },
                            rio.route_lifetime(),
                        )
                    }
                    NdpOption::Mtu(mtu) => {
                        // TODO(https://fxbug.dev/101357): Control whether or
                        // not we should update the link's MTU in response to
                        // RAs.
                        Ipv6DeviceHandler::set_link_mtu(sync_ctx, &device_id, Mtu::new(mtu));
                    }
                }
            }
        }
    }
}

impl<
        B: BufferMut,
        C: BufferIcmpNonSyncCtx<Ipv6, B>,
        SC: InnerIcmpv6Context<C>
            + InnerBufferIcmpContext<Ipv6, C, B>
            + Ipv6DeviceHandler<C>
            + IpDeviceHandler<Ipv6, C>
            + IpDeviceStateContext<Ipv6, C>
            + PmtuHandler<Ipv6, C>
            + NudIpHandler<Ipv6, C>
            + BufferIpLayerHandler<Ipv6, C, EmptyBuf>,
    > BufferIpTransportContext<Ipv6, C, SC, B> for IcmpIpTransportContext
{
    fn receive_ip_packet(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        trace!(
            "<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet({:?}, {})",
            src_ip,
            dst_ip
        );

        let packet = match buffer
            .parse_with::<_, Icmpv6Packet<_>>(IcmpParseArgs::new(src_ip.get(), dst_ip))
        {
            Ok(packet) => packet,
            Err(_) => return Ok(()), // TODO(joshlf): Do something else here?
        };

        match packet {
            Icmpv6Packet::EchoRequest(echo_request) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet::echo_request");

                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    let req = *echo_request.message();
                    let code = echo_request.code();
                    let (local_ip, remote_ip) = (dst_ip, src_ip);
                    // TODO(joshlf): Do something if send_icmp_reply returns an
                    // error?
                    let _ = send_icmp_reply(
                        sync_ctx,
                        ctx,
                        Some(device),
                        remote_ip.into_specified(),
                        local_ip,
                        |src_ip| {
                            buffer.encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                                src_ip,
                                remote_ip,
                                code,
                                req.reply(),
                            ))
                        },
                    );
                } else {
                    trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet: Received echo request with an unspecified source address");
                }
            }
            Icmpv6Packet::EchoReply(echo_reply) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet::echo_reply");
                trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet: Received an EchoReply message");
                // We don't allow creating echo sockets connected to the
                // unspecified address, so it's OK to bail early here if the
                // source address is unspecified.
                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    let id = echo_reply.message().id();
                    let seq = echo_reply.message().seq();
                    receive_icmp_echo_reply(sync_ctx, ctx, src_ip.get(), dst_ip, id, seq, buffer);
                }
            }
            Icmpv6Packet::Ndp(packet) => receive_ndp_packet(sync_ctx, ctx, device, src_ip, packet),
            Icmpv6Packet::PacketTooBig(packet_too_big) => {
                ctx.increment_debug_counter("<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet::packet_too_big");
                trace!("<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet: Received a Packet Too Big message");
                if let Ipv6SourceAddr::Unicast(src_ip) = src_ip {
                    // We are updating the path MTU from the destination address
                    // of this `packet` (which is an IP address on this node) to
                    // some remote (identified by the source address of this
                    // `packet`).
                    //
                    // `update_pmtu_if_less` may return an error, but it will
                    // only happen if the Packet Too Big message's MTU field had
                    // a value that was less than the IPv6 minimum MTU (which as
                    // per IPv6 RFC 8200, must not happen).
                    sync_ctx.update_pmtu_if_less(
                        ctx,
                        dst_ip.get(),
                        src_ip.get(),
                        Mtu::new(packet_too_big.message().mtu()),
                    );
                }
                receive_icmpv6_error(
                    sync_ctx,
                    ctx,
                    device,
                    &packet_too_big,
                    Icmpv6ErrorCode::PacketTooBig,
                );
            }
            Icmpv6Packet::Mld(packet) => {
                sync_ctx.receive_mld_packet(ctx, &device, src_ip, dst_ip, packet);
            }
            Icmpv6Packet::DestUnreachable(dest_unreachable) => receive_icmpv6_error(
                sync_ctx,
                ctx,
                device,
                &dest_unreachable,
                Icmpv6ErrorCode::DestUnreachable(dest_unreachable.code()),
            ),
            Icmpv6Packet::TimeExceeded(time_exceeded) => receive_icmpv6_error(
                sync_ctx,
                ctx,
                device,
                &time_exceeded,
                Icmpv6ErrorCode::TimeExceeded(time_exceeded.code()),
            ),
            Icmpv6Packet::ParameterProblem(parameter_problem) => receive_icmpv6_error(
                sync_ctx,
                ctx,
                device,
                &parameter_problem,
                Icmpv6ErrorCode::ParameterProblem(parameter_problem.code()),
            ),
        }

        Ok(())
    }
}

/// Sends an ICMP reply to a remote host.
///
/// `send_icmp_reply` sends a reply to a non-error message (e.g., "echo request"
/// or "timestamp request" messages). It takes the ingress device, source IP,
/// and destination IP of the packet *being responded to*. It uses ICMP-specific
/// logic to figure out whether and how to send an ICMP reply.
///
/// `get_body_from_src_ip` returns a `Serializer` with the bytes of the ICMP
/// packet, and, when called, is given the source IP address chosen for the
/// outbound packet. This allows `get_body_from_src_ip` to properly compute the
/// ICMP checksum, which relies on both the source and destination IP addresses
/// of the IP packet it's encapsulated in.
fn send_icmp_reply<
    I: crate::ip::IpExt,
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<I, B>,
    SC: BufferIpSocketHandler<I, C, B> + DeviceIdContext<AnyDevice>,
    S: Serializer<Buffer = B>,
    F: FnOnce(SpecifiedAddr<I::Addr>) -> S,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: Option<&SC::DeviceId>,
    original_src_ip: SpecifiedAddr<I::Addr>,
    original_dst_ip: SpecifiedAddr<I::Addr>,
    get_body_from_src_ip: F,
) -> Result<(), S> {
    trace!("send_icmp_reply({:?}, {}, {})", device, original_src_ip, original_dst_ip);
    ctx.increment_debug_counter("send_icmp_reply");
    sync_ctx
        .send_oneshot_ip_packet(
            ctx,
            None,
            Some(original_dst_ip),
            original_src_ip,
            I::ICMP_IP_PROTO,
            DefaultSendOptions,
            get_body_from_src_ip,
            None,
        )
        .map_err(|(body, err, DefaultSendOptions {})| {
            error!("failed to send ICMP reply: {}", err);
            body
        })
}

/// Receive an ICMP(v4) error message.
///
/// `receive_icmpv4_error` handles an incoming ICMP error message by parsing the
/// original IPv4 packet and then delegating to the context.
fn receive_icmpv4_error<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
    BB: ByteSlice,
    M: IcmpMessage<Ipv4, Body<BB> = OriginalPacket<BB>>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    packet: &IcmpPacket<Ipv4, BB, M>,
    err: Icmpv4ErrorCode,
) {
    packet.with_original_packet(|res| match res {
        Ok(original_packet) => {
            let dst_ip = match SpecifiedAddr::new(original_packet.dst_ip()) {
                Some(ip) => ip,
                None => {
                    trace!("receive_icmpv4_error: Got ICMP error message whose original IPv4 packet contains an unspecified destination address; discarding");
                    return;
                },
            };
            InnerIcmpContext::receive_icmp_error(
                sync_ctx,
                ctx,
                device,
                SpecifiedAddr::new(original_packet.src_ip()),
                dst_ip,
                original_packet.proto(),
                original_packet.body().into_inner(),
                err,
            );
        }
        Err(_) => debug!(
            "receive_icmpv4_error: Got ICMP error message with unparsable original IPv4 packet"
        ),
    })
}

/// Receive an ICMPv6 error message.
///
/// `receive_icmpv6_error` handles an incoming ICMPv6 error message by parsing
/// the original IPv6 packet and then delegating to the context.
fn receive_icmpv6_error<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
    BB: ByteSlice,
    M: IcmpMessage<Ipv6, Body<BB> = OriginalPacket<BB>>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    packet: &IcmpPacket<Ipv6, BB, M>,
    err: Icmpv6ErrorCode,
) {
    packet.with_original_packet(|res| match res {
        Ok(original_packet) => {
            let dst_ip = match SpecifiedAddr::new(original_packet.dst_ip()) {
                Some(ip)=>ip,
                None => {
                    trace!("receive_icmpv6_error: Got ICMP error message whose original IPv6 packet contains an unspecified destination address; discarding");
                    return;
                },
            };
            match original_packet.body_proto() {
                Ok((body, proto)) => {
                    InnerIcmpContext::receive_icmp_error(
                        sync_ctx,
                        ctx,
                        device,
                        SpecifiedAddr::new(original_packet.src_ip()),
                        dst_ip,
                        proto,
                        body.into_inner(),
                        err,
                    );
                }
                Err(ExtHdrParseError) => {
                    trace!("receive_icmpv6_error: We could not parse the original packet's extension headers, and so we don't know where the original packet's body begins; discarding");
                    // There's nothing we can do in this case, so we just
                    // return.
                    return;
                }
            }
        }
        Err(_body) => debug!(
            "receive_icmpv6_error: Got ICMPv6 error message with unparsable original IPv6 packet"
        ),
    })
}

/// Send an ICMP(v4) message in response to receiving a packet destined for an
/// unsupported IPv4 protocol.
///
/// `send_icmpv4_protocol_unreachable` sends the appropriate ICMP message in
/// response to receiving an IP packet from `src_ip` to `dst_ip` identifying an
/// unsupported protocol - in particular, a "destination unreachable" message
/// with a "protocol unreachable" code.
///
/// `original_packet` contains the contents of the entire original packet,
/// including the IP header. This must be a whole packet, not a packet fragment.
/// `header_len` is the length of the header including all options.
pub(crate) fn send_icmpv4_protocol_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    original_packet: B,
    header_len: usize,
) {
    ctx.increment_debug_counter("send_icmpv4_protocol_unreachable");

    send_icmpv4_dest_unreachable(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4DestUnreachableCode::DestProtocolUnreachable,
        original_packet,
        header_len,
        // If we are sending a protocol unreachable error it is correct to assume that, if the
        // packet was initially fragmented, it has been successfully reassembled by now. It
        // guarantees that we won't send more than one ICMP Destination Unreachable message for
        // different fragments of the same original packet, so we should behave as if we are
        // handling an initial fragment.
        Ipv4FragmentType::InitialFragment,
    );
}

/// Send an ICMPv6 message in response to receiving a packet destined for an
/// unsupported Next Header.
///
/// `send_icmpv6_protocol_unreachable` is like
/// [`send_icmpv4_protocol_unreachable`], but for ICMPv6. It sends an ICMPv6
/// "parameter problem" message with an "unrecognized next header type" code.
///
/// `header_len` is the length of all IPv6 headers (including extension headers)
/// *before* the payload with the problematic Next Header type.
pub(crate) fn send_icmpv6_protocol_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    original_packet: B,
    header_len: usize,
) {
    ctx.increment_debug_counter("send_icmpv6_protocol_unreachable");

    send_icmpv6_parameter_problem(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
        // Per RFC 4443, the pointer refers to the first byte of the packet
        // whose Next Header field was unrecognized. It is measured as an offset
        // from the beginning of the first IPv6 header. E.g., a pointer of 40
        // (the length of a single IPv6 header) would indicate that the Next
        // Header field from that header - and hence of the first encapsulated
        // packet - was unrecognized.
        //
        // NOTE: Since header_len is a usize, this could theoretically be a
        // lossy conversion. However, all that means in practice is that, if a
        // remote host somehow managed to get us to process a frame with a 4GB
        // IP header and send an ICMP response, the pointer value would be
        // wrong. It's not worth wasting special logic to avoid generating a
        // malformed packet in a case that will almost certainly never happen.
        Icmpv6ParameterProblem::new(header_len as u32),
        original_packet,
        false,
    );
}

/// Send an ICMP(v4) message in response to receiving a packet destined for an
/// unreachable local transport-layer port.
///
/// `send_icmpv4_port_unreachable` sends the appropriate ICMP message in
/// response to receiving an IP packet from `src_ip` to `dst_ip` with an
/// unreachable local transport-layer port. In particular, this is an ICMP
/// "destination unreachable" message with a "port unreachable" code.
///
/// `original_packet` contains the contents of the entire original packet,
/// including the IP header. This must be a whole packet, not a packet fragment.
/// `header_len` is the length of the header including all options.
pub(crate) fn send_icmpv4_port_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    original_packet: B,
    header_len: usize,
) {
    ctx.increment_debug_counter("send_icmpv4_port_unreachable");

    send_icmpv4_dest_unreachable(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4DestUnreachableCode::DestPortUnreachable,
        original_packet,
        header_len,
        // If we are sending a port unreachable error it is correct to assume that, if the packet
        // was initially fragmented, it has been successfully reassembled by now. It guarantees that
        // we won't send more than one ICMP Destination Unreachable message for different fragments
        // of the same original packet, so we should behave as if we are handling an initial
        // fragment.
        Ipv4FragmentType::InitialFragment,
    );
}

/// Send an ICMPv6 message in response to receiving a packet destined for an
/// unreachable local transport-layer port.
///
/// `send_icmpv6_port_unreachable` is like [`send_icmpv4_port_unreachable`], but
/// for ICMPv6.
///
/// `original_packet` contains the contents of the entire original packet,
/// including extension headers.
pub(crate) fn send_icmpv6_port_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    original_packet: B,
) {
    ctx.increment_debug_counter("send_icmpv6_port_unreachable");

    send_icmpv6_dest_unreachable(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6DestUnreachableCode::PortUnreachable,
        original_packet,
    );
}

/// Send an ICMP(v4) message in response to receiving a packet destined for an
/// unreachable network.
///
/// `send_icmpv4_net_unreachable` sends the appropriate ICMP message in response
/// to receiving an IP packet from `src_ip` to an unreachable `dst_ip`. In
/// particular, this is an ICMP "destination unreachable" message with a "net
/// unreachable" code.
///
/// `original_packet` contains the contents of the entire original packet -
/// including all IP headers. `header_len` is the length of the IPv4 header. It
/// is ignored for IPv6.
pub(crate) fn send_icmpv4_net_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    proto: Ipv4Proto,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    ctx.increment_debug_counter("send_icmpv4_net_unreachable");

    // Check whether we MUST NOT send an ICMP error message
    // because the original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv4>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv4_dest_unreachable(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4DestUnreachableCode::DestNetworkUnreachable,
        original_packet,
        header_len,
        fragment_type,
    );
}

/// Send an ICMPv6 message in response to receiving a packet destined for an
/// unreachable network.
///
/// `send_icmpv6_net_unreachable` is like [`send_icmpv4_net_unreachable`], but
/// for ICMPv6. It sends an ICMPv6 "destination unreachable" message with a "no
/// route to destination" code.
///
/// `original_packet` contains the contents of the entire original packet
/// including extension headers. `header_len` is the length of the IP header and
/// all extension headers.
pub(crate) fn send_icmpv6_net_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    original_packet: B,
    header_len: usize,
) {
    ctx.increment_debug_counter("send_icmpv6_net_unreachable");

    // Check whether we MUST NOT send an ICMP error message
    // because the original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv6>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv6_dest_unreachable(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6DestUnreachableCode::NoRoute,
        original_packet,
    );
}

/// Send an ICMP(v4) message in response to receiving a packet whose TTL has
/// expired.
///
/// `send_icmpv4_ttl_expired` sends the appropriate ICMP in response to
/// receiving an IP packet from `src_ip` to `dst_ip` whose TTL has expired. In
/// particular, this is an ICMP "time exceeded" message with a "time to live
/// exceeded in transit" code.
///
/// `original_packet` contains the contents of the entire original packet,
/// including the header. `header_len` is the length of the IP header including
/// options.
pub(crate) fn send_icmpv4_ttl_expired<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    proto: Ipv4Proto,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    ctx.increment_debug_counter("send_icmpv4_ttl_expired");

    // Check whether we MUST NOT send an ICMP error message because the original
    // packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv4>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv4_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv4TimeExceededCode::TtlExpired,
        IcmpTimeExceeded::default(),
        original_packet,
        header_len,
        fragment_type,
    )
}

/// Send an ICMPv6 message in response to receiving a packet whose hop limit has
/// expired.
///
/// `send_icmpv6_ttl_expired` is like [`send_icmpv4_ttl_expired`], but for
/// ICMPv6. It sends an ICMPv6 "time exceeded" message with a "hop limit
/// exceeded in transit" code.
///
/// `original_packet` contains the contents of the entire original packet
/// including extension headers. `header_len` is the length of the IP header and
/// all extension headers.
pub(crate) fn send_icmpv6_ttl_expired<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    original_packet: B,
    header_len: usize,
) {
    ctx.increment_debug_counter("send_icmpv6_ttl_expired");

    // Check whether we MUST NOT send an ICMP error message because the
    // original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv6>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv6_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        Icmpv6TimeExceededCode::HopLimitExceeded,
        IcmpTimeExceeded::default(),
        original_packet,
        false, /* allow_dst_multicast */
    )
}

// TODO(joshlf): Test send_icmpv6_packet_too_big once we support fake IPv6 test
// setups.

/// Send an ICMPv6 message in response to receiving a packet whose size exceeds
/// the MTU of the next hop interface.
///
/// `send_icmpv6_packet_too_big` sends an ICMPv6 "packet too big" message in
/// response to receiving an IP packet from `src_ip` to `dst_ip` whose size
/// exceeds the `mtu` of the next hop interface.
pub(crate) fn send_icmpv6_packet_too_big<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    proto: Ipv6Proto,
    mtu: Mtu,
    original_packet: B,
    header_len: usize,
) {
    ctx.increment_debug_counter("send_icmpv6_packet_too_big");
    // Check whether we MUST NOT send an ICMP error message because the
    // original packet was itself an ICMP error message.
    if is_icmp_error_message::<Ipv6>(proto, &original_packet.as_ref()[header_len..]) {
        return;
    }

    send_icmpv6_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        IcmpUnusedCode,
        Icmpv6PacketTooBig::new(mtu.into()),
        original_packet,
        // As per RFC 4443 section 2.4.e,
        //
        //   An ICMPv6 error message MUST NOT be originated as a result of
        //   receiving the following:
        //
        //     (e.3) A packet destined to an IPv6 multicast address.  (There are
        //           two exceptions to this rule: (1) the Packet Too Big Message
        //           (Section 3.2) to allow Path MTU discovery to work for IPv6
        //           multicast, and (2) the Parameter Problem Message, Code 2
        //           (Section 3.4) reporting an unrecognized IPv6 option (see
        //           Section 4.2 of [IPv6]) that has the Option Type highest-
        //           order two bits set to 10).
        //
        //     (e.4) A packet sent as a link-layer multicast (the exceptions
        //           from e.3 apply to this case, too).
        //
        // Thus, we explicitly allow sending a Packet Too Big error if the
        // destination was a multicast packet.
        true, /* allow_dst_multicast */
    )
}

pub(crate) fn send_icmpv4_parameter_problem<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    code: Icmpv4ParameterProblemCode,
    parameter_problem: Icmpv4ParameterProblem,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    ctx.increment_debug_counter("send_icmpv4_parameter_problem");

    send_icmpv4_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        parameter_problem,
        original_packet,
        header_len,
        fragment_type,
    )
}

/// Send an ICMPv6 Parameter Problem error message.
///
/// If the error message is Code 2 reporting an unrecognized IPv6 option that
/// has the Option Type highest-order two bits set to 10, `allow_dst_multicast`
/// must be set to `true`. See [`should_send_icmpv6_error`] for more details.
///
/// # Panics
///
/// Panics if `allow_multicast_addr` is set to `true`, but this Parameter
/// Problem's code is not 2 (Unrecognized IPv6 Option).
pub(crate) fn send_icmpv6_parameter_problem<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    code: Icmpv6ParameterProblemCode,
    parameter_problem: Icmpv6ParameterProblem,
    original_packet: B,
    allow_dst_multicast: bool,
) {
    // Only allow the `allow_dst_multicast` parameter to be set if the code is
    // the unrecognized IPv6 option as that is one of the few exceptions where
    // we can send an ICMP packet in response to a packet that was destined for
    // a multicast address.
    assert!(!allow_dst_multicast || code == Icmpv6ParameterProblemCode::UnrecognizedIpv6Option);

    ctx.increment_debug_counter("send_icmpv6_parameter_problem");

    send_icmpv6_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        parameter_problem,
        original_packet,
        allow_dst_multicast,
    )
}

fn send_icmpv4_dest_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    code: Icmpv4DestUnreachableCode,
    original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    ctx.increment_debug_counter("send_icmpv4_dest_unreachable");
    send_icmpv4_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        IcmpDestUnreachable::default(),
        original_packet,
        header_len,
        fragment_type,
    )
}

fn send_icmpv6_dest_unreachable<
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    src_ip: UnicastAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    code: Icmpv6DestUnreachableCode,
    original_packet: B,
) {
    send_icmpv6_error_message(
        sync_ctx,
        ctx,
        device,
        frame_dst,
        src_ip,
        dst_ip,
        code,
        IcmpDestUnreachable::default(),
        original_packet,
        false, /* allow_dst_multicast */
    )
}

const SEND_ICMPV4_ERROR_MESSAGE_COUNTER_NAME: &'static str = "send_icmpv4_error_message";

fn send_icmpv4_error_message<
    B: BufferMut,
    M: IcmpMessage<Ipv4>,
    C: BufferIcmpNonSyncCtx<Ipv4, B>,
    SC: InnerBufferIcmpv4Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    original_src_ip: SpecifiedAddr<Ipv4Addr>,
    original_dst_ip: SpecifiedAddr<Ipv4Addr>,
    code: M::Code,
    message: M,
    mut original_packet: B,
    header_len: usize,
    fragment_type: Ipv4FragmentType,
) {
    // TODO(https://fxbug.dev/95827): Come up with rules for when to send ICMP
    // error messages.

    if !should_send_icmpv4_error(frame_dst, original_src_ip, original_dst_ip, fragment_type) {
        return;
    }

    // Per RFC 792, body contains entire IPv4 header + 64 bytes of original
    // body.
    original_packet.shrink_back_to(header_len + 64);

    // TODO(https://fxbug.dev/95828): Improve source address selection for ICMP
    // errors sent from unnumbered/router interfaces.
    let _ = try_send_error!(
        sync_ctx,
        ctx,
        sync_ctx.send_oneshot_ip_packet(
            ctx,
            Some(EitherDeviceId::Strong(device)),
            None,
            original_src_ip,
            Ipv4Proto::Icmp,
            DefaultSendOptions,
            |local_ip| {
                original_packet.encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                    local_ip,
                    original_src_ip,
                    code,
                    message,
                ))
            },
            None
        ),
        SEND_ICMPV4_ERROR_MESSAGE_COUNTER_NAME
    );
}

const SEND_ICMPV6_ERROR_MESSAGE_COUNTER_NAME: &'static str = "send_icmpv6_error_message";

fn send_icmpv6_error_message<
    B: BufferMut,
    M: IcmpMessage<Ipv6>,
    C: BufferIcmpNonSyncCtx<Ipv6, B>,
    SC: InnerBufferIcmpv6Context<C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device: &SC::DeviceId,
    frame_dst: FrameDestination,
    original_src_ip: UnicastAddr<Ipv6Addr>,
    original_dst_ip: SpecifiedAddr<Ipv6Addr>,
    code: M::Code,
    message: M,
    original_packet: B,
    allow_dst_multicast: bool,
) {
    // TODO(https://fxbug.dev/95827): Come up with rules for when to send ICMP
    // error messages.

    let original_src_ip = original_src_ip.into_specified();
    if !should_send_icmpv6_error(frame_dst, original_src_ip, original_dst_ip, allow_dst_multicast) {
        return;
    }

    // TODO(https://fxbug.dev/95828): Improve source address selection for ICMP
    // errors sent from unnumbered/router interfaces.
    let _ = try_send_error!(
        sync_ctx,
        ctx,
        sync_ctx.send_oneshot_ip_packet(
            ctx,
            Some(EitherDeviceId::Strong(device)),
            None,
            original_src_ip,
            Ipv6Proto::Icmpv6,
            DefaultSendOptions,
            |local_ip| {
                let icmp_builder =
                    IcmpPacketBuilder::<Ipv6, _>::new(local_ip, original_src_ip, code, message);

                // Per RFC 4443, body contains as much of the original body as
                // possible without exceeding IPv6 minimum MTU.
                TruncatingSerializer::new(original_packet, TruncateDirection::DiscardBack)
                    .encapsulate(icmp_builder)
            },
            Some(Ipv6::MINIMUM_LINK_MTU.get()),
        ),
        SEND_ICMPV6_ERROR_MESSAGE_COUNTER_NAME
    );
}

/// Should we send an ICMP(v4) response?
///
/// `should_send_icmpv4_error` implements the logic described in RFC 1122
/// Section 3.2.2. It decides whether, upon receiving an incoming packet with
/// the given parameters, we should send an ICMP response or not. In particular,
/// we do not send an ICMP response if we've received:
/// - a packet destined to a broadcast or multicast address
/// - a packet sent in a link-layer broadcast
/// - a non-initial fragment
/// - a packet whose source address does not define a single host (a
///   zero/unspecified address, a loopback address, a broadcast address, a
///   multicast address, or a Class E address)
///
/// Note that `should_send_icmpv4_error` does NOT check whether the incoming
/// packet contained an ICMP error message. This is because that check is
/// unnecessary for some ICMP error conditions. The ICMP error message check can
/// be performed separately with `is_icmp_error_message`.
fn should_send_icmpv4_error(
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv4Addr>,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    fragment_type: Ipv4FragmentType,
) -> bool {
    // NOTE: We do not explicitly implement the "unspecified address" check, as
    // it is enforced by the types of the arguments.

    // TODO(joshlf): Implement the rest of the rules:
    // - a packet destined to a subnet broadcast address
    // - a packet whose source address is a subnet broadcast address

    // NOTE: The FrameDestination type has variants for unicast, multicast, and
    // broadcast. One implication of the fact that we only check for broadcast
    // here (in compliance with the RFC) is that we could, in one very unlikely
    // edge case, respond with an ICMP error message to an IP packet which was
    // sent in a link-layer multicast frame. In particular, that can happen if
    // we subscribe to a multicast IP group and, as a result, subscribe to the
    // corresponding multicast MAC address, and we receive a unicast IP packet
    // in a multicast link-layer frame destined to that MAC address.
    //
    // TODO(joshlf): Should we filter incoming multicast IP traffic to make sure
    // that it matches the multicast MAC address of the frame it was
    // encapsulated in?
    fragment_type == Ipv4FragmentType::InitialFragment
        && !(dst_ip.is_multicast()
            || dst_ip.is_limited_broadcast()
            || frame_dst.is_broadcast()
            || src_ip.is_loopback()
            || src_ip.is_limited_broadcast()
            || src_ip.is_multicast()
            || src_ip.is_class_e())
}

/// Should we send an ICMPv6 response?
///
/// `should_send_icmpv6_error` implements the logic described in RFC 4443
/// Section 2.4.e. It decides whether, upon receiving an incoming packet with
/// the given parameters, we should send an ICMP response or not. In particular,
/// we do not send an ICMP response if we've received:
/// - a packet destined to a multicast address
///   - Two exceptions to this rules:
///     1) the Packet Too Big Message to allow Path MTU discovery to work for
///        IPv6 multicast
///     2) the Parameter Problem Message, Code 2 reporting an unrecognized IPv6
///        option that has the Option Type highest-order two bits set to 10
/// - a packet sent as a link-layer multicast or broadcast
///   - same exceptions apply here as well.
/// - a packet whose source address does not define a single host (a
///   zero/unspecified address, a loopback address, or a multicast address)
///
/// If an ICMP response will be a Packet Too Big Message or a Parameter Problem
/// Message, Code 2 reporting an unrecognized IPv6 option that has the Option
/// Type highest-order two bits set to 10, `info.allow_dst_multicast` must be
/// set to `true` so this function will allow the exception mentioned above.
///
/// Note that `should_send_icmpv6_error` does NOT check whether the incoming
/// packet contained an ICMP error message. This is because that check is
/// unnecessary for some ICMP error conditions. The ICMP error message check can
/// be performed separately with `is_icmp_error_message`.
fn should_send_icmpv6_error(
    frame_dst: FrameDestination,
    src_ip: SpecifiedAddr<Ipv6Addr>,
    dst_ip: SpecifiedAddr<Ipv6Addr>,
    allow_dst_multicast: bool,
) -> bool {
    // NOTE: We do not explicitly implement the "unspecified address" check, as
    // it is enforced by the types of the arguments.
    !((!allow_dst_multicast
        && (dst_ip.is_multicast() || frame_dst.is_multicast() || frame_dst.is_broadcast()))
        || src_ip.is_loopback()
        || src_ip.is_multicast())
}

/// Determine whether or not an IP packet body contains an ICMP error message
/// for the purposes of determining whether or not to send an ICMP response.
///
/// `is_icmp_error_message` checks whether `proto` is ICMP(v4) for IPv4 or
/// ICMPv6 for IPv6 and, if so, attempts to parse `buf` as an ICMP packet in
/// order to determine whether it is an error message or not. If parsing fails,
/// it conservatively assumes that it is an error packet in order to avoid
/// violating the MUST NOT directives of RFC 1122 Section 3.2.2 and [RFC 4443
/// Section 2.4.e].
///
/// [RFC 4443 Section 2.4.e]: https://tools.ietf.org/html/rfc4443#section-2.4
fn is_icmp_error_message<I: IcmpIpExt>(proto: I::Proto, buf: &[u8]) -> bool {
    proto == I::ICMP_IP_PROTO
        && peek_message_type::<I::IcmpMessageType>(buf).map(IcmpMessageType::is_err).unwrap_or(true)
}

/// Common logic for receiving an ICMP echo reply.
fn receive_icmp_echo_reply<
    I: IcmpIpExt + IpExt,
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<I, B>,
    SC: InnerBufferIcmpContext<I, C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    src_ip: I::Addr,
    dst_ip: SpecifiedAddr<I::Addr>,
    id: u16,
    seq: u16,
    body: B,
) {
    let src_ip = match SpecifiedAddr::new(src_ip) {
        Some(src_ip) => src_ip,
        None => {
            trace!("receive_icmp_echo_reply: unspecified source address");
            return;
        }
    };
    let src_ip: SocketIpAddr<_> = match src_ip.try_into() {
        Ok(src_ip) => src_ip,
        Err(AddrIsMappedError {}) => {
            trace!("receive_icmp_echo_reply: mapped source address");
            return;
        }
    };
    let dst_ip: SocketIpAddr<_> = match dst_ip.try_into() {
        Ok(dst_ip) => dst_ip,
        Err(AddrIsMappedError {}) => {
            trace!("receive_icmp_echo_reply: mapped destination address");
            return;
        }
    };
    sync_ctx.with_icmp_sockets(|sockets| {
        if let Some(id) = NonZeroU16::new(id) {
            if let Some(conn) = sockets.bound.conns().get_by_addr(&ConnAddr {
                ip: ConnIpAddr { local: (dst_ip, id), remote: (src_ip, ()) },
                device: None,
            }) {
                trace!("receive_icmp_echo_reply: Received echo reply for local socket");
                ctx.receive_icmp_echo_reply(
                    *conn,
                    src_ip.addr(),
                    dst_ip.addr(),
                    id.get(),
                    seq,
                    body,
                );
                return;
            }
        }
        // TODO(fxbug.dev/47952): Neither the ICMPv4 or ICMPv6 RFCs
        // explicitly state what to do in case we receive an "unsolicited"
        // echo reply. We only expose the replies if we have a registered
        // connection for the IcmpAddr of the incoming reply for now. Given
        // that a reply should only be sent in response to a request, an
        // ICMP unreachable-type message is probably not appropriate for
        // unsolicited replies. However, it's also possible that we sent a
        // request and then closed the socket before receiving the reply, so
        // this doesn't necessarily indicate a buggy or malicious remote
        // host. We should figure this out definitively.
        //
        // If we do decide to send an ICMP error message, the appropriate
        // thing to do is probably to have this function return a `Result`,
        // and then have the top-level implementation of
        // `BufferIpTransportContext::receive_ip_packet` return the
        // appropriate error.
        trace!("receive_icmp_echo_reply: Received echo reply with no local socket");
    })
}

/// Send an ICMPv4 echo request on an existing connection.
///
/// # Panics
///
/// `send_icmpv4_echo_request` panics if `conn` is not associated with an ICMPv4
/// connection.
pub fn send_icmpv4_echo_request<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    conn: SocketId<Ipv4>,
    seq_num: u16,
    body: B,
) -> Result<(), (B, IpSockSendError)> {
    send_icmp_echo_request_inner(&mut Locked::new(sync_ctx), ctx, conn, seq_num, body)
}

/// Send an ICMPv6 echo request on an existing connection.
///
/// # Panics
///
/// `send_icmpv6_echo_request` panics if `conn` is not associated with an ICMPv6
/// connection.
pub fn send_icmpv6_echo_request<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    conn: SocketId<Ipv6>,
    seq_num: u16,
    body: B,
) -> Result<(), (B, IpSockSendError)> {
    send_icmp_echo_request_inner(&mut Locked::new(sync_ctx), ctx, conn, seq_num, body)
}

fn send_icmp_echo_request_inner<
    I: IcmpIpExt + IpExt + datagram::DualStackIpExt,
    B: BufferMut,
    C: BufferIcmpNonSyncCtx<I, B>,
    SC: InnerBufferIcmpContext<I, C, B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    conn: SocketId<I>,
    seq_num: u16,
    body: B,
) -> Result<(), (B, IpSockSendError)> {
    let (socket, id) = sync_ctx.with_icmp_ctx_and_sockets_mut(|_ip_transport_ctx, sockets| {
        let state = assert_matches!(
            sockets.state.get(conn.into()),
            Some(datagram::SocketState::Bound(datagram::BoundSocketState::Connected {
                state,
                sharing: (),
            })) => state,
            "invalid socket ID",
        );
        (state.socket.clone(), state.addr.ip.local.1.get())
    });
    sync_ctx
        .send_ip_packet(
            ctx,
            &socket,
            body.encapsulate(IcmpPacketBuilder::<I, _>::new(
                socket.local_ip().get(),
                socket.remote_ip().get(),
                IcmpUnusedCode,
                IcmpEchoRequest::new(id, seq_num),
            )),
            None,
        )
        .map_err(|(encapsulated, err)| (encapsulated.into_inner(), err))
}

/// An error when attempting to create ang ICMP socket.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum IcmpSockCreationError {
    /// An error was encountered when attempting to create the underlying IP
    /// socket.
    #[error("{}", _0)]
    Ip(#[from] IpSockCreationError),
    /// The specified socket addresses (IP addresses and ICMP ID) conflict with
    /// an existing ICMP socket.
    #[error("addresses conflict with an existing ICMP socket")]
    SockAddrConflict,
    /// The specified remote address is mapped (i.e. ipv4-mapped-ipv6 address).
    /// ICMP sockets do not support dual-stack operations.
    #[error("remote address is mapped")]
    RemoteAddrIsMapped,
}

/// Creates a new unbound ICMPv4 socket.
pub fn create_icmpv4_unbound<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
) -> SocketId<Ipv4> {
    create_icmpv4_unbound_inner(&mut Locked::new(sync_ctx))
}

// TODO(https://fxbug.dev/48578): Make this the external function (replacing the
// existing `create_icmpv4_unbound`) once the ICMP context traits are part of
// the public API.
fn create_icmpv4_unbound_inner<C: IcmpNonSyncCtx<Ipv4>, SC: InnerIcmpv4Context<C>>(
    sync_ctx: &mut SC,
) -> SocketId<Ipv4> {
    InnerIcmpContext::with_icmp_ctx_and_sockets_mut(sync_ctx, |_ip_transport_ctx, sockets| {
        SocketId::new(sockets.state.push(datagram::SocketState::Unbound(Default::default())))
    })
}

/// Creates a new unbound ICMPv6 socket.
pub fn create_icmpv6_unbound<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
) -> SocketId<Ipv6> {
    create_icmpv6_unbound_inner(&mut Locked::new(sync_ctx))
}

// TODO(https://fxbug.dev/48578): Make this the external function (replacing the
// existing `create_icmpv6_unbound`) once the ICMP context traits are part of
// the public API.
fn create_icmpv6_unbound_inner<C: IcmpNonSyncCtx<Ipv6>, SC: InnerIcmpv6Context<C>>(
    sync_ctx: &mut SC,
) -> SocketId<Ipv6> {
    InnerIcmpContext::with_icmp_ctx_and_sockets_mut(sync_ctx, |_ip_transport_ctx, sockets| {
        SocketId::new(sockets.state.push(datagram::SocketState::Unbound(Default::default())))
    })
}

/// Removes an unbound ICMPv4 socket.
///
/// # Panics
///
/// Panics if `id` is not a valid [`SocketId`].
pub fn remove_icmpv4_unbound<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    id: SocketId<Ipv4>,
) {
    remove_icmpv4_unbound_inner(&mut Locked::new(sync_ctx), id)
}

// TODO(https://fxbug.dev/48578): Make this the external function (replacing the
// existing `create_icmpv6_unbound`) once the ICMP context traits are part of
// the public API.
fn remove_icmpv4_unbound_inner<C: IcmpNonSyncCtx<Ipv4>, SC: InnerIcmpv4Context<C>>(
    sync_ctx: &mut SC,
    id: SocketId<Ipv4>,
) {
    assert_matches!(
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(sync_ctx, |_ip_transport_ctx, sockets| {
            sockets.state.remove(id.into())
        }),
        Some(_),
        "unbound ID is invalid: {:?}",
        id
    );
}

/// Removes an unbound ICMPv6 socket.
///
/// # Panics
///
/// Panics if `id` is not a valid [`SocketId`].
pub fn remove_icmpv6_unbound<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    id: SocketId<Ipv6>,
) {
    remove_icmpv6_unbound_inner(&mut Locked::new(sync_ctx), id)
}

// TODO(https://fxbug.dev/48578): Make this the external function (replacing the
// existing `create_icmpv6_unbound`) once the ICMP context traits are part of
// the public API.
fn remove_icmpv6_unbound_inner<C: IcmpNonSyncCtx<Ipv6>, SC: InnerIcmpv6Context<C>>(
    sync_ctx: &mut SC,
    id: SocketId<Ipv6>,
) {
    assert_matches!(
        InnerIcmpContext::with_icmp_ctx_and_sockets_mut(sync_ctx, |_ip_transport_ctx, sockets| {
            sockets.state.remove(id.into())
        }),
        Some(_),
        "unbound ID is invalid: {:?}",
        id
    );
}

/// Connects an unbound ICMPv4 socket.
///
/// Replaces `id` with a new ICMPv4 connection with the provided parameters
/// `local_addr`, `remote_addr` and `icmp_id`, and returns its newly-allocated
/// ID. If `local_addr` is `None`, one will be chosen automatically.
///
/// If a connection with the conflicting parameters already exists, the call
/// fails and returns [`IcmpSockCreationError::SockAddrConflict`].
///
/// # Panics
///
/// Panics if `id` is an invalid [`SocketId`].
pub fn connect_icmpv4<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    id: SocketId<Ipv4>,
    local_addr: Option<SpecifiedAddr<Ipv4Addr>>,
    remote_addr: SpecifiedAddr<Ipv4Addr>,
    icmp_id: u16,
) -> Result<SocketId<Ipv4>, IcmpSockCreationError> {
    connect_icmpv4_inner(&mut Locked::new(sync_ctx), ctx, id, local_addr, remote_addr, icmp_id)
}

// TODO(https://fxbug.dev/48578): Make this the external function (replacing the
// existing `connect_icmpv4`) once the ICMP context traits are part of the
// public API.
fn connect_icmpv4_inner<C: IcmpNonSyncCtx<Ipv4>, SC: InnerIcmpv4Context<C>>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: SocketId<Ipv4>,
    local_addr: Option<SpecifiedAddr<Ipv4Addr>>,
    remote_addr: SpecifiedAddr<Ipv4Addr>,
    icmp_id: u16,
) -> Result<SocketId<Ipv4>, IcmpSockCreationError> {
    let remote_addr = SocketIpAddr::<Ipv4Addr>::new_ipv4_specified(remote_addr);

    InnerIcmpContext::with_icmp_ctx_and_sockets_mut(
        sync_ctx,
        |ip_transport_ctx, IcmpSockets { bound, id_allocator: _, state }| {
            let ip = ip_transport_ctx
                .new_ip_socket(
                    ctx,
                    None,
                    local_addr,
                    remote_addr.into(),
                    Ipv4Proto::Icmp,
                    datagram::IpOptions::default(),
                )
                .map_err(|(e, _ip_options)| e)?;
            connect_icmp_inner(state, bound, id, remote_addr, icmp_id, ip)
        },
    )
}

/// Connects an unbound ICMPv6 socket.
///
/// Replaces `id` with a new ICMPv6 connection with the provided parameters
/// `local_addr`, `remote_addr` and `icmp_id`, and returns its newly-allocated
/// ID. If `local_addr` is `None`, one will be chosen automatically.
///
/// If a connection with the conflicting parameters already exists, the call
/// fails and returns [`IcmpSockCreationError::SockAddrConflict`].
///
/// # Panics
///
/// Panics if `id` is an invalid [`SocketId`].
pub fn connect_icmpv6<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    id: SocketId<Ipv6>,
    local_addr: Option<SpecifiedAddr<Ipv6Addr>>,
    remote_addr: SpecifiedAddr<Ipv6Addr>,
    icmp_id: u16,
) -> Result<SocketId<Ipv6>, IcmpSockCreationError> {
    connect_icmpv6_inner(&mut Locked::new(sync_ctx), ctx, id, local_addr, remote_addr, icmp_id)
}

// TODO(https://fxbug.dev/48578): Make this the external function (replacing the
// existing `connect_icmpv6`) once the ICMP context traits are part of the
// public API.
fn connect_icmpv6_inner<C: IcmpNonSyncCtx<Ipv6>, SC: InnerIcmpv6Context<C>>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: SocketId<Ipv6>,
    local_addr: Option<SpecifiedAddr<Ipv6Addr>>,
    remote_addr: SpecifiedAddr<Ipv6Addr>,
    icmp_id: u16,
) -> Result<SocketId<Ipv6>, IcmpSockCreationError> {
    let remote_addr: SocketIpAddr<_> = remote_addr
        .try_into()
        .map_err(|AddrIsMappedError {}| IcmpSockCreationError::RemoteAddrIsMapped)?;

    InnerIcmpContext::with_icmp_ctx_and_sockets_mut(
        sync_ctx,
        |ip_transport_ctx, IcmpSockets { bound, id_allocator: _, state }| {
            let ip = ip_transport_ctx
                .new_ip_socket(
                    ctx,
                    None,
                    local_addr,
                    remote_addr.into(),
                    Ipv6Proto::Icmpv6,
                    datagram::IpOptions::default(),
                )
                .map_err(|(e, _options)| e)?;
            connect_icmp_inner(state, bound, id, remote_addr, icmp_id, ip)
        },
    )
}

fn connect_icmp_inner<I: IcmpIpExt + IpExt + datagram::DualStackIpExt, D: WeakId>(
    state: &mut datagram::SocketsState<I, D, Icmp>,
    bound: &mut IcmpBoundSockets<I, D>,
    id: SocketId<I>,
    remote_addr: SocketIpAddr<I::Addr>,
    icmp_id: u16,
    ip: IpSock<I, D, datagram::IpOptions<I, D, Icmp>>,
) -> Result<SocketId<I>, IcmpSockCreationError> {
    // TODO(https://fxbug.dev/132092): remove panic opportunity once `IpSock`
    // holds `SocketIpAddr`.
    let local_ip = SocketIpAddr::new_from_specified_or_panic(*ip.local_ip());
    let addr = ConnAddr {
        ip: ConnIpAddr {
            local: (local_ip, NonZeroU16::new(icmp_id).unwrap()),
            remote: (remote_addr, ()),
        },
        device: None,
    };

    match bound.conns_mut().try_insert(addr.clone(), (), id) {
        Ok(_entry) => {
            *state.get_mut(id.into()).expect("invalid ID") =
                datagram::SocketState::Bound(datagram::BoundSocketState::Connected {
                    state: datagram::ConnState {
                        socket: ip,
                        shutdown: Default::default(),
                        addr,
                        clear_device_on_disconnect: true,
                    },
                    sharing: (),
                });
            Ok(id)
        }
        Err((_insert_error, _entry)) => Err(IcmpSockCreationError::SockAddrConflict),
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec, vec::Vec};
    use core::{convert::TryInto, fmt::Debug, num::NonZeroU16, time::Duration};

    use ip_test_macro::ip_test;
    use net_declare::net_ip_v6;
    use net_types::ip::{Ip, IpVersion, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Subnet};
    use packet::{Buf, Serializer};
    use packet_formats::{
        ethernet::EthernetFrameLengthCheck,
        icmp::{
            mld::MldPacket, IcmpEchoRequest, IcmpMessage, IcmpPacket, IcmpUnusedCode,
            Icmpv4TimestampRequest, MessageBody,
        },
        ip::{IpPacketBuilder, IpProto},
        testutil::parse_icmp_packet_in_ip_packet_in_ethernet_frame,
        udp::UdpPacketBuilder,
        utils::NonZeroDuration,
    };

    use super::*;
    use crate::{
        context::testutil::{FakeCtx, FakeInstant, FakeNonSyncCtx, FakeSyncCtx},
        device::{
            testutil::{set_forwarding_enabled, FakeDeviceId, FakeWeakDeviceId},
            DeviceId, FrameDestination,
        },
        ip::{
            device::{
                route_discovery::Ipv6DiscoveredRoute, state::IpDeviceStateIpExt, IpDeviceHandler,
            },
            path_mtu::testutil::FakePmtuState,
            receive_ip_packet,
            socket::testutil::FakeIpSocketCtx,
            testutil::FakeIpDeviceIdCtx,
            SendIpPacketMeta,
        },
        testutil::{
            assert_empty, get_counter_val, handle_queued_rx_packets, Ctx, DEFAULT_INTERFACE_METRIC,
            FAKE_CONFIG_V4, FAKE_CONFIG_V6,
        },
        transport::udp::UdpStateBuilder,
        StackStateBuilder,
    };

    trait TestIpExt: crate::testutil::TestIpExt {
        fn new_icmp_connection<NonSyncCtx: NonSyncContext>(
            sync_ctx: &SyncCtx<NonSyncCtx>,
            ctx: &mut NonSyncCtx,
            local_addr: Option<SpecifiedAddr<Self::Addr>>,
            remote_addr: SpecifiedAddr<Self::Addr>,
            icmp_id: u16,
        ) -> Result<SocketId<Self>, IcmpSockCreationError>;

        fn send_icmp_echo_request<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
            sync_ctx: &SyncCtx<NonSyncCtx>,
            ctx: &mut NonSyncCtx,
            conn: SocketId<Self>,
            seq_num: u16,
            body: B,
        ) -> Result<(), IpSockSendError>;
    }

    impl TestIpExt for Ipv4 {
        fn new_icmp_connection<NonSyncCtx: NonSyncContext>(
            sync_ctx: &SyncCtx<NonSyncCtx>,
            ctx: &mut NonSyncCtx,
            local_addr: Option<SpecifiedAddr<Ipv4Addr>>,
            remote_addr: SpecifiedAddr<Ipv4Addr>,
            icmp_id: u16,
        ) -> Result<SocketId<Ipv4>, IcmpSockCreationError> {
            let unbound = create_icmpv4_unbound(sync_ctx);
            connect_icmpv4(sync_ctx, ctx, unbound, local_addr, remote_addr, icmp_id)
        }

        fn send_icmp_echo_request<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
            sync_ctx: &SyncCtx<NonSyncCtx>,
            ctx: &mut NonSyncCtx,
            conn: SocketId<Ipv4>,
            seq_num: u16,
            body: B,
        ) -> Result<(), IpSockSendError> {
            send_icmpv4_echo_request(sync_ctx, ctx, conn, seq_num, body).map_err(|(_body, err)| err)
        }
    }

    impl TestIpExt for Ipv6 {
        fn new_icmp_connection<NonSyncCtx: NonSyncContext>(
            sync_ctx: &SyncCtx<NonSyncCtx>,
            ctx: &mut NonSyncCtx,
            local_addr: Option<SpecifiedAddr<Ipv6Addr>>,
            remote_addr: SpecifiedAddr<Ipv6Addr>,
            icmp_id: u16,
        ) -> Result<SocketId<Ipv6>, IcmpSockCreationError> {
            let unbound = create_icmpv6_unbound(sync_ctx);
            connect_icmpv6(sync_ctx, ctx, unbound, local_addr, remote_addr, icmp_id)
        }

        fn send_icmp_echo_request<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
            sync_ctx: &SyncCtx<NonSyncCtx>,
            ctx: &mut NonSyncCtx,
            conn: SocketId<Ipv6>,
            seq_num: u16,
            body: B,
        ) -> Result<(), IpSockSendError> {
            send_icmpv6_echo_request(sync_ctx, ctx, conn, seq_num, body).map_err(|(_body, err)| err)
        }
    }

    // Tests that require an entire IP stack.

    /// Test that receiving a particular IP packet results in a particular ICMP
    /// response.
    ///
    /// Test that receiving an IP packet from remote host
    /// `I::FAKE_CONFIG.remote_ip` to host `dst_ip` with `ttl` and `proto`
    /// results in all of the counters in `assert_counters` being triggered at
    /// least once.
    ///
    /// If `expect_message_code` is `Some`, expect that exactly one ICMP packet
    /// was sent in response with the given message and code, and invoke the
    /// given function `f` on the packet. Otherwise, if it is `None`, expect
    /// that no response was sent.
    ///
    /// `modify_packet_builder` is invoked on the `PacketBuilder` before the
    /// packet is serialized.
    ///
    /// `modify_stack_state_builder` is invoked on the `StackStateBuilder`
    /// before it is used to build the context.
    ///
    /// The state is initialized to `I::FAKE_CONFIG` when testing.
    #[allow(clippy::too_many_arguments)]
    fn test_receive_ip_packet<
        I: TestIpExt + IcmpIpExt,
        C: PartialEq + Debug,
        M: IcmpMessage<I, Code = C> + PartialEq + Debug,
        PBF: FnOnce(&mut <I as packet_formats::ip::IpExt>::PacketBuilder),
        SSBF: FnOnce(&mut StackStateBuilder),
        F: for<'a> FnOnce(&IcmpPacket<I, &'a [u8], M>),
    >(
        modify_packet_builder: PBF,
        modify_stack_state_builder: SSBF,
        body: &mut [u8],
        dst_ip: SpecifiedAddr<I::Addr>,
        ttl: u8,
        proto: I::Proto,
        assert_counters: &[&str],
        expect_message_code: Option<(M, C)>,
        f: F,
    ) {
        crate::testutil::set_logger_for_test();
        let mut pb = <I as packet_formats::ip::IpExt>::PacketBuilder::new(
            *I::FAKE_CONFIG.remote_ip,
            dst_ip.get(),
            ttl,
            proto,
        );
        modify_packet_builder(&mut pb);
        let buffer = Buf::new(body, ..).encapsulate(pb).serialize_vec_outer().unwrap();

        let (Ctx { sync_ctx, mut non_sync_ctx }, device_ids) =
            I::FAKE_CONFIG.into_builder().build_with_modifications(modify_stack_state_builder);
        let sync_ctx = &sync_ctx;

        let device: DeviceId<_> = device_ids[0].clone().into();
        set_forwarding_enabled::<_, I>(&sync_ctx, &mut non_sync_ctx, &device, true)
            .expect("error setting routing enabled");
        match I::VERSION {
            IpVersion::V4 => receive_ip_packet::<_, _, Ipv4>(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                FrameDestination::Individual { local: true },
                buffer,
            ),
            IpVersion::V6 => receive_ip_packet::<_, _, Ipv6>(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                FrameDestination::Individual { local: true },
                buffer,
            ),
        }

        for counter in assert_counters {
            assert!(get_counter_val(&non_sync_ctx, counter) > 0, "counter at zero: {}", counter);
        }

        if let Some((expect_message, expect_code)) = expect_message_code {
            assert_eq!(non_sync_ctx.frames_sent().len(), 1);
            let (src_mac, dst_mac, src_ip, dst_ip, _, message, code) =
                parse_icmp_packet_in_ip_packet_in_ethernet_frame::<I, _, M, _>(
                    &non_sync_ctx.frames_sent()[0].1,
                    EthernetFrameLengthCheck::NoCheck,
                    f,
                )
                .unwrap();

            assert_eq!(src_mac, I::FAKE_CONFIG.local_mac.get());
            assert_eq!(dst_mac, I::FAKE_CONFIG.remote_mac.get());
            assert_eq!(src_ip, I::FAKE_CONFIG.local_ip.get());
            assert_eq!(dst_ip, I::FAKE_CONFIG.remote_ip.get());
            assert_eq!(message, expect_message);
            assert_eq!(code, expect_code);
        } else {
            assert_empty(non_sync_ctx.frames_sent().iter());
        }
    }

    #[test]
    fn test_receive_echo() {
        crate::testutil::set_logger_for_test();

        // Test that, when receiving an echo request, we respond with an echo
        // reply with the appropriate parameters.

        fn test<I: TestIpExt + IcmpIpExt>(assert_counters: &[&str]) {
            let req = IcmpEchoRequest::new(0, 0);
            let req_body = &[1, 2, 3, 4];
            let mut buffer = Buf::new(req_body.to_vec(), ..)
                .encapsulate(IcmpPacketBuilder::<I, _>::new(
                    I::FAKE_CONFIG.remote_ip.get(),
                    I::FAKE_CONFIG.local_ip.get(),
                    IcmpUnusedCode,
                    req,
                ))
                .serialize_vec_outer()
                .unwrap();
            test_receive_ip_packet::<I, _, _, _, _, _>(
                |_| {},
                |_| {},
                buffer.as_mut(),
                I::FAKE_CONFIG.local_ip,
                64,
                I::ICMP_IP_PROTO,
                assert_counters,
                Some((req.reply(), IcmpUnusedCode)),
                |packet| assert_eq!(packet.original_packet().bytes(), req_body),
            );
        }

        test::<Ipv4>(&["<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::echo_request", "send_ipv4_packet"]);
        test::<Ipv6>(&["<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet::echo_request", "send_ipv6_packet"]);
    }

    #[test]
    fn test_receive_timestamp() {
        crate::testutil::set_logger_for_test();

        let req = Icmpv4TimestampRequest::new(1, 2, 3);
        let mut buffer = Buf::new(Vec::new(), ..)
            .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                FAKE_CONFIG_V4.remote_ip,
                FAKE_CONFIG_V4.local_ip,
                IcmpUnusedCode,
                req,
            ))
            .serialize_vec_outer()
            .unwrap();
        test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
            |_| {},
            |builder| {
                let _: &mut Icmpv4StateBuilder = builder.ipv4_builder().icmpv4_builder().send_timestamp_reply(true);
            },
            buffer.as_mut(),
            FAKE_CONFIG_V4.local_ip,
            64,
            Ipv4Proto::Icmp,
            &["<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet::timestamp_request", "send_ipv4_packet"],
            Some((req.reply(0x80000000, 0x80000000), IcmpUnusedCode)),
            |_| {},
        );
    }

    #[test]
    fn test_protocol_unreachable() {
        // Test receiving an IP packet for an unreachable protocol. Check to
        // make sure that we respond with the appropriate ICMP message.
        //
        // Currently, for IPv4, we test for all unreachable protocols, while for
        // IPv6, we only test for IGMP and TCP. See the comment below for why
        // that limitation exists. Once the limitation is fixed, we should test
        // with all unreachable protocols for both versions.

        for proto in 0u8..=255 {
            let v4proto = Ipv4Proto::from(proto);
            match v4proto {
                Ipv4Proto::Other(_) => {
                    test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
                        |_| {},
                        |_| {},
                        &mut [0u8; 128],
                        FAKE_CONFIG_V4.local_ip,
                        64,
                        v4proto,
                        &["send_icmpv4_protocol_unreachable"],
                        Some((
                            IcmpDestUnreachable::default(),
                            Icmpv4DestUnreachableCode::DestProtocolUnreachable,
                        )),
                        // Ensure packet is truncated to the right length.
                        |packet| assert_eq!(packet.original_packet().bytes().len(), 84),
                    );
                }
                Ipv4Proto::Icmp
                | Ipv4Proto::Igmp
                | Ipv4Proto::Proto(IpProto::Udp)
                | Ipv4Proto::Proto(IpProto::Tcp) => {}
            }

            // TODO(fxbug.dev/47953): We seem to fail to parse an IPv6 packet if
            // its Next Header value is unrecognized (rather than treating this
            // as a valid parsing but then replying with a parameter problem
            // error message). We should a) fix this and, b) expand this test to
            // ensure we don't regress.
            let v6proto = Ipv6Proto::from(proto);
            match v6proto {
                Ipv6Proto::Icmpv6
                | Ipv6Proto::NoNextHeader
                | Ipv6Proto::Proto(IpProto::Udp)
                | Ipv6Proto::Proto(IpProto::Tcp)
                | Ipv6Proto::Other(_) => {}
            }
        }
    }

    #[test]
    fn test_port_unreachable() {
        // TODO(joshlf): Test TCP as well.

        // Receive an IP packet for an unreachable UDP port (1234). Check to
        // make sure that we respond with the appropriate ICMP message. Then, do
        // the same for a stack which has the UDP `send_port_unreachable` option
        // disable, and make sure that we DON'T respond with an ICMP message.

        fn test<I: TestIpExt + IcmpIpExt, C: PartialEq + Debug>(
            code: C,
            assert_counters: &[&str],
            original_packet_len: usize,
        ) where
            IcmpDestUnreachable:
                for<'a> IcmpMessage<I, Code = C, Body<&'a [u8]> = OriginalPacket<&'a [u8]>>,
        {
            let mut buffer = Buf::new(vec![0; 128], ..)
                .encapsulate(UdpPacketBuilder::new(
                    I::FAKE_CONFIG.remote_ip.get(),
                    I::FAKE_CONFIG.local_ip.get(),
                    None,
                    NonZeroU16::new(1234).unwrap(),
                ))
                .serialize_vec_outer()
                .unwrap();
            test_receive_ip_packet::<I, _, _, _, _, _>(
                |_| {},
                // Enable the `send_port_unreachable` feature.
                |builder| {
                    let _: &mut UdpStateBuilder =
                        builder.transport_builder().udp_builder().send_port_unreachable(true);
                },
                buffer.as_mut(),
                I::FAKE_CONFIG.local_ip,
                64,
                IpProto::Udp.into(),
                assert_counters,
                Some((IcmpDestUnreachable::default(), code)),
                // Ensure packet is truncated to the right length.
                |packet| assert_eq!(packet.original_packet().bytes().len(), original_packet_len),
            );
            test_receive_ip_packet::<I, C, IcmpDestUnreachable, _, _, _>(
                |_| {},
                // Leave the `send_port_unreachable` feature disabled.
                |_: &mut StackStateBuilder| {},
                buffer.as_mut(),
                I::FAKE_CONFIG.local_ip,
                64,
                IpProto::Udp.into(),
                &[],
                None,
                |_| {},
            );
        }

        test::<Ipv4, _>(
            Icmpv4DestUnreachableCode::DestPortUnreachable,
            &["send_icmpv4_port_unreachable"],
            84,
        );
        test::<Ipv6, _>(
            Icmpv6DestUnreachableCode::PortUnreachable,
            &["send_icmpv6_port_unreachable"],
            176,
        );
    }

    #[test]
    fn test_net_unreachable() {
        // Receive an IP packet for an unreachable destination address. Check to
        // make sure that we respond with the appropriate ICMP message.
        test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
            64,
            IpProto::Udp.into(),
            &["send_icmpv4_net_unreachable"],
            Some((
                IcmpDestUnreachable::default(),
                Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            )),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 84),
        );
        test_receive_ip_packet::<Ipv6, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv6Addr::from_bytes([
                1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8,
            ]))
            .unwrap(),
            64,
            IpProto::Udp.into(),
            &["send_icmpv6_net_unreachable"],
            Some((IcmpDestUnreachable::default(), Icmpv6DestUnreachableCode::NoRoute)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 168),
        );
        // Same test for IPv4 but with a non-initial fragment. No ICMP error
        // should be sent.
        test_receive_ip_packet::<Ipv4, _, IcmpDestUnreachable, _, _, _>(
            |pb| pb.fragment_offset(64),
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
            64,
            IpProto::Udp.into(),
            &[],
            None,
            |_| {},
        );
    }

    #[test]
    fn test_ttl_expired() {
        // Receive an IP packet with an expired TTL. Check to make sure that we
        // respond with the appropriate ICMP message.
        test_receive_ip_packet::<Ipv4, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            FAKE_CONFIG_V4.remote_ip,
            1,
            IpProto::Udp.into(),
            &["send_icmpv4_ttl_expired"],
            Some((IcmpTimeExceeded::default(), Icmpv4TimeExceededCode::TtlExpired)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 84),
        );
        test_receive_ip_packet::<Ipv6, _, _, _, _, _>(
            |_| {},
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            FAKE_CONFIG_V6.remote_ip,
            1,
            IpProto::Udp.into(),
            &["send_icmpv6_ttl_expired"],
            Some((IcmpTimeExceeded::default(), Icmpv6TimeExceededCode::HopLimitExceeded)),
            // Ensure packet is truncated to the right length.
            |packet| assert_eq!(packet.original_packet().bytes().len(), 168),
        );
        // Same test for IPv4 but with a non-initial fragment. No ICMP error
        // should be sent.
        test_receive_ip_packet::<Ipv4, _, IcmpTimeExceeded, _, _, _>(
            |pb| pb.fragment_offset(64),
            |_: &mut StackStateBuilder| {},
            &mut [0u8; 128],
            SpecifiedAddr::new(Ipv4Addr::new([1, 2, 3, 4])).unwrap(),
            64,
            IpProto::Udp.into(),
            &[],
            None,
            |_| {},
        );
    }

    #[test]
    fn test_should_send_icmpv4_error() {
        let src_ip = FAKE_CONFIG_V4.local_ip;
        let dst_ip = FAKE_CONFIG_V4.remote_ip;
        let frame_dst = FrameDestination::Individual { local: true };
        let multicast_ip_1 = SpecifiedAddr::new(Ipv4Addr::new([224, 0, 0, 1])).unwrap();
        let multicast_ip_2 = SpecifiedAddr::new(Ipv4Addr::new([224, 0, 0, 2])).unwrap();

        // Should Send, unless non initial fragment.
        assert!(should_send_icmpv4_error(
            frame_dst,
            src_ip,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because destined for IP broadcast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because destined for multicast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because Link Layer Broadcast.
        assert!(!should_send_icmpv4_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from loopback addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LOOPBACK_ADDRESS,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LOOPBACK_ADDRESS,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from limited broadcast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            Ipv4::LIMITED_BROADCAST_ADDRESS,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from multicast addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));

        // Should not send because from class E addr
        assert!(!should_send_icmpv4_error(
            frame_dst,
            SpecifiedAddr::new(Ipv4Addr::new([240, 0, 0, 1])).unwrap(),
            dst_ip,
            Ipv4FragmentType::InitialFragment
        ));
        assert!(!should_send_icmpv4_error(
            frame_dst,
            SpecifiedAddr::new(Ipv4Addr::new([240, 0, 0, 1])).unwrap(),
            dst_ip,
            Ipv4FragmentType::NonInitialFragment
        ));
    }

    #[test]
    fn test_should_send_icmpv6_error() {
        let src_ip = FAKE_CONFIG_V6.local_ip;
        let dst_ip = FAKE_CONFIG_V6.remote_ip;
        let frame_dst = FrameDestination::Individual { local: true };
        let multicast_ip_1 =
            SpecifiedAddr::new(Ipv6Addr::new([0xff00, 0, 0, 0, 0, 0, 0, 1])).unwrap();
        let multicast_ip_2 =
            SpecifiedAddr::new(Ipv6Addr::new([0xff00, 0, 0, 0, 0, 0, 0, 2])).unwrap();

        // Should Send.
        assert!(should_send_icmpv6_error(
            frame_dst, src_ip, dst_ip, false /* allow_dst_multicast */
        ));
        assert!(should_send_icmpv6_error(
            frame_dst, src_ip, dst_ip, true /* allow_dst_multicast */
        ));

        // Should not send because destined for multicast addr, unless exception
        // applies.
        assert!(!should_send_icmpv6_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            false /* allow_dst_multicast */
        ));
        assert!(should_send_icmpv6_error(
            frame_dst,
            src_ip,
            multicast_ip_1,
            true /* allow_dst_multicast */
        ));

        // Should not send because Link Layer Broadcast, unless exception
        // applies.
        assert!(!should_send_icmpv6_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(should_send_icmpv6_error(
            FrameDestination::Broadcast,
            src_ip,
            dst_ip,
            true /* allow_dst_multicast */
        ));

        // Should not send because from loopback addr.
        assert!(!should_send_icmpv6_error(
            frame_dst,
            Ipv6::LOOPBACK_ADDRESS,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            Ipv6::LOOPBACK_ADDRESS,
            dst_ip,
            true /* allow_dst_multicast */
        ));

        // Should not send because from multicast addr.
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            dst_ip,
            true /* allow_dst_multicast */
        ));

        // Should not send because from multicast addr, even though dest
        // multicast exception applies.
        assert!(!should_send_icmpv6_error(
            FrameDestination::Broadcast,
            multicast_ip_2,
            dst_ip,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            FrameDestination::Broadcast,
            multicast_ip_2,
            dst_ip,
            true /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            multicast_ip_1,
            false /* allow_dst_multicast */
        ));
        assert!(!should_send_icmpv6_error(
            frame_dst,
            multicast_ip_2,
            multicast_ip_1,
            true /* allow_dst_multicast */
        ));
    }

    enum IcmpConnectionType {
        Local,
        Remote,
    }

    fn test_icmp_connection<I: Ip + TestIpExt>(conn_type: IcmpConnectionType) {
        crate::testutil::set_logger_for_test();

        let recv_icmp_packet_name = match I::VERSION {
            IpVersion::V4 => {
                "<IcmpIpTransportContext as BufferIpTransportContext<Ipv4>>::receive_ip_packet"
            }
            IpVersion::V6 => {
                "<IcmpIpTransportContext as BufferIpTransportContext<Ipv6>>::receive_ip_packet"
            }
        };

        let config = I::FAKE_CONFIG;

        const LOCAL_CTX_NAME: &str = "alice";
        const REMOTE_CTX_NAME: &str = "bob";
        let (local, local_device_ids) = I::FAKE_CONFIG.into_builder().build();
        let (remote, remove_device_ids) = I::FAKE_CONFIG.swap().into_builder().build();
        let mut net = crate::context::testutil::new_simple_fake_network(
            LOCAL_CTX_NAME,
            local,
            local_device_ids[0].downgrade(),
            REMOTE_CTX_NAME,
            remote,
            remove_device_ids[0].downgrade(),
        );
        core::mem::drop((local_device_ids, remove_device_ids));

        let icmp_id = 13;

        let (remote_addr, ctx_name_receiving_req) = match conn_type {
            IcmpConnectionType::Local => (config.local_ip, LOCAL_CTX_NAME),
            IcmpConnectionType::Remote => (config.remote_ip, REMOTE_CTX_NAME),
        };

        let loopback_device_id =
            net.with_context(LOCAL_CTX_NAME, |Ctx { sync_ctx, non_sync_ctx: _ }| {
                crate::device::add_loopback_device(
                    &&*sync_ctx,
                    Mtu::new(u16::MAX as u32),
                    DEFAULT_INTERFACE_METRIC,
                )
                .expect("create the loopback interface")
                .into()
            });

        let echo_body = vec![1, 2, 3, 4];
        let conn = net.with_context(LOCAL_CTX_NAME, |Ctx { sync_ctx, non_sync_ctx }| {
            crate::device::testutil::enable_device(&&*sync_ctx, non_sync_ctx, &loopback_device_id);

            let conn = I::new_icmp_connection(
                sync_ctx,
                non_sync_ctx,
                Some(config.local_ip),
                remote_addr,
                icmp_id,
            )
            .unwrap();

            I::send_icmp_echo_request(
                sync_ctx,
                non_sync_ctx,
                conn,
                7,
                Buf::new(echo_body.clone(), ..),
            )
            .unwrap();

            handle_queued_rx_packets(sync_ctx, non_sync_ctx);

            conn
        });

        net.run_until_idle(
            crate::device::testutil::receive_frame,
            |Ctx { sync_ctx, non_sync_ctx }, _, id| {
                crate::handle_timer(&&*sync_ctx, non_sync_ctx, id);
                handle_queued_rx_packets(sync_ctx, non_sync_ctx);
            },
        );

        assert_eq!(
            get_counter_val(
                net.non_sync_ctx(LOCAL_CTX_NAME),
                &format!("{}::echo_reply", recv_icmp_packet_name)
            ),
            1
        );
        assert_eq!(
            get_counter_val(
                net.non_sync_ctx(ctx_name_receiving_req),
                &format!("{}::echo_request", recv_icmp_packet_name)
            ),
            1
        );
        let replies = net.non_sync_ctx(LOCAL_CTX_NAME).take_icmp_replies(conn);
        assert_matches!(&replies[..], [(7, body)] if *body == echo_body);
    }

    #[ip_test]
    fn test_local_icmp_connection<I: Ip + TestIpExt>() {
        test_icmp_connection::<I>(IcmpConnectionType::Local);
    }

    #[ip_test]
    fn test_remote_icmp_connection<I: Ip + TestIpExt>() {
        test_icmp_connection::<I>(IcmpConnectionType::Remote);
    }

    // Tests that only require an ICMP stack. Unlike the preceding tests, these
    // only test the ICMP stack and state, and fake everything else. We define
    // the `FakeIcmpv4Ctx` and `FakeIcmpv6Ctx` types, which we wrap in a
    // `FakeCtx` to provide automatic implementations of a number of required
    // traits. The rest we implement manually.

    // The arguments to `InnerIcmpContext::send_icmp_reply`.
    #[derive(Debug, PartialEq)]
    struct SendIcmpReplyArgs<A: IpAddress> {
        device: Option<FakeDeviceId>,
        src_ip: SpecifiedAddr<A>,
        dst_ip: SpecifiedAddr<A>,
        body: Vec<u8>,
    }

    // The arguments to `InnerIcmpContext::send_icmp_error_message`.
    #[derive(Debug, PartialEq)]
    struct SendIcmpErrorMessageArgs<I: IcmpIpExt> {
        src_ip: SpecifiedAddr<I::Addr>,
        dst_ip: SpecifiedAddr<I::Addr>,
        body: Vec<u8>,
        ip_mtu: Option<u32>,
    }

    // The arguments to `BufferIcmpContext::receive_icmp_echo_reply`.
    #[allow(unused)] // TODO(joshlf): Remove once we access these fields.
    struct ReceiveIcmpEchoReply<I: Ip> {
        conn: SocketId<I>,
        seq_num: u16,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        id: u16,
        data: Vec<u8>,
    }

    // The arguments to `IcmpContext::receive_icmp_error`.
    #[derive(Debug, PartialEq)]
    struct ReceiveIcmpSocketErrorArgs<I: IcmpIpExt> {
        conn: SocketId<I>,
        seq_num: u16,
        err: I::ErrorCode,
    }

    struct FakeIcmpCtx<I: IcmpIpExt + IpDeviceStateIpExt, D> {
        receive_icmp_error: Vec<I::ErrorCode>,
        pmtu_state: FakePmtuState<I::Addr>,
        socket_ctx: FakeIpSocketCtx<I, D>,
    }

    impl Default for FakeIcmpCtx<Ipv4, FakeDeviceId> {
        fn default() -> Self {
            FakeIcmpCtx::new(FakeIpSocketCtx::new_fake(
                vec![FAKE_CONFIG_V4.local_ip],
                vec![FAKE_CONFIG_V4.remote_ip],
            ))
        }
    }

    impl Default for FakeIcmpCtx<Ipv6, FakeDeviceId> {
        fn default() -> Self {
            FakeIcmpCtx::new(FakeIpSocketCtx::new_fake(
                vec![FAKE_CONFIG_V6.local_ip],
                vec![FAKE_CONFIG_V6.remote_ip],
            ))
        }
    }

    impl<I: IcmpIpExt + IpDeviceStateIpExt, D> FakeIcmpCtx<I, D> {
        fn new(socket_ctx: FakeIpSocketCtx<I, D>) -> Self {
            FakeIcmpCtx {
                receive_icmp_error: Vec::new(),
                pmtu_state: FakePmtuState::default(),
                socket_ctx,
            }
        }
    }

    struct FakeIcmpv4Ctx {
        inner: FakeIcmpCtx<Ipv4, FakeDeviceId>,
        sockets: IcmpSockets<Ipv4, FakeWeakDeviceId<FakeDeviceId>>,
        error_send_bucket: TokenBucket<FakeInstant>,
    }

    struct FakeIcmpv6Ctx {
        inner: FakeIcmpCtx<Ipv6, FakeDeviceId>,
        sockets: IcmpSockets<Ipv6, FakeWeakDeviceId<FakeDeviceId>>,
        error_send_bucket: TokenBucket<FakeInstant>,
    }

    impl Default for FakeIcmpv4Ctx {
        fn default() -> FakeIcmpv4Ctx {
            FakeIcmpv4Ctx {
                inner: FakeIcmpCtx::default(),
                sockets: Default::default(),
                error_send_bucket: TokenBucket::new(DEFAULT_ERRORS_PER_SECOND),
            }
        }
    }

    impl Default for FakeIcmpv6Ctx {
        fn default() -> FakeIcmpv6Ctx {
            FakeIcmpv6Ctx {
                inner: FakeIcmpCtx::default(),
                sockets: Default::default(),
                error_send_bucket: TokenBucket::new(DEFAULT_ERRORS_PER_SECOND),
            }
        }
    }

    #[derive(Default)]
    struct FakeIcmpNonSyncCtxState<I: IcmpIpExt> {
        receive_icmp_echo_reply: Vec<ReceiveIcmpEchoReply<I>>,
        receive_icmp_socket_error: Vec<ReceiveIcmpSocketErrorArgs<I>>,
    }

    /// Implement a number of traits and methods for the `$inner` and `$outer`
    /// context types.
    macro_rules! impl_context_traits {
        ($ip:ident, $inner:ident, $outer_ctx:ident, $outer_sync_ctx:ident, $outer_non_sync_ctx:ident, $state:ident) => {
            type $outer_ctx = FakeCtx<
                $inner,
                (),
                SendIpPacketMeta<$ip, FakeDeviceId, SpecifiedAddr<<$ip as Ip>::Addr>>,
                (),
                FakeDeviceId,
                FakeIcmpNonSyncCtxState<$ip>,
            >;

            type $outer_sync_ctx = FakeSyncCtx<
                $inner,
                SendIpPacketMeta<$ip, FakeDeviceId, SpecifiedAddr<<$ip as Ip>::Addr>>,
                FakeDeviceId,
            >;

            type $outer_non_sync_ctx = FakeNonSyncCtx<(), (), FakeIcmpNonSyncCtxState<$ip>>;

            impl $inner {
                fn with_errors_per_second(errors_per_second: u64) -> $inner {
                    let mut ctx = $inner::default();
                    ctx.error_send_bucket = TokenBucket::new(errors_per_second);
                    ctx
                }
            }

            impl_pmtu_handler!($outer_sync_ctx, $outer_non_sync_ctx, $ip);

            impl AsMut<FakePmtuState<<$ip as Ip>::Addr>> for $outer_sync_ctx {
                fn as_mut(&mut self) -> &mut FakePmtuState<<$ip as Ip>::Addr> {
                    &mut self.get_mut().inner.pmtu_state
                }
            }

            impl AsRef<FakeIpDeviceIdCtx<FakeDeviceId>> for $inner {
                fn as_ref(&self) -> &FakeIpDeviceIdCtx<FakeDeviceId> {
                    self.inner.socket_ctx.as_ref()
                }
            }

            impl AsRef<FakeIpSocketCtx<$ip, FakeDeviceId>> for $inner {
                fn as_ref(&self) -> &FakeIpSocketCtx<$ip, FakeDeviceId> {
                    &self.inner.socket_ctx
                }
            }

            impl AsMut<FakeIpSocketCtx<$ip, FakeDeviceId>> for $inner {
                fn as_mut(&mut self) -> &mut FakeIpSocketCtx<$ip, FakeDeviceId> {
                    &mut self.inner.socket_ctx
                }
            }

            impl IcmpContext<$ip> for $outer_non_sync_ctx {
                fn receive_icmp_error(
                    &mut self,
                    conn: SocketId<$ip>,
                    seq_num: u16,
                    err: <$ip as IcmpIpExt>::ErrorCode,
                ) {
                    self.state_mut()
                        .receive_icmp_socket_error
                        .push(ReceiveIcmpSocketErrorArgs { conn, seq_num, err });
                }
            }

            impl<B: BufferMut> BufferIcmpContext<$ip, B> for $outer_non_sync_ctx {
                fn receive_icmp_echo_reply(
                    &mut self,
                    conn: SocketId<$ip>,
                    src_ip: <$ip as Ip>::Addr,
                    dst_ip: <$ip as Ip>::Addr,
                    id: u16,
                    seq_num: u16,
                    data: B,
                ) {
                    self.state_mut().receive_icmp_echo_reply.push(ReceiveIcmpEchoReply {
                        conn,
                        src_ip,
                        dst_ip,
                        id,
                        seq_num,
                        data: data.as_ref().to_vec(),
                    });
                }
            }

            impl InnerIcmpContext<$ip, $outer_non_sync_ctx> for $outer_sync_ctx {
                type IpSocketsCtx<'a> = FakeIpSocketCtx<$ip, FakeDeviceId>;

                fn receive_icmp_error(
                    &mut self,
                    ctx: &mut $outer_non_sync_ctx,
                    device: &FakeDeviceId,
                    original_src_ip: Option<SpecifiedAddr<<$ip as Ip>::Addr>>,
                    original_dst_ip: SpecifiedAddr<<$ip as Ip>::Addr>,
                    original_proto: <$ip as packet_formats::ip::IpProtoExt>::Proto,
                    original_body: &[u8],
                    err: <$ip as IcmpIpExt>::ErrorCode,
                ) {
                    ctx.increment_debug_counter("InnerIcmpContext::receive_icmp_error");
                    self.get_mut().inner.receive_icmp_error.push(err);
                    if original_proto == <$ip as packet_formats::icmp::IcmpIpExt>::ICMP_IP_PROTO {
                        <IcmpIpTransportContext as IpTransportContext<$ip, _, _>>::receive_icmp_error(
                            self,
                            ctx,
                            device,
                            original_src_ip,
                            original_dst_ip,
                            original_body,
                            err,
                        );
                    }
                }

                fn with_icmp_sockets<O, F: FnOnce(&IcmpSockets<$ip, FakeWeakDeviceId<FakeDeviceId>>) -> O>(
                    &mut self,
                    cb: F,
                ) -> O {
                    cb(&self.get_ref().sockets)
                }

                fn with_icmp_ctx_and_sockets_mut<
                    O,
                    F: FnOnce(
                        &mut Self::IpSocketsCtx<'_>,
                        &mut IcmpSockets<$ip, FakeWeakDeviceId<FakeDeviceId>>,
                    ) -> O,
                >(
                    &mut self,
                    cb: F,
                ) -> O {
                    let ctx = self.get_mut();
                    cb(&mut ctx.inner.socket_ctx, &mut ctx.sockets)
                }

                fn with_error_send_bucket_mut<O, F: FnOnce(&mut TokenBucket<FakeInstant>) -> O>(
                    &mut self,
                    cb: F,
                ) -> O {
                    cb(&mut self.get_mut().error_send_bucket)
                }
            }
        };
    }

    impl_context_traits!(
        Ipv4,
        FakeIcmpv4Ctx,
        Fakev4Ctx,
        Fakev4SyncCtx,
        Fakev4NonSyncCtx,
        Icmpv4State
    );

    impl InnerIcmpv4Context<Fakev4NonSyncCtx> for Fakev4SyncCtx {
        fn should_send_timestamp_reply(&self) -> bool {
            false
        }
    }

    impl_context_traits!(
        Ipv6,
        FakeIcmpv6Ctx,
        Fakev6Ctx,
        Fakev6SyncCtx,
        Fakev6NonSyncCtx,
        Icmpv6State
    );

    impl IpDeviceHandler<Ipv6, Fakev6NonSyncCtx> for Fakev6SyncCtx {
        fn is_router_device(&mut self, _device_id: &Self::DeviceId) -> bool {
            unimplemented!()
        }

        fn set_default_hop_limit(&mut self, _device_id: &Self::DeviceId, _hop_limit: NonZeroU8) {
            unreachable!()
        }
    }

    impl IpDeviceStateContext<Ipv6, Fakev6NonSyncCtx> for Fakev6SyncCtx {
        fn get_local_addr_for_remote(
            &mut self,
            _device_id: &Self::DeviceId,
            _remote: Option<SpecifiedAddr<Ipv6Addr>>,
        ) -> Option<SpecifiedAddr<Ipv6Addr>> {
            unimplemented!()
        }

        fn get_hop_limit(&mut self, _device_id: &Self::DeviceId) -> NonZeroU8 {
            unimplemented!()
        }

        fn address_status_for_device(
            &mut self,
            _addr: SpecifiedAddr<Ipv6Addr>,
            _device_id: &Self::DeviceId,
        ) -> AddressStatus<Ipv6PresentAddressStatus> {
            unimplemented!()
        }
    }

    impl Ipv6DeviceHandler<Fakev6NonSyncCtx> for Fakev6SyncCtx {
        type LinkLayerAddr = [u8; 0];

        fn get_link_layer_addr_bytes(&mut self, _device_id: &Self::DeviceId) -> Option<[u8; 0]> {
            unimplemented!()
        }

        fn set_discovered_retrans_timer(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
            _retrans_timer: NonZeroDuration,
        ) {
            unimplemented!()
        }

        fn remove_duplicate_tentative_address(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
            _addr: UnicastAddr<Ipv6Addr>,
        ) -> IpAddressState {
            unimplemented!()
        }

        fn set_link_mtu(&mut self, _device_id: &Self::DeviceId, _mtu: Mtu) {
            unimplemented!()
        }

        fn update_discovered_ipv6_route(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
            _route: Ipv6DiscoveredRoute,
            _lifetime: Option<NonZeroNdpLifetime>,
        ) {
            unimplemented!()
        }

        fn apply_slaac_update(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
            _subnet: Subnet<Ipv6Addr>,
            _preferred_lifetime: Option<NonZeroNdpLifetime>,
            _valid_lifetime: Option<NonZeroNdpLifetime>,
        ) {
            unimplemented!()
        }

        fn receive_mld_packet<B: ByteSlice>(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device: &FakeDeviceId,
            _src_ip: Ipv6SourceAddr,
            _dst_ip: SpecifiedAddr<Ipv6Addr>,
            _packet: MldPacket<B>,
        ) {
            unimplemented!()
        }
    }

    impl<B: BufferMut> BufferIpLayerHandler<Ipv6, Fakev6NonSyncCtx, B> for Fakev6SyncCtx {
        fn send_ip_packet_from_device<S: Serializer<Buffer = B>>(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _meta: SendIpPacketMeta<Ipv6, &Self::DeviceId, Option<SpecifiedAddr<Ipv6Addr>>>,
            _body: S,
        ) -> Result<(), S> {
            unimplemented!()
        }
    }

    impl NudIpHandler<Ipv6, Fakev6NonSyncCtx> for Fakev6SyncCtx {
        fn handle_neighbor_probe(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
            _neighbor: SpecifiedAddr<Ipv6Addr>,
            _link_addr: &[u8],
        ) {
            unimplemented!()
        }

        fn handle_neighbor_confirmation(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
            _neighbor: SpecifiedAddr<Ipv6Addr>,
            _link_addr: &[u8],
            _flags: ConfirmationFlags,
        ) {
            unimplemented!()
        }

        fn flush_neighbor_table(
            &mut self,
            _ctx: &mut Fakev6NonSyncCtx,
            _device_id: &Self::DeviceId,
        ) {
            unimplemented!()
        }
    }

    #[test]
    fn test_receive_icmpv4_error() {
        // Chosen arbitrarily to be a) non-zero (it's easy to accidentally get
        // the value 0) and, b) different from each other.
        const ICMP_ID: u16 = 0x0F;
        const SEQ_NUM: u16 = 0xF0;

        /// Test receiving an ICMP error message.
        ///
        /// Test that receiving an ICMP error message with the given code and
        /// message contents, and containing the given original IPv4 packet,
        /// results in the counter values in `assert_counters`. After that
        /// assertion passes, `f` is called on the context so that the caller
        /// can perform whatever extra validation they want.
        ///
        /// The error message will be sent from `FAKE_CONFIG_V4.remote_ip` to
        /// `FAKE_CONFIG_V4.local_ip`. Before the message is sent, an ICMP
        /// socket will be established with the ID `ICMP_ID`, and
        /// `test_receive_icmpv4_error_helper` will assert that its `SocketId`
        /// is 0. This allows the caller to craft the `original_packet` so that
        /// it should be delivered to this socket.
        fn test_receive_icmpv4_error_helper<
            C: Debug,
            M: IcmpMessage<Ipv4, Code = C> + Debug,
            F: Fn(&Fakev4Ctx),
        >(
            original_packet: &mut [u8],
            code: C,
            msg: M,
            assert_counters: &[(&str, usize)],
            f: F,
        ) {
            crate::testutil::set_logger_for_test();

            let mut ctx = Fakev4Ctx::default();
            let FakeCtx { sync_ctx, non_sync_ctx } = &mut ctx;

            // NOTE: This assertion is not a correctness requirement. It's just
            // that the rest of this test assumes that the new connection has ID
            // 0. If this assertion fails in the future, that isn't necessarily
            // evidence of a bug; we may just have to update this test to
            // accommodate whatever new ID allocation scheme is being used.
            let unbound = create_icmpv4_unbound_inner(sync_ctx);
            assert_eq!(
                connect_icmpv4_inner(
                    sync_ctx,
                    non_sync_ctx,
                    unbound,
                    Some(FAKE_CONFIG_V4.local_ip),
                    FAKE_CONFIG_V4.remote_ip,
                    ICMP_ID
                )
                .unwrap(),
                SocketId::new(0)
            );

            <IcmpIpTransportContext as BufferIpTransportContext<Ipv4, _, _, _>>::receive_ip_packet(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FAKE_CONFIG_V4.remote_ip.get(),
                FAKE_CONFIG_V4.local_ip,
                Buf::new(original_packet, ..)
                    .encapsulate(IcmpPacketBuilder::new(
                        FAKE_CONFIG_V4.remote_ip,
                        FAKE_CONFIG_V4.local_ip,
                        code,
                        msg,
                    ))
                    .serialize_vec_outer()
                    .unwrap(),
            )
            .unwrap();

            for (ctr, count) in assert_counters {
                assert_eq!(
                    non_sync_ctx.counter_ctx().get_counter_val(ctr),
                    *count,
                    "wrong count for counter {}",
                    ctr
                );
            }
            f(&ctx);
        }
        // Test that, when we receive various ICMPv4 error messages, we properly
        // pass them up to the IP layer and, sometimes, to the transport layer.

        // First, test with an original packet containing an ICMP message. Since
        // this test fake supports ICMP sockets, this error can be delivered all
        // the way up the stack.

        // A buffer containing an ICMP echo request with ID `ICMP_ID` and
        // sequence number `SEQ_NUM` from the local IP to the remote IP. Any
        // ICMP error message which contains this as its original packet should
        // be delivered to the socket created in
        // `test_receive_icmpv4_error_helper`.
        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(IcmpPacketBuilder::<Ipv4, _>::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                IcmpUnusedCode,
                IcmpEchoRequest::new(ICMP_ID, SEQ_NUM),
            ))
            .encapsulate(<Ipv4 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                64,
                Ipv4Proto::Icmp,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 1),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::DestUnreachable(
                    Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(
                    non_sync_ctx.state().receive_icmp_socket_error,
                    [ReceiveIcmpSocketErrorArgs { conn: SocketId::new(0), seq_num: SEQ_NUM, err }]
                );
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4TimeExceededCode::TtlExpired,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 1),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::TimeExceeded(Icmpv4TimeExceededCode::TtlExpired);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(
                    non_sync_ctx.state().receive_icmp_socket_error,
                    [ReceiveIcmpSocketErrorArgs { conn: SocketId::new(0), seq_num: SEQ_NUM, err }]
                );
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4ParameterProblemCode::PointerIndicatesError,
            Icmpv4ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 1),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::ParameterProblem(
                    Icmpv4ParameterProblemCode::PointerIndicatesError,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(
                    non_sync_ctx.state().receive_icmp_socket_error,
                    [ReceiveIcmpSocketErrorArgs { conn: SocketId::new(0), seq_num: SEQ_NUM, err }]
                );
            },
        );

        // Second, test with an original packet containing a malformed ICMP
        // packet (we accomplish this by leaving the IP packet's body empty). We
        // should process this packet in
        // `IcmpIpTransportContext::receive_icmp_error`, but we should go no
        // further - in particular, we should not call
        // `IcmpContext::receive_icmp_error`.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv4 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                64,
                Ipv4Proto::Icmp,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::DestUnreachable(
                    Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4TimeExceededCode::TtlExpired,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::TimeExceeded(Icmpv4TimeExceededCode::TtlExpired);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4ParameterProblemCode::PointerIndicatesError,
            Icmpv4ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::ParameterProblem(
                    Icmpv4ParameterProblemCode::PointerIndicatesError,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        // Third, test with an original packet containing a UDP packet. This
        // allows us to verify that protocol numbers are handled properly by
        // checking that `IcmpIpTransportContext::receive_icmp_error` was NOT
        // called.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv4 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V4.local_ip,
                FAKE_CONFIG_V4.remote_ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4DestUnreachableCode::DestNetworkUnreachable,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::DestUnreachable(
                    Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4TimeExceededCode::TtlExpired,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::TimeExceeded(Icmpv4TimeExceededCode::TtlExpired);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv4_error_helper(
            buffer.as_mut(),
            Icmpv4ParameterProblemCode::PointerIndicatesError,
            Icmpv4ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev4Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv4ErrorCode::ParameterProblem(
                    Icmpv4ParameterProblemCode::PointerIndicatesError,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );
    }

    #[test]
    fn test_receive_icmpv6_error() {
        // Chosen arbitrarily to be a) non-zero (it's easy to accidentally get
        // the value 0) and, b) different from each other.
        const ICMP_ID: u16 = 0x0F;
        const SEQ_NUM: u16 = 0xF0;

        /// Test receiving an ICMPv6 error message.
        ///
        /// Test that receiving an ICMP error message with the given code and
        /// message contents, and containing the given original IPv4 packet,
        /// results in the counter values in `assert_counters`. After that
        /// assertion passes, `f` is called on the context so that the caller
        /// can perform whatever extra validation they want.
        ///
        /// The error message will be sent from `FAKE_CONFIG_V6.remote_ip` to
        /// `FAKE_CONFIG_V6.local_ip`. Before the message is sent, an ICMP
        /// socket will be established with the ID `ICMP_ID`, and
        /// `test_receive_icmpv6_error_helper` will assert that its `SocketId`
        /// is 0. This allows the caller to craft the `original_packet` so that
        /// it should be delivered to this socket.
        fn test_receive_icmpv6_error_helper<
            C: Debug,
            M: IcmpMessage<Ipv6, Code = C> + Debug,
            F: Fn(&Fakev6Ctx),
        >(
            original_packet: &mut [u8],
            code: C,
            msg: M,
            assert_counters: &[(&str, usize)],
            f: F,
        ) {
            crate::testutil::set_logger_for_test();

            let mut ctx = Fakev6Ctx::default();
            let FakeCtx { sync_ctx, non_sync_ctx } = &mut ctx;
            let unbound = create_icmpv6_unbound_inner(sync_ctx);
            // NOTE: This assertion is not a correctness requirement. It's just
            // that the rest of this test assumes that the new connection has ID
            // 0. If this assertion fails in the future, that isn't necessarily
            // evidence of a bug; we may just have to update this test to
            // accommodate whatever new ID allocation scheme is being used.
            assert_eq!(
                connect_icmpv6_inner(
                    sync_ctx,
                    non_sync_ctx,
                    unbound,
                    Some(FAKE_CONFIG_V6.local_ip),
                    FAKE_CONFIG_V6.remote_ip,
                    ICMP_ID
                )
                .unwrap(),
                SocketId::new(0)
            );

            <IcmpIpTransportContext as BufferIpTransportContext<Ipv6, _, _, _>>::receive_ip_packet(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FAKE_CONFIG_V6.remote_ip.get().try_into().unwrap(),
                FAKE_CONFIG_V6.local_ip,
                Buf::new(original_packet, ..)
                    .encapsulate(IcmpPacketBuilder::new(
                        FAKE_CONFIG_V6.remote_ip,
                        FAKE_CONFIG_V6.local_ip,
                        code,
                        msg,
                    ))
                    .serialize_vec_outer()
                    .unwrap(),
            )
            .unwrap();

            for (ctr, count) in assert_counters {
                assert_eq!(
                    non_sync_ctx.counter_ctx().get_counter_val(ctr),
                    *count,
                    "wrong count for counter {}",
                    ctr
                );
            }
            f(&ctx);
        }
        // Test that, when we receive various ICMPv6 error messages, we properly
        // pass them up to the IP layer and, sometimes, to the transport layer.

        // First, test with an original packet containing an ICMPv6 message.
        // Since this test fake supports ICMPv6 sockets, this error can be
        // delivered all the way up the stack.

        // A buffer containing an ICMPv6 echo request with ID `ICMP_ID` and
        // sequence number `SEQ_NUM` from the local IP to the remote IP. Any
        // ICMPv6 error message which contains this as its original packet
        // should be delivered to the socket created in
        // `test_receive_icmpv6_error_helper`.
        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(IcmpPacketBuilder::<Ipv6, _>::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                IcmpUnusedCode,
                IcmpEchoRequest::new(ICMP_ID, SEQ_NUM),
            ))
            .encapsulate(<Ipv6 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                64,
                Ipv6Proto::Icmpv6,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6DestUnreachableCode::NoRoute,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 1),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(
                    non_sync_ctx.state().receive_icmp_socket_error,
                    [ReceiveIcmpSocketErrorArgs { conn: SocketId::new(0), seq_num: SEQ_NUM, err }]
                );
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6TimeExceededCode::HopLimitExceeded,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 1),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::TimeExceeded(Icmpv6TimeExceededCode::HopLimitExceeded);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(
                    non_sync_ctx.state().receive_icmp_socket_error,
                    [ReceiveIcmpSocketErrorArgs { conn: SocketId::new(0), seq_num: SEQ_NUM, err }]
                );
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
            Icmpv6ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 1),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::ParameterProblem(
                    Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(
                    non_sync_ctx.state().receive_icmp_socket_error,
                    [ReceiveIcmpSocketErrorArgs { conn: SocketId::new(0), seq_num: SEQ_NUM, err }]
                );
            },
        );

        // Second, test with an original packet containing a malformed ICMPv6
        // packet (we accomplish this by leaving the IP packet's body empty). We
        // should process this packet in
        // `IcmpIpTransportContext::receive_icmp_error`, but we should go no
        // further - in particular, we should not call
        // `IcmpContext::receive_icmp_error`.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv6 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                64,
                Ipv6Proto::Icmpv6,
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6DestUnreachableCode::NoRoute,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6TimeExceededCode::HopLimitExceeded,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::TimeExceeded(Icmpv6TimeExceededCode::HopLimitExceeded);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
            Icmpv6ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 1),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::ParameterProblem(
                    Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        // Third, test with an original packet containing a UDP packet. This
        // allows us to verify that protocol numbers are handled properly by
        // checking that `IcmpIpTransportContext::receive_icmp_error` was NOT
        // called.

        let mut buffer = Buf::new(&mut [], ..)
            .encapsulate(<Ipv6 as packet_formats::ip::IpExt>::PacketBuilder::new(
                FAKE_CONFIG_V6.local_ip,
                FAKE_CONFIG_V6.remote_ip,
                64,
                IpProto::Udp.into(),
            ))
            .serialize_vec_outer()
            .unwrap();

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6DestUnreachableCode::NoRoute,
            IcmpDestUnreachable::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6TimeExceededCode::HopLimitExceeded,
            IcmpTimeExceeded::default(),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::TimeExceeded(Icmpv6TimeExceededCode::HopLimitExceeded);
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );

        test_receive_icmpv6_error_helper(
            buffer.as_mut(),
            Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
            Icmpv6ParameterProblem::new(0),
            &[
                ("InnerIcmpContext::receive_icmp_error", 1),
                ("IcmpIpTransportContext::receive_icmp_error", 0),
                ("IcmpContext::receive_icmp_error", 0),
            ],
            |Fakev6Ctx { sync_ctx, non_sync_ctx }| {
                let err = Icmpv6ErrorCode::ParameterProblem(
                    Icmpv6ParameterProblemCode::UnrecognizedNextHeaderType,
                );
                assert_eq!(sync_ctx.get_ref().inner.receive_icmp_error, [err]);
                assert_eq!(non_sync_ctx.state().receive_icmp_socket_error, []);
            },
        );
    }

    #[test]
    fn test_error_rate_limit() {
        crate::testutil::set_logger_for_test();

        /// Call `send_icmpv4_ttl_expired` with fake values.
        fn send_icmpv4_ttl_expired_helper(FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev4Ctx) {
            send_icmpv4_ttl_expired(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V4.remote_ip,
                FAKE_CONFIG_V4.local_ip,
                IpProto::Udp.into(),
                Buf::new(&mut [], ..),
                0,
                Ipv4FragmentType::InitialFragment,
            );
        }

        /// Call `send_icmpv4_parameter_problem` with fake values.
        fn send_icmpv4_parameter_problem_helper(
            FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev4Ctx,
        ) {
            send_icmpv4_parameter_problem(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V4.remote_ip,
                FAKE_CONFIG_V4.local_ip,
                Icmpv4ParameterProblemCode::PointerIndicatesError,
                Icmpv4ParameterProblem::new(0),
                Buf::new(&mut [], ..),
                0,
                Ipv4FragmentType::InitialFragment,
            );
        }

        /// Call `send_icmpv4_dest_unreachable` with fake values.
        fn send_icmpv4_dest_unreachable_helper(FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev4Ctx) {
            send_icmpv4_dest_unreachable(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                FAKE_CONFIG_V4.remote_ip,
                FAKE_CONFIG_V4.local_ip,
                Icmpv4DestUnreachableCode::DestNetworkUnreachable,
                Buf::new(&mut [], ..),
                0,
                Ipv4FragmentType::InitialFragment,
            );
        }

        /// Call `send_icmpv6_ttl_expired` with fake values.
        fn send_icmpv6_ttl_expired_helper(FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev6Ctx) {
            send_icmpv6_ttl_expired(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                UnicastAddr::from_witness(FAKE_CONFIG_V6.remote_ip).unwrap(),
                FAKE_CONFIG_V6.local_ip,
                IpProto::Udp.into(),
                Buf::new(&mut [], ..),
                0,
            );
        }

        /// Call `send_icmpv6_packet_too_big` with fake values.
        fn send_icmpv6_packet_too_big_helper(FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev6Ctx) {
            send_icmpv6_packet_too_big(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                UnicastAddr::from_witness(FAKE_CONFIG_V6.remote_ip).unwrap(),
                FAKE_CONFIG_V6.local_ip,
                IpProto::Udp.into(),
                Mtu::new(0),
                Buf::new(&mut [], ..),
                0,
            );
        }

        /// Call `send_icmpv6_parameter_problem` with fake values.
        fn send_icmpv6_parameter_problem_helper(
            FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev6Ctx,
        ) {
            send_icmpv6_parameter_problem(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                UnicastAddr::new(*FAKE_CONFIG_V6.remote_ip).expect("unicast source address"),
                FAKE_CONFIG_V6.local_ip,
                Icmpv6ParameterProblemCode::ErroneousHeaderField,
                Icmpv6ParameterProblem::new(0),
                Buf::new(&mut [], ..),
                false,
            );
        }

        /// Call `send_icmpv6_dest_unreachable` with fake values.
        fn send_icmpv6_dest_unreachable_helper(FakeCtx { sync_ctx, non_sync_ctx }: &mut Fakev6Ctx) {
            send_icmpv6_dest_unreachable(
                sync_ctx,
                non_sync_ctx,
                &FakeDeviceId,
                FrameDestination::Individual { local: true },
                UnicastAddr::from_witness(FAKE_CONFIG_V6.remote_ip).unwrap(),
                FAKE_CONFIG_V6.local_ip,
                Icmpv6DestUnreachableCode::NoRoute,
                Buf::new(&mut [], ..),
            );
        }

        // Run tests for each function that sends error messages to make sure
        // they're all properly rate limited.

        fn run_test<
            I: IpExt,
            C,
            W: Fn(
                u64,
            ) -> FakeCtx<
                C,
                (),
                SendIpPacketMeta<I, FakeDeviceId, SpecifiedAddr<I::Addr>>,
                (),
                FakeDeviceId,
                FakeIcmpNonSyncCtxState<I>,
            >,
            S: Fn(
                &mut FakeCtx<
                    C,
                    (),
                    SendIpPacketMeta<I, FakeDeviceId, SpecifiedAddr<I::Addr>>,
                    (),
                    FakeDeviceId,
                    FakeIcmpNonSyncCtxState<I>,
                >,
            ),
        >(
            with_errors_per_second: W,
            send: S,
            counter_str: &str,
        ) {
            // Note that we could theoretically have more precise tests here
            // (e.g., a test that we send at the correct rate over the long
            // term), but those would amount to testing the `TokenBucket`
            // implementation, which has its own exhaustive tests. Instead, we
            // just have a few sanity checks to make sure that we're actually
            // invoking it when we expect to (as opposed to bypassing it
            // entirely or something).

            // Test that, if no time has elapsed, we can successfully send up to
            // `ERRORS_PER_SECOND` error messages, but no more.

            // Don't use `DEFAULT_ERRORS_PER_SECOND` because it's 2^16 and it
            // makes this test take a long time.
            const ERRORS_PER_SECOND: u64 = 64;

            let mut ctx = with_errors_per_second(ERRORS_PER_SECOND);

            for i in 0..ERRORS_PER_SECOND {
                send(&mut ctx);
                assert_eq!(
                    ctx.non_sync_ctx.counter_ctx().get_counter_val(counter_str),
                    i as usize + 1
                );
            }

            assert_eq!(
                ctx.non_sync_ctx.counter_ctx().get_counter_val(counter_str),
                ERRORS_PER_SECOND as usize
            );
            send(&mut ctx);
            assert_eq!(
                ctx.non_sync_ctx.counter_ctx().get_counter_val(counter_str),
                ERRORS_PER_SECOND as usize
            );

            // Test that, if we set a rate of 0, we are not able to send any
            // error messages regardless of how much time has elapsed.

            let mut ctx = with_errors_per_second(0);
            send(&mut ctx);
            assert_eq!(ctx.non_sync_ctx.counter_ctx().get_counter_val(counter_str), 0);
            ctx.non_sync_ctx.sleep_skip_timers(Duration::from_secs(1));
            send(&mut ctx);
            assert_eq!(ctx.non_sync_ctx.counter_ctx().get_counter_val(counter_str), 0);
            ctx.non_sync_ctx.sleep_skip_timers(Duration::from_secs(1));
            send(&mut ctx);
            assert_eq!(ctx.non_sync_ctx.counter_ctx().get_counter_val(counter_str), 0);
        }

        fn with_errors_per_second_v4(errors_per_second: u64) -> Fakev4Ctx {
            Fakev4Ctx::with_state(FakeIcmpv4Ctx::with_errors_per_second(errors_per_second))
        }
        run_test::<Ipv4, _, _, _>(
            with_errors_per_second_v4,
            send_icmpv4_ttl_expired_helper,
            SEND_ICMPV4_ERROR_MESSAGE_COUNTER_NAME,
        );
        run_test::<Ipv4, _, _, _>(
            with_errors_per_second_v4,
            send_icmpv4_parameter_problem_helper,
            SEND_ICMPV4_ERROR_MESSAGE_COUNTER_NAME,
        );
        run_test::<Ipv4, _, _, _>(
            with_errors_per_second_v4,
            send_icmpv4_dest_unreachable_helper,
            SEND_ICMPV4_ERROR_MESSAGE_COUNTER_NAME,
        );

        fn with_errors_per_second_v6(errors_per_second: u64) -> Fakev6Ctx {
            Fakev6Ctx::with_state(FakeIcmpv6Ctx::with_errors_per_second(errors_per_second))
        }

        run_test::<Ipv6, _, _, _>(
            with_errors_per_second_v6,
            send_icmpv6_ttl_expired_helper,
            SEND_ICMPV6_ERROR_MESSAGE_COUNTER_NAME,
        );
        run_test::<Ipv6, _, _, _>(
            with_errors_per_second_v6,
            send_icmpv6_packet_too_big_helper,
            SEND_ICMPV6_ERROR_MESSAGE_COUNTER_NAME,
        );
        run_test::<Ipv6, _, _, _>(
            with_errors_per_second_v6,
            send_icmpv6_parameter_problem_helper,
            SEND_ICMPV6_ERROR_MESSAGE_COUNTER_NAME,
        );
        run_test::<Ipv6, _, _, _>(
            with_errors_per_second_v6,
            send_icmpv6_dest_unreachable_helper,
            SEND_ICMPV6_ERROR_MESSAGE_COUNTER_NAME,
        );
    }

    #[test]
    fn test_connect_dual_stack_fails() {
        // Verify that connecting to an ipv4-mapped-ipv6 address fails, as ICMP
        // sockets do not support dual-stack operations.
        const ICMP_ID: u16 = 0x0F;
        let mut ctx = Fakev6Ctx::default();
        let FakeCtx { sync_ctx, non_sync_ctx } = &mut ctx;
        let unbound = create_icmpv6_unbound_inner(sync_ctx);
        assert_eq!(
            connect_icmpv6_inner(
                sync_ctx,
                non_sync_ctx,
                unbound,
                Some(FAKE_CONFIG_V6.local_ip),
                SpecifiedAddr::new(net_ip_v6!("::ffff:192.0.2.1")).unwrap(),
                ICMP_ID
            ),
            Err(IcmpSockCreationError::RemoteAddrIsMapped)
        );
    }
}
