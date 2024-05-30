// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Link-layer sockets (analogous to Linux's AF_PACKET sockets).

use alloc::collections::{HashMap, HashSet};
use core::{fmt::Debug, hash::Hash, num::NonZeroU16};

use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::{ethernet::Mac, ip::IpVersion};
use packet::{BufferMut, ParsablePacket as _, Serializer};
use packet_formats::{
    error::ParseError,
    ethernet::{EtherType, EthernetFrameLengthCheck},
};

use crate::{
    context::{ContextPair, SendFrameContext},
    device::{
        AnyDevice, Device, DeviceIdContext, DeviceLayerTypes, FrameDestination,
        StrongDeviceIdentifier as _, WeakDeviceId, WeakDeviceIdentifier as _,
    },
    sync::{Mutex, PrimaryRc, RwLock, StrongRc},
};

mod integration;
#[cfg(test)]
mod integration_tests;

/// A selector for frames based on link-layer protocol number.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum Protocol {
    /// Select all frames, regardless of protocol number.
    All,
    /// Select frames with the given protocol number.
    Specific(NonZeroU16),
}

/// Selector for devices to send and receive packets on.
#[derive(Clone, Debug, Derivative, Eq, Hash, PartialEq)]
#[derivative(Default(bound = ""))]
pub enum TargetDevice<D> {
    /// Act on any device in the system.
    #[derivative(Default)]
    AnyDevice,
    /// Act on a specific device.
    SpecificDevice(D),
}

/// Information about the bound state of a socket.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct SocketInfo<D> {
    /// The protocol the socket is bound to, or `None` if no protocol is set.
    pub protocol: Option<Protocol>,
    /// The device selector for which the socket is set.
    pub device: TargetDevice<D>,
}

/// Provides associated types for device sockets provided by the bindings
/// context.
pub trait DeviceSocketTypes {
    /// State for the socket held by core and exposed to bindings.
    type SocketState: Send + Sync + Debug;
}

/// The execution context for device sockets provided by bindings.
pub trait DeviceSocketBindingsContext<DeviceId>: DeviceSocketTypes {
    /// Called for each received frame that matches the provided socket.
    ///
    /// `frame` and `raw_frame` are parsed and raw views into the same data.
    fn receive_frame(
        &self,
        socket: &Self::SocketState,
        device: &DeviceId,
        frame: Frame<&[u8]>,
        raw_frame: &[u8],
    );
}

/// Strong owner of socket state.
///
/// This type strongly owns the socket state.
#[derive(Debug)]
pub struct PrimaryDeviceSocketId<S, D>(PrimaryRc<SocketState<S, D>>);

impl<S, D> PrimaryDeviceSocketId<S, D> {
    /// Creates a new socket ID with `external_state`.
    pub fn new(external_state: S) -> Self {
        Self(PrimaryRc::new(SocketState { external_state, target: Default::default() }))
    }

    /// Drops this primary reference returning the internal state.
    ///
    /// # Panics
    ///
    /// Panics if there are strong [`DeviceSocketId`] clones alive.
    pub fn unwrap(self) -> SocketState<S, D> {
        let Self(rc) = self;
        PrimaryRc::unwrap(rc)
    }
}

/// Reference to live socket state.
///
/// The existence of a `StrongId` attests to the liveness of the state of the
/// backing socket.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""), Eq(bound = ""), PartialEq(bound = ""))]
pub struct DeviceSocketId<S, D>(StrongRc<SocketState<S, D>>);

impl<S, D> Debug for DeviceSocketId<S, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("DeviceSocketId").field(&StrongRc::debug_id(rc)).finish()
    }
}

pub trait StrongSocketId: Hash + Eq + PartialEq + Clone {
    type Primary;
    fn clone_strong(primary: &Self::Primary) -> Self;
}

impl<S, D> StrongSocketId for DeviceSocketId<S, D> {
    type Primary = PrimaryDeviceSocketId<S, D>;
    fn clone_strong(primary: &Self::Primary) -> Self {
        let PrimaryDeviceSocketId(rc) = primary;
        Self(PrimaryRc::clone_strong(rc))
    }
}

impl<S, D> OrderedLockAccess<Target<D>> for DeviceSocketId<S, D> {
    type Lock = Mutex<Target<D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        let Self(rc) = self;
        OrderedLockRef::new(&rc.target)
    }
}

/// Holds shared state for sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct Sockets<Id: StrongSocketId> {
    /// Holds strong (but not owning) references to sockets that aren't
    /// targeting a particular device.
    any_device_sockets: RwLock<AnyDeviceSockets<Id>>,

    /// Table of all sockets in the system, regardless of target.
    ///
    /// Holds the primary (owning) reference for all sockets.
    // This needs to be after `any_device_sockets` so that when an instance of
    // this type is dropped, any strong IDs get dropped before their
    // corresponding primary IDs.
    all_sockets: Mutex<AllSockets<Id>>,
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct AnyDeviceSockets<Id>(HashSet<Id>);

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct AllSockets<Id: StrongSocketId>(HashMap<Id, Id::Primary>);

#[derive(Debug)]
pub struct SocketState<S, D> {
    /// State provided by bindings that is held in core.
    external_state: S,
    /// The socket's target device and protocol.
    // TODO(https://fxbug.dev/42077026): Consider splitting up the state here to
    // improve performance.
    target: Mutex<Target<D>>,
}

#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Target<D> {
    protocol: Option<Protocol>,
    device: TargetDevice<D>,
}

/// Per-device state for packet sockets.
///
/// Holds sockets that are bound to a particular device. An instance of this
/// should be held in the state for each device in the system.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derivative(Debug, PartialEq(bound = "Id: Hash + Eq")))]
pub struct DeviceSockets<Id>(HashSet<Id>);

/// Convenience alias for use in device state storage.
pub type HeldDeviceSockets<BT> =
    DeviceSockets<DeviceSocketId<<BT as DeviceSocketTypes>::SocketState, WeakDeviceId<BT>>>;

/// Convenience alias for use in shared storage.
///
/// The type parameter is expected to implement [`DeviceSocketTypes`].
pub type HeldSockets<BT> =
    Sockets<DeviceSocketId<<BT as DeviceSocketTypes>::SocketState, WeakDeviceId<BT>>>;

/// Common types across all core context traits for device sockets.
pub trait DeviceSocketContextTypes<BT: DeviceSocketTypes> {
    /// The strongly-owning socket ID type.
    ///
    /// This type is held in various data structures and its existence
    /// indicates liveness of socket state, but not ownership.
    type SocketId: Clone + Debug + Eq + Hash + StrongSocketId;

    /// Creates a new primary socket ID.
    fn new_primary(external_state: BT::SocketState) -> <Self::SocketId as StrongSocketId>::Primary;
    /// Extracts the socket state information from a primary ID.
    fn unwrap_primary(primary: <Self::SocketId as StrongSocketId>::Primary) -> BT::SocketState;
}

/// Core context for accessing socket state.
pub trait DeviceSocketContext<BT: DeviceSocketTypes>: DeviceSocketAccessor<BT> {
    /// The core context available in callbacks to methods on this context.
    type SocketTablesCoreCtx<'a>: DeviceSocketAccessor<
        BT,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
        SocketId = Self::SocketId,
    >;

    /// Executes the provided callback with mutable access to the collection of
    /// all sockets.
    fn with_all_device_sockets_mut<F: FnOnce(&mut AllSockets<Self::SocketId>) -> R, R>(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with immutable access to socket state.
    fn with_any_device_sockets<
        F: FnOnce(&AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with mutable access to socket state.
    fn with_any_device_sockets_mut<
        F: FnOnce(&mut AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;
}

/// Core context for accessing the state of an individual socket.
pub trait SocketStateAccessor<BT: DeviceSocketTypes>:
    DeviceSocketContextTypes<BT> + DeviceIdContext<AnyDevice>
{
    /// Provides read-only access to the state of a socket.
    fn with_socket_state<F: FnOnce(&BT::SocketState, &Target<Self::WeakDeviceId>) -> R, R>(
        &mut self,
        socket: &Self::SocketId,
        cb: F,
    ) -> R;

    /// Provides mutable access to the state of a socket.
    fn with_socket_state_mut<F: FnOnce(&BT::SocketState, &mut Target<Self::WeakDeviceId>) -> R, R>(
        &mut self,
        socket: &Self::SocketId,
        cb: F,
    ) -> R;
}

/// Core context for accessing the socket state for a device.
pub trait DeviceSocketAccessor<BT: DeviceSocketTypes>: SocketStateAccessor<BT> {
    /// Core context available in callbacks to methods on this context.
    type DeviceSocketCoreCtx<'a>: SocketStateAccessor<
        BT,
        SocketId = Self::SocketId,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Executes the provided callback with immutable access to device-specific
    /// socket state.
    fn with_device_sockets<
        F: FnOnce(&DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;

    // Executes the provided callback with mutable access to device-specific
    // socket state.
    fn with_device_sockets_mut<
        F: FnOnce(&mut DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;
}

/// An error encountered when sending a frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SendFrameError {
    /// The device failed to send the frame.
    SendFailed,
}

enum MaybeUpdate<T> {
    NoChange,
    NewValue(T),
}

fn update_device_and_protocol<CC: DeviceSocketContext<BT>, BT: DeviceSocketTypes>(
    core_ctx: &mut CC,
    socket: &CC::SocketId,
    new_device: TargetDevice<&CC::DeviceId>,
    protocol_update: MaybeUpdate<Protocol>,
) {
    core_ctx.with_any_device_sockets_mut(|AnyDeviceSockets(any_device_sockets), core_ctx| {
        // Even if we're never moving the socket from/to the any-device
        // state, we acquire the lock to make the move between devices
        // atomic from the perspective of frame delivery. Otherwise there
        // would be a brief period during which arriving frames wouldn't be
        // delivered to the socket from either device.
        let old_device = core_ctx.with_socket_state_mut(
            socket,
            |_: &BT::SocketState, Target { protocol, device }| {
                match protocol_update {
                    MaybeUpdate::NewValue(p) => *protocol = Some(p),
                    MaybeUpdate::NoChange => (),
                };
                let old_device = match &device {
                    TargetDevice::SpecificDevice(device) => device.upgrade(),
                    TargetDevice::AnyDevice => {
                        assert!(any_device_sockets.remove(socket));
                        None
                    }
                };
                *device = match &new_device {
                    TargetDevice::AnyDevice => TargetDevice::AnyDevice,
                    TargetDevice::SpecificDevice(d) => TargetDevice::SpecificDevice(d.downgrade()),
                };
                old_device
            },
        );

        // This modification occurs without holding the socket's individual
        // lock. That's safe because all modifications to the socket's
        // device are done within a `with_sockets_mut` call, which
        // synchronizes them.

        if let Some(device) = old_device {
            // Remove the reference to the socket from the old device if
            // there is one, and it hasn't been removed.
            core_ctx.with_device_sockets_mut(
                &device,
                |DeviceSockets(device_sockets), _core_ctx| {
                    assert!(device_sockets.remove(socket), "socket not found in device state");
                },
            );
        }

        // Add the reference to the new device, if there is one.
        match &new_device {
            TargetDevice::SpecificDevice(new_device) => core_ctx.with_device_sockets_mut(
                new_device,
                |DeviceSockets(device_sockets), _core_ctx| {
                    assert!(device_sockets.insert(socket.clone()));
                },
            ),
            TargetDevice::AnyDevice => {
                assert!(any_device_sockets.insert(socket.clone()))
            }
        }
    })
}

/// The device socket API.
pub struct DeviceSocketApi<C>(C);

impl<C> DeviceSocketApi<C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

/// A local alias for [`TcpSocketId`] for use in [`TcpApi`].
///
/// TODO(https://github.com/rust-lang/rust/issues/8995): Make this an inherent
/// associated type.
type ApiSocketId<C> = <<C as ContextPair>::CoreContext as DeviceSocketContextTypes<
    <C as ContextPair>::BindingsContext,
>>::SocketId;

impl<C> DeviceSocketApi<C>
where
    C: ContextPair,
    C::CoreContext: DeviceSocketContext<C::BindingsContext>,
    C::BindingsContext:
        DeviceSocketBindingsContext<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair) = self;
        pair.core_ctx()
    }

    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair) = self;
        pair.contexts()
    }

    /// Creates an packet socket with no protocol set configured for all devices.
    pub fn create(
        &mut self,
        external_state: <C::BindingsContext as DeviceSocketTypes>::SocketState,
    ) -> ApiSocketId<C> {
        let core_ctx = self.core_ctx();

        let strong = core_ctx.with_all_device_sockets_mut(|AllSockets(sockets)| {
            let primary =
                <C::CoreContext as DeviceSocketContextTypes<C::BindingsContext>>::new_primary(
                    external_state,
                );
            let strong = <ApiSocketId<C> as StrongSocketId>::clone_strong(&primary);
            assert!(sockets.insert(strong.clone(), primary).is_none());
            strong
        });
        core_ctx.with_any_device_sockets_mut(|AnyDeviceSockets(any_device_sockets), _core_ctx| {
            // On creation, sockets do not target any device or protocol.
            // Inserting them into the `any_device_sockets` table lets us treat
            // newly-created sockets uniformly with sockets whose target device
            // or protocol was set. The difference is unobservable at runtime
            // since newly-created sockets won't match any frames being
            // delivered.
            assert!(any_device_sockets.insert(strong.clone()));
        });
        strong
    }

    /// Sets the device for which a packet socket will receive packets.
    pub fn set_device(
        &mut self,
        socket: &ApiSocketId<C>,
        device: TargetDevice<&<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
    ) {
        update_device_and_protocol(self.core_ctx(), socket, device, MaybeUpdate::NoChange)
    }

    /// Sets the device and protocol for which a socket will receive packets.
    pub fn set_device_and_protocol(
        &mut self,
        socket: &ApiSocketId<C>,
        device: TargetDevice<&<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>,
        protocol: Protocol,
    ) {
        update_device_and_protocol(self.core_ctx(), socket, device, MaybeUpdate::NewValue(protocol))
    }

    /// Gets the bound info for a socket.
    pub fn get_info(
        &mut self,
        id: &ApiSocketId<C>,
    ) -> SocketInfo<<C::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId> {
        self.core_ctx().with_socket_state(id, |_external_state, Target { device, protocol }| {
            SocketInfo { device: device.clone(), protocol: *protocol }
        })
    }

    /// Removes a bound socket.
    ///
    /// # Panics
    ///
    /// If the provided `id` is not the last instance for a socket, this
    /// method will panic.
    pub fn remove(&mut self, id: ApiSocketId<C>) {
        let core_ctx = self.core_ctx();
        core_ctx.with_any_device_sockets_mut(|AnyDeviceSockets(any_device_sockets), core_ctx| {
            let old_device = core_ctx.with_socket_state_mut(&id, |_external_state, target| {
                let Target { device, protocol: _ } = target;
                match &device {
                    TargetDevice::SpecificDevice(device) => device.upgrade(),
                    TargetDevice::AnyDevice => {
                        assert!(any_device_sockets.remove(&id));
                        None
                    }
                }
            });
            if let Some(device) = old_device {
                core_ctx.with_device_sockets_mut(
                    &device,
                    |DeviceSockets(device_sockets), _core_ctx| {
                        assert!(device_sockets.remove(&id), "device doesn't have socket");
                    },
                )
            }
        });

        core_ctx.with_all_device_sockets_mut(|AllSockets(sockets)| {
            let primary = sockets
                .remove(&id)
                .unwrap_or_else(|| panic!("{id:?} not present in all socket map"));
            // Make sure to drop the strong ID before trying to unwrap the primary
            // ID.
            drop(id);

            let _: <C::BindingsContext as DeviceSocketTypes>::SocketState =
                <C::CoreContext as DeviceSocketContextTypes<C::BindingsContext>>::unwrap_primary(
                    primary,
                );
        });
    }

    /// Sends a frame for the specified socket.
    pub fn send_frame<S, D>(
        &mut self,
        _id: &ApiSocketId<C>,
        metadata: DeviceSocketMetadata<D, <C::CoreContext as DeviceIdContext<D>>::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendFrameError)>
    where
        S: Serializer,
        S::Buffer: BufferMut,
        D: DeviceSocketSendTypes,
        C::CoreContext: DeviceIdContext<D>
            + SendFrameContext<
                C::BindingsContext,
                DeviceSocketMetadata<D, <C::CoreContext as DeviceIdContext<D>>::DeviceId>,
            >,
        C::BindingsContext: DeviceLayerTypes,
    {
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx
            .send_frame(bindings_ctx, metadata, body)
            .map_err(|s| (s, SendFrameError::SendFailed))
    }
}

/// A provider of the types required to send on a device socket.
pub trait DeviceSocketSendTypes: Device {
    /// The metadata required to send a frame on the device.
    type Metadata;
}

/// Metadata required to send a frame on a device socket.
#[derive(Debug, PartialEq)]
pub struct DeviceSocketMetadata<D: DeviceSocketSendTypes, DeviceId> {
    /// The device ID to send via.
    pub device_id: DeviceId,
    /// The metadata required to send that's specific to the device type.
    pub metadata: D::Metadata,
}

/// Parameters needed to apply system-framing of an Ethernet frame.
#[derive(Debug, PartialEq)]
pub struct EthernetHeaderParams {
    /// The destination MAC address to send to.
    pub dest_addr: Mac,
    /// The upperlayer protocol of the data contained in this Ethernet frame.
    pub protocol: EtherType,
}

/// Public identifier for a socket.
///
/// Strongly owns the state of the socket. So long as the `SocketId` for a
/// socket is not dropped, the socket is guaranteed to exist.
pub type SocketId<BC> = DeviceSocketId<<BC as DeviceSocketTypes>::SocketState, WeakDeviceId<BC>>;

impl<S, D> DeviceSocketId<S, D> {
    /// Provides immutable access to [`DeviceSocketTypes::SocketState`] for the
    /// socket.
    pub fn socket_state(&self) -> &S {
        let Self(strong) = self;
        let SocketState { external_state, target: _ } = &**strong;
        external_state
    }
}

/// Allows the rest of the stack to dispatch packets to listening sockets.
///
/// This is implemented on top of [`DeviceSocketContext`] and abstracts packet
/// socket delivery from the rest of the system.
pub trait DeviceSocketHandler<D: Device, BC>: DeviceIdContext<D> {
    /// Dispatch a received frame to sockets.
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        frame: Frame<&[u8]>,
        whole_frame: &[u8],
    );
}

/// A frame received on a device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceivedFrame<B> {
    /// An ethernet frame received on a device.
    Ethernet {
        /// Where the frame was destined.
        destination: FrameDestination,
        /// The parsed ethernet frame.
        frame: EthernetFrame<B>,
    },
    /// An IP frame received on a device.
    ///
    /// Note that this is not an IP packet within an Ethernet Frame. This is an
    /// IP packet received directly from the device (e.g. a pure IP device).
    Ip(IpFrame<B>),
}

/// A frame sent on a device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SentFrame<B> {
    /// An ethernet frame sent on a device.
    Ethernet(EthernetFrame<B>),
    /// An IP frame sent on a device.
    ///
    /// Note that this is not an IP packet within an Ethernet Frame. This is an
    /// IP Packet send directly on the device (e.g. a pure IP device).
    Ip(IpFrame<B>),
}

/// A frame couldn't be parsed as a [`SentFrame`].
#[derive(Debug)]
pub struct ParseSentFrameError;

impl SentFrame<&[u8]> {
    /// Tries to parse the given frame as an Ethernet frame.
    pub(crate) fn try_parse_as_ethernet(
        mut buf: &[u8],
    ) -> Result<SentFrame<&[u8]>, ParseSentFrameError> {
        packet_formats::ethernet::EthernetFrame::parse(&mut buf, EthernetFrameLengthCheck::NoCheck)
            .map_err(|_: ParseError| ParseSentFrameError)
            .map(|frame| SentFrame::Ethernet(frame.into()))
    }
}

/// Data from an Ethernet frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EthernetFrame<B> {
    /// The source address of the frame.
    pub src_mac: Mac,
    /// The destination address of the frame.
    pub dst_mac: Mac,
    /// The EtherType of the frame, or `None` if there was none.
    pub ethertype: Option<EtherType>,
    /// The body of the frame.
    pub body: B,
}

/// Data from an IP frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpFrame<B> {
    /// The IP version of the frame.
    pub ip_version: IpVersion,
    /// The body of the frame.
    pub body: B,
}

impl<B> IpFrame<B> {
    fn ethertype(&self) -> EtherType {
        let IpFrame { ip_version, body: _ } = self;
        EtherType::from_ip_version(*ip_version)
    }
}

/// A frame sent or received on a device
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Frame<B> {
    /// A sent frame.
    Sent(SentFrame<B>),
    /// A received frame.
    Received(ReceivedFrame<B>),
}

impl<B> From<SentFrame<B>> for Frame<B> {
    fn from(value: SentFrame<B>) -> Self {
        Self::Sent(value)
    }
}

impl<B> From<ReceivedFrame<B>> for Frame<B> {
    fn from(value: ReceivedFrame<B>) -> Self {
        Self::Received(value)
    }
}

impl<'a> From<packet_formats::ethernet::EthernetFrame<&'a [u8]>> for EthernetFrame<&'a [u8]> {
    fn from(frame: packet_formats::ethernet::EthernetFrame<&'a [u8]>) -> Self {
        Self {
            src_mac: frame.src_mac(),
            dst_mac: frame.dst_mac(),
            ethertype: frame.ethertype(),
            body: frame.into_body(),
        }
    }
}

impl<'a> ReceivedFrame<&'a [u8]> {
    pub(crate) fn from_ethernet(
        frame: packet_formats::ethernet::EthernetFrame<&'a [u8]>,
        destination: FrameDestination,
    ) -> Self {
        Self::Ethernet { destination, frame: frame.into() }
    }
}

impl<B> Frame<B> {
    fn protocol(&self) -> Option<u16> {
        let ethertype = match self {
            Self::Sent(SentFrame::Ethernet(frame))
            | Self::Received(ReceivedFrame::Ethernet { destination: _, frame }) => frame.ethertype,
            Self::Sent(SentFrame::Ip(frame)) | Self::Received(ReceivedFrame::Ip(frame)) => {
                Some(frame.ethertype())
            }
        };
        ethertype.map(Into::into)
    }

    /// Convenience method for consuming the `Frame` and producing the body.
    pub fn into_body(self) -> B {
        match self {
            Self::Received(ReceivedFrame::Ethernet { destination: _, frame })
            | Self::Sent(SentFrame::Ethernet(frame)) => frame.body,
            Self::Received(ReceivedFrame::Ip(frame)) | Self::Sent(SentFrame::Ip(frame)) => {
                frame.body
            }
        }
    }
}

impl<
        D: Device,
        BC: DeviceSocketBindingsContext<<CC as DeviceIdContext<AnyDevice>>::DeviceId>,
        CC: DeviceSocketContext<BC> + DeviceIdContext<D>,
    > DeviceSocketHandler<D, BC> for CC
where
    <CC as DeviceIdContext<D>>::DeviceId: Into<<CC as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        frame: Frame<&[u8]>,
        whole_frame: &[u8],
    ) {
        let device = device.clone().into();

        // TODO(https://fxbug.dev/42076496): Invert the order of acquisition
        // for the lock on the sockets held in the device and the any-device
        // sockets lock.
        self.with_any_device_sockets(|AnyDeviceSockets(any_device_sockets), core_ctx| {
            // Iterate through the device's sockets while also holding the
            // any-device sockets lock. This prevents double delivery to the
            // same socket. If the two tables were locked independently,
            // we could end up with a race, with the following thread
            // interleaving (thread A is executing this code for device D,
            // thread B is updating the device to D for the same socket X):
            //   A) lock the any device sockets table
            //   A) deliver to socket X in the table
            //   A) unlock the any device sockets table
            //   B) lock the any device sockets table, then D's sockets
            //   B) remove X from the any table and add to D's
            //   B) unlock D's sockets and any device sockets
            //   A) lock D's sockets
            //   A) deliver to socket X in D's table (!)
            core_ctx.with_device_sockets(&device, |DeviceSockets(device_sockets), core_ctx| {
                for socket in any_device_sockets.iter().chain(device_sockets) {
                    core_ctx.with_socket_state(
                        socket,
                        |external_state, Target { protocol, device: _ }| {
                            let should_deliver = match protocol {
                                None => false,
                                Some(p) => match p {
                                    // Sent frames are only delivered to sockets
                                    // matching all protocols for Linux
                                    // compatibility. See https://github.com/google/gvisor/blob/68eae979409452209e4faaeac12aee4191b3d6f0/test/syscalls/linux/packet_socket.cc#L331-L392.
                                    Protocol::Specific(p) => match frame {
                                        Frame::Received(_) => Some(p.get()) == frame.protocol(),
                                        Frame::Sent(_) => false,
                                    },
                                    Protocol::All => true,
                                },
                            };
                            if should_deliver {
                                bindings_ctx.receive_frame(
                                    external_state,
                                    &device,
                                    frame,
                                    whole_frame,
                                )
                            }
                        },
                    )
                }
            })
        })
    }
}

impl<Id: StrongSocketId> OrderedLockAccess<AnyDeviceSockets<Id>> for Sockets<Id> {
    type Lock = RwLock<AnyDeviceSockets<Id>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.any_device_sockets)
    }
}

impl<Id: StrongSocketId> OrderedLockAccess<AllSockets<Id>> for Sockets<Id> {
    type Lock = Mutex<AllSockets<Id>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.all_sockets)
    }
}

#[cfg(test)]
mod testutil {
    use crate::context::testutil::FakeBindingsCtx;
    use crate::device::DeviceLayerStateTypes;
    use crate::testutil::MonotonicIdentifier;

    use super::*;

    impl<TimerId, Event: Debug, State> DeviceSocketTypes
        for FakeBindingsCtx<TimerId, Event, State, ()>
    {
        type SocketState = ();
    }

    impl<TimerId: Debug + PartialEq + Clone + Send + Sync, Event: Debug, State>
        DeviceLayerStateTypes for FakeBindingsCtx<TimerId, Event, State, ()>
    {
        type EthernetDeviceState = ();
        type LoopbackDeviceState = ();
        type PureIpDeviceState = ();
        type DeviceIdentifier = MonotonicIdentifier;
    }

    impl<TimerId, Event: Debug, State, DeviceId> DeviceSocketBindingsContext<DeviceId>
        for FakeBindingsCtx<TimerId, Event, State, ()>
    {
        fn receive_frame(
            &self,
            _socket: &Self::SocketState,
            _device: &DeviceId,
            _frame: Frame<&[u8]>,
            _raw_frame: &[u8],
        ) {
            unimplemented!()
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::HashMap, rc::Rc, vec, vec::Vec};
    use core::{cell::RefCell, cmp::PartialEq, marker::PhantomData};

    use const_unwrap::const_unwrap_option;
    use derivative::Derivative;
    use packet::ParsablePacket;
    use test_case::test_case;

    use crate::{
        context::{ContextProvider, CtxPair},
        device::{
            testutil::{
                FakeReferencyDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId,
            },
            DeviceIdentifier,
        },
    };

    use super::*;

    impl Frame<&[u8]> {
        pub(crate) fn cloned(self) -> Frame<Vec<u8>> {
            match self {
                Self::Sent(SentFrame::Ethernet(frame)) => {
                    Frame::Sent(SentFrame::Ethernet(frame.cloned()))
                }
                Self::Received(super::ReceivedFrame::Ethernet { destination, frame }) => {
                    Frame::Received(super::ReceivedFrame::Ethernet {
                        destination,
                        frame: frame.cloned(),
                    })
                }
                Self::Sent(SentFrame::Ip(frame)) => Frame::Sent(SentFrame::Ip(frame.cloned())),
                Self::Received(super::ReceivedFrame::Ip(frame)) => {
                    Frame::Received(super::ReceivedFrame::Ip(frame.cloned()))
                }
            }
        }
    }

    impl EthernetFrame<&[u8]> {
        fn cloned(self) -> EthernetFrame<Vec<u8>> {
            let Self { src_mac, dst_mac, ethertype, body } = self;
            EthernetFrame { src_mac, dst_mac, ethertype, body: Vec::from(body) }
        }
    }

    impl IpFrame<&[u8]> {
        fn cloned(self) -> IpFrame<Vec<u8>> {
            let Self { ip_version, body } = self;
            IpFrame { ip_version, body: Vec::from(body) }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct ReceivedFrame<D> {
        device: D,
        frame: Frame<Vec<u8>>,
        raw: Vec<u8>,
    }

    type FakeCoreCtx<D> = crate::context::testutil::FakeCoreCtx<FakeSockets<D>, (), D>;
    type FakeCtx<D> = CtxPair<FakeCoreCtx<D>, FakeBindingsCtx<D>>;
    #[derive(Debug, Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeBindingsCtx<D>(PhantomData<D>);

    impl<D> ContextProvider for FakeBindingsCtx<D> {
        type Context = Self;
        fn context(&mut self) -> &mut Self::Context {
            self
        }
    }

    impl<D: DeviceIdentifier> DeviceSocketTypes for FakeBindingsCtx<D> {
        type SocketState = ExternalSocketState<D>;
    }

    impl<D: DeviceIdentifier> DeviceSocketBindingsContext<D> for FakeBindingsCtx<D> {
        fn receive_frame(
            &self,
            state: &ExternalSocketState<D>,
            device: &D,
            frame: Frame<&[u8]>,
            raw_frame: &[u8],
        ) {
            let ExternalSocketState(queue) = state;
            queue.lock().push(ReceivedFrame {
                device: device.clone(),
                frame: frame.cloned(),
                raw: raw_frame.into(),
            })
        }
    }

    /// A trait providing a shortcut to instantiate a [`DeviceSocketApi`] from a
    /// context.
    trait DeviceSocketApiExt: ContextPair + Sized {
        fn device_socket_api(&mut self) -> DeviceSocketApi<&mut Self> {
            DeviceSocketApi::new(self)
        }
    }

    impl<O> DeviceSocketApiExt for O where O: ContextPair + Sized {}

    #[derive(Debug, Derivative)]
    #[derivative(Default(bound = ""))]
    struct ExternalSocketState<D>(Mutex<Vec<ReceivedFrame<D>>>);

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeSockets<D: FakeStrongDeviceId> {
        any_device_sockets: AnyDeviceSockets<FakeStrongId<D>>,
        device_sockets: HashMap<D, DeviceSockets<FakeStrongId<D>>>,
        all_sockets: AllSockets<FakeStrongId<D>>,
    }

    /// Tuple of references
    struct FakeSocketsMutRefs<'m, AnyDevice, AllSockets, Devices, Device>(
        &'m mut AnyDevice,
        &'m mut AllSockets,
        &'m mut Devices,
        PhantomData<Device>,
    );

    /// Helper trait to allow treating a `&mut self` as a
    /// [`FakeSocketsMutRefs`].
    trait AsFakeSocketsMutRefs {
        type AnyDevice: 'static;
        type AllSockets: 'static;
        type Devices: 'static;
        type Device: 'static;
        fn as_sockets_ref(
            &mut self,
        ) -> FakeSocketsMutRefs<'_, Self::AnyDevice, Self::AllSockets, Self::Devices, Self::Device>;
    }

    impl<D: FakeStrongDeviceId> AsFakeSocketsMutRefs for FakeCoreCtx<D> {
        type AnyDevice = AnyDeviceSockets<FakeStrongId<D>>;
        type AllSockets = AllSockets<FakeStrongId<D>>;
        type Devices = HashMap<D, DeviceSockets<FakeStrongId<D>>>;
        type Device = D;

        fn as_sockets_ref(
            &mut self,
        ) -> FakeSocketsMutRefs<
            '_,
            AnyDeviceSockets<FakeStrongId<D>>,
            AllSockets<FakeStrongId<D>>,
            HashMap<D, DeviceSockets<FakeStrongId<D>>>,
            D,
        > {
            let FakeSockets { any_device_sockets, device_sockets, all_sockets } = &mut self.state;
            FakeSocketsMutRefs(any_device_sockets, all_sockets, device_sockets, PhantomData)
        }
    }

    impl<'m, AnyDevice: 'static, AllSockets: 'static, Devices: 'static, Device: 'static>
        AsFakeSocketsMutRefs for FakeSocketsMutRefs<'m, AnyDevice, AllSockets, Devices, Device>
    {
        type AnyDevice = AnyDevice;
        type AllSockets = AllSockets;
        type Devices = Devices;
        type Device = Device;

        fn as_sockets_ref(
            &mut self,
        ) -> FakeSocketsMutRefs<'_, AnyDevice, AllSockets, Devices, Device> {
            let Self(any_device, all_sockets, devices, PhantomData) = self;
            FakeSocketsMutRefs(any_device, all_sockets, devices, PhantomData)
        }
    }

    impl<As: AsFakeSocketsMutRefs<Device: FakeStrongDeviceId>>
        DeviceSocketContextTypes<FakeBindingsCtx<As::Device>> for As
    {
        type SocketId = FakeStrongId<As::Device>;

        fn new_primary(
            external_state: ExternalSocketState<As::Device>,
        ) -> FakePrimaryId<As::Device> {
            FakePrimaryId(FakeStrongId(Rc::new(RefCell::new(FakeSocketState {
                external_state,
                target: Default::default(),
            }))))
        }

        fn unwrap_primary(primary: FakePrimaryId<As::Device>) -> ExternalSocketState<As::Device> {
            let FakePrimaryId(FakeStrongId(rc)) = primary;
            Rc::try_unwrap(rc)
                .unwrap_or_else(|rc| panic!("can't unwrap primary {rc:?}, strong references exist"))
                .into_inner()
                .external_state
        }
    }

    #[derive(Debug)]
    struct FakeSocketState<D: FakeStrongDeviceId> {
        external_state: ExternalSocketState<D>,
        target: Target<D::Weak>,
    }

    #[derive(Clone, Debug)]
    pub struct FakeStrongId<D: FakeStrongDeviceId>(Rc<RefCell<FakeSocketState<D>>>);

    impl<D: FakeStrongDeviceId> Hash for FakeStrongId<D> {
        fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
            let Self(rc) = self;
            Rc::as_ptr(rc).hash(state)
        }
    }

    impl<D: FakeStrongDeviceId> PartialEq for FakeStrongId<D> {
        fn eq(&self, other: &Self) -> bool {
            let Self(this) = self;
            let Self(other) = other;
            Rc::ptr_eq(this, other)
        }
    }

    impl<D: FakeStrongDeviceId> core::cmp::Eq for FakeStrongId<D> {}

    #[derive(Debug)]
    pub struct FakePrimaryId<D: FakeStrongDeviceId>(FakeStrongId<D>);

    impl<D: FakeStrongDeviceId> StrongSocketId for FakeStrongId<D> {
        type Primary = FakePrimaryId<D>;

        fn clone_strong(primary: &Self::Primary) -> Self {
            let FakePrimaryId(strong) = primary;
            strong.clone()
        }
    }

    impl<D: Clone> TargetDevice<&D> {
        fn with_weak_id(&self) -> TargetDevice<FakeWeakDeviceId<D>> {
            match self {
                TargetDevice::AnyDevice => TargetDevice::AnyDevice,
                TargetDevice::SpecificDevice(d) => {
                    TargetDevice::SpecificDevice(FakeWeakDeviceId((*d).clone()))
                }
            }
        }
    }

    impl<D: Eq + Hash + FakeStrongDeviceId> FakeSockets<D> {
        fn new(devices: impl IntoIterator<Item = D>) -> Self {
            let device_sockets =
                devices.into_iter().map(|d| (d, DeviceSockets::default())).collect();
            Self {
                any_device_sockets: AnyDeviceSockets::default(),
                device_sockets,
                all_sockets: Default::default(),
            }
        }
    }

    impl<
            'm,
            DeviceId: FakeStrongDeviceId,
            As: AsFakeSocketsMutRefs<AllSockets = AllSockets<FakeStrongId<DeviceId>>>
                + DeviceIdContext<AnyDevice, DeviceId = DeviceId, WeakDeviceId = DeviceId::Weak>
                + DeviceSocketContextTypes<
                    FakeBindingsCtx<DeviceId>,
                    SocketId = FakeStrongId<DeviceId>,
                >,
        > SocketStateAccessor<FakeBindingsCtx<DeviceId>> for As
    {
        fn with_socket_state<
            F: FnOnce(&ExternalSocketState<Self::DeviceId>, &Target<Self::WeakDeviceId>) -> R,
            R,
        >(
            &mut self,
            socket: &Self::SocketId,
            cb: F,
        ) -> R {
            let FakeStrongId(rc) = socket;
            let borrow = rc.borrow();
            let FakeSocketState { external_state, target } = &*borrow;
            cb(external_state, target)
        }

        fn with_socket_state_mut<
            F: FnOnce(&ExternalSocketState<Self::DeviceId>, &mut Target<Self::WeakDeviceId>) -> R,
            R,
        >(
            &mut self,
            socket: &Self::SocketId,
            cb: F,
        ) -> R {
            let FakeStrongId(rc) = socket;
            let mut borrow = rc.borrow_mut();
            let FakeSocketState { external_state, target } = &mut *borrow;
            cb(external_state, target)
        }
    }

    impl<
            'm,
            DeviceId: FakeStrongDeviceId,
            As: AsFakeSocketsMutRefs<
                    AllSockets = AllSockets<FakeStrongId<DeviceId>>,
                    Devices = HashMap<DeviceId, DeviceSockets<FakeStrongId<DeviceId>>>,
                > + DeviceIdContext<AnyDevice, DeviceId = DeviceId, WeakDeviceId = DeviceId::Weak>
                + DeviceSocketContextTypes<
                    FakeBindingsCtx<DeviceId>,
                    SocketId = FakeStrongId<DeviceId>,
                >,
        > DeviceSocketAccessor<FakeBindingsCtx<DeviceId>> for As
    {
        type DeviceSocketCoreCtx<'a> = FakeSocketsMutRefs<
            'a,
            As::AnyDevice,
            AllSockets<FakeStrongId<DeviceId>>,
            HashSet<DeviceId>,
            DeviceId,
        >;
        fn with_device_sockets<
            F: FnOnce(&DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
            R,
        >(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            let FakeSocketsMutRefs(any_device, all_sockets, device_sockets, PhantomData) =
                self.as_sockets_ref();
            let mut devices = device_sockets.keys().cloned().collect();
            let device = device_sockets.get(device).unwrap();
            cb(device, &mut FakeSocketsMutRefs(any_device, all_sockets, &mut devices, PhantomData))
        }
        fn with_device_sockets_mut<
            F: FnOnce(&mut DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
            R,
        >(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            let FakeSocketsMutRefs(any_device, all_sockets, device_sockets, PhantomData) =
                self.as_sockets_ref();
            let mut devices = device_sockets.keys().cloned().collect();
            let device = device_sockets.get_mut(device).unwrap();
            cb(device, &mut FakeSocketsMutRefs(any_device, all_sockets, &mut devices, PhantomData))
        }
    }

    impl<
            'm,
            DeviceId: FakeStrongDeviceId,
            As: AsFakeSocketsMutRefs<
                    AnyDevice = AnyDeviceSockets<FakeStrongId<DeviceId>>,
                    AllSockets = AllSockets<FakeStrongId<DeviceId>>,
                    Devices = HashMap<DeviceId, DeviceSockets<FakeStrongId<DeviceId>>>,
                > + DeviceIdContext<AnyDevice, DeviceId = DeviceId, WeakDeviceId = DeviceId::Weak>
                + DeviceSocketContextTypes<
                    FakeBindingsCtx<DeviceId>,
                    SocketId = FakeStrongId<DeviceId>,
                >,
        > DeviceSocketContext<FakeBindingsCtx<DeviceId>> for As
    {
        type SocketTablesCoreCtx<'a> = FakeSocketsMutRefs<
            'a,
            (),
            AllSockets<FakeStrongId<DeviceId>>,
            HashMap<DeviceId, DeviceSockets<FakeStrongId<DeviceId>>>,
            DeviceId,
        >;

        fn with_any_device_sockets<
            F: FnOnce(&AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSocketsMutRefs(any_device_sockets, all_sockets, device_sockets, PhantomData) =
                self.as_sockets_ref();
            cb(
                any_device_sockets,
                &mut FakeSocketsMutRefs(&mut (), all_sockets, device_sockets, PhantomData),
            )
        }
        fn with_any_device_sockets_mut<
            F: FnOnce(&mut AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSocketsMutRefs(any_device_sockets, all_sockets, device_sockets, PhantomData) =
                self.as_sockets_ref();
            cb(
                any_device_sockets,
                &mut FakeSocketsMutRefs(&mut (), all_sockets, device_sockets, PhantomData),
            )
        }

        fn with_all_device_sockets_mut<F: FnOnce(&mut AllSockets<Self::SocketId>) -> R, R>(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSocketsMutRefs(_, all_sockets, _, _) = self.as_sockets_ref();
            cb(all_sockets)
        }
    }

    impl<'m, X, Y, Z, D: FakeStrongDeviceId> DeviceIdContext<AnyDevice>
        for FakeSocketsMutRefs<'m, X, Y, Z, D>
    {
        type DeviceId = D;
        type WeakDeviceId = FakeWeakDeviceId<D>;
    }

    const SOME_PROTOCOL: NonZeroU16 = const_unwrap_option(NonZeroU16::new(2000));

    #[test]
    fn create_remove() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));
        let mut api = ctx.device_socket_api();

        let bound = api.create(Default::default());
        assert_eq!(
            api.get_info(&bound),
            SocketInfo { device: TargetDevice::AnyDevice, protocol: None }
        );

        api.remove(bound);
    }

    #[test_case(TargetDevice::AnyDevice)]
    #[test_case(TargetDevice::SpecificDevice(&MultipleDevicesId::A))]
    fn test_set_device(device: TargetDevice<&MultipleDevicesId>) {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));
        let mut api = ctx.device_socket_api();

        let bound = api.create(Default::default());
        api.set_device(&bound, device.clone());
        assert_eq!(
            api.get_info(&bound),
            SocketInfo { device: device.with_weak_id(), protocol: None }
        );

        let device_sockets = &api.core_ctx().state.device_sockets;
        if let TargetDevice::SpecificDevice(d) = device {
            let DeviceSockets(socket_ids) = device_sockets.get(&d).expect("device state exists");
            assert_eq!(socket_ids, &HashSet::from([bound]));
        }
    }

    #[test]
    fn update_device() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));
        let mut api = ctx.device_socket_api();
        let bound = api.create(Default::default());

        api.set_device(&bound, TargetDevice::SpecificDevice(&MultipleDevicesId::A));

        // Now update the device and make sure the socket only appears in the
        // one device's list.
        api.set_device(&bound, TargetDevice::SpecificDevice(&MultipleDevicesId::B));
        assert_eq!(
            api.get_info(&bound),
            SocketInfo {
                device: TargetDevice::SpecificDevice(FakeWeakDeviceId(MultipleDevicesId::B)),
                protocol: None
            }
        );

        let device_sockets = &api.core_ctx().state.device_sockets;
        let device_socket_lists = device_sockets
            .iter()
            .map(|(d, DeviceSockets(indexes))| (d, indexes.iter().collect()))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            device_socket_lists,
            HashMap::from([
                (&MultipleDevicesId::A, vec![]),
                (&MultipleDevicesId::B, vec![&bound]),
                (&MultipleDevicesId::C, vec![])
            ])
        );
    }

    #[test_case(Protocol::All, TargetDevice::AnyDevice)]
    #[test_case(Protocol::Specific(SOME_PROTOCOL), TargetDevice::AnyDevice)]
    #[test_case(Protocol::All, TargetDevice::SpecificDevice(&MultipleDevicesId::A))]
    #[test_case(
        Protocol::Specific(SOME_PROTOCOL),
        TargetDevice::SpecificDevice(&MultipleDevicesId::A)
    )]
    fn create_set_device_and_protocol_remove_multiple(
        protocol: Protocol,
        device: TargetDevice<&MultipleDevicesId>,
    ) {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));
        let mut api = ctx.device_socket_api();

        let mut sockets = [(); 3].map(|()| api.create(Default::default()));
        for socket in &mut sockets {
            api.set_device_and_protocol(socket, device.clone(), protocol);
            assert_eq!(
                api.get_info(socket),
                SocketInfo { device: device.with_weak_id(), protocol: Some(protocol) }
            );
        }

        for socket in sockets {
            api.remove(socket)
        }
    }

    #[test]
    fn change_device_after_removal() {
        let device_to_remove = FakeReferencyDeviceId::default();
        let device_to_maintain = FakeReferencyDeviceId::default();
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new([
            device_to_remove.clone(),
            device_to_maintain.clone(),
        ])));
        let mut api = ctx.device_socket_api();

        let bound = api.create(Default::default());
        // Set the device for the socket before removing the device state
        // entirely.
        api.set_device(&bound, TargetDevice::SpecificDevice(&device_to_remove));

        // Now remove the device; this should cause future attempts to upgrade
        // the device ID to fail.
        device_to_remove.mark_removed();

        // Changing the device should gracefully handle the fact that the
        // earlier-bound device is now gone.
        api.set_device(&bound, TargetDevice::SpecificDevice(&device_to_maintain));
        assert_eq!(
            api.get_info(&bound),
            SocketInfo {
                device: TargetDevice::SpecificDevice(FakeWeakDeviceId(device_to_maintain.clone())),
                protocol: None,
            }
        );

        let device_sockets = &api.core_ctx().state.device_sockets;
        let DeviceSockets(weak_sockets) =
            device_sockets.get(&device_to_maintain).expect("device state exists");
        assert_eq!(weak_sockets, &HashSet::from([bound]));
    }

    struct TestData;
    impl TestData {
        const SRC_MAC: Mac = Mac::new([0, 1, 2, 3, 4, 5]);
        const DST_MAC: Mac = Mac::new([6, 7, 8, 9, 10, 11]);
        /// Arbitrary protocol number.
        const PROTO: NonZeroU16 = const_unwrap_option(NonZeroU16::new(0x08AB));
        const BODY: &'static [u8] = b"some pig";
        const BUFFER: &'static [u8] = &[
            6, 7, 8, 9, 10, 11, 0, 1, 2, 3, 4, 5, 0x08, 0xAB, b's', b'o', b'm', b'e', b' ', b'p',
            b'i', b'g',
        ];

        /// Creates an EthernetFrame with the values specified above.
        fn frame() -> packet_formats::ethernet::EthernetFrame<&'static [u8]> {
            let mut buffer_view = Self::BUFFER;
            packet_formats::ethernet::EthernetFrame::parse(
                &mut buffer_view,
                EthernetFrameLengthCheck::NoCheck,
            )
            .unwrap()
        }
    }

    const WRONG_PROTO: NonZeroU16 = const_unwrap_option(NonZeroU16::new(0x08ff));

    fn make_bound<D: FakeStrongDeviceId>(
        ctx: &mut FakeCtx<D>,
        device: TargetDevice<D>,
        protocol: Option<Protocol>,
        state: ExternalSocketState<D>,
    ) -> FakeStrongId<D> {
        let mut api = ctx.device_socket_api();
        let id = api.create(state);
        let device = match &device {
            TargetDevice::AnyDevice => TargetDevice::AnyDevice,
            TargetDevice::SpecificDevice(d) => TargetDevice::SpecificDevice(d),
        };
        match protocol {
            Some(protocol) => api.set_device_and_protocol(&id, device, protocol),
            None => api.set_device(&id, device),
        };
        id
    }

    /// Deliver one frame to the provided contexts and return the IDs of the
    /// sockets it was delivered to.
    fn deliver_one_frame(
        delivered_frame: Frame<&[u8]>,
        FakeCtx { mut core_ctx, mut bindings_ctx }: FakeCtx<MultipleDevicesId>,
    ) -> HashSet<FakeStrongId<MultipleDevicesId>> {
        DeviceSocketHandler::handle_frame(
            &mut core_ctx,
            &mut bindings_ctx,
            &MultipleDevicesId::A,
            delivered_frame.clone(),
            TestData::BUFFER,
        );

        let FakeSockets {
            all_sockets: AllSockets(all_sockets),
            any_device_sockets: _,
            device_sockets: _,
        } = core_ctx.into_state();

        all_sockets
            .into_iter()
            .filter_map(|(id, _primary)| {
                let FakeStrongId(rc) = &id;
                let borrow = rc.borrow();
                let ExternalSocketState(frames) = &borrow.external_state;
                let frames = frames.lock();
                (!frames.is_empty()).then(|| {
                    assert_eq!(
                        &*frames,
                        &[ReceivedFrame {
                            device: MultipleDevicesId::A,
                            frame: delivered_frame.cloned(),
                            raw: TestData::BUFFER.into(),
                        }]
                    );
                    id.clone()
                })
            })
            .collect()
    }

    #[test]
    fn receive_frame_deliver_to_multiple() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));

        use Protocol::*;
        use TargetDevice::*;
        let never_bound = {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            ctx.device_socket_api().create(state)
        };

        let mut make_bound = |device, protocol| {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            make_bound(&mut ctx, device, protocol, state)
        };
        let bound_a_no_protocol = make_bound(SpecificDevice(MultipleDevicesId::A), None);
        let bound_a_all_protocols = make_bound(SpecificDevice(MultipleDevicesId::A), Some(All));
        let bound_a_right_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::A), Some(Specific(TestData::PROTO)));
        let bound_a_wrong_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::A), Some(Specific(WRONG_PROTO)));
        let bound_b_no_protocol = make_bound(SpecificDevice(MultipleDevicesId::B), None);
        let bound_b_all_protocols = make_bound(SpecificDevice(MultipleDevicesId::B), Some(All));
        let bound_b_right_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::B), Some(Specific(TestData::PROTO)));
        let bound_b_wrong_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::B), Some(Specific(WRONG_PROTO)));
        let bound_any_no_protocol = make_bound(AnyDevice, None);
        let bound_any_all_protocols = make_bound(AnyDevice, Some(All));
        let bound_any_right_protocol = make_bound(AnyDevice, Some(Specific(TestData::PROTO)));
        let bound_any_wrong_protocol = make_bound(AnyDevice, Some(Specific(WRONG_PROTO)));

        let mut sockets_with_received_frames = deliver_one_frame(
            super::ReceivedFrame::from_ethernet(
                TestData::frame(),
                FrameDestination::Individual { local: true },
            )
            .into(),
            ctx,
        );

        let _ = (
            never_bound,
            bound_a_no_protocol,
            bound_a_wrong_protocol,
            bound_b_no_protocol,
            bound_b_all_protocols,
            bound_b_right_protocol,
            bound_b_wrong_protocol,
            bound_any_no_protocol,
            bound_any_wrong_protocol,
        );

        assert!(sockets_with_received_frames.remove(&bound_a_all_protocols));
        assert!(sockets_with_received_frames.remove(&bound_a_right_protocol));
        assert!(sockets_with_received_frames.remove(&bound_any_all_protocols));
        assert!(sockets_with_received_frames.remove(&bound_any_right_protocol));
        assert!(sockets_with_received_frames.is_empty());
    }

    #[test]
    fn sent_frame_deliver_to_multiple() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));

        use Protocol::*;
        use TargetDevice::*;
        let never_bound = {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            ctx.device_socket_api().create(state)
        };

        let mut make_bound = |device, protocol| {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            make_bound(&mut ctx, device, protocol, state)
        };
        let bound_a_no_protocol = make_bound(SpecificDevice(MultipleDevicesId::A), None);
        let bound_a_all_protocols = make_bound(SpecificDevice(MultipleDevicesId::A), Some(All));
        let bound_a_same_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::A), Some(Specific(TestData::PROTO)));
        let bound_a_wrong_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::A), Some(Specific(WRONG_PROTO)));
        let bound_b_no_protocol = make_bound(SpecificDevice(MultipleDevicesId::B), None);
        let bound_b_all_protocols = make_bound(SpecificDevice(MultipleDevicesId::B), Some(All));
        let bound_b_same_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::B), Some(Specific(TestData::PROTO)));
        let bound_b_wrong_protocol =
            make_bound(SpecificDevice(MultipleDevicesId::B), Some(Specific(WRONG_PROTO)));
        let bound_any_no_protocol = make_bound(AnyDevice, None);
        let bound_any_all_protocols = make_bound(AnyDevice, Some(All));
        let bound_any_same_protocol = make_bound(AnyDevice, Some(Specific(TestData::PROTO)));
        let bound_any_wrong_protocol = make_bound(AnyDevice, Some(Specific(WRONG_PROTO)));

        let mut sockets_with_received_frames =
            deliver_one_frame(SentFrame::Ethernet(TestData::frame().into()).into(), ctx);

        let _ = (
            never_bound,
            bound_a_no_protocol,
            bound_a_same_protocol,
            bound_a_wrong_protocol,
            bound_b_no_protocol,
            bound_b_all_protocols,
            bound_b_same_protocol,
            bound_b_wrong_protocol,
            bound_any_no_protocol,
            bound_any_same_protocol,
            bound_any_wrong_protocol,
        );

        // Only any-protocol sockets receive sent frames.
        assert!(sockets_with_received_frames.remove(&bound_a_all_protocols));
        assert!(sockets_with_received_frames.remove(&bound_any_all_protocols));
        assert!(sockets_with_received_frames.is_empty());
    }

    #[test]
    fn deliver_multiple_frames() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtx::with_state(FakeSockets::new(
            MultipleDevicesId::all(),
        )));
        let socket = make_bound(
            &mut ctx,
            TargetDevice::AnyDevice,
            Some(Protocol::All),
            ExternalSocketState::default(),
        );
        let FakeCtx { mut core_ctx, mut bindings_ctx } = ctx;

        const RECEIVE_COUNT: usize = 10;
        for _ in 0..RECEIVE_COUNT {
            DeviceSocketHandler::handle_frame(
                &mut core_ctx,
                &mut bindings_ctx,
                &MultipleDevicesId::A,
                super::ReceivedFrame::from_ethernet(
                    TestData::frame(),
                    FrameDestination::Individual { local: true },
                )
                .into(),
                TestData::BUFFER,
            );
        }

        let FakeSockets {
            all_sockets: AllSockets(mut all_sockets),
            any_device_sockets: _,
            device_sockets: _,
        } = core_ctx.into_state();
        let primary = all_sockets.remove(&socket).unwrap();
        assert!(all_sockets.is_empty());
        drop(socket);
        let ExternalSocketState(received) =
            <FakeCoreCtx<_> as DeviceSocketContextTypes<_>>::unwrap_primary(primary);
        assert_eq!(
            received.into_inner(),
            vec![
                ReceivedFrame {
                    device: MultipleDevicesId::A,
                    frame: Frame::Received(super::ReceivedFrame::Ethernet {
                        destination: FrameDestination::Individual { local: true },
                        frame: EthernetFrame {
                            src_mac: TestData::SRC_MAC,
                            dst_mac: TestData::DST_MAC,
                            ethertype: Some(TestData::PROTO.get().into()),
                            body: Vec::from(TestData::BODY),
                        }
                    }),
                    raw: TestData::BUFFER.into()
                };
                RECEIVE_COUNT
            ]
        );
    }
}
