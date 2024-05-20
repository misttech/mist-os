// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IPv4 and IPv6 sockets.

use core::cmp::Ordering;
use core::convert::Infallible;
use core::num::{NonZeroU32, NonZeroU8};

use net_types::{
    ip::{Ip, Ipv6Addr, Ipv6SourceAddr, Mtu},
    SpecifiedAddr,
};
use packet::{BufferMut, SerializeError};
use thiserror::Error;

use crate::{
    context::{CounterContext, InstantContext, TracingContext},
    device::{
        AnyDevice, DeviceIdContext, EitherDeviceId, StrongDeviceIdentifier,
        WeakDeviceIdentifier as _,
    },
    filter::{
        FilterBindingsTypes, FilterHandler as _, FilterHandlerProvider, TransportPacketSerializer,
    },
    ip::{
        device::{state::IpDeviceStateIpExt, IpDeviceAddr},
        types::{ResolvedRoute, RoutableIpAddr},
        IpCounters, IpDeviceContext, IpExt, IpLayerIpExt, IpLayerPacketMetadata, ResolveRouteError,
        SendIpPacketMeta,
    },
    socket::{SocketIpAddr, SocketIpAddrExt as _},
    trace_duration,
};

/// An execution context defining a type of IP socket.
pub trait IpSocketHandler<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Constructs a new [`IpSock`].
    ///
    /// `new_ip_socket` constructs a new `IpSock` to the given remote IP
    /// address from the given local IP address with the given IP protocol. If
    /// no local IP address is given, one will be chosen automatically. If
    /// `device` is `Some`, the socket will be bound to the given device - only
    /// routes which egress over the device will be used. If no route is
    /// available which egresses over the device - even if routes are available
    /// which egress over other devices - the socket will be considered
    /// unroutable.
    ///
    /// `new_ip_socket` returns an error if no route to the remote was found in
    /// the forwarding table or if the given local IP address is not valid for
    /// the found route.
    fn new_ip_socket(
        &mut self,
        bindings_ctx: &mut BC,
        device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
        local_ip: Option<SocketIpAddr<I::Addr>>,
        remote_ip: SocketIpAddr<I::Addr>,
        proto: I::Proto,
    ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError>;

    /// Sends an IP packet on a socket.
    ///
    /// The generated packet has its metadata initialized from `socket`,
    /// including the source and destination addresses, the Time To Live/Hop
    /// Limit, and the Protocol/Next Header. The outbound device is also chosen
    /// based on information stored in the socket.
    ///
    /// `mtu` may be used to optionally impose an MTU on the outgoing packet.
    /// Note that the device's MTU will still be imposed on the packet. That is,
    /// the smaller of `mtu` and the device's MTU will be imposed on the packet.
    ///
    /// If the socket is currently unroutable, an error is returned.
    fn send_ip_packet<S, O>(
        &mut self,
        bindings_ctx: &mut BC,
        socket: &IpSock<I, Self::WeakDeviceId>,
        body: S,
        mtu: Option<u32>,
        options: &O,
    ) -> Result<(), (S, IpSockSendError)>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
        O: SendOptions<I>;

    /// Creates a temporary IP socket and sends a single packet on it.
    ///
    /// `local_ip`, `remote_ip`, `proto`, and `options` are passed directly to
    /// [`IpSocketHandler::new_ip_socket`]. `get_body_from_src_ip` is given the
    /// source IP address for the packet - which may have been chosen
    /// automatically if `local_ip` is `None` - and returns the body to be
    /// encapsulated. This is provided in case the body's contents depend on the
    /// chosen source IP address.
    ///
    /// If `device` is specified, the available routes are limited to those that
    /// egress over the device.
    ///
    /// `mtu` may be used to optionally impose an MTU on the outgoing packet.
    /// Note that the device's MTU will still be imposed on the packet. That is,
    /// the smaller of `mtu` and the device's MTU will be imposed on the packet.
    ///
    /// # Errors
    ///
    /// If an error is encountered while constructing the temporary IP socket
    /// or sending the packet, `options` will be returned along with the
    /// error. `get_body_from_src_ip` is fallible, and if there's an error,
    /// it will be returned as well.
    fn send_oneshot_ip_packet_with_fallible_serializer<S, E, F, O>(
        &mut self,
        bindings_ctx: &mut BC,
        device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
        local_ip: Option<IpDeviceAddr<I::Addr>>,
        remote_ip: RoutableIpAddr<I::Addr>,
        proto: I::Proto,
        options: &O,
        get_body_from_src_ip: F,
        mtu: Option<u32>,
    ) -> Result<(), SendOneShotIpPacketError<E>>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
        F: FnOnce(SocketIpAddr<I::Addr>) -> Result<S, E>,
        O: SendOptions<I>,
    {
        let tmp = self
            .new_ip_socket(bindings_ctx, device, local_ip, remote_ip, proto)
            .map_err(|err| SendOneShotIpPacketError::CreateAndSendError { err: err.into() })?;
        let packet = get_body_from_src_ip(*tmp.local_ip())
            .map_err(SendOneShotIpPacketError::SerializeError)?;
        self.send_ip_packet(bindings_ctx, &tmp, packet, mtu, options).map_err(|(_body, err)| {
            SendOneShotIpPacketError::CreateAndSendError { err: err.into() }
        })
    }

    /// Sends a one-shot IP packet but with a non-fallible serializer.
    fn send_oneshot_ip_packet<S, F, O>(
        &mut self,
        bindings_ctx: &mut BC,
        device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
        local_ip: Option<SocketIpAddr<I::Addr>>,
        remote_ip: SocketIpAddr<I::Addr>,
        proto: I::Proto,
        options: &O,
        get_body_from_src_ip: F,
        mtu: Option<u32>,
    ) -> Result<(), IpSockCreateAndSendError>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
        F: FnOnce(SocketIpAddr<I::Addr>) -> S,
        O: SendOptions<I>,
    {
        self.send_oneshot_ip_packet_with_fallible_serializer(
            bindings_ctx,
            device,
            local_ip,
            remote_ip,
            proto,
            options,
            |ip| Ok::<_, Infallible>(get_body_from_src_ip(ip)),
            mtu,
        )
        .map_err(|err| match err {
            SendOneShotIpPacketError::CreateAndSendError { err } => err,
            SendOneShotIpPacketError::SerializeError(infallible) => match infallible {},
        })
    }
}

/// An error in sending a packet on an IP socket.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum IpSockSendError {
    /// An MTU was exceeded.
    ///
    /// This could be caused by an MTU at any layer of the stack, including both
    /// device MTUs and packet format body size limits.
    #[error("a maximum transmission unit (MTU) was exceeded")]
    Mtu,
    /// The socket is currently unroutable.
    #[error("the socket is currently unroutable: {}", _0)]
    Unroutable(#[from] ResolveRouteError),
}

impl From<SerializeError<Infallible>> for IpSockSendError {
    fn from(err: SerializeError<Infallible>) -> IpSockSendError {
        match err {
            SerializeError::Alloc(err) => match err {},
            SerializeError::SizeLimitExceeded => IpSockSendError::Mtu,
        }
    }
}

/// An error in sending a packet on a temporary IP socket.
#[derive(Error, Copy, Clone, Debug)]
pub enum IpSockCreateAndSendError {
    /// Cannot send via temporary socket.
    #[error("cannot send via temporary socket: {}", _0)]
    Send(#[from] IpSockSendError),
    /// The temporary socket could not be created.
    #[error("the temporary socket could not be created: {}", _0)]
    Create(#[from] IpSockCreationError),
}

#[derive(Debug)]
pub enum SendOneShotIpPacketError<E> {
    CreateAndSendError { err: IpSockCreateAndSendError },
    SerializeError(E),
}

/// Maximum packet size, that is the maximum IP payload one packet can carry.
///
/// More details from https://www.rfc-editor.org/rfc/rfc1122#section-3.3.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mms(NonZeroU32);

impl Mms {
    pub(crate) fn from_mtu<I: IpExt>(mtu: Mtu, options_size: u32) -> Option<Self> {
        NonZeroU32::new(mtu.get().saturating_sub(I::IP_HEADER_LENGTH.get() + options_size))
            .map(|mms| Self(mms.min(I::IP_MAX_PAYLOAD_LENGTH)))
    }

    pub(crate) fn get(&self) -> NonZeroU32 {
        let Self(mms) = *self;
        mms
    }
}

/// Possible errors when retrieving the maximum transport message size.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MmsError {
    /// Cannot find the device that is used for the ip socket, possibly because
    /// there is no route.
    #[error("cannot find the device: {}", _0)]
    NoDevice(#[from] ResolveRouteError),
    /// The MTU provided by the device is too small such that there is no room
    /// for a transport message at all.
    #[error("invalid MTU: {:?}", _0)]
    MTUTooSmall(Mtu),
}

/// Gets device related information of an IP socket.
pub trait DeviceIpSocketHandler<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Gets the maximum message size for the transport layer, it equals the
    /// device MTU minus the IP header size.
    ///
    /// This corresponds to the GET_MAXSIZES call described in:
    /// https://www.rfc-editor.org/rfc/rfc1122#section-3.4
    fn get_mms(
        &mut self,
        bindings_ctx: &mut BC,
        ip_sock: &IpSock<I, Self::WeakDeviceId>,
    ) -> Result<Mms, MmsError>;
}

/// An error encountered when creating an IP socket.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum IpSockCreationError {
    /// An error occurred while looking up a route.
    #[error("a route cannot be determined: {}", _0)]
    Route(#[from] ResolveRouteError),
}

/// An IP socket.
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct IpSock<I: IpExt, D> {
    /// The definition of the socket.
    ///
    /// This does not change for the lifetime of the socket.
    definition: IpSockDefinition<I, D>,
}

impl<I: IpExt, D> IpSock<I, D> {
    #[cfg(any(test, feature = "testutils"))]
    pub fn definition(&self) -> &IpSockDefinition<I, D> {
        &self.definition
    }
}

/// The definition of an IP socket.
///
/// These values are part of the socket's definition, and never change.
#[derive(Clone, Debug, PartialEq)]
pub struct IpSockDefinition<I: IpExt, D> {
    pub remote_ip: SocketIpAddr<I::Addr>,
    // Guaranteed to be unicast in its subnet since it's always equal to an
    // address assigned to the local device. We can't use the `UnicastAddr`
    // witness type since `Ipv4Addr` doesn't implement `UnicastAddress`.
    //
    // TODO(joshlf): Support unnumbered interfaces. Once we do that, a few
    // issues arise: A) Does the unicast restriction still apply, and is that
    // even well-defined for IPv4 in the absence of a subnet? B) Presumably we
    // have to always bind to a particular interface?
    pub local_ip: SocketIpAddr<I::Addr>,
    pub device: Option<D>,
    pub proto: I::Proto,
}

impl<I: IpExt, D> IpSock<I, D> {
    pub(crate) fn local_ip(&self) -> &SocketIpAddr<I::Addr> {
        &self.definition.local_ip
    }

    pub(crate) fn remote_ip(&self) -> &SocketIpAddr<I::Addr> {
        &self.definition.remote_ip
    }

    pub(crate) fn device(&self) -> Option<&D> {
        self.definition.device.as_ref()
    }

    pub(crate) fn proto(&self) -> I::Proto {
        self.definition.proto
    }
}

// TODO(joshlf): Once we support configuring transport-layer protocols using
// type parameters, use that to ensure that `proto` is the right protocol for
// the caller. We will still need to have a separate enforcement mechanism for
// raw IP sockets once we support those.

/// The bindings execution context for IP sockets.
pub trait IpSocketBindingsContext: InstantContext + TracingContext + FilterBindingsTypes {}
impl<BC: InstantContext + TracingContext + FilterBindingsTypes> IpSocketBindingsContext for BC {}

/// The context required in order to implement [`IpSocketHandler`].
///
/// Blanket impls of `IpSocketHandler` are provided in terms of
/// `IpSocketContext`.
pub trait IpSocketContext<I, BC: IpSocketBindingsContext>:
    DeviceIdContext<AnyDevice> + FilterHandlerProvider<I, BC>
where
    I: IpDeviceStateIpExt + IpExt,
{
    /// Returns a route for a socket.
    ///
    /// If `device` is specified, the available routes are limited to those that
    /// egress over the device.
    fn lookup_route(
        &mut self,
        bindings_ctx: &mut BC,
        device: Option<&Self::DeviceId>,
        src_ip: Option<IpDeviceAddr<I::Addr>>,
        dst_ip: RoutableIpAddr<I::Addr>,
    ) -> Result<ResolvedRoute<I, Self::DeviceId>, ResolveRouteError>;

    /// Send an IP packet to the next-hop node.
    fn send_ip_packet<S>(
        &mut self,
        bindings_ctx: &mut BC,
        meta: SendIpPacketMeta<I, &Self::DeviceId, SpecifiedAddr<I::Addr>>,
        body: S,
        packet_metadata: IpLayerPacketMetadata<I, BC>,
    ) -> Result<(), S>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut;
}

/// Enables a blanket implementation of [`IpSocketHandler`].
///
/// Implementing this marker trait for a type enables a blanket implementation
/// of `IpSocketHandler` given the other requirements are met.
pub trait UseIpSocketHandlerBlanket {}

impl<I, BC, CC> IpSocketHandler<I, BC> for CC
where
    I: IpLayerIpExt + IpDeviceStateIpExt,
    BC: IpSocketBindingsContext,
    CC: IpSocketContext<I, BC> + CounterContext<IpCounters<I>> + UseIpSocketHandlerBlanket,
    CC::DeviceId: crate::filter::InterfaceProperties<BC::DeviceClass>,
{
    fn new_ip_socket(
        &mut self,
        bindings_ctx: &mut BC,
        device: Option<EitherDeviceId<&CC::DeviceId, &CC::WeakDeviceId>>,
        local_ip: Option<SocketIpAddr<I::Addr>>,
        remote_ip: SocketIpAddr<I::Addr>,
        proto: I::Proto,
    ) -> Result<IpSock<I, CC::WeakDeviceId>, IpSockCreationError> {
        let device = device
            .as_ref()
            .map(|d| d.as_strong_ref().ok_or(ResolveRouteError::Unreachable))
            .transpose()?;
        let device = device.as_ref().map(|d| d.as_ref());

        // Make sure the remote is routable with a local address before creating
        // the socket. We do not care about the actual destination here because
        // we will recalculate it when we send a packet so that the best route
        // available at the time is used for each outgoing packet.
        let resolved_route = self.lookup_route(bindings_ctx, device, local_ip, remote_ip)?;
        Ok(new_ip_socket(device, resolved_route, remote_ip, proto))
    }

    fn send_ip_packet<S, O>(
        &mut self,
        bindings_ctx: &mut BC,
        ip_sock: &IpSock<I, CC::WeakDeviceId>,
        body: S,
        mtu: Option<u32>,
        options: &O,
    ) -> Result<(), (S, IpSockSendError)>
    where
        S: TransportPacketSerializer<I>,
        S::Buffer: BufferMut,
        O: SendOptions<I>,
    {
        // TODO(joshlf): Call `trace!` with relevant fields from the socket.
        self.increment(|counters| &counters.send_ip_packet);

        send_ip_packet(self, bindings_ctx, ip_sock, body, mtu, options)
    }
}

/// Provides hooks for altering sending behavior of [`IpSock`].
///
/// Must be implemented by the socket option type of an `IpSock` when using it
/// to call [`IpSocketHandler::send_ip_packet`]. This is implemented as a trait
/// instead of an inherent impl on a type so that users of sockets that don't
/// need certain option types, like TCP for anything multicast-related, can
/// avoid allocating space for those options.
pub trait SendOptions<I: Ip> {
    /// Returns the hop limit to set on a packet going to the given destination.
    ///
    /// If `Some(u)`, `u` will be used as the hop limit (IPv6) or TTL (IPv4) for
    /// a packet going to the given destination. Otherwise the default value
    /// will be used.
    fn hop_limit(&self, destination: &SpecifiedAddr<I::Addr>) -> Option<NonZeroU8>;
}

/// Empty send options that never overrides default values.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DefaultSendOptions;

impl<I: Ip> SendOptions<I> for DefaultSendOptions {
    fn hop_limit(&self, _destination: &SpecifiedAddr<I::Addr>) -> Option<NonZeroU8> {
        None
    }
}

fn new_ip_socket<I, D>(
    requested_device: Option<&D>,
    route: ResolvedRoute<I, D>,
    remote_ip: SocketIpAddr<I::Addr>,
    proto: I::Proto,
) -> IpSock<I, D::Weak>
where
    I: IpExt,
    D: StrongDeviceIdentifier,
{
    // TODO(https://fxbug.dev/323389672): Cache a reference to the route to
    // avoid the route lookup on send as long as the routing table hasn't
    // changed in between these operations.
    let ResolvedRoute { src_addr, device: route_device, local_delivery_device, next_hop: _ } =
        route;

    // If the source or destination address require a device, make sure to
    // set that in the socket definition. Otherwise defer to what was provided.
    let socket_device = (src_addr.as_ref().must_have_zone() || remote_ip.as_ref().must_have_zone())
        .then(|| {
            // NB: The route device might be loopback, and in such cases
            // we want to bind the socket to the device the source IP is
            // assigned to instead.
            local_delivery_device.unwrap_or(route_device)
        })
        .as_ref()
        .or(requested_device)
        .map(|d| d.downgrade());

    let definition =
        IpSockDefinition { local_ip: src_addr, remote_ip, device: socket_device, proto };
    IpSock { definition }
}

fn send_ip_packet<I, S, BC, CC, O>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    socket: &IpSock<I, CC::WeakDeviceId>,
    mut body: S,
    mtu: Option<u32>,
    options: &O,
) -> Result<(), (S, IpSockSendError)>
where
    I: IpExt + IpDeviceStateIpExt + packet_formats::ip::IpExt,
    S: TransportPacketSerializer<I>,
    S::Buffer: BufferMut,
    BC: IpSocketBindingsContext,
    CC: IpSocketContext<I, BC>,
    CC::DeviceId: crate::filter::InterfaceProperties<BC::DeviceClass>,
    O: SendOptions<I> + ?Sized,
{
    trace_duration!(bindings_ctx, c"ip::send_packet");

    let IpSock { definition: IpSockDefinition { remote_ip, local_ip, device, proto } } = socket;
    let device = match device.as_ref().map(|d| d.upgrade()) {
        Some(Some(device)) => Some(device),
        Some(None) => return Err((body, ResolveRouteError::Unreachable.into())),
        None => None,
    };

    let ResolvedRoute { src_addr: got_local_ip, local_delivery_device: _, device, next_hop } =
        match core_ctx.lookup_route(bindings_ctx, device.as_ref(), Some(*local_ip), *remote_ip) {
            Ok(o) => o,
            Err(e) => return Err((body, IpSockSendError::Unroutable(e))),
        };
    assert_eq!(local_ip, &got_local_ip);

    // TODO(https://fxbug.dev/318717702): when we implement NAT, perform re-routing
    // after the LOCAL_EGRESS hook since the packet may have been changed.
    let mut packet =
        crate::filter::TxPacket::new(local_ip.addr(), remote_ip.addr(), *proto, &mut body);

    let mut packet_metadata = IpLayerPacketMetadata::default();
    match core_ctx.filter_handler().local_egress_hook(&mut packet, &device, &mut packet_metadata) {
        crate::filter::Verdict::Drop => {
            packet_metadata.acknowledge_drop();
            return Ok(());
        }
        crate::filter::Verdict::Accept => {}
    }

    let remote_ip: SpecifiedAddr<_> = (*remote_ip).into();
    let local_ip: SpecifiedAddr<_> = (*local_ip).into();

    let (next_hop, broadcast) = next_hop.into_next_hop_and_broadcast_marker(remote_ip);

    IpSocketContext::send_ip_packet(
        core_ctx,
        bindings_ctx,
        SendIpPacketMeta {
            device: &device,
            src_ip: local_ip,
            dst_ip: remote_ip,
            broadcast,
            next_hop,
            ttl: options.hop_limit(&remote_ip),
            proto: *proto,
            mtu,
        },
        body,
        packet_metadata,
    )
    .map_err(|s| (s, IpSockSendError::Mtu))
}

/// Enables a blanket implementation of [`DeviceIpSocketHandler`].
///
/// Implementing this marker trait for a type enables a blanket implementation
/// of `DeviceIpSocketHandler` given the other requirements are met.
pub trait UseDeviceIpSocketHandlerBlanket {}

impl<I, BC, CC> DeviceIpSocketHandler<I, BC> for CC
where
    I: IpLayerIpExt + IpDeviceStateIpExt,
    BC: IpSocketBindingsContext,
    CC: IpDeviceContext<I, BC> + IpSocketContext<I, BC> + UseDeviceIpSocketHandlerBlanket,
{
    fn get_mms(
        &mut self,
        bindings_ctx: &mut BC,
        ip_sock: &IpSock<I, Self::WeakDeviceId>,
    ) -> Result<Mms, MmsError> {
        let IpSockDefinition { remote_ip, local_ip, device, proto: _ } = &ip_sock.definition;
        let device = device
            .as_ref()
            .map(|d| d.upgrade().ok_or(ResolveRouteError::Unreachable))
            .transpose()?;

        let ResolvedRoute { src_addr: _, local_delivery_device: _, device, next_hop: _ } = self
            .lookup_route(bindings_ctx, device.as_ref(), Some(*local_ip), *remote_ip)
            .map_err(MmsError::NoDevice)?;
        let mtu = IpDeviceContext::<I, BC>::get_mtu(self, &device);
        // TODO(https://fxbug.dev/42072935): Calculate the options size when they
        // are supported.
        Mms::from_mtu::<I>(mtu, 0 /* no ip options used */).ok_or(MmsError::MTUTooSmall(mtu))
    }
}

/// IPv6 source address selection as defined in [RFC 6724 Section 5].
pub(crate) mod ipv6_source_address_selection {
    use net_types::ip::{AddrSubnet, IpAddress as _};

    use super::*;

    use crate::ip::device::{state::Ipv6AddressFlags, Ipv6DeviceAddr};

    pub(crate) struct SasCandidate<D> {
        pub(crate) addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        pub(crate) flags: Ipv6AddressFlags,
        pub(crate) device: D,
    }

    /// Selects the source address for an IPv6 socket using the algorithm
    /// defined in [RFC 6724 Section 5].
    ///
    /// This algorithm is only applicable when the user has not explicitly
    /// specified a source address.
    ///
    /// `remote_ip` is the remote IP address of the socket, `outbound_device` is
    /// the device over which outbound traffic to `remote_ip` is sent (according
    /// to the forwarding table), and `addresses` is an iterator of all
    /// addresses on all devices. The algorithm works by iterating over
    /// `addresses` and selecting the address which is most preferred according
    /// to a set of selection criteria.
    pub(crate) fn select_ipv6_source_address<
        'a,
        D: PartialEq,
        I: Iterator<Item = SasCandidate<D>>,
    >(
        remote_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        outbound_device: &D,
        addresses: I,
    ) -> Ipv6SourceAddr {
        // Source address selection as defined in RFC 6724 Section 5.
        //
        // The algorithm operates by defining a partial ordering on available
        // source addresses, and choosing one of the best address as defined by
        // that ordering (given multiple best addresses, the choice from among
        // those is implementation-defined). The partial order is defined in
        // terms of a sequence of rules. If a given rule defines an order
        // between two addresses, then that is their order. Otherwise, the next
        // rule must be consulted, and so on until all of the rules are
        // exhausted.

        let addr = addresses
            // Tentative addresses are not considered available to the source
            // selection algorithm.
            .filter(|SasCandidate { addr_sub: _, flags, device: _ }| flags.assigned)
            .max_by(|a, b| select_ipv6_source_address_cmp(remote_ip, outbound_device, a, b))
            .map(|SasCandidate { addr_sub, flags: _, device: _ }| addr_sub.addr());
        match addr {
            Some(addr) => Ipv6SourceAddr::Unicast(addr),
            None => Ipv6SourceAddr::Unspecified,
        }
    }

    /// Comparison operator used by `select_ipv6_source_address`.
    fn select_ipv6_source_address_cmp<D: PartialEq>(
        remote_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        outbound_device: &D,
        a: &SasCandidate<D>,
        b: &SasCandidate<D>,
    ) -> Ordering {
        // TODO(https://fxbug.dev/42123500): Implement rules 2, 4, 5.5, 6, and 7.

        let a_addr = a.addr_sub.addr().into_specified();
        let b_addr = b.addr_sub.addr().into_specified();

        // Assertions required in order for this implementation to be valid.

        // Required by the implementation of Rule 1.
        if let Some(remote_ip) = remote_ip {
            debug_assert!(!(a_addr == remote_ip && b_addr == remote_ip));
        }

        // Addresses that are not considered assigned are not valid source
        // addresses.
        debug_assert!(a.flags.assigned);
        debug_assert!(b.flags.assigned);

        rule_1(remote_ip, a_addr, b_addr)
            .then_with(|| rule_3(a.flags.deprecated, b.flags.deprecated))
            .then_with(|| rule_5(outbound_device, &a.device, &b.device))
            .then_with(|| rule_8(remote_ip, a.addr_sub, b.addr_sub))
    }

    // Assumes that `a` and `b` are not both equal to `remote_ip`.
    fn rule_1(
        remote_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        a: SpecifiedAddr<Ipv6Addr>,
        b: SpecifiedAddr<Ipv6Addr>,
    ) -> Ordering {
        let remote_ip = match remote_ip {
            Some(remote_ip) => remote_ip,
            None => return Ordering::Equal,
        };
        if (a == remote_ip) != (b == remote_ip) {
            // Rule 1: Prefer same address.
            //
            // Note that both `a` and `b` cannot be equal to `remote_ip` since
            // that would imply that we had added the same address twice to the
            // same device.
            //
            // If `(a == remote_ip) != (b == remote_ip)`, then exactly one of
            // them is equal. If this inequality does not hold, then they must
            // both be unequal to `remote_ip`. In the first case, we have a tie,
            // and in the second case, the rule doesn't apply. In either case,
            // we move onto the next rule.
            if a == remote_ip {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        } else {
            Ordering::Equal
        }
    }

    fn rule_3(a_deprecated: bool, b_deprecated: bool) -> Ordering {
        match (a_deprecated, b_deprecated) {
            (true, false) => Ordering::Less,
            (true, true) | (false, false) => Ordering::Equal,
            (false, true) => Ordering::Greater,
        }
    }

    fn rule_5<D: PartialEq>(outbound_device: &D, a_device: &D, b_device: &D) -> Ordering {
        if (a_device == outbound_device) != (b_device == outbound_device) {
            // Rule 5: Prefer outgoing interface.
            if a_device == outbound_device {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        } else {
            Ordering::Equal
        }
    }

    fn rule_8(
        remote_ip: Option<SpecifiedAddr<Ipv6Addr>>,
        a: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        b: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    ) -> Ordering {
        let remote_ip = match remote_ip {
            Some(remote_ip) => remote_ip,
            None => return Ordering::Equal,
        };
        // Per RFC 6724 Section 2.2:
        //
        //   We define the common prefix length CommonPrefixLen(S, D) of a
        //   source address S and a destination address D as the length of the
        //   longest prefix (looking at the most significant, or leftmost, bits)
        //   that the two addresses have in common, up to the length of S's
        //   prefix (i.e., the portion of the address not including the
        //   interface ID).  For example, CommonPrefixLen(fe80::1, fe80::2) is
        //   64.
        fn common_prefix_len(
            src: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
            dst: SpecifiedAddr<Ipv6Addr>,
        ) -> u8 {
            core::cmp::min(src.addr().common_prefix_len(&dst), src.subnet().prefix())
        }

        // Rule 8: Use longest matching prefix.
        //
        // Note that, per RFC 6724 Section 5:
        //
        //   Rule 8 MAY be superseded if the implementation has other means of
        //   choosing among source addresses.  For example, if the
        //   implementation somehow knows which source address will result in
        //   the "best" communications performance.
        //
        // We don't currently make use of this option, but it's an option for
        // the future.
        common_prefix_len(a, remote_ip).cmp(&common_prefix_len(b, remote_ip))
    }

    #[cfg(test)]
    mod tests {
        use net_declare::net_ip_v6;

        use super::*;

        #[test]
        fn test_select_ipv6_source_address() {
            // Test the comparison operator used by `select_ipv6_source_address`
            // by separately testing each comparison condition.

            let remote = SpecifiedAddr::new(net_ip_v6!("2001:0db8:1::")).unwrap();
            let local0 = SpecifiedAddr::new(net_ip_v6!("2001:0db8:2::")).unwrap();
            let local1 = SpecifiedAddr::new(net_ip_v6!("2001:0db8:3::")).unwrap();
            let dev0 = &0;
            let dev1 = &1;
            let dev2 = &2;

            // Rule 1: Prefer same address
            assert_eq!(rule_1(Some(remote), remote, local0), Ordering::Greater);
            assert_eq!(rule_1(Some(remote), local0, remote), Ordering::Less);
            assert_eq!(rule_1(Some(remote), local0, local1), Ordering::Equal);
            assert_eq!(rule_1(None, local0, local1), Ordering::Equal);

            // Rule 3: Avoid deprecated states
            assert_eq!(rule_3(false, true), Ordering::Greater);
            assert_eq!(rule_3(true, false), Ordering::Less);
            assert_eq!(rule_3(true, true), Ordering::Equal);
            assert_eq!(rule_3(false, false), Ordering::Equal);

            // Rule 5: Prefer outgoing interface
            assert_eq!(rule_5(dev0, dev0, dev2), Ordering::Greater);
            assert_eq!(rule_5(dev0, dev2, dev0), Ordering::Less);
            assert_eq!(rule_5(dev0, dev0, dev0), Ordering::Equal);
            assert_eq!(rule_5(dev0, dev2, dev2), Ordering::Equal);

            // Rule 8: Use longest matching prefix.
            {
                let new_addr_entry = |addr, prefix_len| AddrSubnet::new(addr, prefix_len).unwrap();

                // First, test that the longest prefix match is preferred when
                // using addresses whose common prefix length is shorter than
                // the subnet prefix length.

                // 4 leading 0x01 bytes.
                let remote = SpecifiedAddr::new(net_ip_v6!("1111::")).unwrap();
                // 3 leading 0x01 bytes.
                let local0 = new_addr_entry(net_ip_v6!("1110::"), 64);
                // 2 leading 0x01 bytes.
                let local1 = new_addr_entry(net_ip_v6!("1100::"), 64);

                assert_eq!(rule_8(Some(remote), local0, local1), Ordering::Greater);
                assert_eq!(rule_8(Some(remote), local1, local0), Ordering::Less);
                assert_eq!(rule_8(Some(remote), local0, local0), Ordering::Equal);
                assert_eq!(rule_8(Some(remote), local1, local1), Ordering::Equal);
                assert_eq!(rule_8(None, local0, local1), Ordering::Equal);

                // Second, test that the common prefix length is capped at the
                // subnet prefix length.

                // 3 leading 0x01 bytes, but a subnet prefix length of 8 (1 byte).
                let local0 = new_addr_entry(net_ip_v6!("1110::"), 8);
                // 2 leading 0x01 bytes, but a subnet prefix length of 8 (1 byte).
                let local1 = new_addr_entry(net_ip_v6!("1100::"), 8);

                assert_eq!(rule_8(Some(remote), local0, local1), Ordering::Equal);
                assert_eq!(rule_8(Some(remote), local1, local0), Ordering::Equal);
                assert_eq!(rule_8(Some(remote), local0, local0), Ordering::Equal);
                assert_eq!(rule_8(Some(remote), local1, local1), Ordering::Equal);
                assert_eq!(rule_8(None, local0, local1), Ordering::Equal);
            }

            {
                let new_addr_entry = |addr, device| SasCandidate {
                    addr_sub: AddrSubnet::new(addr, 128).unwrap(),
                    flags: Ipv6AddressFlags { deprecated: false, assigned: true },
                    device,
                };

                // If no rules apply, then the two address entries are equal.
                assert_eq!(
                    select_ipv6_source_address_cmp(
                        Some(remote),
                        dev0,
                        &new_addr_entry(*local0, *dev1),
                        &new_addr_entry(*local1, *dev2),
                    ),
                    Ordering::Equal
                );
            }
        }

        #[test]
        fn test_select_ipv6_source_address_no_remote() {
            // Verify that source address selection correctly applies all
            // applicable rules when the remote is `None`.
            let dev0 = &0;
            let dev1 = &1;
            let dev2 = &2;

            let local0 = SpecifiedAddr::new(net_ip_v6!("2001:0db8:2::")).unwrap();
            let local1 = SpecifiedAddr::new(net_ip_v6!("2001:0db8:3::")).unwrap();

            let new_addr_entry = |addr, deprecated, device| SasCandidate {
                addr_sub: AddrSubnet::new(addr, 128).unwrap(),
                flags: Ipv6AddressFlags { deprecated, assigned: true },
                device,
            };

            // Verify that Rule 3 still applies (avoid deprecated states).
            assert_eq!(
                select_ipv6_source_address_cmp(
                    None,
                    dev0,
                    &new_addr_entry(*local0, false, *dev1),
                    &new_addr_entry(*local1, true, *dev2),
                ),
                Ordering::Greater
            );

            // Verify that Rule 5 still applies (Prefer outgoing interface).
            assert_eq!(
                select_ipv6_source_address_cmp(
                    None,
                    dev0,
                    &new_addr_entry(*local0, false, *dev0),
                    &new_addr_entry(*local1, false, *dev1),
                ),
                Ordering::Greater
            );
        }
    }
}

/// Test fake implementations of the traits defined in the `socket` module.
#[cfg(test)]
pub(crate) mod testutil {
    use alloc::{boxed::Box, collections::HashMap, vec::Vec};
    use core::num::NonZeroUsize;

    use derivative::Derivative;
    use net_types::{
        ip::{GenericOverIp, IpAddr, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Subnet},
        MulticastAddr, Witness as _,
    };

    use super::*;
    use crate::{
        context::{testutil::FakeCoreCtx, SendFrameContext},
        device::testutil::{FakeStrongDeviceId, FakeWeakDeviceId},
        ip::{
            forwarding::{testutil::FakeIpForwardingCtx, ForwardingTable},
            testutil::FakeIpDeviceIdCtx,
            types::Destination,
            HopLimits, MulticastMembershipHandler, TransportIpContext, DEFAULT_HOP_LIMITS,
        },
    };

    /// A fake implementation of [`IpSocketContext`].
    ///
    /// `IpSocketContext` is implemented for `FakeIpSocketCtx` and any
    /// `FakeCtx<S>` where `S` implements `AsRef` and `AsMut` for
    /// `FakeIpSocketCtx`.
    #[derive(Derivative, GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    #[derivative(Default(bound = ""))]
    pub(crate) struct FakeIpSocketCtx<I: Ip, D> {
        pub(crate) table: ForwardingTable<I, D>,
        forwarding: FakeIpForwardingCtx<D>,
        devices: HashMap<D, FakeDeviceState<I>>,
    }

    impl<I: Ip, D> AsRef<Self> for FakeIpSocketCtx<I, D> {
        fn as_ref(&self) -> &Self {
            self
        }
    }

    impl<I: Ip, D> AsMut<Self> for FakeIpSocketCtx<I, D> {
        fn as_mut(&mut self) -> &mut Self {
            self
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId, BC> TransportIpContext<I, BC> for FakeIpSocketCtx<I, D> {
        fn get_default_hop_limits(&mut self, device: Option<&D>) -> HopLimits {
            device.map_or(DEFAULT_HOP_LIMITS, |device| {
                let hop_limit = self.get_device_state(device).default_hop_limit;
                HopLimits { unicast: hop_limit, multicast: DEFAULT_HOP_LIMITS.multicast }
            })
        }

        type DevicesWithAddrIter<'a> = Box<dyn Iterator<Item = D> + 'a>;

        fn get_devices_with_assigned_addr(
            &mut self,
            addr: SpecifiedAddr<<I>::Addr>,
        ) -> Self::DevicesWithAddrIter<'_> {
            Box::new(self.devices.iter().filter_map(move |(device, state)| {
                state.addresses.contains(&addr).then(|| device.clone())
            }))
        }

        fn confirm_reachable_with_destination(
            &mut self,
            _bindings_ctx: &mut BC,
            _dst: SpecifiedAddr<<I>::Addr>,
            _device: Option<&D>,
        ) {
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> DeviceIdContext<AnyDevice> for FakeIpSocketCtx<I, D> {
        type DeviceId = <FakeIpDeviceIdCtx<D> as DeviceIdContext<AnyDevice>>::DeviceId;
        type WeakDeviceId = <FakeIpDeviceIdCtx<D> as DeviceIdContext<AnyDevice>>::WeakDeviceId;
    }

    impl<I, D, BC> IpSocketHandler<I, BC> for FakeIpSocketCtx<I, D>
    where
        I: IpExt,
        D: FakeStrongDeviceId,
    {
        fn new_ip_socket(
            &mut self,
            _bindings_ctx: &mut BC,
            device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
            local_ip: Option<SocketIpAddr<I::Addr>>,
            remote_ip: SocketIpAddr<I::Addr>,
            proto: I::Proto,
        ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError> {
            let device = device
                .as_ref()
                .map(|d| d.as_strong_ref().ok_or(ResolveRouteError::Unreachable))
                .transpose()?;
            let device = device.as_ref().map(|d| d.as_ref());
            let resolved_route = self.lookup_route(device, local_ip, remote_ip)?;
            Ok(new_ip_socket(device, resolved_route, remote_ip, proto))
        }

        fn send_ip_packet<S, O>(
            &mut self,
            _bindings_ctx: &mut BC,
            _socket: &IpSock<I, Self::WeakDeviceId>,
            _body: S,
            _mtu: Option<u32>,
            _options: &O,
        ) -> Result<(), (S, IpSockSendError)>
        where
            S: TransportPacketSerializer<I>,
            S::Buffer: BufferMut,
            O: SendOptions<I>,
        {
            panic!("FakeIpSocketCtx can't send packets, wrap it in a FakeCoreCtx instead");
        }
    }

    impl<
            I: IpExt,
            State: AsRef<FakeIpSocketCtx<I, DeviceId>>
                + AsMut<FakeIpSocketCtx<I, DeviceId>>
                + AsRef<FakeIpDeviceIdCtx<DeviceId>>,
            Meta,
            DeviceId: FakeStrongDeviceId,
            BC,
        > IpSocketHandler<I, BC> for FakeCoreCtx<State, Meta, DeviceId>
    where
        FakeCoreCtx<State, Meta, DeviceId>:
            SendFrameContext<BC, SendIpPacketMeta<I, Self::DeviceId, SpecifiedAddr<I::Addr>>>,
    {
        fn new_ip_socket(
            &mut self,
            bindings_ctx: &mut BC,
            device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
            local_ip: Option<SocketIpAddr<I::Addr>>,
            remote_ip: SocketIpAddr<I::Addr>,
            proto: I::Proto,
        ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError> {
            self.state.as_mut().new_ip_socket(bindings_ctx, device, local_ip, remote_ip, proto)
        }

        fn send_ip_packet<S, O>(
            &mut self,
            bindings_ctx: &mut BC,
            socket: &IpSock<I, Self::WeakDeviceId>,
            body: S,
            mtu: Option<u32>,
            options: &O,
        ) -> Result<(), (S, IpSockSendError)>
        where
            S: TransportPacketSerializer<I>,
            S::Buffer: BufferMut,
            O: SendOptions<I>,
        {
            let meta = match self.state.as_mut().resolve_send_meta(socket, mtu, options) {
                Ok(meta) => meta,
                Err(e) => {
                    return Err((body, e));
                }
            };
            self.send_frame(bindings_ctx, meta, body).map_err(|s| (s, IpSockSendError::Mtu))
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId, BC> MulticastMembershipHandler<I, BC>
        for FakeIpSocketCtx<I, D>
    {
        fn join_multicast_group(
            &mut self,
            _bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            let value = self.get_device_state_mut(device).multicast_groups.entry(addr).or_insert(0);
            *value = value.checked_add(1).unwrap();
        }

        fn leave_multicast_group(
            &mut self,
            _bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            let value = self
                .get_device_state_mut(device)
                .multicast_groups
                .get_mut(&addr)
                .unwrap_or_else(|| panic!("no entry for {addr} on {device:?}"));
            *value = value.checked_sub(1).unwrap();
        }

        fn select_device_for_multicast_group(
            &mut self,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) -> Result<Self::DeviceId, ResolveRouteError> {
            let remote_ip = SocketIpAddr::new_from_multicast(addr);
            self.lookup_route(None, None, remote_ip).map(|ResolvedRoute { device, .. }| device)
        }
    }

    impl<I, BC, D, State, Meta> TransportIpContext<I, BC> for FakeCoreCtx<State, Meta, D>
    where
        I: IpExt,
        BC: InstantContext + TracingContext + FilterBindingsTypes,
        D: FakeStrongDeviceId,
        State: TransportIpContext<I, BC, DeviceId = D>,
        Self: IpSocketHandler<I, BC, DeviceId = D, WeakDeviceId = FakeWeakDeviceId<D>>,
    {
        type DevicesWithAddrIter<'a> = State::DevicesWithAddrIter<'a>
            where Self: 'a;

        fn get_devices_with_assigned_addr(
            &mut self,
            addr: SpecifiedAddr<I::Addr>,
        ) -> Self::DevicesWithAddrIter<'_> {
            TransportIpContext::<I, BC>::get_devices_with_assigned_addr(&mut self.state, addr)
        }

        fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits {
            TransportIpContext::<I, BC>::get_default_hop_limits(&mut self.state, device)
        }

        fn confirm_reachable_with_destination(
            &mut self,
            bindings_ctx: &mut BC,
            dst: SpecifiedAddr<I::Addr>,
            device: Option<&Self::DeviceId>,
        ) {
            TransportIpContext::<I, BC>::confirm_reachable_with_destination(
                &mut self.state,
                bindings_ctx,
                dst,
                device,
            )
        }
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    pub(crate) struct FakeDualStackIpSocketCtx<D> {
        v4: FakeIpSocketCtx<Ipv4, D>,
        v6: FakeIpSocketCtx<Ipv6, D>,
    }

    impl<D: FakeStrongDeviceId> FakeDualStackIpSocketCtx<D> {
        pub(crate) fn new<A: Into<SpecifiedAddr<IpAddr>>>(
            devices: impl IntoIterator<Item = FakeDeviceConfig<D, A>>,
        ) -> Self {
            let partition =
                |v: Vec<A>| -> (Vec<SpecifiedAddr<Ipv4Addr>>, Vec<SpecifiedAddr<Ipv6Addr>>) {
                    v.into_iter().fold((Vec::new(), Vec::new()), |(mut v4, mut v6), i| {
                        match IpAddr::from(i.into()) {
                            IpAddr::V4(a) => v4.push(a),
                            IpAddr::V6(a) => v6.push(a),
                        }
                        (v4, v6)
                    })
                };

            let (v4, v6): (Vec<_>, Vec<_>) = devices
                .into_iter()
                .map(|FakeDeviceConfig { device, local_ips, remote_ips }| {
                    let (local_v4, local_v6) = partition(local_ips);
                    let (remote_v4, remote_v6) = partition(remote_ips);
                    (
                        FakeDeviceConfig {
                            device: device.clone(),
                            local_ips: local_v4,
                            remote_ips: remote_v4,
                        },
                        FakeDeviceConfig { device, local_ips: local_v6, remote_ips: remote_v6 },
                    )
                })
                .unzip();
            Self { v4: FakeIpSocketCtx::new(v4), v6: FakeIpSocketCtx::new(v6) }
        }

        fn inner_mut<I: Ip>(&mut self) -> &mut FakeIpSocketCtx<I, D> {
            I::map_ip(IpInvariant(self), |IpInvariant(s)| &mut s.v4, |IpInvariant(s)| &mut s.v6)
        }

        fn inner<I: Ip>(&self) -> &FakeIpSocketCtx<I, D> {
            I::map_ip(IpInvariant(self), |IpInvariant(s)| &s.v4, |IpInvariant(s)| &s.v6)
        }

        pub(crate) fn add_route(&mut self, device: D, ip: SpecifiedAddr<IpAddr>) {
            match IpAddr::from(ip) {
                IpAddr::V4(ip) => crate::ip::forwarding::testutil::add_on_link_forwarding_entry(
                    &mut self.v4.table,
                    ip,
                    device,
                ),
                IpAddr::V6(ip) => crate::ip::forwarding::testutil::add_on_link_forwarding_entry(
                    &mut self.v6.table,
                    ip,
                    device,
                ),
            }
        }

        pub(crate) fn add_subnet_route<A: IpAddress>(&mut self, device: D, subnet: Subnet<A>) {
            let entry = crate::routes::Entry {
                subnet,
                device,
                gateway: None,
                metric: crate::routes::Metric::ExplicitMetric(crate::routes::RawMetric(0)),
            };
            A::Version::map_ip::<_, ()>(
                entry,
                |entry_v4| {
                    let _ =
                        crate::ip::forwarding::testutil::add_entry(&mut self.v4.table, entry_v4)
                            .expect("Failed to add route");
                },
                |entry_v6| {
                    let _ =
                        crate::ip::forwarding::testutil::add_entry(&mut self.v6.table, entry_v6)
                            .expect("Failed to add route");
                },
            );
        }

        pub(crate) fn get_device_state_mut<I: IpExt>(
            &mut self,
            device: &D,
        ) -> &mut FakeDeviceState<I> {
            self.inner_mut::<I>().get_device_state_mut(device)
        }

        pub(crate) fn multicast_memberships<I: IpExt>(
            &self,
        ) -> HashMap<(D, MulticastAddr<I::Addr>), NonZeroUsize> {
            self.inner::<I>().multicast_memberships()
        }
    }

    impl<D: FakeStrongDeviceId> AsRef<Self> for FakeDualStackIpSocketCtx<D> {
        fn as_ref(&self) -> &Self {
            self
        }
    }

    impl<D: FakeStrongDeviceId> DeviceIdContext<AnyDevice> for FakeDualStackIpSocketCtx<D> {
        type DeviceId = D;
        type WeakDeviceId = D::Weak;
    }

    impl<I: IpExt, D: FakeStrongDeviceId, BC> IpSocketHandler<I, BC> for FakeDualStackIpSocketCtx<D> {
        fn new_ip_socket(
            &mut self,
            bindings_ctx: &mut BC,
            device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
            local_ip: Option<SocketIpAddr<I::Addr>>,
            remote_ip: SocketIpAddr<I::Addr>,
            proto: I::Proto,
        ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError> {
            IpSocketHandler::<I, BC>::new_ip_socket(
                self.inner_mut::<I>(),
                bindings_ctx,
                device,
                local_ip,
                remote_ip,
                proto,
            )
        }

        fn send_ip_packet<S, O>(
            &mut self,
            bindings_ctx: &mut BC,
            socket: &IpSock<I, Self::WeakDeviceId>,
            body: S,
            mtu: Option<u32>,
            options: &O,
        ) -> Result<(), (S, IpSockSendError)>
        where
            S: TransportPacketSerializer<I>,
            S::Buffer: BufferMut,
            O: SendOptions<I>,
        {
            IpSocketHandler::<I, BC>::send_ip_packet(
                self.inner_mut::<I>(),
                bindings_ctx,
                socket,
                body,
                mtu,
                options,
            )
        }
    }

    impl<I: IpExt, Meta, DeviceId: FakeStrongDeviceId, BC> IpSocketHandler<I, BC>
        for FakeCoreCtx<FakeDualStackIpSocketCtx<DeviceId>, Meta, DeviceId>
    where
        FakeCoreCtx<FakeDualStackIpSocketCtx<DeviceId>, Meta, DeviceId>:
            SendFrameContext<BC, SendIpPacketMeta<I, Self::DeviceId, SpecifiedAddr<I::Addr>>>,
    {
        fn new_ip_socket(
            &mut self,
            bindings_ctx: &mut BC,
            device: Option<EitherDeviceId<&Self::DeviceId, &Self::WeakDeviceId>>,
            local_ip: Option<SocketIpAddr<I::Addr>>,
            remote_ip: SocketIpAddr<I::Addr>,
            proto: I::Proto,
        ) -> Result<IpSock<I, Self::WeakDeviceId>, IpSockCreationError> {
            self.state.new_ip_socket(bindings_ctx, device, local_ip, remote_ip, proto)
        }

        fn send_ip_packet<S, O>(
            &mut self,
            bindings_ctx: &mut BC,
            socket: &IpSock<I, Self::WeakDeviceId>,
            body: S,
            mtu: Option<u32>,
            options: &O,
        ) -> Result<(), (S, IpSockSendError)>
        where
            S: TransportPacketSerializer<I>,
            S::Buffer: BufferMut,
            O: SendOptions<I>,
        {
            let meta = match self.state.inner_mut::<I>().resolve_send_meta(socket, mtu, options) {
                Ok(meta) => meta,
                Err(e) => {
                    return Err((body, e));
                }
            };
            self.send_frame(bindings_ctx, meta, body).map_err(|s| (s, IpSockSendError::Mtu))
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId, BC> MulticastMembershipHandler<I, BC>
        for FakeDualStackIpSocketCtx<D>
    {
        fn join_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            MulticastMembershipHandler::<I, BC>::join_multicast_group(
                self.inner_mut::<I>(),
                bindings_ctx,
                device,
                addr,
            )
        }

        fn leave_multicast_group(
            &mut self,
            bindings_ctx: &mut BC,
            device: &Self::DeviceId,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) {
            MulticastMembershipHandler::<I, BC>::leave_multicast_group(
                self.inner_mut::<I>(),
                bindings_ctx,
                device,
                addr,
            )
        }

        fn select_device_for_multicast_group(
            &mut self,
            addr: MulticastAddr<<I as Ip>::Addr>,
        ) -> Result<Self::DeviceId, ResolveRouteError> {
            MulticastMembershipHandler::<I, BC>::select_device_for_multicast_group(
                self.inner_mut::<I>(),
                addr,
            )
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId, BC> TransportIpContext<I, BC>
        for FakeDualStackIpSocketCtx<D>
    {
        fn get_default_hop_limits(&mut self, device: Option<&Self::DeviceId>) -> HopLimits {
            TransportIpContext::<I, BC>::get_default_hop_limits(self.inner_mut::<I>(), device)
        }

        type DevicesWithAddrIter<'a> = alloc::boxed::Box<dyn Iterator<Item = D> + 'a>;

        fn get_devices_with_assigned_addr(
            &mut self,
            addr: SpecifiedAddr<<I>::Addr>,
        ) -> Self::DevicesWithAddrIter<'_> {
            TransportIpContext::<I, BC>::get_devices_with_assigned_addr(self.inner_mut::<I>(), addr)
        }

        fn confirm_reachable_with_destination(
            &mut self,
            bindings_ctx: &mut BC,
            dst: SpecifiedAddr<<I>::Addr>,
            device: Option<&Self::DeviceId>,
        ) {
            TransportIpContext::<I, BC>::confirm_reachable_with_destination(
                self.inner_mut::<I>(),
                bindings_ctx,
                dst,
                device,
            )
        }
    }

    #[derive(Clone, GenericOverIp)]
    #[generic_over_ip()]
    pub(crate) struct FakeDeviceConfig<D, A> {
        pub(crate) device: D,
        pub(crate) local_ips: Vec<A>,
        pub(crate) remote_ips: Vec<A>,
    }

    pub(crate) struct FakeDeviceState<I: Ip> {
        pub default_hop_limit: NonZeroU8,
        pub addresses: Vec<SpecifiedAddr<I::Addr>>,
        pub multicast_groups: HashMap<MulticastAddr<I::Addr>, usize>,
    }

    impl<I: Ip> FakeDeviceState<I> {
        pub fn is_in_multicast_group(&self, addr: &MulticastAddr<I::Addr>) -> bool {
            self.multicast_groups.get(addr).is_some_and(|v| *v != 0)
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> FakeIpSocketCtx<I, D> {
        /// Creates a new `FakeIpSocketCtx` with the given device
        /// configs.
        pub(crate) fn new(
            device_configs: impl IntoIterator<Item = FakeDeviceConfig<D, SpecifiedAddr<I::Addr>>>,
        ) -> Self {
            let mut table = ForwardingTable::default();
            let mut devices = HashMap::default();
            for FakeDeviceConfig { device, local_ips, remote_ips } in device_configs {
                for addr in remote_ips {
                    crate::ip::forwarding::testutil::add_on_link_forwarding_entry(
                        &mut table,
                        addr,
                        device.clone(),
                    )
                }
                let state = FakeDeviceState {
                    default_hop_limit: crate::ip::DEFAULT_HOP_LIMITS.unicast,
                    addresses: local_ips,
                    multicast_groups: Default::default(),
                };
                assert!(
                    devices.insert(device.clone(), state).is_none(),
                    "duplicate entries for {device:?}",
                );
            }

            Self { table, devices, forwarding: Default::default() }
        }

        pub(crate) fn get_device_state(&self, device: &D) -> &FakeDeviceState<I> {
            self.devices.get(device).unwrap_or_else(|| panic!("no device {device:?}"))
        }

        pub(crate) fn get_device_state_mut(&mut self, device: &D) -> &mut FakeDeviceState<I> {
            self.devices.get_mut(device).unwrap_or_else(|| panic!("no device {device:?}"))
        }

        pub(crate) fn multicast_memberships(
            &self,
        ) -> HashMap<(D, MulticastAddr<I::Addr>), NonZeroUsize> {
            self.devices
                .iter()
                .map(|(device, state)| {
                    state.multicast_groups.iter().filter_map(|(group, count)| {
                        NonZeroUsize::new(*count).map(|count| ((device.clone(), *group), count))
                    })
                })
                .flatten()
                .collect()
        }

        fn lookup_route(
            &mut self,
            device: Option<&D>,
            local_ip: Option<IpDeviceAddr<I::Addr>>,
            addr: RoutableIpAddr<I::Addr>,
        ) -> Result<ResolvedRoute<I, D>, ResolveRouteError> {
            let Self { table, devices, forwarding } = self;
            let (destination, ()) = table
                .lookup_filter_map(forwarding, device, addr.addr(), |_, d| match &local_ip {
                    None => Some(()),
                    Some(local_ip) => devices.get(d).and_then(|state| {
                        state.addresses.contains(local_ip.as_ref()).then_some(())
                    }),
                })
                .next()
                .ok_or(ResolveRouteError::Unreachable)?;

            let Destination { device, next_hop } = destination;
            let mut addrs = devices.get(device).unwrap().addresses.iter();
            let local_ip = match local_ip {
                None => {
                    let addr = addrs.next().ok_or(ResolveRouteError::NoSrcAddr)?;
                    SocketIpAddr::new(addr.get()).expect("not valid socket addr")
                }
                Some(local_ip) => {
                    // We already constrained the set of devices so this
                    // should be a given.
                    assert!(
                        addrs.any(|a| a.get() == local_ip.addr()),
                        "didn't find IP {:?} in {:?}",
                        local_ip,
                        addrs.collect::<Vec<_>>()
                    );
                    local_ip
                }
            };

            Ok(ResolvedRoute {
                src_addr: local_ip,
                device: device.clone(),
                local_delivery_device: None,
                next_hop,
            })
        }

        fn resolve_send_meta<O>(
            &mut self,
            socket: &IpSock<I, D::Weak>,
            mtu: Option<u32>,
            options: &O,
        ) -> Result<SendIpPacketMeta<I, D, SpecifiedAddr<I::Addr>>, IpSockSendError>
        where
            O: SendOptions<I>,
        {
            let IpSockDefinition { remote_ip, local_ip, device, proto } = &socket.definition;
            let device = device
                .as_ref()
                .map(|d| d.upgrade().ok_or(ResolveRouteError::Unreachable))
                .transpose()?;
            let ResolvedRoute { src_addr, device, next_hop, local_delivery_device: _ } =
                self.lookup_route(device.as_ref(), Some(*local_ip), *remote_ip)?;

            let remote_ip: &SpecifiedAddr<_> = remote_ip.as_ref();
            let (next_hop, broadcast) = next_hop.into_next_hop_and_broadcast_marker(*remote_ip);
            Ok(SendIpPacketMeta {
                device,
                src_ip: src_addr.into(),
                dst_ip: *remote_ip,
                broadcast,
                next_hop,
                proto: *proto,
                ttl: options.hop_limit(remote_ip),
                mtu,
            })
        }
    }
}
