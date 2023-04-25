// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Link-layer sockets (analogous to Linux's AF_PACKET sockets).

use alloc::vec::Vec;
use core::{fmt::Debug, hash::Hash, marker::PhantomData, num::NonZeroU16};
use net_types::ethernet::Mac;
use packet::{BufferMut, Serializer};

use derivative::Derivative;
use lock_order::{lock::RwLockFor, relation::LockBefore, Locked};
use packet_formats::ethernet::{EtherType, EthernetFrame};

use crate::{
    context::SendFrameContext,
    data_structures::id_map::{EntryKey, IdMap},
    device::{
        with_ethernet_state, with_loopback_state, AnyDevice, Device, DeviceId, DeviceIdContext,
        FrameDestination, WeakDeviceId,
    },
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

impl<D> TargetDevice<D> {
    fn map<T>(self, f: impl FnOnce(D) -> T) -> TargetDevice<T> {
        match self {
            Self::AnyDevice => TargetDevice::AnyDevice,
            Self::SpecificDevice(d) => TargetDevice::SpecificDevice(f(d)),
        }
    }
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

/// Non-sync context for packet sockets.
pub trait NonSyncContext<DeviceId> {
    /// State for the socket held by core and exposed to bindings.
    type SocketState: Debug;

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

/// Identifier for a socket.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Debug(bound = ""),
    Hash(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = "")
)]
pub struct SocketId<C>(usize, PhantomData<C>);

impl<C> EntryKey for SocketId<C> {
    fn get_key_index(&self) -> usize {
        let Self(index, _marker) = self;
        *index
    }
}

/// Holds sockets that are not bound to a particular device.
///
/// Holds the state for sockets. References in the form of indexes into this
/// state are held in the [`DeviceSockets`] instance for the device to which
/// they are bound.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Sockets<S, D>(
    // TODO(https://fxbug.dev/84345): Make socket IDs shared references instead
    // of indexes into this state.
    IdMap<SocketState<S, D>>,
);

#[derive(Derivative)]
struct SocketState<S, D> {
    external_state: S,
    protocol: Option<Protocol>,
    device: TargetDevice<D>,
}

/// Per-device state for packet sockets.
///
/// Holds sockets that are bound to a particular device. An instance of this
/// should be held in the state for each device in the system.
#[derive(Default)]
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub(crate) struct DeviceSockets(
    // TODO(https://fxbug.dev/84345): Make socket IDs shared references instead
    // of indexes into this state.
    Vec<usize>,
);

pub(crate) trait SyncContext<C: NonSyncContext<Self::DeviceId>>:
    DeviceSocketAccessor
{
    type DeviceSocketAccessor<'a>: DeviceSocketAccessor<
        DeviceId = Self::DeviceId,
        WeakDeviceId = Self::WeakDeviceId,
    >;

    /// Executes the provided callback with immutable access to socket state.
    fn with_sockets<
        F: FnOnce(
            &Sockets<C::SocketState, Self::WeakDeviceId>,
            &mut Self::DeviceSocketAccessor<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;

    /// Executes the provided callback with mutable access to socket state.
    fn with_sockets_mut<
        F: FnOnce(
            &mut Sockets<C::SocketState, Self::WeakDeviceId>,
            &mut Self::DeviceSocketAccessor<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R;
}

pub(crate) trait DeviceSocketAccessor: DeviceIdContext<AnyDevice> {
    /// Executes the provided callback with immutable access to device-specific socket state.
    fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;

    // Executes the provided callback with mutable access to device-specific socket state.
    fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R;
}

/// Internal implementation trait that allows abstracting over device ID types.
trait SocketHandler<C: NonSyncContext<Self::DeviceId>>: DeviceIdContext<AnyDevice> {
    /// Creates a new packet socket.
    fn create(&mut self, external_state: C::SocketState) -> SocketId<C>;

    /// Sets the device for a packet socket without affecting the protocol.
    fn set_device(&mut self, socket: &mut SocketId<C>, device: TargetDevice<&Self::DeviceId>);

    /// Sets both the device and protocol for a packet socket.
    fn set_device_and_protocol(
        &mut self,
        id: &mut SocketId<C>,
        device: TargetDevice<&Self::DeviceId>,
        protocol: Protocol,
    );

    /// Gets information about a socket.
    fn get_info(&mut self, id: &SocketId<C>) -> SocketInfo<Self::WeakDeviceId>;

    /// Removes a packet socket.
    fn remove(&mut self, id: SocketId<C>);
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
        socket: &SocketId<C>,
        params: SendFrameParams<Self::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendFrameError)>;

    /// Sends a datagram with a constructed link-layer header or returns an
    /// error.
    fn send_datagram<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        socket: &SocketId<C>,
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
    socket: &SocketId<C>,
    new_device: TargetDevice<&SC::DeviceId>,
    protocol_update: MaybeUpdate<Protocol>,
) {
    let SocketId(index, _marker) = socket;
    sync_ctx.with_sockets_mut(|Sockets(sockets), sync_ctx| {
        let SocketState { protocol, device, external_state: _ } =
            sockets.get_mut(*index).unwrap_or_else(|| panic!("invalid socket ID {:?}", socket));

        match protocol_update {
            MaybeUpdate::NewValue(p) => *protocol = Some(p),
            MaybeUpdate::NoChange => (),
        };

        // Remove the reference to the socket from the old device if there is
        // one.
        match &device {
            TargetDevice::SpecificDevice(device) => {
                if let Some(device) = sync_ctx.upgrade_weak_device_id(device) {
                    let _index: usize = sync_ctx.with_device_sockets_mut(
                        &device,
                        |DeviceSockets(device_sockets)| {
                            device_sockets.swap_remove(
                                device_sockets
                                    .iter()
                                    .position(|i| i == index)
                                    .unwrap_or_else(|| panic!("invalid socket ID {:?}", socket)),
                            )
                        },
                    );
                }
            }
            TargetDevice::AnyDevice => (),
        };

        // Add the reference to the new device, if there is one.
        match &new_device {
            TargetDevice::SpecificDevice(new_device) => sync_ctx
                .with_device_sockets_mut(new_device, |DeviceSockets(device_sockets)| {
                    device_sockets.push(*index)
                }),
            TargetDevice::AnyDevice => (),
        };

        *device = new_device.map(|d| sync_ctx.downgrade_device_id(d));
    });
}

impl<SC: SyncContext<C>, C: NonSyncContext<SC::DeviceId>> SocketHandler<C> for SC {
    fn create(&mut self, external_state: C::SocketState) -> SocketId<C> {
        let index =
            self.with_sockets_mut(|Sockets(sockets), _: &mut SC::DeviceSocketAccessor<'_>| {
                sockets.push(SocketState {
                    external_state,
                    device: TargetDevice::AnyDevice,
                    protocol: None,
                })
            });
        SocketId(index, Default::default())
    }

    fn set_device(&mut self, socket: &mut SocketId<C>, device: TargetDevice<&SC::DeviceId>) {
        update_device_and_protocol(self, socket, device, MaybeUpdate::NoChange)
    }

    fn set_device_and_protocol(
        &mut self,
        socket: &mut SocketId<C>,
        device: TargetDevice<&SC::DeviceId>,
        protocol: Protocol,
    ) {
        update_device_and_protocol(self, socket, device, MaybeUpdate::NewValue(protocol))
    }

    fn get_info(&mut self, id: &SocketId<C>) -> SocketInfo<Self::WeakDeviceId> {
        self.with_sockets(|Sockets(sockets), _: &mut SC::DeviceSocketAccessor<'_>| {
            let SocketId(index, _marker) = id;
            let SocketState { protocol, device, external_state: _ } =
                sockets.get(*index).unwrap_or_else(|| panic!("invalid socket ID {id:?}"));
            let device = device.clone();
            SocketInfo { device, protocol: *protocol }
        })
    }

    fn remove(&mut self, id: SocketId<C>) {
        let SocketId(index, _marker) = id;
        self.with_sockets_mut(|Sockets(sockets), sync_ctx| {
            let SocketState { device, protocol: _, external_state: _ } =
                sockets.remove(index).unwrap_or_else(|| panic!("invalid socket ID {id:?}"));

            let device = match device {
                TargetDevice::AnyDevice => return,
                TargetDevice::SpecificDevice(device) => device,
            };

            match sync_ctx.upgrade_weak_device_id(&device) {
                None => {
                    // The device was removed earlier so there's no state that
                    // needs to be cleaned up.
                    return;
                }
                Some(strong_device) => {
                    sync_ctx.with_device_sockets_mut(&strong_device, |DeviceSockets(sockets)| {
                        let _: usize = sockets.swap_remove(
                            sockets
                                .iter()
                                .position(|p| *p == index)
                                .unwrap_or_else(|| panic!("invalid socket ID {id:?}")),
                        );
                    })
                }
            }
        })
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct DeviceSocketMetadata<D> {
    pub(crate) device_id: D,
    pub(crate) header: Option<DatagramHeader>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct DatagramHeader {
    pub(crate) dest_addr: Mac,
    pub(crate) protocol: EtherType,
}

impl<SC: SyncContext<C>, C: NonSyncContext<SC::DeviceId>, B: BufferMut>
    BufferSocketSendHandler<B, C> for SC
where
    for<'a> SC::DeviceSocketAccessor<'a>:
        SendFrameContext<C, B, DeviceSocketMetadata<SC::DeviceId>>,
{
    fn send_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        id: &SocketId<C>,
        params: SendFrameParams<SC::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendFrameError)> {
        let SocketId(index, _marker) = id;
        self.with_sockets(|Sockets(sockets), sync_ctx| {
            let state = sockets.get(*index).unwrap_or_else(|| panic!("invalid socket ID {id:?}"));
            send_socket_frame(state, params, sync_ctx, ctx, body, None)
        })
    }

    fn send_datagram<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        id: &SocketId<C>,
        params: SendDatagramParams<Self::DeviceId>,
        body: S,
    ) -> Result<(), (S, SendDatagramError)> {
        let SocketId(index, _marker) = id;
        self.with_sockets(|Sockets(sockets), sync_ctx| {
            let state = sockets.get(*index).unwrap_or_else(|| panic!("invalid socket ID {id:?}"));
            let SendDatagramParams { frame, protocol: target_protocol, dest_addr } = params;

            let SocketState { protocol, device: _, external_state: _ } = &state;
            let protocol = match target_protocol.or_else(|| {
                protocol.and_then(|p| match p {
                    Protocol::Specific(p) => Some(p),
                    Protocol::All => None,
                })
            }) {
                None => return Err((body, SendDatagramError::NoProtocol)),
                Some(p) => p,
            };

            send_socket_frame(
                state,
                frame,
                sync_ctx,
                ctx,
                body,
                Some(DatagramHeader { dest_addr, protocol: EtherType::from(protocol.get()) }),
            )
            .map_err(|(s, e)| (s, SendDatagramError::Frame(e)))
        })
    }
}

fn send_socket_frame<
    SC: DeviceIdContext<AnyDevice> + SendFrameContext<C, B, DeviceSocketMetadata<SC::DeviceId>>,
    B: BufferMut,
    C: NonSyncContext<SC::DeviceId>,
    S: Serializer<Buffer = B>,
>(
    state: &SocketState<C::SocketState, SC::WeakDeviceId>,
    params: SendFrameParams<<SC as DeviceIdContext<AnyDevice>>::DeviceId>,
    sync_ctx: &mut SC,
    ctx: &mut C,
    body: S,
    header: Option<DatagramHeader>,
) -> Result<(), (S, SendFrameError)> {
    let SocketState { protocol: _, device, external_state: _ } = state;
    let SendFrameParams { device: target_device } = params;

    let device_id = match target_device.or_else(|| match device {
        TargetDevice::AnyDevice => None,
        TargetDevice::SpecificDevice(d) => sync_ctx.upgrade_weak_device_id(d),
    }) {
        Some(d) => d,
        None => return Err((body, SendFrameError::NoDevice)),
    };

    let metadata = DeviceSocketMetadata { device_id, header };
    sync_ctx.send_frame(ctx, metadata, body).map_err(|s| (s, SendFrameError::SendFailed))
}

/// Creates an packet socket with no protocol set configured for all devices.
pub fn create<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    external_state: C::SocketState,
) -> SocketId<C> {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::create(&mut sync_ctx, external_state)
}

/// Sets the device for which a packet socket will receive packets.
pub fn set_device<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &mut SocketId<C>,
    device: TargetDevice<&DeviceId<C>>,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::set_device(&mut sync_ctx, id, device)
}

/// Sets the device and protocol for which a socket will receive packets.
pub fn set_device_and_protocol<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &mut SocketId<C>,
    device: TargetDevice<&DeviceId<C>>,
    protocol: Protocol,
) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::set_device_and_protocol(&mut sync_ctx, id, device, protocol)
}

/// Gets the bound info for a socket.
pub fn get_info<C: crate::NonSyncContext>(
    sync_ctx: &SyncCtx<C>,
    id: &SocketId<C>,
) -> SocketInfo<WeakDeviceId<C>> {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::get_info(&mut sync_ctx, id)
}

/// Removes a bound socket.
pub fn remove<C: crate::NonSyncContext>(sync_ctx: &SyncCtx<C>, id: SocketId<C>) {
    let mut sync_ctx = Locked::new(sync_ctx);
    SocketHandler::remove(&mut sync_ctx, id)
}

/// Sends a frame for the specified socket without any additional framing.
pub fn send_frame<C: crate::NonSyncContext, B: BufferMut>(
    sync_ctx: &SyncCtx<C>,
    ctx: &mut C,
    id: &mut SocketId<C>,
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
    id: &mut SocketId<C>,
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
pub(crate) trait BufferSocketHandler<D: Device, C>: DeviceIdContext<D> {
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
    pub(crate) fn from_ethernet(
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

    #[cfg(test)]
    fn cloned(self) -> Frame<Vec<u8>> {
        match self {
            Self::Ethernet { frame_dst, src_mac, dst_mac, protocol, body } => {
                Frame::Ethernet { frame_dst, src_mac, dst_mac, protocol, body: Vec::from(body) }
            }
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

        self.with_sockets(|Sockets(all_sockets), _sync_ctx| {
            for (_index, state) in all_sockets.iter() {
                let SocketState { protocol, device: target_device, external_state } = state;
                match target_device {
                    TargetDevice::SpecificDevice(d) => {
                        if d != &device {
                            continue;
                        }
                    }
                    TargetDevice::AnyDevice => (),
                }

                if protocol.map_or(false, |p| match p {
                    Protocol::Specific(p) => Some(p.get()) == frame.protocol(),
                    Protocol::All => true,
                }) {
                    ctx.receive_frame(external_state, &device, frame, whole_frame)
                }
            }
        })
    }
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::AnyDeviceSockets>> SyncContext<C>
    for Locked<&SyncCtx<C>, L>
{
    type DeviceSocketAccessor<'a> = Locked<&'a SyncCtx<C>, crate::lock_ordering::AnyDeviceSockets>;

    fn with_sockets<
        F: FnOnce(&Sockets<C::SocketState, WeakDeviceId<C>>, &mut Self::DeviceSocketAccessor<'_>) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (sockets, mut locked) = self.read_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&*sockets, &mut locked)
    }

    fn with_sockets_mut<
        F: FnOnce(
            &mut Sockets<C::SocketState, WeakDeviceId<C>>,
            &mut Self::DeviceSocketAccessor<'_>,
        ) -> R,
        R,
    >(
        &mut self,
        cb: F,
    ) -> R {
        let (mut sockets, mut locked) =
            self.write_lock_and::<crate::lock_ordering::AnyDeviceSockets>();
        cb(&mut *sockets, &mut locked)
    }
}

impl<C: crate::NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceSockets>>
    DeviceSocketAccessor for Locked<&SyncCtx<C>, L>
{
    fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R {
        match device {
            DeviceId::Ethernet(device) => with_ethernet_state(self, device, |mut locked| {
                let device_sockets = locked.read_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&*device_sockets)
            }),
            DeviceId::Loopback(device) => with_loopback_state(self, device, |mut locked| {
                let device_sockets = locked.read_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&*device_sockets)
            }),
        }
    }

    fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> R {
        match device {
            DeviceId::Ethernet(device) => with_ethernet_state(self, device, |mut locked| {
                let mut device_sockets = locked.write_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&mut *device_sockets)
            }),
            DeviceId::Loopback(device) => with_loopback_state(self, device, |mut locked| {
                let mut device_sockets = locked.write_lock::<crate::lock_ordering::DeviceSockets>();
                cb(&mut *device_sockets)
            }),
        }
    }
}

impl<C: crate::NonSyncContext> RwLockFor<crate::lock_ordering::AnyDeviceSockets> for SyncCtx<C> {
    type ReadData<'l> = crate::sync::RwLockReadGuard<'l, Sockets<C::SocketState, WeakDeviceId<C>>>
        where Self: 'l;
    type WriteData<'l> = crate::sync::RwLockWriteGuard<'l, Sockets<C::SocketState, WeakDeviceId<C>>>
        where Self: 'l;

    fn read_lock(&self) -> Self::ReadData<'_> {
        self.state.device.shared_sockets.read()
    }
    fn write_lock(&self) -> Self::WriteData<'_> {
        self.state.device.shared_sockets.write()
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use alloc::{
        collections::{HashMap, HashSet},
        rc::Rc,
        vec,
    };
    use derivative::Derivative;
    use net_types::ethernet::Mac;
    use nonzero_ext::nonzero;
    use packet::{Buf, ParsablePacket};
    use packet_formats::ethernet::EthernetFrameLengthCheck;
    use test_case::test_case;

    use crate::{
        context::testutil::FakeSyncCtx,
        device::{
            testutil::{FakeStrongDeviceId, FakeWeakDeviceId, MultipleDevicesId},
            StrongId,
        },
    };

    use super::*;

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

    impl<D: Clone + Debug> NonSyncContext<D> for FakeNonSyncCtx<D> {
        type SocketState = ExternalSocketState<D>;

        fn receive_frame(
            &self,
            state: &ExternalSocketState<D>,
            device: &D,
            frame: Frame<&[u8]>,
            raw_frame: &[u8],
        ) {
            let ExternalSocketState(queue) = state;
            queue.borrow_mut().push(ReceivedFrame {
                device: device.clone(),
                frame: frame.cloned(),
                raw: raw_frame.into(),
            })
        }
    }

    #[derive(Debug, Derivative)]
    #[derivative(Default(bound = ""))]
    struct ExternalSocketState<D>(Rc<RefCell<Vec<ReceivedFrame<D>>>>);

    impl<D> ExternalSocketState<D> {
        fn clone(ExternalSocketState(rc): &Self) -> Self {
            ExternalSocketState(Rc::clone(rc))
        }

        fn take(&self) -> Vec<ReceivedFrame<D>> {
            let Self(rc) = self;
            core::mem::take(&mut *rc.borrow_mut())
        }
    }

    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    struct FakeSockets<D> {
        shared_sockets: Sockets<ExternalSocketState<D>, FakeWeakDeviceId<D>>,
        device_sockets: HashMap<D, DeviceSockets>,
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

    impl<D: Eq + Hash> FakeSockets<D> {
        fn new(devices: impl IntoIterator<Item = D>) -> Self {
            let device_sockets =
                devices.into_iter().map(|d| (d, DeviceSockets::default())).collect();
            Self { shared_sockets: Default::default(), device_sockets }
        }

        fn remove_device(&mut self, device: &D) -> DeviceSockets {
            let Self { shared_sockets: _, device_sockets } = self;
            device_sockets.remove(device).unwrap()
        }
    }

    impl<D: StrongId<Weak = FakeWeakDeviceId<D>> + 'static> DeviceSocketAccessor
        for HashMap<D, DeviceSockets>
    {
        fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            cb(self.get_mut(device).unwrap())
        }
        fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            cb(self.get_mut(device).unwrap())
        }
    }

    impl<D: StrongId<Weak = FakeWeakDeviceId<D>> + 'static> DeviceIdContext<AnyDevice>
        for HashMap<D, DeviceSockets>
    {
        type DeviceId = D;
        type WeakDeviceId = D::Weak;
        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            FakeWeakDeviceId(device_id.clone())
        }
        fn upgrade_weak_device_id(
            &self,
            FakeWeakDeviceId(id): &Self::WeakDeviceId,
        ) -> Option<Self::DeviceId> {
            self.contains_key(id).then_some(id.clone())
        }
        fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
            self.contains_key(device_id)
        }
    }

    impl<D: FakeStrongDeviceId + 'static> DeviceSocketAccessor for FakeSyncCtx<FakeSockets<D>, (), D> {
        fn with_device_sockets<F: FnOnce(&DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            DeviceSocketAccessor::with_device_sockets(
                &mut self.get_mut().device_sockets,
                device,
                cb,
            )
        }
        fn with_device_sockets_mut<F: FnOnce(&mut DeviceSockets) -> R, R>(
            &mut self,
            device: &Self::DeviceId,
            cb: F,
        ) -> R {
            DeviceSocketAccessor::with_device_sockets_mut(
                &mut self.get_mut().device_sockets,
                device,
                cb,
            )
        }
    }

    impl<D: FakeStrongDeviceId + 'static> DeviceIdContext<AnyDevice>
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        type DeviceId = D;
        type WeakDeviceId = FakeWeakDeviceId<D>;
        fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
            DeviceIdContext::downgrade_device_id(&self.get_ref().device_sockets, device_id)
        }
        fn upgrade_weak_device_id(&self, device_id: &Self::WeakDeviceId) -> Option<Self::DeviceId> {
            DeviceIdContext::upgrade_weak_device_id(&self.get_ref().device_sockets, device_id)
        }
        fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
            DeviceIdContext::is_device_installed(&self.get_ref().device_sockets, device_id)
        }
    }

    impl<D: FakeStrongDeviceId + 'static> SyncContext<FakeNonSyncCtx<D>>
        for FakeSyncCtx<FakeSockets<D>, (), D>
    {
        type DeviceSocketAccessor<'a> = HashMap<D, DeviceSockets>;

        fn with_sockets<
            F: FnOnce(
                &Sockets<ExternalSocketState<D>, FakeWeakDeviceId<D>>,
                &mut Self::DeviceSocketAccessor<'_>,
            ) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSockets { shared_sockets, device_sockets } = self.get_mut();
            cb(shared_sockets, device_sockets)
        }
        fn with_sockets_mut<
            F: FnOnce(
                &mut Sockets<ExternalSocketState<D>, FakeWeakDeviceId<D>>,
                &mut Self::DeviceSocketAccessor<'_>,
            ) -> R,
            R,
        >(
            &mut self,
            cb: F,
        ) -> R {
            let FakeSockets { shared_sockets, device_sockets } = self.get_mut();
            cb(shared_sockets, device_sockets)
        }
    }

    impl<D: FakeStrongDeviceId, B: BufferMut>
        SendFrameContext<FakeNonSyncCtx<D>, B, DeviceSocketMetadata<D>>
        for HashMap<D, DeviceSockets>
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

        let bound = SocketHandler::create(&mut sync_ctx, Default::default());
        assert_eq!(
            SocketHandler::get_info(&mut sync_ctx, &bound),
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
            SocketHandler::get_info(&mut sync_ctx, &bound),
            SocketInfo { device: device.with_weak_id(), protocol: None }
        );

        let FakeSockets { device_sockets, shared_sockets: _ } = sync_ctx.get_ref();
        if let TargetDevice::SpecificDevice(d) = device {
            let DeviceSockets(indexes) = device_sockets.get(&d).expect("device state exists");
            let SocketId(index, _marker) = bound;
            assert_eq!(indexes, &[index]);
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
            SocketHandler::get_info(&mut sync_ctx, &bound),
            SocketInfo {
                device: TargetDevice::SpecificDevice(FakeWeakDeviceId(MultipleDevicesId::B)),
                protocol: None
            }
        );

        let FakeSockets { device_sockets, shared_sockets: _ } = sync_ctx.get_ref();
        let device_socket_lists = device_sockets
            .iter()
            .map(|(d, DeviceSockets(indexes))| (d, indexes.as_slice()))
            .collect::<HashMap<_, _>>();

        let SocketId(index, _marker) = bound;
        assert_eq!(
            device_socket_lists,
            HashMap::from([
                (&MultipleDevicesId::A, [].as_slice()),
                (&MultipleDevicesId::B, &[index]),
                (&MultipleDevicesId::C, [].as_slice())
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
        assert_eq!(
            removed,
            DeviceSockets(vec![{
                let SocketId(index, _marker) = bound;
                index
            }])
        );

        // Changing the device should gracefully handle the fact that the
        // earlier-bound device is now gone.
        SocketHandler::set_device(
            &mut sync_ctx,
            &mut bound,
            TargetDevice::SpecificDevice(&MultipleDevicesId::B),
        );
        assert_eq!(
            SocketHandler::get_info(&mut sync_ctx, &bound),
            SocketInfo {
                device: TargetDevice::SpecificDevice(FakeWeakDeviceId(MultipleDevicesId::B)),
                protocol: None,
            }
        );

        let FakeSockets { device_sockets, shared_sockets: _ } = sync_ctx.get_ref();
        let DeviceSockets(indexes) =
            device_sockets.get(&MultipleDevicesId::B).expect("device state exists");
        let SocketId(index, _marker) = bound;
        assert_eq!(indexes, &[index]);
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
    ) -> SocketId<C> {
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
        let mut sockets = IdMap::new();

        use Protocol::*;
        use TargetDevice::*;
        let mut make_bound = |device, protocol| {
            let state = ExternalSocketState::<MultipleDevicesId>::default();
            let id =
                make_bound(&mut sync_ctx, device, protocol, ExternalSocketState::clone(&state));
            sockets.push((id, state))
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

        let received: HashSet<_> = sockets
            .into_iter()
            .filter_map(|(index, (_, frames))| {
                let frames = frames.take();
                if !frames.is_empty() {
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
                    );
                    Some(index)
                } else {
                    None
                }
            })
            .collect();

        let _ = (
            bound_a_no_protocol,
            bound_a_wrong_protocol,
            bound_b_no_protocol,
            bound_b_all_protocols,
            bound_b_right_protocol,
            bound_b_wrong_protocol,
            bound_any_no_protocol,
            bound_any_wrong_protocol,
        );

        assert_eq!(
            received,
            HashSet::from([
                bound_a_all_protocols,
                bound_a_right_protocol,
                bound_any_all_protocols,
                bound_any_right_protocol,
            ])
        )
    }

    #[test]
    fn deliver_multiple_frames() {
        let mut sync_ctx = FakeSyncCtx::with_state(FakeSockets::new(MultipleDevicesId::all()));
        let mut non_sync_ctx = FakeNonSyncCtx::default();
        let received = ExternalSocketState::default();
        let _socket = make_bound(
            &mut sync_ctx,
            TargetDevice::AnyDevice,
            Some(Protocol::All),
            ExternalSocketState::clone(&received),
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

        assert_eq!(
            received.take(),
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
}
