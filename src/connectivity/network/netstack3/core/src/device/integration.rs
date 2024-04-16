// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of device layer traits for [`CoreCtx`].

use alloc::boxed::Box;
use core::{num::NonZeroU8, ops::Deref as _};

use lock_order::{
    lock::{RwLockFor, UnlockedAccess},
    relation::LockBefore,
    wrap::prelude::*,
};
use net_types::{
    ethernet::Mac,
    ip::{AddrSubnet, Ip, IpAddress, IpInvariant, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu},
    map_ip_twice, MulticastAddr, SpecifiedAddr, Witness as _,
};
use packet::{BufferMut, Serializer};
use packet_formats::ethernet::EthernetIpExt;

use crate::{
    context::{
        CoreCtxAndResource, CounterContext, Locked, RecvFrameContext, ResourceCounterContext,
        SendFrameContext,
    },
    device::{
        config::DeviceConfigurationContext,
        ethernet::{
            self, CoreCtxWithDeviceId, EthernetIpLinkDeviceDynamicStateContext, EthernetLinkDevice,
        },
        loopback::{self, LoopbackDevice, LoopbackDeviceId, LoopbackPrimaryDeviceId},
        pure_ip::{self, PureIpDeviceId},
        queue::tx::TransmitQueueHandler,
        socket,
        state::{DeviceStateSpec, IpLinkDeviceState},
        AnyDevice, BaseDeviceId, DeviceCollectionContext, DeviceCounters, DeviceId,
        DeviceIdContext, DeviceLayerEventDispatcher, DeviceLayerState, DeviceLayerTypes, Devices,
        DevicesIter, EthernetDeviceCounters, EthernetDeviceId, EthernetPrimaryDeviceId,
        EthernetWeakDeviceId, Ipv6DeviceLinkLayerAddr, OriginTracker, OriginTrackerContext,
        PureIpDeviceCounters, RecvIpFrameMeta, WeakDeviceId,
    },
    error::{ExistsError, NotFoundError},
    filter::{IpPacket, ProofOfEgressCheck},
    for_any_device_id,
    ip::{
        device::{
            integration::CoreCtxWithIpDeviceConfiguration,
            nud::{
                ConfirmationFlags, DynamicNeighborUpdateSource, NudHandler, NudIpHandler,
                NudUserConfig,
            },
            state::{
                AssignedAddress as _, DualStackIpDeviceState, IpDeviceFlags, Ipv4AddressEntry,
                Ipv4AddressState, Ipv4DeviceConfiguration, Ipv6AddressEntry, Ipv6AddressState,
                Ipv6DadState, Ipv6DeviceConfiguration, Ipv6NetworkLearnedParameters,
            },
            IpDeviceAddressContext, IpDeviceAddressIdContext, IpDeviceConfigurationContext,
            IpDeviceIpExt, IpDeviceSendContext, IpDeviceStateContext, Ipv6DeviceAddr,
            Ipv6DeviceConfigurationContext, Ipv6DeviceContext,
        },
        types::IpTypesIpExt,
    },
    sync::{PrimaryRc, StrongRc},
    BindingsContext, BindingsTypes, CoreCtx, StackState,
};

impl<BC: BindingsTypes> UnlockedAccess<crate::lock_ordering::DeviceLayerStateOrigin>
    for StackState<BC>
{
    type Data = OriginTracker;
    type Guard<'l> = &'l OriginTracker where Self: 'l;
    fn access(&self) -> Self::Guard<'_> {
        &self.device.origin
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
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::EthernetIpv4Arp>
            + LockBefore<crate::lock_ordering::EthernetIpv6Nud>,
    > NudIpHandler<I, BC> for CoreCtx<'_, BC, L>
where
    Self: NudHandler<I, EthernetLinkDevice, BC>
        + DeviceIdContext<EthernetLinkDevice, DeviceId = EthernetDeviceId<BC>>,
{
    fn handle_neighbor_probe(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &DeviceId<BC>,
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
            // NUD is not supported on Loopback and Pure IP devices.
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
        }
    }

    fn handle_neighbor_confirmation(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &DeviceId<BC>,
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
            // NUD is not supported on Loopback and Pure IP devices.
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
        }
    }

    fn flush_neighbor_table(&mut self, bindings_ctx: &mut BC, device_id: &DeviceId<BC>) {
        match device_id {
            DeviceId::Ethernet(id) => {
                NudHandler::<I, EthernetLinkDevice, _>::flush(self, bindings_ctx, &id)
            }
            // NUD is not supported on Loopback and Pure IP devices.
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
        }
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<I: IpTypesIpExt, BC: BindingsContext, L: LockBefore<crate::lock_ordering::FilterState<I>>>
    IpDeviceSendContext<I, BC> for CoreCtx<'_, BC, L>
{
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        local_addr: SpecifiedAddr<<I as Ip>::Addr>,
        body: S,
        broadcast: Option<<I as IpTypesIpExt>::BroadcastMarker>,
        ProofOfEgressCheck { .. }: ProofOfEgressCheck,
    ) -> Result<(), S>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut,
    {
        send_ip_frame(self, bindings_ctx, device, local_addr, body, broadcast)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I: IpTypesIpExt,
        Config,
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::FilterState<I>>,
    > IpDeviceSendContext<I, BC> for CoreCtxWithIpDeviceConfiguration<'_, Config, L, BC>
{
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        local_addr: SpecifiedAddr<<I as Ip>::Addr>,
        body: S,
        broadcast: Option<<I as IpTypesIpExt>::BroadcastMarker>,
        ProofOfEgressCheck { .. }: ProofOfEgressCheck,
    ) -> Result<(), S>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut,
    {
        let Self { config: _, core_ctx } = self;
        send_ip_frame(core_ctx, bindings_ctx, device, local_addr, body, broadcast)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>>
    IpDeviceConfigurationContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type DevicesIter<'s> = DevicesIter<'s, BC>;
    type WithIpDeviceConfigurationInnerCtx<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv4DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv4>,
        BC,
    >;
    type WithIpDeviceConfigurationMutInner<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv4DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv4>,
        BC,
    >;
    type DeviceAddressAndGroupsAccessor<'s> =
        CoreCtx<'s, BC, crate::lock_ordering::DeviceLayerState>;

    fn with_ip_device_configuration<
        O,
        F: FnOnce(&Ipv4DeviceConfiguration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_core_ctx(self, device_id, |mut core_ctx_and_resource| {
            let (state, mut locked) = core_ctx_and_resource
                .read_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>, _>(
                |c| c.right(),
            );
            cb(
                &state,
                CoreCtxWithIpDeviceConfiguration {
                    config: &state,
                    core_ctx: locked.cast_core_ctx(),
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
        with_ip_device_state_and_core_ctx(self, device_id, |mut core_ctx_and_resource| {
            let (mut state, mut locked) = core_ctx_and_resource
                .write_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>, _>(
                |c| c.right(),
            );
            cb(CoreCtxWithIpDeviceConfiguration {
                config: &mut state,
                core_ctx: locked.cast_core_ctx(),
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
        let Devices { ethernet, pure_ip, loopback } = &*devices;

        cb(
            DevicesIter {
                ethernet: ethernet.values(),
                pure_ip: pure_ip.values(),
                loopback: loopback.iter(),
            },
            locked,
        )
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        get_mtu(self, device_id)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|primary| DeviceId::Loopback(primary.clone_strong()))
    }
}

impl<BC: BindingsContext, L> IpDeviceAddressIdContext<Ipv4> for CoreCtx<'_, BC, L> {
    type AddressId = StrongRc<Ipv4AddressEntry<BC::Instant>>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::Ipv4DeviceAddressState>>
    IpDeviceAddressContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    fn with_ip_address_state<O, F: FnOnce(&Ipv4AddressState<BC::Instant>) -> O>(
        &mut self,
        _: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(addr_id.deref());
        let let_binding_needed_for_lifetimes =
            cb(&locked
                .read_lock_with::<crate::lock_ordering::Ipv4DeviceAddressState, _>(|c| c.right()));
        let_binding_needed_for_lifetimes
    }

    fn with_ip_address_state_mut<O, F: FnOnce(&mut Ipv4AddressState<BC::Instant>) -> O>(
        &mut self,
        _: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(addr_id.deref());
        let let_binding_needed_for_lifetimes =
            cb(&mut locked
                .write_lock_with::<crate::lock_ordering::Ipv4DeviceAddressState, _>(|c| c.right()));
        let_binding_needed_for_lifetimes
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv4>>>
    IpDeviceStateContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type IpDeviceAddressCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IpDeviceAddresses<Ipv4>>;

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
        config: <Ipv4 as IpDeviceIpExt>::AddressConfig<BC::Instant>,
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
    ) -> (AddrSubnet<Ipv4Addr>, <Ipv4 as IpDeviceIpExt>::AddressConfig<BC::Instant>) {
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
        with_ip_device_state_and_core_ctx(self, device_id, |mut core_ctx_and_resource| {
            let (state, mut locked) = core_ctx_and_resource
                .read_lock_with_and::<crate::lock_ordering::IpDeviceAddresses<Ipv4>, _>(|c| {
                    c.right()
                });
            cb(Box::new(state.iter().map(PrimaryRc::clone_strong)), &mut locked.cast_core_ctx())
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
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        join_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }

    fn leave_link_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv4Addr>,
    ) {
        leave_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>>
    Ipv6DeviceConfigurationContext<BC> for CoreCtx<'_, BC, L>
{
    type Ipv6DeviceStateCtx<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        BC,
    >;
    type WithIpv6DeviceConfigurationMutInner<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        BC,
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

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>>
    IpDeviceConfigurationContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type DevicesIter<'s> = DevicesIter<'s, BC>;
    type WithIpDeviceConfigurationInnerCtx<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        BC,
    >;
    type WithIpDeviceConfigurationMutInner<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv6DeviceConfiguration,
        crate::lock_ordering::IpDeviceConfiguration<Ipv6>,
        BC,
    >;
    type DeviceAddressAndGroupsAccessor<'s> =
        CoreCtx<'s, BC, crate::lock_ordering::DeviceLayerState>;

    fn with_ip_device_configuration<
        O,
        F: FnOnce(&Ipv6DeviceConfiguration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        with_ip_device_state_and_core_ctx(self, device_id, |mut core_ctx_and_resource| {
            let (state, mut locked) = core_ctx_and_resource
                .read_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>, _>(
                |c| c.right(),
            );
            cb(
                &state,
                CoreCtxWithIpDeviceConfiguration {
                    config: &state,
                    core_ctx: locked.cast_core_ctx(),
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
        with_ip_device_state_and_core_ctx(self, device_id, |mut core_ctx_and_resource| {
            let (mut state, mut locked) = core_ctx_and_resource
                .write_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>, _>(
                |c| c.right(),
            );
            cb(CoreCtxWithIpDeviceConfiguration {
                config: &mut state,
                core_ctx: locked.cast_core_ctx(),
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
        let Devices { ethernet, pure_ip, loopback } = &*devices;

        cb(
            DevicesIter {
                ethernet: ethernet.values(),
                pure_ip: pure_ip.values(),
                loopback: loopback.iter(),
            },
            locked,
        )
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        get_mtu(self, device_id)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let mut locked = self.cast_with(|s| &s.device);
        let devices = &*locked.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|primary| DeviceId::Loopback(primary.clone_strong()))
    }
}

impl<BC: BindingsContext, L> IpDeviceAddressIdContext<Ipv6> for CoreCtx<'_, BC, L> {
    type AddressId = StrongRc<Ipv6AddressEntry<BC::Instant>>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::Ipv6DeviceAddressState>>
    IpDeviceAddressContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    fn with_ip_address_state<O, F: FnOnce(&Ipv6AddressState<BC::Instant>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(addr_id.deref());
        let x = cb(&locked
            .read_lock_with::<crate::lock_ordering::Ipv6DeviceAddressState, _>(|c| c.right()));
        x
    }

    fn with_ip_address_state_mut<O, F: FnOnce(&mut Ipv6AddressState<BC::Instant>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O {
        let mut locked = self.adopt(addr_id.deref());
        let x = cb(&mut locked
            .write_lock_with::<crate::lock_ordering::Ipv6DeviceAddressState, _>(|c| c.right()));
        x
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv6>>>
    IpDeviceStateContext<Ipv6, BC> for CoreCtx<'_, BC, L>
{
    type IpDeviceAddressCtx<'a> = CoreCtx<'a, BC, crate::lock_ordering::IpDeviceAddresses<Ipv6>>;

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
        addr: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        config: <Ipv6 as IpDeviceIpExt>::AddressConfig<BC::Instant>,
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
    ) -> (AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>, <Ipv6 as IpDeviceIpExt>::AddressConfig<BC::Instant>)
    {
        let primary = with_ip_device_state(self, device_id, |mut state| {
            state
                .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
                .remove(&addr.addr().addr())
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
                .find_map(|a| (a.addr().addr() == *addr).then(|| PrimaryRc::clone_strong(a)))
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
        with_ip_device_state_and_core_ctx(self, device_id, |mut core_ctx_and_resource| {
            let (state, mut core_ctx) = core_ctx_and_resource
                .read_lock_with_and::<crate::lock_ordering::IpDeviceAddresses<Ipv6>, _>(
                |c| c.right(),
            );
            cb(Box::new(state.iter().map(PrimaryRc::clone_strong)), &mut core_ctx.cast_core_ctx())
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
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        join_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }

    fn leave_link_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<Ipv6Addr>,
    ) {
        leave_link_multicast_group(self, bindings_ctx, device_id, multicast_addr)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceAddresses<Ipv6>>>
    Ipv6DeviceContext<BC> for CoreCtx<'_, BC, L>
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
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => None,
        }
    }

    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]> {
        match device_id {
            DeviceId::Ethernet(id) => {
                Some(ethernet::get_mac(self, &id).to_eui64_with_magic(Mac::DEFAULT_EUI_MAGIC))
            }
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => None,
        }
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        if mtu < Ipv6::MINIMUM_LINK_MTU {
            return;
        }

        match device_id {
            DeviceId::Ethernet(id) => ethernet::set_mtu(self, &id, mtu),
            DeviceId::Loopback(LoopbackDeviceId { .. }) => {}
            DeviceId::PureIp(id) => pure_ip::set_mtu(self, &id, mtu),
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

impl<BT: BindingsTypes, L> DeviceIdContext<EthernetLinkDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = EthernetDeviceId<BT>;
    type WeakDeviceId = EthernetWeakDeviceId<BT>;
}

impl<'a, CC: DeviceIdContext<EthernetLinkDevice> + CounterContext<DeviceCounters>>
    DeviceIdContext<EthernetLinkDevice> for CoreCtxWithDeviceId<'a, CC>
{
    type DeviceId = CC::DeviceId;
    type WeakDeviceId = CC::WeakDeviceId;
}

impl<BC: socket::DeviceSocketBindingsContext<DeviceId<BC>> + DeviceLayerEventDispatcher>
    socket::DeviceSocketBindingsContext<EthernetDeviceId<BC>> for BC
{
    fn receive_frame(
        &self,
        state: &Self::SocketState,
        device: &EthernetDeviceId<BC>,
        frame: socket::Frame<&[u8]>,
        whole_frame: &[u8],
    ) {
        self.receive_frame(state, &device.clone().into(), frame, whole_frame)
    }
}

impl<BC: BindingsContext, L> SendFrameContext<BC, socket::DeviceSocketMetadata<DeviceId<BC>>>
    for CoreCtx<'_, BC, L>
where
    Self: SendFrameContext<BC, socket::DeviceSocketMetadata<EthernetDeviceId<BC>>>
        + SendFrameContext<BC, socket::DeviceSocketMetadata<LoopbackDeviceId<BC>>>
        + SendFrameContext<BC, socket::DeviceSocketMetadata<PureIpDeviceId<BC>>>,
{
    fn send_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: socket::DeviceSocketMetadata<DeviceId<BC>>,
        frame: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let socket::DeviceSocketMetadata { device_id, header } = metadata;
        for_any_device_id!(
            DeviceId,
            device_id,
            device_id => SendFrameContext::send_frame(
                self,
                bindings_ctx,
                socket::DeviceSocketMetadata { device_id, header },
                frame,
            )
        )
    }
}

impl<BT: BindingsTypes> RwLockFor<crate::lock_ordering::DeviceLayerState> for StackState<BT> {
    type Data = Devices<BT>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Devices<BT>>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Devices<BT>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.device.devices.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.device.devices.write()
    }
}

impl<BC: DeviceLayerTypes + socket::DeviceSocketBindingsContext<DeviceId<BC>>>
    RwLockFor<crate::lock_ordering::DeviceLayerState> for DeviceLayerState<BC>
{
    type Data = Devices<BC>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Devices<BC>>
        where
            Self: 'l ;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Devices<BC>>
        where
            Self: 'l ;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.devices.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.devices.write()
    }
}

impl<BC: BindingsContext, L> DeviceIdContext<AnyDevice> for CoreCtx<'_, BC, L> {
    type DeviceId = DeviceId<BC>;
    type WeakDeviceId = WeakDeviceId<BC>;
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::EthernetRxDequeue>>
    RecvFrameContext<BC, RecvIpFrameMeta<EthernetDeviceId<BC>, Ipv4>> for CoreCtx<'_, BC, L>
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: RecvIpFrameMeta<EthernetDeviceId<BC>, Ipv4>,
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

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::EthernetRxDequeue>>
    RecvFrameContext<BC, RecvIpFrameMeta<EthernetDeviceId<BC>, Ipv6>> for CoreCtx<'_, BC, L>
{
    fn receive_frame<B: BufferMut>(
        &mut self,
        bindings_ctx: &mut BC,
        metadata: RecvIpFrameMeta<EthernetDeviceId<BC>, Ipv6>,
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

pub(crate) fn with_device_state<
    BT: BindingsTypes,
    O,
    F: FnOnce(Locked<&'_ IpLinkDeviceState<D, BT>, L>) -> O,
    L,
    D: DeviceStateSpec,
>(
    core_ctx: &mut CoreCtx<'_, BT, L>,
    device_id: &BaseDeviceId<D, BT>,
    cb: F,
) -> O {
    with_device_state_and_core_ctx(core_ctx, device_id, |mut core_ctx_and_resource| {
        cb(core_ctx_and_resource.cast_resource())
    })
}

pub(crate) fn with_device_state_and_core_ctx<
    BT: BindingsTypes,
    O,
    F: FnOnce(CoreCtxAndResource<'_, BT, IpLinkDeviceState<D, BT>, L>) -> O,
    L,
    D: DeviceStateSpec,
>(
    core_ctx: &mut CoreCtx<'_, BT, L>,
    id: &BaseDeviceId<D, BT>,
    cb: F,
) -> O {
    let state = id.device_state();
    // Make sure that the pointer belongs to this `sync_ctx`.
    assert_eq!(
        *core_ctx.unlocked_access::<crate::lock_ordering::DeviceLayerStateOrigin>(),
        state.origin
    );
    cb(core_ctx.adopt(state))
}

pub(crate) fn with_ip_device_state<
    BC: BindingsContext,
    O,
    F: FnOnce(Locked<&DualStackIpDeviceState<BC>, L>) -> O,
    L,
>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    device: &DeviceId<BC>,
    cb: F,
) -> O {
    for_any_device_id!(
        DeviceId,
        device,
        id => with_device_state(core_ctx, id, |mut state| cb(state.cast()))
    )
}

pub(crate) fn with_ip_device_state_and_core_ctx<
    BC: BindingsContext,
    O,
    F: FnOnce(CoreCtxAndResource<'_, BC, DualStackIpDeviceState<BC>, L>) -> O,
    L,
>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    device: &DeviceId<BC>,
    cb: F,
) -> O {
    for_any_device_id!(
        DeviceId,
        device,
        id => with_device_state_and_core_ctx(core_ctx, id, |mut core_ctx_and_resource| {
            cb(core_ctx_and_resource.cast_right(|r| r.as_ref()))
        })
    )
}

fn get_mtu<BC: BindingsContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    device: &DeviceId<BC>,
) -> Mtu {
    match device {
        DeviceId::Ethernet(id) => self::ethernet::get_mtu(core_ctx, &id),
        DeviceId::Loopback(id) => self::loopback::get_mtu(core_ctx, id),
        DeviceId::PureIp(id) => self::pure_ip::get_mtu(core_ctx, &id),
    }
}

fn join_link_multicast_group<
    BC: BindingsContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device_id: &DeviceId<BC>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::join_link_multicast(
            core_ctx,
            bindings_ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId { .. }) | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
    }
}

fn leave_link_multicast_group<
    BC: BindingsContext,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device_id: &DeviceId<BC>,
    multicast_addr: MulticastAddr<A>,
) {
    match device_id {
        DeviceId::Ethernet(id) => self::ethernet::leave_link_multicast(
            core_ctx,
            bindings_ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId { .. }) | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
    }
}

fn send_ip_frame<BC, S, A, L>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    local_addr: SpecifiedAddr<A>,
    body: S,
    broadcast: Option<<A::Version as IpTypesIpExt>::BroadcastMarker>,
) -> Result<(), S>
where
    BC: BindingsContext,
    S: Serializer,
    S::Buffer: BufferMut,
    A: IpAddress,
    L: LockBefore<crate::lock_ordering::IpState<A::Version>>
        + LockBefore<crate::lock_ordering::LoopbackTxQueue>
        + LockBefore<crate::lock_ordering::PureIpDeviceTxQueue>,
    A::Version: EthernetIpExt + IpTypesIpExt,
    for<'a> CoreCtx<'a, BC, L>: EthernetIpLinkDeviceDynamicStateContext<BC, DeviceId = EthernetDeviceId<BC>>
        + NudHandler<A::Version, EthernetLinkDevice, BC>
        + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = ()>,
{
    match device {
        DeviceId::Ethernet(id) => ethernet::send_ip_frame::<_, _, A, _>(
            core_ctx,
            bindings_ctx,
            &id,
            local_addr,
            body,
            broadcast,
        ),
        DeviceId::Loopback(id) => {
            loopback::send_ip_frame::<_, A, _, _>(core_ctx, bindings_ctx, id, local_addr, body)
        }
        DeviceId::PureIp(id) => {
            pure_ip::send_ip_frame::<_, _, A::Version, _>(core_ctx, bindings_ctx, id, body)
        }
    }
}

impl<'a, BT, L> DeviceCollectionContext<EthernetLinkDevice, BT> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
    L: LockBefore<crate::lock_ordering::DeviceLayerState>,
{
    fn insert(&mut self, device: EthernetPrimaryDeviceId<BT>) {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        let strong = device.clone_strong();
        assert!(devices.ethernet.insert(strong, device).is_none());
    }

    fn remove(&mut self, device: &EthernetDeviceId<BT>) -> Option<EthernetPrimaryDeviceId<BT>> {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.ethernet.remove(device)
    }
}

impl<'a, BT, L> DeviceCollectionContext<LoopbackDevice, BT> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
    L: LockBefore<crate::lock_ordering::DeviceLayerState>,
{
    fn insert(&mut self, device: LoopbackPrimaryDeviceId<BT>) {
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        let prev = devices.loopback.replace(device);
        // NB: At a previous version we returned an error when bindings tried to
        // install the loopback device twice. Turns out that all callers
        // panicked on that error so might as well panic here and simplify the
        // API code.
        assert!(prev.is_none(), "can't install loopback device more than once");
    }

    fn remove(&mut self, device: &LoopbackDeviceId<BT>) -> Option<LoopbackPrimaryDeviceId<BT>> {
        // We assert here because there's an invariant that only one loopback
        // device exists. So if we're calling this function with a loopback
        // device ID then it *must* exist and it *must* be the same as the
        // currently installed device.
        let mut devices = self.write_lock::<crate::lock_ordering::DeviceLayerState>();
        let primary = devices.loopback.take().expect("loopback device not installed");
        assert_eq!(device, &primary);
        Some(primary)
    }
}

impl<'a, BT: BindingsTypes, L> OriginTrackerContext for CoreCtx<'a, BT, L> {
    fn origin_tracker(&mut self) -> OriginTracker {
        self.unlocked_access::<crate::lock_ordering::DeviceLayerStateOrigin>().clone()
    }
}

impl<'a, BT, L> DeviceConfigurationContext<EthernetLinkDevice> for CoreCtx<'a, BT, L>
where
    L: LockBefore<crate::lock_ordering::NudConfig<Ipv4>>
        + LockBefore<crate::lock_ordering::NudConfig<Ipv6>>,
    BT: BindingsTypes,
{
    fn with_nud_config<I: Ip, O, F: FnOnce(Option<&NudUserConfig>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        with_device_state(self, device_id, |state| {
            // NB: We need map_ip here because we can't write a lock ordering
            // restriction for all IP versions.
            let IpInvariant(o) =
                map_ip_twice!(I, IpInvariant((state, f)), |IpInvariant((mut state, f))| {
                    IpInvariant(f(Some(&*state.read_lock::<crate::lock_ordering::NudConfig<I>>())))
                });
            o
        })
    }

    fn with_nud_config_mut<I: Ip, O, F: FnOnce(Option<&mut NudUserConfig>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        with_device_state(self, device_id, |state| {
            // NB: We need map_ip here because we can't write a lock ordering
            // restriction for all IP versions.
            let IpInvariant(o) =
                map_ip_twice!(I, IpInvariant((state, f)), |IpInvariant((mut state, f))| {
                    IpInvariant(f(Some(
                        &mut *state.write_lock::<crate::lock_ordering::NudConfig<I>>(),
                    )))
                });
            o
        })
    }
}

impl<'a, BT, L> DeviceConfigurationContext<LoopbackDevice> for CoreCtx<'a, BT, L>
where
    BT: BindingsTypes,
{
    fn with_nud_config<I: Ip, O, F: FnOnce(Option<&NudUserConfig>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        // Loopback doesn't support NUD.
        f(None)
    }

    fn with_nud_config_mut<I: Ip, O, F: FnOnce(Option<&mut NudUserConfig>) -> O>(
        &mut self,
        _device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        // Loopback doesn't support NUD.
        f(None)
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::EthernetDeviceCounters>
    for StackState<BC>
{
    type Data = EthernetDeviceCounters;
    type Guard<'l> = &'l EthernetDeviceCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.ethernet_device_counters()
    }
}

impl<BC: BindingsContext, L> CounterContext<EthernetDeviceCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&EthernetDeviceCounters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::EthernetDeviceCounters>())
    }
}

impl<BC: BindingsContext> UnlockedAccess<crate::lock_ordering::PureIpDeviceCounters>
    for StackState<BC>
{
    type Data = PureIpDeviceCounters;
    type Guard<'l> = &'l PureIpDeviceCounters where Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        self.pure_ip_device_counters()
    }
}

impl<BC: BindingsContext, L> CounterContext<PureIpDeviceCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&PureIpDeviceCounters) -> O>(&self, cb: F) -> O {
        cb(self.unlocked_access::<crate::lock_ordering::PureIpDeviceCounters>())
    }
}

impl<'a, BC: BindingsContext, L> ResourceCounterContext<DeviceId<BC>, DeviceCounters>
    for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&DeviceCounters) -> O>(
        &mut self,
        device_id: &DeviceId<BC>,
        cb: F,
    ) -> O {
        for_any_device_id!(DeviceId, device_id, id => {
            with_device_state(self, id, |state| cb(state.unlocked_access::<crate::lock_ordering::DeviceCounters>()))
        })
    }
}

impl<'a, BC: BindingsContext, D: DeviceStateSpec, L>
    ResourceCounterContext<BaseDeviceId<D, BC>, DeviceCounters> for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&DeviceCounters) -> O>(
        &mut self,
        device_id: &BaseDeviceId<D, BC>,
        cb: F,
    ) -> O {
        with_device_state(self, device_id, |state| {
            cb(state.unlocked_access::<crate::lock_ordering::DeviceCounters>())
        })
    }
}

impl<'a, BC: BindingsContext, L>
    ResourceCounterContext<EthernetDeviceId<BC>, EthernetDeviceCounters> for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&EthernetDeviceCounters) -> O>(
        &mut self,
        device_id: &EthernetDeviceId<BC>,
        cb: F,
    ) -> O {
        with_device_state(self, device_id, |state| {
            cb(state.unlocked_access::<crate::lock_ordering::EthernetDeviceCounters>())
        })
    }
}

impl<'a, BC: BindingsContext, L>
    ResourceCounterContext<LoopbackDeviceId<BC>, EthernetDeviceCounters> for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&EthernetDeviceCounters) -> O>(
        &mut self,
        device_id: &LoopbackDeviceId<BC>,
        cb: F,
    ) -> O {
        with_device_state(self, device_id, |state| {
            cb(state.unlocked_access::<crate::lock_ordering::EthernetDeviceCounters>())
        })
    }
}

impl<'a, BC: BindingsContext, L> ResourceCounterContext<PureIpDeviceId<BC>, PureIpDeviceCounters>
    for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&PureIpDeviceCounters) -> O>(
        &mut self,
        device_id: &PureIpDeviceId<BC>,
        cb: F,
    ) -> O {
        with_device_state(self, device_id, |state| {
            cb(state.unlocked_access::<crate::lock_ordering::PureIpDeviceCounters>())
        })
    }
}
