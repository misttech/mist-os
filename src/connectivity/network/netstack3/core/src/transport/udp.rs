// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The User Datagram Protocol (UDP).

use alloc::{collections::hash_map::DefaultHasher, vec::Vec};
use core::{
    convert::Infallible as Never,
    fmt::Debug,
    hash::{Hash, Hasher},
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU8, NonZeroUsize},
    ops::RangeInclusive,
};
use lock_order::{Locked, Unlocked};

use derivative::Derivative;
use either::Either;
use net_types::{
    ip::{GenericOverIp, Ip, IpAddress, IpInvariant, IpVersionMarker, Ipv4, Ipv6},
    MulticastAddr, SpecifiedAddr, Witness, ZonedAddr,
};
use nonzero_ext::nonzero;
use packet::{BufferMut, Nested, ParsablePacket, ParseBuffer, Serializer};
use packet_formats::{
    error::ParseError,
    ip::IpProto,
    udp::{UdpPacket, UdpPacketBuilder, UdpPacketRaw, UdpParseArgs},
};
use tracing::trace;

use thiserror::Error;

use crate::{
    algorithm::{PortAlloc, PortAllocImpl, ProtocolFlowId},
    context::{CounterContext, InstantContext, RngContext, TracingContext},
    data_structures::{
        id_map_collection::IdMapCollectionKey,
        socketmap::{IterShadows as _, SocketMap, Tagged},
    },
    device::{AnyDevice, DeviceId, DeviceIdContext, Id, WeakDeviceId, WeakId},
    error::{LocalAddressError, SocketError, ZonedAddressError},
    ip::{
        icmp::IcmpIpExt,
        socket::{IpSockCreateAndSendError, IpSockCreationError, IpSockSendError},
        BufferIpTransportContext, BufferTransportIpContext, IpExt, IpTransportContext,
        MulticastMembershipHandler, TransportIpContext, TransportReceiveError,
    },
    socket::{
        address::{ConnAddr, ConnIpAddr, IpPortSpec, ListenerAddr, ListenerIpAddr},
        datagram::{
            self, AddrEntry, ConnState, ConnectError, DatagramBoundId, DatagramFlowId,
            DatagramSocketId, DatagramSocketSpec, DatagramSocketStateSpec, DatagramSockets,
            DatagramStateContext, DatagramStateNonSyncContext, ExpectedConnError,
            ExpectedUnboundError, FoundSockets, InUseError, ListenerState,
            LocalIdentifierAllocator, MulticastMembershipInterfaceSelector,
            SendError as DatagramSendError, SetMulticastMembershipError, Shutdown, ShutdownType,
            SocketHopLimits, SocketInfo as DatagramSocketInfo, UnboundSocketState,
        },
        AddrVec, Bound, BoundSocketMap, Connection, ConvertSocketTypeState, IncompatibleError,
        InsertError, RemoveResult, SocketAddrType, SocketMapAddrStateSpec, SocketMapConflictPolicy,
        SocketMapStateSpec,
    },
    sync::RwLock,
    trace_duration, transport, SyncCtx,
};

/// A builder for UDP layer state.
#[derive(Clone)]
pub(crate) struct UdpStateBuilder {
    send_port_unreachable: bool,
}

impl Default for UdpStateBuilder {
    fn default() -> UdpStateBuilder {
        UdpStateBuilder { send_port_unreachable: false }
    }
}

impl UdpStateBuilder {
    /// Enable or disable sending ICMP Port Unreachable messages in response to
    /// inbound UDP packets for which a corresponding local socket does not
    /// exist (default: disabled).
    ///
    /// Responding with an ICMP Port Unreachable error is a vector for reflected
    /// Denial-of-Service (DoS) attacks. The attacker can send a UDP packet to a
    /// closed port with the source address set to the address of the victim,
    /// and ICMP response will be sent there.
    ///
    /// According to [RFC 1122 Section 4.1.3.1], "\[i\]f a datagram arrives
    /// addressed to a UDP port for which there is no pending LISTEN call, UDP
    /// SHOULD send an ICMP Port Unreachable message." Since an ICMP response is
    /// not mandatory, and due to the security risks described, responses are
    /// disabled by default.
    ///
    /// [RFC 1122 Section 4.1.3.1]: https://tools.ietf.org/html/rfc1122#section-4.1.3.1
    #[cfg(test)]
    pub(crate) fn send_port_unreachable(&mut self, send_port_unreachable: bool) -> &mut Self {
        self.send_port_unreachable = send_port_unreachable;
        self
    }

    pub(crate) fn build<I: IpExt, D: WeakId>(self) -> UdpState<I, D> {
        UdpState { sockets: Default::default(), send_port_unreachable: self.send_port_unreachable }
    }
}

/// Convenience alias to make names shorter.
type UdpBoundSocketMap<I, D> = BoundSocketMap<IpPortSpec<I, D>, Udp<I, D>>;

/// A collection of UDP sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Sockets<I: Ip + IpExt, D: WeakId> {
    sockets: DatagramSockets<IpPortSpec<I, D>, Udp<I, D>>,
    /// lazy_port_alloc is lazy-initialized when it's used.
    lazy_port_alloc: Option<PortAlloc<UdpBoundSocketMap<I, D>>>,
}

/// The state associated with the UDP protocol.
///
/// `D` is the device ID type.
pub(crate) struct UdpState<I: IpExt, D: WeakId> {
    pub(crate) sockets: RwLock<Sockets<I, D>>,
    pub(crate) send_port_unreachable: bool,
}

impl<I: IpExt, D: WeakId> Default for UdpState<I, D> {
    fn default() -> UdpState<I, D> {
        UdpStateBuilder::default().build()
    }
}

/// Uninstantiatable type for implementing [`DatagramSocketSpec`].
struct Udp<I, D>(PhantomData<(I, D)>, Never);

/// Produces an iterator over eligible receiving socket addresses.
#[cfg(test)]
fn iter_receiving_addrs<I: Ip + IpExt, D: WeakId>(
    addr: ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    device: D,
) -> impl Iterator<Item = AddrVec<IpPortSpec<I, D>>> {
    crate::socket::address::AddrVecIter::with_device(addr.into(), device)
}

fn check_posix_sharing<I: IpExt, D: WeakId>(
    new_sharing: Sharing,
    dest: AddrVec<IpPortSpec<I, D>>,
    socketmap: &SocketMap<AddrVec<IpPortSpec<I, D>>, Bound<Udp<I, D>>>,
) -> Result<(), InsertError> {
    // Having a value present at a shadowed address is disqualifying, unless
    // both the new and existing sockets allow port sharing.
    if dest.iter_shadows().any(|a| {
        socketmap.get(&a).map_or(false, |bound| {
            !bound.tag(&a).to_sharing_options().is_shareable_with_new_state(new_sharing)
        })
    }) {
        return Err(InsertError::ShadowAddrExists);
    }

    // Likewise, the presence of a value that shadows the target address is
    // disqualifying unless both allow port sharing.
    match &dest {
        AddrVec::Conn(ConnAddr { ip: _, device: None }) | AddrVec::Listen(_) => {
            if socketmap.descendant_counts(&dest).any(|(tag, _): &(_, NonZeroUsize)| {
                !tag.to_sharing_options().is_shareable_with_new_state(new_sharing)
            }) {
                return Err(InsertError::ShadowerExists);
            }
        }
        AddrVec::Conn(ConnAddr { ip: _, device: Some(_) }) => {
            // No need to check shadows here because there are no addresses
            // that shadow a ConnAddr with a device.
            debug_assert_eq!(socketmap.descendant_counts(&dest).len(), 0)
        }
    }

    // There are a few combinations of addresses that can conflict with
    // each other even though there is not a direct shadowing relationship:
    // - listener address with device and connected address without.
    // - "any IP" listener with device and specific IP listener without.
    // - "any IP" listener with device and connected address without.
    //
    // The complication is that since these pairs of addresses don't have a
    // direct shadowing relationship, it's not possible to query for one
    // from the other in the socketmap without a linear scan. Instead. we
    // rely on the fact that the tag values in the socket map have different
    // values for entries with and without device IDs specified.
    fn conflict_exists<I: IpExt, D: WeakId>(
        new_sharing: Sharing,
        socketmap: &SocketMap<AddrVec<IpPortSpec<I, D>>, Bound<Udp<I, D>>>,
        addr: impl Into<AddrVec<IpPortSpec<I, D>>>,
        mut is_conflicting: impl FnMut(&AddrVecTag) -> bool,
    ) -> bool {
        socketmap.descendant_counts(&addr.into()).any(|(tag, _): &(_, NonZeroUsize)| {
            is_conflicting(tag)
                && !tag.to_sharing_options().is_shareable_with_new_state(new_sharing)
        })
    }

    let found_indirect_conflict = match dest {
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier },
            device: Some(_device),
        }) => {
            // An address with a device will shadow an any-IP listener
            // `dest` with a device so we only need to check for addresses
            // without a device. Likewise, an any-IP listener will directly
            // shadow `dest`, so an indirect conflict can only come from a
            // specific listener or connected socket (without a device).
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr { ip: ListenerIpAddr { addr: None, identifier }, device: None },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    !*has_device
                        && match addr_type {
                            SocketAddrType::SpecificListener | SocketAddrType::Connected => true,
                            SocketAddrType::AnyListener => false,
                        }
                },
            )
        }
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: Some(ip), identifier },
            device: Some(_device),
        }) => {
            // A specific-IP listener `dest` with a device will be shadowed
            // by a connected socket with a device and will shadow
            // specific-IP addresses without a device and any-IP listeners
            // with and without devices. That means an indirect conflict can
            // only come from a connected socket without a device.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr { ip: ListenerIpAddr { addr: Some(ip), identifier }, device: None },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    !*has_device
                        && match addr_type {
                            SocketAddrType::Connected => true,
                            SocketAddrType::AnyListener | SocketAddrType::SpecificListener => false,
                        }
                },
            )
        }
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: Some(_), identifier },
            device: None,
        }) => {
            // A specific-IP listener `dest` without a device will be
            // shadowed by a specific-IP listener with a device and by any
            // connected socket (with or without a device).  It will also
            // shadow an any-IP listener without a device, which means an
            // indirect conflict can only come from an any-IP listener with
            // a device.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr { ip: ListenerIpAddr { addr: None, identifier }, device: None },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    *has_device
                        && match addr_type {
                            SocketAddrType::AnyListener => true,
                            SocketAddrType::SpecificListener | SocketAddrType::Connected => false,
                        }
                },
            )
        }
        AddrVec::Conn(ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_identifier), remote: _ },
            device: None,
        }) => {
            // A connected socket `dest` without a device shadows listeners
            // without devices, and is shadowed by a connected socket with
            // a device. It can indirectly conflict with listening sockets
            // with devices.

            // Check for specific-IP listeners with devices, which would
            // indirectly conflict.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr {
                    ip: ListenerIpAddr {
                        addr: Some(local_ip),
                        identifier: local_identifier.clone(),
                    },
                    device: None,
                },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    *has_device
                        && match addr_type {
                            SocketAddrType::SpecificListener => true,
                            SocketAddrType::AnyListener | SocketAddrType::Connected => false,
                        }
                },
            ) ||
            // Check for any-IP listeners with devices since they conflict.
            // Note that this check cannot be combined with the one above
            // since they examine tag counts for different addresses. While
            // the counts of tags matched above *will* also be propagated to
            // the any-IP listener entry, they would be indistinguishable
            // from non-conflicting counts. For a connected address with
            // `Some(local_ip)`, the descendant counts at the listener
            // address with `addr = None` would include any
            // `SpecificListener` tags for both addresses with
            // `Some(local_ip)` and `Some(other_local_ip)`. The former
            // indirectly conflicts with `dest` but the latter does not,
            // hence this second distinct check.
            conflict_exists(
                new_sharing,
                socketmap,
                ListenerAddr {
                    ip: ListenerIpAddr { addr: None, identifier: local_identifier },
                    device: None,
                },
                |AddrVecTag { has_device, addr_type, sharing: _ }| {
                    *has_device
                        && match addr_type {
                            SocketAddrType::AnyListener => true,
                            SocketAddrType::SpecificListener | SocketAddrType::Connected => false,
                        }
                },
            )
        }
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier: _ },
            device: _,
        }) => false,
        AddrVec::Conn(ConnAddr { ip: _, device: Some(_device) }) => false,
    };
    if found_indirect_conflict {
        Err(InsertError::IndirectConflict)
    } else {
        Ok(())
    }
}

impl<I: IpExt, D: Id> SocketMapStateSpec for Udp<I, D> {
    type ListenerId = ListenerId<I>;
    type ConnId = ConnId<I>;

    type ListenerState = ListenerState<I::Addr, D>;
    type ConnState = ConnState<I, D>;

    type AddrVecTag = AddrVecTag;

    type ListenerSharingState = Sharing;
    type ConnSharingState = Sharing;

    type ListenerAddrState = AddrState<Self::ListenerId>;

    type ConnAddrState = AddrState<Self::ConnId>;
}

impl<AA, I: IpExt, D: WeakId> SocketMapConflictPolicy<AA, Sharing, IpPortSpec<I, D>> for Udp<I, D>
where
    AA: Into<AddrVec<IpPortSpec<I, D>>> + Clone,
{
    fn check_insert_conflicts(
        new_sharing_state: &Sharing,
        addr: &AA,
        socketmap: &SocketMap<AddrVec<IpPortSpec<I, D>>, Bound<Self>>,
    ) -> Result<(), InsertError> {
        check_posix_sharing(*new_sharing_state, addr.clone().into(), socketmap)
    }
}

impl<I: IpExt, D: WeakId> DatagramSocketStateSpec for Udp<I, D> {
    type UnboundId = UnboundId<I>;
    type UnboundSharingState = Sharing;
}

impl<I: IpExt, D: WeakId> DatagramSocketSpec<IpPortSpec<I, D>> for Udp<I, D> {
    type Serializer<B: BufferMut> = Nested<B, UdpPacketBuilder<I::Addr>>;
    fn make_packet<B: BufferMut>(
        body: B,
        addr: &ConnIpAddr<I::Addr, NonZeroU16, NonZeroU16>,
    ) -> Self::Serializer<B> {
        let ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) } = addr;
        body.encapsulate(UdpPacketBuilder::new(
            local_ip.get(),
            remote_ip.get(),
            Some(*local_port),
            *remote_port,
        ))
    }
}

enum LookupResult<I: Ip, D: Id> {
    Conn(ConnId<I>, ConnAddr<I::Addr, D, NonZeroU16, NonZeroU16>),
    Listener(ListenerId<I>, ListenerAddr<I::Addr, D, NonZeroU16>),
}

#[derive(Hash, Copy, Clone)]
struct SocketSelectorParams<I: Ip, A: AsRef<I::Addr>> {
    src_ip: I::Addr,
    dst_ip: A,
    src_port: u16,
    dst_port: u16,
    _ip: IpVersionMarker<I>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum AddrState<T> {
    Exclusive(T),
    ReusePort(Vec<T>),
}

impl<'a, A: IpAddress, LI> From<&'a ListenerIpAddr<A, LI>> for SocketAddrType {
    fn from(ListenerIpAddr { addr, identifier: _ }: &'a ListenerIpAddr<A, LI>) -> Self {
        match addr {
            Some(_) => SocketAddrType::SpecificListener,
            None => SocketAddrType::AnyListener,
        }
    }
}

impl<'a, A: IpAddress, LI, RI> From<&'a ConnIpAddr<A, LI, RI>> for SocketAddrType {
    fn from(_: &'a ConnIpAddr<A, LI, RI>) -> Self {
        SocketAddrType::Connected
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Sharing {
    Exclusive,
    ReusePort,
}

impl Default for Sharing {
    fn default() -> Self {
        Self::Exclusive
    }
}

impl Sharing {
    pub(crate) fn is_shareable_with_new_state(&self, new_state: Sharing) -> bool {
        match (self, new_state) {
            (Sharing::Exclusive, Sharing::Exclusive) => false,
            (Sharing::Exclusive, Sharing::ReusePort) => false,
            (Sharing::ReusePort, Sharing::Exclusive) => false,
            (Sharing::ReusePort, Sharing::ReusePort) => true,
        }
    }

    pub(crate) fn is_reuse_port(&self) -> bool {
        match self {
            Sharing::Exclusive => false,
            Sharing::ReusePort => true,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AddrVecTag {
    pub(crate) has_device: bool,
    pub(crate) addr_type: SocketAddrType,
    pub(crate) sharing: Sharing,
}

impl<T, A: IpAddress, D, LI> Tagged<ListenerAddr<A, D, LI>> for AddrState<T> {
    type Tag = AddrVecTag;

    fn tag(&self, address: &ListenerAddr<A, D, LI>) -> Self::Tag {
        let ListenerAddr { ip, device } = address;
        AddrVecTag {
            has_device: device.is_some(),
            addr_type: ip.into(),
            sharing: self.to_sharing_options(),
        }
    }
}

impl<T, A: IpAddress, D, LI, RI> Tagged<ConnAddr<A, D, LI, RI>> for AddrState<T> {
    type Tag = AddrVecTag;

    fn tag(&self, address: &ConnAddr<A, D, LI, RI>) -> Self::Tag {
        let ConnAddr { ip, device } = address;
        AddrVecTag {
            has_device: device.is_some(),
            addr_type: ip.into(),
            sharing: self.to_sharing_options(),
        }
    }
}

pub(crate) trait ToSharingOptions {
    fn to_sharing_options(&self) -> Sharing;
}

impl ToSharingOptions for AddrVecTag {
    fn to_sharing_options(&self) -> Sharing {
        let AddrVecTag { has_device: _, addr_type: _, sharing } = self;
        *sharing
    }
}

impl<T> ToSharingOptions for AddrState<T> {
    fn to_sharing_options(&self) -> Sharing {
        match self {
            AddrState::Exclusive(_) => Sharing::Exclusive,
            AddrState::ReusePort(_) => Sharing::ReusePort,
        }
    }
}

impl<T> ToSharingOptions for (T, Sharing) {
    fn to_sharing_options(&self) -> Sharing {
        let (_state, sharing) = self;
        *sharing
    }
}

impl<I: Debug + Eq> SocketMapAddrStateSpec for AddrState<I> {
    type Id = I;
    type SharingState = Sharing;
    type Inserter<'a> = &'a mut Vec<I> where I: 'a;

    fn new(new_sharing_state: &Sharing, id: I) -> Self {
        match new_sharing_state {
            Sharing::Exclusive => Self::Exclusive(id),
            Sharing::ReusePort => Self::ReusePort(Vec::from([id])),
        }
    }

    fn contains_id(&self, id: &Self::Id) -> bool {
        match self {
            Self::Exclusive(x) => id == x,
            Self::ReusePort(ids) => ids.contains(id),
        }
    }

    fn try_get_inserter<'a, 'b>(
        &'b mut self,
        new_sharing_state: &'a Sharing,
    ) -> Result<&'b mut Vec<I>, IncompatibleError> {
        match (self, new_sharing_state) {
            (AddrState::Exclusive(_), _) | (AddrState::ReusePort(_), Sharing::Exclusive) => {
                Err(IncompatibleError)
            }
            (AddrState::ReusePort(ids), Sharing::ReusePort) => Ok(ids),
        }
    }

    fn could_insert(
        &self,
        new_sharing_state: &Self::SharingState,
    ) -> Result<(), IncompatibleError> {
        match (self, new_sharing_state) {
            (AddrState::Exclusive(_), _) | (_, Sharing::Exclusive) => Err(IncompatibleError),
            (AddrState::ReusePort(_), Sharing::ReusePort) => Ok(()),
        }
    }

    fn remove_by_id(&mut self, id: I) -> RemoveResult {
        match self {
            AddrState::Exclusive(_) => RemoveResult::IsLast,
            AddrState::ReusePort(ids) => {
                let index = ids.iter().position(|i| i == &id).expect("couldn't find ID to remove");
                assert_eq!(ids.swap_remove(index), id);
                if ids.is_empty() {
                    RemoveResult::IsLast
                } else {
                    RemoveResult::Success
                }
            }
        }
    }
}

impl<T> AddrState<T> {
    fn select_receiver<I: Ip, A: AsRef<I::Addr> + Hash>(
        &self,
        selector: SocketSelectorParams<I, A>,
    ) -> &T {
        match self {
            AddrState::Exclusive(id) => id,
            AddrState::ReusePort(ids) => {
                let mut hasher = DefaultHasher::new();
                selector.hash(&mut hasher);
                let index: usize = hasher.finish() as usize % ids.len();
                &ids[index]
            }
        }
    }

    fn collect_all_ids(&self) -> impl Iterator<Item = &'_ T> {
        match self {
            AddrState::Exclusive(id) => Either::Left(core::iter::once(id)),
            AddrState::ReusePort(ids) => Either::Right(ids.iter()),
        }
    }
}

impl<'a, I: Ip + IpExt, D: WeakId + 'a> AddrEntry<'a, IpPortSpec<I, D>, Udp<I, D>> {
    /// Returns an iterator that yields a `LookupResult` for each contained ID.
    fn collect_all_ids(self) -> impl Iterator<Item = LookupResult<I, D>> + 'a {
        match self {
            Self::Listen(state, l) => Either::Left(
                state.collect_all_ids().map(move |id| LookupResult::Listener(*id, l.clone())),
            ),
            Self::Conn(state, c) => Either::Right(
                state.collect_all_ids().map(move |id| LookupResult::Conn(*id, c.clone())),
            ),
        }
    }

    /// Returns a `LookupResult` for the contained ID that matches the selector.
    fn select_receiver<A: AsRef<I::Addr> + Hash>(
        self,
        selector: SocketSelectorParams<I, A>,
    ) -> LookupResult<I, D> {
        match self {
            Self::Listen(state, l) => LookupResult::Listener(*state.select_receiver(selector), l),
            Self::Conn(state, c) => LookupResult::Conn(*state.select_receiver(selector), c),
        }
    }
}

impl<I: Ip + IpExt, D: WeakId> DatagramSockets<IpPortSpec<I, D>, Udp<I, D>> {
    /// Finds the socket(s) that should receive an incoming packet.
    ///
    /// Uses the provided addresses and receiving device to look up sockets that
    /// should receive a matching incoming packet. The returned iterator may
    /// yield 0, 1, or multiple sockets.
    fn lookup(
        &self,
        (src_ip, src_port): (I::Addr, Option<NonZeroU16>),
        (dst_ip, dst_port): (SpecifiedAddr<I::Addr>, NonZeroU16),
        device: D,
    ) -> impl Iterator<Item = LookupResult<I, D>> + '_ {
        let matching_entries = self.iter_receivers((src_ip, src_port), (dst_ip, dst_port), device);
        let Self { bound: _, bound_state, unbound: _ } = self;

        match matching_entries {
            None => Either::Left(None),
            Some(FoundSockets::Single(entry)) => {
                let selector = SocketSelectorParams {
                    src_ip,
                    dst_ip,
                    src_port: src_port.map_or(0, NonZeroU16::get),
                    dst_port: dst_port.get(),
                    _ip: IpVersionMarker::default(),
                };
                Either::Left(Some(entry.select_receiver(selector)))
            }

            Some(FoundSockets::Multicast(entries)) => {
                Either::Right(entries.into_iter().flat_map(AddrEntry::collect_all_ids))
            }
        }
        .into_iter()
        .filter(|lookup_result| match lookup_result {
            LookupResult::Conn(id, _) => {
                let (
                    ConnState {
                        socket: _,
                        shutdown: Shutdown { send: _, receive: shutdown_receive },
                        clear_device_on_disconnect: _,
                    },
                    _sharing,
                    _addr,
                ) = Connection::from_socket_state_ref(
                    bound_state.get(&id.clone().into()).expect("invalid conn ID"),
                );
                !shutdown_receive
            }
            LookupResult::Listener(_, _) => true,
        })
    }
}

/// Helper function to allocate a listen port.
///
/// Finds a random ephemeral port that is not in the provided `used_ports` set.
fn try_alloc_listen_port<I: IpExt, C: StateNonSyncContext<I>, D: WeakId>(
    ctx: &mut C,
    is_available: impl Fn(NonZeroU16) -> Result<(), InUseError>,
) -> Option<NonZeroU16> {
    let mut port = UdpBoundSocketMap::<I, D>::rand_ephemeral(&mut ctx.rng());
    for _ in UdpBoundSocketMap::<I, D>::EPHEMERAL_RANGE {
        // We can unwrap here because we know that the EPHEMERAL_RANGE doesn't
        // include 0.
        let tryport = NonZeroU16::new(port.get()).unwrap();
        match is_available(tryport) {
            Ok(()) => return Some(tryport),
            Err(InUseError {}) => port.next(),
        }
    }
    None
}

impl<I: IpExt, D: WeakId> PortAllocImpl for UdpBoundSocketMap<I, D> {
    const EPHEMERAL_RANGE: RangeInclusive<u16> = 49152..=65535;
    const TABLE_SIZE: NonZeroUsize = nonzero!(20usize);
    type Id = ProtocolFlowId<I::Addr>;

    fn is_port_available(&self, id: &Self::Id, port: u16) -> bool {
        // We can safely unwrap here, because the ports received in
        // `is_port_available` are guaranteed to be in `EPHEMERAL_RANGE`.
        let port = NonZeroU16::new(port).unwrap();
        let conn = ConnAddr::from_protocol_flow_and_local_port(id, port);

        // A port is free if there are no sockets currently using it, and if
        // there are no sockets that are shadowing it.
        AddrVec::from(conn).iter_shadows().all(|a| match &a {
            AddrVec::Listen(l) => self.listeners().get_by_addr(&l).is_none(),
            AddrVec::Conn(c) => self.conns().get_by_addr(&c).is_none(),
        } && self.get_shadower_counts(&a) == 0)
    }
}

/// Information associated with a UDP connection.
#[derive(Debug, GenericOverIp)]
#[cfg_attr(test, derive(PartialEq))]
pub struct ConnInfo<A: IpAddress, D> {
    /// The local address associated with a UDP connection.
    pub local_ip: ZonedAddr<A, D>,
    /// The local port associated with a UDP connection.
    pub local_port: NonZeroU16,
    /// The remote address associated with a UDP connection.
    pub remote_ip: ZonedAddr<A, D>,
    /// The remote port associated with a UDP connection.
    pub remote_port: NonZeroU16,
}

impl<A: IpAddress, D: Clone + Debug> From<ConnAddr<A, D, NonZeroU16, NonZeroU16>>
    for ConnInfo<A, D>
{
    fn from(c: ConnAddr<A, D, NonZeroU16, NonZeroU16>) -> Self {
        let ConnAddr {
            device,
            ip: ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) },
        } = c;
        Self {
            local_ip: transport::maybe_with_zone(local_ip, &device),
            local_port,
            remote_ip: transport::maybe_with_zone(remote_ip, device),
            remote_port,
        }
    }
}

/// Information associated with a UDP listener
#[derive(GenericOverIp)]
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub struct ListenerInfo<A: IpAddress, D> {
    /// The local address associated with a UDP listener, or `None` for any
    /// address.
    pub local_ip: Option<ZonedAddr<A, D>>,
    /// The local port associated with a UDP listener.
    pub local_port: NonZeroU16,
}

impl<A: IpAddress, D> From<ListenerAddr<A, D, NonZeroU16>> for ListenerInfo<A, D> {
    fn from(
        ListenerAddr { ip: ListenerIpAddr { addr, identifier }, device }: ListenerAddr<
            A,
            D,
            NonZeroU16,
        >,
    ) -> Self {
        let local_ip = addr.map(|addr| transport::maybe_with_zone(addr, device));
        Self { local_ip, local_port: identifier }
    }
}

impl<A: IpAddress, D> From<NonZeroU16> for ListenerInfo<A, D> {
    fn from(local_port: NonZeroU16) -> Self {
        Self { local_ip: None, local_port }
    }
}

/// The identifier for an unbound UDP socket.
///
/// New UDP sockets are created in an unbound state, and are assigned opaque
/// identifiers. These identifiers can then be used to connect the socket or
/// make it a listener.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
pub(crate) struct UnboundId<I: Ip>(usize, IpVersionMarker<I>);

impl<I: Ip> UnboundId<I> {
    fn new(id: usize) -> UnboundId<I> {
        UnboundId(id, IpVersionMarker::default())
    }
}

impl<I: Ip> From<usize> for UnboundId<I> {
    fn from(index: usize) -> Self {
        Self::new(index)
    }
}

impl<I: Ip> From<UnboundId<I>> for usize {
    fn from(UnboundId(index, _): UnboundId<I>) -> usize {
        index
    }
}

impl<I: Ip> IdMapCollectionKey for UnboundId<I> {
    const VARIANT_COUNT: NonZeroUsize = nonzero!(1usize);

    fn get_variant(&self) -> usize {
        0
    }

    fn get_id(&self) -> usize {
        (*self).into()
    }
}

/// The ID identifying a UDP connection.
///
/// When a new UDP connection is added, it is given a unique `ConnId`. These
/// are opaque `usize`s which are intentionally allocated as densely as possible
/// around 0, making it possible to store any associated data in a `Vec` indexed
/// by the ID. `ConnId` implements `Into<usize>`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
pub(crate) struct ConnId<I: Ip>(usize, IpVersionMarker<I>);

impl<I: Ip> ConnId<I> {
    fn new(id: usize) -> ConnId<I> {
        ConnId(id, IpVersionMarker::default())
    }
}

impl<I: Ip> From<ConnId<I>> for usize {
    fn from(id: ConnId<I>) -> usize {
        id.0
    }
}

impl<I: Ip> IdMapCollectionKey for ConnId<I> {
    const VARIANT_COUNT: NonZeroUsize = nonzero!(1usize);

    fn get_variant(&self) -> usize {
        0
    }

    fn get_id(&self) -> usize {
        self.0
    }
}

impl<I: Ip> From<usize> for ConnId<I> {
    fn from(index: usize) -> Self {
        ConnId::new(index)
    }
}

/// The ID identifying a UDP listener.
///
/// When a new UDP listener is added, it is given a unique `ListenerId`.
/// These are opaque `usize`s which are intentionally allocated as densely as
/// possible around 0, making it possible to store any associated data in a
/// `Vec` indexed by the ID. The `listener_type` field is used to look at the
/// correct backing `Vec`: `listeners` or `wildcard_listeners`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
pub(crate) struct ListenerId<I: Ip> {
    id: usize,
    _marker: IpVersionMarker<I>,
}

impl<I: Ip> ListenerId<I> {
    fn new(id: usize) -> Self {
        ListenerId { id, _marker: IpVersionMarker::default() }
    }
}

impl<I: Ip> IdMapCollectionKey for ListenerId<I> {
    const VARIANT_COUNT: NonZeroUsize = nonzero!(1usize);
    fn get_variant(&self) -> usize {
        0
    }
    fn get_id(&self) -> usize {
        self.id
    }
}

impl<I: Ip> From<ListenerId<I>> for usize {
    fn from(ListenerId { id, _marker }: ListenerId<I>) -> Self {
        id
    }
}

impl<I: Ip> From<usize> for ListenerId<I> {
    fn from(index: usize) -> Self {
        ListenerId::new(index)
    }
}

/// A unique identifier for a bound UDP connection or listener.
///
/// Contains either a [`ConnId`] or [`ListenerId`] in contexts where either
/// can be present.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
pub(crate) enum BoundId<I: Ip> {
    /// A UDP connection.
    Connected(ConnId<I>),
    /// A UDP listener.
    Listening(ListenerId<I>),
}

impl<I: Ip> From<ConnId<I>> for BoundId<I> {
    fn from(id: ConnId<I>) -> Self {
        Self::Connected(id)
    }
}

impl<I: Ip> From<ListenerId<I>> for BoundId<I> {
    fn from(id: ListenerId<I>) -> Self {
        Self::Listening(id)
    }
}

impl<I: Ip + IpExt, D: WeakId> From<BoundId<I>> for DatagramBoundId<Udp<I, D>> {
    fn from(id: BoundId<I>) -> Self {
        match id {
            BoundId::Connected(id) => DatagramBoundId::Connection(id),
            BoundId::Listening(id) => DatagramBoundId::Listener(id),
        }
    }
}

impl<I: Ip + IpExt, D: WeakId> From<ConnId<I>> for DatagramBoundId<Udp<I, D>> {
    fn from(id: ConnId<I>) -> Self {
        DatagramBoundId::Connection(id)
    }
}

impl<I: Ip + IpExt, D: WeakId> From<ListenerId<I>> for DatagramBoundId<Udp<I, D>> {
    fn from(id: ListenerId<I>) -> Self {
        DatagramBoundId::Listener(id)
    }
}

/// A unique identifier for a UDP socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]

pub struct SocketId<I: Ip>(SocketIdInner<I>);

// TODO(https://fxbug.dev/125489): remove this once Bindings isn't doing
// lookups by socket ID.
impl<I: Ip> IdMapCollectionKey for SocketId<I> {
    const VARIANT_COUNT: NonZeroUsize = nonzero!(3usize);
    fn get_variant(&self) -> usize {
        let Self(id) = self;
        match id {
            SocketIdInner::Unbound(UnboundId(_, _marker)) => 0,
            SocketIdInner::Bound(BoundId::Listening(ListenerId { id: _, _marker })) => 1,
            SocketIdInner::Bound(BoundId::Connected(ConnId(_, _marker))) => 2,
        }
    }

    fn get_id(&self) -> usize {
        let Self(id) = self;
        *match id {
            SocketIdInner::Unbound(UnboundId(index, _marker)) => index,
            SocketIdInner::Bound(BoundId::Listening(ListenerId { id, _marker })) => id,
            SocketIdInner::Bound(BoundId::Connected(ConnId(index, _marker))) => index,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, GenericOverIp)]
enum SocketIdInner<I: Ip> {
    /// A bound UDP socket ID.
    Bound(BoundId<I>),
    /// An unbound UDP socket ID.
    Unbound(UnboundId<I>),
}

impl<I: Ip> From<BoundId<I>> for SocketId<I> {
    fn from(id: BoundId<I>) -> Self {
        Self(SocketIdInner::Bound(id))
    }
}

impl<I: Ip> From<UnboundId<I>> for SocketId<I> {
    fn from(id: UnboundId<I>) -> Self {
        Self(SocketIdInner::Unbound(id))
    }
}

impl<I: Ip> From<ListenerId<I>> for SocketId<I> {
    fn from(id: ListenerId<I>) -> Self {
        Self(SocketIdInner::Bound(id.into()))
    }
}

impl<I: Ip> From<ConnId<I>> for SocketId<I> {
    fn from(id: ConnId<I>) -> Self {
        Self(SocketIdInner::Bound(id.into()))
    }
}

impl<I: Ip> TryFrom<SocketId<I>> for UnboundId<I> {
    type Error = ExpectedUnboundError;
    fn try_from(SocketId(id): SocketId<I>) -> Result<Self, Self::Error> {
        match id {
            SocketIdInner::Unbound(id) => Ok(id),
            SocketIdInner::Bound(_) => Err(ExpectedUnboundError),
        }
    }
}

impl<I: IpExt, D: WeakId> From<SocketId<I>> for DatagramSocketId<Udp<I, D>> {
    fn from(SocketId(id): SocketId<I>) -> Self {
        match id {
            SocketIdInner::Unbound(id) => Self::Unbound(id),
            SocketIdInner::Bound(id) => Self::Bound(id.into()),
        }
    }
}

impl<I: IpExt, D: WeakId> From<crate::socket::SocketId<Udp<I, D>>> for BoundId<I> {
    fn from(id: crate::socket::SocketId<Udp<I, D>>) -> Self {
        match id {
            crate::socket::SocketId::Listener(id) => Self::Listening(id),
            crate::socket::SocketId::Connection(id) => Self::Connected(id),
        }
    }
}

impl<I: IpExt, D: WeakId> From<DatagramSocketId<Udp<I, D>>> for SocketId<I> {
    fn from(id: DatagramSocketId<Udp<I, D>>) -> Self {
        match id {
            DatagramSocketId::Unbound(id) => Self(SocketIdInner::Unbound(id)),
            DatagramSocketId::Bound(id) => Self(SocketIdInner::Bound(id.into())),
        }
    }
}

/// An execution context for the UDP protocol.
pub trait NonSyncContext<I: IcmpIpExt> {
    /// Receives an ICMP error message related to a previously-sent UDP packet.
    ///
    /// `err` is the specific error identified by the incoming ICMP error
    /// message.
    ///
    /// Concretely, this method is called when an ICMP error message is received
    /// which contains an original packet which - based on its source and
    /// destination IPs and ports - most likely originated from the given
    /// socket. Note that the offending packet is not guaranteed to have
    /// originated from the given socket. For example, it may have originated
    /// from a previous socket with the same addresses, it may be the result of
    /// packet corruption on the network, it may have been injected by a
    /// malicious party, etc.
    fn receive_icmp_error(&mut self, id: SocketId<I>, err: I::ErrorCode);
}

/// The non-synchronized context for UDP.
pub trait StateNonSyncContext<I: IpExt>:
    InstantContext + RngContext + NonSyncContext<I> + CounterContext + TracingContext
{
}
impl<
        I: IpExt,
        C: InstantContext + RngContext + NonSyncContext<I> + CounterContext + TracingContext,
    > StateNonSyncContext<I> for C
{
}

/// An execution context for the UDP protocol which also provides access to state.
pub(crate) trait StateContext<I: IpExt, C: StateNonSyncContext<I>>:
    DeviceIdContext<AnyDevice>
{
    /// The synchronized context passed to the callback provided to
    /// `with_sockets_mut`.
    type IpSocketsCtx<'a>: TransportIpContext<I, C, DeviceId = Self::DeviceId, WeakDeviceId = Self::WeakDeviceId>
        + MulticastMembershipHandler<I, C>;

    /// Calls the function with an immutable reference to UDP sockets.
    fn with_sockets<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &Sockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to UDP sockets.
    fn with_sockets_mut<
        O,
        F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut Sockets<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Returns true if UDP may send a port unreachable ICMP error message.
    fn should_send_port_unreachable(&mut self) -> bool;
}

/// An execution context for the UDP protocol when a buffer is provided.
///
/// `BufferNonSyncContext` is like [`UdpContext`], except that it also requires that
/// the context be capable of receiving frames in buffers of type `B`. This is
/// used when a buffer of type `B` is provided to UDP (in particular, in
/// [`send_udp_conn`] and [`send_udp_listener`]), and allows any generated
/// link-layer frames to reuse that buffer rather than needing to always
/// allocate a new one.
pub trait BufferNonSyncContext<I: IpExt, B: BufferMut>: NonSyncContext<I> {
    /// Receives a UDP packet on a socket.
    fn receive_udp(
        &mut self,
        id: SocketId<I>,
        dst_ip: I::Addr,
        src_addr: (I::Addr, Option<NonZeroU16>),
        body: &B,
    );
}

/// The non-synchronized context for UDP with a buffer.
pub trait BufferStateNonSyncContext<I: IpExt, B: BufferMut>:
    StateNonSyncContext<I> + BufferNonSyncContext<I, B>
{
}
impl<I: IpExt, B: BufferMut, C: StateNonSyncContext<I> + BufferNonSyncContext<I, B>>
    BufferStateNonSyncContext<I, B> for C
{
}

/// An execution context for the UDP protocol when a buffer is provided which
/// also provides access to state.
pub(crate) trait BufferStateContext<I: IpExt, C: BufferStateNonSyncContext<I, B>, B: BufferMut>:
    StateContext<I, C>
where
    for<'a> Self: StateContext<I, C, IpSocketsCtx<'a> = Self::BufferIpSocketsCtx<'a>>,
{
    type BufferIpSocketsCtx<'a>: BufferTransportIpContext<
        I,
        C,
        B,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;
}

/// An implementation of [`IpTransportContext`] for UDP.
pub(crate) enum UdpIpTransportContext {}

impl<I: IpExt, C: StateNonSyncContext<I>, SC: StateContext<I, C>> IpTransportContext<I, C, SC>
    for UdpIpTransportContext
{
    fn receive_icmp_error(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        src_ip: Option<SpecifiedAddr<I::Addr>>,
        dst_ip: SpecifiedAddr<I::Addr>,
        mut original_udp_packet: &[u8],
        err: I::ErrorCode,
    ) {
        ctx.increment_debug_counter("UdpIpTransportContext::receive_icmp_error");
        trace!("UdpIpTransportContext::receive_icmp_error({:?})", err);

        let udp_packet = match original_udp_packet
            .parse_with::<_, UdpPacketRaw<_>>(IpVersionMarker::<I>::default())
        {
            Ok(packet) => packet,
            Err(e) => {
                let _: ParseError = e;
                // TODO(joshlf): Do something with this error.
                return;
            }
        };
        if let (Some(src_ip), Some(src_port), Some(dst_port)) =
            (src_ip, udp_packet.src_port(), udp_packet.dst_port())
        {
            sync_ctx.with_sockets(|sync_ctx, state| {
                let Sockets {sockets, lazy_port_alloc: _} = state;

                let receiver = sockets
                    .lookup((*dst_ip, Some(dst_port)), (src_ip, src_port), sync_ctx.downgrade_device_id(device))
                    .next();

            if let Some(id) = receiver {
                let id = match id {
                    LookupResult::Listener(id, _) => id.into(),
                    LookupResult::Conn(id, _) => id.into(),
                };
                ctx.receive_icmp_error(id, err);
            } else {
                trace!("UdpIpTransportContext::receive_icmp_error: Got ICMP error message for nonexistent UDP socket; either the socket responsible has since been removed, or the error message was sent in error or corrupted");
            }
        });
        } else {
            trace!("UdpIpTransportContext::receive_icmp_error: Got ICMP error message for IP packet with an invalid source or destination IP or port");
        }
    }
}

impl<
        I: IpExt,
        B: BufferMut,
        C: BufferStateNonSyncContext<I, B>,
        SC: BufferStateContext<I, C, B>,
    > BufferIpTransportContext<I, C, SC, B> for UdpIpTransportContext
{
    fn receive_ip_packet(
        sync_ctx: &mut SC,
        ctx: &mut C,
        device: &SC::DeviceId,
        src_ip: I::RecvSrcAddr,
        dst_ip: SpecifiedAddr<I::Addr>,
        mut buffer: B,
    ) -> Result<(), (B, TransportReceiveError)> {
        trace_duration!(ctx, "udp::receive_ip_packet");
        trace!("received UDP packet: {:x?}", buffer.as_mut());
        let src_ip = src_ip.into();

        let send_port_unreachable = sync_ctx.should_send_port_unreachable();

        sync_ctx.with_sockets(|sync_ctx, state| {
            let packet = if let Ok(packet) =
                buffer.parse_with::<_, UdpPacket<_>>(UdpParseArgs::new(src_ip, dst_ip.get()))
            {
                packet
            } else {
                // TODO(joshlf): Do something with ICMP here?
                return Ok(());
            };
            let Sockets { sockets, lazy_port_alloc: _ } = state;

            let device_weak = sync_ctx.downgrade_device_id(device);

            let src_port = packet.src_port();
            let mut recipients = sockets
                .lookup((src_ip, src_port), (dst_ip, packet.dst_port()), device_weak.clone())
                .peekable();

            if recipients.peek().is_some() {
                drop(packet);
                for lookup_result in recipients {
                    match lookup_result {
                        LookupResult::Conn(
                            id,
                            ConnAddr {
                                ip: ConnIpAddr { local: _, remote: (remote_ip, remote_port) },
                                device: _,
                            },
                        ) => ctx.receive_udp(
                            id.into(),
                            dst_ip.get(),
                            (remote_ip.get(), Some(remote_port)),
                            &buffer,
                        ),
                        LookupResult::Listener(id, _) => {
                            ctx.receive_udp(id.into(), dst_ip.get(), (src_ip, src_port), &buffer)
                        }
                    }
                }
                Ok(())
            } else if send_port_unreachable {
                // Unfortunately, type inference isn't smart enough for us to just
                // do packet.parse_metadata().
                let meta =
                    ParsablePacket::<_, packet_formats::udp::UdpParseArgs<I::Addr>>::parse_metadata(
                        &packet,
                    );
                drop(packet);
                buffer.undo_parse(meta);
                Err((buffer, TransportReceiveError::new_port_unreachable()))
            } else {
                Ok(())
            }
        })
    }
}

/// An error encountered while sending a UDP packet to an alternate address.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq)]
pub enum SendToError {
    /// The socket is not writeable.
    #[error("not writeable")]
    NotWriteable,
    /// An error was encountered while trying to create a temporary IP socket
    /// to use for the send operation.
    #[error("could not create a temporary connection socket: {}", _0)]
    CreateSock(IpSockCreationError),
    /// An MTU was exceeded.
    #[error("the maximum transmission unit (MTU) was exceeded")]
    Mtu,
    /// There was a problem with the remote address relating to its zone.
    #[error("zone error: {}", _0)]
    Zone(ZonedAddressError),
}

pub(crate) trait SocketHandler<I: IpExt, C>: DeviceIdContext<AnyDevice> {
    fn create_udp_unbound(&mut self) -> UnboundId<I>;

    fn connect(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: ZonedAddr<I::Addr, Self::DeviceId>,
        remote_port: NonZeroU16,
    ) -> Result<SocketId<I>, ConnectError>;

    fn set_device(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        device_id: Option<&Self::DeviceId>,
    ) -> Result<(), SocketError>;

    fn get_udp_bound_device(&mut self, ctx: &C, id: SocketId<I>) -> Option<Self::WeakDeviceId>;

    fn set_udp_posix_reuse_port(&mut self, ctx: &mut C, id: UnboundId<I>, reuse_port: bool);

    fn get_udp_posix_reuse_port(&mut self, ctx: &C, id: SocketId<I>) -> bool;

    fn set_udp_multicast_membership(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_group: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, Self::DeviceId>,
        want_membership: bool,
    ) -> Result<(), SetMulticastMembershipError>;

    fn set_udp_unicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        unicast_hop_limit: Option<NonZeroU8>,
    );

    fn set_udp_multicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_hop_limit: Option<NonZeroU8>,
    );

    fn get_udp_unicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8;

    fn get_udp_multicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8;

    fn disconnect_connected(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> Result<SocketId<I>, ExpectedConnError>;

    fn shutdown(
        &mut self,
        ctx: &C,
        id: SocketId<I>,
        which: ShutdownType,
    ) -> Result<(), ExpectedConnError>;

    fn get_shutdown(&mut self, ctx: &C, id: SocketId<I>) -> Option<ShutdownType>;

    fn remove_udp(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId>;

    fn get_udp_info(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId>;

    fn listen_udp(
        &mut self,
        ctx: &mut C,
        id: UnboundId<I>,
        addr: Option<ZonedAddr<I::Addr, Self::DeviceId>>,
        port: Option<NonZeroU16>,
    ) -> Result<ListenerId<I>, LocalAddressError>;
}

impl<I: IpExt, C: StateNonSyncContext<I>, SC: StateContext<I, C>> SocketHandler<I, C> for SC {
    fn create_udp_unbound(&mut self) -> UnboundId<I> {
        datagram::create_unbound(self)
    }

    fn connect(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: ZonedAddr<<I>::Addr, Self::DeviceId>,
        remote_port: NonZeroU16,
    ) -> Result<SocketId<I>, ConnectError> {
        datagram::connect(self, ctx, id.into(), remote_ip, remote_port, IpProto::Udp)
            .map(Into::into)
    }

    fn set_device(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        device_id: Option<&Self::DeviceId>,
    ) -> Result<(), SocketError> {
        datagram::set_device(self, ctx, id.into(), device_id)
    }

    fn get_udp_bound_device(&mut self, ctx: &C, id: SocketId<I>) -> Option<Self::WeakDeviceId> {
        datagram::get_bound_device(self, ctx, id)
    }

    fn set_udp_posix_reuse_port(&mut self, _ctx: &mut C, id: UnboundId<I>, reuse_port: bool) {
        self.with_sockets_mut(|_sync_ctx, state| {
            let Sockets {
                sockets: DatagramSockets { bound_state: _, unbound, bound: _ },
                lazy_port_alloc: _,
            } = state;

            let UnboundSocketState { device: _, sharing, ip_options: _ } =
                unbound.get_mut(id.into()).expect("unbound UDP socket not found");
            *sharing = if reuse_port { Sharing::ReusePort } else { Sharing::Exclusive };
        })
    }

    fn get_udp_posix_reuse_port(&mut self, _ctx: &C, id: SocketId<I>) -> bool {
        datagram::get_sharing(self, id).is_reuse_port()
    }

    fn set_udp_multicast_membership(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_group: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, Self::DeviceId>,
        want_membership: bool,
    ) -> Result<(), SetMulticastMembershipError> {
        datagram::set_multicast_membership(
            self,
            ctx,
            id,
            multicast_group,
            interface,
            want_membership,
        )
        .map_err(Into::into)
    }

    fn set_udp_unicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        unicast_hop_limit: Option<NonZeroU8>,
    ) {
        crate::socket::datagram::update_ip_hop_limit(
            self,
            ctx,
            id,
            SocketHopLimits::set_unicast(unicast_hop_limit),
        )
    }

    fn set_udp_multicast_hop_limit(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        multicast_hop_limit: Option<NonZeroU8>,
    ) {
        crate::socket::datagram::update_ip_hop_limit(
            self,
            ctx,
            id,
            SocketHopLimits::set_multicast(multicast_hop_limit),
        )
    }

    fn get_udp_unicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8 {
        crate::socket::datagram::get_ip_hop_limits(self, ctx, id).unicast
    }

    fn get_udp_multicast_hop_limit(&mut self, ctx: &C, id: SocketId<I>) -> NonZeroU8 {
        crate::socket::datagram::get_ip_hop_limits(self, ctx, id).multicast
    }

    fn disconnect_connected(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> Result<SocketId<I>, ExpectedConnError> {
        datagram::disconnect_connected(self, ctx, id.into()).map(Into::into)
    }

    fn shutdown(
        &mut self,
        ctx: &C,
        id: SocketId<I>,
        which: ShutdownType,
    ) -> Result<(), ExpectedConnError> {
        datagram::shutdown_connected(self, ctx, id.into(), which)
    }

    fn get_shutdown(&mut self, ctx: &C, id: SocketId<I>) -> Option<ShutdownType> {
        datagram::get_shutdown_connected(self, ctx, id.into())
    }

    fn remove_udp(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId> {
        datagram::remove(self, ctx, id).into()
    }

    fn get_udp_info(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
    ) -> SocketInfo<I::Addr, Self::WeakDeviceId> {
        datagram::get_info(self, ctx, id).into()
    }

    fn listen_udp(
        &mut self,
        ctx: &mut C,
        id: UnboundId<I>,
        addr: Option<ZonedAddr<I::Addr, Self::DeviceId>>,
        port: Option<NonZeroU16>,
    ) -> Result<ListenerId<I>, LocalAddressError> {
        datagram::listen(self, ctx, id, addr, port)
    }
}

/// Error when sending a packet on a socket.
#[derive(Copy, Clone, Debug, Eq, PartialEq, GenericOverIp)]
pub enum SendError {
    /// The socket is not writeable.
    NotWriteable,
    /// The packet couldn't be sent.
    IpSock(IpSockSendError),
}

pub(crate) trait BufferSocketHandler<I: IpExt, C, B: BufferMut>:
    SocketHandler<I, C>
{
    fn send(
        &mut self,
        ctx: &mut C,
        conn: SocketId<I>,
        body: B,
    ) -> Result<(), (B, Either<SendError, ExpectedConnError>)>;

    fn send_to(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: ZonedAddr<I::Addr, Self::DeviceId>,
        remote_port: NonZeroU16,
        body: B,
    ) -> Result<
        Option<SocketId<I>>,
        (B, Either<LocalAddressError, (Option<SocketId<I>>, SendToError)>),
    >;
}

impl<
        I: IpExt,
        B: BufferMut,
        C: BufferStateNonSyncContext<I, B>,
        SC: BufferStateContext<I, C, B>,
    > BufferSocketHandler<I, C, B> for SC
{
    fn send(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        body: B,
    ) -> Result<(), (B, Either<SendError, ExpectedConnError>)> {
        datagram::send_conn(self, ctx, id.into(), body).map_err(|send_error| match send_error {
            DatagramSendError::NotConnected(b) => (b, Either::Right(ExpectedConnError)),
            DatagramSendError::NotWriteable(b) => (b, Either::Left(SendError::NotWriteable)),
            DatagramSendError::IpSock(body, err) => {
                (body.into_inner(), Either::Left(SendError::IpSock(err)))
            }
        })
    }

    fn send_to(
        &mut self,
        ctx: &mut C,
        id: SocketId<I>,
        remote_ip: ZonedAddr<I::Addr, Self::DeviceId>,
        remote_port: NonZeroU16,
        body: B,
    ) -> Result<
        Option<SocketId<I>>,
        (B, Either<LocalAddressError, (Option<SocketId<I>>, SendToError)>),
    > {
        datagram::send_to(self, ctx, id.into(), remote_ip, remote_port, IpProto::Udp.into(), body)
            .map(|id| id.map(Into::into))
            .map_err(|e| match e {
                Either::Left((body, e)) => (body, Either::Left(e)),
                Either::Right((new_id, e)) => {
                    let (body, err) = match e {
                        datagram::SendToError::NotWriteable(body) => {
                            (body, SendToError::NotWriteable)
                        }
                        datagram::SendToError::Zone(body, e) => (body, SendToError::Zone(e)),
                        datagram::SendToError::CreateAndSend(s, e) => (
                            s.into_inner(),
                            match e {
                                IpSockCreateAndSendError::Mtu => SendToError::Mtu,
                                IpSockCreateAndSendError::Create(e) => SendToError::CreateSock(e),
                            },
                        ),
                    };
                    (body, Either::Right((new_id.map(Into::into), err)))
                }
            })
    }
}

/// Sends a UDP packet on an existing socket.
///
/// # Errors
///
/// Returns an error if the socket is not connected or the packet cannot be sent.
/// On error, the original `body` is returned unmodified so that it can be
/// reused by the caller.
///
/// # Panics
///
/// Panics if `id` is not a valid UDP socket identifier.
pub fn send_udp<I: IpExt, B: BufferMut, C: crate::BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    body: B,
) -> Result<(), (B, Either<SendError, ExpectedConnError>)> {
    let mut sync_ctx = Locked::new(sync_ctx);

    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, body)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, body)), id)| {
            BufferSocketHandler::<Ipv4, _, _>::send(sync_ctx, ctx, id, body).map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, body)), id)| {
            BufferSocketHandler::<Ipv6, _, _>::send(sync_ctx, ctx, id, body).map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Sends a UDP packet to the provided destination address.
///
/// If this is called with an unbound socket, the socket will be implicitly
/// bound. If that succeeds, the ID for the new socket is returned.
///
/// # Errors
///
/// Returns an error if the socket is unbound and connecting fails, or if the
/// packet could not be sent. If the socket is unbound and connecting succeeds
/// but sending fails, the socket remains connected.
///
/// # Panics
///
/// Panics if `id` is not a valid UDP socket identifier.
pub fn send_udp_to<I: IpExt, B: BufferMut, C: crate::BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    remote_ip: ZonedAddr<I::Addr, DeviceId<C>>,
    remote_port: NonZeroU16,
    body: B,
) -> Result<Option<SocketId<I>>, (B, Either<LocalAddressError, (Option<SocketId<I>>, SendToError)>)>
{
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    #[derive(GenericOverIp)]
    struct Error<I: Ip, B>((B, Either<LocalAddressError, (Option<SocketId<I>>, SendToError)>));

    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, remote_port, body)), id, remote_ip),
        |(IpInvariant((sync_ctx, ctx, remote_port, body)), id, remote_ip)| {
            BufferSocketHandler::<Ipv4, _, _>::send_to(
                sync_ctx,
                ctx,
                id,
                remote_ip,
                remote_port,
                body,
            )
            .map_err(Error)
        },
        |(IpInvariant((sync_ctx, ctx, remote_port, body)), id, remote_ip)| {
            BufferSocketHandler::<Ipv6, _, _>::send_to(
                sync_ctx,
                ctx,
                id,
                remote_ip,
                remote_port,
                body,
            )
            .map_err(Error)
        },
    )
    .map_err(|Error(e)| e)
}

impl<I: IpExt, C: StateNonSyncContext<I>, SC: StateContext<I, C>>
    DatagramStateContext<IpPortSpec<I, SC::WeakDeviceId>, C, Udp<I, SC::WeakDeviceId>> for SC
{
    type IpSocketsCtx<'a> = SC::IpSocketsCtx<'a>;
    type LocalIdAllocator = Option<PortAlloc<UdpBoundSocketMap<I, SC::WeakDeviceId>>>;

    fn with_sockets<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &DatagramSockets<IpPortSpec<I, SC::WeakDeviceId>, Udp<I, SC::WeakDeviceId>>,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_sockets(|sync_ctx, Sockets { sockets: state, lazy_port_alloc: _ }| {
            cb(sync_ctx, state)
        })
    }

    fn with_sockets_mut<
        O,
        F: FnOnce(
            &mut Self::IpSocketsCtx<'_>,
            &mut DatagramSockets<IpPortSpec<I, SC::WeakDeviceId>, Udp<I, SC::WeakDeviceId>>,
            &mut Self::LocalIdAllocator,
        ) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        self.with_sockets_mut(|sync_ctx, Sockets { sockets: state, lazy_port_alloc }| {
            cb(sync_ctx, state, lazy_port_alloc)
        })
    }
}

impl<I: IpExt, D: WeakId, C: StateNonSyncContext<I>>
    DatagramStateNonSyncContext<IpPortSpec<I, D>, Udp<I, D>> for C
{
    fn try_alloc_listen_identifier(
        &mut self,
        is_available: impl Fn(NonZeroU16) -> Result<(), InUseError>,
    ) -> Option<NonZeroU16> {
        try_alloc_listen_port::<_, _, D>(self, is_available)
    }
}

impl<I: IpExt, C: StateNonSyncContext<I>, D: WeakId>
    LocalIdentifierAllocator<IpPortSpec<I, D>, C, Udp<I, D>>
    for Option<PortAlloc<UdpBoundSocketMap<I, D>>>
{
    fn try_alloc_local_id(
        &mut self,
        bound: &BoundSocketMap<IpPortSpec<I, D>, Udp<I, D>>,
        ctx: &mut C,
        flow: datagram::DatagramFlowId<I::Addr, NonZeroU16>,
    ) -> Option<NonZeroU16> {
        let DatagramFlowId { local_ip, remote_ip, remote_id } = flow;
        let id = ProtocolFlowId::new(local_ip, remote_ip, remote_id);
        let mut rng = ctx.rng();
        // Lazily init port_alloc if it hasn't been inited yet.
        let port_alloc = self.get_or_insert_with(|| PortAlloc::new(&mut rng));
        port_alloc.try_alloc(&id, bound).and_then(NonZeroU16::new)
    }
}

fn listen_udp_unbound<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &mut Locked<&SyncCtx<C>, Unlocked>,
    ctx: &mut C,
    id: UnboundId<I>,
    addr: Option<ZonedAddr<I::Addr, DeviceId<C>>>,
    port: Option<NonZeroU16>,
) -> Result<ListenerId<I>, LocalAddressError> {
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((sync_ctx, ctx, port)), id, addr),
        |(IpInvariant((sync_ctx, ctx, port)), id, addr)| {
            SocketHandler::<Ipv4, _>::listen_udp(sync_ctx, ctx, id, addr, port)
        },
        |(IpInvariant((sync_ctx, ctx, port)), id, addr)| {
            SocketHandler::<Ipv6, _>::listen_udp(sync_ctx, ctx, id, addr, port)
        },
    )
}

/// Creates an unbound UDP socket.
///
/// `create_udp` creates a new UDP socket and returns an identifier for it.
pub fn create_udp<I: IpExt, C: crate::NonSyncContext>(sync_ctx: &SyncCtx<C>) -> SocketId<I> {
    let mut sync_ctx = Locked::new(sync_ctx);
    UnboundId::into(I::map_ip(
        IpInvariant(&mut sync_ctx),
        |IpInvariant(sync_ctx)| SocketHandler::<Ipv4, _>::create_udp_unbound(sync_ctx),
        |IpInvariant(sync_ctx)| SocketHandler::<Ipv6, _>::create_udp_unbound(sync_ctx),
    ))
}

/// Connect a UDP socket
///
/// `connect` binds `id` as a connection to the remote address and port.
/// It is also bound to a local address and port, meaning that packets sent on
/// this connection will always come from that address and port. The local
/// address will be chosen based on the route to the remote address, and the
/// local port will be chosen from the available ones.
///
/// # Errors
///
/// `connect` will fail in the following cases:
/// - If both `local_ip` and `local_port` are specified but conflict with an
///   existing connection or listener
/// - If one or both are left unspecified but there is still no way to satisfy
///   the request (e.g., `local_ip` is specified but there are no available
///   local ports for that address)
/// - If there is no route to `remote_ip`
/// - If `id` belongs to an already-connected socket
///
/// # Panics
///
/// `connect` panics if `id` is not a valid [`UnboundId`].
pub fn connect<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    remote_ip: ZonedAddr<I::Addr, DeviceId<C>>,
    remote_port: NonZeroU16,
) -> Result<SocketId<I>, ConnectError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, remote_port)), id, remote_ip),
        |(IpInvariant((sync_ctx, ctx, remote_port)), id, remote_ip)| {
            SocketHandler::<Ipv4, _>::connect(sync_ctx, ctx, id, remote_ip, remote_port)
                .map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, remote_port)), id, remote_ip)| {
            SocketHandler::<Ipv6, _>::connect(sync_ctx, ctx, id, remote_ip, remote_port)
                .map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Sets the bound device for a socket.
///
/// Sets the device to be used for sending and receiving packets for a socket.
/// If the socket is not currently bound to a local address and port, the device
/// will be used when binding.
///
/// # Panics
///
/// Panics if `id` is not a valid [`SocketId`].
pub fn set_udp_device<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    device_id: Option<&DeviceId<C>>,
) -> Result<(), SocketError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, device_id)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, device_id)), id)| {
            SocketHandler::<Ipv4, _>::set_device(sync_ctx, ctx, id, device_id).map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, device_id)), id)| {
            SocketHandler::<Ipv6, _>::set_device(sync_ctx, ctx, id, device_id).map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Gets the device the specified socket is bound to.
///
/// # Panics
///
/// Panics if `id` is not a valid socket ID.
pub fn get_udp_bound_device<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> Option<WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(device) = I::map_ip::<_, IpInvariant<Option<WeakDeviceId<C>>>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_bound_device(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_bound_device(sync_ctx, ctx, id))
        },
    );
    device
}

/// Sets the POSIX `SO_REUSEPORT` option for the specified socket.
///
/// # Errors
///
/// Returns an error if the socket is already bound.
///
/// # Panics
///
/// `set_udp_posix_reuse_port` panics if `id` is not a valid `SocketId`.
pub fn set_udp_posix_reuse_port<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    reuse_port: bool,
) -> Result<(), ExpectedUnboundError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id: UnboundId<I> = id.clone().try_into()?;
    Ok(I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, reuse_port)), id),
        |(IpInvariant((sync_ctx, ctx, reuse_port)), id)| {
            SocketHandler::<Ipv4, _>::set_udp_posix_reuse_port(sync_ctx, ctx, id, reuse_port)
        },
        |(IpInvariant((sync_ctx, ctx, reuse_port)), id)| {
            SocketHandler::<Ipv6, _>::set_udp_posix_reuse_port(sync_ctx, ctx, id, reuse_port)
        },
    ))
}

/// Gets the POSIX `SO_REUSEPORT` option for the specified socket.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn get_udp_posix_reuse_port<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> bool {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(reuse_port) = I::map_ip::<_, IpInvariant<bool>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_posix_reuse_port(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_posix_reuse_port(sync_ctx, ctx, id))
        },
    );
    reuse_port
}

/// Sets the specified socket's membership status for the given group.
///
/// If `id` is unbound, the membership state will take effect when it is bound.
/// An error is returned if the membership change request is invalid (e.g.
/// leaving a group that was not joined, or joining a group multiple times) or
/// if the device to use to join is unspecified or conflicts with the existing
/// socket state.
pub fn set_udp_multicast_membership<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    multicast_group: MulticastAddr<I::Addr>,
    interface: MulticastMembershipInterfaceSelector<I::Addr, DeviceId<C>>,
    want_membership: bool,
) -> Result<(), SetMulticastMembershipError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip::<_, Result<_, _>>(
        (IpInvariant((&mut sync_ctx, ctx, want_membership)), id, multicast_group, interface),
        |(IpInvariant((sync_ctx, ctx, want_membership)), id, multicast_group, interface)| {
            SocketHandler::<Ipv4, _>::set_udp_multicast_membership(
                sync_ctx,
                ctx,
                id,
                multicast_group,
                interface,
                want_membership,
            )
            .map_err(IpInvariant)
        },
        |(IpInvariant((sync_ctx, ctx, want_membership)), id, multicast_group, interface)| {
            SocketHandler::<Ipv6, _>::set_udp_multicast_membership(
                sync_ctx,
                ctx,
                id,
                multicast_group,
                interface,
                want_membership,
            )
            .map_err(IpInvariant)
        },
    )
    .map_err(|IpInvariant(e)| e)
}

/// Sets the hop limit for packets sent by the socket to a unicast destination.
///
/// Sets the hop limit (IPv6) or TTL (IPv4) for outbound packets going to a
/// unicast address.
pub fn set_udp_unicast_hop_limit<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    unicast_hop_limit: Option<NonZeroU8>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, unicast_hop_limit)), id),
        |(IpInvariant((sync_ctx, ctx, unicast_hop_limit)), id)| {
            SocketHandler::<Ipv4, _>::set_udp_unicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                unicast_hop_limit,
            )
        },
        |(IpInvariant((sync_ctx, ctx, unicast_hop_limit)), id)| {
            SocketHandler::<Ipv6, _>::set_udp_unicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                unicast_hop_limit,
            )
        },
    )
}

/// Sets the hop limit for packets sent by the socket to a multicast destination.
///
/// Sets the hop limit (IPv6) or TTL (IPv4) for outbound packets going to a
/// unicast address.
pub fn set_udp_multicast_hop_limit<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    multicast_hop_limit: Option<NonZeroU8>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, multicast_hop_limit)), id),
        |(IpInvariant((sync_ctx, ctx, multicast_hop_limit)), id)| {
            SocketHandler::<Ipv4, _>::set_udp_multicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                multicast_hop_limit,
            )
        },
        |(IpInvariant((sync_ctx, ctx, multicast_hop_limit)), id)| {
            SocketHandler::<Ipv6, _>::set_udp_multicast_hop_limit(
                sync_ctx,
                ctx,
                id,
                multicast_hop_limit,
            )
        },
    )
}

/// Gets the hop limit for packets sent by the socket to a unicast destination.
pub fn get_udp_unicast_hop_limit<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> NonZeroU8 {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(hop_limit) = I::map_ip::<_, IpInvariant<NonZeroU8>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_unicast_hop_limit(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_unicast_hop_limit(sync_ctx, ctx, id))
        },
    );
    hop_limit
}

/// Sets the hop limit for packets sent by the socket to a multicast destination.
///
/// Sets the hop limit (IPv6) or TTL (IPv4) for outbound packets going to a
/// unicast address.
pub fn get_udp_multicast_hop_limit<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> NonZeroU8 {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id = id.clone();
    let IpInvariant(hop_limit) = I::map_ip::<_, IpInvariant<NonZeroU8>>(
        (IpInvariant((&mut sync_ctx, ctx)), id),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv4, _>::get_udp_multicast_hop_limit(sync_ctx, ctx, id))
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            IpInvariant(SocketHandler::<Ipv6, _>::get_udp_multicast_hop_limit(sync_ctx, ctx, id))
        },
    );
    hop_limit
}

/// Disconnects a connected UDP socket.
///
/// `disconnect_udp_connected` removes an existing connected socket and replaces
/// it with a listening socket bound to the same local address and port.
///
/// # Errors
///
/// Returns an error if the socket is not connected.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn disconnect_udp_connected<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
) -> Result<SocketId<I>, ExpectedConnError> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::disconnect_connected(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::disconnect_connected(sync_ctx, ctx, id)
        },
    )
}

/// Shuts down a socket for reading and/or writing.
///
/// # Errors
///
/// Returns an error if the socket is not connected.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn shutdown<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
    which: ShutdownType,
) -> Result<(), ExpectedConnError> {
    let mut sync_ctx = Locked::new(sync_ctx);

    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx, which)), id.clone()),
        |(IpInvariant((sync_ctx, ctx, which)), id)| {
            SocketHandler::<Ipv4, _>::shutdown(sync_ctx, ctx, id, which)
        },
        |(IpInvariant((sync_ctx, ctx, which)), id)| {
            SocketHandler::<Ipv6, _>::shutdown(sync_ctx, ctx, id, which)
        },
    )
}

/// Get the shutdown state for a socket.
///
/// If the socket is not connected, or if `shutdown` was not called on it,
/// returns `None`.
///
/// # Panics
///
/// Panics if `id` is not a valid `SocketId`.
pub fn get_shutdown<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &C,
    id: &SocketId<I>,
) -> Option<ShutdownType> {
    let mut sync_ctx = Locked::new(sync_ctx);

    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::get_shutdown(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::get_shutdown(sync_ctx, ctx, id)
        },
    )
}

/// Information about the addresses for a socket.
#[derive(GenericOverIp)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub enum SocketInfo<A: IpAddress, D> {
    /// The socket was not bound.
    Unbound,
    /// The socket was listening.
    Listener(ListenerInfo<A, D>),
    /// The socket was connected.
    Connected(ConnInfo<A, D>),
}

impl<I: IpExt, D: WeakId> From<DatagramSocketInfo<IpPortSpec<I, D>>> for SocketInfo<I::Addr, D> {
    fn from(value: DatagramSocketInfo<IpPortSpec<I, D>>) -> Self {
        match value {
            DatagramSocketInfo::Unbound => Self::Unbound,
            DatagramSocketInfo::Listener(addr) => Self::Listener(addr.into()),
            DatagramSocketInfo::Connected(addr) => Self::Connected(addr.into()),
        }
    }
}

/// Gets the [`SocketInfo`] associated with the UDP socket referenced by `id`.
///
/// # Panics
///
/// `get_udp_info` panics if `id` is not a valid `SocketId`.
pub fn get_udp_info<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
) -> SocketInfo<I::Addr, WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::get_udp_info(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::get_udp_info(sync_ctx, ctx, id)
        },
    )
}

/// Removes a socket that was previously created.
///
/// # Panics
///
/// Panics if `id` is not a valid [`SocketId`].
pub fn remove_udp<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: SocketId<I>,
) -> SocketInfo<I::Addr, WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    I::map_ip(
        (IpInvariant((&mut sync_ctx, ctx)), id.clone()),
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv4, _>::remove_udp(sync_ctx, ctx, id)
        },
        |(IpInvariant((sync_ctx, ctx)), id)| {
            SocketHandler::<Ipv6, _>::remove_udp(sync_ctx, ctx, id)
        },
    )
}

/// Use an existing socket to listen for incoming UDP packets.
///
/// `listen_udp` converts `id` into a listening socket and registers the new
/// socket as a listener for incoming UDP packets on the given `port`. If `addr`
/// is `None`, the listener is a "wildcard listener", and is bound to all local
/// addresses. See the [`crate::transport`] module documentation for more
/// details.
///
/// If `addr` is `Some``, and `addr` is already bound on the given port (either
/// by a listener or a connection), `listen_udp` will fail. If `addr` is `None`,
/// and a wildcard listener is already bound to the given port, `listen_udp`
/// will fail.
///
/// # Errors
///
/// Returns an error if the socket is not currently unbound.
///
/// # Panics
///
/// `listen_udp` panics if `id` is not a valid [`SocketId`].
pub fn listen_udp<I: IpExt, C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &SocketId<I>,
    addr: Option<ZonedAddr<I::Addr, DeviceId<C>>>,
    port: Option<NonZeroU16>,
) -> Result<SocketId<I>, Either<ExpectedUnboundError, LocalAddressError>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    let id: UnboundId<I> = id.clone().try_into().map_err(Either::Left)?;

    listen_udp_unbound(&mut sync_ctx, ctx, id, addr, port)
        .map_err(Either::Right)
        .map(|id: ListenerId<I>| SocketId::from(id))
}

#[cfg(test)]
mod tests {
    use alloc::{
        borrow::ToOwned,
        collections::{HashMap, HashSet},
        vec,
        vec::Vec,
    };
    use const_unwrap::const_unwrap_option;
    use core::convert::TryInto as _;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use itertools::Itertools as _;
    use net_declare::net_ip_v4 as ip_v4;
    use net_declare::net_ip_v6;
    use net_types::{
        ip::{Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr},
        AddrAndZone, LinkLocalAddr, MulticastAddr, Scope as _, ScopeableAddress as _,
    };
    use nonzero_ext::nonzero;
    use packet::{Buf, InnerPacketBuilder, ParsablePacket, Serializer};
    use packet_formats::{
        icmp::{Icmpv4DestUnreachableCode, Icmpv6DestUnreachableCode},
        ip::IpPacketBuilder,
        ipv4::{Ipv4Header, Ipv4PacketRaw},
        ipv6::{Ipv6Header, Ipv6PacketRaw},
    };
    use test_case::test_case;

    use super::*;
    use crate::{
        context::testutil::{
            FakeCtxWithSyncCtx, FakeFrameCtx, FakeNonSyncCtx, FakeSyncCtx, WrappedFakeSyncCtx,
        },
        device::testutil::{FakeDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
        error::RemoteAddressError,
        ip::{
            device::state::IpDeviceStateIpExt,
            icmp::{Icmpv4ErrorCode, Icmpv6ErrorCode},
            socket::testutil::{FakeBufferIpSocketCtx, FakeDeviceConfig, FakeIpSocketCtx},
            testutil::FakeIpDeviceIdCtx,
            ResolveRouteError, SendIpPacketMeta,
        },
        socket::{self, datagram::MulticastInterfaceSelector, SocketState},
        testutil::{assert_empty, set_logger_for_test, TestIpExt as _},
    };

    /// The listener data sent through a [`FakeUdpCtx`].
    #[derive(Debug, PartialEq)]
    struct ListenData<I: Ip> {
        listener: ListenerId<I>,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        src_port: Option<NonZeroU16>,
        body: Vec<u8>,
    }

    /// The UDP connection data sent through a [`FakeUdpCtx`].
    #[derive(Debug, PartialEq)]
    struct ConnData<I: Ip> {
        conn: ConnId<I>,
        body: Vec<u8>,
    }

    /// An ICMP error delivered to a [`FakeUdpCtx`].
    #[derive(Debug, Eq, PartialEq)]
    struct IcmpError<I: TestIpExt> {
        id: SocketId<I>,
        err: I::ErrorCode,
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeUdpState<I: TestIpExt, D: FakeStrongDeviceId> {
        sockets: Sockets<I, FakeWeakDeviceId<D>>,
    }

    impl<I: TestIpExt> Default for FakeUdpSyncCtx<I, FakeDeviceId> {
        fn default() -> Self {
            Self::with_local_remote_ip_addrs(vec![local_ip::<I>()], vec![remote_ip::<I>()])
        }
    }

    impl<I: Ip + TestIpExt> FakeUdpSyncCtx<I, FakeDeviceId> {
        fn with_local_remote_ip_addrs(
            local_ips: Vec<SpecifiedAddr<I::Addr>>,
            remote_ips: Vec<SpecifiedAddr<I::Addr>>,
        ) -> Self {
            FakeUdpSyncCtx::with_inner_and_outer_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new_fake(local_ips, remote_ips)),
                FakeUdpState::default(),
            )
        }
    }

    impl<I: Ip + TestIpExt, D: FakeStrongDeviceId> FakeUdpSyncCtx<I, D> {
        fn with_state(state: FakeBufferIpSocketCtx<I, D>) -> Self {
            FakeUdpSyncCtx::with_inner_and_outer_state(state, FakeUdpState::default())
        }
    }

    /// `FakeCtx` specialized for UDP.
    type FakeUdpCtx<I, D> =
        FakeCtxWithSyncCtx<FakeUdpSyncCtx<I, D>, (), (), FakeNonSyncCtxState<I>>;

    /// `FakeSyncCtx` specialized for UDP.
    type FakeUdpSyncCtx<I, D> = WrappedFakeSyncCtx<
        FakeUdpState<I, D>,
        FakeBufferIpSocketCtx<I, D>,
        SendIpPacketMeta<I, D, SpecifiedAddr<<I as Ip>::Addr>>,
        D,
    >;
    /// `FakeNonSyncCtx` specialized for UDP.
    type FakeUdpNonSyncCtx<I> = FakeNonSyncCtx<(), (), FakeNonSyncCtxState<I>>;

    /// The FakeSyncCtx held as the inner state of the [`WrappedFakeSyncCtx`] that
    /// is [`FakeUdpSyncCtx`].
    type FakeBufferSyncCtx<I, D> = FakeSyncCtx<
        FakeBufferIpSocketCtx<I, D>,
        SendIpPacketMeta<I, D, SpecifiedAddr<<I as Ip>::Addr>>,
        D,
    >;

    type UdpFakeDeviceCtx<I> = FakeUdpCtx<I, FakeDeviceId>;
    type UdpFakeDeviceSyncCtx<I> = FakeUdpSyncCtx<I, FakeDeviceId>;
    type UdpFakeDeviceNonSyncCtx<I> = FakeUdpNonSyncCtx<I>;

    #[derive(Default)]
    struct FakeNonSyncCtxState<I: TestIpExt> {
        listen_data: Vec<ListenData<I>>,
        conn_data: Vec<ConnData<I>>,
        icmp_errors: Vec<IcmpError<I>>,
    }

    impl<I: TestIpExt> FakeNonSyncCtxState<I> {
        fn listen_data(&self) -> HashMap<ListenerId<I>, Vec<&'_ [u8]>> {
            self.listen_data.iter().fold(
                HashMap::new(),
                |mut map, ListenData { listener, body, src_ip: _, dst_ip: _, src_port: _ }| {
                    map.entry(*listener).or_default().push(&body);
                    map
                },
            )
        }
    }

    impl<I: TestIpExt> NonSyncContext<I> for FakeUdpNonSyncCtx<I> {
        fn receive_icmp_error(&mut self, id: SocketId<I>, err: I::ErrorCode) {
            self.state_mut().icmp_errors.push(IcmpError { id, err })
        }
    }

    impl<I: TestIpExt, D: FakeStrongDeviceId + 'static> StateContext<I, FakeUdpNonSyncCtx<I>>
        for FakeUdpSyncCtx<I, D>
    {
        type IpSocketsCtx<'a> = FakeBufferSyncCtx<I, D>;

        fn with_sockets<
            O,
            F: FnOnce(&mut Self::IpSocketsCtx<'_>, &Sockets<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            let FakeUdpState { sockets } = outer;
            cb(inner, sockets)
        }

        fn with_sockets_mut<
            O,
            F: FnOnce(&mut Self::IpSocketsCtx<'_>, &mut Sockets<I, Self::WeakDeviceId>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let Self { outer, inner } = self;
            let FakeUdpState { sockets } = outer;
            cb(inner, sockets)
        }

        fn should_send_port_unreachable(&mut self) -> bool {
            false
        }
    }

    impl<I: TestIpExt, D: FakeStrongDeviceId + 'static, B: BufferMut>
        BufferStateContext<I, FakeUdpNonSyncCtx<I>, B> for FakeUdpSyncCtx<I, D>
    {
        type BufferIpSocketsCtx<'a> = Self::IpSocketsCtx<'a>;
    }

    impl<I: TestIpExt, B: BufferMut> BufferNonSyncContext<I, B> for FakeUdpNonSyncCtx<I> {
        fn receive_udp(
            &mut self,
            id: SocketId<I>,
            dst_ip: I::Addr,
            (src_ip, src_port): (I::Addr, Option<NonZeroU16>),
            body: &B,
        ) {
            let SocketId(id) = id;
            let FakeNonSyncCtxState { conn_data, listen_data, icmp_errors: _ } = self.state_mut();
            match id {
                SocketIdInner::Unbound(_) => unreachable!("unbound sockets can't receive"),
                SocketIdInner::Bound(BoundId::Connected(conn)) => {
                    conn_data.push(ConnData { conn, body: body.as_ref().to_owned() })
                }
                SocketIdInner::Bound(BoundId::Listening(listener)) => {
                    listen_data.push(ListenData {
                        listener,
                        src_ip,
                        dst_ip,
                        src_port,
                        body: body.as_ref().to_owned(),
                    })
                }
            }
        }
    }

    fn local_ip<I: TestIpExt>() -> SpecifiedAddr<I::Addr> {
        I::get_other_ip_address(1)
    }

    fn remote_ip<I: TestIpExt>() -> SpecifiedAddr<I::Addr> {
        I::get_other_ip_address(2)
    }

    trait TestIpExt: crate::testutil::TestIpExt + IpExt + IpDeviceStateIpExt {
        fn try_into_recv_src_addr(addr: Self::Addr) -> Option<Self::RecvSrcAddr>;
    }

    impl TestIpExt for Ipv4 {
        fn try_into_recv_src_addr(addr: Ipv4Addr) -> Option<Ipv4Addr> {
            Some(addr)
        }
    }

    impl TestIpExt for Ipv6 {
        fn try_into_recv_src_addr(addr: Ipv6Addr) -> Option<Ipv6SourceAddr> {
            Ipv6SourceAddr::new(addr)
        }
    }

    /// Helper function to inject an UDP packet with the provided parameters.
    fn receive_udp_packet<I: TestIpExt, D: FakeStrongDeviceId + 'static>(
        sync_ctx: &mut FakeUdpSyncCtx<I, D>,
        ctx: &mut FakeUdpNonSyncCtx<I>,
        device: D,
        src_ip: I::Addr,
        dst_ip: I::Addr,
        src_port: impl Into<u16>,
        dst_port: NonZeroU16,
        body: &[u8],
    ) {
        let builder =
            UdpPacketBuilder::new(src_ip, dst_ip, NonZeroU16::new(src_port.into()), dst_port);
        let buffer = Buf::new(body.to_owned(), ..)
            .encapsulate(builder)
            .serialize_vec_outer()
            .unwrap()
            .into_inner();
        UdpIpTransportContext::receive_ip_packet(
            sync_ctx,
            ctx,
            &device,
            I::try_into_recv_src_addr(src_ip).unwrap(),
            SpecifiedAddr::new(dst_ip).unwrap(),
            buffer,
        )
        .expect("Receive IP packet succeeds");
    }

    const LOCAL_PORT: NonZeroU16 = nonzero!(100u16);
    const REMOTE_PORT: NonZeroU16 = nonzero!(200u16);

    fn conn_addr<I>(
        device: Option<FakeWeakDeviceId<FakeDeviceId>>,
    ) -> AddrVec<IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>>
    where
        I: Ip + TestIpExt,
    {
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        ConnAddr {
            ip: ConnIpAddr { local: (local_ip, LOCAL_PORT), remote: (remote_ip, REMOTE_PORT) },
            device,
        }
        .into()
    }

    fn local_listener<I>(
        device: Option<FakeWeakDeviceId<FakeDeviceId>>,
    ) -> AddrVec<IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>>
    where
        I: Ip + TestIpExt,
    {
        let local_ip = local_ip::<I>();
        ListenerAddr { ip: ListenerIpAddr { identifier: LOCAL_PORT, addr: Some(local_ip) }, device }
            .into()
    }

    fn wildcard_listener<I>(
        device: Option<FakeWeakDeviceId<FakeDeviceId>>,
    ) -> AddrVec<IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>>
    where
        I: Ip + TestIpExt,
    {
        ListenerAddr { ip: ListenerIpAddr { identifier: LOCAL_PORT, addr: None }, device }.into()
    }

    #[ip_test]
    #[test_case(conn_addr(Some(FakeWeakDeviceId(FakeDeviceId))), [
            conn_addr(None), local_listener(Some(FakeWeakDeviceId(FakeDeviceId))), local_listener(None),
            wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))), wildcard_listener(None)
        ]; "conn with device")]
    #[test_case(local_listener(Some(FakeWeakDeviceId(FakeDeviceId))),
        [local_listener(None), wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))), wildcard_listener(None)];
        "local listener with device")]
    #[test_case(wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))), [wildcard_listener(None)];
        "wildcard listener with device")]
    #[test_case(conn_addr(None), [local_listener(None), wildcard_listener(None)]; "conn no device")]
    #[test_case(local_listener(None), [wildcard_listener(None)]; "local listener no device")]
    #[test_case(wildcard_listener(None), []; "wildcard listener no device")]
    fn test_udp_addr_vec_iter_shadows_conn<I: Ip + IpExt, D: WeakId, const N: usize>(
        addr: AddrVec<IpPortSpec<I, D>>,
        expected_shadows: [AddrVec<IpPortSpec<I, D>>; N],
    ) {
        assert_eq!(addr.iter_shadows().collect::<HashSet<_>>(), HashSet::from(expected_shadows));
    }

    #[ip_test]
    fn test_iter_receiving_addrs<I: Ip + TestIpExt>() {
        let addr = ConnIpAddr {
            local: (local_ip::<I>(), LOCAL_PORT),
            remote: (remote_ip::<I>(), REMOTE_PORT),
        };
        assert_eq!(
            iter_receiving_addrs::<I, _>(addr, FakeWeakDeviceId(FakeDeviceId)).collect::<Vec<_>>(),
            vec![
                // A socket connected on exactly the receiving vector has precedence.
                conn_addr(Some(FakeWeakDeviceId(FakeDeviceId))),
                // Connected takes precedence over listening with device match.
                conn_addr(None),
                local_listener(Some(FakeWeakDeviceId(FakeDeviceId))),
                // Specific IP takes precedence over device match.
                local_listener(None),
                wildcard_listener(Some(FakeWeakDeviceId(FakeDeviceId))),
                // Fallback to least specific
                wildcard_listener(None)
            ]
        );
    }

    /// Tests UDP listeners over different IP versions.
    ///
    /// Tests that a listener can be created, that the context receives packet
    /// notifications for that listener, and that we can send data using that
    /// listener.
    #[ip_test]
    fn test_listen_udp<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        // Create a listener on local port 100, bound to the local IP:
        let listener = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");

        // Inject a packet and check that the context receives it:
        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip.get(),
            local_ip.get(),
            NonZeroU16::new(200).unwrap(),
            NonZeroU16::new(100).unwrap(),
            &body[..],
        );

        let listen_data = &non_sync_ctx.state().listen_data;
        assert_eq!(listen_data.len(), 1);
        let pkt = &listen_data[0];
        assert_eq!(pkt.listener, listener);
        assert_eq!(pkt.src_ip, remote_ip.get());
        assert_eq!(pkt.dst_ip, local_ip.get());
        assert_eq!(pkt.src_port.unwrap().get(), 200);
        assert_eq!(pkt.body, &body[..]);

        // Send a packet providing a local ip:
        assert_matches!(
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                ZonedAddr::Unzoned(remote_ip),
                NonZeroU16::new(200).unwrap(),
                Buf::new(body.to_vec(), ..),
            )
            .expect("send_udp_listener failed"),
            None
        );
        // And send a packet that doesn't:
        assert_matches!(
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                ZonedAddr::Unzoned(remote_ip),
                NonZeroU16::new(200).unwrap(),
                Buf::new(body.to_vec(), ..),
            )
            .expect("send_udp_listener failed"),
            None
        );
        let frames = sync_ctx.inner.frames();
        assert_eq!(frames.len(), 2);
        let check_frame = |(meta, frame_body): &(
            SendIpPacketMeta<I, FakeDeviceId, SpecifiedAddr<I::Addr>>,
            Vec<u8>,
        )| {
            let SendIpPacketMeta { device: _, src_ip, dst_ip, next_hop, proto, ttl: _, mtu: _ } =
                meta;
            assert_eq!(next_hop, &remote_ip);
            assert_eq!(src_ip, &local_ip);
            assert_eq!(dst_ip, &remote_ip);
            assert_eq!(proto, &IpProto::Udp.into());
            let mut buf = &frame_body[..];
            let udp_packet =
                UdpPacket::parse(&mut buf, UdpParseArgs::new(src_ip.get(), dst_ip.get()))
                    .expect("Parsed sent UDP packet");
            assert_eq!(udp_packet.src_port().unwrap().get(), 100);
            assert_eq!(udp_packet.dst_port().get(), 200);
            assert_eq!(udp_packet.body(), &body[..]);
        };
        check_frame(&frames[0]);
        check_frame(&frames[1]);
    }

    /// Tests that UDP packets without a connection are dropped.
    ///
    /// Tests that receiving a UDP packet on a port over which there isn't a
    /// listener causes the packet to be dropped correctly.
    #[ip_test]
    fn test_udp_drop<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();

        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip.get(),
            local_ip.get(),
            NonZeroU16::new(200).unwrap(),
            NonZeroU16::new(100).unwrap(),
            &body[..],
        );
        assert_empty(non_sync_ctx.state().listen_data.iter());
        assert_empty(non_sync_ctx.state().conn_data.iter());
    }

    /// Tests that UDP connections can be created and data can be transmitted
    /// over it.
    ///
    /// Only tests with specified local port and address bounds.
    #[ip_test]
    fn test_udp_conn_basic<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        // Create a UDP connection with a specified local port and local IP.
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(NonZeroU16::new(100).unwrap()),
        )
        .expect("listen_udp failed");
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Unzoned(remote_ip),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        // Inject a UDP packet and see if we receive it on the context.
        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip.get(),
            local_ip.get(),
            NonZeroU16::new(200).unwrap(),
            NonZeroU16::new(100).unwrap(),
            &body[..],
        );

        let conn_data = &non_sync_ctx.state().conn_data;
        assert_eq!(conn_data.len(), 1);
        let pkt = &conn_data[0];
        assert_eq!(pkt.conn, conn.try_into().unwrap());
        assert_eq!(pkt.body, &body[..]);

        // Now try to send something over this new connection.
        BufferSocketHandler::send(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn,
            Buf::new(body.to_vec(), ..),
        )
        .expect("send_udp_conn returned an error");

        let frames = sync_ctx.inner.frames();
        assert_eq!(frames.len(), 1);

        // Check first frame.
        let (
            SendIpPacketMeta { device: _, src_ip, dst_ip, next_hop, proto, ttl: _, mtu: _ },
            frame_body,
        ) = &frames[0];
        assert_eq!(next_hop, &remote_ip);
        assert_eq!(src_ip, &local_ip);
        assert_eq!(dst_ip, &remote_ip);
        assert_eq!(proto, &IpProto::Udp.into());
        let mut buf = &frame_body[..];
        let udp_packet = UdpPacket::parse(&mut buf, UdpParseArgs::new(src_ip.get(), dst_ip.get()))
            .expect("Parsed sent UDP packet");
        assert_eq!(udp_packet.src_port().unwrap().get(), 100);
        assert_eq!(udp_packet.dst_port().get(), 200);
        assert_eq!(udp_packet.body(), &body[..]);
    }

    /// Tests that UDP connections fail with an appropriate error for
    /// non-routable remote addresses.
    #[ip_test]
    fn test_udp_conn_unroutable<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        // Set fake context callback to treat all addresses as unroutable.
        let _local_ip = local_ip::<I>();
        let remote_ip = I::get_other_ip_address(127);
        // Create a UDP connection with a specified local port and local IP.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn_err = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip),
            NonZeroU16::new(200).unwrap(),
        )
        .unwrap_err();

        assert_eq!(conn_err, ConnectError::Ip(ResolveRouteError::Unreachable.into()));
    }

    /// Tests that UDP listener creation fails with an appropriate error when
    /// local address is non-local.
    #[ip_test]
    fn test_udp_conn_cannot_bind<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        // Use remote address to trigger IpSockCreationError::LocalAddrNotAssigned.
        let remote_ip = remote_ip::<I>();
        // Create a UDP listener with a specified local port and local ip:
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let result = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(remote_ip)),
            NonZeroU16::new(200),
        );

        assert_eq!(result, Err(LocalAddressError::CannotBindToAddress));
    }

    #[test]
    fn test_udp_conn_picks_link_local_source_address() {
        set_logger_for_test();
        // When the remote address has global scope but the source address
        // is link-local, make sure that the socket implicitly has its bound
        // device set.
        set_logger_for_test();
        let local_ip = SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap();
        let remote_ip = SpecifiedAddr::new(net_ip_v6!("1:2:3:4::")).unwrap();
        const REMOTE_PORT: NonZeroU16 = nonzero!(100u16);
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::<Ipv6>::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip],
            ),
        );
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip),
            REMOTE_PORT,
        )
        .expect("can connect");

        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, conn);
        let (conn_local_ip, conn_remote_ip) = assert_matches!(
            info,
            SocketInfo::Connected(ConnInfo {
                local_ip: ZonedAddr::Zoned(conn_local_ip),
                remote_ip: ZonedAddr::Unzoned(conn_remote_ip),
                local_port: _,
                remote_port: REMOTE_PORT
            }) => (conn_local_ip, conn_remote_ip)
        );
        assert_eq!(
            conn_local_ip,
            AddrAndZone::new(local_ip.get(), FakeWeakDeviceId(FakeDeviceId)).unwrap()
        );
        assert_eq!(conn_remote_ip, remote_ip);

        // Double-check that the bound device can't be changed after being set
        // implicitly.
        assert_eq!(
            SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, conn, None),
            Err(SocketError::Local(
                LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch,)
            ))
        );
    }

    fn set_device_removed<I: TestIpExt>(
        sync_ctx: &mut UdpFakeDeviceSyncCtx<I>,
        device_removed: bool,
    ) {
        let sync_ctx: &mut FakeSyncCtx<_, _, _> = sync_ctx.as_mut();
        let sync_ctx: &mut FakeIpDeviceIdCtx<_> = sync_ctx.get_mut().as_mut();
        sync_ctx.set_device_removed(FakeDeviceId, device_removed);
    }

    #[ip_test]
    #[test_case(
        true,
        Err(IpSockCreationError::Route(ResolveRouteError::Unreachable).into()); "remove device")]
    #[test_case(false, Ok(()); "dont remove device")]
    fn test_udp_conn_device_removed<I: Ip + TestIpExt>(
        remove_device: bool,
        expected: Result<(), ConnectError>,
    ) {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&FakeDeviceId),
        )
        .unwrap();

        if remove_device {
            set_device_removed(&mut sync_ctx, remove_device);
        }

        let remote_ip = remote_ip::<I>();
        assert_eq!(
            SocketHandler::<I, _>::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound.into(),
                ZonedAddr::Unzoned(remote_ip),
                NonZeroU16::new(100).unwrap(),
            )
            .map(|_: SocketId<I>| ()),
            expected,
        );
    }

    /// Tests that UDP connections fail with an appropriate error when local
    /// ports are exhausted.
    #[ip_test]
    fn test_udp_conn_exhausted<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        let local_ip = local_ip::<I>();
        // Exhaust local ports to trigger FailedToAllocateLocalPort error.
        for port_num in UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE {
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            let _: ListenerId<_> = SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                Some(ZonedAddr::Unzoned(local_ip)),
                NonZeroU16::new(port_num),
            )
            .unwrap();
        }

        let remote_ip = remote_ip::<I>();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn_err = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip),
            NonZeroU16::new(100).unwrap(),
        )
        .unwrap_err();

        assert_eq!(conn_err, ConnectError::CouldNotAllocateLocalPort);
    }

    #[ip_test]
    fn test_connect_success<I: Ip + TestIpExt>() {
        set_logger_for_test();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let local_port = nonzero!(100u16);
        let multicast_addr = I::get_multicast_addr(3);
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        // Set some properties on the socket that should be preserved.
        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, unbound, true);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            multicast_addr,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join multicast group should succeed");

        let socket = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(local_port),
        )
        .expect("Initial call to listen_udp was expected to succeed");

        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket.into(),
            ZonedAddr::Unzoned(remote_ip),
            nonzero!(200u16),
        )
        .expect("connect should succeed");

        // Check that socket options set on the listener are propagated to the
        // connected socket.
        assert!(SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, conn));
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::from([((FakeDeviceId, multicast_addr), nonzero!(1usize))])
        );
        assert_eq!(
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                conn,
                multicast_addr,
                MulticastInterfaceSelector::LocalAddress(local_ip).into(),
                true
            ),
            Err(SetMulticastMembershipError::NoMembershipChange)
        );
    }

    #[ip_test]
    fn test_connect_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();
        let remote_ip = I::get_other_ip_address(127);
        let multicast_addr = I::get_multicast_addr(3);
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        // Set some properties on the socket that should be preserved.
        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, unbound, true);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            multicast_addr,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join multicast group should succeed");

        // Create a UDP connection with a specified local port and local IP.
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(nonzero!(100u16)),
        )
        .expect("Initial call to listen_udp was expected to succeed");

        assert_matches!(
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                ZonedAddr::Unzoned(remote_ip),
                nonzero!(1234u16)
            ),
            Err(ConnectError::Ip(IpSockCreationError::Route(ResolveRouteError::Unreachable)))
        );

        // Check that the listener was unchanged by the failed connection.
        assert!(SocketHandler::get_udp_posix_reuse_port(
            &mut sync_ctx,
            &non_sync_ctx,
            listener.into()
        ));
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::from([((FakeDeviceId, multicast_addr), nonzero!(1usize))])
        );
        assert_eq!(
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                multicast_addr,
                MulticastInterfaceSelector::LocalAddress(local_ip).into(),
                true
            ),
            Err(SetMulticastMembershipError::NoMembershipChange)
        );
    }

    #[ip_test]
    fn test_reconnect_udp_conn_success<I: Ip + TestIpExt>() {
        set_logger_for_test();

        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let other_remote_ip = I::get_other_ip_address(3);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip, other_remote_ip],
            ));

        let local_port = NonZeroU16::new(100).unwrap();
        let local_ip = ZonedAddr::Unzoned(local_ip);
        let remote_ip = ZonedAddr::Unzoned(remote_ip);
        let other_remote_ip = ZonedAddr::Unzoned(other_remote_ip);

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let bound = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip),
            Some(local_port),
        )
        .expect("listen should succeed");

        let socket = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            bound.into(),
            remote_ip,
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect was expected to succeed");
        let other_remote_port = NonZeroU16::new(300).unwrap();
        let socket = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            other_remote_ip,
            other_remote_port,
        )
        .expect("connect should succeed");
        assert_eq!(
            SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Connected(ConnInfo {
                local_ip: local_ip.map_zone(FakeWeakDeviceId),
                local_port,
                remote_ip: other_remote_ip.map_zone(FakeWeakDeviceId),
                remote_port: other_remote_port
            })
        );
    }

    #[ip_test]
    fn test_reconnect_udp_conn_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>());
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>());
        let other_remote_ip = ZonedAddr::Unzoned(I::get_other_ip_address(3));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let bound = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");

        let socket = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            bound.into(),
            remote_ip,
            REMOTE_PORT,
        )
        .expect("connect was expected to succeed");
        let other_remote_port = NonZeroU16::new(300).unwrap();
        let error = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            other_remote_ip,
            other_remote_port,
        )
        .expect_err("connect should fail");
        assert_matches!(
            error,
            ConnectError::Ip(IpSockCreationError::Route(ResolveRouteError::Unreachable))
        );

        assert_eq!(
            SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, socket),
            SocketInfo::Connected(ConnInfo {
                local_ip: local_ip.map_zone(FakeWeakDeviceId),
                local_port: LOCAL_PORT,
                remote_ip: remote_ip.map_zone(FakeWeakDeviceId),
                remote_port: REMOTE_PORT
            })
        );
    }

    #[ip_test]
    fn test_send_to<I: Ip + TestIpExt>() {
        set_logger_for_test();

        let local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        let other_remote_ip = I::get_other_ip_address(3);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip, other_remote_ip],
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Unzoned(remote_ip),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        let body = [1, 2, 3, 4, 5];
        // Try to send something with send_to
        assert_matches!(
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                conn,
                ZonedAddr::Unzoned(other_remote_ip),
                NonZeroU16::new(200).unwrap(),
                Buf::new(body.to_vec(), ..),
            )
            .expect("send_to failed"),
            None
        );

        // The socket should not have been affected.
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, conn);
        let info = assert_matches!(info, SocketInfo::Connected(info) => info);
        assert_eq!(info.local_ip, ZonedAddr::Unzoned(local_ip));
        assert_eq!(info.remote_ip, ZonedAddr::Unzoned(remote_ip));
        assert_eq!(info.remote_port, REMOTE_PORT);

        // Check first frame.
        let frames = sync_ctx.inner.frames();
        let (
            SendIpPacketMeta { device: _, src_ip, dst_ip, next_hop, proto, ttl: _, mtu: _ },
            frame_body,
        ) = &frames[0];

        assert_eq!(next_hop, &other_remote_ip);
        assert_eq!(src_ip, &local_ip);
        assert_eq!(dst_ip, &other_remote_ip);
        assert_eq!(proto, &I::Proto::from(IpProto::Udp));
        let mut buf = &frame_body[..];
        let udp_packet = UdpPacket::parse(&mut buf, UdpParseArgs::new(src_ip.get(), dst_ip.get()))
            .expect("Parsed sent UDP packet");
        assert_eq!(udp_packet.src_port().unwrap(), LOCAL_PORT);
        assert_eq!(udp_packet.dst_port(), REMOTE_PORT);
        assert_eq!(udp_packet.body(), &body[..]);
    }

    /// Tests that UDP send failures are propagated as errors.
    ///
    /// Only tests with specified local port and address bounds.
    #[ip_test]
    fn test_send_udp_conn_failure<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let _local_ip = local_ip::<I>();
        let remote_ip = remote_ip::<I>();
        // Create a UDP connection with a specified local port and local IP.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        // Instruct the fake frame context to throw errors.
        let frames: &mut FakeFrameCtx<SendIpPacketMeta<I, _, _>> = sync_ctx.as_mut();
        frames.set_should_error_for_frame(|_frame_meta| true);

        // Now try to send something over this new connection:
        let (_, send_err) = BufferSocketHandler::send(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn,
            Buf::new(Vec::new(), ..),
        )
        .unwrap_err();
        assert_eq!(send_err, Either::Left(SendError::IpSock(IpSockSendError::Mtu)));
    }

    #[ip_test]
    fn test_send_udp_conn_device_removed<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let remote_ip = remote_ip::<I>();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&FakeDeviceId),
        )
        .unwrap();
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        for (device_removed, expected_res) in [
            (false, Ok(())),
            (
                true,
                Err((
                    Buf::new(Vec::new(), ..),
                    Either::Left(SendError::IpSock(IpSockSendError::Unroutable(
                        ResolveRouteError::Unreachable,
                    ))),
                )),
            ),
        ] {
            set_device_removed(&mut sync_ctx, device_removed);

            assert_eq!(
                BufferSocketHandler::send(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    conn,
                    Buf::new(Vec::new(), ..),
                ),
                expected_res,
            )
        }
    }

    #[ip_test]
    #[test_case(false, ShutdownType::Send; "shutdown send then send")]
    #[test_case(false, ShutdownType::SendAndReceive; "shutdown both then send")]
    #[test_case(true, ShutdownType::Send; "shutdown send then sendto")]
    #[test_case(true, ShutdownType::SendAndReceive; "shutdown both then sendto")]
    fn test_send_udp_after_shutdown<I: Ip + TestIpExt>(send_to: bool, shutdown: ShutdownType) {
        set_logger_for_test();

        #[derive(Debug)]
        struct NotWriteableError;

        fn send<
            I: Ip + TestIpExt,
            SC: BufferSocketHandler<I, FakeUdpNonSyncCtx<I>, Buf<Vec<u8>>>,
        >(
            remote_ip: Option<ZonedAddr<I::Addr, SC::DeviceId>>,
            sync_ctx: &mut SC,
            non_sync_ctx: &mut FakeUdpNonSyncCtx<I>,
            id: SocketId<I>,
        ) -> Result<(), NotWriteableError> {
            match remote_ip {
                Some(remote_ip) => BufferSocketHandler::send_to(
                    sync_ctx,
                    non_sync_ctx,
                    id,
                    remote_ip,
                    REMOTE_PORT,
                    Buf::new(Vec::new(), ..),
                )
                .map(|new_id| assert_matches!(new_id, None))
                .map_err(
                    |(_, e)| assert_matches!(e, Either::Right((None, SendToError::NotWriteable)) => NotWriteableError)
                ),
                None => BufferSocketHandler::send(
                    sync_ctx,
                    non_sync_ctx,
                    id,
                    Buf::new(Vec::new(), ..),
                )
                .map_err(|(_, e)| assert_matches!(e, Either::Left(SendError::NotWriteable) => NotWriteableError)),
            }
        }

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>());
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(200));
        let send_to_ip = send_to.then_some(remote_ip);

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            remote_ip,
            REMOTE_PORT,
        )
        .expect("connect failed");

        send(send_to_ip, &mut sync_ctx, &mut non_sync_ctx, conn).expect("can send");
        SocketHandler::shutdown(&mut sync_ctx, &non_sync_ctx, conn, shutdown)
            .expect("is connected");

        assert_matches!(
            send(send_to_ip, &mut sync_ctx, &mut non_sync_ctx, conn),
            Err(NotWriteableError)
        );
    }

    #[ip_test]
    #[test_case(ShutdownType::Receive; "receive")]
    #[test_case(ShutdownType::SendAndReceive; "both")]
    fn test_marked_for_receive_shutdown<I: Ip + TestIpExt>(which: ShutdownType) {
        set_logger_for_test();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        const REMOTE_PORT: NonZeroU16 = const_unwrap_option(NonZeroU16::new(200));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let bound = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip::<I>().into()),
            Some(LOCAL_PORT),
        )
        .expect("can bind");
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            bound.into(),
            ZonedAddr::Unzoned(remote_ip::<I>()),
            REMOTE_PORT,
        )
        .expect("can connect");

        // Receive once, then set the shutdown flag, then receive again and
        // check that it doesn't get to the socket.

        let packet = [1, 1, 1, 1];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip::<I>().get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &packet[..],
        );

        assert_matches!(&non_sync_ctx.state().conn_data[..], [_]);
        SocketHandler::shutdown(&mut sync_ctx, &non_sync_ctx, conn, which).expect("is connected");
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip::<I>().get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &packet[..],
        );
        assert_matches!(&non_sync_ctx.state().conn_data[..], [_]);

        // Calling shutdown for the send direction doesn't change anything.
        SocketHandler::shutdown(&mut sync_ctx, &non_sync_ctx, conn, ShutdownType::Send)
            .expect("is connected");
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip::<I>().get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &packet[..],
        );
        assert_matches!(&non_sync_ctx.state().conn_data[..], [_]);
    }

    /// Tests that if we have multiple listeners and connections, demuxing the
    /// flows is performed correctly.
    #[ip_test]
    fn test_udp_demux<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let local_ip = local_ip::<I>();
        let remote_ip_a = I::get_other_ip_address(70);
        let remote_ip_b = I::get_other_ip_address(72);
        let local_port_a = NonZeroU16::new(100).unwrap();
        let local_port_b = NonZeroU16::new(101).unwrap();
        let local_port_c = NonZeroU16::new(102).unwrap();
        let local_port_d = NonZeroU16::new(103).unwrap();
        let remote_port_a = NonZeroU16::new(200).unwrap();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(
                vec![local_ip],
                vec![remote_ip_a, remote_ip_b],
            ));

        // Create some UDP connections and listeners:
        // conn2 has just a remote addr different than conn1, which requires
        // allowing them to share the local port.
        let [conn1, conn2] = [remote_ip_a, remote_ip_b].map(|remote_ip| {
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                true,
            );
            let listen = SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                Some(ZonedAddr::Unzoned(local_ip)),
                Some(local_port_d),
            )
            .expect("connect failed");
            SocketHandler::<I, _>::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listen.into(),
                ZonedAddr::Unzoned(remote_ip),
                remote_port_a,
            )
            .expect("connect failed")
        });
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let list1 = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(local_port_a),
        )
        .expect("listen_udp failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let list2 = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(local_port_b),
        )
        .expect("listen_udp failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let wildcard_list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            Some(local_port_c),
        )
        .expect("listen_udp failed");

        // Now inject UDP packets that each of the created connections should
        // receive.
        let body_conn1 = [1, 1, 1, 1];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_d,
            &body_conn1[..],
        );
        let body_conn2 = [2, 2, 2, 2];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_b.get(),
            local_ip.get(),
            remote_port_a,
            local_port_d,
            &body_conn2[..],
        );
        let body_list1 = [3, 3, 3, 3];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_a,
            &body_list1[..],
        );
        let body_list2 = [4, 4, 4, 4];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_b,
            &body_list2[..],
        );
        let body_wildcard_list = [5, 5, 5, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip.get(),
            remote_port_a,
            local_port_c,
            &body_wildcard_list[..],
        );
        // Check that we got everything in order.
        let conn_packets = &non_sync_ctx.state().conn_data;
        assert_eq!(conn_packets.len(), 2);
        let pkt = &conn_packets[0];
        assert_eq!(pkt.conn, conn1.try_into().unwrap());
        assert_eq!(pkt.body, &body_conn1[..]);
        let pkt = &conn_packets[1];
        assert_eq!(pkt.conn, conn2.try_into().unwrap());
        assert_eq!(pkt.body, &body_conn2[..]);

        let list_packets = &non_sync_ctx.state().listen_data;
        assert_eq!(list_packets.len(), 3);
        let pkt = &list_packets[0];
        assert_eq!(pkt.listener, list1);
        assert_eq!(pkt.src_ip, remote_ip_a.get());
        assert_eq!(pkt.dst_ip, local_ip.get());
        assert_eq!(pkt.src_port.unwrap(), remote_port_a);
        assert_eq!(pkt.body, &body_list1[..]);

        let pkt = &list_packets[1];
        assert_eq!(pkt.listener, list2);
        assert_eq!(pkt.src_ip, remote_ip_a.get());
        assert_eq!(pkt.dst_ip, local_ip.get());
        assert_eq!(pkt.src_port.unwrap(), remote_port_a);
        assert_eq!(pkt.body, &body_list2[..]);

        let pkt = &list_packets[2];
        assert_eq!(pkt.listener, wildcard_list);
        assert_eq!(pkt.src_ip, remote_ip_a.get());
        assert_eq!(pkt.dst_ip, local_ip.get());
        assert_eq!(pkt.src_port.unwrap(), remote_port_a);
        assert_eq!(pkt.body, &body_wildcard_list[..]);
    }

    /// Tests UDP wildcard listeners for different IP versions.
    #[ip_test]
    fn test_wildcard_listeners<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip_a = I::get_other_ip_address(1);
        let local_ip_b = I::get_other_ip_address(2);
        let remote_ip_a = I::get_other_ip_address(70);
        let remote_ip_b = I::get_other_ip_address(72);
        let listener_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            Some(listener_port),
        )
        .expect("listen_udp failed");

        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_a.get(),
            local_ip_a.get(),
            remote_port,
            listener_port,
            &body[..],
        );
        // Receive into a different local IP.
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            remote_ip_b.get(),
            local_ip_b.get(),
            remote_port,
            listener_port,
            &body[..],
        );

        // Check that we received both packets for the listener.
        let listen_packets = &non_sync_ctx.state().listen_data;
        assert_eq!(listen_packets.len(), 2);
        let pkt = &listen_packets[0];
        assert_eq!(pkt.listener, listener);
        assert_eq!(pkt.src_ip, remote_ip_a.get());
        assert_eq!(pkt.dst_ip, local_ip_a.get());
        assert_eq!(pkt.src_port.unwrap(), remote_port);
        assert_eq!(pkt.body, &body[..]);
        let pkt = &listen_packets[1];
        assert_eq!(pkt.listener, listener);
        assert_eq!(pkt.src_ip, remote_ip_b.get());
        assert_eq!(pkt.dst_ip, local_ip_b.get());
        assert_eq!(pkt.src_port.unwrap(), remote_port);
        assert_eq!(pkt.body, &body[..]);
    }

    #[ip_test]
    fn test_receive_source_port_zero_on_listener<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            Some(LOCAL_PORT),
        )
        .expect("listen_udp failed");

        let body = [];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            I::FAKE_CONFIG.remote_ip.get(),
            I::FAKE_CONFIG.local_ip.get(),
            0u16,
            LOCAL_PORT,
            &body[..],
        );
        // Check that we received both packets for the listener.
        assert_eq!(
            non_sync_ctx.state().listen_data(),
            HashMap::from([(listener, vec![[].as_slice()])]),
        );
    }

    #[ip_test]
    fn test_receive_source_addr_unspecified_on_listener<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            Some(LOCAL_PORT),
        )
        .expect("listen_udp failed");

        let body = [];
        receive_udp_packet(
            &mut sync_ctx,
            &mut non_sync_ctx,
            FakeDeviceId,
            I::UNSPECIFIED_ADDRESS,
            I::FAKE_CONFIG.local_ip.get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &body[..],
        );
        // Check that we received both packets for the listener.
        assert_eq!(
            non_sync_ctx.state().listen_data(),
            HashMap::from([(listener, vec![[].as_slice()])]),
        );
    }

    #[ip_test]
    #[test_case(nonzero!(u16::MAX), Ok(nonzero!(u16::MAX)); "ephemeral available")]
    #[test_case(nonzero!(100u16), Err(LocalAddressError::FailedToAllocateLocalPort);
        "no ephemeral available")]
    fn test_bind_picked_port_all_others_taken<I: Ip + TestIpExt>(
        available_port: NonZeroU16,
        expected_result: Result<NonZeroU16, LocalAddressError>,
    ) {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        for port in 1..=u16::MAX {
            let port = NonZeroU16::new(port).unwrap();
            if port == available_port {
                continue;
            }
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            let _listener = SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(port),
            )
            .expect("uncontested bind");
        }

        // Now that all but the LOCAL_PORT are occupied, ask the stack to
        // select a port.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let result =
            SocketHandler::listen_udp(&mut sync_ctx, &mut non_sync_ctx, unbound, None, None).map(
                |listener| {
                    let info = SocketHandler::get_udp_info(
                        &mut sync_ctx,
                        &mut non_sync_ctx,
                        listener.into(),
                    );
                    assert_matches!(info, SocketInfo::Listener(info) => info.local_port)
                },
            );
        assert_eq!(result, expected_result);
    }

    #[ip_test]
    fn test_receive_multicast_packet<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let local_ip = local_ip::<I>();
        let remote_ip = I::get_other_ip_address(70);
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let multicast_addr = I::get_multicast_addr(0);
        let multicast_addr_other = I::get_multicast_addr(1);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(vec![local_ip], vec![remote_ip]),
        );

        // Create 3 sockets: one listener for all IPs, two listeners on the same
        // local address.
        let any_listener = {
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                true,
            );
            SocketHandler::<I, _>::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(local_port),
            )
            .expect("listen_udp failed")
        };

        let specific_listeners = [(); 2].map(|()| {
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                true,
            );
            SocketHandler::<I, _>::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                Some(ZonedAddr::Unzoned(multicast_addr.into_specified())),
                Some(local_port),
            )
            .expect("listen_udp failed")
        });

        let mut receive_packet = |body, local_ip: MulticastAddr<I::Addr>| {
            let body = [body];
            receive_udp_packet(
                &mut sync_ctx,
                &mut non_sync_ctx,
                FakeDeviceId,
                remote_ip.get(),
                local_ip.get(),
                remote_port,
                local_port,
                &body,
            )
        };

        // These packets should be received by all listeners.
        receive_packet(1, multicast_addr);
        receive_packet(2, multicast_addr);

        // This packet should be received only by the all-IPs listener.
        receive_packet(3, multicast_addr_other);

        assert_eq!(
            non_sync_ctx.state().listen_data(),
            HashMap::from([
                (specific_listeners[0], vec![[1].as_slice(), &[2]]),
                (specific_listeners[1], vec![&[1], &[2]]),
                (any_listener, vec![&[1], &[2], &[3]]),
            ]),
        );
    }

    type UdpMultipleDevicesCtx<I> = FakeUdpCtx<I, MultipleDevicesId>;
    type UdpMultipleDevicesSyncCtx<I> = FakeUdpSyncCtx<I, MultipleDevicesId>;
    type UdpMultipleDevicesNonSyncCtx<I> = FakeUdpNonSyncCtx<I>;

    impl<I: TestIpExt> Default for FakeUdpSyncCtx<I, MultipleDevicesId> {
        fn default() -> Self {
            let remote_ips = vec![I::get_other_remote_ip_address(1)];
            Self::with_inner_and_outer_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    MultipleDevicesId::all().into_iter().enumerate().map(|(i, device)| {
                        FakeDeviceConfig {
                            device,
                            local_ips: vec![I::get_other_ip_address((i + 1).try_into().unwrap())],
                            remote_ips: remote_ips.clone(),
                        }
                    }),
                )),
                FakeUdpState::default(),
            )
        }
    }

    /// Tests that if sockets are bound to devices, they will only receive
    /// packets that are received on those devices.
    #[ip_test]
    fn test_bound_to_device_receive<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());
        let sync_ctx = &mut sync_ctx;
        let bound_first_device = {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            let listen = SocketHandler::listen_udp(
                sync_ctx,
                &mut non_sync_ctx,
                unbound,
                Some(ZonedAddr::Unzoned(local_ip::<I>())),
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed");
            let conn = SocketHandler::connect(
                sync_ctx,
                &mut non_sync_ctx,
                listen.into(),
                ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)),
                REMOTE_PORT,
            )
            .expect("connect should succeed");
            SocketHandler::set_device(
                sync_ctx,
                &mut non_sync_ctx,
                conn,
                Some(&MultipleDevicesId::A),
            )
            .expect("bind should succeed");
            conn
        };

        let bound_second_device = {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            SocketHandler::set_device(
                sync_ctx,
                &mut non_sync_ctx,
                unbound.into(),
                Some(&MultipleDevicesId::B),
            )
            .unwrap();
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, unbound, None, Some(LOCAL_PORT))
                .expect("listen should succeed")
        };

        // Inject a packet received on `MultipleDevicesId::A` from the specified
        // remote; this should go to the first socket.
        let body = [1, 2, 3, 4, 5];
        receive_udp_packet(
            sync_ctx,
            &mut non_sync_ctx,
            MultipleDevicesId::A,
            I::get_other_remote_ip_address(1).get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &body[..],
        );

        let conn_data = &non_sync_ctx.state().conn_data;
        assert_matches!(&conn_data[..], &[ConnData {conn, body: _ }] if conn == bound_first_device.try_into().unwrap());

        // A second packet received on `MultipleDevicesId::B` will go to the
        // second socket.
        receive_udp_packet(
            sync_ctx,
            &mut non_sync_ctx,
            MultipleDevicesId::B,
            I::get_other_remote_ip_address(1).get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &body[..],
        );

        let listen_data = &non_sync_ctx.state().listen_data;
        assert_matches!(&listen_data[..], &[ListenData {
            listener, src_ip: _, dst_ip: _, src_port: _, body: _
        }] if listener== bound_second_device);
    }

    /// Tests that if sockets are bound to devices, they will send packets out
    /// of those devices.
    #[ip_test]
    fn test_bound_to_device_send<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());
        let sync_ctx = &mut sync_ctx;
        let bound_on_devices = MultipleDevicesId::all().map(|device| {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, unbound.into(), Some(&device))
                .unwrap();
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, unbound, None, Some(LOCAL_PORT))
                .expect("listen should succeed")
        });

        // Send a packet from each socket.
        let body = [1, 2, 3, 4, 5];
        for socket in bound_on_devices {
            assert_matches!(
                BufferSocketHandler::send_to(
                    sync_ctx,
                    &mut non_sync_ctx,
                    socket.into(),
                    ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)),
                    REMOTE_PORT,
                    Buf::new(body.to_vec(), ..),
                )
                .expect("send should succeed"),
                None
            );
        }

        let mut received_devices = sync_ctx
            .inner
            .frames()
            .iter()
            .map(
                |(
                    SendIpPacketMeta {
                        device,
                        src_ip: _,
                        dst_ip,
                        next_hop: _,
                        proto,
                        ttl: _,
                        mtu: _,
                    },
                    _body,
                )| {
                    assert_eq!(proto, &IpProto::Udp.into());
                    assert_eq!(dst_ip, &I::get_other_remote_ip_address(1),);
                    *device
                },
            )
            .collect::<Vec<_>>();
        received_devices.sort();
        assert_eq!(received_devices, &MultipleDevicesId::all());
    }

    fn receive_packet_on<I: Ip + TestIpExt>(
        sync_ctx: &mut UdpMultipleDevicesSyncCtx<I>,
        ctx: &mut UdpMultipleDevicesNonSyncCtx<I>,
        device: MultipleDevicesId,
    ) {
        const BODY: [u8; 5] = [1, 2, 3, 4, 5];
        receive_udp_packet(
            sync_ctx,
            ctx,
            device,
            I::get_other_remote_ip_address(1).get(),
            local_ip::<I>().get(),
            REMOTE_PORT,
            LOCAL_PORT,
            &BODY[..],
        )
    }

    /// Check that sockets can be bound to and unbound from devices.
    #[ip_test]
    fn test_bind_unbind_device<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());
        let sync_ctx = &mut sync_ctx;

        // Start with `socket` bound to a device on all IPs.
        let socket = {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            SocketHandler::set_device(
                sync_ctx,
                &mut non_sync_ctx,
                unbound.into(),
                Some(&MultipleDevicesId::A),
            )
            .unwrap();
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, unbound, None, Some(LOCAL_PORT))
                .expect("listen failed")
        };

        // Since it is bound, it does not receive a packet from another device.
        receive_packet_on(sync_ctx, &mut non_sync_ctx, MultipleDevicesId::B);
        let listen_data = &non_sync_ctx.state().listen_data;
        assert_matches!(&listen_data[..], &[]);

        // When unbound, the socket can receive packets on the other device.
        SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket.into(), None)
            .expect("clearing bound device failed");
        receive_packet_on(sync_ctx, &mut non_sync_ctx, MultipleDevicesId::B);
        let listen_data = &non_sync_ctx.state().listen_data;
        assert_matches!(&listen_data[..],
            &[ListenData {listener, body:_, src_ip: _, dst_ip: _, src_port: _ }] =>
            assert_eq!(listener, socket));
    }

    /// Check that bind fails as expected when it would cause illegal shadowing.
    #[ip_test]
    fn test_unbind_device_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());
        let sync_ctx = &mut sync_ctx;

        let bound_on_devices = MultipleDevicesId::all().map(|device| {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, unbound.into(), Some(&device))
                .unwrap();
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, unbound, None, Some(LOCAL_PORT))
                .expect("listen should succeed")
        });

        // Clearing the bound device is not allowed for either socket since it
        // would then be shadowed by the other socket.
        for socket in bound_on_devices {
            assert_matches!(
                SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, socket.into(), None),
                Err(SocketError::Local(LocalAddressError::AddressInUse))
            );
        }
    }

    /// Check that binding a device fails if it would make a connected socket
    /// unroutable.
    #[ip_test]
    fn test_bind_conn_socket_device_fails<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let device_configs = HashMap::from(
            [(MultipleDevicesId::A, 1), (MultipleDevicesId::B, 2)].map(|(device, i)| {
                (
                    device,
                    FakeDeviceConfig {
                        device,
                        local_ips: vec![I::get_other_ip_address(i)],
                        remote_ips: vec![I::get_other_remote_ip_address(i)],
                    },
                )
            }),
        );
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    device_configs.iter().map(|(_, v)| v).cloned(),
                )),
            ));

        let socket = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let socket = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket.into(),
            ZonedAddr::Unzoned(device_configs[&MultipleDevicesId::A].remote_ips[0]),
            LOCAL_PORT,
        )
        .expect("connect should succeed");

        // `socket` is not explicitly bound to device `A` but its route must
        // go through it because of the destination address. Therefore binding
        // to device `B` wil not work.
        assert_matches!(
            SocketHandler::set_device(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                Some(&MultipleDevicesId::B)
            ),
            Err(SocketError::Remote(RemoteAddressError::NoRoute))
        );

        // Binding to device `A` should be fine.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            Some(&MultipleDevicesId::A),
        )
        .expect("routing picked A already");
    }

    #[ip_test]
    fn test_bound_device_receive_multicast_packet<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let remote_ip = I::get_other_ip_address(1);
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let multicast_addr = I::get_multicast_addr(0);

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());

        // Create 3 sockets: one listener bound on each device and one not bound
        // to a device.

        let sync_ctx = &mut sync_ctx;
        let bound_on_devices = MultipleDevicesId::all().map(|device| {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            SocketHandler::set_device(sync_ctx, &mut non_sync_ctx, unbound.into(), Some(&device))
                .unwrap();
            SocketHandler::set_udp_posix_reuse_port(sync_ctx, &mut non_sync_ctx, unbound, true);
            let listener = SocketHandler::listen_udp(
                sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed");

            (device, listener)
        });

        let listener = {
            let unbound = SocketHandler::create_udp_unbound(sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(sync_ctx, &mut non_sync_ctx, unbound, true);
            SocketHandler::listen_udp(sync_ctx, &mut non_sync_ctx, unbound, None, Some(LOCAL_PORT))
                .expect("listen should succeed")
        };

        fn index_for_device(id: MultipleDevicesId) -> u8 {
            match id {
                MultipleDevicesId::A => 0,
                MultipleDevicesId::B => 1,
                MultipleDevicesId::C => 2,
            }
        }

        let mut receive_packet = |remote_ip: SpecifiedAddr<I::Addr>, device: MultipleDevicesId| {
            let body = vec![index_for_device(device)];
            receive_udp_packet(
                sync_ctx,
                &mut non_sync_ctx,
                device,
                remote_ip.get(),
                multicast_addr.get(),
                remote_port,
                local_port,
                &body,
            )
        };

        // Receive packets from the remote IP on each device (2 packets total).
        // Listeners bound on devices should receive one, and the other listener
        // should receive both.
        for device in MultipleDevicesId::all() {
            receive_packet(remote_ip, device);
        }

        let listen_data = non_sync_ctx.state().listen_data();

        for (device, listener) in bound_on_devices {
            assert_eq!(listen_data[&listener], vec![&[index_for_device(device)]]);
        }
        let expected_listener_data = &MultipleDevicesId::all().map(|d| vec![index_for_device(d)]);
        assert_eq!(&listen_data[&listener], expected_listener_data);
    }

    /// Tests establishing a UDP connection without providing a local IP
    #[ip_test]
    fn test_conn_unspecified_local_ip<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            Some(local_port),
        )
        .expect("listen_udp failed");
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Unzoned(remote_ip::<I>()),
            remote_port,
        )
        .expect("connect failed");
        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        let (_state, _tag_state, addr) =
            Connection::from_socket_state_ref(bound_state.get(&conn.try_into().unwrap()).unwrap());

        assert_eq!(
            addr,
            &ConnAddr {
                ip: ConnIpAddr {
                    local: (local_ip::<I>(), local_port),
                    remote: (remote_ip::<I>(), remote_port),
                },
                device: None
            }
        );
    }

    #[derive(Debug)]
    pub(crate) struct ExpectedBoundError;

    impl<I: IpExt, D: WeakId> TryFrom<SocketId<I>> for DatagramBoundId<Udp<I, D>> {
        type Error = ExpectedBoundError;
        fn try_from(SocketId(id): SocketId<I>) -> Result<Self, Self::Error> {
            match id {
                SocketIdInner::Unbound(_) => Err(ExpectedBoundError),
                SocketIdInner::Bound(id) => Ok(id.into()),
            }
        }
    }
    impl<I: Ip> TryFrom<SocketId<I>> for ConnId<I> {
        type Error = ExpectedConnError;
        fn try_from(SocketId(id): SocketId<I>) -> Result<Self, Self::Error> {
            match id {
                SocketIdInner::Bound(id) => id.try_into(),
                SocketIdInner::Unbound(_) => Err(ExpectedConnError),
            }
        }
    }

    impl<I: Ip> TryFrom<BoundId<I>> for ConnId<I> {
        type Error = ExpectedConnError;
        fn try_from(id: BoundId<I>) -> Result<Self, Self::Error> {
            match id {
                BoundId::Connected(id) => Ok(id),
                BoundId::Listening(_) => Err(ExpectedConnError),
            }
        }
    }

    /// Tests local port allocation for [`connect`].
    ///
    /// Tests that calling [`connect`] causes a valid local port to be
    /// allocated.
    #[ip_test]
    fn test_udp_local_port_alloc<I: Ip + TestIpExt>() {
        let local_ip = local_ip::<I>();
        let ip_a = I::get_other_ip_address(100);
        let ip_b = I::get_other_ip_address(200);

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(vec![local_ip], vec![ip_a, ip_b]),
        );

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn_a = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(ip_a),
            NonZeroU16::new(1010).unwrap(),
        )
        .expect("connect failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn_b = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(ip_b),
            NonZeroU16::new(1010).unwrap(),
        )
        .expect("connect failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn_c = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(ip_a),
            NonZeroU16::new(2020).unwrap(),
        )
        .expect("connect failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let conn_d = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(ip_a),
            NonZeroU16::new(1010).unwrap(),
        )
        .expect("connect failed");
        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        let valid_range = &UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE;
        let port_a = assert_matches!(bound_state.get(&conn_a.try_into().unwrap()),
            Some(SocketState::Connected((_state, _tag_state, ConnAddr{ip: ConnIpAddr{local: (_, local_identifier), ..}, device: _}))) => local_identifier)
        .get();
        assert!(valid_range.contains(&port_a));
        let port_b = assert_matches!(bound_state.get(&conn_b.try_into().unwrap()),
            Some(SocketState::Connected((_state, _tag_state, ConnAddr{ip: ConnIpAddr{local: (_, local_identifier), ..}, device: _}))) => local_identifier)
        .get();
        assert_ne!(port_a, port_b);
        let port_c = assert_matches!(bound_state.get(&conn_c.try_into().unwrap()),
            Some(SocketState::Connected((_state, _tag_state, ConnAddr{ip: ConnIpAddr{local: (_, local_identifier), ..}, device: _}))) => local_identifier)
        .get();
        assert_ne!(port_a, port_c);
        let port_d = assert_matches!(bound_state.get(&conn_d.try_into().unwrap()),
            Some(SocketState::Connected((_state, _tag_state, ConnAddr{ip: ConnIpAddr{local: (_, local_identifier), ..}, device: _}))) => local_identifier)
        .get();
        assert_ne!(port_a, port_d);
    }

    /// Tests that if `listen_udp` fails, it can be retried later.
    #[ip_test]
    fn test_udp_retry_listen_after_removing_conflict<I: Ip + TestIpExt>() {
        set_logger_for_test();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        fn listen_unbound<I: Ip + TestIpExt, C: StateNonSyncContext<I>>(
            sync_ctx: &mut impl StateContext<I, C>,
            non_sync_ctx: &mut C,
            unbound: UnboundId<I>,
        ) -> Result<ListenerId<I>, LocalAddressError> {
            SocketHandler::<I, _>::listen_udp(
                sync_ctx,
                non_sync_ctx,
                unbound,
                Some(ZonedAddr::Unzoned(local_ip::<I>())),
                Some(NonZeroU16::new(100).unwrap()),
            )
        }

        // Tie up the address so the second call to `connect` fails.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = listen_unbound(&mut sync_ctx, &mut non_sync_ctx, unbound)
            .expect("Initial call to listen_udp was expected to succeed");

        // Trying to connect on the same address should fail.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        assert_eq!(
            listen_unbound(&mut sync_ctx, &mut non_sync_ctx, unbound),
            Err(LocalAddressError::AddressInUse)
        );

        // Once the first listener is removed, the second socket can be
        // connected.
        let _: SocketInfo<_, _> =
            SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, listener.into());

        let _: ListenerId<_> = listen_unbound(&mut sync_ctx, &mut non_sync_ctx, unbound)
            .expect("listen should succeed");
    }

    /// Tests local port allocation for [`listen_udp`].
    ///
    /// Tests that calling [`listen_udp`] causes a valid local port to be
    /// allocated when no local port is passed.
    #[ip_test]
    fn test_udp_listen_port_alloc<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let wildcard_list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            None,
        )
        .expect("listen_udp failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let specified_list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            None,
        )
        .expect("listen_udp failed");

        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        let wildcard_port = assert_matches!(bound_state.get(&DatagramBoundId::Listener(wildcard_list)),
            Some(SocketState::Listener((
                _,
                _,
                ListenerAddr{ ip: ListenerIpAddr {identifier, addr: None}, device: None}
            ))) => identifier);
        let specified_port = assert_matches!(bound_state.get(&DatagramBoundId::Listener(specified_list)),
            Some(SocketState::Listener((
                _,
                _,
                ListenerAddr{ ip: ListenerIpAddr {identifier, addr: _}, device: None}
            ))) => identifier);
        assert!(UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE
            .contains(&wildcard_port.get()));
        assert!(UdpBoundSocketMap::<I, FakeWeakDeviceId<FakeDeviceId>>::EPHEMERAL_RANGE
            .contains(&specified_port.get()));
        assert_ne!(wildcard_port, specified_port);
    }

    #[ip_test]
    fn test_bind_multiple_reuse_port<I: Ip + TestIpExt>() {
        let local_port = NonZeroU16::new(100).unwrap();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let listeners = [(), ()].map(|()| {
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                true,
            );
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(local_port),
            )
            .expect("listen_udp failed")
        });

        let expected_addr = ListenerAddr {
            ip: ListenerIpAddr { addr: None, identifier: local_port },
            device: None,
        };
        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        for listener in listeners {
            assert_matches!(bound_state.get(&DatagramBoundId::Listener(listener)),
                Some(SocketState::Listener((_, _, addr))) => assert_eq!(addr, &expected_addr));
        }
    }

    #[ip_test]
    fn test_set_unset_reuse_port<I: Ip + TestIpExt>() {
        let local_port = NonZeroU16::new(100).unwrap();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let _listener = {
            let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                true,
            );
            SocketHandler::set_udp_posix_reuse_port(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                false,
            );
            SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(local_port),
            )
            .expect("listen_udp failed")
        };

        // Because there is already a listener bound without `SO_REUSEPORT` set,
        // the next bind to the same address should fail.
        assert_eq!(
            {
                let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
                SocketHandler::listen_udp(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    unbound,
                    None,
                    Some(local_port),
                )
            },
            Err(LocalAddressError::AddressInUse)
        );
    }

    /// Tests [`remove_udp`]
    #[ip_test]
    fn test_remove_udp_conn<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>());
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>());
        let local_port = NonZeroU16::new(100).unwrap();
        let remote_port = NonZeroU16::new(200).unwrap();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip),
            Some(local_port),
        )
        .unwrap();
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            remote_ip,
            remote_port,
        )
        .expect("connect failed");
        let info = SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, conn);
        let info = assert_matches!(info, SocketInfo::Connected(info) => info);
        // Assert that the info gotten back matches what was expected.
        assert_eq!(info.local_ip, local_ip.map_zone(FakeWeakDeviceId));
        assert_eq!(info.local_port, local_port);
        assert_eq!(info.remote_ip, remote_ip.map_zone(FakeWeakDeviceId));
        assert_eq!(info.remote_port, remote_port);

        // Assert that that connection id was removed from the connections
        // state.
        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        assert_matches!(bound_state.get(&DatagramBoundId::Listener(listener)), None);
    }

    /// Tests [`remove_udp`]
    #[ip_test]
    fn test_remove_udp_listener<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>());
        let local_port = NonZeroU16::new(100).unwrap();

        // Test removing a specified listener.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip),
            Some(local_port),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, list.into());
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip.unwrap(), local_ip.map_zone(FakeWeakDeviceId));
        assert_eq!(info.local_port, local_port);
        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        assert_matches!(bound_state.get(&DatagramBoundId::Listener(list)), None);

        // Test removing a wildcard listener.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            Some(local_port),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, list.into());
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip, None);
        assert_eq!(info.local_port, local_port);
        let Sockets {
            sockets: DatagramSockets { bound_state, bound: _, unbound: _ },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        assert_matches!(bound_state.get(&DatagramBoundId::Listener(list)), None);
    }

    fn try_join_leave_multicast<I: Ip + TestIpExt>(
        mcast_addr: MulticastAddr<I::Addr>,
        interface: MulticastMembershipInterfaceSelector<I::Addr, MultipleDevicesId>,
        make_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx<I>,
            &mut UdpFakeDeviceNonSyncCtx<I>,
            UnboundId<I>,
        ) -> SocketId<I>,
    ) -> (
        Result<(), SetMulticastMembershipError>,
        HashMap<(MultipleDevicesId, MulticastAddr<I::Addr>), NonZeroUsize>,
    )
where {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let socket = make_socket(&mut sync_ctx, &mut non_sync_ctx, unbound);
        let result = SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            socket,
            mcast_addr,
            interface,
            true,
        );

        let memberships_snapshot = AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
            .multicast_memberships();
        if let Ok(()) = result {
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                socket,
                mcast_addr,
                interface,
                false,
            )
            .expect("leaving group failed");
        }
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::default()
        );

        (result, memberships_snapshot)
    }

    fn leave_unbound<I: TestIpExt>(
        _sync_ctx: &mut UdpMultipleDevicesSyncCtx<I>,
        _non_sync_ctx: &mut UdpFakeDeviceNonSyncCtx<I>,
        unbound: UnboundId<I>,
    ) -> SocketId<I> {
        unbound.into()
    }

    fn bind_as_listener<I: TestIpExt>(
        sync_ctx: &mut UdpMultipleDevicesSyncCtx<I>,
        non_sync_ctx: &mut UdpFakeDeviceNonSyncCtx<I>,
        unbound: UnboundId<I>,
    ) -> SocketId<I>
where {
        SocketHandler::<I, _>::listen_udp(
            sync_ctx,
            non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip::<I>())),
            NonZeroU16::new(100),
        )
        .expect("listen should succeed")
        .into()
    }

    fn bind_as_connected<I: TestIpExt>(
        sync_ctx: &mut UdpMultipleDevicesSyncCtx<I>,
        non_sync_ctx: &mut UdpFakeDeviceNonSyncCtx<I>,
        unbound: UnboundId<I>,
    ) -> SocketId<I>
where {
        SocketHandler::<I, _>::connect(
            sync_ctx,
            non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect should succeed")
    }

    fn iface_id<A: IpAddress>(
        id: MultipleDevicesId,
    ) -> MulticastInterfaceSelector<A, MultipleDevicesId> {
        MulticastInterfaceSelector::Interface(id)
    }
    fn iface_addr<A: IpAddress>(
        addr: SpecifiedAddr<A>,
    ) -> MulticastInterfaceSelector<A, MultipleDevicesId> {
        MulticastInterfaceSelector::LocalAddress(addr)
    }

    #[ip_test]
    #[test_case(iface_id(MultipleDevicesId::A), leave_unbound::<I>; "device_no_addr_unbound")]
    #[test_case(iface_addr(local_ip::<I>()), leave_unbound::<I>; "addr_no_device_unbound")]
    #[test_case(iface_id(MultipleDevicesId::A), bind_as_listener::<I>; "device_no_addr_listener")]
    #[test_case(iface_addr(local_ip::<I>()), bind_as_listener::<I>; "addr_no_device_listener")]
    #[test_case(iface_id(MultipleDevicesId::A), bind_as_connected::<I>; "device_no_addr_connected")]
    #[test_case(iface_addr(local_ip::<I>()), bind_as_connected::<I>; "addr_no_device_connected")]
    fn test_join_leave_multicast_succeeds<I: Ip + TestIpExt>(
        interface: MulticastInterfaceSelector<I::Addr, MultipleDevicesId>,
        make_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx<I>,
            &mut UdpFakeDeviceNonSyncCtx<I>,
            UnboundId<I>,
        ) -> SocketId<I>,
    ) {
        let mcast_addr = I::get_multicast_addr(3);

        let (result, ip_options) =
            try_join_leave_multicast(mcast_addr, interface.into(), make_socket);
        assert_eq!(result, Ok(()));
        assert_eq!(
            ip_options,
            HashMap::from([((MultipleDevicesId::A, mcast_addr), nonzero!(1usize))])
        );
    }

    #[ip_test]
    #[test_case(MultipleDevicesId::A, Some(local_ip::<I>()), leave_unbound, Ok(());
        "with_ip_unbound")]
    #[test_case(MultipleDevicesId::A, None, leave_unbound, Ok(());
        "without_ip_unbound")]
    #[test_case(MultipleDevicesId::A, Some(local_ip::<I>()), bind_as_listener, Ok(());
        "with_ip_listener")]
    #[test_case(MultipleDevicesId::A, Some(local_ip::<I>()), bind_as_connected, Ok(());
        "with_ip_connected")]
    fn test_join_leave_multicast_interface_inferred_from_bound_device<I: Ip + TestIpExt>(
        bound_device: MultipleDevicesId,
        interface_addr: Option<SpecifiedAddr<I::Addr>>,
        make_socket: impl FnOnce(
            &mut UdpMultipleDevicesSyncCtx<I>,
            &mut UdpFakeDeviceNonSyncCtx<I>,
            UnboundId<I>,
        ) -> SocketId<I>,
        expected_result: Result<(), SetMulticastMembershipError>,
    ) {
        let mcast_addr = I::get_multicast_addr(3);
        let (result, ip_options) = try_join_leave_multicast(
            mcast_addr,
            interface_addr
                .map(MulticastInterfaceSelector::LocalAddress)
                .map(Into::into)
                .unwrap_or(MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute),
            |sync_ctx, non_sync_ctx, unbound| {
                SocketHandler::set_device(
                    sync_ctx,
                    non_sync_ctx,
                    unbound.into(),
                    Some(&bound_device),
                )
                .unwrap();
                make_socket(sync_ctx, non_sync_ctx, unbound)
            },
        );
        assert_eq!(result, expected_result);
        assert_eq!(
            ip_options,
            expected_result.map_or(HashMap::default(), |()| HashMap::from([(
                (bound_device, mcast_addr),
                nonzero!(1usize)
            )]))
        );
    }

    #[ip_test]
    fn test_multicast_membership_with_removed_device<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&FakeDeviceId),
        )
        .unwrap();

        set_device_removed(&mut sync_ctx, true);

        let group = I::get_multicast_addr(4);
        assert_eq!(
            SocketHandler::set_udp_multicast_membership(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound.into(),
                group,
                // Will use the socket's bound device.
                MulticastMembershipInterfaceSelector::AnyInterfaceWithRoute,
                true,
            ),
            Err(SetMulticastMembershipError::DeviceDoesNotExist),
        );

        // Should not have updated the device's multicast state.
        //
        // Note that even though we mock the device being removed above, its
        // state still exists in the fake IP socket context so we can inspect
        // it here.
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::default(),
        );
    }

    #[ip_test]
    fn test_remove_udp_unbound_leaves_multicast_groups<I: Ip + TestIpExt>() {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let group = I::get_multicast_addr(4);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            group,
            MulticastInterfaceSelector::LocalAddress(local_ip::<I>()).into(),
            true,
        )
        .expect("join group failed");

        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::from([((MultipleDevicesId::A, group), nonzero!(1usize))])
        );

        let _: SocketInfo<_, _> =
            SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, unbound.into());
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::default()
        );
    }

    #[ip_test]
    fn test_remove_udp_listener_leaves_multicast_groups<I: Ip + TestIpExt>() {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();
        let local_port = NonZeroU16::new(100).unwrap();

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let first_group = I::get_multicast_addr(4);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            first_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        let list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(local_port),
        )
        .expect("listen_udp failed");
        let second_group = I::get_multicast_addr(5);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            list.into(),
            second_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::from([
                ((MultipleDevicesId::A, first_group), nonzero!(1usize)),
                ((MultipleDevicesId::A, second_group), nonzero!(1usize))
            ])
        );

        let _: SocketInfo<_, _> =
            SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, list.into());
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::default()
        );
    }

    #[ip_test]
    fn test_remove_udp_connected_leaves_multicast_groups<I: Ip + TestIpExt>() {
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let first_group = I::get_multicast_addr(4);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            first_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(I::get_other_remote_ip_address(1)),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");

        let second_group = I::get_multicast_addr(5);
        SocketHandler::set_udp_multicast_membership(
            &mut sync_ctx,
            &mut non_sync_ctx,
            conn,
            second_group,
            MulticastInterfaceSelector::LocalAddress(local_ip).into(),
            true,
        )
        .expect("join group failed");

        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::from([
                ((MultipleDevicesId::A, first_group), nonzero!(1usize)),
                ((MultipleDevicesId::A, second_group), nonzero!(1usize))
            ])
        );

        let _: SocketInfo<_, _> = SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, conn);
        assert_eq!(
            AsRef::<FakeIpSocketCtx<_, _>>::as_ref(sync_ctx.inner.get_ref())
                .multicast_memberships(),
            HashMap::default()
        );
    }

    #[ip_test]
    #[should_panic(expected = "unbound")]
    fn test_listen_udp_removes_unbound<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = local_ip::<I>();
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        let _: ListenerId<_> = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");

        // Attempting to create a new listener from the same unbound ID should
        // panic since the unbound socket ID is now invalid.
        let _: ListenerId<_> = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            NonZeroU16::new(200),
        )
        .expect("listen_udp failed");
    }

    #[ip_test]
    fn test_get_conn_info<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>());
        let remote_ip = ZonedAddr::Unzoned(remote_ip::<I>());
        // Create a UDP connection with a specified local port and local IP.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");
        let conn = SocketHandler::<I, _>::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            remote_ip,
            NonZeroU16::new(200).unwrap(),
        )
        .expect("connect failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, conn);
        let info = assert_matches!(info, SocketInfo::Connected(info) => info);
        assert_eq!(info.local_ip, local_ip.map_zone(FakeWeakDeviceId));
        assert_eq!(info.local_port.get(), 100);
        assert_eq!(info.remote_ip, remote_ip.map_zone(FakeWeakDeviceId));
        assert_eq!(info.remote_port.get(), 200);
    }

    #[ip_test]
    fn test_get_listener_info<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let local_ip = ZonedAddr::Unzoned(local_ip::<I>());

        // Check getting info on specified listener.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(local_ip),
            NonZeroU16::new(100),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, list.into());
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip.unwrap(), local_ip.map_zone(FakeWeakDeviceId));
        assert_eq!(info.local_port.get(), 100);

        // Check getting info on wildcard listener.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let list = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            NonZeroU16::new(200),
        )
        .expect("listen_udp failed");
        let info = SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, list.into());
        let info = assert_matches!(info, SocketInfo::Listener(info) => info);
        assert_eq!(info.local_ip, None);
        assert_eq!(info.local_port.get(), 200);
    }

    #[ip_test]
    fn test_get_reuse_port<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, unbound.into()),
            false,
        );

        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, unbound, true);

        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, unbound.into()),
            true
        );

        let listen = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip::<I>())),
            None,
        )
        .expect("listen failed");
        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, listen.into()),
            true
        );
        let _: SocketInfo<_, _> =
            SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, listen.into());

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_udp_posix_reuse_port(&mut sync_ctx, &mut non_sync_ctx, unbound, true);
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip::<I>()),
            nonzero!(569u16),
        )
        .expect("connect failed");

        assert_eq!(
            SocketHandler::get_udp_posix_reuse_port(&mut sync_ctx, &non_sync_ctx, conn),
            true
        );
    }

    #[ip_test]
    fn test_get_bound_device_unbound<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, unbound.into()),
            None
        );

        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&FakeDeviceId),
        )
        .unwrap();
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, unbound.into()),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
    }

    #[ip_test]
    fn test_get_bound_device_listener<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&FakeDeviceId),
        )
        .unwrap();
        let listen = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip::<I>())),
            Some(NonZeroU16::new(100).unwrap()),
        )
        .expect("failed to listen");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, listen.into()),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, listen.into(), None)
            .expect("failed to set device");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, listen.into()),
            None
        );
    }

    #[ip_test]
    fn test_get_bound_device_connected<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&FakeDeviceId),
        )
        .unwrap();
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            ZonedAddr::Unzoned(remote_ip::<I>()),
            NonZeroU16::new(200).unwrap(),
        )
        .expect("failed to connect");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, conn),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, conn, None)
            .expect("failed to set device");
        assert_eq!(SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, conn), None);
    }

    #[ip_test]
    fn test_listen_udp_forwards_errors<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let remote_ip = remote_ip::<I>();

        // Check listening to a non-local IP fails.
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listen_err = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(remote_ip)),
            NonZeroU16::new(100),
        )
        .expect_err("listen_udp unexpectedly succeeded");
        assert_eq!(listen_err, LocalAddressError::CannotBindToAddress);

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let _ = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            NonZeroU16::new(200),
        )
        .expect("listen_udp failed");
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listen_err = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            None,
            NonZeroU16::new(200),
        )
        .expect_err("listen_udp unexpectedly succeeded");
        assert_eq!(listen_err, LocalAddressError::AddressInUse);
    }

    const IPV6_LINK_LOCAL_ADDR: Ipv6Addr = net_ip_v6!("fe80::1234");
    #[test_case(IPV6_LINK_LOCAL_ADDR, IPV6_LINK_LOCAL_ADDR; "unicast")]
    #[test_case(IPV6_LINK_LOCAL_ADDR, MulticastAddr::new(net_ip_v6!("ff02::1234")).unwrap().get(); "multicast")]
    fn test_listen_udp_ipv6_link_local_requires_zone(
        interface_addr: Ipv6Addr,
        bind_addr: Ipv6Addr,
    ) {
        type I = Ipv6;
        let interface_addr = LinkLocalAddr::new(interface_addr).unwrap().into_specified();

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(
                vec![interface_addr],
                vec![remote_ip::<I>()],
            ));

        let bind_addr = LinkLocalAddr::new(bind_addr).unwrap().into_specified();
        assert!(bind_addr.scope().can_have_zone());

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let result = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(bind_addr)),
            NonZeroU16::new(200),
        );
        assert_eq!(
            result,
            Err(LocalAddressError::Zone(ZonedAddressError::RequiredZoneNotProvided))
        );
    }

    #[test_case(MultipleDevicesId::A, Ok(()); "matching")]
    #[test_case(MultipleDevicesId::B, Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch)); "not matching")]
    fn test_listen_udp_ipv6_link_local_with_bound_device_set(
        zone_id: MultipleDevicesId,
        expected_result: Result<(), LocalAddressError>,
    ) {
        type I = Ipv6;
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(ll_addr.scope().can_have_zone());

        let remote_ips = vec![remote_ip::<I>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<I>())].map(
                        |(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        },
                    ),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&MultipleDevicesId::A),
        )
        .unwrap();

        let result = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), zone_id).unwrap())),
            NonZeroU16::new(200),
        )
        .map(|_: ListenerId<I>| ());
        assert_eq!(result, expected_result);
    }

    #[test_case(MultipleDevicesId::A, Ok(()); "matching")]
    #[test_case(MultipleDevicesId::B, Err(LocalAddressError::AddressMismatch); "not matching")]
    fn test_listen_udp_ipv6_link_local_with_zone_requires_addr_assigned_to_device(
        zone_id: MultipleDevicesId,
        expected_result: Result<(), LocalAddressError>,
    ) {
        type I = Ipv6;
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(ll_addr.scope().can_have_zone());

        let remote_ips = vec![remote_ip::<I>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<I>())].map(
                        |(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        },
                    ),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let result = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), zone_id).unwrap())),
            NonZeroU16::new(200),
        )
        .map(|_: ListenerId<I>| ());
        assert_eq!(result, expected_result);
    }

    #[test_case(None, Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch)); "clear device")]
    #[test_case(Some(MultipleDevicesId::A), Ok(()); "set same device")]
    #[test_case(Some(MultipleDevicesId::B),
                Err(LocalAddressError::Zone(ZonedAddressError::DeviceZoneMismatch)); "change device")]
    fn test_listen_udp_ipv6_listen_link_local_update_bound_device(
        new_device: Option<MultipleDevicesId>,
        expected_result: Result<(), LocalAddressError>,
    ) {
        type I = Ipv6;
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(ll_addr.scope().can_have_zone());

        let remote_ips = vec![remote_ip::<I>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<I>())].map(
                        |(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        },
                    ),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), MultipleDevicesId::A).unwrap())),
            NonZeroU16::new(200),
        )
        .expect("listen failed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, listener.into()),
            Some(FakeWeakDeviceId(MultipleDevicesId::A))
        );

        assert_eq!(
            SocketHandler::set_device(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                new_device.as_ref()
            ),
            expected_result.map_err(SocketError::Local),
        );
    }

    #[test_case(None; "bind all IPs")]
    #[test_case(Some(ZonedAddr::Unzoned(local_ip::<Ipv6>())); "bind unzoned")]
    #[test_case(Some(ZonedAddr::Zoned(AddrAndZone::new(net_ip_v6!("fe80::1"),
        MultipleDevicesId::A).unwrap())); "bind with same zone")]
    fn test_udp_ipv6_connect_with_unzoned(
        bound_addr: Option<ZonedAddr<Ipv6Addr, MultipleDevicesId>>,
    ) {
        let remote_ips = vec![remote_ip::<Ipv6>()];

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new([
                    FakeDeviceConfig {
                        device: MultipleDevicesId::A,
                        local_ips: vec![
                            local_ip::<Ipv6>(),
                            SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap(),
                        ],
                        remote_ips: remote_ips.clone(),
                    },
                    FakeDeviceConfig {
                        device: MultipleDevicesId::B,
                        local_ips: vec![SpecifiedAddr::new(net_ip_v6!("fe80::2")).unwrap()],
                        remote_ips: remote_ips.clone(),
                    },
                ])),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        let listener = SocketHandler::<Ipv6, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            bound_addr,
            Some(LOCAL_PORT),
        )
        .unwrap();

        assert_matches!(
            SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                ZonedAddr::Unzoned(remote_ip::<Ipv6>()),
                REMOTE_PORT,
            ),
            Ok(_)
        );
    }

    #[test]
    fn test_udp_ipv6_connect_zoned_get_info() {
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let remote_ips = vec![remote_ip::<Ipv6>()];
        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<Ipv6>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [(MultipleDevicesId::A, ll_addr), (MultipleDevicesId::B, local_ip::<Ipv6>())]
                        .map(|(device, local_ip)| FakeDeviceConfig {
                            device,
                            local_ips: vec![local_ip],
                            remote_ips: remote_ips.clone(),
                        }),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&MultipleDevicesId::A),
        )
        .unwrap();

        let zoned_local_addr =
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), MultipleDevicesId::A).unwrap());
        let listener = SocketHandler::<Ipv6, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(zoned_local_addr),
            Some(LOCAL_PORT),
        )
        .unwrap();

        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Unzoned(remote_ip::<Ipv6>()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_info(&mut sync_ctx, &mut non_sync_ctx, conn),
            SocketInfo::Connected(ConnInfo {
                local_ip: zoned_local_addr.map_zone(FakeWeakDeviceId),
                local_port: LOCAL_PORT,
                remote_ip: ZonedAddr::Unzoned(remote_ip::<Ipv6>()),
                remote_port: REMOTE_PORT,
            })
        );
    }

    #[test_case(ZonedAddr::Zoned(AddrAndZone::new(net_ip_v6!("fe80::2"),
        MultipleDevicesId::B).unwrap()),
        Err(ConnectError::Zone(ZonedAddressError::DeviceZoneMismatch));
        "connect to different zone")]
    #[test_case(ZonedAddr::Unzoned(SpecifiedAddr::new(net_ip_v6!("fe80::3")).unwrap()),
        Ok(FakeWeakDeviceId(MultipleDevicesId::A)); "connect implicit zone")]
    fn test_udp_ipv6_bind_zoned(
        remote_addr: ZonedAddr<Ipv6Addr, MultipleDevicesId>,
        expected: Result<FakeWeakDeviceId<MultipleDevicesId>, ConnectError>,
    ) {
        let remote_ips = vec![SpecifiedAddr::new(net_ip_v6!("fe80::3")).unwrap()];

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new([
                    FakeDeviceConfig {
                        device: MultipleDevicesId::A,
                        local_ips: vec![SpecifiedAddr::new(net_ip_v6!("fe80::1")).unwrap()],
                        remote_ips: remote_ips.clone(),
                    },
                    FakeDeviceConfig {
                        device: MultipleDevicesId::B,
                        local_ips: vec![SpecifiedAddr::new(net_ip_v6!("fe80::2")).unwrap()],
                        remote_ips: remote_ips.clone(),
                    },
                ])),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        let listener = SocketHandler::<Ipv6, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Zoned(
                AddrAndZone::new(net_ip_v6!("fe80::1"), MultipleDevicesId::A).unwrap(),
            )),
            Some(LOCAL_PORT),
        )
        .unwrap();

        let result = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            remote_addr,
            REMOTE_PORT,
        )
        .map(|id| SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, id).unwrap());
        assert_eq!(result, expected);
    }

    #[ip_test]
    fn test_listen_udp_loopback_no_zone_is_required<I: Ip + TestIpExt>() {
        let loopback_addr = I::LOOPBACK_ADDRESS;
        let remote_ips = vec![remote_ip::<I>()];

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<I>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [
                        (MultipleDevicesId::A, loopback_addr),
                        (MultipleDevicesId::B, local_ip::<I>()),
                    ]
                    .map(|(device, local_ip)| FakeDeviceConfig {
                        device,
                        local_ips: vec![local_ip],
                        remote_ips: remote_ips.clone(),
                    }),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(&MultipleDevicesId::A),
        )
        .unwrap();

        let result = SocketHandler::<I, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(loopback_addr)),
            NonZeroU16::new(200),
        );
        assert_matches!(result, Ok(_));
    }

    #[test_case(None, true, Ok(()); "connected success")]
    #[test_case(None, false, Ok(()); "listening success")]
    #[test_case(Some(MultipleDevicesId::A), true, Ok(()); "conn bind same device")]
    #[test_case(Some(MultipleDevicesId::A), false, Ok(()); "listen bind same device")]
    #[test_case(
        Some(MultipleDevicesId::B),
        true,
        Err(SendToError::Zone(ZonedAddressError::DeviceZoneMismatch));
        "conn bind different device")]
    #[test_case(
        Some(MultipleDevicesId::B),
        false,
        Err(SendToError::Zone(ZonedAddressError::DeviceZoneMismatch));
        "listen bind different device")]
    fn test_udp_ipv6_send_to_zoned(
        bind_device: Option<MultipleDevicesId>,
        connect: bool,
        expected: Result<(), SendToError>,
    ) {
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));
        let conn_remote_ip = Ipv6::get_other_remote_ip_address(1);

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<Ipv6>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [
                        (MultipleDevicesId::A, Ipv6::get_other_ip_address(1)),
                        (MultipleDevicesId::B, Ipv6::get_other_ip_address(2)),
                    ]
                    .map(|(device, local_ip)| FakeDeviceConfig {
                        device,
                        local_ips: vec![local_ip],
                        remote_ips: vec![ll_addr, conn_remote_ip],
                    }),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        if let Some(device) = bind_device {
            SocketHandler::set_device(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound.into(),
                Some(&device),
            )
            .unwrap();
        }

        let send_to_remote_addr =
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), MultipleDevicesId::A).unwrap());
        let result = if connect {
            let id = SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound.into(),
                ZonedAddr::Unzoned(conn_remote_ip),
                REMOTE_PORT,
            )
            .expect("connect should succeed");
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                id,
                send_to_remote_addr,
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        } else {
            let id = SocketHandler::listen_udp(
                &mut sync_ctx,
                &mut non_sync_ctx,
                unbound,
                None,
                Some(LOCAL_PORT),
            )
            .expect("listen should succeed");
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                id.into(),
                send_to_remote_addr,
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        };

        assert_eq!(
            result
                .map(|new_id| assert_matches!(new_id, None))
                .map_err(|(_buf, err)| assert_matches!(err, Either::Right((None, e)) => e)),
            expected
        );
    }

    #[test_case(true; "connected")]
    #[test_case(false; "listening")]
    fn test_udp_ipv6_bound_zoned_send_to_zoned(connect: bool) {
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::5678")).unwrap().into_specified();
        let device_a_local_ip = net_ip_v6!("fe80::1111");
        let conn_remote_ip = Ipv6::get_other_remote_ip_address(1);

        let UdpMultipleDevicesCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpMultipleDevicesCtx::with_sync_ctx(UdpMultipleDevicesSyncCtx::<Ipv6>::with_state(
                FakeBufferIpSocketCtx::with_ctx(FakeIpSocketCtx::new(
                    [
                        (MultipleDevicesId::A, device_a_local_ip),
                        (MultipleDevicesId::B, net_ip_v6!("fe80::2222")),
                    ]
                    .map(|(device, local_ip)| FakeDeviceConfig {
                        device,
                        local_ips: vec![LinkLocalAddr::new(local_ip).unwrap().into_specified()],
                        remote_ips: vec![ll_addr, conn_remote_ip],
                    }),
                )),
            ));

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Zoned(
                AddrAndZone::new(device_a_local_ip, MultipleDevicesId::A).unwrap(),
            )),
            Some(LOCAL_PORT),
        )
        .expect("listen should succeed");

        // Use a remote address on device B, while the socket is listening on
        // device A. This should cause a failure when sending.
        let send_to_remote_addr =
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), MultipleDevicesId::B).unwrap());

        let result = if connect {
            let id = SocketHandler::connect(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                ZonedAddr::Unzoned(conn_remote_ip),
                REMOTE_PORT,
            )
            .expect("connect should succeed");
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                id,
                send_to_remote_addr,
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        } else {
            BufferSocketHandler::send_to(
                &mut sync_ctx,
                &mut non_sync_ctx,
                listener.into(),
                send_to_remote_addr,
                REMOTE_PORT,
                Buf::new(Vec::new(), ..),
            )
        };

        assert_matches!(
            result,
            Err((
                _,
                Either::Right((None, SendToError::Zone(ZonedAddressError::DeviceZoneMismatch)))
            ))
        );
    }

    #[test_case(None; "removes implicit")]
    #[test_case(Some(FakeDeviceId); "preserves implicit")]
    fn test_connect_disconnect_affects_bound_device(bind_device: Option<FakeDeviceId>) {
        // If a socket is bound to an unzoned address, whether or not it has a
        // bound device should be restored after `connect` then `disconnect`.
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let local_ip = local_ip::<Ipv6>();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::<Ipv6>::with_local_remote_ip_addrs(vec![local_ip], vec![ll_addr]),
        );

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            bind_device.as_ref(),
        )
        .unwrap();

        let listener = SocketHandler::<Ipv6, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(LOCAL_PORT),
        )
        .unwrap();
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), FakeDeviceId).unwrap()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, conn),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        let listener = SocketHandler::disconnect_connected(&mut sync_ctx, &mut non_sync_ctx, conn)
            .expect("was connected");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, listener),
            bind_device.map(FakeWeakDeviceId),
        );
    }

    #[test]
    fn test_bind_zoned_addr_connect_disconnect() {
        // If a socket is bound to a zoned address, the address's device should
        // be retained after `connect` then `disconnect`.
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let remote_ip = remote_ip::<Ipv6>();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::<Ipv6>::with_local_remote_ip_addrs(
                vec![ll_addr],
                vec![remote_ip],
            ),
        );

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::<Ipv6, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), FakeDeviceId).unwrap())),
            Some(LOCAL_PORT),
        )
        .unwrap();
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Unzoned(remote_ip),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, conn),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        let listener = SocketHandler::disconnect_connected(&mut sync_ctx, &mut non_sync_ctx, conn)
            .expect("was connected");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, listener),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
    }

    #[test]
    fn test_bind_device_after_connect_persists_after_disconnect() {
        // If a socket is bound to an unzoned address, connected to a zoned address, and then has
        // its device set, the device should be *retained* after `disconnect`.
        let ll_addr = LinkLocalAddr::new(net_ip_v6!("fe80::1234")).unwrap().into_specified();
        assert!(socket::must_have_zone(&ll_addr));

        let local_ip = local_ip::<Ipv6>();
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } = UdpFakeDeviceCtx::with_sync_ctx(
            UdpFakeDeviceSyncCtx::<Ipv6>::with_local_remote_ip_addrs(vec![local_ip], vec![ll_addr]),
        );

        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let listener = SocketHandler::<Ipv6, _>::listen_udp(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound,
            Some(ZonedAddr::Unzoned(local_ip)),
            Some(LOCAL_PORT),
        )
        .unwrap();
        let conn = SocketHandler::connect(
            &mut sync_ctx,
            &mut non_sync_ctx,
            listener.into(),
            ZonedAddr::Zoned(AddrAndZone::new(ll_addr.get(), FakeDeviceId).unwrap()),
            REMOTE_PORT,
        )
        .expect("connect should succeed");

        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, conn),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );

        // This is a no-op functionally since the socket is already bound to the
        // device but it implies that we shouldn't unbind the device on
        // disconnect.
        SocketHandler::set_device(&mut sync_ctx, &mut non_sync_ctx, conn, Some(&FakeDeviceId))
            .expect("binding same device should succeed");

        let listener = SocketHandler::disconnect_connected(&mut sync_ctx, &mut non_sync_ctx, conn)
            .expect("was connected");
        assert_eq!(
            SocketHandler::get_udp_bound_device(&mut sync_ctx, &non_sync_ctx, listener),
            Some(FakeWeakDeviceId(FakeDeviceId))
        );
    }

    #[ip_test]
    fn test_remove_udp_unbound<I: Ip + TestIpExt>() {
        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::default());
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);
        let _: SocketInfo<_, _> =
            SocketHandler::remove_udp(&mut sync_ctx, &mut non_sync_ctx, unbound.into());

        let Sockets {
            sockets: DatagramSockets { bound_state: _, bound: _, unbound: unbound_sockets },
            lazy_port_alloc: _,
        } = &sync_ctx.outer.sockets;
        assert_matches!(unbound_sockets.get(unbound.into()), None)
    }

    #[ip_test]
    fn test_hop_limits_used_for_sending_packets<I: Ip + TestIpExt>() {
        let some_multicast_addr: MulticastAddr<I::Addr> = I::map_ip(
            (),
            |()| Ipv4::ALL_SYSTEMS_MULTICAST_ADDRESS,
            |()| MulticastAddr::new(net_ip_v6!("ff0e::1")).unwrap(),
        );

        let UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx } =
            UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::<I>::with_local_remote_ip_addrs(
                vec![local_ip::<I>()],
                vec![remote_ip::<I>(), some_multicast_addr.into_specified()],
            ));
        let unbound = SocketHandler::create_udp_unbound(&mut sync_ctx);

        const UNICAST_HOPS: NonZeroU8 = nonzero!(23u8);
        const MULTICAST_HOPS: NonZeroU8 = nonzero!(98u8);
        SocketHandler::set_udp_unicast_hop_limit(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(UNICAST_HOPS),
        );
        SocketHandler::set_udp_multicast_hop_limit(
            &mut sync_ctx,
            &mut non_sync_ctx,
            unbound.into(),
            Some(MULTICAST_HOPS),
        );

        let listener =
            SocketHandler::listen_udp(&mut sync_ctx, &mut non_sync_ctx, unbound, None, None)
                .expect("listen failed");

        let mut send_and_get_ttl = |remote_ip| {
            assert_matches!(
                BufferSocketHandler::send_to(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    listener.into(),
                    ZonedAddr::Unzoned(remote_ip),
                    nonzero!(9090u16),
                    Buf::new(vec![], ..),
                )
                .expect("send failed"),
                None
            );

            let (
                SendIpPacketMeta {
                    device: _,
                    src_ip: _,
                    dst_ip,
                    next_hop: _,
                    proto: _,
                    ttl,
                    mtu: _,
                },
                _body,
            ) = sync_ctx.inner.frames().last().unwrap();
            assert_eq!(*dst_ip, remote_ip);
            *ttl
        };

        assert_eq!(send_and_get_ttl(some_multicast_addr.into_specified()), Some(MULTICAST_HOPS));
        assert_eq!(send_and_get_ttl(remote_ip::<I>()), Some(UNICAST_HOPS));
    }

    #[test]
    fn test_icmp_error() {
        // Create a context with:
        // - A wildcard listener on port 1
        // - A listener on the local IP and port 2
        // - A connection from the local IP to the remote IP on local port 2 and
        //   remote port 3
        fn initialize_context<I: TestIpExt>(
        ) -> (UdpFakeDeviceCtx<I>, ListenerId<I>, ListenerId<I>, SocketId<I>) {
            let mut ctx = UdpFakeDeviceCtx::with_sync_ctx(UdpFakeDeviceSyncCtx::default());
            let UdpFakeDeviceCtx { sync_ctx, non_sync_ctx } = &mut ctx;
            let listener1 = {
                let unbound = SocketHandler::create_udp_unbound(sync_ctx);
                SocketHandler::listen_udp(
                    sync_ctx,
                    non_sync_ctx,
                    unbound,
                    None,
                    Some(NonZeroU16::new(1).unwrap()),
                )
                .unwrap()
            };

            let listener2 = {
                let unbound = SocketHandler::create_udp_unbound(sync_ctx);
                SocketHandler::listen_udp(
                    sync_ctx,
                    non_sync_ctx,
                    unbound,
                    Some(ZonedAddr::Unzoned(local_ip::<I>())),
                    Some(NonZeroU16::new(2).unwrap()),
                )
                .unwrap()
            };
            let conn = {
                let unbound = SocketHandler::create_udp_unbound(sync_ctx);
                let listener = SocketHandler::listen_udp(
                    sync_ctx,
                    non_sync_ctx,
                    unbound,
                    Some(ZonedAddr::Unzoned(local_ip::<I>())),
                    Some(NonZeroU16::new(3).unwrap()),
                )
                .unwrap();
                SocketHandler::connect(
                    sync_ctx,
                    non_sync_ctx,
                    listener.into(),
                    ZonedAddr::Unzoned(remote_ip::<I>()),
                    NonZeroU16::new(4).unwrap(),
                )
                .unwrap()
            };
            (ctx, listener1, listener2, conn)
        }

        // Serialize a UDP-in-IP packet with the given values, and then receive
        // an ICMP error message with that packet as the original packet.
        fn receive_icmp_error<
            I: TestIpExt,
            F: Fn(&mut UdpFakeDeviceSyncCtx<I>, &mut UdpFakeDeviceNonSyncCtx<I>, &[u8], I::ErrorCode),
        >(
            sync_ctx: &mut UdpFakeDeviceSyncCtx<I>,
            ctx: &mut UdpFakeDeviceNonSyncCtx<I>,
            src_ip: I::Addr,
            dst_ip: I::Addr,
            src_port: u16,
            dst_port: u16,
            err: I::ErrorCode,
            f: F,
        ) where
            I::PacketBuilder: core::fmt::Debug,
        {
            let packet = (&[0u8][..])
                .into_serializer()
                .encapsulate(UdpPacketBuilder::new(
                    src_ip,
                    dst_ip,
                    NonZeroU16::new(src_port),
                    NonZeroU16::new(dst_port).unwrap(),
                ))
                .encapsulate(I::PacketBuilder::new(src_ip, dst_ip, 64, IpProto::Udp.into()))
                .serialize_vec_outer()
                .unwrap();
            f(sync_ctx, ctx, packet.as_ref(), err);
        }

        fn test<
            I: TestIpExt + PartialEq,
            F: Copy
                + Fn(
                    &mut UdpFakeDeviceSyncCtx<I>,
                    &mut UdpFakeDeviceNonSyncCtx<I>,
                    &[u8],
                    I::ErrorCode,
                ),
        >(
            err: I::ErrorCode,
            f: F,
            other_remote_ip: I::Addr,
        ) where
            I::PacketBuilder: core::fmt::Debug,
            I::ErrorCode: Copy + core::fmt::Debug + PartialEq,
        {
            let (UdpFakeDeviceCtx { mut sync_ctx, mut non_sync_ctx }, listener1, listener2, conn) =
                initialize_context::<I>();

            let src_ip = local_ip::<I>();
            let dst_ip = remote_ip::<I>();

            // Test that we receive an error for the connection.
            receive_icmp_error(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                3,
                4,
                err,
                f,
            );
            assert_eq!(non_sync_ctx.state().icmp_errors.as_slice(), [IcmpError { id: conn, err }]);

            // Test that we receive an error for the listener.
            receive_icmp_error(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                2,
                4,
                err,
                f,
            );
            assert_eq!(
                &non_sync_ctx.state().icmp_errors.as_slice()[1..],
                [IcmpError { id: listener2.into(), err }]
            );

            // Test that we receive an error for the wildcard listener.
            receive_icmp_error(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                1,
                4,
                err,
                f,
            );
            assert_eq!(
                &non_sync_ctx.state().icmp_errors.as_slice()[2..],
                [IcmpError { id: listener1.into(), err }]
            );

            // Test that we receive an error for the wildcard listener even if
            // the original packet was sent to a different remote IP/port.
            receive_icmp_error(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                other_remote_ip,
                1,
                5,
                err,
                f,
            );
            assert_eq!(
                &non_sync_ctx.state().icmp_errors.as_slice()[3..],
                [IcmpError { id: listener1.into(), err }]
            );

            // Test that an error that doesn't correspond to any connection or
            // listener isn't received.
            receive_icmp_error(
                &mut sync_ctx,
                &mut non_sync_ctx,
                src_ip.get(),
                dst_ip.get(),
                3,
                5,
                err,
                f,
            );
            assert_eq!(non_sync_ctx.state().icmp_errors.len(), 4);
        }

        test(
            Icmpv4ErrorCode::DestUnreachable(Icmpv4DestUnreachableCode::DestNetworkUnreachable),
            |sync_ctx: &mut UdpFakeDeviceSyncCtx<Ipv4>,
             ctx: &mut UdpFakeDeviceNonSyncCtx<Ipv4>,
             mut packet,
             error_code| {
                let packet = packet.parse::<Ipv4PacketRaw<_>>().unwrap();
                let device = FakeDeviceId;
                let src_ip = SpecifiedAddr::new(packet.src_ip());
                let dst_ip = SpecifiedAddr::new(packet.dst_ip()).unwrap();
                let body = packet.body().into_inner();
                <UdpIpTransportContext as IpTransportContext<Ipv4, _, _>>::receive_icmp_error(
                    sync_ctx, ctx, &device, src_ip, dst_ip, body, error_code,
                )
            },
            Ipv4Addr::new([1, 2, 3, 4]),
        );

        test(
            Icmpv6ErrorCode::DestUnreachable(Icmpv6DestUnreachableCode::NoRoute),
            |sync_ctx: &mut UdpFakeDeviceSyncCtx<Ipv6>,
             ctx: &mut UdpFakeDeviceNonSyncCtx<Ipv6>,
             mut packet,
             error_code| {
                let packet = packet.parse::<Ipv6PacketRaw<_>>().unwrap();
                let device = FakeDeviceId;
                let src_ip = SpecifiedAddr::new(packet.src_ip());
                let dst_ip = SpecifiedAddr::new(packet.dst_ip()).unwrap();
                let body = packet.body().unwrap().into_inner();
                <UdpIpTransportContext as IpTransportContext<Ipv6, _, _>>::receive_icmp_error(
                    sync_ctx, ctx, &device, src_ip, dst_ip, body, error_code,
                )
            },
            Ipv6Addr::from_bytes([1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8]),
        );
    }

    type FakeBoundSocketMap<I> = BoundSocketMap<
        IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>,
        Udp<I, FakeWeakDeviceId<FakeDeviceId>>,
    >;

    fn listen<I: Ip + IpExt>(
        ip: I::Addr,
        port: u16,
    ) -> AddrVec<IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>> {
        let addr = SpecifiedAddr::new(ip);
        let port = NonZeroU16::new(port).expect("port must be nonzero");
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr, identifier: port },
            device: None,
        })
    }

    fn listen_device<I: Ip + IpExt>(
        ip: I::Addr,
        port: u16,
        device: FakeWeakDeviceId<FakeDeviceId>,
    ) -> AddrVec<IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>> {
        let addr = SpecifiedAddr::new(ip);
        let port = NonZeroU16::new(port).expect("port must be nonzero");
        AddrVec::Listen(ListenerAddr {
            ip: ListenerIpAddr { addr, identifier: port },
            device: Some(device),
        })
    }

    fn conn<I: Ip + IpExt>(
        local_ip: I::Addr,
        local_port: u16,
        remote_ip: I::Addr,
        remote_port: u16,
    ) -> AddrVec<IpPortSpec<I, FakeWeakDeviceId<FakeDeviceId>>> {
        let local_ip = SpecifiedAddr::new(local_ip).expect("addr must be specified");
        let local_port = NonZeroU16::new(local_port).expect("port must be nonzero");
        let remote_ip = SpecifiedAddr::new(remote_ip).expect("addr must be specified");
        let remote_port = NonZeroU16::new(remote_port).expect("port must be nonzero");
        AddrVec::Conn(ConnAddr {
            ip: ConnIpAddr { local: (local_ip, local_port), remote: (remote_ip, remote_port) },
            device: None,
        })
    }

    #[test_case([
        (listen(ip_v4!("0.0.0.0"), 1), Sharing::Exclusive),
        (listen(ip_v4!("0.0.0.0"), 2), Sharing::Exclusive)],
            Ok(()); "listen_any_ip_different_port")]
    #[test_case([
        (listen(ip_v4!("0.0.0.0"), 1), Sharing::Exclusive),
        (listen(ip_v4!("0.0.0.0"), 1), Sharing::Exclusive)],
            Err(InsertError::Exists); "any_ip_same_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive)],
            Err(InsertError::Exists); "listen_same_specific_ip")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort)],
            Ok(()); "listen_same_specific_ip_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort)],
            Err(InsertError::Exists); "listen_same_specific_ip_exclusive_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive)],
            Err(InsertError::Exists); "listen_same_specific_ip_reuse_port_exclusive")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::ReusePort)],
            Ok(()); "conn_shadows_listener_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::ShadowAddrExists); "conn_shadows_listener_exclusive")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::ReusePort)],
            Err(InsertError::ShadowAddrExists); "conn_shadows_listener_exclusive_reuse_port")]
    #[test_case([
        (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::ShadowAddrExists); "conn_shadows_listener_reuse_port_exclusive")]
    #[test_case([
        (listen_device(ip_v4!("1.1.1.1"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "conn_indirect_conflict_specific_listener")]
    #[test_case([
        (listen_device(ip_v4!("0.0.0.0"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive),
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "conn_indirect_conflict_any_listener")]
    #[test_case([
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive),
        (listen_device(ip_v4!("1.1.1.1"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "specific_listener_indirect_conflict_conn")]
    #[test_case([
        (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 2), Sharing::Exclusive),
        (listen_device(ip_v4!("0.0.0.0"), 1, FakeWeakDeviceId(FakeDeviceId)), Sharing::Exclusive)],
            Err(InsertError::IndirectConflict); "any_listener_indirect_conflict_conn")]
    fn bind_sequence<
        C: IntoIterator<Item = (AddrVec<IpPortSpec<Ipv4, FakeWeakDeviceId<FakeDeviceId>>>, Sharing)>,
    >(
        spec: C,
        expected: Result<(), InsertError>,
    ) {
        let mut map = FakeBoundSocketMap::<Ipv4>::default();
        let mut spec = spec.into_iter().enumerate().peekable();
        let mut try_insert = |(index, (addr, options))| match addr {
            AddrVec::Conn(c) => map
                .conns_mut()
                .try_insert(c, options, |_addr, _sharing| ConnId::from(index))
                .map(|_| ())
                .map_err(|(e, _)| e),
            AddrVec::Listen(l) => map
                .listeners_mut()
                .try_insert(l, options, |_addr, _sharing| ListenerId::from(index))
                .map(|_| ())
                .map_err(|(e, _)| e),
        };
        let last = loop {
            let one_spec = spec.next().expect("empty list of test cases");
            if spec.peek().is_none() {
                break one_spec;
            } else {
                try_insert(one_spec).expect("intermediate bind failed")
            }
        };

        let result = try_insert(last);
        assert_eq!(result, expected);
    }

    #[test_case([
            (listen(ip_v4!("1.1.1.1"), 1), Sharing::Exclusive),
            (listen(ip_v4!("2.2.2.2"), 2), Sharing::Exclusive),
        ]; "distinct")]
    #[test_case([
            (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
            (listen(ip_v4!("1.1.1.1"), 1), Sharing::ReusePort),
        ]; "listen_reuse_port")]
    #[test_case([
            (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 3), Sharing::ReusePort),
            (conn(ip_v4!("1.1.1.1"), 1, ip_v4!("2.2.2.2"), 3), Sharing::ReusePort),
        ]; "conn_reuse_port")]
    fn remove_sequence<I>(spec: I)
    where
        I: IntoIterator<
            Item = (AddrVec<IpPortSpec<Ipv4, FakeWeakDeviceId<FakeDeviceId>>>, Sharing),
        >,
        I::IntoIter: ExactSizeIterator,
    {
        enum Socket<A: IpAddress, D, LI, RI> {
            Listener(ListenerId<A::Version>, ListenerAddr<A, D, LI>),
            Conn(ConnId<A::Version>, ConnAddr<A, D, LI, RI>),
        }
        let spec = spec.into_iter();
        let spec_len = spec.len();
        for spec in spec.permutations(spec_len) {
            let mut map = FakeBoundSocketMap::<Ipv4>::default();
            let sockets = spec
                .into_iter()
                .enumerate()
                .map(|(socket_index, (addr, options))| match addr {
                    AddrVec::Conn(c) => map
                        .conns_mut()
                        .try_insert(c, options, |_addr, _sharing| ConnId::from(socket_index))
                        .map(|entry| Socket::Conn(entry.id().clone(), entry.get_addr().clone()))
                        .expect("insert_failed"),
                    AddrVec::Listen(l) => map
                        .listeners_mut()
                        .try_insert(l, options, |_addr, _sharing| ListenerId::from(socket_index))
                        .map(|entry| Socket::Listener(entry.id().clone(), entry.get_addr().clone()))
                        .expect("insert_failed"),
                })
                .collect::<Vec<_>>();

            for socket in sockets {
                match socket {
                    Socket::Listener(l, addr) => {
                        assert_matches!(map.listeners_mut().remove(&l, &addr), Ok(()));
                    }
                    Socket::Conn(c, addr) => {
                        assert_matches!(map.conns_mut().remove(&c, &addr), Ok(()));
                    }
                }
            }
        }
    }
}
