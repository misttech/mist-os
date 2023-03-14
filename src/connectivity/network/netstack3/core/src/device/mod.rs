// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The device layer.

pub(crate) mod arp;
pub mod ethernet;
pub(crate) mod link;
pub(crate) mod loopback;
pub(crate) mod ndp;
pub mod queue;
mod state;

use alloc::vec::Vec;
use core::{
    fmt::{self, Debug, Display, Formatter},
    marker::PhantomData,
};

use derivative::Derivative;
use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    Locked, Unlocked,
};
use log::{debug, trace};
use net_types::{
    ethernet::Mac,
    ip::{
        AddrSubnet, AddrSubnetEither, Ip, IpAddr, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu,
    },
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{Buf, BufferMut, Serializer};
use packet_formats::ethernet::EthernetIpExt;

use crate::{
    context::{InstantContext, RecvFrameContext},
    data_structures::{
        id_map::{self, IdMap},
        id_map_collection::IdMapCollectionKey,
    },
    device::{
        ethernet::{
            EthernetDeviceState, EthernetDeviceStateBuilder, EthernetIpLinkDeviceContext,
            EthernetLinkDevice, EthernetTimerId,
        },
        loopback::{LoopbackDevice, LoopbackDeviceId, LoopbackDeviceState, LoopbackWeakDeviceId},
        queue::{
            rx::ReceiveQueueHandler,
            tx::{BufferTransmitQueueHandler, TransmitQueueConfiguration, TransmitQueueHandler},
        },
        state::IpLinkDeviceState,
    },
    error::{ExistsError, NotFoundError, NotSupportedError},
    ip::{
        device::{
            nud::{BufferNudHandler, DynamicNeighborUpdateSource, NudHandler, NudIpHandler},
            state::{
                AddrConfig, DualStackIpDeviceState, Ipv4DeviceConfiguration, Ipv4DeviceState,
                Ipv6DeviceConfiguration, Ipv6DeviceState,
            },
            BufferIpDeviceContext, DualStackDeviceContext, DualStackDeviceStateRef,
            IpDeviceContext, IpDeviceStateAccessor, Ipv6DeviceContext,
        },
        DualStackDeviceIdContext, IpDeviceId, IpDeviceIdContext, StrongIpDeviceId, WeakIpDeviceId,
    },
    sync::{PrimaryRc, RwLock, StrongRc, WeakRc},
    BufferNonSyncContext, Instant, NonSyncContext, SyncCtx,
};

/// A device.
///
/// `Device` is used to identify a particular device implementation. It
/// is only intended to exist at the type level, never instantiated at runtime.
pub(crate) trait Device: 'static {}

/// An execution context which provides a `DeviceId` type for various device
/// layer internals to share.
pub(crate) trait DeviceIdContext<D: Device> {
    type DeviceId: Clone + Display + Debug + Eq + Send + Sync + 'static;
}

struct RecvIpFrameMeta<D, I: Ip> {
    device: D,
    frame_dst: FrameDestination,
    _marker: PhantomData<I>,
}

impl<D, I: Ip> RecvIpFrameMeta<D, I> {
    fn new(device: D, frame_dst: FrameDestination) -> RecvIpFrameMeta<D, I> {
        RecvIpFrameMeta { device, frame_dst, _marker: PhantomData }
    }
}

impl<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>
    RecvFrameContext<
        NonSyncCtx,
        B,
        RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>, Ipv4>,
    > for &'_ SyncCtx<NonSyncCtx>
{
    fn receive_frame(
        &mut self,
        ctx: &mut NonSyncCtx,
        metadata: RecvIpFrameMeta<
            EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
            Ipv4,
        >,
        frame: B,
    ) {
        crate::ip::receive_ipv4_packet(
            self,
            ctx,
            &metadata.device.into(),
            metadata.frame_dst,
            frame,
        );
    }
}

impl<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>
    RecvFrameContext<
        NonSyncCtx,
        B,
        RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>, Ipv6>,
    > for &'_ SyncCtx<NonSyncCtx>
{
    fn receive_frame(
        &mut self,
        ctx: &mut NonSyncCtx,
        metadata: RecvIpFrameMeta<
            EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
            Ipv6,
        >,
        frame: B,
    ) {
        crate::ip::receive_ipv6_packet(
            self,
            ctx,
            &metadata.device.into(),
            metadata.frame_dst,
            frame,
        );
    }
}

impl<NonSyncCtx: NonSyncContext> UnlockedAccess<crate::lock_ordering::DeviceLayerStateOrigin>
    for SyncCtx<NonSyncCtx>
{
    type Data<'l> = &'l OriginTracker where Self: 'l;
    fn access(&self) -> Self::Data<'_> {
        &self.state.device.origin
    }
}

fn with_ethernet_state_and_sync_ctx<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<
            '_,
            IpLinkDeviceState<NonSyncCtx::Instant, NonSyncCtx::DeviceState, EthernetDeviceState>,
            L,
        >,
        &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    EthernetDeviceId(_id, state): &EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
    cb: F,
) -> O {
    // Make sure that the pointer belongs to this `sync_ctx`.
    assert_eq!(
        *sync_ctx.unlocked_access::<crate::lock_ordering::DeviceLayerStateOrigin>(),
        state.origin
    );

    // Even though the device state is technically accessible outside of the
    // `SyncCtx`, it is held inside `SyncCtx` so we propagate the same lock
    // level as we were called with to avoid lock ordering issues.
    cb(Locked::new_locked(&state), sync_ctx)
}

fn with_ethernet_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<
            '_,
            IpLinkDeviceState<NonSyncCtx::Instant, NonSyncCtx::DeviceState, EthernetDeviceState>,
            L,
        >,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    device_id: &EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
    cb: F,
) -> O {
    with_ethernet_state_and_sync_ctx(sync_ctx, device_id, |ip_device_state, _sync_ctx| {
        cb(ip_device_state)
    })
}

fn with_loopback_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<
            '_,
            IpLinkDeviceState<NonSyncCtx::Instant, NonSyncCtx::DeviceState, LoopbackDeviceState>,
            L,
        >,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    device_id: &LoopbackDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
    cb: F,
) -> O {
    with_loopback_state_and_sync_ctx(sync_ctx, device_id, |ip_device_state, _sync_ctx| {
        cb(ip_device_state)
    })
}

fn with_loopback_state_and_sync_ctx<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<
            '_,
            IpLinkDeviceState<NonSyncCtx::Instant, NonSyncCtx::DeviceState, LoopbackDeviceState>,
            L,
        >,
        &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    LoopbackDeviceId(state): &LoopbackDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
    cb: F,
) -> O {
    // Make sure that the pointer belongs to this `sync_ctx`.
    assert_eq!(
        *sync_ctx.unlocked_access::<crate::lock_ordering::DeviceLayerStateOrigin>(),
        state.origin
    );

    // Even though the device state is technically accessible outside of the
    // `SyncCtx`, it is held inside `SyncCtx` so we propagate the same lock
    // level as we were called with to avoid lock ordering issues.
    cb(Locked::new_locked(&state), sync_ctx)
}

fn with_ip_device_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(Locked<'_, DualStackIpDeviceState<NonSyncCtx::Instant>, L>) -> O,
    L,
>(
    ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => with_ethernet_state(ctx, id, |mut state| cb(state.cast())),
        DeviceIdInner::Loopback(id) => with_loopback_state(ctx, id, |mut state| cb(state.cast())),
    }
}

fn get_mtu<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>(
    ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
) -> Mtu {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => self::ethernet::get_mtu(ctx, &id),
        DeviceIdInner::Loopback(id) => self::loopback::get_mtu(ctx, id),
    }
}

fn join_link_multicast_group<
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::DeviceLayerState>,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &DeviceId<NonSyncCtx>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id.inner() {
        DeviceIdInner::Ethernet(id) => self::ethernet::join_link_multicast(
            sync_ctx,
            ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceIdInner::Loopback(LoopbackDeviceId(_)) => {}
    }
}

fn leave_link_multicast_group<
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::DeviceLayerState>,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &DeviceId<NonSyncCtx>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id.inner() {
        DeviceIdInner::Ethernet(id) => self::ethernet::leave_link_multicast(
            sync_ctx,
            ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceIdInner::Loopback(LoopbackDeviceId(_)) => {}
    }
}

impl<NonSyncCtx: NonSyncContext> DualStackDeviceContext<NonSyncCtx> for &'_ SyncCtx<NonSyncCtx> {
    fn with_dual_stack_device_state<
        O,
        F: FnOnce(DualStackDeviceStateRef<'_, NonSyncCtx::Instant>) -> O,
    >(
        &self,
        device_id: &Self::DualStackDeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(&mut Locked::new(self), device_id, |mut state| {
            let (ipv4, mut locked) =
                state.read_lock_and::<crate::lock_ordering::EthernetDeviceIpState<Ipv4>>();
            let ipv6 = locked.read_lock::<crate::lock_ordering::EthernetDeviceIpState<Ipv6>>();
            cb(DualStackDeviceStateRef { ipv4: &ipv4, ipv6: &ipv6 })
        })
    }
}

/// Iterator over devices.
///
/// Implements `Iterator<Item=DeviceId<C>>` by pulling from provided loopback
/// and ethernet device ID iterators. This struct only exists as a named type
/// so it can be an associated type on impls of the [`IpDeviceContext`] trait.
pub(crate) struct DevicesIter<'s, C: DeviceLayerEventDispatcher> {
    ethernet: id_map::Iter<
        's,
        PrimaryRc<IpLinkDeviceState<C::Instant, C::DeviceState, EthernetDeviceState>>,
    >,
    loopback: core::option::Iter<
        's,
        PrimaryRc<IpLinkDeviceState<C::Instant, C::DeviceState, LoopbackDeviceState>>,
    >,
}

impl<'s, C: DeviceLayerEventDispatcher> Iterator for DevicesIter<'s, C> {
    type Item = DeviceId<C>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self { ethernet, loopback } = self;
        ethernet
            .map(|(id, state)| EthernetDeviceId(id, PrimaryRc::clone_strong(state)).into())
            .chain(loopback.map(|state| {
                DeviceIdInner::Loopback(LoopbackDeviceId(PrimaryRc::clone_strong(state))).into()
            }))
            .next()
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> IpDeviceContext<Ipv4, NonSyncCtx> for &'_ SyncCtx<NonSyncCtx> {
    type DevicesIter<'s> = <Locked<'s, SyncCtx<NonSyncCtx>, Unlocked> as IpDeviceContext<
        Ipv4,
        NonSyncCtx,
    >>::DevicesIter<'s>;

    type DeviceStateAccessor<'s> = <Locked<'s, SyncCtx<NonSyncCtx>, Unlocked> as IpDeviceContext<
        Ipv4,
        NonSyncCtx,
    >>::DeviceStateAccessor<'s>;

    fn with_ip_device_state_mut<O, F: FnOnce(&mut Ipv4DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        IpDeviceContext::<Ipv4, _>::with_ip_device_state_mut(&mut Locked::new(*self), device, cb)
    }

    fn with_devices<O, F: FnOnce(Self::DevicesIter<'_>) -> O>(&mut self, cb: F) -> O {
        IpDeviceContext::<Ipv4, _>::with_devices(&mut Locked::new(*self), cb)
    }

    fn with_devices_and_state<
        O,
        F: for<'a> FnOnce(Self::DevicesIter<'a>, Self::DeviceStateAccessor<'a>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        IpDeviceContext::<Ipv4, _>::with_devices_and_state(&mut Locked::new(*self), cb)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        IpDeviceContext::<Ipv4, _>::get_mtu(&mut Locked::new(*self), device_id)
    }

    fn join_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        IpDeviceContext::<Ipv4, _>::join_link_multicast_group(
            &mut Locked::new(*self),
            ctx,
            device_id,
            multicast_addr,
        )
    }

    fn leave_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        IpDeviceContext::<Ipv4, _>::leave_link_multicast_group(
            &mut Locked::new(*self),
            ctx,
            device_id,
            multicast_addr,
        )
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        IpDeviceContext::<Ipv4, _>::loopback_id(&mut Locked::new(*self))
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> IpDeviceStateAccessor<Ipv4, NonSyncCtx::Instant>
    for &'_ SyncCtx<NonSyncCtx>
{
    fn with_ip_device_state<O, F: FnOnce(&Ipv4DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        IpDeviceStateAccessor::<Ipv4, _>::with_ip_device_state(&mut Locked::new(*self), device, cb)
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>
    IpDeviceContext<Ipv4, NonSyncCtx> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    type DevicesIter<'s> = DevicesIter<'s, NonSyncCtx>;

    type DeviceStateAccessor<'s> =
        Locked<'s, SyncCtx<NonSyncCtx>, crate::lock_ordering::DeviceLayerState>;

    fn with_ip_device_state_mut<O, F: FnOnce(&mut Ipv4DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::EthernetDeviceIpState<Ipv4>>();
            cb(&mut state)
        })
    }

    fn with_devices<O, F: FnOnce(Self::DevicesIter<'_>) -> O>(&mut self, cb: F) -> O {
        let mut locked = self.cast_with(|s| &s.state.device);
        let Devices { ethernet, loopback } =
            &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();

        cb(DevicesIter { ethernet: ethernet.iter(), loopback: loopback.iter() })
    }

    fn with_devices_and_state<
        O,
        F: for<'a> FnOnce(Self::DevicesIter<'a>, Self::DeviceStateAccessor<'a>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (devices, locked) = self.read_lock_and::<crate::lock_ordering::DeviceLayerState>();
        let Devices { ethernet, loopback } = &*devices;

        cb(DevicesIter { ethernet: ethernet.iter(), loopback: loopback.iter() }, locked)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        get_mtu(self, device_id)
    }

    fn join_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        join_link_multicast_group(self, ctx, device_id, multicast_addr)
    }

    fn leave_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        leave_link_multicast_group(self, ctx, device_id, multicast_addr)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.state.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|state| {
            DeviceIdInner::Loopback(LoopbackDeviceId(PrimaryRc::clone_strong(state))).into()
        })
    }
}

impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::EthernetDeviceIpState<Ipv4>>,
    > IpDeviceStateAccessor<Ipv4, NonSyncCtx::Instant> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    fn with_ip_device_state<O, F: FnOnce(&Ipv4DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::EthernetDeviceIpState<Ipv4>>();
            cb(&state)
        })
    }
}

fn send_ip_frame<
    B: BufferMut,
    NonSyncCtx: BufferNonSyncContext<B>,
    S: Serializer<Buffer = B>,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::IpState<A::Version>>
        + LockBefore<crate::lock_ordering::LoopbackTxQueue>,
>(
    sync_ctx: &mut Locked<'_, SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    local_addr: SpecifiedAddr<A>,
    body: S,
) -> Result<(), S>
where
    A::Version: EthernetIpExt,
    for<'a> Locked<'a, SyncCtx<NonSyncCtx>, L>: EthernetIpLinkDeviceContext<
            NonSyncCtx,
            DeviceId = EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>,
        > + BufferNudHandler<B, A::Version, EthernetLinkDevice, NonSyncCtx>
        + BufferTransmitQueueHandler<EthernetLinkDevice, B, NonSyncCtx, Meta = ()>,
{
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            self::ethernet::send_ip_frame::<_, _, _, A, _>(sync_ctx, ctx, &id, local_addr, body)
        }
        DeviceIdInner::Loopback(id) => {
            self::loopback::send_ip_frame(sync_ctx, ctx, id, local_addr, body)
        }
    }
}

fn bytes_to_mac(b: &[u8]) -> Option<Mac> {
    (b.len() >= Mac::BYTES).then(|| {
        Mac::new({
            let mut bytes = [0; Mac::BYTES];
            bytes.copy_from_slice(&b[..Mac::BYTES]);
            bytes
        })
    })
}

impl<I: Ip, C: NonSyncContext> NudIpHandler<I, C> for &'_ SyncCtx<C>
where
    Self: NudHandler<I, EthernetLinkDevice, C>
        + DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<C::Instant, C::DeviceState>>,
{
    fn handle_neighbor_probe(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId<C>,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        match device_id.inner() {
            DeviceIdInner::Ethernet(id) => {
                if let Some(link_addr) = bytes_to_mac(link_addr) {
                    NudHandler::<I, EthernetLinkDevice, _>::set_dynamic_neighbor(
                        self,
                        ctx,
                        &id,
                        neighbor,
                        link_addr,
                        DynamicNeighborUpdateSource::Probe,
                    )
                }
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => {}
        }
    }

    fn handle_neighbor_confirmation(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId<C>,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        match device_id.inner() {
            DeviceIdInner::Ethernet(id) => {
                if let Some(link_addr) = bytes_to_mac(link_addr) {
                    NudHandler::<I, EthernetLinkDevice, _>::set_dynamic_neighbor(
                        self,
                        ctx,
                        &id,
                        neighbor,
                        link_addr,
                        DynamicNeighborUpdateSource::Confirmation,
                    )
                }
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => {}
        }
    }

    fn flush_neighbor_table(&mut self, ctx: &mut C, device_id: &DeviceId<C>) {
        match device_id.inner() {
            DeviceIdInner::Ethernet(id) => {
                NudHandler::<I, EthernetLinkDevice, _>::flush(self, ctx, &id)
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => {}
        }
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>> BufferIpDeviceContext<Ipv4, NonSyncCtx, B>
    for &'_ SyncCtx<NonSyncCtx>
{
    fn send_ip_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        local_addr: SpecifiedAddr<Ipv4Addr>,
        body: S,
    ) -> Result<(), S> {
        BufferIpDeviceContext::<Ipv4, _, _>::send_ip_frame(
            &mut Locked::new(*self),
            ctx,
            device,
            local_addr,
            body,
        )
    }
}

impl<
        B: BufferMut,
        NonSyncCtx: BufferNonSyncContext<B>,
        L: LockBefore<crate::lock_ordering::IpState<Ipv4>>,
    > BufferIpDeviceContext<Ipv4, NonSyncCtx, B> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    fn send_ip_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        local_addr: SpecifiedAddr<Ipv4Addr>,
        body: S,
    ) -> Result<(), S> {
        send_ip_frame(self, ctx, device, local_addr, body)
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> IpDeviceContext<Ipv6, NonSyncCtx> for &'_ SyncCtx<NonSyncCtx> {
    type DevicesIter<'s> = <Locked<'s, SyncCtx<NonSyncCtx>, Unlocked> as IpDeviceContext<
        Ipv6,
        NonSyncCtx,
    >>::DevicesIter<'s>;

    type DeviceStateAccessor<'s> = <Locked<'s, SyncCtx<NonSyncCtx>, Unlocked> as IpDeviceContext<
        Ipv6,
        NonSyncCtx,
    >>::DeviceStateAccessor<'s>;

    fn with_ip_device_state_mut<O, F: FnOnce(&mut Ipv6DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        IpDeviceContext::<Ipv6, _>::with_ip_device_state_mut(&mut Locked::new(*self), device, cb)
    }

    fn with_devices<O, F: FnOnce(Self::DevicesIter<'_>) -> O>(&mut self, cb: F) -> O {
        IpDeviceContext::<Ipv6, _>::with_devices(&mut Locked::new(*self), cb)
    }

    fn with_devices_and_state<
        O,
        F: for<'a> FnOnce(Self::DevicesIter<'a>, Self::DeviceStateAccessor<'a>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        IpDeviceContext::<Ipv6, _>::with_devices_and_state(&mut Locked::new(*self), cb)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        IpDeviceContext::<Ipv6, _>::get_mtu(&mut Locked::new(*self), device_id)
    }

    fn join_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        IpDeviceContext::<Ipv6, _>::join_link_multicast_group(
            &mut Locked::new(*self),
            ctx,
            device_id,
            multicast_addr,
        )
    }

    fn leave_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        IpDeviceContext::<Ipv6, _>::leave_link_multicast_group(
            &mut Locked::new(*self),
            ctx,
            device_id,
            multicast_addr,
        )
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        IpDeviceContext::<Ipv6, _>::loopback_id(&mut Locked::new(*self))
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>
    IpDeviceContext<Ipv6, NonSyncCtx> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    type DevicesIter<'s> = DevicesIter<'s, NonSyncCtx>;

    type DeviceStateAccessor<'s> =
        Locked<'s, SyncCtx<NonSyncCtx>, crate::lock_ordering::DeviceLayerState>;

    fn with_ip_device_state_mut<O, F: FnOnce(&mut Ipv6DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::EthernetDeviceIpState<Ipv6>>();
            cb(&mut state)
        })
    }

    fn with_devices<O, F: FnOnce(Self::DevicesIter<'_>) -> O>(&mut self, cb: F) -> O {
        let mut locked = self.cast_with(|s| &s.state.device);
        let Devices { ethernet, loopback } =
            &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();

        cb(DevicesIter { ethernet: ethernet.iter(), loopback: loopback.iter() })
    }
    fn with_devices_and_state<
        O,
        F: for<'a> FnOnce(Self::DevicesIter<'a>, Self::DeviceStateAccessor<'a>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let (devices, locked) = self.read_lock_and::<crate::lock_ordering::DeviceLayerState>();
        let Devices { ethernet, loopback } = &*devices;

        cb(DevicesIter { ethernet: ethernet.iter(), loopback: loopback.iter() }, locked)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        get_mtu(self, device_id)
    }

    fn join_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        join_link_multicast_group(self, ctx, device_id, multicast_addr)
    }

    fn leave_link_multicast_group(
        &mut self,
        ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        leave_link_multicast_group(self, ctx, device_id, multicast_addr)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.state.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|state| {
            DeviceIdInner::Loopback(LoopbackDeviceId(PrimaryRc::clone_strong(state))).into()
        })
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> IpDeviceStateAccessor<Ipv6, NonSyncCtx::Instant>
    for &'_ SyncCtx<NonSyncCtx>
{
    fn with_ip_device_state<O, F: FnOnce(&Ipv6DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        IpDeviceStateAccessor::<Ipv6, _>::with_ip_device_state(&mut Locked::new(*self), device, cb)
    }
}

impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::EthernetDeviceIpState<Ipv6>>,
    > IpDeviceStateAccessor<Ipv6, NonSyncCtx::Instant> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    fn with_ip_device_state<O, F: FnOnce(&Ipv6DeviceState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::EthernetDeviceIpState<Ipv6>>();
            cb(&state)
        })
    }
}

pub(crate) enum Ipv6DeviceLinkLayerAddr {
    Mac(Mac),
    // Add other link-layer address types as needed.
}

impl AsRef<[u8]> for Ipv6DeviceLinkLayerAddr {
    fn as_ref(&self) -> &[u8] {
        match self {
            Ipv6DeviceLinkLayerAddr::Mac(a) => a.as_ref(),
        }
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> Ipv6DeviceContext<NonSyncCtx> for &'_ SyncCtx<NonSyncCtx> {
    type LinkLayerAddr = Ipv6DeviceLinkLayerAddr;

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Ipv6DeviceLinkLayerAddr> {
        Locked::new(*self).get_link_layer_addr_bytes(device_id)
    }

    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]> {
        Locked::new(*self).get_eui64_iid(device_id)
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        Locked::new(*self).set_link_mtu(device_id, mtu)
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>
    Ipv6DeviceContext<NonSyncCtx> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    type LinkLayerAddr = Ipv6DeviceLinkLayerAddr;

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Ipv6DeviceLinkLayerAddr> {
        match device_id.inner() {
            DeviceIdInner::Ethernet(id) => {
                Some(Ipv6DeviceLinkLayerAddr::Mac(ethernet::get_mac(self, &id).get()))
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => None,
        }
    }

    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]> {
        match device_id.inner() {
            DeviceIdInner::Ethernet(id) => {
                Some(ethernet::get_mac(self, &id).to_eui64_with_magic(Mac::DEFAULT_EUI_MAGIC))
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => None,
        }
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        if mtu < Ipv6::MINIMUM_LINK_MTU {
            return;
        }

        match device_id.inner() {
            DeviceIdInner::Ethernet(id) => ethernet::set_mtu(self, &id, mtu),
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => {}
        }
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>> BufferIpDeviceContext<Ipv6, NonSyncCtx, B>
    for &'_ SyncCtx<NonSyncCtx>
{
    fn send_ip_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        local_addr: SpecifiedAddr<Ipv6Addr>,
        body: S,
    ) -> Result<(), S> {
        BufferIpDeviceContext::<Ipv6, _, _>::send_ip_frame(
            &mut Locked::new(*self),
            ctx,
            device,
            local_addr,
            body,
        )
    }
}

impl<
        B: BufferMut,
        NonSyncCtx: BufferNonSyncContext<B>,
        L: LockBefore<crate::lock_ordering::IpState<Ipv6>>,
    > BufferIpDeviceContext<Ipv6, NonSyncCtx, B> for Locked<'_, SyncCtx<NonSyncCtx>, L>
{
    fn send_ip_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        local_addr: SpecifiedAddr<Ipv6Addr>,
        body: S,
    ) -> Result<(), S> {
        send_ip_frame(self, ctx, device, local_addr, body)
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub(crate) struct EthernetWeakDeviceId<I: Instant, S>(
    usize,
    WeakRc<IpLinkDeviceState<I, S, EthernetDeviceState>>,
);

impl<I: Instant, S> PartialEq for EthernetWeakDeviceId<I, S> {
    fn eq(&self, EthernetWeakDeviceId(other_id, other_ptr): &EthernetWeakDeviceId<I, S>) -> bool {
        let EthernetWeakDeviceId(me_id, me_ptr) = self;
        other_id == me_id && WeakRc::ptr_eq(me_ptr, other_ptr)
    }
}

impl<I: Instant, S> PartialEq<EthernetDeviceId<I, S>> for EthernetWeakDeviceId<I, S> {
    fn eq(&self, other: &EthernetDeviceId<I, S>) -> bool {
        <EthernetDeviceId<I, S> as PartialEq<EthernetWeakDeviceId<I, S>>>::eq(other, self)
    }
}

impl<I: Instant, S> Eq for EthernetWeakDeviceId<I, S> {}

/// Device IDs identifying Ethernet devices.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub(crate) struct EthernetDeviceId<I: Instant, S>(
    usize,
    StrongRc<IpLinkDeviceState<I, S, EthernetDeviceState>>,
);

impl<I: Instant, S> PartialEq for EthernetDeviceId<I, S> {
    fn eq(&self, EthernetDeviceId(other_id, other_ptr): &EthernetDeviceId<I, S>) -> bool {
        let EthernetDeviceId(me_id, me_ptr) = self;
        other_id == me_id && StrongRc::ptr_eq(me_ptr, other_ptr)
    }
}

impl<I: Instant, S> PartialEq<EthernetWeakDeviceId<I, S>> for EthernetDeviceId<I, S> {
    fn eq(&self, EthernetWeakDeviceId(other_id, other_ptr): &EthernetWeakDeviceId<I, S>) -> bool {
        let EthernetDeviceId(me_id, me_ptr) = self;
        other_id == me_id && StrongRc::weak_ptr_eq(me_ptr, other_ptr)
    }
}

impl<I: Instant, S> Eq for EthernetDeviceId<I, S> {}

impl<I: Instant, S> Debug for EthernetDeviceId<I, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<I: Instant, S> Display for EthernetDeviceId<I, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let EthernetDeviceId(id, _ptr) = self;
        write!(f, "Ethernet({})", id)
    }
}

/// The identifier for timer events in the device layer.
#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
pub(crate) struct DeviceLayerTimerId<C: DeviceLayerEventDispatcher>(DeviceLayerTimerIdInner<C>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
enum DeviceLayerTimerIdInner<C: DeviceLayerEventDispatcher> {
    /// A timer event for an Ethernet device.
    Ethernet(EthernetTimerId<EthernetDeviceId<C::Instant, C::DeviceState>>),
}

impl<C: DeviceLayerEventDispatcher>
    From<EthernetTimerId<EthernetDeviceId<C::Instant, C::DeviceState>>> for DeviceLayerTimerId<C>
{
    fn from(
        id: EthernetTimerId<EthernetDeviceId<C::Instant, C::DeviceState>>,
    ) -> DeviceLayerTimerId<C> {
        DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id))
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> DeviceIdContext<EthernetLinkDevice> for &'_ SyncCtx<NonSyncCtx> {
    type DeviceId = EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>;
}

impl<'a, NonSyncCtx: NonSyncContext, L> DeviceIdContext<EthernetLinkDevice>
    for Locked<'a, SyncCtx<NonSyncCtx>, L>
{
    type DeviceId = EthernetDeviceId<NonSyncCtx::Instant, NonSyncCtx::DeviceState>;
}

impl_timer_context!(
    C: DeviceLayerEventDispatcher,
    DeviceLayerTimerId<C>,
    EthernetTimerId<
        EthernetDeviceId<
            <C as InstantContext>::Instant,
            <C as DeviceLayerEventDispatcher>::DeviceState,
        >,
    >,
    DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id)),
    id
);

/// Handle a timer event firing in the device layer.
pub(crate) fn handle_timer<NonSyncCtx: NonSyncContext>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    id: DeviceLayerTimerId<NonSyncCtx>,
) {
    match id.0 {
        DeviceLayerTimerIdInner::Ethernet(id) => ethernet::handle_timer(&mut sync_ctx, ctx, id),
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
enum WeakDeviceIdInner<C: DeviceLayerEventDispatcher> {
    Ethernet(EthernetWeakDeviceId<C::Instant, C::DeviceState>),
    Loopback(LoopbackWeakDeviceId<C::Instant, C::DeviceState>),
}

impl<C: DeviceLayerEventDispatcher> PartialEq<DeviceId<C>> for WeakDeviceId<C> {
    fn eq(&self, other: &DeviceId<C>) -> bool {
        <DeviceId<C> as PartialEq<WeakDeviceId<C>>>::eq(other, self)
    }
}

/// A weak ID identifying a device.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
pub struct WeakDeviceId<C: DeviceLayerEventDispatcher>(WeakDeviceIdInner<C>);

impl<C: DeviceLayerEventDispatcher> From<WeakDeviceIdInner<C>> for WeakDeviceId<C> {
    fn from(id: WeakDeviceIdInner<C>) -> WeakDeviceId<C> {
        WeakDeviceId(id)
    }
}

impl<C: DeviceLayerEventDispatcher> From<EthernetWeakDeviceId<C::Instant, C::DeviceState>>
    for WeakDeviceId<C>
{
    fn from(id: EthernetWeakDeviceId<C::Instant, C::DeviceState>) -> WeakDeviceId<C> {
        WeakDeviceIdInner::Ethernet(id).into()
    }
}

impl<C: DeviceLayerEventDispatcher> From<LoopbackWeakDeviceId<C::Instant, C::DeviceState>>
    for WeakDeviceId<C>
{
    fn from(id: LoopbackWeakDeviceId<C::Instant, C::DeviceState>) -> WeakDeviceId<C> {
        WeakDeviceIdInner::Loopback(id).into()
    }
}

impl<C: DeviceLayerEventDispatcher> WeakDeviceId<C> {
    fn inner(&self) -> &WeakDeviceIdInner<C> {
        let WeakDeviceId(id) = self;
        id
    }

    /// Attempts to upgrade the ID.
    pub fn upgrade(&self) -> Option<DeviceId<C>> {
        match self.inner() {
            WeakDeviceIdInner::Ethernet(EthernetWeakDeviceId(id, ptr)) => {
                ptr.upgrade().map(|ptr| DeviceIdInner::Ethernet(EthernetDeviceId(*id, ptr)))
            }
            WeakDeviceIdInner::Loopback(LoopbackWeakDeviceId(ptr)) => {
                ptr.upgrade().map(|ptr| DeviceIdInner::Loopback(LoopbackDeviceId(ptr)))
            }
        }
        .map(Into::into)
    }
}

impl<C: DeviceLayerEventDispatcher> IpDeviceId for WeakDeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self.inner() {
            WeakDeviceIdInner::Loopback(LoopbackWeakDeviceId(_)) => true,
            WeakDeviceIdInner::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerEventDispatcher> WeakIpDeviceId for WeakDeviceId<C> {
    type Strong = DeviceId<C>;
}

impl<C: DeviceLayerEventDispatcher> Display for WeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        match self.inner() {
            WeakDeviceIdInner::Ethernet(EthernetWeakDeviceId(id, _ptr)) => {
                write!(f, "Weak Ethernet({})", id)
            }
            WeakDeviceIdInner::Loopback(LoopbackWeakDeviceId(_)) => write!(f, "Weak Loopback"),
        }
    }
}

impl<C: DeviceLayerEventDispatcher> Debug for WeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        Display::fmt(self, f)
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
enum DeviceIdInner<C: DeviceLayerEventDispatcher> {
    Ethernet(EthernetDeviceId<C::Instant, C::DeviceState>),
    Loopback(LoopbackDeviceId<C::Instant, C::DeviceState>),
}

/// An ID identifying a device.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
pub struct DeviceId<C: DeviceLayerEventDispatcher>(DeviceIdInner<C>);

impl<C: DeviceLayerEventDispatcher> PartialEq<WeakDeviceId<C>> for DeviceId<C> {
    fn eq(&self, other: &WeakDeviceId<C>) -> bool {
        match (self.inner(), other.inner()) {
            (DeviceIdInner::Ethernet(strong), WeakDeviceIdInner::Ethernet(weak)) => strong == weak,
            (DeviceIdInner::Loopback(strong), WeakDeviceIdInner::Loopback(weak)) => strong == weak,
            (DeviceIdInner::Loopback(_), WeakDeviceIdInner::Ethernet(_))
            | (DeviceIdInner::Ethernet(_), WeakDeviceIdInner::Loopback(_)) => false,
        }
    }
}

impl<C: DeviceLayerEventDispatcher> From<DeviceIdInner<C>> for DeviceId<C> {
    fn from(id: DeviceIdInner<C>) -> DeviceId<C> {
        DeviceId(id)
    }
}

impl<C: DeviceLayerEventDispatcher> From<EthernetDeviceId<C::Instant, C::DeviceState>>
    for DeviceId<C>
{
    fn from(id: EthernetDeviceId<C::Instant, C::DeviceState>) -> DeviceId<C> {
        DeviceIdInner::Ethernet(id).into()
    }
}

impl<C: DeviceLayerEventDispatcher> From<LoopbackDeviceId<C::Instant, C::DeviceState>>
    for DeviceId<C>
{
    fn from(id: LoopbackDeviceId<C::Instant, C::DeviceState>) -> DeviceId<C> {
        DeviceIdInner::Loopback(id).into()
    }
}

impl<C: DeviceLayerEventDispatcher> DeviceId<C> {
    /// Returns a reference to the external state for the device.
    pub fn external_state(&self) -> &C::DeviceState {
        match self.inner() {
            DeviceIdInner::Ethernet(EthernetDeviceId(_id, ptr)) => &ptr.external_state,
            DeviceIdInner::Loopback(LoopbackDeviceId(ptr)) => &ptr.external_state,
        }
    }

    fn inner(&self) -> &DeviceIdInner<C> {
        let DeviceId(id) = self;
        id
    }

    fn into_inner(self) -> DeviceIdInner<C> {
        let DeviceId(id) = self;
        id
    }

    /// Downgrade to a [`WeakDeviceId`].
    pub fn downgrade(&self) -> WeakDeviceId<C> {
        match self.inner() {
            DeviceIdInner::Ethernet(EthernetDeviceId(id, ptr)) => {
                WeakDeviceIdInner::Ethernet(EthernetWeakDeviceId(*id, StrongRc::downgrade(ptr)))
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(ptr)) => {
                WeakDeviceIdInner::Loopback(LoopbackWeakDeviceId(StrongRc::downgrade(ptr)))
            }
        }
        .into()
    }

    fn removed(&self) -> bool {
        match self.inner() {
            DeviceIdInner::Ethernet(EthernetDeviceId(_id, rc)) => {
                StrongRc::marked_for_destruction(rc)
            }
            DeviceIdInner::Loopback(LoopbackDeviceId(rc)) => StrongRc::marked_for_destruction(rc),
        }
    }
}

impl<C: DeviceLayerEventDispatcher> IpDeviceId for DeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self.inner() {
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => true,
            DeviceIdInner::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerEventDispatcher> StrongIpDeviceId for DeviceId<C> {
    type Weak = WeakDeviceId<C>;
}

impl<C: DeviceLayerEventDispatcher> Display for DeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        match self.inner() {
            DeviceIdInner::Ethernet(id) => Display::fmt(id, f),
            DeviceIdInner::Loopback(id) => Display::fmt(id, f),
        }
    }
}

impl<C: DeviceLayerEventDispatcher> Debug for DeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        Display::fmt(self, f)
    }
}

impl<C: DeviceLayerEventDispatcher> IdMapCollectionKey for DeviceId<C> {
    const VARIANT_COUNT: usize = 2;

    fn get_id(&self) -> usize {
        match self.inner() {
            DeviceIdInner::Ethernet(EthernetDeviceId(id, _)) => *id,
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => 0,
        }
    }

    fn get_variant(&self) -> usize {
        match self.inner() {
            DeviceIdInner::Ethernet(_) => 0,
            DeviceIdInner::Loopback(LoopbackDeviceId(_)) => 1,
        }
    }
}

// TODO(joshlf): Does the IP layer ever need to distinguish between broadcast
// and multicast frames?

/// The type of address used as the source address in a device-layer frame:
/// unicast or broadcast.
///
/// `FrameDestination` is used to implement RFC 1122 section 3.2.2 and RFC 4443
/// section 2.4.e, which govern when to avoid sending an ICMP error message for
/// ICMP and ICMPv6 respectively.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FrameDestination {
    /// A unicast address - one which is neither multicast nor broadcast.
    Unicast,
    /// A multicast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Multicast,
    /// A broadcast address; if the addressing scheme supports overlap between
    /// multicast and broadcast, then broadcast addresses should use the
    /// `Broadcast` variant.
    Broadcast,
}

impl FrameDestination {
    /// Is this `FrameDestination::Multicast`?
    pub(crate) fn is_multicast(self) -> bool {
        self == FrameDestination::Multicast
    }

    /// Is this `FrameDestination::Broadcast`?
    pub(crate) fn is_broadcast(self) -> bool {
        self == FrameDestination::Broadcast
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Devices<C: DeviceLayerEventDispatcher> {
    ethernet: IdMap<PrimaryRc<IpLinkDeviceState<C::Instant, C::DeviceState, EthernetDeviceState>>>,
    loopback: Option<PrimaryRc<IpLinkDeviceState<C::Instant, C::DeviceState, LoopbackDeviceState>>>,
}

/// The state associated with the device layer.
pub(crate) struct DeviceLayerState<C: DeviceLayerEventDispatcher> {
    devices: RwLock<Devices<C>>,
    origin: OriginTracker,
}

impl<NonSyncCtx: NonSyncContext> RwLockFor<crate::lock_ordering::DeviceLayerState>
    for SyncCtx<NonSyncCtx>
{
    type ReadData<'l> = crate::sync::RwLockReadGuard<'l, Devices<NonSyncCtx>>
        where
            Self: 'l ;
    type WriteData<'l> = crate::sync::RwLockWriteGuard<'l, Devices<NonSyncCtx>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadData<'_> {
        self.state.device.devices.read()
    }
    fn write_lock(&self) -> Self::WriteData<'_> {
        self.state.device.devices.write()
    }
}

impl<C: DeviceLayerEventDispatcher> RwLockFor<crate::lock_ordering::DeviceLayerState>
    for DeviceLayerState<C>
{
    type ReadData<'l> = crate::sync::RwLockReadGuard<'l, Devices<C>>
        where
            Self: 'l ;
    type WriteData<'l> = crate::sync::RwLockWriteGuard<'l, Devices<C>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadData<'_> {
        self.devices.read()
    }
    fn write_lock(&self) -> Self::WriteData<'_> {
        self.devices.write()
    }
}

/// Light-weight tracker for recording the source of some instance.
///
/// This should be held as a field in a parent type that is cloned into each
/// child instance. Then, the origin of a child instance can be verified by
/// asserting equality against the parent's field.
///
/// This is only enabled in debug builds; in non-debug builds, all
/// `OriginTracker` instances are identical so all operations are no-ops.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OriginTracker(#[cfg(debug_assertions)] u64);

impl OriginTracker {
    /// Creates a new `OriginTracker` that isn't derived from any other
    /// instance.
    ///
    /// In debug builds, this creates a unique `OriginTracker` that won't be
    /// equal to any instances except those cloned from it. In non-debug builds
    /// all `OriginTracker` instances are identical.
    #[cfg_attr(not(debug_assertions), inline)]
    fn new() -> Self {
        Self(
            #[cfg(debug_assertions)]
            {
                static COUNTER: core::sync::atomic::AtomicU64 =
                    core::sync::atomic::AtomicU64::new(0);
                COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
            },
        )
    }
}

impl<C: DeviceLayerEventDispatcher> DeviceLayerState<C> {
    /// Creates a new [`DeviceLayerState`] instance.
    pub(crate) fn new() -> Self {
        Self { devices: Default::default(), origin: OriginTracker::new() }
    }

    /// Add a new ethernet device to the device layer.
    ///
    /// `add` adds a new `EthernetDeviceState` with the given MAC address and
    /// maximum frame size. The frame size is the limit on the size of the data
    /// payload and the header but not the FCS.
    pub(crate) fn add_ethernet_device<F: FnOnce() -> C::DeviceState>(
        &self,
        mac: UnicastAddr<Mac>,
        max_frame_size: ethernet::MaxFrameSize,
        external_state: F,
    ) -> DeviceId<C> {
        let Devices { ethernet, loopback: _ } = &mut *self.devices.write();

        let ptr = PrimaryRc::new(IpLinkDeviceState::new(
            EthernetDeviceStateBuilder::new(mac, max_frame_size).build(),
            external_state(),
            self.origin.clone(),
        ));
        let strong_ptr = PrimaryRc::clone_strong(&ptr);
        let id = ethernet.push(ptr);
        debug!("adding Ethernet device with ID {} and MTU {:?}", id, max_frame_size);
        EthernetDeviceId(id, strong_ptr).into()
    }

    /// Adds a new loopback device to the device layer.
    pub(crate) fn add_loopback_device<F: FnOnce() -> C::DeviceState>(
        &self,
        mtu: Mtu,
        external_state: F,
    ) -> Result<DeviceId<C>, ExistsError> {
        let Devices { ethernet: _, loopback } = &mut *self.devices.write();

        if let Some(_) = loopback {
            return Err(ExistsError);
        }

        let ptr = PrimaryRc::new(IpLinkDeviceState::new(
            LoopbackDeviceState::new(mtu),
            external_state(),
            self.origin.clone(),
        ));
        let id = PrimaryRc::clone_strong(&ptr);

        *loopback = Some(ptr);

        debug!("added loopback device");

        Ok(DeviceIdInner::Loopback(LoopbackDeviceId(id)).into())
    }
}

/// An event dispatcher for the device layer.
///
/// See the `EventDispatcher` trait in the crate root for more details.
pub trait DeviceLayerEventDispatcher: InstantContext + Sized {
    /// The state associated to devices.
    type DeviceState: Send + Sync;

    /// Signals to the dispatcher that RX frames are available and ready to be
    /// handled by [`handle_queued_rx_packets`].
    ///
    /// Implementations must make sure that [`handle_queued_rx_packets`] is
    /// scheduled to be called as soon as possible so that enqueued RX frames
    /// are promptly handled.
    fn wake_rx_task(&mut self, device: &DeviceId<Self>);

    /// Signals to the dispatcher that TX frames are available and ready to be
    /// sent by [`transmit_queued_tx_frames`].
    ///
    /// Implementations must make sure that [`transmit_queued_tx_frames`] is
    /// scheduled to be called as soon as possible so that enqueued TX frames
    /// are promptly sent.
    fn wake_tx_task(&mut self, device: &DeviceId<Self>);

    /// Send a frame to a device driver.
    ///
    /// If there was an MTU error while attempting to serialize the frame, the
    /// original serializer is returned in the `Err` variant. All other errors
    /// (for example, errors in allocating a buffer) are silently ignored and
    /// reported as success.
    fn send_frame(
        &mut self,
        device: &DeviceId<Self>,
        frame: Buf<Vec<u8>>,
    ) -> Result<(), DeviceSendFrameError<Buf<Vec<u8>>>>;
}

/// An error encountered when sending a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum DeviceSendFrameError<T> {
    /// The device is not ready to send frames.
    DeviceNotReady(T),
}

/// Sets the TX queue configuration for a device.
pub fn set_tx_queue_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    config: TransmitQueueConfiguration,
) {
    let sync_ctx = &mut Locked::new(sync_ctx);
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            TransmitQueueHandler::<EthernetLinkDevice, _>::set_configuration(
                sync_ctx, ctx, id, config,
            )
        }
        DeviceIdInner::Loopback(id) => {
            TransmitQueueHandler::<LoopbackDevice, _>::set_configuration(sync_ctx, ctx, id, config)
        }
    }
}

/// Does the work of transmitting frames for a device.
pub fn transmit_queued_tx_frames<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
) -> Result<(), DeviceSendFrameError<()>> {
    let sync_ctx = &mut Locked::new(sync_ctx);
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            TransmitQueueHandler::<EthernetLinkDevice, _>::transmit_queued_frames(sync_ctx, ctx, id)
        }
        DeviceIdInner::Loopback(id) => {
            TransmitQueueHandler::<LoopbackDevice, _>::transmit_queued_frames(sync_ctx, ctx, id)
        }
    }
}

/// Handle a batch of queued RX packets for the device.
///
/// If packets remain in the RX queue after a batch of RX packets has been
/// handled, the RX task will be scheduled to run again so the next batch of
/// RX packets may be handled. See [`DeviceLayerEventDispatcher::wake_rx_task`]
/// for more details.
pub fn handle_queued_rx_packets<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
) {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            panic!("ethernet device {} does not support RX queues", id)
        }
        DeviceIdInner::Loopback(id) => {
            ReceiveQueueHandler::<LoopbackDevice, _>::handle_queued_rx_packets(
                &mut Locked::new(sync_ctx),
                ctx,
                id,
            )
        }
    }
}

/// Remove a device from the device layer.
///
/// # Panics
///
/// Panics if `device` does not refer to an existing device.
pub fn remove_device<NonSyncCtx: NonSyncContext>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: DeviceId<NonSyncCtx>,
) -> NonSyncCtx::DeviceState {
    // Start cleaning up the device by disabling IP state. This removes timers
    // for the device that would otherwise hold references to defunct device
    // state.
    crate::ip::device::clear_ipv4_device_state(&mut sync_ctx, ctx, &device);
    crate::ip::device::clear_ipv6_device_state(&mut sync_ctx, ctx, &device);

    // Uninstall all routes associated with the device.
    crate::ip::del_device_routes::<Ipv4, _, _>(&mut sync_ctx, ctx, &device);
    crate::ip::del_device_routes::<Ipv6, _, _>(&mut sync_ctx, ctx, &device);

    let mut devices = sync_ctx.state.device.devices.write();

    match device.into_inner() {
        DeviceIdInner::Ethernet(EthernetDeviceId(id, ptr)) => {
            let removed = devices
                .ethernet
                .remove(id)
                .unwrap_or_else(|| panic!("no such Ethernet device: {}", id));
            assert!(PrimaryRc::ptr_eq(&removed, &ptr));
            core::mem::drop(ptr);
            debug!("removing Ethernet device with ID {}", id);
            PrimaryRc::unwrap(removed).external_state
        }
        DeviceIdInner::Loopback(LoopbackDeviceId(ptr)) => {
            let removed: PrimaryRc<IpLinkDeviceState<_, _, _>> =
                devices.loopback.take().expect("loopback device does not exist");
            assert!(PrimaryRc::ptr_eq(&removed, &ptr));
            core::mem::drop(ptr);
            debug!("removing Loopback device");
            PrimaryRc::unwrap(removed).external_state
        }
    }
}

/// Adds a new Ethernet device to the stack.
pub fn add_ethernet_device_with_state<
    NonSyncCtx: NonSyncContext,
    F: FnOnce() -> NonSyncCtx::DeviceState,
>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mac: UnicastAddr<Mac>,
    max_frame_size: ethernet::MaxFrameSize,
    external_state: F,
) -> DeviceId<NonSyncCtx> {
    sync_ctx.state.device.add_ethernet_device(mac, max_frame_size, external_state)
}

/// Adds a new Ethernet device to the stack.
pub fn add_ethernet_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mac: UnicastAddr<Mac>,
    max_frame_size: ethernet::MaxFrameSize,
) -> DeviceId<NonSyncCtx>
where
    NonSyncCtx::DeviceState: Default,
{
    add_ethernet_device_with_state(sync_ctx, mac, max_frame_size, Default::default)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
pub fn add_loopback_device_with_state<
    NonSyncCtx: NonSyncContext,
    F: FnOnce() -> NonSyncCtx::DeviceState,
>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mtu: Mtu,
    external_state: F,
) -> Result<DeviceId<NonSyncCtx>, crate::error::ExistsError> {
    sync_ctx.state.device.add_loopback_device(mtu, external_state)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
pub fn add_loopback_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mtu: Mtu,
) -> Result<DeviceId<NonSyncCtx>, crate::error::ExistsError>
where
    NonSyncCtx::DeviceState: Default,
{
    add_loopback_device_with_state(sync_ctx, mtu, Default::default)
}

/// Receive a device layer frame from the network.
pub fn receive_frame<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    buffer: B,
) -> Result<(), NotSupportedError> {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            Ok(self::ethernet::receive_frame(&mut sync_ctx, ctx, id, buffer))
        }
        DeviceIdInner::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
    }
}

/// Set the promiscuous mode flag on `device`.
// TODO(rheacock): remove `allow(dead_code)` when this is used.
#[allow(dead_code)]
pub(crate) fn set_promiscuous_mode<NonSyncCtx: NonSyncContext>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    enabled: bool,
) -> Result<(), NotSupportedError> {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            Ok(self::ethernet::set_promiscuous_mode(&mut sync_ctx, ctx, id, enabled))
        }
        DeviceIdInner::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
    }
}

/// Adds an IP address and associated subnet to this device.
///
/// For IPv6, this function also joins the solicited-node multicast group and
/// begins performing Duplicate Address Detection (DAD).
pub(crate) fn add_ip_addr_subnet<NonSyncCtx: NonSyncContext, A: IpAddress>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr_sub: AddrSubnet<A>,
) -> Result<(), ExistsError> {
    trace!("add_ip_addr_subnet: adding addr {:?} to device {:?}", addr_sub, device);

    match addr_sub.into() {
        AddrSubnetEither::V4(addr_sub) => {
            crate::ip::device::add_ipv4_addr_subnet(&mut sync_ctx, ctx, device, addr_sub)
        }
        AddrSubnetEither::V6(addr_sub) => crate::ip::device::add_ipv6_addr_subnet(
            &mut sync_ctx,
            ctx,
            device,
            addr_sub,
            AddrConfig::Manual,
        ),
    }
}

/// Removes an IP address and associated subnet from this device.
///
/// Should only be called on user action.
pub(crate) fn del_ip_addr<NonSyncCtx: NonSyncContext, A: IpAddress>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: &SpecifiedAddr<A>,
) -> Result<(), NotFoundError> {
    trace!("del_ip_addr: removing addr {:?} from device {:?}", addr, device);

    match Into::into(*addr) {
        IpAddr::V4(addr) => crate::ip::device::del_ipv4_addr(&mut sync_ctx, ctx, &device, &addr),
        IpAddr::V6(addr) => crate::ip::device::del_ipv6_addr_with_reason(
            &mut sync_ctx,
            ctx,
            &device,
            &addr,
            crate::ip::device::state::DelIpv6AddrReason::ManualAction,
        ),
    }
}

// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext> DualStackDeviceIdContext for &'_ SyncCtx<NonSyncCtx> {
    type DualStackDeviceId = DeviceId<NonSyncCtx>;
}

impl<'a, NonSyncCtx: NonSyncContext, L> DualStackDeviceIdContext
    for Locked<'a, SyncCtx<NonSyncCtx>, L>
{
    type DualStackDeviceId =
        <&'a SyncCtx<NonSyncCtx> as DualStackDeviceIdContext>::DualStackDeviceId;
}

// Temporary blanket impl until we switch over entirely to the traits defined in
// the `context` module.
// TODO(https://fxbug.dev/121448): Remove this when it is unused.
impl<NonSyncCtx: NonSyncContext, I: Ip> IpDeviceIdContext<I> for &'_ SyncCtx<NonSyncCtx> {
    type DeviceId = DeviceId<NonSyncCtx>;
    type WeakDeviceId = WeakDeviceId<NonSyncCtx>;

    fn downgrade_device_id(&self, device_id: &DeviceId<NonSyncCtx>) -> WeakDeviceId<NonSyncCtx> {
        device_id.downgrade()
    }

    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &WeakDeviceId<NonSyncCtx>,
    ) -> Option<DeviceId<NonSyncCtx>> {
        weak_device_id.upgrade()
    }

    fn is_device_installed(&self, device_id: &DeviceId<NonSyncCtx>) -> bool {
        !device_id.removed()
    }
}

impl<'a, NonSyncCtx: NonSyncContext, I: Ip, L> IpDeviceIdContext<I>
    for Locked<'a, SyncCtx<NonSyncCtx>, L>
{
    type DeviceId = <&'a SyncCtx<NonSyncCtx> as IpDeviceIdContext<I>>::DeviceId;
    type WeakDeviceId = <&'a SyncCtx<NonSyncCtx> as IpDeviceIdContext<I>>::WeakDeviceId;

    fn downgrade_device_id(&self, device_id: &DeviceId<NonSyncCtx>) -> WeakDeviceId<NonSyncCtx> {
        device_id.downgrade()
    }

    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &WeakDeviceId<NonSyncCtx>,
    ) -> Option<DeviceId<NonSyncCtx>> {
        weak_device_id.upgrade()
    }

    fn is_device_installed(&self, device_id: &DeviceId<NonSyncCtx>) -> bool {
        !device_id.removed()
    }
}

/// Insert a static entry into this device's ARP table.
///
/// This will cause any conflicting dynamic entry to be removed, and
/// any future conflicting gratuitous ARPs to be ignored.
// TODO(rheacock): remove `cfg(test)` when this is used. Will probably be
// called by a pub fn in the device mod.
#[cfg(test)]
pub(super) fn insert_static_arp_table_entry<NonSyncCtx: NonSyncContext>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: Ipv4Addr,
    mac: UnicastAddr<Mac>,
) -> Result<(), NotSupportedError> {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => Ok(self::ethernet::insert_static_arp_table_entry(
            &mut sync_ctx,
            ctx,
            id,
            addr,
            mac.into(),
        )),
        DeviceIdInner::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
    }
}

/// Insert an entry into this device's NDP table.
///
/// This method only gets called when testing to force set a neighbor's link
/// address so that lookups succeed immediately, without doing address
/// resolution.
// TODO(rheacock): Remove when this is called from non-test code.
#[cfg(test)]
pub(crate) fn insert_ndp_table_entry<NonSyncCtx: NonSyncContext>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: UnicastAddr<Ipv6Addr>,
    mac: Mac,
) -> Result<(), NotSupportedError> {
    match device.inner() {
        DeviceIdInner::Ethernet(id) => {
            Ok(self::ethernet::insert_ndp_table_entry(&mut sync_ctx, ctx, id, addr, mac))
        }
        DeviceIdInner::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
    }
}

/// Gets the IPv4 Configuration for a `device`.
pub fn get_ipv4_configuration<NonSyncCtx: NonSyncContext>(
    mut ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Ipv4DeviceConfiguration {
    crate::ip::device::get_ipv4_configuration(&mut ctx, device)
}

/// Gets the IPv6 Configuration for a `device`.
pub fn get_ipv6_configuration<NonSyncCtx: NonSyncContext>(
    mut ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Ipv6DeviceConfiguration {
    crate::ip::device::get_ipv6_configuration(&mut ctx, device)
}

/// Updates the IPv4 Configuration for a `device`.
pub fn update_ipv4_configuration<
    NonSyncCtx: NonSyncContext,
    F: FnOnce(&mut Ipv4DeviceConfiguration),
>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    update_cb: F,
) {
    crate::ip::device::update_ipv4_configuration(&mut sync_ctx, ctx, device, update_cb)
}

/// Updates the IPv6 Configuration for a `device`.
pub fn update_ipv6_configuration<
    NonSyncCtx: NonSyncContext,
    F: FnOnce(&mut Ipv6DeviceConfiguration),
>(
    mut sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    update_cb: F,
) {
    crate::ip::device::update_ipv6_configuration(&mut sync_ctx, ctx, device, update_cb)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::Ctx;

    /// Calls [`receive_frame`], panicking on error.
    pub(crate) fn receive_frame_or_panic<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
        Ctx { sync_ctx, non_sync_ctx }: &mut Ctx<NonSyncCtx>,
        device: DeviceId<NonSyncCtx>,
        buffer: B,
    ) {
        crate::device::receive_frame(sync_ctx, non_sync_ctx, &device, buffer).unwrap()
    }

    pub fn enable_device<NonSyncCtx: NonSyncContext>(
        mut sync_ctx: &SyncCtx<NonSyncCtx>,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
    ) {
        crate::ip::device::update_ipv4_configuration(&mut sync_ctx, ctx, device, |config| {
            config.ip_config.ip_enabled = true;
        });
        crate::ip::device::update_ipv6_configuration(&mut sync_ctx, ctx, device, |config| {
            config.ip_config.ip_enabled = true;
        });
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::HashMap, vec::Vec};

    use net_declare::net_mac;
    use net_types::ip::SubnetEither;
    use nonzero_ext::nonzero;
    use test_case::test_case;

    use super::*;
    use crate::{
        testutil::{
            FakeEventDispatcherConfig, FakeSyncCtx, TestIpExt as _, FAKE_CONFIG_V4,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        Ctx,
    };

    #[test]
    fn test_origin_tracker() {
        let tracker = OriginTracker::new();
        if cfg!(debug_assertions) {
            assert_ne!(tracker, OriginTracker::new());
        } else {
            assert_eq!(tracker, OriginTracker::new());
        }
        assert_eq!(tracker.clone(), tracker);
    }

    #[test]
    fn test_iter_devices() {
        let Ctx { sync_ctx, non_sync_ctx: _ } = crate::testutil::FakeCtx::default();
        let mut sync_ctx = &sync_ctx;

        fn check(
            sync_ctx: &mut &FakeSyncCtx,
            expected: &[DeviceId<crate::testutil::FakeNonSyncCtx>],
        ) {
            assert_eq!(
                IpDeviceContext::<Ipv4, _>::with_devices(sync_ctx, |devices| devices
                    .collect::<Vec<_>>()),
                expected
            );
            assert_eq!(
                IpDeviceContext::<Ipv6, _>::with_devices(sync_ctx, |devices| devices
                    .collect::<Vec<_>>()),
                expected
            );
        }
        check(&mut sync_ctx, &[][..]);

        let loopback_device = crate::device::add_loopback_device(&mut sync_ctx, Mtu::new(55))
            .expect("error adding loopback device");
        check(&mut sync_ctx, &[loopback_device.clone()][..]);

        let FakeEventDispatcherConfig {
            subnet: _,
            local_ip: _,
            local_mac,
            remote_ip: _,
            remote_mac: _,
        } = FAKE_CONFIG_V4;
        let ethernet_device = crate::device::add_ethernet_device(
            &mut sync_ctx,
            local_mac,
            ethernet::MaxFrameSize::MIN,
        );
        check(&mut sync_ctx, &[ethernet_device, loopback_device][..]);
    }

    fn get_routes<NonSyncCtx: NonSyncContext>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        device: &DeviceId<NonSyncCtx>,
    ) -> HashMap<SubnetEither, Option<IpAddr<SpecifiedAddr<Ipv4Addr>, SpecifiedAddr<Ipv6Addr>>>>
    {
        crate::ip::get_all_routes(&sync_ctx)
            .into_iter()
            .map(|entry| {
                let (subnet, route_device, gateway) = entry.into_subnet_device_gateway();
                assert_eq!(&route_device, device);
                (subnet, gateway)
            })
            .collect::<HashMap<_, _>>()
    }

    #[test]
    fn test_no_default_routes() {
        let Ctx { mut sync_ctx, non_sync_ctx: _ } = crate::testutil::FakeCtx::default();

        let loopback_device = crate::device::add_loopback_device(&mut sync_ctx, Mtu::new(55))
            .expect("error adding loopback device");

        assert_eq!(get_routes(&sync_ctx, &loopback_device), HashMap::new());

        let ethernet_device = crate::device::add_ethernet_device(
            &mut sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            ethernet::MaxFrameSize::MIN,
        );
        assert_eq!(get_routes(&sync_ctx, &ethernet_device), HashMap::new());
    }

    #[test]
    fn remove_device_disables_timers() {
        let Ctx { mut sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();

        let ethernet_device = crate::device::add_ethernet_device(
            &mut sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            ethernet::MaxFrameSize::from_mtu(Mtu::new(1500)).unwrap(),
        );

        // Enable the device, turning on a bunch of features that install
        // timers.
        crate::device::update_ipv4_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &ethernet_device,
            |state| {
                state.ip_config.ip_enabled = true;
                state.ip_config.gmp_enabled = true;
            },
        );
        crate::device::update_ipv6_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &ethernet_device,
            |state| {
                state.ip_config.ip_enabled = true;
                state.ip_config.gmp_enabled = true;
                state.max_router_solicitations = Some(nonzero!(2u8));
                state.slaac_config.enable_stable_addresses = true;
            },
        );

        crate::device::remove_device(&mut sync_ctx, &mut non_sync_ctx, ethernet_device);
        assert_eq!(non_sync_ctx.timer_ctx().timers(), &[]);
    }

    fn add_ethernet(
        sync_ctx: &mut &crate::testutil::FakeSyncCtx,
        _non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        crate::device::add_ethernet_device(
            sync_ctx,
            Ipv6::FAKE_CONFIG.local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        )
    }

    fn add_loopback(
        sync_ctx: &mut &crate::testutil::FakeSyncCtx,
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        let device = crate::device::add_loopback_device(sync_ctx, Ipv6::MINIMUM_LINK_MTU).unwrap();
        crate::device::add_ip_addr_subnet(
            sync_ctx,
            non_sync_ctx,
            &device,
            AddrSubnet::from_witness(Ipv6::LOOPBACK_ADDRESS, Ipv6::LOOPBACK_SUBNET.prefix())
                .unwrap(),
        )
        .unwrap();
        device
    }

    fn check_transmitted_ethernet(
        non_sync_ctx: &crate::testutil::FakeNonSyncCtx,
        _device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        assert_eq!(non_sync_ctx.frames_sent().len(), count);
    }

    fn check_transmitted_loopback(
        non_sync_ctx: &crate::testutil::FakeNonSyncCtx,
        device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        // Loopback frames leave the stack; outgoing frames land in
        // its RX queue.
        if count == 0 {
            assert_eq!(non_sync_ctx.state().rx_available, <[DeviceId::<_>; 0]>::default());
        } else {
            assert_eq!(non_sync_ctx.state().rx_available, [device_id.clone()]);
        }
    }

    #[test_case(add_ethernet, check_transmitted_ethernet, true; "ethernet with queue")]
    #[test_case(add_ethernet, check_transmitted_ethernet, false; "ethernet without queue")]
    #[test_case(add_loopback, check_transmitted_loopback, true; "loopback with queue")]
    #[test_case(add_loopback, check_transmitted_loopback, false; "loopback without queue")]
    fn tx_queue(
        add_device: fn(
            &mut &crate::testutil::FakeSyncCtx,
            &mut crate::testutil::FakeNonSyncCtx,
        ) -> DeviceId<crate::testutil::FakeNonSyncCtx>,
        check_transmitted: fn(
            &crate::testutil::FakeNonSyncCtx,
            &DeviceId<crate::testutil::FakeNonSyncCtx>,
            usize,
        ),
        with_tx_queue: bool,
    ) {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();
        let mut sync_ctx = &sync_ctx;
        let device = add_device(&mut sync_ctx, &mut non_sync_ctx);

        if with_tx_queue {
            crate::device::set_tx_queue_configuration(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &device,
                TransmitQueueConfiguration::Fifo,
            );
        }

        crate::device::update_ipv6_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &device,
            |state| {
                state.ip_config.ip_enabled = true;
                // Enable DAD so that the auto-generated address triggers a DAD
                // message immediately on interface enable.
                state.dad_transmits = Some(nonzero!(1u8));
            },
        );

        if with_tx_queue {
            check_transmitted(&non_sync_ctx, &device, 0);
            assert_eq!(
                core::mem::take(&mut non_sync_ctx.state_mut().tx_available),
                [device.clone()]
            );
            crate::device::transmit_queued_tx_frames(&mut sync_ctx, &mut non_sync_ctx, &device)
                .unwrap();
        }

        check_transmitted(&non_sync_ctx, &device, 1);
        assert_eq!(non_sync_ctx.state_mut().tx_available, <[DeviceId::<_>; 0]>::default());
    }
}
