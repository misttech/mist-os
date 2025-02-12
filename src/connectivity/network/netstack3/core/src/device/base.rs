// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementations of device layer traits for [`CoreCtx`].

use core::fmt::Debug;
use core::num::NonZeroU8;
use core::ops::Deref as _;

use lock_order::lock::{DelegatedOrderedLockAccess, LockLevelFor, UnlockedAccess};
use lock_order::relation::LockBefore;
use log::debug;
use net_types::ethernet::Mac;
use net_types::ip::{
    AddrSubnet, Ip, IpAddress, IpInvariant, IpVersion, IpVersionMarker, Ipv4, Ipv4Addr, Ipv6,
    Ipv6Addr, Mtu,
};
use net_types::{map_ip_twice, MulticastAddr, SpecifiedAddr, Witness as _};
use netstack3_base::{
    AnyDevice, BroadcastIpExt, CounterContext, DeviceIdContext, ExistsError,
    IpDeviceAddressIdContext, Ipv4DeviceAddr, Ipv6DeviceAddr, NotFoundError, ReceivableFrameMeta,
    RecvIpFrameMeta, ReferenceNotifiersExt, RemoveResourceResultWithContext,
    ResourceCounterContext, SendFrameError, WeakDeviceIdentifier,
};
use netstack3_device::blackhole::{BlackholeDeviceCounters, BlackholeDeviceId};
use netstack3_device::ethernet::{
    self, EthernetDeviceCounters, EthernetDeviceId, EthernetIpLinkDeviceDynamicStateContext,
    EthernetLinkDevice, EthernetPrimaryDeviceId, EthernetWeakDeviceId,
};
use netstack3_device::loopback::{self, LoopbackDevice, LoopbackDeviceId, LoopbackPrimaryDeviceId};
use netstack3_device::pure_ip::{self, PureIpDeviceCounters, PureIpDeviceId};
use netstack3_device::queue::TransmitQueueHandler;
use netstack3_device::socket::{DeviceSocketCounters, DeviceSocketId, HeldDeviceSockets};
use netstack3_device::{
    for_any_device_id, ArpCounters, BaseDeviceId, DeviceCollectionContext,
    DeviceConfigurationContext, DeviceCounters, DeviceId, DeviceLayerState, DeviceStateSpec,
    Devices, DevicesIter, IpLinkDeviceState, IpLinkDeviceStateInner, Ipv6DeviceLinkLayerAddr,
    OriginTracker, OriginTrackerContext, WeakDeviceId,
};
use netstack3_filter::ProofOfEgressCheck;
use netstack3_ip::device::{
    AddressId, AddressIdIter, AssignedAddressState as _, DualStackIpDeviceState,
    IpDeviceAddressContext, IpDeviceConfigurationContext, IpDeviceFlags, IpDeviceIpExt,
    IpDeviceSendContext, IpDeviceStateContext, Ipv4AddressEntry, Ipv4AddressState,
    Ipv4DeviceConfiguration, Ipv6AddressEntry, Ipv6AddressState, Ipv6DadState,
    Ipv6DeviceConfiguration, Ipv6DeviceConfigurationContext, Ipv6DeviceContext,
    Ipv6NetworkLearnedParameters, PrimaryAddressId, WeakAddressId,
};
use netstack3_ip::nud::{
    ConfirmationFlags, DynamicNeighborUpdateSource, NudHandler, NudIpHandler, NudUserConfig,
};
use netstack3_ip::{
    self as ip, DeviceIpLayerMetadata, IpPacketDestination, IpRoutingDeviceContext, RawMetric,
};
use packet::{BufferMut, Serializer};
use packet_formats::ethernet::EthernetIpExt;

use crate::context::prelude::*;
use crate::context::{CoreCtxAndResource, Locked, WrapLockLevel};
use crate::ip::integration::CoreCtxWithIpDeviceConfiguration;
use crate::{BindingsContext, BindingsTypes, CoreCtx, StackState};

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
            // NUD is not supported on Loopback, Blackhole, and Pure IP devices.
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::Blackhole(BlackholeDeviceId { .. })
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
            // NUD is not supported on Loopback, Blackhole, and Pure IP devices.
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::Blackhole(BlackholeDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
        }
    }

    fn flush_neighbor_table(&mut self, bindings_ctx: &mut BC, device_id: &DeviceId<BC>) {
        match device_id {
            DeviceId::Ethernet(id) => {
                NudHandler::<I, EthernetLinkDevice, _>::flush(self, bindings_ctx, &id)
            }
            // NUD is not supported on Loopback, Blackhole, and Pure IP devices.
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::Blackhole(BlackholeDeviceId { .. })
            | DeviceId::PureIp(PureIpDeviceId { .. }) => {}
        }
    }
}

impl<I, D, L, BC> ReceivableFrameMeta<CoreCtx<'_, BC, L>, BC>
    for RecvIpFrameMeta<D, DeviceIpLayerMetadata<BC>, I>
where
    BC: BindingsContext,
    D: Into<DeviceId<BC>>,
    L: LockBefore<crate::lock_ordering::IcmpAllSocketsSet<Ipv4>>,
    I: Ip,
{
    fn receive_meta<B: BufferMut + Debug>(
        self,
        core_ctx: &mut CoreCtx<'_, BC, L>,
        bindings_ctx: &mut BC,
        frame: B,
    ) {
        let RecvIpFrameMeta {
            device,
            frame_dst,
            ip_layer_metadata,
            marker: IpVersionMarker { .. },
        } = self;
        let device = device.into();
        match I::VERSION {
            IpVersion::V4 => ip::receive_ipv4_packet(
                core_ctx,
                bindings_ctx,
                &device,
                frame_dst,
                ip_layer_metadata,
                frame,
            ),
            IpVersion::V6 => ip::receive_ipv6_packet(
                core_ctx,
                bindings_ctx,
                &device,
                frame_dst,
                ip_layer_metadata,
                frame,
            ),
        }
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I: BroadcastIpExt,
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::FilterState<I>>,
    > IpDeviceSendContext<I, BC> for CoreCtx<'_, BC, L>
{
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        destination: IpPacketDestination<I, &DeviceId<BC>>,
        ip_layer_metadata: DeviceIpLayerMetadata<BC>,
        body: S,
        ProofOfEgressCheck { .. }: ProofOfEgressCheck,
    ) -> Result<(), SendFrameError<S>>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        send_ip_frame(self, bindings_ctx, device, destination, ip_layer_metadata, body)
    }
}

#[netstack3_macros::instantiate_ip_impl_block(I)]
impl<
        I: BroadcastIpExt,
        Config,
        BC: BindingsContext,
        L: LockBefore<crate::lock_ordering::FilterState<I>>,
    > IpDeviceSendContext<I, BC> for CoreCtxWithIpDeviceConfiguration<'_, Config, L, BC>
{
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &DeviceId<BC>,
        destination: IpPacketDestination<I, &DeviceId<BC>>,
        ip_layer_metadata: DeviceIpLayerMetadata<BC>,
        body: S,
        ProofOfEgressCheck { .. }: ProofOfEgressCheck,
    ) -> Result<(), SendFrameError<S>>
    where
        S: Serializer,
        S::Buffer: BufferMut,
    {
        let Self { config: _, core_ctx } = self;
        send_ip_frame(core_ctx, bindings_ctx, device, destination, ip_layer_metadata, body)
    }
}

impl<BC: BindingsContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>>
    IpDeviceConfigurationContext<Ipv4, BC> for CoreCtx<'_, BC, L>
{
    type DevicesIter<'s> = DevicesIter<'s, BC>;
    type WithIpDeviceConfigurationInnerCtx<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s Ipv4DeviceConfiguration,
        WrapLockLevel<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>,
        BC,
    >;
    type WithIpDeviceConfigurationMutInner<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv4DeviceConfiguration,
        WrapLockLevel<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>,
        BC,
    >;
    type DeviceAddressAndGroupsAccessor<'s> =
        CoreCtx<'s, BC, WrapLockLevel<crate::lock_ordering::DeviceLayerState>>;

    fn with_ip_device_configuration<
        O,
        F: FnOnce(&Ipv4DeviceConfiguration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut core_ctx_and_resource = ip_device_state_and_core_ctx(self, device_id);
        let (state, mut locked) = core_ctx_and_resource
            .read_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>, _>(|c| {
                c.right()
            });
        cb(
            &state,
            CoreCtxWithIpDeviceConfiguration { config: &state, core_ctx: locked.cast_core_ctx() },
        )
    }

    fn with_ip_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpDeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut core_ctx_and_resource = ip_device_state_and_core_ctx(self, device_id);
        let (mut state, mut locked) = core_ctx_and_resource
            .write_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>, _>(
            |c| c.right(),
        );
        cb(CoreCtxWithIpDeviceConfiguration {
            config: &mut state,
            core_ctx: locked.cast_core_ctx(),
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
        cb(devices.iter(), locked)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let devices = &*self.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|primary| DeviceId::Loopback(primary.clone_strong()))
    }
}

impl<BC: BindingsContext, L> IpDeviceAddressIdContext<Ipv4> for CoreCtx<'_, BC, L> {
    type AddressId = AddressId<Ipv4AddressEntry<BC>>;
    type WeakAddressId = WeakAddressId<Ipv4AddressEntry<BC>>;
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
    type IpDeviceAddressCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IpDeviceAddresses<Ipv4>>>;

    fn with_ip_device_flags<O, F: FnOnce(&IpDeviceFlags) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let flags = &*state.lock::<crate::lock_ordering::IpDeviceFlags<Ipv4>>();
        cb(flags)
    }

    fn add_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: AddrSubnet<Ipv4Addr, Ipv4DeviceAddr>,
        config: <Ipv4 as IpDeviceIpExt>::AddressConfig<BC::Instant>,
    ) -> Result<Self::AddressId, ExistsError> {
        let mut state = ip_device_state(self, device_id);
        let addr_id = state
            .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>()
            .add(Ipv4AddressEntry::new(addr, config));
        addr_id
    }

    fn remove_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: Self::AddressId,
    ) -> RemoveResourceResultWithContext<AddrSubnet<Ipv4Addr>, BC> {
        let mut state = ip_device_state(self, device_id);
        let primary = state
            .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>()
            .remove(&addr.addr().addr())
            .expect("should exist when address ID exists");
        assert!(PrimaryAddressId::ptr_eq(&primary, &addr));
        core::mem::drop(addr);

        BC::unwrap_or_notify_with_new_reference_notifier(primary.into_inner(), |entry| {
            entry.addr_sub().to_witness::<SpecifiedAddr<_>>()
        })
    }

    fn get_address_id(
        &mut self,
        device_id: &Self::DeviceId,
        addr: SpecifiedAddr<Ipv4Addr>,
    ) -> Result<Self::AddressId, NotFoundError> {
        let mut state = ip_device_state(self, device_id);
        let addr_id = state
            .read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv4>>()
            .iter()
            .find(|a| a.addr().addr() == *addr)
            .map(PrimaryAddressId::clone_strong)
            .ok_or(NotFoundError);
        addr_id
    }

    type AddressIdsIter<'a> = AddressIdIter<'a, Ipv4, BC>;
    fn with_address_ids<
        O,
        F: FnOnce(Self::AddressIdsIter<'_>, &mut Self::IpDeviceAddressCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut core_ctx_and_resource = ip_device_state_and_core_ctx(self, device_id);
        let (state, mut locked) = core_ctx_and_resource
            .read_lock_with_and::<crate::lock_ordering::IpDeviceAddresses<Ipv4>, _>(|c| c.right());
        cb(state.strong_iter(), &mut locked.cast_core_ctx())
    }

    fn with_default_hop_limit<O, F: FnOnce(&NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let mut state = state.read_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv4>>();
        cb(&mut state)
    }

    fn with_default_hop_limit_mut<O, F: FnOnce(&mut NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let mut state = state.write_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv4>>();
        cb(&mut state)
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
        WrapLockLevel<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
        BC,
    >;
    type WithIpv6DeviceConfigurationMutInner<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv6DeviceConfiguration,
        WrapLockLevel<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
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
        WrapLockLevel<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
        BC,
    >;
    type WithIpDeviceConfigurationMutInner<'s> = CoreCtxWithIpDeviceConfiguration<
        's,
        &'s mut Ipv6DeviceConfiguration,
        WrapLockLevel<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>,
        BC,
    >;
    type DeviceAddressAndGroupsAccessor<'s> =
        CoreCtx<'s, BC, WrapLockLevel<crate::lock_ordering::DeviceLayerState>>;

    fn with_ip_device_configuration<
        O,
        F: FnOnce(&Ipv6DeviceConfiguration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut core_ctx_and_resource = ip_device_state_and_core_ctx(self, device_id);
        let (state, mut locked) = core_ctx_and_resource
            .read_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>, _>(|c| {
                c.right()
            });
        cb(
            &state,
            CoreCtxWithIpDeviceConfiguration { config: &state, core_ctx: locked.cast_core_ctx() },
        )
    }

    fn with_ip_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpDeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut core_ctx_and_resource = ip_device_state_and_core_ctx(self, device_id);
        let (mut state, mut locked) = core_ctx_and_resource
            .write_lock_with_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>, _>(
            |c| c.right(),
        );
        cb(CoreCtxWithIpDeviceConfiguration {
            config: &mut state,
            core_ctx: locked.cast_core_ctx(),
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
        cb(devices.iter(), locked)
    }

    fn loopback_id(&mut self) -> Option<Self::DeviceId> {
        let devices = &*self.read_lock::<crate::lock_ordering::DeviceLayerState>();
        devices.loopback.as_ref().map(|primary| DeviceId::Loopback(primary.clone_strong()))
    }
}

impl<BC: BindingsContext, L> IpDeviceAddressIdContext<Ipv6> for CoreCtx<'_, BC, L> {
    type AddressId = AddressId<Ipv6AddressEntry<BC>>;
    type WeakAddressId = WeakAddressId<Ipv6AddressEntry<BC>>;
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
    type IpDeviceAddressCtx<'a> =
        CoreCtx<'a, BC, WrapLockLevel<crate::lock_ordering::IpDeviceAddresses<Ipv6>>>;

    fn with_ip_device_flags<O, F: FnOnce(&IpDeviceFlags) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let flags = &*state.lock::<crate::lock_ordering::IpDeviceFlags<Ipv6>>();
        cb(flags)
    }

    fn add_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        config: <Ipv6 as IpDeviceIpExt>::AddressConfig<BC::Instant>,
    ) -> Result<Self::AddressId, ExistsError> {
        let mut state = ip_device_state(self, device_id);
        let addr_id = state
            .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
            .add(Ipv6AddressEntry::new(addr, Ipv6DadState::Uninitialized, config));
        addr_id
    }

    fn remove_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: Self::AddressId,
    ) -> RemoveResourceResultWithContext<AddrSubnet<Ipv6Addr>, BC> {
        let mut state = ip_device_state(self, device_id);
        let primary = state
            .write_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
            .remove(&addr.addr().addr())
            .expect("should exist when address ID exists");
        assert!(PrimaryAddressId::ptr_eq(&primary, &addr));
        core::mem::drop(addr);

        BC::unwrap_or_notify_with_new_reference_notifier(primary.into_inner(), |entry| {
            entry.addr_sub().to_witness::<SpecifiedAddr<_>>()
        })
    }

    fn get_address_id(
        &mut self,
        device_id: &Self::DeviceId,
        addr: SpecifiedAddr<Ipv6Addr>,
    ) -> Result<Self::AddressId, NotFoundError> {
        let mut state = ip_device_state(self, device_id);
        let addr_id = state
            .read_lock::<crate::lock_ordering::IpDeviceAddresses<Ipv6>>()
            .iter()
            .find_map(|a| (a.addr().addr() == *addr).then(|| PrimaryAddressId::clone_strong(a)))
            .ok_or(NotFoundError);
        addr_id
    }

    type AddressIdsIter<'a> = AddressIdIter<'a, Ipv6, BC>;
    fn with_address_ids<
        O,
        F: FnOnce(Self::AddressIdsIter<'_>, &mut Self::IpDeviceAddressCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut core_ctx_and_resource = ip_device_state_and_core_ctx(self, device_id);
        let (state, mut core_ctx) = core_ctx_and_resource
            .read_lock_with_and::<crate::lock_ordering::IpDeviceAddresses<Ipv6>, _>(|c| c.right());
        cb(state.strong_iter(), &mut core_ctx.cast_core_ctx())
    }

    fn with_default_hop_limit<O, F: FnOnce(&NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let mut state = state.read_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv6>>();
        cb(&mut state)
    }

    fn with_default_hop_limit_mut<O, F: FnOnce(&mut NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let mut state = state.write_lock::<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv6>>();
        cb(&mut state)
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

    fn get_link_layer_addr(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Ipv6DeviceLinkLayerAddr> {
        match device_id {
            DeviceId::Ethernet(id) => {
                Some(Ipv6DeviceLinkLayerAddr::Mac(ethernet::get_mac(self, &id).get()))
            }
            DeviceId::Loopback(LoopbackDeviceId { .. })
            | DeviceId::Blackhole(BlackholeDeviceId { .. })
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
            DeviceId::Blackhole(BlackholeDeviceId { .. }) => {}
        }
    }

    fn with_network_learned_parameters<O, F: FnOnce(&Ipv6NetworkLearnedParameters) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let state = state.read_lock::<crate::lock_ordering::Ipv6DeviceLearnedParams>();
        cb(&state)
    }

    fn with_network_learned_parameters_mut<O, F: FnOnce(&mut Ipv6NetworkLearnedParameters) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let mut state = ip_device_state(self, device_id);
        let mut state = state.write_lock::<crate::lock_ordering::Ipv6DeviceLearnedParams>();
        cb(&mut state)
    }
}

impl<BT: BindingsTypes, L> DeviceIdContext<EthernetLinkDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = EthernetDeviceId<BT>;
    type WeakDeviceId = EthernetWeakDeviceId<BT>;
}

impl<BT: BindingsTypes> DelegatedOrderedLockAccess<Devices<BT>> for StackState<BT> {
    type Inner = DeviceLayerState<BT>;
    fn delegate_ordered_lock_access(&self) -> &Self::Inner {
        &self.device
    }
}

impl<BT: BindingsTypes> LockLevelFor<StackState<BT>> for crate::lock_ordering::DeviceLayerState {
    type Data = Devices<BT>;
}

impl<BT: BindingsTypes, L> DeviceIdContext<AnyDevice> for CoreCtx<'_, BT, L> {
    type DeviceId = DeviceId<BT>;
    type WeakDeviceId = WeakDeviceId<BT>;
}

/// It is safe to provide unlocked access to [`IpLinkDeviceStateInner`] itself
/// here because care has been taken to avoid exposing publicly to the core
/// integration crate any state that is held by a lock, as opposed to read-only
/// state that can be accessed safely at any lock level, e.g. state with no
/// interior mutability or atomics.
///
/// Access to state held by locks *must* be mediated using the global lock
/// ordering declared in [`crate::lock_ordering`].
impl<T, BT: BindingsTypes> UnlockedAccess<crate::lock_ordering::UnlockedState>
    for IpLinkDeviceStateInner<T, BT>
{
    type Data = IpLinkDeviceStateInner<T, BT>;
    type Guard<'l>
        = &'l IpLinkDeviceStateInner<T, BT>
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self
    }
}

pub(crate) fn device_state<'a, BT: BindingsTypes, L, D: DeviceStateSpec>(
    core_ctx: &'a mut CoreCtx<'_, BT, L>,
    device_id: &'a BaseDeviceId<D, BT>,
) -> Locked<&'a IpLinkDeviceState<D, BT>, L> {
    let state = device_id.device_state(
        &core_ctx.unlocked_access::<crate::lock_ordering::UnlockedState>().device.origin,
    );
    core_ctx.replace(state)
}

pub(crate) fn device_state_and_core_ctx<'a, BT: BindingsTypes, L, D: DeviceStateSpec>(
    core_ctx: &'a mut CoreCtx<'_, BT, L>,
    id: &'a BaseDeviceId<D, BT>,
) -> CoreCtxAndResource<'a, BT, IpLinkDeviceState<D, BT>, L> {
    let state = id.device_state(
        &core_ctx.unlocked_access::<crate::lock_ordering::UnlockedState>().device.origin,
    );
    core_ctx.adopt(state)
}

pub(crate) fn ip_device_state<'a, BC: BindingsContext, L>(
    core_ctx: &'a mut CoreCtx<'_, BC, L>,
    device: &'a DeviceId<BC>,
) -> Locked<&'a DualStackIpDeviceState<BC>, L> {
    for_any_device_id!(
        DeviceId,
        device,
        id => {
            let state = id.device_state(
                &core_ctx.unlocked_access::<crate::lock_ordering::UnlockedState>().device.origin
            );
            core_ctx.replace(state.as_ref())
        }
    )
}

pub(crate) fn ip_device_state_and_core_ctx<'a, BC: BindingsContext, L>(
    core_ctx: &'a mut CoreCtx<'_, BC, L>,
    device: &'a DeviceId<BC>,
) -> CoreCtxAndResource<'a, BC, DualStackIpDeviceState<BC>, L> {
    for_any_device_id!(
        DeviceId,
        device,
        id => {
            let state = id.device_state(
                &core_ctx.unlocked_access::<crate::lock_ordering::UnlockedState>().device.origin
            );
            core_ctx.adopt(state.as_ref())
        }
    )
}

pub(crate) fn get_mtu<
    BC: BindingsContext,
    L: LockBefore<crate::lock_ordering::EthernetDeviceDynamicState>,
>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    device: &DeviceId<BC>,
) -> Mtu {
    match device {
        DeviceId::Ethernet(id) => ethernet::get_mtu(core_ctx, &id),
        DeviceId::Loopback(id) => device_state(core_ctx, id).cast_with(|s| &s.link.mtu).copied(),
        DeviceId::PureIp(id) => pure_ip::get_mtu(core_ctx, &id),
        DeviceId::Blackhole(_id) => Mtu::no_limit(),
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
        DeviceId::Ethernet(id) => ethernet::join_link_multicast(
            core_ctx,
            bindings_ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId { .. })
        | DeviceId::PureIp(PureIpDeviceId { .. })
        | DeviceId::Blackhole(BlackholeDeviceId { .. }) => {}
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
        DeviceId::Ethernet(id) => ethernet::leave_link_multicast(
            core_ctx,
            bindings_ctx,
            &id,
            MulticastAddr::from(&multicast_addr),
        ),
        DeviceId::Loopback(LoopbackDeviceId { .. })
        | DeviceId::PureIp(PureIpDeviceId { .. })
        | DeviceId::Blackhole(BlackholeDeviceId { .. }) => {}
    }
}

fn send_ip_frame<BC, S, I, L>(
    core_ctx: &mut CoreCtx<'_, BC, L>,
    bindings_ctx: &mut BC,
    device: &DeviceId<BC>,
    destination: IpPacketDestination<I, &DeviceId<BC>>,
    ip_layer_metadata: DeviceIpLayerMetadata<BC>,
    body: S,
) -> Result<(), SendFrameError<S>>
where
    BC: BindingsContext,
    S: Serializer,
    S::Buffer: BufferMut,
    I: EthernetIpExt + BroadcastIpExt,
    L: LockBefore<crate::lock_ordering::IpState<I>>
        + LockBefore<crate::lock_ordering::LoopbackTxQueue>
        + LockBefore<crate::lock_ordering::PureIpDeviceTxQueue>,
    for<'a> CoreCtx<'a, BC, L>: EthernetIpLinkDeviceDynamicStateContext<BC, DeviceId = EthernetDeviceId<BC>>
        + NudHandler<I, EthernetLinkDevice, BC>
        + TransmitQueueHandler<EthernetLinkDevice, BC, Meta = BC::TxMetadata>,
{
    match device {
        DeviceId::Ethernet(id) => ethernet::send_ip_frame(
            core_ctx,
            bindings_ctx,
            id,
            destination,
            body,
            ip_layer_metadata.into_tx_metadata(),
        ),
        DeviceId::Loopback(id) => loopback::send_ip_frame(
            core_ctx,
            bindings_ctx,
            id,
            destination,
            ip_layer_metadata,
            body,
        ),
        DeviceId::PureIp(id) => pure_ip::send_ip_frame(
            core_ctx,
            bindings_ctx,
            id,
            destination,
            body,
            ip_layer_metadata.into_tx_metadata(),
        ),
        DeviceId::Blackhole(id) => {
            // Just drop the frame.
            debug!("dropping frame in send_ip_frame on blackhole device {id:?}");
            core_ctx.increment(id, DeviceCounters::send_frame::<I>);
            Ok(())
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
        self.unlocked_access::<crate::lock_ordering::UnlockedState>().device.origin.clone()
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
        let state = device_state(self, device_id);
        // NB: We need map_ip here because we can't write a lock ordering
        // restriction for all IP versions.
        let IpInvariant(o) =
            map_ip_twice!(I, IpInvariant((state, f)), |IpInvariant((mut state, f))| {
                IpInvariant(f(Some(&*state.read_lock::<crate::lock_ordering::NudConfig<I>>())))
            });
        o
    }

    fn with_nud_config_mut<I: Ip, O, F: FnOnce(Option<&mut NudUserConfig>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        f: F,
    ) -> O {
        let state = device_state(self, device_id);
        // NB: We need map_ip here because we can't write a lock ordering
        // restriction for all IP versions.
        let IpInvariant(o) =
            map_ip_twice!(I, IpInvariant((state, f)), |IpInvariant((mut state, f))| {
                IpInvariant(f(Some(&mut *state.write_lock::<crate::lock_ordering::NudConfig<I>>())))
            });
        o
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

impl<BC: BindingsContext, L> CounterContext<EthernetDeviceCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&EthernetDeviceCounters) -> O>(&self, cb: F) -> O {
        cb(&self.unlocked_access::<crate::lock_ordering::UnlockedState>().device.ethernet_counters)
    }
}

impl<BC: BindingsContext, L> CounterContext<DeviceSocketCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&DeviceSocketCounters) -> O>(&self, cb: F) -> O {
        cb(&self
            .unlocked_access::<crate::lock_ordering::UnlockedState>()
            .device
            .device_socket_counters)
    }
}

impl<BC: BindingsContext, L> CounterContext<PureIpDeviceCounters> for CoreCtx<'_, BC, L> {
    fn with_counters<O, F: FnOnce(&PureIpDeviceCounters) -> O>(&self, cb: F) -> O {
        cb(&self.unlocked_access::<crate::lock_ordering::UnlockedState>().device.pure_ip_counters)
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
            cb(&device_state(self, id)
                .unlocked_access::<crate::lock_ordering::UnlockedState>()
                .counters)
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
        cb(&device_state(self, device_id)
            .unlocked_access::<crate::lock_ordering::UnlockedState>()
            .counters)
    }
}

impl<'a, BC: BindingsContext, L, D: WeakDeviceIdentifier>
    ResourceCounterContext<DeviceSocketId<D, BC>, DeviceSocketCounters> for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&DeviceSocketCounters) -> O>(
        &mut self,
        socket_id: &DeviceSocketId<D, BC>,
        cb: F,
    ) -> O {
        cb(socket_id.counters())
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
        cb(&device_state(self, device_id)
            .unlocked_access::<crate::lock_ordering::UnlockedState>()
            .link
            .counters)
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
        cb(&device_state(self, device_id)
            .unlocked_access::<crate::lock_ordering::UnlockedState>()
            .link
            .counters)
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
        cb(&device_state(self, device_id)
            .unlocked_access::<crate::lock_ordering::UnlockedState>()
            .link
            .counters)
    }
}

// Blackhole devices have no device-specific counters.
impl<'a, BC: BindingsContext, L> CounterContext<BlackholeDeviceCounters> for CoreCtx<'a, BC, L> {
    fn with_counters<O, F: FnOnce(&BlackholeDeviceCounters) -> O>(&self, cb: F) -> O {
        cb(&BlackholeDeviceCounters)
    }
}

impl<'a, BC: BindingsContext, L>
    ResourceCounterContext<BlackholeDeviceId<BC>, BlackholeDeviceCounters> for CoreCtx<'a, BC, L>
{
    fn with_per_resource_counters<O, F: FnOnce(&BlackholeDeviceCounters) -> O>(
        &mut self,
        _device_id: &BlackholeDeviceId<BC>,
        cb: F,
    ) -> O {
        cb(&BlackholeDeviceCounters)
    }
}

impl<T, BT: BindingsTypes> LockLevelFor<IpLinkDeviceStateInner<T, BT>>
    for crate::lock_ordering::DeviceSockets
{
    type Data = HeldDeviceSockets<BT>;
}

impl<BT: BindingsTypes, L> CounterContext<DeviceCounters> for CoreCtx<'_, BT, L> {
    fn with_counters<O, F: FnOnce(&DeviceCounters) -> O>(&self, cb: F) -> O {
        cb(&self.unlocked_access::<crate::lock_ordering::UnlockedState>().device.counters)
    }
}

impl<I: IpDeviceIpExt, BC: BindingsContext, L> IpRoutingDeviceContext<I> for CoreCtx<'_, BC, L>
where
    Self: IpDeviceStateContext<I, BC, DeviceId = DeviceId<BC>>,
{
    fn get_routing_metric(&mut self, device_id: &Self::DeviceId) -> RawMetric {
        let state = ip_device_state(self, device_id);
        *state.unlocked_access::<crate::lock_ordering::UnlockedState>().metric()
    }

    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        IpDeviceStateContext::<I, _>::with_ip_device_flags(
            self,
            device_id,
            |IpDeviceFlags { ip_enabled }| *ip_enabled,
        )
    }
}

impl<BT: BindingsTypes, L> CounterContext<ArpCounters> for CoreCtx<'_, BT, L> {
    fn with_counters<O, F: FnOnce(&ArpCounters) -> O>(&self, cb: F) -> O {
        cb(&self.unlocked_access::<crate::lock_ordering::UnlockedState>().device.arp_counters)
    }
}
