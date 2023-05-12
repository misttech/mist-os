// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The device layer.

pub(crate) mod arp;
pub mod ethernet;
pub(crate) mod link;
pub mod loopback;
pub(crate) mod ndp;
pub mod queue;
pub mod socket;
mod state;

use alloc::vec::Vec;
use core::{
    fmt::{self, Debug, Display, Formatter},
    hash::Hash,
    marker::PhantomData,
    num::NonZeroU8,
};

use derivative::Derivative;
use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    Locked,
};
use net_types::{
    ethernet::Mac,
    ip::{
        AddrSubnet, AddrSubnetEither, Ip, IpAddr, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu,
    },
    BroadcastAddr, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{Buf, BufferMut, Serializer};
use packet_formats::{ethernet::EthernetIpExt, utils::NonZeroDuration};
use tracing::{debug, trace};

use crate::{
    context::{InstantContext, RecvFrameContext, SendFrameContext},
    data_structures::{
        id_map::{self, IdMap},
        id_map_collection::IdMapCollectionKey,
    },
    device::{
        ethernet::{
            EthernetDeviceState, EthernetDeviceStateBuilder,
            EthernetIpLinkDeviceDynamicStateContext, EthernetLinkDevice, EthernetTimerId,
        },
        loopback::{LoopbackDevice, LoopbackDeviceId, LoopbackDeviceState, LoopbackWeakDeviceId},
        queue::{
            rx::ReceiveQueueHandler,
            tx::{BufferTransmitQueueHandler, TransmitQueueConfiguration, TransmitQueueHandler},
        },
        socket::HeldSockets,
        state::IpLinkDeviceState,
    },
    error::{ExistsError, NotFoundError, NotSupportedError},
    ip::{
        device::{
            integration::SyncCtxWithIpDeviceConfiguration,
            nud::{BufferNudHandler, DynamicNeighborUpdateSource, NudHandler, NudIpHandler},
            state::{
                AddrConfig, DualStackIpDeviceState, IpDeviceAddresses, Ipv4DeviceConfiguration,
                Ipv6DeviceConfiguration,
            },
            BufferIpDeviceContext, DualStackDeviceContext, DualStackDeviceStateRef,
            IpDeviceConfigurationContext, IpDeviceStateContext, Ipv4DeviceConfigurationUpdate,
            Ipv6DeviceConfigurationContext, Ipv6DeviceConfigurationUpdate, Ipv6DeviceContext,
        },
        forwarding::IpForwardingDeviceContext,
        types::RawMetric,
    },
    sync::{PrimaryRc, RwLock, StrongRc, WeakRc},
    BufferNonSyncContext, NonSyncContext, SyncCtx,
};

/// A device.
///
/// `Device` is used to identify a particular device implementation. It
/// is only intended to exist at the type level, never instantiated at runtime.
pub(crate) trait Device: 'static {}

/// Marker type for a generic device.
pub(crate) enum AnyDevice {}

impl Device for AnyDevice {}

// An identifier for a device.
pub(crate) trait Id: Clone + Display + Debug + Eq + Hash + PartialEq + Send + Sync {
    /// Returns true if the device is a loopback device.
    fn is_loopback(&self) -> bool;
}

pub(crate) trait StrongId: Id {
    type Weak: WeakId<Strong = Self>;
}

pub(crate) trait WeakId: Id + PartialEq<Self::Strong> {
    type Strong: StrongId<Weak = Self>;
}

/// An execution context which provides device ID types type for various
/// netstack internals to share.
pub(crate) trait DeviceIdContext<D: Device> {
    /// The type of device IDs.
    type DeviceId: StrongId<Weak = Self::WeakDeviceId> + 'static;

    /// The type of weakly referenced device IDs.
    type WeakDeviceId: WeakId<Strong = Self::DeviceId> + 'static;

    /// Returns a weak ID for the strong ID.
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId;

    /// Attempts to upgrade the weak device ID to a strong ID.
    ///
    /// Returns `None` if the device has been removed.
    fn upgrade_weak_device_id(&self, weak_device_id: &Self::WeakDeviceId)
        -> Option<Self::DeviceId>;

    /// Returns true if the device has not been removed.
    fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool;
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

impl<
        B: BufferMut,
        NonSyncCtx: BufferNonSyncContext<B>,
        L: LockBefore<crate::lock_ordering::EthernetRxDequeue>,
    > RecvFrameContext<NonSyncCtx, B, RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv4>>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn receive_frame(
        &mut self,
        ctx: &mut NonSyncCtx,
        metadata: RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv4>,
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

impl<
        B: BufferMut,
        NonSyncCtx: BufferNonSyncContext<B>,
        L: LockBefore<crate::lock_ordering::EthernetRxDequeue>,
    > RecvFrameContext<NonSyncCtx, B, RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv6>>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn receive_frame(
        &mut self,
        ctx: &mut NonSyncCtx,
        metadata: RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv6>,
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
            &IpLinkDeviceState<NonSyncCtx, NonSyncCtx::EthernetDeviceState, EthernetDeviceState>,
            L,
        >,
        &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    EthernetDeviceId(_id, state): &EthernetDeviceId<NonSyncCtx>,
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
            &IpLinkDeviceState<NonSyncCtx, NonSyncCtx::EthernetDeviceState, EthernetDeviceState>,
            L,
        >,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &EthernetDeviceId<NonSyncCtx>,
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
            &'_ IpLinkDeviceState<NonSyncCtx, NonSyncCtx::LoopbackDeviceState, LoopbackDeviceState>,
            L,
        >,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
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
            &IpLinkDeviceState<NonSyncCtx, NonSyncCtx::LoopbackDeviceState, LoopbackDeviceState>,
            L,
        >,
        &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    LoopbackDeviceId(state): &LoopbackDeviceId<NonSyncCtx>,
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

pub(crate) fn with_ip_device_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(Locked<&DualStackIpDeviceState<NonSyncCtx::Instant>, L>) -> O,
    L,
>(
    ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    match device {
        DeviceId::Ethernet(id) => with_ethernet_state(ctx, id, |mut state| cb(state.cast())),
        DeviceId::Loopback(id) => with_loopback_state(ctx, id, |mut state| cb(state.cast())),
    }
}

pub(crate) fn with_ip_device_state_and_sync_ctx<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<&DualStackIpDeviceState<NonSyncCtx::Instant>, L>,
        &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    match device {
        DeviceId::Ethernet(id) => {
            with_ethernet_state_and_sync_ctx(ctx, id, |mut state, ctx| cb(state.cast(), ctx))
        }
        DeviceId::Loopback(id) => {
            with_loopback_state_and_sync_ctx(ctx, id, |mut state, ctx| cb(state.cast(), ctx))
        }
    }
}

fn get_mtu<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>(
    ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
) -> Mtu {
    match device {
        DeviceId::Ethernet(id) => self::ethernet::get_mtu(ctx, &id),
        DeviceId::Loopback(id) => self::loopback::get_mtu(ctx, id),
    }
}

fn join_link_multicast_group<
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &DeviceId<NonSyncCtx>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::join_link_multicast(
            sync_ctx,
            ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId(_)) => {}
    }
}

fn leave_link_multicast_group<
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device_id: &DeviceId<NonSyncCtx>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::leave_link_multicast(
            sync_ctx,
            ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId(_)) => {}
    }
}

impl<NonSyncCtx: NonSyncContext> DualStackDeviceContext<NonSyncCtx>
    for Locked<&SyncCtx<NonSyncCtx>, crate::lock_ordering::Unlocked>
{
    fn with_dual_stack_device_state<
        O,
        F: FnOnce(DualStackDeviceStateRef<'_, NonSyncCtx::Instant>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let (ipv4, mut locked) =
                state.read_lock_and::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>();
            let ipv6 = locked.read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>();
            cb(DualStackDeviceStateRef { ipv4: &ipv4, ipv6: &ipv6 })
        })
    }
}

/// Iterator over devices.
///
/// Implements `Iterator<Item=DeviceId<C>>` by pulling from provided loopback
/// and ethernet device ID iterators. This struct only exists as a named type
/// so it can be an associated type on impls of the [`IpDeviceContext`] trait.
pub(crate) struct DevicesIter<'s, C: NonSyncContext> {
    ethernet: id_map::Iter<
        's,
        PrimaryRc<IpLinkDeviceState<C, C::EthernetDeviceState, EthernetDeviceState>>,
    >,
    loopback: core::option::Iter<
        's,
        PrimaryRc<IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>>,
    >,
}

impl<'s, C: NonSyncContext> Iterator for DevicesIter<'s, C> {
    type Item = DeviceId<C>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self { ethernet, loopback } = self;
        ethernet
            .map(|(id, state)| EthernetDeviceId(id, PrimaryRc::clone_strong(state)).into())
            .chain(loopback.map(|state| {
                DeviceId::Loopback(LoopbackDeviceId(PrimaryRc::clone_strong(state))).into()
            }))
            .next()
    }
}

impl<NonSyncCtx: NonSyncContext, L> IpForwardingDeviceContext for Locked<&SyncCtx<NonSyncCtx>, L> {
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric {
        match device_id {
            DeviceId::Ethernet(id) => self::ethernet::get_routing_metric(self, id),
            DeviceId::Loopback(id) => self::loopback::get_routing_metric(self, id),
        }
    }
}

impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>,
    > IpDeviceConfigurationContext<Ipv4, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type DevicesIter<'s> = DevicesIter<'s, NonSyncCtx>;
    type WithIpDeviceConfigurationInnerCtx<'s> =
        SyncCtxWithIpDeviceConfiguration<'s, &'s Ipv4DeviceConfiguration, Ipv4, NonSyncCtx>;
    type WithIpDeviceConfigurationMutInner<'s> =
        SyncCtxWithIpDeviceConfiguration<'s, &'s mut Ipv4DeviceConfiguration, Ipv4, NonSyncCtx>;
    type DeviceAddressAndGroupsAccessor<'s> =
        Locked<&'s SyncCtx<NonSyncCtx>, crate::lock_ordering::DeviceLayerState>;

    fn with_ip_device_configuration<
        O,
        F: FnOnce(&Ipv4DeviceConfiguration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            let state = state.read_lock::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>();
            cb(
                &state,
                SyncCtxWithIpDeviceConfiguration {
                    config: &state,
                    sync_ctx: sync_ctx
                        .cast_locked::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>(),
                },
            )
        })
    }

    fn with_ip_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpDeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            let mut state = state.write_lock::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>();
            cb(SyncCtxWithIpDeviceConfiguration {
                config: &mut state,
                sync_ctx: sync_ctx
                    .cast_locked::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>(),
            })
        })
    }

    fn with_devices_and_state<
        O,
        F: FnOnce(Self::DevicesIter<'_>, Self::DeviceAddressAndGroupsAccessor<'_>) -> O,
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

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.state.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|state| {
            DeviceId::Loopback(LoopbackDeviceId(PrimaryRc::clone_strong(state))).into()
        })
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv4>>>
    IpDeviceStateContext<Ipv4, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_ip_device_addresses<
        O,
        F: FnOnce(&IpDeviceAddresses<NonSyncCtx::Instant, Ipv4>) -> O,
    >(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>();
            cb(&state)
        })
    }

    fn with_ip_device_addresses_mut<
        O,
        F: FnOnce(&mut IpDeviceAddresses<NonSyncCtx::Instant, Ipv4>) -> O,
    >(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>();
            cb(&mut state)
        })
    }

    fn with_default_hop_limit<O, F: FnOnce(&NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let mut state =
                state.read_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv4>>();
            cb(&mut state)
        })
    }

    fn with_default_hop_limit_mut<O, F: FnOnce(&mut NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let mut state =
                state.write_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv4>>();
            cb(&mut state)
        })
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
}

fn send_ip_frame<
    B: BufferMut,
    NonSyncCtx: BufferNonSyncContext<B>,
    S: Serializer<Buffer = B>,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::IpState<A::Version>>
        + LockBefore<crate::lock_ordering::LoopbackTxQueue>,
>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    local_addr: SpecifiedAddr<A>,
    body: S,
) -> Result<(), S>
where
    A::Version: EthernetIpExt,
    for<'a> Locked<&'a SyncCtx<NonSyncCtx>, L>: EthernetIpLinkDeviceDynamicStateContext<NonSyncCtx, DeviceId = EthernetDeviceId<NonSyncCtx>>
        + BufferNudHandler<B, A::Version, EthernetLinkDevice, NonSyncCtx>
        + BufferTransmitQueueHandler<EthernetLinkDevice, B, NonSyncCtx, Meta = ()>,
{
    match device {
        DeviceId::Ethernet(id) => {
            self::ethernet::send_ip_frame::<_, _, _, A, _>(sync_ctx, ctx, &id, local_addr, body)
        }
        DeviceId::Loopback(id) => {
            self::loopback::send_ip_frame::<_, _, A, _, _>(sync_ctx, ctx, id, local_addr, body)
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

impl<
        I: Ip,
        C: NonSyncContext,
        L: LockBefore<crate::lock_ordering::EthernetIpv4Arp>
            + LockBefore<crate::lock_ordering::EthernetIpv6Nud>,
    > NudIpHandler<I, C> for Locked<&SyncCtx<C>, L>
where
    Self: NudHandler<I, EthernetLinkDevice, C>
        + DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<C>>,
{
    fn handle_neighbor_probe(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId<C>,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        match device_id {
            DeviceId::Ethernet(id) => {
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
            DeviceId::Loopback(LoopbackDeviceId(_)) => {}
        }
    }

    fn handle_neighbor_confirmation(
        &mut self,
        ctx: &mut C,
        device_id: &DeviceId<C>,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        match device_id {
            DeviceId::Ethernet(id) => {
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
            DeviceId::Loopback(LoopbackDeviceId(_)) => {}
        }
    }

    fn flush_neighbor_table(&mut self, ctx: &mut C, device_id: &DeviceId<C>) {
        match device_id {
            DeviceId::Ethernet(id) => NudHandler::<I, EthernetLinkDevice, _>::flush(self, ctx, &id),
            DeviceId::Loopback(LoopbackDeviceId(_)) => {}
        }
    }
}

impl<
        B: BufferMut,
        NonSyncCtx: BufferNonSyncContext<B>,
        L: LockBefore<crate::lock_ordering::IpState<Ipv4>>,
    > BufferIpDeviceContext<Ipv4, NonSyncCtx, B> for Locked<&SyncCtx<NonSyncCtx>, L>
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

// Manual Impl fails for some reason :/
impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
    > Ipv6DeviceConfigurationContext<NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type Ipv6DeviceStateCtx<'s> =
        SyncCtxWithIpDeviceConfiguration<'s, &'s Ipv6DeviceConfiguration, Ipv6, NonSyncCtx>;
    type WithIpv6DeviceConfigurationMutInner<'s> =
        SyncCtxWithIpDeviceConfiguration<'s, &'s mut Ipv6DeviceConfiguration, Ipv6, NonSyncCtx>;

    fn with_ipv6_device_configuration<
        O,
        F: FnOnce(&Ipv6DeviceConfiguration, Self::Ipv6DeviceStateCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        IpDeviceConfigurationContext::<Ipv6, _>::with_ip_device_configuration(self, device_id, cb)
    }

    fn with_ipv6_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpv6DeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        IpDeviceConfigurationContext::<Ipv6, _>::with_ip_device_configuration_mut(
            self, device_id, cb,
        )
    }
}

impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
    > IpDeviceConfigurationContext<Ipv6, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type DevicesIter<'s> = DevicesIter<'s, NonSyncCtx>;
    type WithIpDeviceConfigurationInnerCtx<'s> =
        SyncCtxWithIpDeviceConfiguration<'s, &'s Ipv6DeviceConfiguration, Ipv6, NonSyncCtx>;
    type WithIpDeviceConfigurationMutInner<'s> =
        SyncCtxWithIpDeviceConfiguration<'s, &'s mut Ipv6DeviceConfiguration, Ipv6, NonSyncCtx>;
    type DeviceAddressAndGroupsAccessor<'s> =
        Locked<&'s SyncCtx<NonSyncCtx>, crate::lock_ordering::DeviceLayerState>;

    fn with_ip_device_configuration<
        O,
        F: FnOnce(&Ipv6DeviceConfiguration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            let state = state.read_lock::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>();
            cb(
                &state,
                SyncCtxWithIpDeviceConfiguration {
                    config: &state,
                    sync_ctx: sync_ctx
                        .cast_locked::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>(),
                },
            )
        })
    }

    fn with_ip_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpDeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            let mut state = state.write_lock::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>();
            cb(SyncCtxWithIpDeviceConfiguration {
                config: &mut state,
                sync_ctx: sync_ctx
                    .cast_locked::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>(),
            })
        })
    }

    fn with_devices_and_state<
        O,
        F: FnOnce(Self::DevicesIter<'_>, Self::DeviceAddressAndGroupsAccessor<'_>) -> O,
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

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.state.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|state| {
            DeviceId::Loopback(LoopbackDeviceId(PrimaryRc::clone_strong(state))).into()
        })
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv6>>>
    IpDeviceStateContext<Ipv6, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_ip_device_addresses<
        O,
        F: FnOnce(&IpDeviceAddresses<NonSyncCtx::Instant, Ipv6>) -> O,
    >(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>();
            cb(&state)
        })
    }

    fn with_ip_device_addresses_mut<
        O,
        F: FnOnce(&mut IpDeviceAddresses<NonSyncCtx::Instant, Ipv6>) -> O,
    >(
        &mut self,
        device: &DeviceId<NonSyncCtx>,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>();
            cb(&mut state)
        })
    }

    fn with_default_hop_limit<O, F: FnOnce(&NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let mut state =
                state.read_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv6>>();
            cb(&mut state)
        })
    }

    fn with_default_hop_limit_mut<O, F: FnOnce(&mut NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let mut state =
                state.write_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv6>>();
            cb(&mut state)
        })
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

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv6>>>
    Ipv6DeviceContext<NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type LinkLayerAddr = Ipv6DeviceLinkLayerAddr;

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Ipv6DeviceLinkLayerAddr> {
        match device_id {
            DeviceId::Ethernet(id) => {
                Some(Ipv6DeviceLinkLayerAddr::Mac(ethernet::get_mac(self, &id).get()))
            }
            DeviceId::Loopback(LoopbackDeviceId(_)) => None,
        }
    }

    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]> {
        match device_id {
            DeviceId::Ethernet(id) => {
                Some(ethernet::get_mac(self, &id).to_eui64_with_magic(Mac::DEFAULT_EUI_MAGIC))
            }
            DeviceId::Loopback(LoopbackDeviceId(_)) => None,
        }
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        if mtu < Ipv6::MINIMUM_LINK_MTU {
            return;
        }

        match device_id {
            DeviceId::Ethernet(id) => ethernet::set_mtu(self, &id, mtu),
            DeviceId::Loopback(LoopbackDeviceId(_)) => {}
        }
    }

    fn with_retrans_timer<O, F: FnOnce(&NonZeroDuration) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::Ipv6DeviceRetransTimeout>();
            cb(&state)
        })
    }

    fn with_retrans_timer_mut<O, F: FnOnce(&mut NonZeroDuration) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::Ipv6DeviceRetransTimeout>();
            cb(&mut state)
        })
    }
}

impl<
        B: BufferMut,
        NonSyncCtx: BufferNonSyncContext<B>,
        L: LockBefore<crate::lock_ordering::IpState<Ipv6>>,
    > BufferIpDeviceContext<Ipv6, NonSyncCtx, B> for Locked<&SyncCtx<NonSyncCtx>, L>
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

/// A weak device ID identifying an ethernet device.
///
/// This device ID is like [`WeakDeviceId`] but specifically for ethernet
/// devices.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub struct EthernetWeakDeviceId<C: DeviceLayerTypes>(
    usize,
    WeakRc<IpLinkDeviceState<C, C::EthernetDeviceState, EthernetDeviceState>>,
);

impl<C: DeviceLayerTypes> PartialEq for EthernetWeakDeviceId<C> {
    fn eq(&self, EthernetWeakDeviceId(other_id, other_ptr): &EthernetWeakDeviceId<C>) -> bool {
        let EthernetWeakDeviceId(me_id, me_ptr) = self;
        other_id == me_id && WeakRc::ptr_eq(me_ptr, other_ptr)
    }
}

impl<C: DeviceLayerTypes> PartialEq<EthernetDeviceId<C>> for EthernetWeakDeviceId<C> {
    fn eq(&self, other: &EthernetDeviceId<C>) -> bool {
        <EthernetDeviceId<C> as PartialEq<EthernetWeakDeviceId<C>>>::eq(other, self)
    }
}

impl<C: DeviceLayerTypes> Eq for EthernetWeakDeviceId<C> {}

impl<C: DeviceLayerTypes> Debug for EthernetWeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<C: DeviceLayerTypes> Display for EthernetWeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self(id, _ptr) = self;
        write!(f, "Weak Ethernet({id})")
    }
}

impl<C: DeviceLayerTypes> Id for EthernetWeakDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    fn is_loopback(&self) -> bool {
        false
    }
}

impl<C: DeviceLayerTypes> WeakId for EthernetWeakDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    type Strong = EthernetDeviceId<C>;
}

impl<C: DeviceLayerTypes> EthernetWeakDeviceId<C> {
    /// Attempts to upgrade the ID to an [`EthernetDeviceId`], failing if the
    /// device no longer exists.
    pub fn upgrade(&self) -> Option<EthernetDeviceId<C>> {
        let Self(id, rc) = self;
        rc.upgrade().map(|rc| EthernetDeviceId(*id, rc))
    }
}

/// A strong device ID identifying an ethernet device.
///
/// This device ID is like [`DeviceId`] but specifically for ethernet devices.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Hash(bound = ""))]
pub struct EthernetDeviceId<C: DeviceLayerTypes>(
    usize,
    StrongRc<IpLinkDeviceState<C, C::EthernetDeviceState, EthernetDeviceState>>,
);

impl<C: DeviceLayerTypes> PartialEq for EthernetDeviceId<C> {
    fn eq(&self, EthernetDeviceId(other_id, other_ptr): &EthernetDeviceId<C>) -> bool {
        let EthernetDeviceId(me_id, me_ptr) = self;
        other_id == me_id && StrongRc::ptr_eq(me_ptr, other_ptr)
    }
}

impl<C: DeviceLayerTypes> PartialEq<EthernetWeakDeviceId<C>> for EthernetDeviceId<C> {
    fn eq(&self, EthernetWeakDeviceId(other_id, other_ptr): &EthernetWeakDeviceId<C>) -> bool {
        let EthernetDeviceId(me_id, me_ptr) = self;
        other_id == me_id && StrongRc::weak_ptr_eq(me_ptr, other_ptr)
    }
}

impl<C: DeviceLayerTypes> Eq for EthernetDeviceId<C> {}

impl<C: DeviceLayerTypes> Debug for EthernetDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<C: DeviceLayerTypes> Display for EthernetDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self(id, _ptr) = self;
        write!(f, "Ethernet({id})")
    }
}

impl<C: DeviceLayerTypes> Id for EthernetDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    fn is_loopback(&self) -> bool {
        false
    }
}

impl<C: DeviceLayerTypes> StrongId for EthernetDeviceId<C>
where
    C::EthernetDeviceState: Send + Sync,
{
    type Weak = EthernetWeakDeviceId<C>;
}

impl<C: DeviceLayerTypes> EthernetDeviceId<C> {
    /// Returns a reference to the external state for the device.
    pub fn external_state(&self) -> &C::EthernetDeviceState {
        let Self(_id, rc) = self;
        &rc.external_state
    }

    /// Downgrades the ID to an [`EthernetWeakDeviceId`].
    pub fn downgrade(&self) -> EthernetWeakDeviceId<C> {
        let Self(id, rc) = self;
        EthernetWeakDeviceId(*id, StrongRc::downgrade(rc))
    }

    fn removed(&self) -> bool {
        let Self(_id, rc) = self;
        StrongRc::marked_for_destruction(rc)
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
pub(crate) struct DeviceLayerTimerId<C: DeviceLayerTypes>(DeviceLayerTimerIdInner<C>);

#[derive(Derivative)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
enum DeviceLayerTimerIdInner<C: DeviceLayerTypes> {
    /// A timer event for an Ethernet device.
    Ethernet(EthernetTimerId<EthernetDeviceId<C>>),
}

impl<C: DeviceLayerTypes> From<EthernetTimerId<EthernetDeviceId<C>>> for DeviceLayerTimerId<C> {
    fn from(id: EthernetTimerId<EthernetDeviceId<C>>) -> DeviceLayerTimerId<C> {
        DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id))
    }
}

impl<NonSyncCtx: NonSyncContext, L> DeviceIdContext<EthernetLinkDevice>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type DeviceId = EthernetDeviceId<NonSyncCtx>;
    type WeakDeviceId = EthernetWeakDeviceId<NonSyncCtx>;
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        device_id.downgrade()
    }

    fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
        !device_id.removed()
    }

    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        weak_device_id.upgrade()
    }
}

impl<C: socket::NonSyncContext<DeviceId<C>> + DeviceLayerEventDispatcher>
    socket::NonSyncContext<EthernetDeviceId<C>> for C
{
    fn receive_frame(
        &self,
        state: &Self::SocketState,
        device: &EthernetDeviceId<C>,
        frame: socket::Frame<&[u8]>,
        whole_frame: &[u8],
    ) {
        self.receive_frame(state, &device.clone().into(), frame, whole_frame)
    }
}

impl<C: NonSyncContext, B: BufferMut, L>
    SendFrameContext<C, B, socket::DeviceSocketMetadata<DeviceId<C>>> for Locked<&SyncCtx<C>, L>
where
    Self: SendFrameContext<C, B, socket::DeviceSocketMetadata<EthernetDeviceId<C>>>
        + SendFrameContext<C, B, socket::DeviceSocketMetadata<LoopbackDeviceId<C>>>,
{
    fn send_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        metadata: socket::DeviceSocketMetadata<DeviceId<C>>,
        frame: S,
    ) -> Result<(), S> {
        let socket::DeviceSocketMetadata { device_id, header } = metadata;
        match device_id {
            DeviceId::Ethernet(device_id) => SendFrameContext::send_frame(
                self,
                ctx,
                socket::DeviceSocketMetadata { device_id, header },
                frame,
            ),
            DeviceId::Loopback(device_id) => SendFrameContext::send_frame(
                self,
                ctx,
                socket::DeviceSocketMetadata { device_id, header },
                frame,
            ),
        }
    }
}

impl_timer_context!(
    C: NonSyncContext,
    DeviceLayerTimerId<C>,
    EthernetTimerId<EthernetDeviceId<C>>,
    DeviceLayerTimerId(DeviceLayerTimerIdInner::Ethernet(id)),
    id
);

/// Handle a timer event firing in the device layer.
pub(crate) fn handle_timer<NonSyncCtx: NonSyncContext>(
    sync_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, crate::lock_ordering::Unlocked>,
    ctx: &mut NonSyncCtx,
    DeviceLayerTimerId(id): DeviceLayerTimerId<NonSyncCtx>,
) {
    match id {
        DeviceLayerTimerIdInner::Ethernet(id) => ethernet::handle_timer(sync_ctx, ctx, id),
    }
}

/// A weak ID identifying a device.
///
/// This device ID makes no claim about the live-ness of the underlying device.
/// See [`DeviceId`] for a device ID that acts as a witness to the live-ness of
/// a device.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum WeakDeviceId<C: DeviceLayerTypes> {
    Ethernet(EthernetWeakDeviceId<C>),
    Loopback(LoopbackWeakDeviceId<C>),
}

impl<C: DeviceLayerTypes> PartialEq<DeviceId<C>> for WeakDeviceId<C> {
    fn eq(&self, other: &DeviceId<C>) -> bool {
        <DeviceId<C> as PartialEq<WeakDeviceId<C>>>::eq(other, self)
    }
}

impl<C: DeviceLayerTypes> From<EthernetWeakDeviceId<C>> for WeakDeviceId<C> {
    fn from(id: EthernetWeakDeviceId<C>) -> WeakDeviceId<C> {
        WeakDeviceId::Ethernet(id)
    }
}

impl<C: DeviceLayerTypes> From<LoopbackWeakDeviceId<C>> for WeakDeviceId<C> {
    fn from(id: LoopbackWeakDeviceId<C>) -> WeakDeviceId<C> {
        WeakDeviceId::Loopback(id)
    }
}

impl<C: DeviceLayerTypes> WeakDeviceId<C> {
    /// Attempts to upgrade the ID.
    pub fn upgrade(&self) -> Option<DeviceId<C>> {
        match self {
            WeakDeviceId::Ethernet(id) => id.upgrade().map(Into::into),
            WeakDeviceId::Loopback(id) => id.upgrade().map(Into::into),
        }
    }
}

impl<C: DeviceLayerTypes> Id for WeakDeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self {
            WeakDeviceId::Loopback(LoopbackWeakDeviceId(_)) => true,
            WeakDeviceId::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerTypes> WeakId for WeakDeviceId<C> {
    type Strong = DeviceId<C>;
}

impl<C: DeviceLayerTypes> Display for WeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            WeakDeviceId::Ethernet(id) => Display::fmt(id, f),
            WeakDeviceId::Loopback(id) => Display::fmt(id, f),
        }
    }
}

impl<C: DeviceLayerTypes> Debug for WeakDeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        Display::fmt(self, f)
    }
}

/// A strong ID identifying a device.
///
/// Holders may safely assume that the underlying device is "alive" in the sense
/// that the device is still recognized by the stack. That is, operations that
/// use this device ID will never fail as a result of "unrecognized device"-like
/// errors.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Eq(bound = ""), PartialEq(bound = ""), Hash(bound = ""))]
#[allow(missing_docs)]
pub enum DeviceId<C: DeviceLayerTypes> {
    Ethernet(EthernetDeviceId<C>),
    Loopback(LoopbackDeviceId<C>),
}

impl<C: DeviceLayerTypes> PartialEq<WeakDeviceId<C>> for DeviceId<C> {
    fn eq(&self, other: &WeakDeviceId<C>) -> bool {
        match (self, other) {
            (DeviceId::Ethernet(strong), WeakDeviceId::Ethernet(weak)) => strong == weak,
            (DeviceId::Loopback(strong), WeakDeviceId::Loopback(weak)) => strong == weak,
            (DeviceId::Loopback(_), WeakDeviceId::Ethernet(_))
            | (DeviceId::Ethernet(_), WeakDeviceId::Loopback(_)) => false,
        }
    }
}

impl<C: DeviceLayerTypes> From<EthernetDeviceId<C>> for DeviceId<C> {
    fn from(id: EthernetDeviceId<C>) -> DeviceId<C> {
        DeviceId::Ethernet(id)
    }
}

impl<C: DeviceLayerTypes> From<LoopbackDeviceId<C>> for DeviceId<C> {
    fn from(id: LoopbackDeviceId<C>) -> DeviceId<C> {
        DeviceId::Loopback(id)
    }
}

impl<C: DeviceLayerTypes> DeviceId<C> {
    /// Downgrade to a [`WeakDeviceId`].
    pub fn downgrade(&self) -> WeakDeviceId<C> {
        match self {
            DeviceId::Ethernet(id) => id.downgrade().into(),
            DeviceId::Loopback(id) => id.downgrade().into(),
        }
    }

    fn removed(&self) -> bool {
        match self {
            DeviceId::Ethernet(id) => id.removed(),
            DeviceId::Loopback(id) => id.removed(),
        }
    }
}

impl<C: DeviceLayerTypes> Id for DeviceId<C> {
    fn is_loopback(&self) -> bool {
        match self {
            DeviceId::Loopback(LoopbackDeviceId(_)) => true,
            DeviceId::Ethernet(_) => false,
        }
    }
}

impl<C: DeviceLayerTypes> StrongId for DeviceId<C> {
    type Weak = WeakDeviceId<C>;
}

impl<C: DeviceLayerTypes> Display for DeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            DeviceId::Ethernet(id) => Display::fmt(id, f),
            DeviceId::Loopback(id) => Display::fmt(id, f),
        }
    }
}

impl<C: DeviceLayerTypes> Debug for DeviceId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        Display::fmt(self, f)
    }
}

impl<C: DeviceLayerTypes> IdMapCollectionKey for DeviceId<C> {
    const VARIANT_COUNT: usize = 2;

    fn get_id(&self) -> usize {
        match self {
            DeviceId::Ethernet(EthernetDeviceId(id, _)) => *id,
            DeviceId::Loopback(LoopbackDeviceId(_)) => 0,
        }
    }

    fn get_variant(&self) -> usize {
        match self {
            DeviceId::Ethernet(_) => 0,
            DeviceId::Loopback(LoopbackDeviceId(_)) => 1,
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
pub enum FrameDestination {
    /// A unicast address - one which is neither multicast nor broadcast.
    Individual {
        /// Whether the frame's destination address belongs to the receiver.
        local: bool,
    },
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

    pub(crate) fn from_dest(destination: Mac, local_mac: Mac) -> Self {
        BroadcastAddr::new(destination)
            .map(Into::into)
            .or_else(|| MulticastAddr::new(destination).map(Into::into))
            .unwrap_or_else(|| FrameDestination::Individual { local: destination == local_mac })
    }
}

impl From<BroadcastAddr<Mac>> for FrameDestination {
    fn from(_value: BroadcastAddr<Mac>) -> Self {
        Self::Broadcast
    }
}

impl From<MulticastAddr<Mac>> for FrameDestination {
    fn from(_value: MulticastAddr<Mac>) -> Self {
        Self::Multicast
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct Devices<C: DeviceLayerTypes> {
    ethernet: IdMap<PrimaryRc<IpLinkDeviceState<C, C::EthernetDeviceState, EthernetDeviceState>>>,
    loopback: Option<PrimaryRc<IpLinkDeviceState<C, C::LoopbackDeviceState, LoopbackDeviceState>>>,
}

/// The state associated with the device layer.
pub(crate) struct DeviceLayerState<C: DeviceLayerTypes + socket::NonSyncContext<DeviceId<C>>> {
    devices: RwLock<Devices<C>>,
    origin: OriginTracker,
    shared_sockets: HeldSockets<C>,
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

impl<C: DeviceLayerTypes + socket::NonSyncContext<DeviceId<C>>>
    RwLockFor<crate::lock_ordering::DeviceLayerState> for DeviceLayerState<C>
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

impl<C: DeviceLayerTypes + socket::NonSyncContext<DeviceId<C>>> DeviceLayerState<C> {
    /// Creates a new [`DeviceLayerState`] instance.
    pub(crate) fn new() -> Self {
        Self {
            devices: Default::default(),
            origin: OriginTracker::new(),
            shared_sockets: Default::default(),
        }
    }

    /// Add a new ethernet device to the device layer.
    ///
    /// `add` adds a new `EthernetDeviceState` with the given MAC address and
    /// maximum frame size. The frame size is the limit on the size of the data
    /// payload and the header but not the FCS.
    pub(crate) fn add_ethernet_device<F: FnOnce() -> C::EthernetDeviceState>(
        &self,
        mac: UnicastAddr<Mac>,
        max_frame_size: ethernet::MaxFrameSize,
        metric: RawMetric,
        external_state: F,
    ) -> EthernetDeviceId<C> {
        let Devices { ethernet, loopback: _ } = &mut *self.devices.write();

        let ptr = PrimaryRc::new(IpLinkDeviceState::new(
            EthernetDeviceStateBuilder::new(mac, max_frame_size, metric).build(),
            external_state(),
            self.origin.clone(),
        ));
        let strong_ptr = PrimaryRc::clone_strong(&ptr);
        let id = ethernet.push(ptr);
        debug!("adding Ethernet device with ID {} and MTU {:?}", id, max_frame_size);
        EthernetDeviceId(id, strong_ptr).into()
    }

    /// Adds a new loopback device to the device layer.
    pub(crate) fn add_loopback_device<F: FnOnce() -> C::LoopbackDeviceState>(
        &self,
        mtu: Mtu,
        metric: RawMetric,
        external_state: F,
    ) -> Result<LoopbackDeviceId<C>, ExistsError> {
        let Devices { ethernet: _, loopback } = &mut *self.devices.write();

        if let Some(_) = loopback {
            return Err(ExistsError);
        }

        let ptr = PrimaryRc::new(IpLinkDeviceState::new(
            LoopbackDeviceState::new(mtu, metric),
            external_state(),
            self.origin.clone(),
        ));
        let id = PrimaryRc::clone_strong(&ptr);

        *loopback = Some(ptr);

        debug!("added loopback device");

        Ok(LoopbackDeviceId(id))
    }
}

/// Provides associated types used in the device layer.
pub trait DeviceLayerStateTypes: InstantContext {
    /// The state associated with loopback devices.
    type LoopbackDeviceState: Send + Sync;

    /// The state associated with ethernet devices.
    type EthernetDeviceState: Send + Sync;
}

/// Provides associated types used in the device layer.
///
/// This trait groups together state types used throughout the device layer. It
/// is blanket-implemented for all types that implement
/// [`socket::DeviceSocketTypes`] and [`DeviceLayerStateTypes`].
pub trait DeviceLayerTypes: DeviceLayerStateTypes + socket::DeviceSocketTypes {}
impl<C: DeviceLayerStateTypes + socket::DeviceSocketTypes> DeviceLayerTypes for C {}

/// An event dispatcher for the device layer.
///
/// See the `EventDispatcher` trait in the crate root for more details.
pub trait DeviceLayerEventDispatcher: DeviceLayerTypes + Sized {
    /// Signals to the dispatcher that RX frames are available and ready to be
    /// handled by [`handle_queued_rx_packets`].
    ///
    /// Implementations must make sure that [`handle_queued_rx_packets`] is
    /// scheduled to be called as soon as possible so that enqueued RX frames
    /// are promptly handled.
    fn wake_rx_task(&mut self, device: &LoopbackDeviceId<Self>);

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
    /// original buffer is returned in the `Err` variant. All other errors (for
    /// example, errors in allocating a buffer) are silently ignored and
    /// reported as success. Implementations are expected to gracefully handle
    /// non-conformant but correctable input, e.g. by padding too-small frames.
    fn send_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
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
    match device {
        DeviceId::Ethernet(id) => TransmitQueueHandler::<EthernetLinkDevice, _>::set_configuration(
            sync_ctx, ctx, id, config,
        ),
        DeviceId::Loopback(id) => {
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
    match device {
        DeviceId::Ethernet(id) => {
            TransmitQueueHandler::<EthernetLinkDevice, _>::transmit_queued_frames(sync_ctx, ctx, id)
        }
        DeviceId::Loopback(id) => {
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
    device: &LoopbackDeviceId<NonSyncCtx>,
) {
    ReceiveQueueHandler::<LoopbackDevice, _>::handle_queued_rx_frames(
        &mut Locked::new(sync_ctx),
        ctx,
        device,
    )
}

/// Removes an ethernet device from the device layer.
///
/// # Panics
///
/// Panics if the caller holds strong device IDs for `device`.
pub fn remove_ethernet_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: EthernetDeviceId<NonSyncCtx>,
) -> NonSyncCtx::EthernetDeviceState {
    // Start cleaning up the device by disabling IP state. This removes timers
    // for the device that would otherwise hold references to defunct device
    // state.
    {
        let mut sync_ctx = Locked::new(sync_ctx);

        let device = device.clone().into();
        crate::ip::device::clear_ipv4_device_state(&mut sync_ctx, ctx, &device);
        crate::ip::device::clear_ipv6_device_state(&mut sync_ctx, ctx, &device);

        // Uninstall all routes associated with the device.
        crate::ip::forwarding::del_device_routes::<Ipv4, _, _>(&mut sync_ctx, ctx, &device);
        crate::ip::forwarding::del_device_routes::<Ipv6, _, _>(&mut sync_ctx, ctx, &device);
    }

    let EthernetDeviceId(id, rc) = device;
    let mut devices = sync_ctx.state.device.devices.write();
    let removed =
        devices.ethernet.remove(id).unwrap_or_else(|| panic!("no such Ethernet device: {}", id));
    assert!(PrimaryRc::ptr_eq(&removed, &rc));
    core::mem::drop(rc);
    debug!("removing Ethernet device with ID {}", id);
    PrimaryRc::unwrap(removed).external_state
}

/// Adds a new Ethernet device to the stack.
pub fn add_ethernet_device_with_state<
    NonSyncCtx: NonSyncContext,
    F: FnOnce() -> NonSyncCtx::EthernetDeviceState,
>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mac: UnicastAddr<Mac>,
    max_frame_size: ethernet::MaxFrameSize,
    metric: RawMetric,
    external_state: F,
) -> EthernetDeviceId<NonSyncCtx> {
    sync_ctx.state.device.add_ethernet_device(mac, max_frame_size, metric, external_state)
}

/// Adds a new Ethernet device to the stack.
#[cfg(test)]
pub(crate) fn add_ethernet_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mac: UnicastAddr<Mac>,
    max_frame_size: ethernet::MaxFrameSize,
    metric: RawMetric,
) -> EthernetDeviceId<NonSyncCtx>
where
    NonSyncCtx::EthernetDeviceState: Default,
{
    add_ethernet_device_with_state(sync_ctx, mac, max_frame_size, metric, Default::default)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
pub fn add_loopback_device_with_state<
    NonSyncCtx: NonSyncContext,
    F: FnOnce() -> NonSyncCtx::LoopbackDeviceState,
>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mtu: Mtu,
    metric: RawMetric,
    external_state: F,
) -> Result<LoopbackDeviceId<NonSyncCtx>, crate::error::ExistsError> {
    sync_ctx.state.device.add_loopback_device(mtu, metric, external_state)
}

/// Adds a new loopback device to the stack.
///
/// Adds a new loopback device to the stack. Only one loopback device may be
/// installed at any point in time, so if there is one already, an error is
/// returned.
#[cfg(test)]
pub(crate) fn add_loopback_device<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    mtu: Mtu,
    metric: RawMetric,
) -> Result<LoopbackDeviceId<NonSyncCtx>, crate::error::ExistsError>
where
    NonSyncCtx::LoopbackDeviceState: Default,
{
    add_loopback_device_with_state(sync_ctx, mtu, metric, Default::default)
}

/// Receive a device layer frame from the network.
pub fn receive_frame<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &EthernetDeviceId<NonSyncCtx>,
    buffer: B,
) {
    self::ethernet::receive_frame(&mut Locked::new(sync_ctx), ctx, device, buffer)
}

/// Set the promiscuous mode flag on `device`.
// TODO(rheacock): remove `allow(dead_code)` when this is used.
#[allow(dead_code)]
pub(crate) fn set_promiscuous_mode<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    enabled: bool,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => {
            Ok(self::ethernet::set_promiscuous_mode(&mut Locked::new(sync_ctx), ctx, id, enabled))
        }
        DeviceId::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
    }
}

/// Adds an IP address and associated subnet to this device.
///
/// For IPv6, this function also joins the solicited-node multicast group and
/// begins performing Duplicate Address Detection (DAD).
pub(crate) fn add_ip_addr_subnet<NonSyncCtx: NonSyncContext, A: IpAddress>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr_sub: AddrSubnet<A>,
) -> Result<(), ExistsError> {
    trace!("add_ip_addr_subnet: adding addr {:?} to device {:?}", addr_sub, device);
    let mut sync_ctx = Locked::new(sync_ctx);

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
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: &SpecifiedAddr<A>,
) -> Result<(), NotFoundError> {
    trace!("del_ip_addr: removing addr {:?} from device {:?}", addr, device);
    let mut sync_ctx = Locked::new(sync_ctx);

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

impl<NonSyncCtx: NonSyncContext, L> DeviceIdContext<AnyDevice> for Locked<&SyncCtx<NonSyncCtx>, L> {
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

/// Insert a static entry into this device's ARP table.
///
/// This will cause any conflicting dynamic entry to be removed, and
/// any future conflicting gratuitous ARPs to be ignored.
// TODO(rheacock): remove `cfg(test)` when this is used. Will probably be
// called by a pub fn in the device mod.
#[cfg(test)]
pub(super) fn insert_static_arp_table_entry<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: Ipv4Addr,
    mac: UnicastAddr<Mac>,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::insert_static_arp_table_entry(
            &mut Locked::new(sync_ctx),
            ctx,
            id,
            addr,
            mac.into(),
        )),
        DeviceId::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
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
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    addr: UnicastAddr<Ipv6Addr>,
    mac: Mac,
) -> Result<(), NotSupportedError> {
    match device {
        DeviceId::Ethernet(id) => Ok(self::ethernet::insert_ndp_table_entry(
            &mut Locked::new(sync_ctx),
            ctx,
            id,
            addr,
            mac,
        )),
        DeviceId::Loopback(LoopbackDeviceId(_)) => Err(NotSupportedError),
    }
}

/// Gets the IPv4 Configuration for a `device`.
pub fn get_ipv4_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Ipv4DeviceConfiguration {
    crate::ip::device::get_ipv4_configuration(&mut Locked::new(sync_ctx), device)
}

/// Gets the IPv6 Configuration for a `device`.
pub fn get_ipv6_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    device: &DeviceId<NonSyncCtx>,
) -> Ipv6DeviceConfiguration {
    crate::ip::device::get_ipv6_configuration(&mut Locked::new(sync_ctx), device)
}

/// Updates the IPv4 configuration for a device.
///
/// Each field in [`Ipv4DeviceConfigurationUpdate`] represents an optionally
/// updateable configuration. If the field has a `Some(_)` value, then an
/// attempt will be made to update that configuration on the device. A `None`
/// value indicates that an update for the configuration is not requested.
///
/// Note that some fields have the type `Option<Option<T>>`. In this case,
/// as long as the outer `Option` is `Some`, then an attempt will be made to
/// update the configuration.
///
/// The IP device configuration is left unchanged if an `Err` is returned.
/// Otherwise, the previous values are returned for configurations that were
/// requested to be updated.
pub fn update_ipv4_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    config: Ipv4DeviceConfigurationUpdate,
) -> Result<Ipv4DeviceConfigurationUpdate, NotSupportedError> {
    crate::ip::device::update_ipv4_configuration(&mut Locked::new(sync_ctx), ctx, device, config)
}

/// Updates the IPv6 configuration for a device.
///
/// Each field in [`Ipv6DeviceConfigurationUpdate`] represents an optionally
/// updateable configuration. If the field has a `Some(_)` value, then an
/// attempt will be made to update that configuration on the device. A `None`
/// value indicates that an update for the configuration is not requested.
///
/// Note that some fields have the type `Option<Option<T>>`. In this case,
/// as long as the outer `Option` is `Some`, then an attempt will be made to
/// update the configuration.
///
/// The IP device configuration is left unchanged if an `Err` is returned.
/// Otherwise, the previous values are returned for configurations that were
/// requested to be updated.
pub fn update_ipv6_configuration<NonSyncCtx: NonSyncContext>(
    sync_ctx: &SyncCtx<NonSyncCtx>,
    ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    config: Ipv6DeviceConfigurationUpdate,
) -> Result<Ipv6DeviceConfigurationUpdate, NotSupportedError> {
    crate::ip::device::update_ipv6_configuration(&mut Locked::new(sync_ctx), ctx, device, config)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use net_types::ip::IpVersion;

    use crate::{ip::device::IpDeviceConfigurationUpdate, testutil::Ctx};

    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
    pub(crate) struct FakeWeakDeviceId<D>(pub(crate) D);

    impl<D: PartialEq> PartialEq<D> for FakeWeakDeviceId<D> {
        fn eq(&self, other: &D) -> bool {
            let Self(this) = self;
            this == other
        }
    }

    impl<D: Debug> core::fmt::Display for FakeWeakDeviceId<D> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            core::fmt::Debug::fmt(self, f)
        }
    }

    impl<D: StrongId<Weak = Self>> WeakId for FakeWeakDeviceId<D> {
        type Strong = D;
    }

    impl<D: Id> Id for FakeWeakDeviceId<D> {
        fn is_loopback(&self) -> bool {
            let Self(inner) = self;
            inner.is_loopback()
        }
    }

    /// A fake device ID for use in testing.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
    pub(crate) struct FakeDeviceId;

    impl StrongId for FakeDeviceId {
        type Weak = FakeWeakDeviceId<Self>;
    }

    impl Id for FakeDeviceId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    impl Display for FakeDeviceId {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "FakeDeviceId")
        }
    }

    pub(crate) trait FakeStrongDeviceId: StrongId<Weak = FakeWeakDeviceId<Self>> {}

    impl<D: StrongId<Weak = FakeWeakDeviceId<Self>>> FakeStrongDeviceId for D {}

    /// Calls [`receive_frame`], with a [`Ctx`].
    pub(crate) fn receive_frame<B: BufferMut, NonSyncCtx: BufferNonSyncContext<B>>(
        Ctx { sync_ctx, non_sync_ctx }: &mut Ctx<NonSyncCtx>,
        device: EthernetDeviceId<NonSyncCtx>,
        buffer: B,
    ) {
        crate::device::receive_frame(sync_ctx, non_sync_ctx, &device, buffer)
    }

    pub fn enable_device<NonSyncCtx: NonSyncContext>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
    ) {
        let ip_config =
            Some(IpDeviceConfigurationUpdate { ip_enabled: Some(true), ..Default::default() });
        let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
            sync_ctx,
            ctx,
            device,
            Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
        )
        .unwrap();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            ctx,
            device,
            Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
        )
        .unwrap();
    }

    /// Enables or disables IP packet routing on `device`.
    pub(crate) fn set_forwarding_enabled<NonSyncCtx: NonSyncContext, I: Ip>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        enabled: bool,
    ) -> Result<(), NotSupportedError> {
        let ip_config = Some(IpDeviceConfigurationUpdate {
            forwarding_enabled: Some(enabled),
            ..Default::default()
        });
        match I::VERSION {
            IpVersion::V4 => {
                let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
                    sync_ctx,
                    ctx,
                    device,
                    Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
                )
                .unwrap();
            }
            IpVersion::V6 => {
                let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
                    sync_ctx,
                    ctx,
                    device,
                    Ipv6DeviceConfigurationUpdate { ip_config, ..Default::default() },
                )
                .unwrap();
            }
        }

        Ok(())
    }

    /// Returns whether IP packet routing is enabled on `device`.
    pub(crate) fn is_forwarding_enabled<NonSyncCtx: NonSyncContext, I: Ip>(
        sync_ctx: &SyncCtx<NonSyncCtx>,
        device: &DeviceId<NonSyncCtx>,
    ) -> bool {
        let mut sync_ctx = Locked::new(sync_ctx);
        match I::VERSION {
            IpVersion::V4 => {
                crate::ip::device::is_ip_forwarding_enabled::<Ipv4, _, _>(&mut sync_ctx, device)
            }
            IpVersion::V6 => {
                crate::ip::device::is_ip_forwarding_enabled::<Ipv6, _, _>(&mut sync_ctx, device)
            }
        }
    }

    /// A device ID type that supports identifying more than one distinct
    /// device.
    #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
    pub(crate) enum MultipleDevicesId {
        A,
        B,
        C,
    }

    impl MultipleDevicesId {
        pub(crate) fn all() -> [Self; 3] {
            [Self::A, Self::B, Self::C]
        }
    }

    impl core::fmt::Display for MultipleDevicesId {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            core::fmt::Debug::fmt(self, f)
        }
    }

    impl Id for MultipleDevicesId {
        fn is_loopback(&self) -> bool {
            false
        }
    }

    impl StrongId for MultipleDevicesId {
        type Weak = FakeWeakDeviceId<Self>;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use net_declare::net_mac;
    use nonzero_ext::nonzero;
    use test_case::test_case;

    use super::*;
    use crate::{
        ip::device::{slaac::SlaacConfiguration, IpDeviceConfigurationUpdate},
        testutil::{
            Ctx, TestIpExt as _, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
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
    fn frame_destination_from_dest() {
        const LOCAL_ADDR: Mac = net_mac!("88:88:88:88:88:88");

        assert_eq!(
            FrameDestination::from_dest(
                UnicastAddr::new(net_mac!("00:11:22:33:44:55")).unwrap().get(),
                LOCAL_ADDR
            ),
            FrameDestination::Individual { local: false }
        );
        assert_eq!(
            FrameDestination::from_dest(LOCAL_ADDR, LOCAL_ADDR),
            FrameDestination::Individual { local: true }
        );
        assert_eq!(
            FrameDestination::from_dest(Mac::BROADCAST, LOCAL_ADDR),
            FrameDestination::Broadcast,
        );
        assert_eq!(
            FrameDestination::from_dest(
                MulticastAddr::new(net_mac!("11:11:11:11:11:11")).unwrap().get(),
                LOCAL_ADDR
            ),
            FrameDestination::Multicast
        );
    }

    #[test]
    fn test_no_default_routes() {
        let Ctx { sync_ctx, non_sync_ctx: _ } = crate::testutil::FakeCtx::default();

        let _loopback_device: LoopbackDeviceId<_> =
            crate::device::add_loopback_device(&sync_ctx, Mtu::new(55), DEFAULT_INTERFACE_METRIC)
                .expect("error adding loopback device");

        assert_eq!(crate::ip::get_all_routes(&sync_ctx), []);
        let _ethernet_device: EthernetDeviceId<_> = crate::device::add_ethernet_device(
            &sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            ethernet::MaxFrameSize::MIN,
            DEFAULT_INTERFACE_METRIC,
        );
        assert_eq!(crate::ip::get_all_routes(&sync_ctx), []);
    }

    #[test]
    fn remove_ethernet_device_disables_timers() {
        let Ctx { sync_ctx, mut non_sync_ctx } = crate::testutil::FakeCtx::default();

        let ethernet_device = crate::device::add_ethernet_device(
            &sync_ctx,
            UnicastAddr::new(net_mac!("aa:bb:cc:dd:ee:ff")).expect("MAC is unicast"),
            ethernet::MaxFrameSize::from_mtu(Mtu::new(1500)).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        );

        {
            let device = ethernet_device.clone().into();
            // Enable the device, turning on a bunch of features that install
            // timers.
            let ip_config = Some(IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                gmp_enabled: Some(true),
                ..Default::default()
            });
            let _: Ipv4DeviceConfigurationUpdate = update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                Ipv4DeviceConfigurationUpdate { ip_config, ..Default::default() },
            )
            .unwrap();
            let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                Ipv6DeviceConfigurationUpdate {
                    max_router_solicitations: Some(Some(nonzero!(2u8))),
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config,
                    ..Default::default()
                },
            )
            .unwrap();
        }

        crate::device::remove_ethernet_device(&sync_ctx, &mut non_sync_ctx, ethernet_device);
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
            DEFAULT_INTERFACE_METRIC,
        )
        .into()
    }

    fn add_loopback(
        sync_ctx: &mut &crate::testutil::FakeSyncCtx,
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
    ) -> DeviceId<crate::testutil::FakeNonSyncCtx> {
        let device = crate::device::add_loopback_device(
            sync_ctx,
            Ipv6::MINIMUM_LINK_MTU,
            DEFAULT_INTERFACE_METRIC,
        )
        .unwrap()
        .into();
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
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
        _device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        assert_eq!(non_sync_ctx.frames_sent().len(), count);
    }

    fn check_transmitted_loopback(
        non_sync_ctx: &mut crate::testutil::FakeNonSyncCtx,
        device_id: &DeviceId<crate::testutil::FakeNonSyncCtx>,
        count: usize,
    ) {
        // Loopback frames leave the stack; outgoing frames land in
        // its RX queue.
        let rx_available = core::mem::take(&mut non_sync_ctx.state_mut().rx_available);
        if count == 0 {
            assert_eq!(rx_available, <[LoopbackDeviceId::<_>; 0]>::default());
        } else {
            assert_eq!(
                rx_available.into_iter().map(DeviceId::Loopback).collect::<Vec<_>>(),
                [device_id.clone()]
            );
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
            &mut crate::testutil::FakeNonSyncCtx,
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
                &sync_ctx,
                &mut non_sync_ctx,
                &device,
                TransmitQueueConfiguration::Fifo,
            );
        }

        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            &sync_ctx,
            &mut non_sync_ctx,
            &device,
            Ipv6DeviceConfigurationUpdate {
                // Enable DAD so that the auto-generated address triggers a DAD
                // message immediately on interface enable.
                dad_transmits: Some(Some(nonzero!(1u8))),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();

        if with_tx_queue {
            check_transmitted(&mut non_sync_ctx, &device, 0);
            assert_eq!(
                core::mem::take(&mut non_sync_ctx.state_mut().tx_available),
                [device.clone()]
            );
            crate::device::transmit_queued_tx_frames(&sync_ctx, &mut non_sync_ctx, &device)
                .unwrap();
        }

        check_transmitted(&mut non_sync_ctx, &device, 1);
        assert_eq!(non_sync_ctx.state_mut().tx_available, <[DeviceId::<_>; 0]>::default());
    }
}
