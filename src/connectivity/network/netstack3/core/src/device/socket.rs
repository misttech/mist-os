// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Link-layer sockets (analogous to Linux's AF_PACKET sockets).

use alloc::collections::HashSet;
use core::{fmt::Debug, hash::Hash, num::NonZeroU16};

use derivative::Derivative;
use lock_order::{
    lock::{LockFor, RwLockFor},
    relation::LockBefore,
    Locked,
};
use net_types::ethernet::Mac;
use packet::{BufferMut, Serializer};
use packet_formats::ethernet::{EtherType, EthernetFrame};

use crate::{
    context::SendFrameContext,
    data_structures::id_map::{EntryKey, IdMap},
    device::{
        with_ethernet_state_and_sync_ctx, with_loopback_state_and_sync_ctx, AnyDevice, Device,
        DeviceId, DeviceIdContext, FrameDestination, WeakDeviceId,
    },
    sync::{Mutex, PrimaryRc, RwLock, StrongRc},
    SyncCtx,
};

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

/// Provides associated types for device sockets provided by the non-sync
/// context.
pub trait DeviceSocketTypes {
    /// State for the socket held by core and exposed to bindings.
    type SocketState: Send + Sync + Debug;
}

/// Non-sync context for packet sockets.
pub trait NonSyncContext<DeviceId>: DeviceSocketTypes {
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
pub(crate) struct PrimaryId<S, D>(PrimaryRc<SocketState<S, D>>);

/// Reference to live socket state.
///
/// The existence of a `StrongId` attests to the liveness of the state of the
/// backing socket.
#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub(crate) struct StrongId<S, D>(StrongRc<SocketState<S, D>>);

impl<S, D> PartialEq for StrongId<S, D> {
    fn eq(&self, StrongId(other): &Self) -> bool {
        let Self(strong) = self;
        StrongRc::ptr_eq(strong, other)
    }
}

impl<S, D> Eq for StrongId<S, D> {}

impl<S, D> EntryKey for StrongId<S, D> {
    fn get_key_index(&self) -> usize {
        let Self(strong) = self;
        let SocketState { external_state: _, all_sockets_index, target: _ } = &**strong;
        *all_sockets_index
    }
}

trait StrongSocketId {
    type Primary;
}

impl<S, D> StrongSocketId for StrongId<S, D> {
    type Primary = PrimaryId<S, D>;
}

/// Holds shared state for sockets.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(super) struct Sockets<Primary, Strong> {
    /// Holds strong (but not owning) references to sockets that aren't
    /// targeting a particular device.
    any_device_sockets: RwLock<AnyDeviceSockets<Strong>>,

    /// Table of all sockets in the system, regardless of target.
    ///
    /// Holds the primary (owning) reference for all sockets.
    // This needs to be after `any_device_sockets` so that when an instance of
    // this type is dropped, any strong IDs get dropped before their
    // corresponding primary IDs.
    all_sockets: Mutex<AllSockets<Primary>>,
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct AnyDeviceSockets<Id>(HashSet<Id>);

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct AllSockets<Id>(IdMap<Id>);

#[derive(Debug)]
struct SocketState<S, D> {
    /// The index into `Sockets::all_sockets` for this state.
    all_sockets_index: usize,
    /// State provided by bindings that is held in core.
    external_state: S,
    /// The socket's target device and protocol.
    // TODO(https://fxbug.dev/126263): Consider splitting up the state here to
    // improve performance.
    target: Mutex<Target<D>>,
}

#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
struct Target<D> {
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
pub(crate) struct DeviceSockets<Id>(HashSet<Id>);

/// Convenience alias for use in device state storage.
pub(super) type HeldDeviceSockets<C> =
    DeviceSockets<StrongId<<C as DeviceSocketTypes>::SocketState, WeakDeviceId<C>>>;

/// Convenience alias for use in shared storage.
///
/// The type parameter is expected to implement [`crate::NonSyncContext`].
pub(super) type HeldSockets<C> = Sockets<
    PrimaryId<<C as DeviceSocketTypes>::SocketState, WeakDeviceId<C>>,
    StrongId<<C as DeviceSocketTypes>::SocketState, WeakDeviceId<C>>,
>;

/// Common types across all synchronized context traits for device sockets.
trait SyncContextTypes {
    /// The strongly-owning socket ID type.
    ///
    /// This type is held in various data structures and its existence
    /// indicates liveness of socket state, but not ownership.
    type SocketId: Clone + Debug + Eq + Hash + StrongSocketId;
}

/// Synchronzied context for accessing socket state.
trait SyncContext<C: NonSyncContext<Self::DeviceId>>:
    SyncContextTypes + DeviceIdContext<AnyDevice>
{
    /// The synchronized context available in callbacks to methods on this
    /// context.
    type SocketTablesSyncCtx<'a>: DeviceSocketAccessor<
        C,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
        SocketId = Self::SocketId,
    >;

    /// Creates a new socket with the given external state.
    ///
    /// The ID returned by this method must be removed by calling
    /// [`SyncContext::remove_socket`] once it is no longer in use.
    fn create_socket(&mut self, state: C::SocketState) -> Self::SocketId;

    /// Removes a socket.
    ///
    /// # Panics
    ///
    /// This will panic if the provided socket ID is not the last instance.
    fn remove_socket(&mut self, socket: Self::SocketId);

    /// Executes the provided callback with immutable access to socket state.
    fn with_any_device_sockets<
        F: FnOnce(&AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with mutable access to socket state.
    fn with_any_device_sockets_mut<
        F: FnOnce(&mut AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback without accessing socket state.
    ///
    /// Ideally this would not exist and [`DeviceSocketAccessor`] would be a
    /// supertrait of [`SyncContext`]. That makes writing the test
    /// implementations of the traits difficult since it requires `O(N^2)` total
    /// trait-type impls.
    // TODO(https://fxbug.dev/125732): Determine whether generic test impls of
    // this and the other traits can be used to allow stacking.
    fn without_sockets<R>(&mut self, cb: impl FnOnce(&mut Self::SocketTablesSyncCtx<'_>) -> R)
        -> R;
}

/// Synchronized context for accessing the state of an individual socket.
trait SocketStateAccessor<C: NonSyncContext<Self::DeviceId>>:
    SyncContextTypes + DeviceIdContext<AnyDevice>
{
    /// Synchronized context available in callbacks to methods on this context.
    type SocketStateSyncCtx<'a>: DeviceIdContext<
        AnyDevice,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Provides read-only access to the state of a socket.
    fn with_socket_state<
        F: FnOnce(
            &C::SocketState,
            &Target<Self::WeakDeviceId>,
            &mut Self::SocketStateSyncCtx<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        socket: &Self::SocketId,
        cb: F,
    ) -> R;

    /// Provides mutable access to the state of a socket.
    fn with_socket_state_mut<
        F: FnOnce(
            &C::SocketState,
            &mut Target<Self::WeakDeviceId>,
            &mut Self::SocketStateSyncCtx<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        socket: &Self::SocketId,
        cb: F,
    ) -> R;
}

/// Synchronized context for accessing the socket state for a device.
trait DeviceSocketAccessor<C: NonSyncContext<Self::DeviceId>>:
    SyncContextTypes + DeviceIdContext<AnyDevice>
{
    /// Synchronized context available in callbacks to methods on this context.
    type DeviceSocketSyncCtx<'a>: SocketStateAccessor<
        C,
        SocketId = Self::SocketId,
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Executes the provided callback with immutable access to device-specific
    /// socket state.
    fn with_device_sockets<
        F: FnOnce(&DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;

    // Executes the provided callback with mutable access to device-specific
    // socket state.
    fn with_device_sockets_mut<
        F: FnOnce(&mut DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;

    /// Executes the provided callback without accessing device socket state.
    ///
    /// See [`SyncContext::without_sockets`] for why this method exists.
    fn without_device_sockets<R>(
        &mut self,
        cb: impl FnOnce(&mut Self::DeviceSocketSyncCtx<'_>) -> R,
    ) -> R;
}

/// Internal implementation trait that allows abstracting over device ID types.
trait SocketHandler<C: NonSyncContext<Self::DeviceId>>:
    SyncContextTypes + DeviceIdContext<AnyDevice>
{
    /// Creates a new packet socket.
    fn create(&mut self, external_state: C::SocketState) -> Self::SocketId;

    /// Sets the device for a packet socket without affecting the protocol.
    fn set_device(&mut self, socket: &Self::SocketId, device: TargetDevice<&Self::DeviceId>);

    /// Sets both the device and protocol for a packet socket.
    fn set_device_and_protocol(
        &mut self,
        id: &Self::SocketId,
        device: TargetDevice<&Self::DeviceId>,
        protocol: Protocol,
    );

    /// Gets information about a socket.
    fn get_info(&mut self, id: &Self::SocketId) -> SocketInfo<Self::WeakDeviceId>;

    /// Removes a packet socket.
    fn remove(&mut self, id: Self::SocketId);
}

/// An error encountered when sending a frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SendFrameError {
    /// The socket is not bound to a device and no egress device was specified.
    NoDevice,
    /// The device failed to send the frame.
    SendFailed,
}

/// An error encountered when sending a datagram.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SendDatagramError {
    /// There was a problem sending the constructed frame.
    Frame(SendFrameError),
    /// No protocol number was provided.
    NoProtocol,
}

/// The destination to use when sending a datagram.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SendDatagramParams<D> {
    /// The frame-level sending parameters.
    pub frame: SendFrameParams<D>,
    /// The protocol to use, or `None` to use the socket's bound protocol.
    pub protocol: Option<NonZeroU16>,
    /// The destination address.
    pub dest_addr: Mac,
}

/// The destination to use when sending a frame.
#[derive(Debug, Clone, Derivative, Eq, PartialEq)]
#[derivative(Default(bound = ""))]
pub struct SendFrameParams<D> {
    /// The egress device, or `None` to use the socket's bound device.
    pub device: Option<D>,
}

trait BufferSocketSendHandler<B: BufferMut, C: NonSyncContext<Self::DeviceId>>:
    SocketHandler<C>
{
    /// Sends a frame exactly as provided, or returns an error.
    fn send_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        socket: &Self::SocketId,
        params: SendFrameParams<Self::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendFrameError)>;

    /// Sends a datagram with a constructed link-layer header or returns an
    /// error.
    fn send_datagram<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        socket: &Self::SocketId,
        params: SendDatagramParams<Self::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendDatagramError)>;
}

enum MaybeUpdate<T> {
    NoChange,
    NewValue(T),
}

fn update_device_and_protocol<SC: SyncContext<C>, C: NonSyncContext<SC::DeviceId>>(
    sync_ctx: &mut SC,
    socket: &SC::SocketId,
    new_device: TargetDevice<&SC::DeviceId>,
    protocol_update: MaybeUpdate<Protocol>,
) {
    sync_ctx.with_any_device_sockets_mut(|AnyDeviceSockets(any_device_sockets), sync_ctx| {
        // Even if we're never moving the socket from/to the any-device
        // state, we acquire the lock to make the move between devices
        // atomic from the perspective of frame delivery. Otherwise there
        // would be a brief period during which arriving frames wouldn't be
        // delivered to the socket from either device.
        let old_device = sync_ctx.without_device_sockets(|sync_ctx| {
            sync_ctx.with_socket_state_mut(
                socket,
                |_: &C::SocketState, Target { protocol, device }, sync_ctx| {
                    match protocol_update {
                        MaybeUpdate::NewValue(p) => *protocol = Some(p),
                        MaybeUpdate::NoChange => (),
                    };
                    let old_device = match &device {
                        TargetDevice::SpecificDevice(device) => {
                            sync_ctx.upgrade_weak_device_id(device)
                        }
                        TargetDevice::AnyDevice => {
                            assert!(any_device_sockets.remove(socket));
                            None
                        }
                    };
                    *device = match &new_device {
                        TargetDevice::AnyDevice => TargetDevice::AnyDevice,
                        TargetDevice::SpecificDevice(d) => {
                            TargetDevice::SpecificDevice(sync_ctx.downgrade_device_id(d))
                        }
                    };
                    old_device
                },
            )
        });

        // This modification occurs without holding the socket's individual
        // lock. That's safe because all modifications to the socket's
        // device are done within a `with_sockets_mut` call, which
        // synchronizes them.

        if let Some(device) = old_device {
            // Remove the reference to the socket from the old device if
            // there is one, and it hasn't been removed.
            sync_ctx.with_device_sockets_mut(
                &device,
                |DeviceSockets(device_sockets), _sync_ctx| {
                    assert!(device_sockets.remove(socket), "socket not found in device state");
                },
            );
        }

        // Add the reference to the new device, if there is one.
        match &new_device {
            TargetDevice::SpecificDevice(new_device) => sync_ctx.with_device_sockets_mut(
                new_device,
                |DeviceSockets(device_sockets), _sync_ctx| {
                    assert!(device_sockets.insert(socket.clone()));
                },
            ),
            TargetDevice::AnyDevice => {
                assert!(any_device_sockets.insert(socket.clone()))
            }
        }
    })
}

impl<SC: SyncContext<C>, C: NonSyncContext<SC::DeviceId>> SocketHandler<C> for SC {
    fn create(&mut self, external_state: C::SocketState) -> Self::SocketId {
        let strong = self.create_socket(external_state);
        self.with_any_device_sockets_mut(|AnyDeviceSockets(any_device_sockets), _sync_ctx| {
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

    fn set_device(&mut self, socket: &Self::SocketId, device: TargetDevice<&SC::DeviceId>) {
        update_device_and_protocol(self, socket, device, MaybeUpdate::NoChange)
    }

    fn set_device_and_protocol(
        &mut self,
        socket: &Self::SocketId,
        device: TargetDevice<&SC::DeviceId>,
        protocol: Protocol,
    ) {
        update_device_and_protocol(self, socket, device, MaybeUpdate::NewValue(protocol))
    }

    fn get_info(&mut self, id: &Self::SocketId) -> SocketInfo<Self::WeakDeviceId> {
        self.without_sockets(|sync_ctx| {
            sync_ctx.without_device_sockets(|sync_ctx| {
                sync_ctx.with_socket_state(
                    id,
                    |_external_state, Target { device, protocol }, _sync_ctx| SocketInfo {
                        device: device.clone(),
                        protocol: *protocol,
                    },
                )
            })
        })
    }

    fn remove(&mut self, id: Self::SocketId) {
        self.with_any_device_sockets_mut(|AnyDeviceSockets(any_device_sockets), sync_ctx| {
            let old_device = sync_ctx.without_device_sockets(|sync_ctx| {
                sync_ctx.with_socket_state_mut(&id, |_: &C::SocketState, target, sync_ctx| {
                    let Target { device, protocol: _ } = target;
                    match &device {
                        TargetDevice::SpecificDevice(device) => {
                            sync_ctx.upgrade_weak_device_id(device)
                        }
                        TargetDevice::AnyDevice => {
                            assert!(any_device_sockets.remove(&id));
                            None
                        }
                    }
                })
            });
            if let Some(device) = old_device {
                sync_ctx.with_device_sockets_mut(
                    &device,
                    |DeviceSockets(device_sockets), _sync_ctx| {
                        assert!(device_sockets.remove(&id), "device doesn't have socket");
                    },
                )
            }
        });

        self.remove_socket(id)
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct DeviceSocketMetadata<D> {
    pub(super) device_id: D,
    pub(super) header: Option<DatagramHeader>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct DatagramHeader {
    pub(super) dest_addr: Mac,
    pub(super) protocol: EtherType,
}

impl<
        B: BufferMut,
        C: NonSyncContext<SC::DeviceId>,
        SC: SyncContext<C> + SendFrameContext<C, B, DeviceSocketMetadata<SC::DeviceId>>,
    > BufferSocketSendHandler<B, C> for SC
{
    fn send_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        id: &Self::SocketId,
        params: SendFrameParams<SC::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendFrameError)> {
        let metadata = match self.without_sockets(|sync_ctx| {
            sync_ctx.without_device_sockets(|sync_ctx| {
                sync_ctx.with_socket_state(id, |_: &C::SocketState, target, sync_ctx| {
                    make_send_metadata(sync_ctx, target, params, None)
                })
            })
        }) {
            Ok(metadata) => metadata,
            Err(e) => return Err((body, e)),
        };
        self.send_frame(ctx, metadata, body).map_err(|s| (s, SendFrameError::SendFailed))
    }

    fn send_datagram<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        id: &Self::SocketId,
        params: SendDatagramParams<Self::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendDatagramError)> {
        let metadata = match self.without_sockets(|sync_ctx| {
            sync_ctx.without_device_sockets(|sync_ctx| {
                sync_ctx.with_socket_state(
                    id,
                    |_: &C::SocketState, target @ Target { device: _, protocol }, sync_ctx| {
                        let SendDatagramParams { frame, protocol: target_protocol, dest_addr } =
                            params;
                        let protocol = match target_protocol.or_else(|| {
                            protocol.and_then(|p| match p {
                                Protocol::Specific(p) => Some(p),
                                Protocol::All => None,
                            })
                        }) {
                            None => return Err(SendDatagramError::NoProtocol),
                            Some(p) => p,
                        };

                        make_send_metadata(
                            sync_ctx,
                            target,
                            frame,
                            Some(DatagramHeader {
                                dest_addr,
                                protocol: EtherType::from(protocol.get()),
                            }),
                        )
                        .map_err(SendDatagramError::Frame)
                    },
                )
            })
        }) {
            Ok(metadata) => metadata,
            Err(e) => return Err((body, e)),
        };
        self.send_frame(ctx, metadata, body)
            .map_err(|s| (s, SendDatagramError::Frame(SendFrameError::SendFailed)))
    }
}

fn make_send_metadata<SC: DeviceIdContext<AnyDevice>>(
    sync_ctx: &mut SC,
    bound: &Target<SC::WeakDeviceId>,
    params: SendFrameParams<<SC as DeviceIdContext<AnyDevice>>::DeviceId>,
    header: Option<DatagramHeader>,
) -> Result<DeviceSocketMetadata<SC::DeviceId>, SendFrameError> {
    let Target { protocol: _, device } = bound;
    let SendFrameParams { device: target_device } = params;

    let device_id = match target_device.or_else(|| match device {
        TargetDevice::AnyDevice => None,
        TargetDevice::SpecificDevice(d) => sync_ctx.upgrade_weak_device_id(d),
    }) {
        Some(d) => d,
        None => return Err(SendFrameError::NoDevice),
    };

    Ok(DeviceSocketMetadata { device_id, header })
}

/// Public identifier for a socket.
#[derive(Derivative)]
#[derivative(Debug(bound = "C::SocketState: Debug"))]
pub struct SocketId<C: crate::NonSyncContext>(StrongId<C::SocketState, WeakDeviceId<C>>);

/// Creates an packet socket with no protocol set configured for all devices.
pub fn create<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    external_state: C::SocketState,
) -> SocketId<C> {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketId(SocketHandler::create(&mut sync_ctx, external_state))
}

/// Sets the device for which a packet socket will receive packets.
pub fn set_device<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    SocketId(id): &SocketId<C>,
    device: TargetDevice<&DeviceId<C>>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::set_device(&mut sync_ctx, id, device)
}

/// Sets the device and protocol for which a socket will receive packets.
pub fn set_device_and_protocol<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    SocketId(id): &SocketId<C>,
    device: TargetDevice<&DeviceId<C>>,
    protocol: Protocol,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::set_device_and_protocol(&mut sync_ctx, id, device, protocol)
}

/// Gets the bound info for a socket.
pub fn get_info<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    SocketId(id): &SocketId<C>,
) -> SocketInfo<WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::get_info(&mut sync_ctx, id)
}

/// Removes a bound socket.
///
/// # Panics
///
/// If the provided [`SocketId`] is not the last instance for a socket, this
/// method will panic.
pub fn remove<C: crate::NonSyncContext>(sync_ctx: &SyncCtx<C>, SocketId(id): SocketId<C>) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::remove(&mut sync_ctx, id)
}

/// Sends a frame for the specified socket without any additional framing.
pub fn send_frame<C: crate::NonSyncContext, B: BufferMut>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    SocketId(id): &SocketId<C>,
    params: SendFrameParams<DeviceId<C>>,
    body: B,
) -> Result<(), (B, SendFrameError)> {
    let mut sync_ctx = Locked::new(sync_ctx);
    BufferSocketSendHandler::send_frame(&mut sync_ctx, ctx, id, params, body)
}

/// Sends a datagram with system-determined framing.
pub fn send_datagram<C: crate::NonSyncContext, B: BufferMut>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    SocketId(id): &SocketId<C>,
    params: SendDatagramParams<DeviceId<C>>,
    body: B,
) -> Result<(), (B, SendDatagramError)> {
    let mut sync_ctx = Locked::new(sync_ctx);
    BufferSocketSendHandler::send_datagram(&mut sync_ctx, ctx, id, params, body)
}

/// Allows the rest of the stack to dispatch packets to listening sockets.
///
/// This is implemented on top of [`SyncContext`] and abstracts packet socket
/// delivery from the rest of the system.
pub(super) trait BufferSocketHandler<D: Device, C>: DeviceIdContext<D> {
    /// Dispatch a received frame to sockets.
    fn handle_received_frame(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        frame: Frame<&[u8]>,
        whole_frame: &[u8],
    );
}

/// A frame received on a socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Frame<B> {
    /// An ethernet frame received on a socket.
    Ethernet {
        /// Where the frame was destined.
        frame_dst: FrameDestination,
        /// The source address of the frame.
        src_mac: Mac,
        /// The destination address of the frame.
        dst_mac: Mac,
        /// The protocol of the frame, or `None` if there was none.
        protocol: Option<u16>,
        /// The body of the frame.
        body: B,
    },
}

impl<'a> Frame<&'a [u8]> {
    pub(super) fn from_ethernet(
        frame: &'a EthernetFrame<&'a [u8]>,
        frame_dst: FrameDestination,
    ) -> Self {
        Self::Ethernet {
            frame_dst,
            src_mac: frame.src_mac(),
            dst_mac: frame.dst_mac(),
            protocol: frame.ethertype().map(Into::into),
            body: frame.body(),
        }
    }

    fn protocol(&self) -> Option<u16> {
        match self {
            Self::Ethernet { frame_dst: _, src_mac: _, dst_mac: _, protocol, body: _ } => *protocol,
        }
    }
}

impl<
        D: Device,
        C: NonSyncContext<<SC as DeviceIdContext<AnyDevice>>::DeviceId>,
        SC: SyncContext<C> + DeviceIdContext<D>,
    > BufferSocketHandler<D, C> for SC
where
    <SC as DeviceIdContext<D>>::DeviceId: Into<<SC as DeviceIdContext<AnyDevice>>::DeviceId>,
{
    fn handle_received_frame(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        frame: Frame<&[u8]>,
        whole_frame: &[u8],
    ) {
        let device = device.clone().into();

        // TODO(https://fxbug.dev/125732): Invert the order of acquisition
        // for the lock on the sockets held in the device and the any-device
        // sockets lock.
        self.with_any_device_sockets(|AnyDeviceSockets(any_device_sockets), sync_ctx| {
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
            sync_ctx.with_device_sockets(&device, |DeviceSockets(device_sockets), sync_ctx| {
                for socket in any_device_sockets.iter().chain(device_sockets) {
                    sync_ctx.with_socket_state(
                        socket,
                        |external_state, Target { protocol, device: _ }, _sync_ctx| {
                            if protocol.map_or(false, |p| match p {
                                Protocol::Specific(p) => Some(p.get()) == frame.protocol(),
                                Protocol::All => true,
                            }) {
                                ctx.receive_frame(external_state, &device, frame, whole_frame)
                            }
                        },
                    )
                }
            })
        })
    }
}

impl<C: crate::NonSyncContext, L> SyncContextTypes for Locked<&SyncCtx<C>, L> {
    type SocketId = StrongId<C::SocketState, WeakDeviceId<C>>;
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::AllDeviceSockets>> SyncContext<C>
    for Locked<&SyncCtx<C>, L>
{
    type SocketTablesSyncCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::AnyDeviceSockets>;

    fn create_socket(&mut self, state: C::SocketState) -> Self::SocketId {
        let mut sockets = self.lock();
        let AllSockets(sockets) = &mut *sockets;
        let entry = sockets.push_with(|index| {
            PrimaryId(PrimaryRc::new(SocketState {
                all_sockets_index: index,
                external_state: state,
                target: Mutex::new(Target::default()),
            }))
        });
        let PrimaryId(primary) = &entry.get();
        StrongId(PrimaryRc::clone_strong(primary))
    }

    fn remove_socket(&mut self, socket: Self::SocketId) {
        let mut state = self.lock();
        let AllSockets(sockets) = &mut *state;

        let PrimaryId(primary) = sockets.remove(socket.get_key_index()).expect("unknown socket ID");
        // Make sure to drop the strong ID before trying to unwrap the primary
        // ID.
        drop(socket);

        let _: SocketState<_, _> = PrimaryRc::unwrap(primary);
    }

    fn with_any_device_sockets<
        F: FnOnce(&AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (sockets, mut locked) = self.read_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&*sockets, &mut locked)
    }

    fn with_any_device_sockets_mut<
        F: FnOnce(&mut AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (mut sockets, mut locked) =
            self.write_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&mut *sockets, &mut locked)
    }

    fn without_sockets<R>(
        &mut self,
        cb: impl FnOnce(&mut Self::SocketTablesSyncCtx<'_>) -> R,
    ) -> R {
        cb(&mut self.cast_locked::<crate::lock_ordering::AnyDeviceSockets>())
    }
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceSocketState>>
    SocketStateAccessor<C> for Locked<&SyncCtx<C>, L>
{
    type SocketStateSyncCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::DeviceSocketState>;

    fn with_socket_state<
        F: FnOnce(
            &C::SocketState,
            &Target<Self::WeakDeviceId>,
            &mut Self::SocketStateSyncCtx<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        StrongId(strong): &Self::SocketId,
        cb: F,
    ) -> R {
        let SocketState { external_state, target, all_sockets_index: _ } = &**strong;
        cb(external_state, &*target.lock(), &mut self.cast_locked())
    }

    fn with_socket_state_mut<
        F: FnOnce(
            &C::SocketState,
            &mut Target<Self::WeakDeviceId>,
            &mut Self::SocketStateSyncCtx<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        StrongId(primary): &Self::SocketId,
        cb: F,
    ) -> R {
        let SocketState { external_state, target, all_sockets_index: _ } = &**primary;
        cb(external_state, &mut *target.lock(), &mut self.cast_locked())
    }
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceSockets>>
    DeviceSocketAccessor<C> for Locked<&SyncCtx<C>, L>
{
    type DeviceSocketSyncCtx<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::DeviceSockets>;

    fn with_device_sockets<
        F: FnOnce(&DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R {
        match device {
            DeviceId::Ethernet(device) => {
                with_ethernet_state_and_sync_ctx(self, device, |mut device_state, locked| {
                    let device_sockets =
                        device_state.read_lock::<crate::lock_ordering::DeviceSockets>();
                    cb(&*device_sockets, &mut locked.cast_locked())
                })
            }
            DeviceId::Loopback(device) => {
                with_loopback_state_and_sync_ctx(self, device, |mut device_state, locked| {
                    let device_sockets =
                        device_state.read_lock::<crate::lock_ordering::DeviceSockets>();
                    cb(&*device_sockets, &mut locked.cast_locked())
                })
            }
        }
    }

    fn with_device_sockets_mut<
        F: FnOnce(&mut DeviceSockets<Self::SocketId>, &mut Self::DeviceSocketSyncCtx<'_>) -> R,
        R,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R {
        match device {
            DeviceId::Ethernet(device) => {
                with_ethernet_state_and_sync_ctx(self, device, |mut device_state, locked| {
                    let mut device_sockets =
                        device_state.write_lock::<crate::lock_ordering::DeviceSockets>();
                    cb(&mut *device_sockets, &mut locked.cast_locked())
                })
            }
            DeviceId::Loopback(device) => {
                with_loopback_state_and_sync_ctx(self, device, |mut device_state, locked| {
                    let mut device_sockets =
                        device_state.write_lock::<crate::lock_ordering::DeviceSockets>();
                    cb(&mut *device_sockets, &mut locked.cast_locked())
                })
            }
        }
    }
    fn without_device_sockets<R>(
        &mut self,
        cb: impl FnOnce(&mut Self::DeviceSocketSyncCtx<'_>) -> R,
    ) -> R {
        cb(&mut self.cast_locked())
    }
}

impl<C: crate::NonSyncContext> RwLockFor<crate::lock_ordering::AnyDeviceSockets> for SyncCtx<C> {
    type ReadData<'l> = crate::sync::RwLockReadGuard<'l, AnyDeviceSockets<StrongId<C::SocketState, WeakDeviceId<C>>>>
        where Self: 'l;
    type WriteData<'l> = crate::sync::RwLockWriteGuard<'l, AnyDeviceSockets<StrongId<C::SocketState, WeakDeviceId<C>>>>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadData<'_> {
        self.state.device.shared_sockets.any_device_sockets.read()
    }
    fn write_lock(&self) -> Self::WriteData<'_> {
        self.state.device.shared_sockets.any_device_sockets.write()
    }
}

impl<C: crate::NonSyncContext> LockFor<crate::lock_ordering::AllDeviceSockets> for SyncCtx<C> {
    type Data<'l> = crate::sync::LockGuard<'l, AllSockets<PrimaryId<C::SocketState, WeakDeviceId<C>>>>
        where Self: 'l;

    fn lock(&self) -> Self::Data<'_> {
        self.state.device.shared_sockets.all_sockets.lock()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{
        collections::{HashMap, HashSet},
        vec,
        vec::Vec,
    };

    use derivative::Derivative;
    use net_types::ethernet::Mac;
    use nonzero_ext::nonzero;
    use packet::{Buf, BufferMut, ParsablePacket};
    use packet_formats::ethernet::EthernetFrameLengthCheck;
    use test_case::test_case;

    use crate::{
        context::testutil::FakeSyncCtx,
        data_structures::id_map::IdMap,
        device::{
            testutil::{FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
            Id,
        },
        sync::Mutex,
    };

    use super::*;

    impl Frame<&[u8]> {
        fn cloned(self) -> Frame<Vec<u8>> {
            match self {
                Self::Ethernet { frame_dst, src_mac, dst_mac, protocol, body } => {
                    Frame::Ethernet { frame_dst, src_mac, dst_mac, protocol, body: Vec::from(body) }
                }
            }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct ReceivedFrame<D> {
        device: D,
        frame: Frame<Vec<u8>>,
        raw: Vec<u8>,
    }

    #[derive(Debug, Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeNonSyncCtx<D> {
        sent: Vec<(DeviceSocketMetadata<D>, Vec<u8>)>,
    }

    impl<D: Id> DeviceSocketTypes for FakeNonSyncCtx<D> {
        type SocketState = ExternalSocketState<D>;
    }

    impl<D: Id> NonSyncContext<D> for FakeNonSyncCtx<D> {
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

    impl<D: FakeStrongDeviceId, B: BufferMut>
        SendFrameContext<FakeNonSyncCtx<D>, B, DeviceSocketMetadata<D>>
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        fn send_frame<S: Serializer<Buffer = B>>(
            &mut self,
            ctx: &mut FakeNonSyncCtx<D>,
            metadata: DeviceSocketMetadata<D>,
            frame: S,
        ) -> Result<(), S> {
            let DeviceSocketMetadata { device_id: _, header: _ } = &metadata;
            match frame.serialize_vec_outer() {
                Ok(frame) => Ok(ctx.sent.push((
                    metadata,
                    frame.map_b(Buf::into_inner).map_a(|b| b.to_flattened_vec()).into_inner(),
                ))),
                Err((_, s)) => Err(s),
            }
        }
    }

    #[derive(Debug, Derivative)]
    #[derivative(Default(bound = ""))]
    struct ExternalSocketState<D>(Mutex<Vec<ReceivedFrame<D>>>);

    type FakeAllSockets<D> =
        IdMap<(ExternalSocketState<D>, Target<<D as crate::device::StrongId>::Weak>)>;

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeSockets<D: FakeStrongDeviceId> {
        any_device_sockets: AnyDeviceSockets<FakeStrongId>,
        device_sockets: HashMap<D, DeviceSockets<FakeStrongId>>,
        all_sockets: FakeAllSockets<D>,
    }

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    struct FakeStrongId(usize);

    #[derive(Debug)]
    struct FakePrimaryId(usize);

    impl StrongSocketId for FakeStrongId {
        type Primary = FakePrimaryId;
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
                all_sockets: FakeAllSockets::<D>::default(),
            }
        }

        fn remove_device(&mut self, device: &D) -> DeviceSockets<FakeStrongId> {
            let Self { any_device_sockets: _, device_sockets, all_sockets: _ } = self;
            device_sockets.remove(device).unwrap()
        }
    }

    /// Simplified trait that provides a blanket impl of [`DeviceIdContext`].
    pub(crate) trait FakeDeviceIdContext {
        type DeviceId: FakeStrongDeviceId + 'static;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool;
    }

    impl<SC: FakeDeviceIdContext> DeviceIdContext<AnyDevice> for SC {
        type DeviceId = SC::DeviceId;
        type WeakDeviceId = FakeWeakDeviceId<Self::DeviceId>;
        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            FakeWeakDeviceId(device_id.clone())
        }
        fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
            self.contains_id(device_id)
        }
        fn upgrade_weak_device_id(
            &self,
            FakeWeakDeviceId(device_id): &Self::WeakDeviceId,
        ) -> Option<Self::DeviceId> {
            self.contains_id(device_id).then_some(device_id.clone())
        }
    }

    impl<D: FakeStrongDeviceId + 'static> DeviceIdContext<AnyDevice>
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        type DeviceId = D;
        type WeakDeviceId = D::Weak;
        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            self.get_ref().downgrade_device_id(device_id)
        }
        fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
            self.get_ref().is_device_installed(device_id)
        }
        fn upgrade_weak_device_id(&self, device_id: &Self::WeakDeviceId) -> Option<Self::DeviceId> {
            self.get_ref().upgrade_weak_device_id(device_id)
        }
    }

    impl<D: FakeStrongDeviceId + 'static> FakeDeviceIdContext for HashSet<D> {
        type DeviceId = D;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool {
            self.contains(device_id)
        }
    }

    impl<V, D: FakeStrongDeviceId + 'static> FakeDeviceIdContext for HashMap<D, V> {
        type DeviceId = D;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool {
            self.contains_key(device_id)
        }
    }

    impl<D: FakeStrongDeviceId + 'static> FakeDeviceIdContext for FakeSockets<D> {
        type DeviceId = D;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool {
            self.device_sockets.contains_key(device_id)
        }
    }

    impl<B: FakeDeviceIdContext> FakeDeviceIdContext for &B {
        type DeviceId = B::DeviceId;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool {
            (*self).contains_id(device_id)
        }
    }

    impl<A, B: FakeDeviceIdContext> FakeDeviceIdContext for (A, &B) {
        type DeviceId = B::DeviceId;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool {
            self.1.contains_id(device_id)
        }
    }

    impl<A, B: FakeDeviceIdContext> FakeDeviceIdContext for (A, &mut B) {
        type DeviceId = B::DeviceId;
        fn contains_id(&self, device_id: &Self::DeviceId) -> bool {
            self.1.contains_id(device_id)
        }
    }

    impl<B: FakeDeviceIdContext> SyncContextTypes for (&mut FakeAllSockets<B::DeviceId>, &B) {
        type SocketId = FakeStrongId;
    }

    impl<B: FakeDeviceIdContext> SyncContextTypes for (&mut FakeAllSockets<B::DeviceId>, &mut B) {
        type SocketId = FakeStrongId;
    }

    impl<D: FakeStrongDeviceId> SyncContextTypes for FakeSyncCtx<FakeSockets<D>, (), D> {
        type SocketId = FakeStrongId;
    }

    impl<
            'b,
            C: NonSyncContext<B::DeviceId, SocketState = ExternalSocketState<B::DeviceId>>,
            B: FakeDeviceIdContext,
        > SocketStateAccessor<C> for (&'b mut FakeAllSockets<B::DeviceId>, &'b B)
    {
        type SocketStateSyncCtx<'a> = &'b B;

        fn with_socket_state<
            F: FnOnce(
                &C::SocketState,
                &Target<Self::WeakDeviceId>,
                &mut Self::SocketStateSyncCtx<'_>,
            ) -> R,
            R,
        >(
            &mut self,
            socket: &Self::SocketId,
            cb: F,
        ) -> R {
            let (sockets, sync_ctx) = self;
            let (state, target) = sockets.get(socket.0).unwrap();
            cb(state, target, sync_ctx)
        }

        fn with_socket_state_mut<
            F: FnOnce(
                &C::SocketState,
                &mut Target<Self::WeakDeviceId>,
                &mut Self::SocketStateSyncCtx<'_>,
            ) -> R,
            R,
        >(
            &mut self,
            socket: &Self::SocketId,
            cb: F,
        ) -> R {
            let (sockets, sync_ctx) = self;
            let (state, target) = sockets.get_mut(socket.0).unwrap();
            cb(state, target, sync_ctx)
        }
    }

    impl<D: FakeStrongDeviceId + 'static> DeviceSocketAccessor<FakeNonSyncCtx<D>>
        for (&mut FakeAllSockets<D>, &mut HashMap<D, DeviceSockets<FakeStrongId>>)
    {
        type DeviceSocketSyncCtx<'a> = (&'a mut FakeAllSockets<D>, &'a HashSet<D>);
        fn with_device_sockets<
            F: FnOnce(&DeviceSockets<FakeStrongId>, &mut Self::DeviceSocketSyncCtx<'_>) -> R,
            R,
        >(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            let (sync_ctx, device_sockets) = self;
            let devices = device_sockets.keys().cloned().collect();
            cb(device_sockets.get(device).unwrap(), &mut (sync_ctx, &devices))
        }
        fn with_device_sockets_mut<
            F: FnOnce(&mut DeviceSockets<FakeStrongId>, &mut Self::DeviceSocketSyncCtx<'_>) -> R,
            R,
        >(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            let (sync_ctx, device_sockets) = self;
            let devices = device_sockets.keys().cloned().collect();
            cb(device_sockets.get_mut(device).unwrap(), &mut (sync_ctx, &devices))
        }
        fn without_device_sockets<R>(
            &mut self,
            cb: impl FnOnce(&mut Self::DeviceSocketSyncCtx<'_>) -> R,
        ) -> R {
            let (sync_ctx, device_sockets) = self;
            let devices = device_sockets.keys().cloned().collect();
            cb(&mut (sync_ctx, &devices))
        }
    }

    impl<D: FakeStrongDeviceId + 'static> SyncContext<FakeNonSyncCtx<D>>
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        type SocketTablesSyncCtx<'a> =
            (&'a mut FakeAllSockets<D>, &'a mut HashMap<D, DeviceSockets<FakeStrongId>>);

        fn create_socket(&mut self, state: ExternalSocketState<D>) -> Self::SocketId {
            FakeStrongId(self.get_mut().all_sockets.push((state, Target::default())))
        }

        fn remove_socket(&mut self, id: Self::SocketId) {
            let FakeSockets {
                any_device_sockets: AnyDeviceSockets(any_device_sockets),
                device_sockets,
                all_sockets,
            } = self.get_mut();
            // Ensure there aren't any additional references to the socket's
            // state.
            assert!(!any_device_sockets.contains(&id));
            assert!(!device_sockets
                .iter()
                .any(|(_device, DeviceSockets(sockets))| sockets.contains(&id)));

            let FakeStrongId(index) = id;
            let _: (_, _) = all_sockets.remove(index).unwrap();
        }

        fn with_any_device_sockets<
            F: FnOnce(&AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesSyncCtx<'_>) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSockets { all_sockets, any_device_sockets, device_sockets } = self.get_mut();
            cb(any_device_sockets, &mut (all_sockets, device_sockets))
        }
        fn with_any_device_sockets_mut<
            F: FnOnce(&mut AnyDeviceSockets<Self::SocketId>, &mut Self::SocketTablesSyncCtx<'_>) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSockets { all_sockets, any_device_sockets, device_sockets } = self.get_mut();
            cb(any_device_sockets, &mut (all_sockets, device_sockets))
        }
        fn without_sockets<R>(
            &mut self,
            cb: impl FnOnce(&mut Self::SocketTablesSyncCtx<'_>) -> R,
        ) -> R {
            let FakeSockets { all_sockets, any_device_sockets: _, device_sockets } = self.get_mut();
            cb(&mut (all_sockets, device_sockets))
        }
    }

    impl<D: FakeStrongDeviceId, B: BufferMut>
        SendFrameContext<FakeNonSyncCtx<D>, B, DeviceSocketMetadata<D>>
        for HashMap<D, DeviceSockets<FakeStrongId>>
    {
        fn send_frame<S: Serializer<Buffer = B>>(
            &mut self,
            ctx: &mut FakeNonSyncCtx<D>,
            metadata: DeviceSocketMetadata<D>,
            frame: S,
        ) -> Result<(), S> {
            let body = frame.serialize_vec_outer().map_err(|(_, s)| s)?;
            let body = body.map_a(|b| b.to_flattened_vec()).map_b(Buf::into_inner).into_inner();
            ctx.sent.push((metadata, body));
            Ok(())
        }
    }

    const SOME_PROTOCOL: NonZeroU16 = nonzero!(2000u16);

    #[test]
    fn create_remove() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx, Default::default());
        assert_eq!(
            SocketHandler::get_info(&mut sync_ctx, &mut bound),
            SocketInfo { device: TargetDevice::AnyDevice, protocol: None }
        );

        SocketHandler::remove(&mut sync_ctx, bound);
    }

    #[test_case(TargetDevice::AnyDevice)]
    #[test_case(TargetDevice::SpecificDevice(&MultipleDevicesId::A))]
    fn set_device(device: TargetDevice<&MultipleDevicesId>) {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx, Default::default());
        SocketHandler::set_device(&mut sync_ctx, &mut bound, device.clone());
        assert_eq!(
            SocketHandler::get_info(&mut sync_ctx, &mut bound),
            SocketInfo { device: device.with_weak_id(), protocol: None }
        );

        let FakeSockets { device_sockets, any_device_sockets: _, all_sockets: _ } =
            sync_ctx.get_ref();
        if let TargetDevice::SpecificDevice(d) = device {
            let DeviceSockets(socket_ids) = device_sockets.get(&d).expect("device state exists");
            assert_eq!(socket_ids, &HashSet::from([bound]));
        }
    }

    #[test]
    fn update_device() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx, Default::default());

        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            TargetDevice::SpecificDevice(&MultipleDevicesId::A),
        );

        // Now update the device and make sure the socket only appears in the
        // one device's list.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            TargetDevice::SpecificDevice(&MultipleDevicesId::B),
        );
        assert_eq!(
            SocketHandler::get_info(&mut sync_ctx, &mut bound),
            SocketInfo {
                device: TargetDevice::SpecificDevice(FakeWeakDeviceId(MultipleDevicesId::B)),
                protocol: None
            }
        );

        let FakeSockets { device_sockets, any_device_sockets: _, all_sockets: _ } =
            sync_ctx.get_ref();
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
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut sockets =
            [(); 3].map(|()| SocketHandler::create(&mut sync_ctx, Default::default()));
        for socket in &mut sockets {
            SocketHandler::set_device_and_protocol(&mut sync_ctx, socket, device.clone(), protocol);
            assert_eq!(
                SocketHandler::get_info(&mut sync_ctx, socket),
                SocketInfo { device: device.with_weak_id(), protocol: Some(protocol) }
            );
        }

        for socket in sockets {
            SocketHandler::remove(&mut sync_ctx, socket)
        }
    }

    #[test]
    fn change_device_after_removal() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));

        let mut bound = SocketHandler::create(&mut sync_ctx, Default::default());
        // Set the device for the socket before removing the device state
        // entirely.
        const DEVICE_TO_REMOVE: MultipleDevicesId = MultipleDevicesId::A;
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            TargetDevice::SpecificDevice(&DEVICE_TO_REMOVE),
        );

        // Now remove the device; this should cause future attempts to upgrade
        // the device ID to fail.
        let removed = sync_ctx.get_mut().remove_device(&DEVICE_TO_REMOVE);
        assert_eq!(removed, DeviceSockets(HashSet::from([bound.clone()])));

        // Changing the device should gracefully handle the fact that the
        // earlier-bound device is now gone.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            TargetDevice::SpecificDevice(&MultipleDevicesId::B),
        );
        assert_eq!(
            SocketHandler::get_info(&mut sync_ctx, &mut bound),
            SocketInfo {
                device: TargetDevice::SpecificDevice(FakeWeakDeviceId(MultipleDevicesId::B)),
                protocol: None,
            }
        );

        let FakeSockets { device_sockets, any_device_sockets: _, all_sockets: _ } =
            sync_ctx.get_ref();
        let DeviceSockets(weak_sockets) =
            device_sockets.get(&MultipleDevicesId::B).expect("device state exists");
        assert_eq!(weak_sockets, &HashSet::from([bound]));
    }

    struct TestData;
    impl TestData {
        const SRC_MAC: Mac = Mac::new([0, 1, 2, 3, 4, 5]);
        const DST_MAC: Mac = Mac::new([6, 7, 8, 9, 10, 11]);
        /// Arbitrary protocol number.
        const PROTO: NonZeroU16 = nonzero!(0x08ABu16);
        const BODY: &'static [u8] = b"some pig";
        const BUFFER: &'static [u8] = &[
            6, 7, 8, 9, 10, 11, 0, 1, 2, 3, 4, 5, 0x08, 0xAB, b's', b'o', b'm', b'e', b' ', b'p',
            b'i', b'g',
        ];

        /// Creates an EthernetFrame with the values specified above.
        fn frame() -> EthernetFrame<&'static [u8]> {
            let mut buffer_view = Self::BUFFER;
            EthernetFrame::parse(&mut buffer_view, EthernetFrameLengthCheck::NoCheck).unwrap()
        }
    }

    const WRONG_PROTO: NonZeroU16 = nonzero!(0x08ffu16);

    fn make_bound<SC: SocketHandler<C>, C: NonSyncContext<SC::DeviceId>>(
        sync_ctx: &mut SC,
        device: TargetDevice<SC::DeviceId>,
        protocol: Option<Protocol>,
        state: C::SocketState,
    ) -> SC::SocketId {
        let mut id = SocketHandler::create(sync_ctx, state);
        let device = match &device {
            TargetDevice::AnyDevice => TargetDevice::AnyDevice,
            TargetDevice::SpecificDevice(d) => TargetDevice::SpecificDevice(d),
        };
        match protocol {
            Some(protocol) => {
                SocketHandler::set_device_and_protocol(sync_ctx, &mut id, device, protocol)
            }
            None => SocketHandler::set_device(sync_ctx, &mut id, device),
        };
        id
    }

    #[test]
    fn receive_frame_deliver_to_multiple() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        use Protocol::*;
        use TargetDevice::*;
        let never_bound = {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            SocketHandler::create(&mut sync_ctx, state)
        };

        let mut make_bound = |device, protocol| {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            make_bound(&mut sync_ctx, device, protocol, state)
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

        BufferSocketHandler::handle_received_frame(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &MultipleDevicesId::A,
            Frame::from_ethernet(&TestData::frame(), FrameDestination::Individual { local: true }),
            TestData::BUFFER,
        );

        let FakeSockets { all_sockets, any_device_sockets: _, device_sockets: _ } =
            sync_ctx.into_state();

        let mut sockets_with_received_frames = all_sockets
            .into_iter()
            .filter_map(|(index, (ExternalSocketState(frames), _)): (_, (_, Target<_>))| {
                let frames = frames.into_inner();
                (!frames.is_empty()).then_some((FakeStrongId(index), frames))
            })
            .collect::<HashMap<_, _>>();
        let mut assert_received = |id| {
            let frames = sockets_with_received_frames.remove(&id).unwrap();
            assert_eq!(
                frames,
                &[ReceivedFrame {
                    device: MultipleDevicesId::A,
                    frame: Frame::Ethernet {
                        frame_dst: FrameDestination::Individual { local: true },
                        src_mac: TestData::SRC_MAC,
                        dst_mac: TestData::DST_MAC,
                        protocol: Some(TestData::PROTO.into()),
                        body: Vec::from(TestData::BODY),
                    },
                    raw: TestData::BUFFER.into(),
                }]
            )
        };

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

        assert_received(bound_a_all_protocols);
        assert_received(bound_a_right_protocol);
        assert_received(bound_any_all_protocols);
        assert_received(bound_any_right_protocol);
        assert!(sockets_with_received_frames.is_empty());
    }

    #[test]
    fn deliver_multiple_frames() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));
        let mut non_sync_ctx = FakeNonSyncCtx::default();
        let socket = make_bound(
            &mut sync_ctx,
            TargetDevice::AnyDevice,
            Some(Protocol::All),
            ExternalSocketState::default(),
        );

        const RECEIVE_COUNT: usize = 10;
        for _ in 0..RECEIVE_COUNT {
            BufferSocketHandler::handle_received_frame(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &MultipleDevicesId::A,
                Frame::from_ethernet(
                    &TestData::frame(),
                    FrameDestination::Individual { local: true },
                ),
                TestData::BUFFER,
            );
        }

        let FakeSockets { mut all_sockets, any_device_sockets: _, device_sockets: _ } =
            sync_ctx.into_state();
        let FakeStrongId(index) = socket;
        let (ExternalSocketState(received), _): (_, Target<_>) = all_sockets.remove(index).unwrap();
        assert_eq!(
            received.into_inner(),
            vec![
                ReceivedFrame {
                    device: MultipleDevicesId::A,
                    frame: Frame::Ethernet {
                        frame_dst: FrameDestination::Individual { local: true },
                        src_mac: TestData::SRC_MAC,
                        dst_mac: TestData::DST_MAC,
                        protocol: Some(TestData::PROTO.into()),
                        body: Vec::from(TestData::BODY),
                    },
                    raw: TestData::BUFFER.into()
                };
                RECEIVE_COUNT
            ]
        );
        assert!(all_sockets.is_empty());
    }

    #[test_case(None, None, Err(SendFrameError::NoDevice); "no bound or override device")]
    #[test_case(Some(MultipleDevicesId::A), None, Ok(MultipleDevicesId::A); "bound device set")]
    #[test_case(None, Some(MultipleDevicesId::A), Ok(MultipleDevicesId::A); "send device set")]
    #[test_case(Some(MultipleDevicesId::A), Some(MultipleDevicesId::A), Ok(MultipleDevicesId::A);
        "both set same")]
    #[test_case(Some(MultipleDevicesId::A), Some(MultipleDevicesId::B), Ok(MultipleDevicesId::B);
        "send overides")]
    fn send_frame_on_socket(
        bind_device: Option<MultipleDevicesId>,
        send_device: Option<MultipleDevicesId>,
        expected_device: Result<MultipleDevicesId, SendFrameError>,
    ) {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let mut id = SocketHandler::create(&mut sync_ctx, Default::default());
        if let Some(bind_device) = bind_device {
            SocketHandler::set_device(
                &mut sync_ctx,
                &mut id,
                TargetDevice::SpecificDevice(&bind_device),
            );
        }

        let destination = SendFrameParams { device: send_device };
        let expected_status = expected_device.as_ref().map(|_| ()).map_err(|e| *e);
        assert_eq!(
            BufferSocketSendHandler::send_frame(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &id,
                destination,
                Buf::new(Vec::from(TestData::BODY), ..),
            )
            .map_err(|(_, e): (Buf<Vec<u8>>, _)| e),
            expected_status
        );

        if let Ok(expected_device) = expected_device {
            let FakeNonSyncCtx { sent } = non_sync_ctx;
            assert_eq!(
                sent,
                [(
                    DeviceSocketMetadata { device_id: expected_device, header: None },
                    Vec::from(TestData::BODY)
                )]
            )
        }
    }

    #[test_case(
        None, None,
        SendDatagramParams {
            frame: SendFrameParams {device: None},
            dest_addr: TestData::DST_MAC,
            protocol: None
        },
        Err(SendDatagramError::NoProtocol); "no protocol or device")]
    #[test_case(
        None, Some(MultipleDevicesId::A),
        SendDatagramParams {
            frame: SendFrameParams {device: None},
            dest_addr: TestData::DST_MAC,
            protocol: None
        },
        Err(SendDatagramError::NoProtocol); "bound no protocol")]
    #[test_case(
        Some(Protocol::All), Some(MultipleDevicesId::A),
        SendDatagramParams {
            frame: SendFrameParams {device: None},
            dest_addr: TestData::DST_MAC,
            protocol: None
        },
        Err(SendDatagramError::NoProtocol); "bound all protocols")]
    #[test_case(
        Some(Protocol::Specific(TestData::PROTO)), None,
        SendDatagramParams {
            frame: SendFrameParams {device: None},
            dest_addr: TestData::DST_MAC,
            protocol: None,
        },
        Err(SendDatagramError::Frame(SendFrameError::NoDevice)); "no device")]
    #[test_case(
        Some(Protocol::Specific(TestData::PROTO)), Some(MultipleDevicesId::A),
        SendDatagramParams {
            frame: SendFrameParams {device: None},
            dest_addr: TestData::DST_MAC,
            protocol: None,
        },
        Ok(MultipleDevicesId::A); "device and proto from bound")]
    #[test_case(
        None, None,
        SendDatagramParams {
            frame: SendFrameParams { device: Some(MultipleDevicesId::C), },
            dest_addr: TestData::DST_MAC,
            protocol: Some(TestData::PROTO),
        },
        Ok(MultipleDevicesId::C); "device and proto from destination")]
    #[test_case(
        Some(Protocol::Specific(WRONG_PROTO)), Some(MultipleDevicesId::A),
        SendDatagramParams {
            frame: SendFrameParams {device: Some(MultipleDevicesId::C),},
            dest_addr: TestData::DST_MAC,
            protocol: Some(TestData::PROTO),
        },
        Ok(MultipleDevicesId::C); "destination overrides")]
    fn send_datagram_on_socket(
        bind_protocol: Option<Protocol>,
        bind_device: Option<MultipleDevicesId>,
        destination: SendDatagramParams<MultipleDevicesId>,
        expected_device: Result<MultipleDevicesId, SendDatagramError>,
    ) {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));
        let mut non_sync_ctx = FakeNonSyncCtx::default();

        let id = make_bound(
            &mut sync_ctx,
            bind_device.map_or(TargetDevice::AnyDevice, TargetDevice::SpecificDevice),
            bind_protocol,
            Default::default(),
        );

        let expected_status = expected_device.as_ref().map(|_| ()).map_err(|e| *e);
        assert_eq!(
            BufferSocketSendHandler::send_datagram(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &id,
                destination,
                Buf::new(Vec::from(TestData::BODY), ..),
            )
            .map_err(|(_, e): (Buf<Vec<u8>>, _)| e),
            expected_status
        );

        if let Ok(expected_device) = expected_device {
            let FakeNonSyncCtx { sent } = non_sync_ctx;
            let expected_sent = (
                DeviceSocketMetadata {
                    device_id: expected_device,
                    header: Some(DatagramHeader {
                        dest_addr: TestData::DST_MAC,
                        protocol: TestData::PROTO.get().into(),
                    }),
                },
                Vec::from(TestData::BODY),
            );
            assert_eq!(sent, [expected_sent])
        }
    }

    #[test]
    fn drop_real_ids() {
        /// Test with a real `SyncCtx` to assert that IDs aren't dropped in the
        /// wrong order.
        use crate::testutil::{Ctx, FakeEventDispatcherBuilder, FAKE_CONFIG_V4};
        let (mut ctx, device_ids) = FakeEventDispatcherBuilder::from_config(FAKE_CONFIG_V4).build();

        let Ctx { sync_ctx, non_sync_ctx: _ } = &mut ctx;

        let never_bound = create(sync_ctx, ());
        let bound_any_device = {
            let id = create(sync_ctx, ());
            set_device(sync_ctx, &id, TargetDevice::AnyDevice);
            id
        };
        let bound_specific_device = {
            let id = create(sync_ctx, ());
            set_device(
                sync_ctx,
                &id,
                TargetDevice::SpecificDevice(&DeviceId::Ethernet(device_ids[0].clone())),
            );
            id
        };

        // Make sure the socket IDs go out of scope before `ctx`.
        drop((never_bound, bound_any_device, bound_specific_device));
    }
}
