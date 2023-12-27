// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of device layer traits for [`SyncCtx`].

use alloc::boxed::Box;
use core::{num::NonZeroU8, ops::Deref as _};

use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    Locked,
};
use net_types::{
    ethernet::Mac,
    ip::{AddrSubnet, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu},
    MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{BufferMut, Serializer};
use packet_formats::ethernet::EthernetIpExt;

use crate::{
    context::{CounterContext, RecvFrameContext, SendFrameContext},
    device::{
        ethernet::{
            self, EthernetIpLinkDeviceDynamicStateContext, EthernetLinkDevice, SyncCtxWithDeviceId,
        },
        loopback::{self, LoopbackDevice, LoopbackDeviceId},
        queue::tx::TransmitQueueHandler,
        socket,
        state::IpLinkDeviceState,
        AnyDevice, DeviceCounters, DeviceId, DeviceIdContext, DeviceLayerEventDispatcher,
        DeviceLayerState, DeviceLayerTypes, Devices, DevicesIter, EthernetDeviceId,
        EthernetWeakDeviceId, Ipv6DeviceLinkLayerAddr, OriginTracker, RecvIpFrameMeta,
        WeakDeviceId,
    },
    error::{ExistsError, NotFoundError},
    ip::device::{
        integration::SyncCtxWithIpDeviceConfiguration,
        nud::{ConfirmationFlags, DynamicNeighborUpdateSource, NudHandler, NudIpHandler},
        state::{
            AssignedAddress as _, DualStackIpDeviceState, IpDeviceFlags, Ipv4AddressEntry,
            Ipv4AddressState, Ipv4DeviceConfiguration, Ipv6AddressEntry, Ipv6AddressState,
            Ipv6DadState, Ipv6DeviceConfiguration, Ipv6NetworkLearnedParameters,
        },
        DualStackDeviceContext, DualStackDeviceStateRef, IpDeviceAddressContext,
        IpDeviceAddressIdContext, IpDeviceConfigurationContext, IpDeviceIpExt, IpDeviceSendContext,
        IpDeviceStateContext, Ipv6DeviceConfigurationContext, Ipv6DeviceContext,
    },
    sync::{PrimaryRc, StrongRc},
    NonSyncContext, SyncCtx,
};

impl<NonSyncCtx: NonSyncContext> UnlockedAccess<crate::lock_ordering::DeviceLayerStateOrigin>
    for SyncCtx<NonSyncCtx>
{
    type Data = OriginTracker;
    type Guard<'l> = &'l OriginTracker where Self: 'l;
    fn access(&self) -> Self::Guard<'_> {
        &self.state.device.origin
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
        bindings_ctx: &mut C,
        device_id: &DeviceId<C>,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        match device_id {
            DeviceId::Ethernet(id) => {
                if let Some(link_addr) = bytes_to_mac(link_addr) {
                    NudHandler::<I, EthernetLinkDevice, _>::handle_neighbor_update(
                        self,
                        bindings_ctx,
                        &id,
                        neighbor,
                        link_addr,
                        DynamicNeighborUpdateSource::Probe,
                    )
                }
            }
            DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
        }
    }

    fn handle_neighbor_confirmation(
        &mut self,
        bindings_ctx: &mut C,
        device_id: &DeviceId<C>,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
        flags: ConfirmationFlags,
    ) {
        match device_id {
            DeviceId::Ethernet(id) => {
                if let Some(link_addr) = bytes_to_mac(link_addr) {
                    NudHandler::<I, EthernetLinkDevice, _>::handle_neighbor_update(
                        self,
                        bindings_ctx,
                        &id,
                        neighbor,
                        link_addr,
                        DynamicNeighborUpdateSource::Confirmation(flags),
                    )
                }
            }
            DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
        }
    }

    fn flush_neighbor_table(&mut self, bindings_ctx: &mut C, device_id: &DeviceId<C>) {
        match device_id {
            DeviceId::Ethernet(id) => {
                NudHandler::<I, EthernetLinkDevice, _>::flush(self, bindings_ctx, &id)
            }
            DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
        }
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv4>>>
    IpDeviceSendContext<Ipv4, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        local_addr: SpecifiedAddr<Ipv4Addr>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        send_ip_frame(self, bindings_ctx, device, local_addr, body)
    }
}

impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>,
    > IpDeviceConfigurationContext<Ipv4, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type DevicesIter<'s> = DevicesIter<'s, NonSyncCtx>;
    type WithIpDeviceConfigurationInnerCtx<'s> = SyncCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv4DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv4>,
        NonSyncCtx,
    >;
    type WithIpDeviceConfigurationMutInner<'s> = SyncCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv4DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv4>,
        NonSyncCtx,
    >;
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

        cb(DevicesIter { ethernet: ethernet.values(), loopback: loopback.iter() }, locked)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        get_mtu(self, device_id)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.state.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|primary| DeviceId::Loopback(primary.clone_strong()))
    }
}

impl<NonSyncCtx: NonSyncContext, L> IpDeviceAddressIdContext<Ipv4>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type AddressId = StrongRc<Ipv4AddressEntry<NonSyncCtx::Instant>>;
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::Ipv4DeviceAddressState>>
    IpDeviceAddressContext<Ipv4, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_ip_address_state<O, F: FnOnce(&Ipv4AddressState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        _: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut entry = Locked::<_, L>::new_locked(addr_id.deref());
        let let_binding_needed_for_lifetimes =
            cb(&entry.read_lock::<crate::lock_ordering::Ipv4DeviceAddressState>());
        let_binding_needed_for_lifetimes
    }

    fn with_ip_address_state_mut<O, F: FnOnce(&mut Ipv4AddressState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        _: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut entry = Locked::<_, L>::new_locked(addr_id.deref());
        let let_binding_needed_for_lifetimes =
            cb(&mut entry.write_lock::<crate::lock_ordering::Ipv4DeviceAddressState>());
        let_binding_needed_for_lifetimes
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv4>>>
    IpDeviceStateContext<Ipv4, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type IpDeviceAddressCtx<'a> =
        Locked<&'a SyncCtx<NonSyncCtx>, crate::lock_ordering::IpDeviceAddresses<Ipv4>>;

    fn with_ip_device_flags<O, F: FnOnce(&IpDeviceFlags) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            cb(&*state.lock::<crate::lock_ordering::IpDeviceFlags<Ipv4>>())
        })
    }

    fn add_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: AddrSubnet<Ipv4Addr>,
        config: <Ipv4 as IpDeviceIpExt>::AddressConfig<NonSyncCtx::Instant>,
    ) -> Result<Self::AddressId, ExistsError> {
        with_ip_device_state(self, device_id, |mut state| {
            state
                .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>()
                .add(Ipv4AddressEntry::new(addr, config))
        })
    }

    fn remove_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: Self::AddressId,
    ) -> (AddrSubnet<Ipv4Addr>, <Ipv4 as IpDeviceIpExt>::AddressConfig<NonSyncCtx::Instant>) {
        let primary = with_ip_device_state(self, device_id, |mut state| {
            state.write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>().remove(&addr.addr())
        })
        .expect("should exist when address ID exists");
        assert!(PrimaryRc::ptr_eq(&primary, &addr));
        core::mem::drop(addr);

        let Ipv4AddressEntry { addr_sub, state } = PrimaryRc::unwrap(primary);
        let Ipv4AddressState { config } = state.into_inner();
        (addr_sub, config)
    }

    fn get_address_id(
        &mut self,
        device_id: &Self::DeviceId,
        addr: SpecifiedAddr<Ipv4Addr>,
    ) -> Result<Self::AddressId, NotFoundError> {
        with_ip_device_state(self, device_id, |mut state| {
            state
                .read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>()
                .iter()
                .find(|a| a.addr() == addr)
                .map(PrimaryRc::clone_strong)
                .ok_or(NotFoundError)
        })
    }

    fn with_address_ids<
        O,
        F: FnOnce(
            Box<dyn Iterator<Item = Self::AddressId> + '_>,
            &mut Self::IpDeviceAddressCtx<'_>,
        ) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            cb(
                Box::new(
                    state
                        .read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>()
                        .iter()
                        .map(PrimaryRc::clone_strong),
                ),
                &mut sync_ctx.cast_locked::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>(),
            )
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
        bindings_ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        join_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }

    fn leave_link_multicast_group(
        &mut self,
        bindings_ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        leave_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }
}

impl<
        NonSyncCtx: NonSyncContext,
        L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
    > Ipv6DeviceConfigurationContext<NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type Ipv6DeviceStateCtx<'s> = SyncCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        NonSyncCtx,
    >;
    type WithIpv6DeviceConfigurationMutInner<'s> = SyncCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        NonSyncCtx,
    >;

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
    type WithIpDeviceConfigurationInnerCtx<'s> = SyncCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        NonSyncCtx,
    >;
    type WithIpDeviceConfigurationMutInner<'s> = SyncCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        NonSyncCtx,
    >;
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

        cb(DevicesIter { ethernet: ethernet.values(), loopback: loopback.iter() }, locked)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        get_mtu(self, device_id)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.state.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|primary| DeviceId::Loopback(primary.clone_strong()))
    }
}

impl<NonSyncCtx: NonSyncContext, L> IpDeviceAddressIdContext<Ipv6>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type AddressId = StrongRc<Ipv6AddressEntry<NonSyncCtx::Instant>>;
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::Ipv6DeviceAddressState>>
    IpDeviceAddressContext<Ipv6, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn with_ip_address_state<O, F: FnOnce(&Ipv6AddressState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut entry = Locked::<_, L>::new_locked(addr_id.deref());
        let x = cb(&entry.read_lock::<crate::lock_ordering::Ipv6DeviceAddressState>());
        x
    }

    fn with_ip_address_state_mut<O, F: FnOnce(&mut Ipv6AddressState<NonSyncCtx::Instant>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut entry = Locked::<_, L>::new_locked(addr_id.deref());
        let x = cb(&mut entry.write_lock::<crate::lock_ordering::Ipv6DeviceAddressState>());
        x
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv6>>>
    IpDeviceStateContext<Ipv6, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    type IpDeviceAddressCtx<'a> =
        Locked<&'a SyncCtx<NonSyncCtx>, crate::lock_ordering::IpDeviceAddresses<Ipv6>>;

    fn with_ip_device_flags<O, F: FnOnce(&IpDeviceFlags) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            cb(&*state.lock::<crate::lock_ordering::IpDeviceFlags<Ipv6>>())
        })
    }

    fn add_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>,
        config: <Ipv6 as IpDeviceIpExt>::AddressConfig<NonSyncCtx::Instant>,
    ) -> Result<Self::AddressId, ExistsError> {
        with_ip_device_state(self, device_id, |mut state| {
            state
                .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
                .add(Ipv6AddressEntry::new(addr, Ipv6DadState::Uninitialized, config))
        })
    }

    fn remove_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: Self::AddressId,
    ) -> (
        AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>,
        <Ipv6 as IpDeviceIpExt>::AddressConfig<NonSyncCtx::Instant>,
    ) {
        let primary = with_ip_device_state(self, device_id, |mut state| {
            state.write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>().remove(&addr.addr())
        })
        .expect("should exist when address ID exists");
        assert!(PrimaryRc::ptr_eq(&primary, &addr));
        core::mem::drop(addr);

        let Ipv6AddressEntry { addr_sub, dad_state: _, state } = PrimaryRc::unwrap(primary);
        let Ipv6AddressState { flags: _, config } = state.into_inner();
        (addr_sub, config)
    }

    fn get_address_id(
        &mut self,
        device_id: &Self::DeviceId,
        addr: SpecifiedAddr<Ipv6Addr>,
    ) -> Result<Self::AddressId, NotFoundError> {
        with_ip_device_state(self, device_id, |mut state| {
            state
                .read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
                .iter()
                .find_map(|a| (a.addr() == addr).then(|| PrimaryRc::clone_strong(a)))
                .ok_or(NotFoundError)
        })
    }

    fn with_address_ids<
        O,
        F: FnOnce(
            Box<dyn Iterator<Item = Self::AddressId> + '_>,
            &mut Self::IpDeviceAddressCtx<'_>,
        ) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_sync_ctx(self, device_id, |mut state, sync_ctx| {
            cb(
                Box::new(
                    state
                        .read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
                        .iter()
                        .map(PrimaryRc::clone_strong),
                ),
                &mut sync_ctx.cast_locked::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>(),
            )
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
        bindings_ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        join_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }

    fn leave_link_multicast_group(
        &mut self,
        bindings_ctx: &mut NonSyncCtx,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        leave_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv6>>>
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
            DeviceId::Loopback(LoopbackDeviceId { .. }) => None,
        }
    }

    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]> {
        match device_id {
            DeviceId::Ethernet(id) => {
                Some(ethernet::get_mac(self, &id).to_eui64_with_magic(Mac::DEFAULT_EUI_MAGIC))
            }
            DeviceId::Loopback(LoopbackDeviceId { .. }) => None,
        }
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        if mtu < Ipv6::MINIMUM_LINK_MTU {
            return;
        }

        match device_id {
            DeviceId::Ethernet(id) => ethernet::set_mtu(self, &id, mtu),
            DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
        }
    }

    fn with_network_learned_parameters<O, F: FnOnce(&Ipv6NetworkLearnedParameters) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::Ipv6DeviceLearnedParams>();
            cb(&state)
        })
    }

    fn with_network_learned_parameters_mut<O, F: FnOnce(&mut Ipv6NetworkLearnedParameters) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state(self, device_id, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::Ipv6DeviceLearnedParams>();
            cb(&mut state)
        })
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv6>>>
    IpDeviceSendContext<Ipv6, NonSyncCtx> for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut NonSyncCtx,
        device: &DeviceId<NonSyncCtx>,
        local_addr: SpecifiedAddr<Ipv6Addr>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        send_ip_frame(self, bindings_ctx, device, local_addr, body)
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

    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        weak_device_id.upgrade()
    }
}

impl<'a, SC: DeviceIdContext<EthernetLinkDevice> + CounterContext<DeviceCounters>>
    DeviceIdContext<EthernetLinkDevice> for SyncCtxWithDeviceId<'a, SC>
{
    type DeviceId = SC::DeviceId;
    type WeakDeviceId = SC::WeakDeviceId;
    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        let Self { sync_ctx, device_id: _ } = self;
        SC::downgrade_device_id(sync_ctx, device_id)
    }

    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        let Self { sync_ctx, device_id: _ } = self;
        SC::upgrade_weak_device_id(sync_ctx, weak_device_id)
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

impl<C: NonSyncContext, L> SendFrameContext<C, socket::DeviceSocketMetadata<DeviceId<C>>>
    for Locked<&SyncCtx<C>, L>
where
    Self: SendFrameContext<C, socket::DeviceSocketMetadata<EthernetDeviceId<C>>>
        + SendFrameContext<C, socket::DeviceSocketMetadata<LoopbackDeviceId<C>>>,
{
    fn send_frame<S>(
        &mut self,
        bindings_ctx: &mut C,
        metadata: socket::DeviceSocketMetadata<DeviceId<C>>,
        frame: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let socket::DeviceSocketMetadata { device_id, header } = metadata;
        match device_id {
            DeviceId::Ethernet(device_id) => SendFrameContext::send_frame(
                self,
                bindings_ctx,
                socket::DeviceSocketMetadata { device_id, header },
                frame,
            ),
            DeviceId::Loopback(device_id) => SendFrameContext::send_frame(
                self,
                bindings_ctx,
                socket::DeviceSocketMetadata { device_id, header },
                frame,
            ),
        }
    }
}

impl<NonSyncCtx: NonSyncContext> RwLockFor<crate::lock_ordering::DeviceLayerState>
    for SyncCtx<NonSyncCtx>
{
    type Data = Devices<NonSyncCtx>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Devices<NonSyncCtx>>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Devices<NonSyncCtx>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.device.devices.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.device.devices.write()
    }
}

impl<C: DeviceLayerTypes + socket::NonSyncContext<DeviceId<C>>>
    RwLockFor<crate::lock_ordering::DeviceLayerState> for DeviceLayerState<C>
{
    type Data = Devices<C>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Devices<C>>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Devices<C>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.devices.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.devices.write()
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
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::EthernetRxDequeue>>
    RecvFrameContext<NonSyncCtx, RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv4>>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut NonSyncCtx,
        metadata: RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv4>,
        frame: B,
    ) {
        crate::ip::receive_ipv4_packet(
            self,
            bindings_ctx,
            &metadata.device.into(),
            metadata.frame_dst,
            frame,
        );
    }
}

impl<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::EthernetRxDequeue>>
    RecvFrameContext<NonSyncCtx, RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv6>>
    for Locked<&SyncCtx<NonSyncCtx>, L>
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut NonSyncCtx,
        metadata: RecvIpFrameMeta<EthernetDeviceId<NonSyncCtx>, Ipv6>,
        frame: B,
    ) {
        crate::ip::receive_ipv6_packet(
            self,
            bindings_ctx,
            &metadata.device.into(),
            metadata.frame_dst,
            frame,
        );
    }
}

pub(crate) fn with_ethernet_state_and_sync_ctx<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<&IpLinkDeviceState<EthernetLinkDevice, NonSyncCtx>, L>,
        &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    id: &EthernetDeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    let state = id.device_state();
    // Make sure that the pointer belongs to this `sync_ctx`.
    assert_eq!(
        *core_ctx.unlocked_access::<crate::lock_ordering::DeviceLayerStateOrigin>(),
        state.origin
    );

    // Even though the device state is technically accessible outside of the
    // `SyncCtx`, it is held inside `SyncCtx` so we propagate the same lock
    // level as we were called with to avoid lock ordering issues.
    cb(Locked::new_locked(state), core_ctx)
}

pub(crate) fn with_ethernet_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(Locked<&IpLinkDeviceState<EthernetLinkDevice, NonSyncCtx>, L>) -> O,
    L,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &EthernetDeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    with_ethernet_state_and_sync_ctx(core_ctx, device_id, |ip_device_state, _sync_ctx| {
        cb(ip_device_state)
    })
}

pub(crate) fn with_loopback_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(Locked<&'_ IpLinkDeviceState<LoopbackDevice, NonSyncCtx>, L>) -> O,
    L,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device_id: &LoopbackDeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    with_loopback_state_and_sync_ctx(core_ctx, device_id, |ip_device_state, _sync_ctx| {
        cb(ip_device_state)
    })
}

pub(crate) fn with_loopback_state_and_sync_ctx<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(
        Locked<&IpLinkDeviceState<LoopbackDevice, NonSyncCtx>, L>,
        &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    ) -> O,
    L,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    id: &LoopbackDeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    let state = id.device_state();
    // Make sure that the pointer belongs to this `sync_ctx`.
    assert_eq!(
        *core_ctx.unlocked_access::<crate::lock_ordering::DeviceLayerStateOrigin>(),
        state.origin
    );

    // Even though the device state is technically accessible outside of the
    // `SyncCtx`, it is held inside `SyncCtx` so we propagate the same lock
    // level as we were called with to avoid lock ordering issues.
    cb(Locked::new_locked(state), core_ctx)
}

pub(crate) fn with_ip_device_state<
    NonSyncCtx: NonSyncContext,
    O,
    F: FnOnce(Locked<&DualStackIpDeviceState<NonSyncCtx::Instant>, L>) -> O,
    L,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    match device {
        DeviceId::Ethernet(id) => with_ethernet_state(core_ctx, id, |mut state| cb(state.cast())),
        DeviceId::Loopback(id) => with_loopback_state(core_ctx, id, |mut state| cb(state.cast())),
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
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
    cb: F,
) -> O {
    match device {
        DeviceId::Ethernet(id) => {
            with_ethernet_state_and_sync_ctx(core_ctx, id, |mut state, ctx| cb(state.cast(), ctx))
        }
        DeviceId::Loopback(id) => {
            with_loopback_state_and_sync_ctx(core_ctx, id, |mut state, ctx| cb(state.cast(), ctx))
        }
    }
}

fn get_mtu<NonSyncCtx: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    device: &DeviceId<NonSyncCtx>,
) -> Mtu {
    match device {
        DeviceId::Ethernet(id) => self::ethernet::get_mtu(core_ctx, &id),
        DeviceId::Loopback(id) => self::loopback::get_mtu(core_ctx, id),
    }
}

fn join_link_multicast_group<
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    bindings_ctx: &mut NonSyncCtx,
    device_id: &DeviceId<NonSyncCtx>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::join_link_multicast(
            core_ctx,
            bindings_ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
    }
}

fn leave_link_multicast_group<
    NonSyncCtx: NonSyncContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    bindings_ctx: &mut NonSyncCtx,
    device_id: &DeviceId<NonSyncCtx>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::leave_link_multicast(
            core_ctx,
            bindings_ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
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

fn send_ip_frame<NonSyncCtx, S, A, L>(
    core_ctx: &mut Locked<&SyncCtx<NonSyncCtx>, L>,
    bindings_ctx: &mut NonSyncCtx,
    device: &DeviceId<NonSyncCtx>,
    local_addr: SpecifiedAddr<A>,
    body: S,
) -> Result<(), S>
where
    NonSyncCtx: NonSyncContext,
    S: Serializer,
    S::Buffer: BufferMut,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::IpState<A::Version>>
        + LockBefore<crate::lock_ordering::LoopbackTxQueue>,
    A::Version: EthernetIpExt,
    for<'a> Locked<&'a SyncCtx<NonSyncCtx>, L>: EthernetIpLinkDeviceDynamicStateContext<NonSyncCtx, DeviceId = EthernetDeviceId<NonSyncCtx>>
        + NudHandler<A::Version, EthernetLinkDevice, NonSyncCtx>
        + TransmitQueueHandler<EthernetLinkDevice, NonSyncCtx, Meta = ()>,
{
    match device {
        DeviceId::Ethernet(id) => {
            ethernet::send_ip_frame::<_, _, A, _>(core_ctx, bindings_ctx, &id, local_addr, body)
        }
        DeviceId::Loopback(id) => {
            loopback::send_ip_frame::<_, A, _, _>(core_ctx, bindings_ctx, id, local_addr, body)
        }
    }
}
