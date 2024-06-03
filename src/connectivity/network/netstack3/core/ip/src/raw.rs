// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Facilities backing raw IP sockets.

use alloc::collections::{btree_map::Entry, BTreeMap, HashMap};
use core::fmt::Debug;
use derivative::Derivative;
use net_types::{
    ip::{Ip, IpVersionMarker},
    SpecifiedAddr, ZonedAddr,
};
use netstack3_base::{
    socket::{DualStackIpExt, DualStackRemoteIp, SocketZonedAddrExt as _},
    sync::{PrimaryRc, StrongRc, WeakRc},
    AnyDevice, ContextPair, DeviceIdContext, ReferenceNotifiers, ReferenceNotifiersExt as _,
    RemoveResourceResultWithContext, StrongDeviceIdentifier, WeakDeviceIdentifier,
    ZonedAddressError,
};
use netstack3_filter::RawIpBody;
use packet::BufferMut;
use packet_formats::ip::IpPacket;
use tracing::debug;
use zerocopy::ByteSlice;

use crate::{
    internal::{
        base::IpExt,
        raw::{
            protocol::RawIpSocketProtocol,
            state::{RawIpSocketLockedState, RawIpSocketState},
        },
    },
    socket::{DefaultSendOptions, IpSockCreateAndSendError, IpSocketHandler},
};

pub(crate) mod protocol;
pub(crate) mod state;

/// Types provided by bindings used in the raw IP socket implementation.
pub trait RawIpSocketsBindingsTypes {
    /// The bindings state (opaque to core) associated with a socket.
    type RawIpSocketState<I: Ip>: Send + Sync + Debug;
}

/// Functionality provided by bindings used in the raw IP socket implementation.
pub trait RawIpSocketsBindingsContext<I: IpExt, D: StrongDeviceIdentifier>:
    RawIpSocketsBindingsTypes + Sized
{
    /// Called for each received IP packet that matches the provided socket.
    fn receive_packet<B: ByteSlice>(
        &self,
        socket: &RawIpSocketId<I, D::Weak, Self>,
        packet: &I::Packet<B>,
        device: &D,
    );
}

/// The raw IP socket API.
pub struct RawIpSocketApi<I: Ip, C> {
    ctx: C,
    _ip_mark: IpVersionMarker<I>,
}

impl<I: Ip, C> RawIpSocketApi<I, C> {
    /// Constructs a new RAW IP socket API.
    pub fn new(ctx: C) -> Self {
        Self { ctx, _ip_mark: IpVersionMarker::new() }
    }
}

impl<I: IpExt + DualStackIpExt, C> RawIpSocketApi<I, C>
where
    C: ContextPair,
    C::BindingsContext: RawIpSocketsBindingsTypes + ReferenceNotifiers + 'static,
    C::CoreContext: RawIpSocketMapContext<I, C::BindingsContext>
        + RawIpSocketStateContext<I, C::BindingsContext>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self { ctx, _ip_mark } = self;
        ctx.core_ctx()
    }

    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self { ctx, _ip_mark } = self;
        ctx.contexts()
    }

    /// Creates a raw IP socket for the given protocol.
    pub fn create(
        &mut self,
        protocol: RawIpSocketProtocol<I>,
        external_state: <C::BindingsContext as RawIpSocketsBindingsTypes>::RawIpSocketState<I>,
    ) -> RawIpApiSocketId<I, C> {
        let socket =
            PrimaryRawIpSocketId(PrimaryRc::new(RawIpSocketState::new(protocol, external_state)));
        let strong = self.core_ctx().with_socket_map_mut(|socket_map| socket_map.insert(socket));
        debug!("created raw IP socket {strong:?}, on protocol {protocol:?}");
        strong
    }

    /// Removes the raw IP socket from the system, returning its external state.
    ///
    /// # Panics
    ///
    /// If the provided `id` is not the last instance for the socket, this
    /// method will panic.
    pub fn close(
        &mut self,
        id: RawIpApiSocketId<I, C>,
    ) -> RemoveResourceResultWithContext<
        <C::BindingsContext as RawIpSocketsBindingsTypes>::RawIpSocketState<I>,
        C::BindingsContext,
    > {
        let primary = self.core_ctx().with_socket_map_mut(|socket_map| socket_map.remove(id));
        debug!("removed raw IP socket {primary:?}");
        let PrimaryRawIpSocketId(primary) = primary;

        C::BindingsContext::unwrap_or_notify_with_new_reference_notifier(
            primary,
            |state: RawIpSocketState<I, _, C::BindingsContext>| state.into_external_state(),
        )
    }

    /// Sends an IP packet on the raw IP socket to the provided destination.
    ///
    /// The provided `body` is not expected to include an IP header; a system
    /// determined header will automatically be applied.
    pub fn send_to<B: BufferMut>(
        &mut self,
        id: &RawIpApiSocketId<I, C>,
        remote_ip: Option<
            ZonedAddr<
                SpecifiedAddr<I::Addr>,
                <C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId,
            >,
        >,
        body: B,
    ) -> Result<(), RawIpSocketSendToError> {
        match id.protocol() {
            RawIpSocketProtocol::Raw => return Err(RawIpSocketSendToError::ProtocolRaw),
            RawIpSocketProtocol::Proto(_) => {}
        }
        // TODO(https://fxbug.dev/339692009): Return an error if IP_HDRINCL is
        // set.

        // TODO(https://fxbug.dev/342579393): Use the socket's bound address.
        let local_ip = None;

        let remote_ip = match DualStackRemoteIp::<I, _>::new(remote_ip) {
            DualStackRemoteIp::ThisStack(addr) => addr,
            DualStackRemoteIp::OtherStack(_addr) => {
                return Err(RawIpSocketSendToError::MappedRemoteIp)
            }
        };
        let protocol = id.protocol().proto();

        // TODO(https://fxbug.dev/337818855): Use the socket's send options.
        let options = DefaultSendOptions::default();

        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.with_locked_state_and_socket_handler(id, |state, core_ctx| {
            let device = state.bound_device.clone();
            let (remote_ip, device) =
                remote_ip.resolve_addr_with_device(device).map_err(RawIpSocketSendToError::Zone)?;
            core_ctx
                .send_oneshot_ip_packet(
                    bindings_ctx,
                    device.as_ref().map(|d| d.as_ref()),
                    local_ip,
                    remote_ip,
                    protocol,
                    &options,
                    |src_ip| RawIpBody::new(protocol, src_ip.addr(), remote_ip.addr(), body),
                    None,
                )
                .map_err(RawIpSocketSendToError::Ip)
        })
    }

    // TODO(https://fxbug.dev/342577389): Add a `send` function that does not
    // require a remote_ip to support sending on connected sockets.
    // TODO(https://fxbug.dev/339692009): Add a `send` function that does not
    // require a remote_ip to support sending when the remote_ip is provided via
    // IP_HDRINCL.

    /// Sets the socket's bound device, returning the original value.
    pub fn set_device(
        &mut self,
        id: &RawIpApiSocketId<I, C>,
        device: Option<&<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        let device = device.map(|strong| strong.downgrade());
        // TODO(https://fxbug.dev/342579393): Verify the device is compatible
        // with the socket's bound address.
        // TODO(https://fxbug.dev/342577389): Verify the device is compatible
        // with the socket's peer address.
        self.core_ctx()
            .with_locked_state_mut(id, |state| core::mem::replace(&mut state.bound_device, device))
    }

    /// Gets the socket's bound device,
    pub fn get_device(
        &mut self,
        id: &RawIpApiSocketId<I, C>,
    ) -> Option<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        self.core_ctx().with_locked_state(id, |state| state.bound_device.clone())
    }
}

/// Errors that may occur when calling [`RawIpSocketApi::send_to`].
#[derive(Debug)]
pub enum RawIpSocketSendToError {
    /// The socket's protocol is `RawIpSocketProtocol::Raw`, which disallows
    /// `send_to` (the remote IP should be specified in the included header, not
    /// as a separate address argument).
    ProtocolRaw,
    /// The provided remote_ip was an IPv4-mapped-IPv6 address. Dual stack
    /// operations are not supported on raw IP sockets.
    MappedRemoteIp,
    /// There was an error when resolving the remote_ip's zone.
    Zone(ZonedAddressError),
    /// The IP layer failed to send the packet.
    Ip(IpSockCreateAndSendError),
}

/// The owner of socket state.
struct PrimaryRawIpSocketId<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes>(
    PrimaryRc<RawIpSocketState<I, D, BT>>,
);

impl<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> Debug
    for PrimaryRawIpSocketId<I, D, BT>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("RawIpSocketId").field(&PrimaryRc::debug_id(rc)).finish()
    }
}

/// Reference to the state of a live socket.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), Hash(bound = ""), PartialEq(bound = ""))]
pub struct RawIpSocketId<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes>(
    StrongRc<RawIpSocketState<I, D, BT>>,
);

impl<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> RawIpSocketId<I, D, BT> {
    /// Return the bindings state associated with this socket.
    pub fn external_state(&self) -> &BT::RawIpSocketState<I> {
        let RawIpSocketId(strong_rc) = self;
        strong_rc.external_state()
    }
    /// Return the protocol associated with this socket.
    pub fn protocol(&self) -> &RawIpSocketProtocol<I> {
        let RawIpSocketId(strong_rc) = self;
        strong_rc.protocol()
    }
    /// Downgrades this ID to a weak reference.
    pub fn downgrade(&self) -> WeakRawIpSocketId<I, D, BT> {
        let Self(rc) = self;
        WeakRawIpSocketId(StrongRc::downgrade(rc))
    }

    /// Gets the socket state.
    pub fn state(&self) -> &RawIpSocketState<I, D, BT> {
        let RawIpSocketId(strong_rc) = self;
        &*strong_rc
    }
}

impl<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> Debug
    for RawIpSocketId<I, D, BT>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("RawIpSocketId").field(&StrongRc::debug_id(rc)).finish()
    }
}

/// A weak reference to a raw IP socket.
pub struct WeakRawIpSocketId<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes>(
    WeakRc<RawIpSocketState<I, D, BT>>,
);

impl<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> Debug
    for WeakRawIpSocketId<I, D, BT>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("WeakRawIpSocketId").field(&WeakRc::debug_id(rc)).finish()
    }
}

/// An alias for [`RawIpSocketId`] in [`RawIpSocketApi`], for brevity.
type RawIpApiSocketId<I, C> = RawIpSocketId<
    I,
    <<C as ContextPair>::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
    <C as ContextPair>::BindingsContext,
>;

/// Provides access to the [`RawIpSocketLockedState`] for a raw IP socket.
///
/// Implementations must ensure a proper lock ordering is adhered to.
pub trait RawIpSocketStateContext<I: IpExt, BT: RawIpSocketsBindingsTypes>:
    DeviceIdContext<AnyDevice>
{
    /// The implementation of `IpSocketHandler` available after having locked
    /// the state for an individual socket.
    type SocketHandler<'a>: IpSocketHandler<
        I,
        BT,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the callback with an immutable reference to the socket's locked
    /// state.
    fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I, Self::WeakDeviceId>) -> O>(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BT>,
        cb: F,
    ) -> O;

    /// Calls the callback with an immutable reference to the socket's locked
    /// state and the `SocketHandler`.
    fn with_locked_state_and_socket_handler<
        O,
        F: FnOnce(&RawIpSocketLockedState<I, Self::WeakDeviceId>, &mut Self::SocketHandler<'_>) -> O,
    >(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BT>,
        cb: F,
    ) -> O;

    /// Calls the callback with a mutable reference to the socket's locked
    /// state.
    fn with_locked_state_mut<
        O,
        F: FnOnce(&mut RawIpSocketLockedState<I, Self::WeakDeviceId>) -> O,
    >(
        &mut self,
        id: &RawIpSocketId<I, Self::WeakDeviceId, BT>,
        cb: F,
    ) -> O;
}

/// The collection of all raw IP sockets installed in the system.
///
/// Implementations must ensure a proper lock ordering is adhered to.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct RawIpSocketMap<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> {
    /// All sockets installed in the system.
    ///
    /// This is a nested collection, with the outer `BTreeMap` indexable by the
    /// socket's protocol, which allows for more efficient delivery of received
    /// IP packets.
    ///
    /// NB: The inner map is a `HashMap` keyed by strong IDs, rather than an
    /// `HashSet` keyed by primary IDs, because it would be impossible to build
    /// a lookup key for the hashset (there can only ever exist 1 primary ID,
    /// which is *in* the set).
    sockets: BTreeMap<
        RawIpSocketProtocol<I>,
        HashMap<RawIpSocketId<I, D, BT>, PrimaryRawIpSocketId<I, D, BT>>,
    >,
}

impl<I: IpExt, D: WeakDeviceIdentifier, BT: RawIpSocketsBindingsTypes> RawIpSocketMap<I, D, BT> {
    fn insert(&mut self, socket: PrimaryRawIpSocketId<I, D, BT>) -> RawIpSocketId<I, D, BT> {
        let RawIpSocketMap { sockets } = self;
        let PrimaryRawIpSocketId(primary) = &socket;
        let strong = RawIpSocketId(PrimaryRc::clone_strong(primary));
        // NB: The socket must be newly inserted because there can only ever
        // be a single primary ID for a socket.
        assert!(sockets
            .entry(*strong.protocol())
            .or_default()
            .insert(strong.clone(), socket)
            .is_none());
        strong
    }

    fn remove(&mut self, socket: RawIpSocketId<I, D, BT>) -> PrimaryRawIpSocketId<I, D, BT> {
        // NB: This function asserts on the presence of `protocol` in the
        // outer map, and the `socket` in the inner map.  The strong ID is
        // witness to the liveness of socket.
        let RawIpSocketMap { sockets } = self;
        let protocol = *socket.protocol();
        match sockets.entry(protocol) {
            Entry::Vacant(_) => unreachable!(
                "{socket:?} with protocol {protocol:?} must be present in the socket map"
            ),
            Entry::Occupied(mut entry) => {
                let map = entry.get_mut();
                let primary = map.remove(&socket).unwrap();
                // NB: If this was the last socket for this protocol, remove
                // the entry from the outer `BTreeMap`.
                if map.is_empty() {
                    let _: HashMap<RawIpSocketId<I, D, BT>, PrimaryRawIpSocketId<I, D, BT>> =
                        entry.remove();
                }
                primary
            }
        }
    }

    fn iter_sockets_for_protocol(
        &self,
        protocol: &RawIpSocketProtocol<I>,
    ) -> impl Iterator<Item = &RawIpSocketId<I, D, BT>> {
        let RawIpSocketMap { sockets } = self;
        sockets.get(protocol).map(|sockets| sockets.keys()).into_iter().flatten()
    }
}

/// A type that provides access to the `RawIpSocketMap` used by the system.
pub trait RawIpSocketMapContext<I: IpExt, BT: RawIpSocketsBindingsTypes>:
    DeviceIdContext<AnyDevice>
{
    /// The implementation of `RawIpSocketStateContext` available after having
    /// accessed the system's socket map.
    type StateCtx<'a>: RawIpSocketStateContext<
        I,
        BT,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Calls the callback with an immutable reference to the socket map.
    fn with_socket_map_and_state_ctx<
        O,
        F: FnOnce(&RawIpSocketMap<I, Self::WeakDeviceId, BT>, &mut Self::StateCtx<'_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;
    /// Calls the callback with a mutable reference to the socket map.
    fn with_socket_map_mut<O, F: FnOnce(&mut RawIpSocketMap<I, Self::WeakDeviceId, BT>) -> O>(
        &mut self,
        cb: F,
    ) -> O;
}

/// A type that provides the raw IP socket functionality required by core.
pub trait RawIpSocketHandler<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Deliver a received IP packet to all appropriate raw IP sockets.
    fn deliver_packet_to_raw_ip_sockets<B: ByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        packet: &I::Packet<B>,
        device: &Self::DeviceId,
    );
}

impl<I, BC, CC> RawIpSocketHandler<I, BC> for CC
where
    I: IpExt,
    BC: RawIpSocketsBindingsContext<I, CC::DeviceId>,
    CC: RawIpSocketMapContext<I, BC>,
{
    fn deliver_packet_to_raw_ip_sockets<B: ByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        packet: &I::Packet<B>,
        device: &CC::DeviceId,
    ) {
        let protocol = RawIpSocketProtocol::new(packet.proto());

        // NB: sockets with `RawIpSocketProtocol::Raw` are send only, and cannot
        // receive packets.
        match protocol {
            RawIpSocketProtocol::Raw => {
                debug!("received IP packet with raw protocol (IANA Reserved - 255); dropping");
                return;
            }
            RawIpSocketProtocol::Proto(_) => {}
        };

        self.with_socket_map_and_state_ctx(|socket_map, core_ctx| {
            socket_map.iter_sockets_for_protocol(&protocol).for_each(|socket| {
                if core_ctx.with_locked_state(socket, |state| {
                    should_deliver_to_socket(packet, device, state)
                }) {
                    bindings_ctx.receive_packet(socket, packet, device)
                }
            })
        })
    }
}

/// Returns 'True' if the given packet should be delivered to the given socket.
fn should_deliver_to_socket<I: IpExt, D: StrongDeviceIdentifier, B: ByteSlice>(
    _packet: &I::Packet<B>,
    device: &D,
    socket: &RawIpSocketLockedState<I, D::Weak>,
) -> bool {
    // TODO(https://fxbug.dev/337816586): Check ICMPv6 Filters.
    let RawIpSocketLockedState { bound_device, _marker } = socket;
    bound_device.as_ref().map_or(true, |bound_device| bound_device == device)
}

#[cfg(test)]
mod test {
    use super::*;

    use alloc::{rc::Rc, vec, vec::Vec};
    use assert_matches::assert_matches;
    use core::{cell::RefCell, convert::Infallible as Never, marker::PhantomData};
    use ip_test_macro::ip_test;
    use net_types::ip::{IpVersion, Ipv4, Ipv6};
    use netstack3_base::{
        sync::{DynDebugReferences, Mutex},
        testutil::{FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId, TestIpExt},
        ContextProvider, CtxPair,
    };
    use packet::{Buf, InnerPacketBuilder as _, ParseBuffer as _, Serializer as _};
    use packet_formats::ip::{IpPacketBuilder, IpProto, IpProtoExt};
    use test_case::test_case;

    use crate::{
        internal::{base::testutil::FakeIpDeviceIdCtx, socket::testutil::FakeIpSocketCtx},
        socket::testutil::FakeDeviceConfig,
        SendIpPacketMeta,
    };

    #[derive(Derivative, Debug)]
    #[derivative(Default(bound = ""))]
    struct FakeExternalSocketState<D> {
        /// The collection of IP packets received on this socket.
        received_packets: Mutex<Vec<ReceivedIpPacket<D>>>,
    }

    #[derive(Debug, PartialEq)]
    struct ReceivedIpPacket<D> {
        data: Vec<u8>,
        device: D,
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeBindingsCtx<D> {
        _device_id_type: PhantomData<D>,
    }

    /// State required to test raw IP sockets. Held by `FakeCoreCtx`.
    struct FakeCoreCtxState<I: IpExt, D: FakeStrongDeviceId> {
        // NB: Hold in an `Rc<RefCell<...>>` to switch to runtime borrow
        // checking. This allows us to borrow the socket map at the same time
        // as the outer `FakeCoreCtx` is mutably borrowed (Required to implement
        // `RawIpSocketMapContext::with_socket_map_and_state_ctx`).
        socket_map: Rc<RefCell<RawIpSocketMap<I, D::Weak, FakeBindingsCtx<D>>>>,
        /// An inner fake implementation of `DeviceIdContext`.
        device_id_ctx: FakeIpDeviceIdCtx<D>,
        /// An inner fake implementation of `IpSocketHandler`. By implementing
        /// `AsRef` and `AsMut` below, the `FakeCoreCtx` will be eligible for a
        /// blanket impl of `IpSocketHandler`.
        ip_socket_ctx: FakeIpSocketCtx<I, D>,
    }

    impl<I: IpExt, D: FakeStrongDeviceId> AsRef<FakeIpDeviceIdCtx<D>> for FakeCoreCtxState<I, D> {
        fn as_ref(&self) -> &FakeIpDeviceIdCtx<D> {
            &self.device_id_ctx
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> AsRef<FakeIpSocketCtx<I, D>> for FakeCoreCtxState<I, D> {
        fn as_ref(&self) -> &FakeIpSocketCtx<I, D> {
            &self.ip_socket_ctx
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> AsMut<FakeIpSocketCtx<I, D>> for FakeCoreCtxState<I, D> {
        fn as_mut(&mut self) -> &mut FakeIpSocketCtx<I, D> {
            &mut self.ip_socket_ctx
        }
    }

    type FakeCoreCtx<I, D> = netstack3_base::testutil::FakeCoreCtx<
        FakeCoreCtxState<I, D>,
        SendIpPacketMeta<I, D, SpecifiedAddr<<I as Ip>::Addr>>,
        D,
    >;

    impl<D: FakeStrongDeviceId> RawIpSocketsBindingsTypes for FakeBindingsCtx<D> {
        type RawIpSocketState<I: Ip> = FakeExternalSocketState<D>;
    }

    impl<I: IpExt, D: Copy + FakeStrongDeviceId> RawIpSocketsBindingsContext<I, D>
        for FakeBindingsCtx<D>
    {
        fn receive_packet<B: ByteSlice>(
            &self,
            socket: &RawIpSocketId<I, D::Weak, Self>,
            packet: &I::Packet<B>,
            device: &D,
        ) {
            let packet = ReceivedIpPacket { data: packet.to_vec(), device: *device };
            let FakeExternalSocketState { received_packets } = socket.external_state();
            received_packets.lock().push(packet);
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> RawIpSocketStateContext<I, FakeBindingsCtx<D>>
        for FakeCoreCtx<I, D>
    {
        type SocketHandler<'a> = FakeCoreCtx<I, D>;
        fn with_locked_state<O, F: FnOnce(&RawIpSocketLockedState<I, D::Weak>) -> O>(
            &mut self,
            id: &RawIpSocketId<I, D::Weak, FakeBindingsCtx<D>>,
            cb: F,
        ) -> O {
            let RawIpSocketId(state_rc) = id;
            let guard = state_rc.locked_state().read();
            cb(&guard)
        }
        fn with_locked_state_and_socket_handler<
            O,
            F: FnOnce(&RawIpSocketLockedState<I, D::Weak>, &mut Self::SocketHandler<'_>) -> O,
        >(
            &mut self,
            id: &RawIpSocketId<I, D::Weak, FakeBindingsCtx<D>>,
            cb: F,
        ) -> O {
            let RawIpSocketId(state_rc) = id;
            let guard = state_rc.locked_state().read();
            cb(&guard, self)
        }
        fn with_locked_state_mut<O, F: FnOnce(&mut RawIpSocketLockedState<I, D::Weak>) -> O>(
            &mut self,
            id: &RawIpSocketId<I, D::Weak, FakeBindingsCtx<D>>,
            cb: F,
        ) -> O {
            let RawIpSocketId(state_rc) = id;
            let mut guard = state_rc.locked_state().write();
            cb(&mut guard)
        }
    }

    impl<I: IpExt, D: FakeStrongDeviceId> RawIpSocketMapContext<I, FakeBindingsCtx<D>>
        for FakeCoreCtx<I, D>
    {
        type StateCtx<'a> = FakeCoreCtx<I, D>;
        fn with_socket_map_and_state_ctx<
            O,
            F: FnOnce(&RawIpSocketMap<I, D::Weak, FakeBindingsCtx<D>>, &mut Self::StateCtx<'_>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            let socket_map = self.state.socket_map.clone();
            let borrow = socket_map.borrow();
            cb(&borrow, self)
        }
        fn with_socket_map_mut<
            O,
            F: FnOnce(&mut RawIpSocketMap<I, D::Weak, FakeBindingsCtx<D>>) -> O,
        >(
            &mut self,
            cb: F,
        ) -> O {
            cb(&mut self.state.socket_map.borrow_mut())
        }
    }

    impl<D> ContextProvider for FakeBindingsCtx<D> {
        type Context = FakeBindingsCtx<D>;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    impl<D> ReferenceNotifiers for FakeBindingsCtx<D> {
        type ReferenceReceiver<T: 'static> = Never;

        type ReferenceNotifier<T: Send + 'static> = Never;

        fn new_reference_notifier<T: Send + 'static>(
            _debug_references: DynDebugReferences,
        ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>) {
            unimplemented!("raw IP socket removal shouldn't be deferred in tests");
        }
    }

    fn new_raw_ip_socket_api<I: IpExt + TestIpExt>() -> RawIpSocketApi<
        I,
        CtxPair<FakeCoreCtx<I, MultipleDevicesId>, FakeBindingsCtx<MultipleDevicesId>>,
    > {
        // Set up all devices with a local IP and a route to the remote IP.
        let device_configs = [MultipleDevicesId::A, MultipleDevicesId::B, MultipleDevicesId::C]
            .into_iter()
            .map(|device| FakeDeviceConfig {
                device,
                local_ips: vec![I::TEST_ADDRS.local_ip],
                remote_ips: vec![I::TEST_ADDRS.remote_ip],
            });
        let state = FakeCoreCtxState {
            socket_map: Default::default(),
            device_id_ctx: Default::default(),
            ip_socket_ctx: FakeIpSocketCtx::new(device_configs),
        };

        RawIpSocketApi::new(CtxPair::with_core_ctx(FakeCoreCtx::with_state(state)))
    }

    /// Arbitrary data to put inside of an IP packet.
    const IP_BODY: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    /// Constructs a buffer containing an IP packet with sensible defaults.
    fn new_ip_packet_buf<I: IpExt>(ip_body: &[u8], proto: I::Proto) -> impl AsRef<[u8]> {
        const TTL: u8 = 255;
        ip_body
            .into_serializer()
            .encapsulate(I::PacketBuilder::new(
                *I::LOOPBACK_ADDRESS,
                *I::LOOPBACK_ADDRESS,
                TTL,
                proto,
            ))
            .serialize_vec_outer()
            .unwrap()
    }

    #[ip_test]
    #[test_case(IpProto::Udp; "UDP")]
    #[test_case(IpProto::Reserved; "IPPROTO_RAW")]
    fn create_and_close<I: Ip + IpExt + DualStackIpExt + TestIpExt>(proto: IpProto) {
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(RawIpSocketProtocol::new(proto.into()), Default::default());
        let FakeExternalSocketState { received_packets: _ } = api.close(sock).into_removed();
    }

    #[ip_test]
    fn set_device<I: Ip + IpExt + DualStackIpExt + TestIpExt>() {
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(RawIpSocketProtocol::new(IpProto::Udp.into()), Default::default());

        assert_eq!(api.get_device(&sock), None);
        assert_eq!(api.set_device(&sock, Some(&MultipleDevicesId::A)), None);
        assert_eq!(api.get_device(&sock), Some(FakeWeakDeviceId(MultipleDevicesId::A)));
        assert_eq!(
            api.set_device(&sock, Some(&MultipleDevicesId::B)),
            Some(FakeWeakDeviceId(MultipleDevicesId::A))
        );
        assert_eq!(api.get_device(&sock), Some(FakeWeakDeviceId(MultipleDevicesId::B)));
        assert_eq!(api.set_device(&sock, None), Some(FakeWeakDeviceId(MultipleDevicesId::B)));
        assert_eq!(api.get_device(&sock), None);
    }

    #[ip_test]
    fn receive_ip_packet<I: Ip + IpExt + DualStackIpExt + TestIpExt>() {
        let mut api = new_raw_ip_socket_api::<I>();

        // Create two sockets with the right protocol, and one socket with the
        // wrong protocol.
        let proto: I::Proto = IpProto::Udp.into();
        let wrong_proto: I::Proto = IpProto::Tcp.into();
        let sock1 = api.create(RawIpSocketProtocol::new(proto), Default::default());
        let sock2 = api.create(RawIpSocketProtocol::new(proto), Default::default());
        let wrong_sock = api.create(RawIpSocketProtocol::new(wrong_proto), Default::default());

        // Receive an IP packet with protocol `proto`.
        const DEVICE: MultipleDevicesId = MultipleDevicesId::A;
        let buf = new_ip_packet_buf::<I>(&IP_BODY, proto);
        let mut buf_ref = buf.as_ref();
        let packet = buf_ref.parse::<I::Packet<_>>().expect("parse should succeed");
        {
            let (core_ctx, bindings_ctx) = api.ctx.contexts();
            core_ctx.deliver_packet_to_raw_ip_sockets(bindings_ctx, &packet, &DEVICE);
        }

        let FakeExternalSocketState { received_packets: sock1_packets } =
            api.close(sock1).into_removed();
        let FakeExternalSocketState { received_packets: sock2_packets } =
            api.close(sock2).into_removed();
        let FakeExternalSocketState { received_packets: wrong_sock_packets } =
            api.close(wrong_sock).into_removed();

        // Expect delivery to the two right sockets, but not the wrong socket.
        for ReceivedIpPacket { data, device } in
            [sock1_packets.lock().pop().unwrap(), sock2_packets.lock().pop().unwrap()]
        {
            assert_eq!(&data[..], buf.as_ref());
            assert_eq!(device, DEVICE);
        }
        assert_eq!(wrong_sock_packets.lock().pop(), None);
    }

    // Verify that sockets created with `RawIpSocketProtocol::Raw` cannot
    // receive packets
    #[ip_test]
    fn cannot_receive_ip_packet_with_proto_raw<I: Ip + IpExt + DualStackIpExt + TestIpExt>() {
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(RawIpSocketProtocol::Raw, Default::default());

        // Try to deliver to an arbitrary proto (UDP), and to the reserved
        // proto; neither should be delivered to the socket.
        let protocols_to_test = match I::VERSION {
            IpVersion::V4 => vec![IpProto::Udp, IpProto::Reserved],
            // NB: Don't test `Reserved` with IPv6; the packet will fail to
            // parse.
            IpVersion::V6 => vec![IpProto::Udp],
        };
        for proto in protocols_to_test {
            let buf = new_ip_packet_buf::<I>(&IP_BODY, proto.into());
            let mut buf_ref = buf.as_ref();
            let packet = buf_ref.parse::<I::Packet<_>>().expect("parse should succeed");
            let (core_ctx, bindings_ctx) = api.ctx.contexts();
            core_ctx.deliver_packet_to_raw_ip_sockets(bindings_ctx, &packet, &MultipleDevicesId::A);
        }

        let FakeExternalSocketState { received_packets } = api.close(sock).into_removed();
        assert_eq!(received_packets.lock().pop(), None);
    }

    #[ip_test]
    #[test_case(MultipleDevicesId::A, None, true; "no_bound_device")]
    #[test_case(MultipleDevicesId::A, Some(MultipleDevicesId::A), true; "bound_same_device")]
    #[test_case(MultipleDevicesId::A, Some(MultipleDevicesId::B), false; "bound_diff_device")]
    fn receive_ip_packet_with_bound_device<I: Ip + IpExt + DualStackIpExt + TestIpExt>(
        send_dev: MultipleDevicesId,
        bound_dev: Option<MultipleDevicesId>,
        should_deliver: bool,
    ) {
        const PROTO: IpProto = IpProto::Udp;
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(RawIpSocketProtocol::new(PROTO.into()), Default::default());

        assert_eq!(api.set_device(&sock, bound_dev.as_ref()), None);

        // Deliver an arbitrary packet on `send_dev`.
        let buf = new_ip_packet_buf::<I>(&IP_BODY, PROTO.into());
        let mut buf_ref = buf.as_ref();
        let packet = buf_ref.parse::<I::Packet<_>>().expect("parse should succeed");
        {
            let (core_ctx, bindings_ctx) = api.ctx.contexts();
            core_ctx.deliver_packet_to_raw_ip_sockets(bindings_ctx, &packet, &send_dev);
        }

        // Verify the packet was/wasn't received, as expected.
        let FakeExternalSocketState { received_packets } = api.close(sock).into_removed();
        if should_deliver {
            let ReceivedIpPacket { data, device } = received_packets.lock().pop().unwrap();
            assert_eq!(&data[..], buf.as_ref());
            assert_eq!(device, send_dev);
        } else {
            assert_eq!(received_packets.lock().pop(), None);
        }
    }

    #[ip_test]
    #[test_case(Some(MultipleDevicesId::A); "bound_to_dev")]
    #[test_case(None; "not_bound_to_dev")]
    fn send_to<I: Ip + IpExt + DualStackIpExt + TestIpExt>(bound_dev: Option<MultipleDevicesId>) {
        const PROTO: IpProto = IpProto::Udp;
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(RawIpSocketProtocol::new(PROTO.into()), Default::default());

        assert_eq!(api.set_device(&sock, bound_dev.as_ref()), None);

        let remote_ip = ZonedAddr::Unzoned(I::TEST_ADDRS.remote_ip);
        assert_matches!(&api.ctx.core_ctx().take_frames()[..], []);
        api.send_to(&sock, Some(remote_ip), Buf::new(IP_BODY.to_vec(), ..))
            .expect("send should succeed");
        let frames = api.core_ctx().take_frames();
        let (SendIpPacketMeta { device, src_ip, dst_ip, proto, mtu, ttl, .. }, data) =
            assert_matches!( &frames[..], [packet] => packet);
        assert_eq!(&data[..], &IP_BODY[..]);
        assert_eq!(*dst_ip, remote_ip.addr());
        assert_eq!(*src_ip, I::TEST_ADDRS.local_ip);
        if let Some(bound_dev) = bound_dev {
            assert_eq!(*device, bound_dev);
        }
        assert_eq!(*proto, <I as IpProtoExt>::Proto::from(PROTO));
        assert_eq!(*mtu, None);
        assert_eq!(*ttl, None);
    }

    #[ip_test]
    fn send_to_disallows_raw_protocol<I: Ip + IpExt + DualStackIpExt + TestIpExt>() {
        let mut api = new_raw_ip_socket_api::<I>();
        let sock = api.create(RawIpSocketProtocol::Raw, Default::default());
        assert_matches!(
            api.send_to(&sock, None, Buf::new(IP_BODY.to_vec(), ..)),
            Err(RawIpSocketSendToError::ProtocolRaw)
        );
    }

    #[test]
    fn send_to_disallows_dualstack() {
        let mut api = new_raw_ip_socket_api::<Ipv6>();
        let sock = api.create(RawIpSocketProtocol::new(IpProto::Udp.into()), Default::default());
        let mapped_remote_ip = ZonedAddr::Unzoned(Ipv4::TEST_ADDRS.local_ip.to_ipv6_mapped());
        assert_matches!(
            api.send_to(&sock, Some(mapped_remote_ip), Buf::new(IP_BODY.to_vec(), ..)),
            Err(RawIpSocketSendToError::MappedRemoteIp)
        );
    }
}
