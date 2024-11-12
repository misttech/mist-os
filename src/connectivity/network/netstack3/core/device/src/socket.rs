// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Link-layer sockets (analogous to Linux's AF_PACKET sockets).

use alloc::collections::{HashMap, HashSet};
use core::fmt::Debug;
use core::hash::Hash;
use core::num::NonZeroU16;

use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::ethernet::Mac;
use net_types::ip::IpVersion;
use netstack3_base::sync::{Mutex, PrimaryRc, RwLock, StrongRc, WeakRc};
use netstack3_base::{
    AnyDevice, ContextPair, Device, DeviceIdContext, FrameDestination, ReferenceNotifiers,
    ReferenceNotifiersExt as _, RemoveResourceResultWithContext, SendFrameContext,
    SendFrameErrorReason, StrongDeviceIdentifier as _, WeakDeviceIdentifier as _,
};
use packet::{BufferMut, ParsablePacket as _, Serializer};
use packet_formats::error::ParseError;
use packet_formats::ethernet::{EtherType, EthernetFrameLengthCheck};

use crate::internal::base::DeviceLayerTypes;
use crate::internal::id::WeakDeviceId;

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
pub struct PrimaryDeviceSocketId<D, BT: DeviceSocketTypes>(PrimaryRc<SocketState<D, BT>>);

impl<D, BT: DeviceSocketTypes> PrimaryDeviceSocketId<D, BT> {
    /// Creates a new socket ID with `external_state`.
    fn new(external_state: BT::SocketState) -> Self {
        Self(PrimaryRc::new(SocketState { external_state, target: Default::default() }))
    }

    /// Clones the primary's underlying reference and returns as a strong id.
    fn clone_strong(&self) -> DeviceSocketId<D, BT> {
        let PrimaryDeviceSocketId(rc) = self;
        DeviceSocketId(PrimaryRc::clone_strong(rc))
    }
}

/// Reference to live socket state.
///
/// The existence of a `StrongId` attests to the liveness of the state of the
/// backing socket.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""), Eq(bound = ""), PartialEq(bound = ""))]
pub struct DeviceSocketId<D, BT: DeviceSocketTypes>(StrongRc<SocketState<D, BT>>);

impl<D, BT: DeviceSocketTypes> Debug for DeviceSocketId<D, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("DeviceSocketId").field(&StrongRc::debug_id(rc)).finish()
    }
}

impl<D, BT: DeviceSocketTypes> OrderedLockAccess<Target<D>> for DeviceSocketId<D, BT> {
    type Lock = Mutex<Target<D>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        let Self(rc) = self;
        OrderedLockRef::new(&rc.target)
    }
}

/// A weak reference to socket state.
///
/// The existence of a [`WeakSocketDeviceId`] does not attest to the liveness of
/// the backing socket.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""), Eq(bound = ""), PartialEq(bound = ""))]
pub struct WeakDeviceSocketId<D, BT: DeviceSocketTypes>(WeakRc<SocketState<D, BT>>);

impl<D, BT: DeviceSocketTypes> Debug for WeakDeviceSocketId<D, BT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self(rc) = self;
        f.debug_tuple("WeakDeviceSocketId").field(&WeakRc::debug_id(rc)).finish()
    }
}

/// Holds shared state for sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct Sockets<D, BT: DeviceSocketTypes> {
    /// Holds strong (but not owning) references to sockets that aren't
    /// targeting a particular device.
    any_device_sockets: RwLock<AnyDeviceSockets<D, BT>>,

    /// Table of all sockets in the system, regardless of target.
    ///
    /// Holds the primary (owning) reference for all sockets.
    // This needs to be after `any_device_sockets` so that when an instance of
    // this type is dropped, any strong IDs get dropped before their
    // corresponding primary IDs.
    all_sockets: Mutex<AllSockets<D, BT>>,
}

/// The set of sockets associated with a device.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct AnyDeviceSockets<D, BT: DeviceSocketTypes>(HashSet<DeviceSocketId<D, BT>>);

/// A collection of all device sockets in the system.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct AllSockets<D, BT: DeviceSocketTypes>(
    HashMap<DeviceSocketId<D, BT>, PrimaryDeviceSocketId<D, BT>>,
);

/// State held by a device socket.
#[derive(Debug)]
pub struct SocketState<D, BT: DeviceSocketTypes> {
    /// State provided by bindings that is held in core.
    pub external_state: BT::SocketState,
    /// The socket's target device and protocol.
    // TODO(https://fxbug.dev/42077026): Consider splitting up the state here to
    // improve performance.
    target: Mutex<Target<D>>,
}

/// A device socket's binding information.
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
#[cfg_attr(test, derivative(Debug, PartialEq(bound = "BT::SocketState: Hash + Eq, D: Hash + Eq")))]
pub struct DeviceSockets<D, BT: DeviceSocketTypes>(HashSet<DeviceSocketId<D, BT>>);

/// Convenience alias for use in device state storage.
pub type HeldDeviceSockets<BT> = DeviceSockets<WeakDeviceId<BT>, BT>;

/// Convenience alias for use in shared storage.
///
/// The type parameter is expected to implement [`DeviceSocketTypes`].
pub type HeldSockets<BT> = Sockets<WeakDeviceId<BT>, BT>;

/// Core context for accessing socket state.
pub trait DeviceSocketContext<BT: DeviceSocketTypes>: DeviceSocketAccessor<BT> {
    /// The core context available in callbacks to methods on this context.
    type SocketTablesCoreCtx<'a>: DeviceSocketAccessor<
        BT,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Executes the provided callback with mutable access to the collection of
    /// all sockets.
    fn with_all_device_sockets_mut<F: FnOnce(&mut AllSockets<Self::WeakDeviceId, BT>) -> R, R>(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with immutable access to socket state.
    fn with_any_device_sockets<
        F: FnOnce(&AnyDeviceSockets<Self::WeakDeviceId, BT>, &mut Self::SocketTablesCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with mutable access to socket state.
    fn with_any_device_sockets_mut<
        F: FnOnce(
            &mut AnyDeviceSockets<Self::WeakDeviceId, BT>,
            &mut Self::SocketTablesCoreCtx<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;
}

/// Core context for accessing the state of an individual socket.
pub trait SocketStateAccessor<BT: DeviceSocketTypes>: DeviceIdContext<AnyDevice> {
    /// Provides read-only access to the state of a socket.
    fn with_socket_state<F: FnOnce(&BT::SocketState, &Target<Self::WeakDeviceId>) -> R, R>(
        &mut self,
        socket: &DeviceSocketId<Self::WeakDeviceId, BT>,
        cb: F,
    ) -> R;

    /// Provides mutable access to the state of a socket.
    fn with_socket_state_mut<F: FnOnce(&BT::SocketState, &mut Target<Self::WeakDeviceId>) -> R, R>(
        &mut self,
        socket: &DeviceSocketId<Self::WeakDeviceId, BT>,
        cb: F,
    ) -> R;
}

/// Core context for accessing the socket state for a device.
pub trait DeviceSocketAccessor<BT: DeviceSocketTypes>: SocketStateAccessor<BT> {
    /// Core context available in callbacks to methods on this context.
    type DeviceSocketCoreCtx<'a>: SocketStateAccessor<
        BT,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Executes the provided callback with immutable access to device-specific
    /// socket state.
    fn with_device_sockets<
        F: FnOnce(&DeviceSockets<Self::WeakDeviceId, BT>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;

    /// Executes the provided callback with mutable access to device-specific
    /// socket state.
    fn with_device_sockets_mut<
        F: FnOnce(&mut DeviceSockets<Self::WeakDeviceId, BT>, &mut Self::DeviceSocketCoreCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;
}

enum MaybeUpdate<T> {
    NoChange,
    NewValue(T),
}

fn update_device_and_protocol<CC: DeviceSocketContext<BT>, BT: DeviceSocketTypes>(
    core_ctx: &mut CC,
    socket: &DeviceSocketId<CC::WeakDeviceId, BT>,
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
    /// Creates a new `DeviceSocketApi` for `ctx`.
    pub fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

/// A local alias for [`TcpSocketId`] for use in [`TcpApi`].
///
/// TODO(https://github.com/rust-lang/rust/issues/8995): Make this an inherent
/// associated type.
type ApiSocketId<C> = DeviceSocketId<
    <<C as ContextPair>::CoreContext as DeviceIdContext<AnyDevice>>::WeakDeviceId,
    <C as ContextPair>::BindingsContext,
>;

impl<C> DeviceSocketApi<C>
where
    C: ContextPair,
    C::CoreContext: DeviceSocketContext<C::BindingsContext>,
    C::BindingsContext: DeviceSocketBindingsContext<<C::CoreContext as DeviceIdContext<AnyDevice>>::DeviceId>
        + ReferenceNotifiers
        + 'static,
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
            let primary = PrimaryDeviceSocketId::new(external_state);
            let strong = primary.clone_strong();
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
    pub fn remove(
        &mut self,
        id: ApiSocketId<C>,
    ) -> RemoveResourceResultWithContext<
        <C::BindingsContext as DeviceSocketTypes>::SocketState,
        C::BindingsContext,
    > {
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

            let PrimaryDeviceSocketId(primary) = primary;
            C::BindingsContext::unwrap_or_notify_with_new_reference_notifier(
                primary,
                |SocketState { external_state, target: _ }| external_state,
            )
        })
    }

    /// Sends a frame for the specified socket.
    pub fn send_frame<S, D>(
        &mut self,
        _id: &ApiSocketId<C>,
        metadata: DeviceSocketMetadata<D, <C::CoreContext as DeviceIdContext<D>>::DeviceId>,
        body: S,
    ) -> Result<(), SendFrameErrorReason>
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
        core_ctx.send_frame(bindings_ctx, metadata, body).map_err(|e| e.into_err())
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
pub type SocketId<BC> = DeviceSocketId<WeakDeviceId<BC>, BC>;

impl<D, BT: DeviceSocketTypes> DeviceSocketId<D, BT> {
    /// Provides immutable access to [`DeviceSocketTypes::SocketState`] for the
    /// socket.
    pub fn socket_state(&self) -> &BT::SocketState {
        let Self(strong) = self;
        let SocketState { external_state, target: _ } = &**strong;
        external_state
    }

    /// Obtain a [`WeakDeviceSocketId`] from this [`DeviceSocketId`].
    pub fn downgrade(&self) -> WeakDeviceSocketId<D, BT> {
        let Self(inner) = self;
        WeakDeviceSocketId(StrongRc::downgrade(inner))
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
    pub fn try_parse_as_ethernet(mut buf: &[u8]) -> Result<SentFrame<&[u8]>, ParseSentFrameError> {
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
    /// Returns ether type for the packet if it's known.
    pub fn protocol(&self) -> Option<u16> {
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

impl<D, BT: DeviceSocketTypes> OrderedLockAccess<AnyDeviceSockets<D, BT>> for Sockets<D, BT> {
    type Lock = RwLock<AnyDeviceSockets<D, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.any_device_sockets)
    }
}

impl<D, BT: DeviceSocketTypes> OrderedLockAccess<AllSockets<D, BT>> for Sockets<D, BT> {
    type Lock = Mutex<AllSockets<D, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.all_sockets)
    }
}

#[cfg(any(test, feature = "testutils"))]
mod testutil {
    use core::num::NonZeroU64;

    use netstack3_base::testutil::{FakeBindingsCtx, MonotonicIdentifier};

    use super::*;
    use crate::internal::base::{
        DeviceClassMatcher, DeviceIdAndNameMatcher, DeviceLayerStateTypes,
    };

    impl<TimerId, Event: Debug, State> DeviceSocketTypes
        for FakeBindingsCtx<TimerId, Event, State, ()>
    {
        type SocketState = ();
    }

    impl<
            TimerId: Debug + PartialEq + Clone + Send + Sync + 'static,
            Event: Debug + 'static,
            State: 'static,
        > DeviceLayerStateTypes for FakeBindingsCtx<TimerId, Event, State, ()>
    {
        type EthernetDeviceState = ();
        type LoopbackDeviceState = ();
        type PureIpDeviceState = ();
        type DeviceIdentifier = MonotonicIdentifier;
    }

    impl DeviceClassMatcher<()> for () {
        fn device_class_matches(&self, (): &()) -> bool {
            unimplemented!()
        }
    }

    impl DeviceIdAndNameMatcher for MonotonicIdentifier {
        fn id_matches(&self, _id: &NonZeroU64) -> bool {
            unimplemented!()
        }

        fn name_matches(&self, _name: &str) -> bool {
            unimplemented!()
        }
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
    use alloc::collections::HashMap;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cmp::PartialEq;
    use core::marker::PhantomData;

    use const_unwrap::const_unwrap_option;
    use core::convert::Infallible as Never;
    use derivative::Derivative;
    use netstack3_base::sync::DynDebugReferences;
    use netstack3_base::testutil::{
        FakeReferencyDeviceId, FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId,
    };
    use netstack3_base::{ContextProvider, CtxPair, DeviceIdentifier};
    use packet::ParsablePacket;
    use test_case::test_case;

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

    type FakeCoreCtx<D> = netstack3_base::testutil::FakeCoreCtx<FakeSockets<D>, (), D>;
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

    impl<D> ReferenceNotifiers for FakeBindingsCtx<D> {
        type ReferenceReceiver<T: 'static> = Never;

        type ReferenceNotifier<T: Send + 'static> = Never;

        fn new_reference_notifier<T: Send + 'static>(
            debug_references: DynDebugReferences,
        ) -> (Self::ReferenceNotifier<T>, Self::ReferenceReceiver<T>) {
            panic!("device socket removal unexpectedly deferred in test: {debug_references:?}");
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
        any_device_sockets: AnyDeviceSockets<D::Weak, FakeBindingsCtx<D>>,
        device_sockets: HashMap<D, DeviceSockets<D::Weak, FakeBindingsCtx<D>>>,
        all_sockets: AllSockets<D::Weak, FakeBindingsCtx<D>>,
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
        type AnyDevice = AnyDeviceSockets<D::Weak, FakeBindingsCtx<D>>;
        type AllSockets = AllSockets<D::Weak, FakeBindingsCtx<D>>;
        type Devices = HashMap<D, DeviceSockets<D::Weak, FakeBindingsCtx<D>>>;
        type Device = D;

        fn as_sockets_ref(
            &mut self,
        ) -> FakeSocketsMutRefs<
            '_,
            AnyDeviceSockets<D::Weak, FakeBindingsCtx<D>>,
            AllSockets<D::Weak, FakeBindingsCtx<D>>,
            HashMap<D, DeviceSockets<D::Weak, FakeBindingsCtx<D>>>,
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
            As: AsFakeSocketsMutRefs<
                    AllSockets = AllSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
                > + DeviceIdContext<AnyDevice, DeviceId = DeviceId, WeakDeviceId = DeviceId::Weak>,
        > SocketStateAccessor<FakeBindingsCtx<DeviceId>> for As
    {
        fn with_socket_state<
            F: FnOnce(&ExternalSocketState<Self::DeviceId>, &Target<Self::WeakDeviceId>) -> R,
            R,
        >(
            &mut self,
            socket: &DeviceSocketId<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>,
            cb: F,
        ) -> R {
            let DeviceSocketId(rc) = socket;
            // NB: Circumvent lock ordering for tests.
            let target = rc.target.lock();
            cb(&rc.external_state, &target)
        }

        fn with_socket_state_mut<
            F: FnOnce(&ExternalSocketState<Self::DeviceId>, &mut Target<Self::WeakDeviceId>) -> R,
            R,
        >(
            &mut self,
            socket: &DeviceSocketId<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>,
            cb: F,
        ) -> R {
            let DeviceSocketId(rc) = socket;
            // NB: Circumvent lock ordering for tests.
            let mut target = rc.target.lock();
            cb(&rc.external_state, &mut target)
        }
    }

    impl<
            'm,
            DeviceId: FakeStrongDeviceId,
            As: AsFakeSocketsMutRefs<
                    AllSockets = AllSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
                    Devices = HashMap<
                        DeviceId,
                        DeviceSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
                    >,
                > + DeviceIdContext<AnyDevice, DeviceId = DeviceId, WeakDeviceId = DeviceId::Weak>,
        > DeviceSocketAccessor<FakeBindingsCtx<DeviceId>> for As
    {
        type DeviceSocketCoreCtx<'a> = FakeSocketsMutRefs<
            'a,
            As::AnyDevice,
            AllSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
            HashSet<DeviceId>,
            DeviceId,
        >;
        fn with_device_sockets<
            F: FnOnce(
                &DeviceSockets<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>,
                &mut Self::DeviceSocketCoreCtx<'_>,
            ) -> R,
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
            F: FnOnce(
                &mut DeviceSockets<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>,
                &mut Self::DeviceSocketCoreCtx<'_>,
            ) -> R,
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
                    AnyDevice = AnyDeviceSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
                    AllSockets = AllSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
                    Devices = HashMap<
                        DeviceId,
                        DeviceSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
                    >,
                > + DeviceIdContext<AnyDevice, DeviceId = DeviceId, WeakDeviceId = DeviceId::Weak>,
        > DeviceSocketContext<FakeBindingsCtx<DeviceId>> for As
    {
        type SocketTablesCoreCtx<'a> = FakeSocketsMutRefs<
            'a,
            (),
            AllSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>,
            HashMap<DeviceId, DeviceSockets<DeviceId::Weak, FakeBindingsCtx<DeviceId>>>,
            DeviceId,
        >;

        fn with_any_device_sockets<
            F: FnOnce(
                &AnyDeviceSockets<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>,
                &mut Self::SocketTablesCoreCtx<'_>,
            ) -> R,
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
            F: FnOnce(
                &mut AnyDeviceSockets<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>,
                &mut Self::SocketTablesCoreCtx<'_>,
            ) -> R,
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

        fn with_all_device_sockets_mut<
            F: FnOnce(&mut AllSockets<Self::WeakDeviceId, FakeBindingsCtx<Self::DeviceId>>) -> R,
            R,
        >(
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

        let ExternalSocketState(_received_frames) = api.remove(bound).into_removed();
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
            let ExternalSocketState(_received_frames) = api.remove(socket).into_removed();
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
    ) -> DeviceSocketId<D::Weak, FakeBindingsCtx<D>> {
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
        FakeCtx { core_ctx, bindings_ctx }: &mut FakeCtx<MultipleDevicesId>,
    ) -> HashSet<
        DeviceSocketId<FakeWeakDeviceId<MultipleDevicesId>, FakeBindingsCtx<MultipleDevicesId>>,
    > {
        DeviceSocketHandler::handle_frame(
            core_ctx,
            bindings_ctx,
            &MultipleDevicesId::A,
            delivered_frame.clone(),
            TestData::BUFFER,
        );

        let FakeSockets {
            all_sockets: AllSockets(all_sockets),
            any_device_sockets: _,
            device_sockets: _,
        } = &core_ctx.state;

        all_sockets
            .iter()
            .filter_map(|(id, _primary)| {
                let DeviceSocketId(rc) = &id;
                let ExternalSocketState(frames) = &rc.external_state;
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
            &mut ctx,
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
            deliver_one_frame(SentFrame::Ethernet(TestData::frame().into()).into(), &mut ctx);

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
        let PrimaryDeviceSocketId(primary) = primary;
        assert!(all_sockets.is_empty());
        drop(socket);
        let SocketState { external_state: ExternalSocketState(received), target: _ } =
            PrimaryRc::unwrap(primary);
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
