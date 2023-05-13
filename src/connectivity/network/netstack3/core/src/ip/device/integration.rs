// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of an IP device.

use alloc::boxed::Box;
use core::{borrow::Borrow, marker::PhantomData, num::NonZeroU8};

use lock_order::{relation::LockBefore, Locked};
use net_types::{
    ip::{AddrSubnet, Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu},
    LinkLocalUnicastAddr, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{BufferMut, EmptyBuf, Serializer};
use packet_formats::{
    icmp::{
        ndp::{NeighborSolicitation, RouterSolicitation},
        IcmpUnusedCode,
    },
    utils::NonZeroDuration,
};

use crate::{
    context::{InstantContext, SendFrameContext},
    device::{AnyDevice, DeviceIdContext},
    error::{ExistsError, NotFoundError},
    ip::{
        self,
        device::{
            self, add_ipv6_addr_subnet_with_config,
            dad::{DadAddressStateRef, DadContext, DadHandler, DadStateRef},
            del_ipv6_addr_with_config, get_ipv6_hop_limit, is_ip_device_enabled,
            is_ip_forwarding_enabled,
            nud::NudIpHandler,
            route_discovery::{
                Ipv6DiscoveredRoute, Ipv6DiscoveredRoutesContext, Ipv6RouteDiscoveryContext,
                Ipv6RouteDiscoveryState,
            },
            router_solicitation::{RsContext, RsHandler},
            send_ip_frame,
            slaac::{
                SlaacAddressEntry, SlaacAddressEntryMut, SlaacAddresses, SlaacAddrsMutAndConfig,
                SlaacContext,
            },
            state::{
                AddrConfig, IpDeviceAddresses, IpDeviceConfiguration, Ipv4DeviceConfiguration,
                Ipv6AddressEntry, Ipv6AddressFlags, Ipv6DadState, Ipv6DeviceConfiguration,
                SlaacConfig,
            },
            IpDeviceIpExt, IpDeviceNonSyncContext, IpDeviceStateContext, RemovedReason,
        },
        forwarding::AddRouteError,
        gmp::{
            self,
            igmp::{IgmpContext, IgmpGroupState, IgmpPacketMetadata, IgmpStateContext},
            mld::{MldContext, MldFrameMetadata, MldGroupState, MldStateContext},
            GmpHandler, GmpQueryHandler, GmpState, MulticastGroupSet,
        },
        types::{AddableEntry, AddableMetric},
        AddressStatus, IpLayerIpExt, Ipv4PresentAddressStatus, Ipv6PresentAddressStatus,
        NonSyncContext, SyncCtx, DEFAULT_TTL,
    },
};

pub(crate) struct SlaacAddrs<'a, C: NonSyncContext> {
    pub(crate) sync_ctx: SyncCtxWithIpDeviceConfiguration<'a, &'a Ipv6DeviceConfiguration, Ipv6, C>,
    pub(crate) device_id: <SyncCtxWithIpDeviceConfiguration<
        'a,
        &'a Ipv6DeviceConfiguration,
        Ipv6,
        C,
    > as DeviceIdContext<AnyDevice>>::DeviceId,
    pub(crate) config: &'a Ipv6DeviceConfiguration,
    pub(crate) _marker: PhantomData<C>,
}

fn with_iter_slaac_addrs_mut<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: device::Ipv6DeviceContext<C>,
    O,
    F: FnOnce(Box<dyn Iterator<Item = SlaacAddressEntryMut<'_, C::Instant>> + '_>) -> O,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
    cb: F,
) -> O {
    SC::with_ip_device_addresses_mut(sync_ctx, device_id, |addrs| {
        cb(Box::new(addrs.iter_mut().filter_map(
            |Ipv6AddressEntry {
                 addr_sub,
                 state: _,
                 config,
                 flags: Ipv6AddressFlags { deprecated, assigned: _ },
             }| match config {
                AddrConfig::Slaac(config) => {
                    Some(SlaacAddressEntryMut { addr_sub: *addr_sub, config, deprecated })
                }
                AddrConfig::Manual => None,
            },
        )))
    })
}

impl<'a, C: NonSyncContext> SlaacAddresses<C> for SlaacAddrs<'a, C> {
    fn with_addrs_mut<
        O,
        F: FnOnce(Box<dyn Iterator<Item = SlaacAddressEntryMut<'_, C::Instant>> + '_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let SlaacAddrs { sync_ctx, device_id, config: _, _marker } = self;
        with_iter_slaac_addrs_mut(sync_ctx, device_id, cb)
    }

    fn with_addrs<
        O,
        F: FnOnce(Box<dyn Iterator<Item = SlaacAddressEntry<C::Instant>> + '_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let SlaacAddrs { sync_ctx, device_id, config: _, _marker } = self;
        device::IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
            sync_ctx,
            device_id,
            |addrs| {
                cb(Box::new(addrs.iter().cloned().filter_map(
                    |Ipv6AddressEntry {
                         addr_sub,
                         state: _,
                         config,
                         flags: Ipv6AddressFlags { deprecated, assigned: _ },
                     }| {
                        match config {
                            AddrConfig::Slaac(config) => {
                                Some(SlaacAddressEntry { addr_sub, config, deprecated })
                            }
                            AddrConfig::Manual => None,
                        }
                    },
                )))
            },
        )
    }

    fn add_addr_sub_and_then<O, F: FnOnce(SlaacAddressEntryMut<'_, C::Instant>, &mut C) -> O>(
        &mut self,
        ctx: &mut C,
        add_addr_sub: AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>,
        slaac_config: SlaacConfig<C::Instant>,
        and_then: F,
    ) -> Result<O, ExistsError> {
        let SlaacAddrs { sync_ctx, device_id, config, _marker } = self;

        add_ipv6_addr_subnet_with_config(
            sync_ctx,
            ctx,
            device_id,
            add_addr_sub.to_witness(),
            AddrConfig::Slaac(slaac_config),
            config,
        )
        .map(|()| {
            with_iter_slaac_addrs_mut(sync_ctx, device_id, |mut addrs| {
                and_then(
                    addrs
                        .find(|SlaacAddressEntryMut { addr_sub, config: _, deprecated: _ }| {
                            addr_sub == &add_addr_sub
                        })
                        .unwrap(),
                    ctx,
                )
            })
        })
    }

    fn remove_addr(
        &mut self,
        ctx: &mut C,
        addr: &UnicastAddr<Ipv6Addr>,
    ) -> Result<(AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>, SlaacConfig<C::Instant>), NotFoundError>
    {
        let SlaacAddrs { sync_ctx, device_id, config, _marker } = self;
        del_ipv6_addr_with_config(
            sync_ctx,
            ctx,
            device_id,
            &addr.into_specified(),
            RemovedReason::Manual,
            config,
        )
        .map(|Ipv6AddressEntry { addr_sub, state: _, config, flags: _ }| {
            assert_eq!(&addr_sub.addr(), addr);
            match config {
                AddrConfig::Slaac(s) => (addr_sub, s),
                AddrConfig::Manual => {
                    unreachable!(
                        "address {} on device {} should have been a SLAAC address; config = {:?}",
                        addr_sub, *device_id, config
                    );
                }
            }
        })
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceGmp<Ipv4>>> IgmpStateContext<C>
    for Locked<&SyncCtx<C>, L>
{
    fn with_igmp_state<
        O,
        F: FnOnce(&MulticastGroupSet<Ipv4Addr, IgmpGroupState<C::Instant>>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::with_ip_device_state(self, device, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::IpDeviceGmp<Ipv4>>();
            cb(&state)
        })
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceGmp<Ipv6>>> MldStateContext<C>
    for Locked<&SyncCtx<C>, L>
{
    fn with_mld_state<
        O,
        F: FnOnce(&MulticastGroupSet<Ipv6Addr, MldGroupState<C::Instant>>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::with_ip_device_state(self, device, |mut state| {
            let state = state.read_lock::<crate::lock_ordering::IpDeviceGmp<Ipv6>>();
            cb(&state)
        })
    }
}

/// Iterator over a device and its status for an address.
///
/// This is functionally identical to using `Iterator::filter_map` on the
/// provided devices and yielding devices with the address assigned (and the
/// status), but is named so that it can be used as an associated type.
pub(crate) struct FilterPresentWithDevices<
    I: IpLayerIpExt,
    Devices: Iterator<Item = Accessor::DeviceId>,
    Accessor: DeviceIdContext<AnyDevice>,
    C,
> {
    devices: Devices,
    addr: SpecifiedAddr<I::Addr>,
    state_accessor: Accessor,
    assignment_state: fn(
        &mut Accessor,
        &Accessor::DeviceId,
        SpecifiedAddr<I::Addr>,
    ) -> AddressStatus<I::AddressStatus>,
    _marker: PhantomData<C>,
}

impl<
        I: IpLayerIpExt,
        Devices: Iterator<Item = Accessor::DeviceId>,
        Accessor: DeviceIdContext<AnyDevice>,
        C,
    > FilterPresentWithDevices<I, Devices, Accessor, C>
{
    fn new(
        devices: Devices,
        state_accessor: Accessor,
        assignment_state: fn(
            &mut Accessor,
            &Accessor::DeviceId,
            SpecifiedAddr<I::Addr>,
        ) -> AddressStatus<I::AddressStatus>,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Self {
        Self { devices, addr, state_accessor, assignment_state, _marker: PhantomData }
    }
}

impl<
        's,
        C: InstantContext,
        I: Ip + IpLayerIpExt + IpDeviceIpExt,
        Devices: Iterator<Item = Accessor::DeviceId>,
        Accessor: IpDeviceStateContext<I, C> + GmpQueryHandler<I, C>,
    > Iterator for FilterPresentWithDevices<I, Devices, Accessor, C>
where
    <I as IpDeviceIpExt>::State<C::Instant>: 's,
{
    type Item = (Accessor::DeviceId, I::AddressStatus);
    fn next(&mut self) -> Option<Self::Item> {
        let Self { devices, addr, state_accessor, assignment_state, _marker } = self;
        devices
            .filter_map(|d| match assignment_state(state_accessor, &d, *addr) {
                AddressStatus::Present(status) => Some((d, status)),
                AddressStatus::Unassigned => None,
            })
            .next()
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceGmp<Ipv4>>>
    ip::IpDeviceStateContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        _remote: Option<SpecifiedAddr<Ipv4Addr>>,
    ) -> Option<SpecifiedAddr<Ipv4Addr>> {
        device::IpDeviceStateContext::<Ipv4, _>::with_ip_device_addresses(
            self,
            device_id,
            |addrs| addrs.iter().next().map(|subnet| subnet.addr()),
        )
    }

    fn get_hop_limit(&mut self, _device_id: &Self::DeviceId) -> NonZeroU8 {
        DEFAULT_TTL
    }

    fn address_status_for_device(
        &mut self,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        device_id: &Self::DeviceId,
    ) -> AddressStatus<Ipv4PresentAddressStatus> {
        if dst_ip.is_limited_broadcast() {
            return AddressStatus::Present(Ipv4PresentAddressStatus::LimitedBroadcast);
        }

        assignment_state_v4(self, device_id, dst_ip)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>>
    ip::IpDeviceContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_device_enabled::<Ipv4, _, _>(self, device_id)
    }

    type DeviceAndAddressStatusIter<'a, 's> =
        FilterPresentWithDevices<Ipv4, <Self as device::IpDeviceConfigurationContext<Ipv4, C>>::DevicesIter<'a>, <Self as device::IpDeviceConfigurationContext<Ipv4, C>>::DeviceAddressAndGroupsAccessor<'a>, C> where
                Self: 's;

    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<Ipv4Addr>,
        cb: F,
    ) -> R {
        device::IpDeviceConfigurationContext::<Ipv4, _>::with_devices_and_state(
            self,
            |devices, state| {
                cb(FilterPresentWithDevices::new(devices, state, assignment_state_v4, addr))
            },
        )
    }

    fn is_device_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_forwarding_enabled::<Ipv4, _, _>(self, device_id)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        device::IpDeviceConfigurationContext::<Ipv4, _>::get_mtu(self, device_id)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceGmp<Ipv6>>>
    ip::IpDeviceStateContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: Option<SpecifiedAddr<Ipv6Addr>>,
    ) -> Option<SpecifiedAddr<Ipv6Addr>> {
        device::IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
            self,
            device_id,
            |addrs| {
                crate::ip::socket::ipv6_source_address_selection::select_ipv6_source_address(
                    remote,
                    device_id,
                    addrs.iter().map(move |a| (a, device_id.clone())),
                )
                .map(|a| a.into_specified())
            },
        )
    }

    fn get_hop_limit(&mut self, device_id: &Self::DeviceId) -> NonZeroU8 {
        get_ipv6_hop_limit(self, device_id)
    }

    fn address_status_for_device(
        &mut self,
        addr: SpecifiedAddr<Ipv6Addr>,
        device_id: &Self::DeviceId,
    ) -> AddressStatus<<Ipv6 as IpLayerIpExt>::AddressStatus> {
        assignment_state_v6(self, device_id, addr)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>>
    ip::IpDeviceContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_device_enabled::<Ipv6, _, _>(self, device_id)
    }

    type DeviceAndAddressStatusIter<'a, 's> =
        FilterPresentWithDevices<Ipv6, <Self as device::IpDeviceConfigurationContext<Ipv6, C>>::DevicesIter<'a>, <Self as device::IpDeviceConfigurationContext<Ipv6, C>>::DeviceAddressAndGroupsAccessor<'a>, C> where
                Self: 's;

    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<Ipv6Addr>,
        cb: F,
    ) -> R {
        device::IpDeviceConfigurationContext::<Ipv6, _>::with_devices_and_state(
            self,
            |devices, state| {
                cb(FilterPresentWithDevices::new(devices, state, assignment_state_v6, addr))
            },
        )
    }

    fn is_device_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_forwarding_enabled::<Ipv6, _, _>(self, device_id)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        device::IpDeviceConfigurationContext::<Ipv6, _>::get_mtu(self, device_id)
    }
}

fn assignment_state_v4<
    C: InstantContext,
    SC: IpDeviceStateContext<Ipv4, C> + GmpQueryHandler<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    device: &SC::DeviceId,
    addr: SpecifiedAddr<Ipv4Addr>,
) -> AddressStatus<Ipv4PresentAddressStatus> {
    if MulticastAddr::new(addr.get())
        .map_or(false, |addr| GmpQueryHandler::gmp_is_in_group(sync_ctx, device, addr))
    {
        return AddressStatus::Present(Ipv4PresentAddressStatus::Multicast);
    }

    sync_ctx.with_ip_device_addresses(device, |addrs| {
        addrs
            .iter()
            .find_map(|dev_addr| {
                let (dev_addr, subnet) = dev_addr.addr_subnet();

                if dev_addr == addr {
                    Some(AddressStatus::Present(Ipv4PresentAddressStatus::Unicast))
                } else if addr.get() == subnet.broadcast() {
                    Some(AddressStatus::Present(Ipv4PresentAddressStatus::SubnetBroadcast))
                } else {
                    None
                }
            })
            .unwrap_or(AddressStatus::Unassigned)
    })
}

fn assignment_state_v6<
    C: InstantContext,
    SC: IpDeviceStateContext<Ipv6, C> + GmpQueryHandler<Ipv6, C>,
>(
    sync_ctx: &mut SC,
    device: &SC::DeviceId,
    addr: SpecifiedAddr<Ipv6Addr>,
) -> AddressStatus<Ipv6PresentAddressStatus> {
    if MulticastAddr::new(addr.get())
        .map_or(false, |addr| GmpQueryHandler::gmp_is_in_group(sync_ctx, device, addr))
    {
        return AddressStatus::Present(Ipv6PresentAddressStatus::Multicast);
    }

    sync_ctx.with_ip_device_addresses(device, |addrs| {
        addrs.find(&*addr).map(|addr| addr.state).map_or(AddressStatus::Unassigned, |state| {
            match state {
                Ipv6DadState::Assigned => {
                    AddressStatus::Present(Ipv6PresentAddressStatus::UnicastAssigned)
                }
                Ipv6DadState::Uninitialized
                | Ipv6DadState::Tentative { dad_transmits_remaining: _ } => {
                    AddressStatus::Present(Ipv6PresentAddressStatus::UnicastTentative)
                }
            }
        })
    })
}

impl<
        I: IpLayerIpExt + IpDeviceIpExt,
        B: BufferMut,
        C: IpDeviceNonSyncContext<I, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<I, C, B> + ip::IpDeviceStateContext<I, C>,
    > ip::BufferIpDeviceContext<I, C, B> for SC
{
    fn send_ip_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        next_hop: SpecifiedAddr<I::Addr>,
        packet: S,
    ) -> Result<(), S> {
        send_ip_frame(self, ctx, device_id, next_hop, packet)
    }
}

pub(crate) struct SyncCtxWithIpDeviceConfiguration<'a, Config, I: IpDeviceIpExt, C: NonSyncContext>
{
    pub config: Config,
    pub sync_ctx: Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>>,
}

impl<'a, I: gmp::IpExt + IpDeviceIpExt, C: NonSyncContext>
    device::WithIpDeviceConfigurationMutInner<I, C>
    for SyncCtxWithIpDeviceConfiguration<'a, &mut I::Configuration, I, C>
where
    for<'s> SyncCtxWithIpDeviceConfiguration<'s, &'s I::Configuration, I, C>: IpDeviceStateContext<I, C, DeviceId = Self::DeviceId>
        + GmpHandler<I, C>
        + NudIpHandler<I, C>,
{
    type IpDeviceStateCtx<'s> = SyncCtxWithIpDeviceConfiguration<'s, &'s I::Configuration, I, C> where Self: 's;

    fn ip_device_configuration_and_ctx(
        &mut self,
    ) -> (&I::Configuration, Self::IpDeviceStateCtx<'_>) {
        let Self { config, sync_ctx } = self;
        let config = &**config;
        (config, SyncCtxWithIpDeviceConfiguration { config, sync_ctx: sync_ctx.cast_with(|r| r) })
    }

    fn configuration_mut(&mut self) -> &mut I::Configuration {
        let Self { config, sync_ctx: _ } = self;
        *config
    }
}

impl<'a, C: NonSyncContext> device::WithIpv6DeviceConfigurationMutInner<C>
    for SyncCtxWithIpDeviceConfiguration<'a, &mut Ipv6DeviceConfiguration, Ipv6, C>
where
    for<'s> SyncCtxWithIpDeviceConfiguration<'s, &'s Ipv6DeviceConfiguration, Ipv6, C>:
        IpDeviceStateContext<Ipv6, C, DeviceId = Self::DeviceId>
            + GmpHandler<Ipv6, C>
            + NudIpHandler<Ipv6, C>
            + DadHandler<C>
            + RsHandler<C>,
{
    type Ipv6DeviceStateCtx<'s> = SyncCtxWithIpDeviceConfiguration<'s, &'s Ipv6DeviceConfiguration, Ipv6, C> where Self: 's;

    fn ipv6_device_configuration_and_ctx(
        &mut self,
    ) -> (&Ipv6DeviceConfiguration, Self::Ipv6DeviceStateCtx<'_>) {
        let Self { config, sync_ctx } = self;
        let config = &**config;
        (config, SyncCtxWithIpDeviceConfiguration { config, sync_ctx: sync_ctx.cast_with(|r| r) })
    }
}

impl<'a, Config, I: IpDeviceIpExt, C: NonSyncContext> DeviceIdContext<AnyDevice>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, I, C>
{
    type DeviceId = <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>> as DeviceIdContext<AnyDevice>>::DeviceId;
    type WeakDeviceId = <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>> as DeviceIdContext<AnyDevice>>::WeakDeviceId;

    fn downgrade_device_id(&self, device_id: &Self::DeviceId) -> Self::WeakDeviceId {
        let Self { config: _, sync_ctx } = self;
        <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>> as DeviceIdContext<AnyDevice>>::downgrade_device_id(sync_ctx, device_id)
    }

    fn upgrade_weak_device_id(
        &self,
        weak_device_id: &Self::WeakDeviceId,
    ) -> Option<Self::DeviceId> {
        let Self { config: _, sync_ctx } = self;
        <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>> as DeviceIdContext<AnyDevice>>::upgrade_weak_device_id(sync_ctx, weak_device_id)
    }

    fn is_device_installed(&self, device_id: &Self::DeviceId) -> bool {
        let Self { config: _, sync_ctx } = self;
        <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>> as DeviceIdContext<AnyDevice>>::is_device_installed(sync_ctx, device_id)
    }
}

impl<'a, Config: Borrow<Ipv6DeviceConfiguration>, C: NonSyncContext> SlaacContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    type SlaacAddrs<'s> = SlaacAddrs<'s, C>;

    fn with_slaac_addrs_mut_and_configs<
        O,
        F: FnOnce(SlaacAddrsMutAndConfig<'_, C, Self::SlaacAddrs<'_>>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config, sync_ctx } = self;
        let retrans_timer =
            device::Ipv6DeviceContext::with_retrans_timer(sync_ctx, device_id, |retrans_timer| {
                retrans_timer.get()
            });
        let interface_identifier = device::Ipv6DeviceContext::get_eui64_iid(sync_ctx, device_id)
            .unwrap_or_else(Default::default);

        let config = Borrow::borrow(config);
        let Ipv6DeviceConfiguration {
            dad_transmits,
            max_router_solicitations: _,
            slaac_config,
            ip_config: _,
        } = *config;

        let sync_ctx =
            SyncCtxWithIpDeviceConfiguration { config, sync_ctx: sync_ctx.cast_with(|d| d) };

        let mut addrs =
            SlaacAddrs { sync_ctx, device_id: device_id.clone(), config, _marker: PhantomData };

        cb(SlaacAddrsMutAndConfig {
            addrs: &mut addrs,
            config: slaac_config,
            dad_transmits,
            retrans_timer,
            interface_identifier,
            _marker: PhantomData,
        })
    }
}

impl<'a, Config: Borrow<Ipv6DeviceConfiguration>, C: NonSyncContext> DadContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    fn with_dad_state<O, F: FnOnce(DadStateRef<'_>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
        cb: F,
    ) -> O {
        let Self { config, sync_ctx } = self;
        let retrans_timer =
            device::Ipv6DeviceContext::<C>::with_retrans_timer(sync_ctx, device_id, |s| *s);

        device::IpDeviceStateContext::<Ipv6, C>::with_ip_device_addresses_mut(
            sync_ctx,
            device_id,
            |addrs| {
                cb(DadStateRef {
                    state: addrs.find_mut(&addr).map(
                        |Ipv6AddressEntry {
                             addr_sub: _,
                             state,
                             config: _,
                             flags: Ipv6AddressFlags { deprecated: _, assigned },
                         }| DadAddressStateRef {
                            dad_state: state,
                            assigned,
                        },
                    ),
                    retrans_timer: &retrans_timer,
                    max_dad_transmits: &Borrow::borrow(&*config).dad_transmits,
                })
            },
        )
    }

    fn send_dad_packet(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        dst_ip: MulticastAddr<Ipv6Addr>,
        message: NeighborSolicitation,
    ) -> Result<(), ()> {
        let Self { config: _, sync_ctx } = self;
        crate::ip::icmp::send_ndp_packet(
            sync_ctx,
            ctx,
            device_id,
            None,
            dst_ip.into_specified(),
            EmptyBuf,
            IcmpUnusedCode,
            message,
        )
        .map_err(|EmptyBuf| ())
    }
}

impl<'a, Config: Borrow<Ipv6DeviceConfiguration>, C: NonSyncContext> RsContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    type LinkLayerAddr = <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<Ipv6>> as device::Ipv6DeviceContext<C>>::LinkLayerAddr;

    /// Calls the callback with a mutable reference to the remaining number of
    /// router soliciations to send and the maximum number of router solications
    /// to send.
    fn with_rs_remaining_mut_and_max<
        O,
        F: FnOnce(&mut Option<NonZeroU8>, Option<NonZeroU8>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config, sync_ctx } = self;
        crate::device::with_ip_device_state(sync_ctx, device_id, |mut state| {
            let mut state = state.lock::<crate::lock_ordering::Ipv6DeviceRouterSolicitations>();
            cb(&mut state, Borrow::borrow(&*config).max_router_solicitations)
        })
    }

    /// Gets the device's link-layer address bytes, if the device supports
    /// link-layer addressing.
    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Self::LinkLayerAddr> {
        let Self { config: _, sync_ctx } = self;
        device::Ipv6DeviceContext::get_link_layer_addr_bytes(sync_ctx, device_id)
    }

    /// Sends an NDP Router Solicitation to the local-link.
    ///
    /// The callback is called with a source address suitable for an outgoing
    /// router solicitation message and returns the message body.
    fn send_rs_packet<
        S: Serializer<Buffer = EmptyBuf>,
        F: FnOnce(Option<UnicastAddr<Ipv6Addr>>) -> S,
    >(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        message: RouterSolicitation,
        body: F,
    ) -> Result<(), S> {
        let Self { config: _, sync_ctx } = self;
        let dst_ip = Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS.into_specified();
        let src_ip = device::IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
            sync_ctx,
            device_id,
            |addrs| {
                crate::ip::socket::ipv6_source_address_selection::select_ipv6_source_address(
                    Some(dst_ip),
                    device_id,
                    addrs.iter().map(move |a| (a, device_id.clone())),
                )
            },
        );
        crate::ip::icmp::send_ndp_packet(
            sync_ctx,
            ctx,
            device_id,
            src_ip.map(|a| a.into_specified()),
            dst_ip,
            body(src_ip),
            IcmpUnusedCode,
            message,
        )
    }
}

impl<C: NonSyncContext> Ipv6DiscoveredRoutesContext<C>
    for Locked<&SyncCtx<C>, crate::lock_ordering::Ipv6DeviceRouteDiscovery>
{
    fn add_discovered_ipv6_route(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        Ipv6DiscoveredRoute { subnet, gateway }: Ipv6DiscoveredRoute,
    ) -> Result<(), AddRouteError> {
        let device_id = device_id.clone();
        let entry = match gateway {
            Some(gateway) => AddableEntry::with_gateway(
                subnet,
                Some(device_id),
                (*gateway).into_specified(),
                AddableMetric::MetricTracksInterface,
            ),
            None => AddableEntry::without_gateway(
                subnet,
                device_id,
                AddableMetric::MetricTracksInterface,
            ),
        };

        crate::ip::forwarding::add_route::<Ipv6, _, _>(self, ctx, entry)
    }

    fn del_discovered_ipv6_route(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        Ipv6DiscoveredRoute { subnet, gateway }: Ipv6DiscoveredRoute,
    ) -> Result<(), NotFoundError> {
        crate::ip::forwarding::del_specific_routes::<Ipv6, _, _>(
            self,
            ctx,
            subnet,
            device_id,
            gateway.map(|g| (*g).into_specified()),
        )
    }
}

impl<'a, Config, C: NonSyncContext> Ipv6RouteDiscoveryContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    type WithDiscoveredRoutesMutCtx<'b> =
        Locked<&'b SyncCtx<C>, crate::lock_ordering::Ipv6DeviceRouteDiscovery>;

    fn with_discovered_routes_mut<
        F: FnOnce(&mut Ipv6RouteDiscoveryState, &mut Self::WithDiscoveredRoutesMutCtx<'_>),
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) {
        let Self { config: _, sync_ctx } = self;
        crate::device::with_ip_device_state_and_sync_ctx(
            sync_ctx,
            device_id,
            |mut state, sync_ctx| {
                let mut state = state.lock::<crate::lock_ordering::Ipv6DeviceRouteDiscovery>();
                // We lock the state according to the IPv6 route discovery
                // lock level but the callback needs access to the sync context
                // so we cast its lock-level to IPv6 route discovery so that
                // only locks that may be acquired _after_ the IPv6 route
                // discovery lock may be acquired.
                //
                // Note that specifying the lock-level type in `cast_locked`
                // is not explicitly needed as `Self::WithDiscoveredRoutesMutCtx`
                // includes the lock-level, but we do it here to be explicit
                // and catch changes to the associated type.
                //
                // TODO(https://fxbug.dev/126743): Support this natively in the
                // lock-order crate.
                cb(
                    &mut state,
                    &mut sync_ctx.cast_locked::<crate::lock_ordering::Ipv6DeviceRouteDiscovery>(),
                )
            },
        )
    }
}

impl<'a, Config, C: NonSyncContext> device::Ipv6DeviceContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    type LinkLayerAddr = <Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<Ipv6>> as device::Ipv6DeviceContext<C>>::LinkLayerAddr;

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Self::LinkLayerAddr> {
        let Self { config: _, sync_ctx } = self;
        device::Ipv6DeviceContext::get_link_layer_addr_bytes(sync_ctx, device_id)
    }

    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]> {
        let Self { config: _, sync_ctx } = self;
        device::Ipv6DeviceContext::get_eui64_iid(sync_ctx, device_id)
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        let Self { config: _, sync_ctx } = self;
        device::Ipv6DeviceContext::set_link_mtu(sync_ctx, device_id, mtu)
    }

    fn with_retrans_timer<O, F: FnOnce(&NonZeroDuration) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config: _, sync_ctx } = self;
        device::Ipv6DeviceContext::with_retrans_timer(sync_ctx, device_id, cb)
    }

    fn with_retrans_timer_mut<O, F: FnOnce(&mut NonZeroDuration) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config: _, sync_ctx } = self;
        device::Ipv6DeviceContext::with_retrans_timer_mut(sync_ctx, device_id, cb)
    }
}

impl<'a, Config, I: IpDeviceIpExt, C: NonSyncContext> device::IpDeviceStateContext<I, C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, I, C>
where
    Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>>:
        device::IpDeviceStateContext<I, C>,
{
    fn with_ip_device_addresses<O, F: FnOnce(&IpDeviceAddresses<C::Instant, I>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<I, C>::with_ip_device_addresses(sync_ctx, device_id, cb)
    }

    fn with_ip_device_addresses_mut<O, F: FnOnce(&mut IpDeviceAddresses<C::Instant, I>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<I, C>::with_ip_device_addresses_mut(sync_ctx, device_id, cb)
    }

    fn with_default_hop_limit<O, F: FnOnce(&NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<I, C>::with_default_hop_limit(sync_ctx, device_id, cb)
    }

    fn with_default_hop_limit_mut<O, F: FnOnce(&mut NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<I, C>::with_default_hop_limit_mut(sync_ctx, device_id, cb)
    }

    fn join_link_multicast_group(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    ) {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<I, C>::join_link_multicast_group(
            sync_ctx,
            ctx,
            device_id,
            multicast_addr,
        )
    }

    fn leave_link_multicast_group(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    ) {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<I, C>::leave_link_multicast_group(
            sync_ctx,
            ctx,
            device_id,
            multicast_addr,
        )
    }
}

impl<'a, Config: Borrow<Ipv4DeviceConfiguration>, C: NonSyncContext> IgmpContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv4, C>
{
    /// Calls the function with a mutable reference to the device's IGMP state
    /// and whether or not IGMP is enabled for the `device`.
    fn with_igmp_state_mut<
        O,
        F: FnOnce(GmpState<'_, Ipv4Addr, IgmpGroupState<C::Instant>>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config, sync_ctx } = self;
        let Ipv4DeviceConfiguration {
            ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled, forwarding_enabled: _ },
        } = Borrow::borrow(&*config);

        crate::device::with_ip_device_state(sync_ctx, device, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::IpDeviceGmp<Ipv4>>();
            let enabled = *ip_enabled && *gmp_enabled;
            cb(GmpState { enabled, groups: &mut state })
        })
    }

    fn get_ip_addr_subnet(&mut self, device: &Self::DeviceId) -> Option<AddrSubnet<Ipv4Addr>> {
        let Self { config: _, sync_ctx } = self;
        crate::ip::device::get_ipv4_addr_subnet(sync_ctx, device)
    }
}

impl<'a, Config, C: NonSyncContext>
    SendFrameContext<
        C,
        EmptyBuf,
        IgmpPacketMetadata<<Self as DeviceIdContext<AnyDevice>>::DeviceId>,
    > for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv4, C>
{
    fn send_frame<S: Serializer<Buffer = EmptyBuf>>(
        &mut self,
        ctx: &mut C,
        meta: IgmpPacketMetadata<<Self as DeviceIdContext<AnyDevice>>::DeviceId>,
        body: S,
    ) -> Result<(), S> {
        let Self { config: _, sync_ctx } = self;
        device::BufferIpDeviceContext::<Ipv4, _, _>::send_ip_frame(
            sync_ctx,
            ctx,
            &meta.device,
            meta.dst_ip.into_specified(),
            body,
        )
    }
}

impl<'a, Config: Borrow<Ipv6DeviceConfiguration>, C: NonSyncContext> MldContext<C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    fn with_mld_state_mut<O, F: FnOnce(GmpState<'_, Ipv6Addr, MldGroupState<C::Instant>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        let Self { config, sync_ctx } = self;
        let Ipv6DeviceConfiguration {
            dad_transmits: _,
            max_router_solicitations: _,
            slaac_config: _,
            ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled, forwarding_enabled: _ },
        } = Borrow::borrow(&*config);

        crate::device::with_ip_device_state(sync_ctx, device, |mut state| {
            let mut state = state.write_lock::<crate::lock_ordering::IpDeviceGmp<Ipv6>>();
            let enabled = *ip_enabled && *gmp_enabled;
            cb(GmpState { enabled, groups: &mut state })
        })
    }

    fn get_ipv6_link_local_addr(
        &mut self,
        device: &Self::DeviceId,
    ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>> {
        let Self { config: _, sync_ctx } = self;
        device::IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
            sync_ctx,
            device,
            |addrs| {
                addrs.iter().find_map(|a| {
                    if a.flags.assigned {
                        LinkLocalUnicastAddr::new(a.addr_sub().addr())
                    } else {
                        None
                    }
                })
            },
        )
    }
}

impl<'a, Config, C: NonSyncContext>
    SendFrameContext<C, EmptyBuf, MldFrameMetadata<<Self as DeviceIdContext<AnyDevice>>::DeviceId>>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, Ipv6, C>
{
    fn send_frame<S: Serializer<Buffer = EmptyBuf>>(
        &mut self,
        ctx: &mut C,
        meta: MldFrameMetadata<<Self as DeviceIdContext<AnyDevice>>::DeviceId>,
        body: S,
    ) -> Result<(), S> {
        let Self { config: _, sync_ctx } = self;
        device::BufferIpDeviceContext::<Ipv6, _, _>::send_ip_frame(
            sync_ctx,
            ctx,
            &meta.device,
            meta.dst_ip.into_specified(),
            body,
        )
    }
}

impl<'a, Config, I: IpDeviceIpExt, C: NonSyncContext> NudIpHandler<I, C>
    for SyncCtxWithIpDeviceConfiguration<'a, Config, I, C>
where
    Locked<&'a SyncCtx<C>, crate::lock_ordering::IpDeviceConfiguration<I>>: NudIpHandler<I, C>,
{
    fn handle_neighbor_probe(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        let Self { config: _, sync_ctx } = self;
        NudIpHandler::<I, C>::handle_neighbor_probe(sync_ctx, ctx, device_id, neighbor, link_addr)
    }

    fn handle_neighbor_confirmation(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        neighbor: SpecifiedAddr<I::Addr>,
        link_addr: &[u8],
    ) {
        let Self { config: _, sync_ctx } = self;
        NudIpHandler::<I, C>::handle_neighbor_confirmation(
            sync_ctx, ctx, device_id, neighbor, link_addr,
        )
    }

    fn flush_neighbor_table(&mut self, ctx: &mut C, device_id: &Self::DeviceId) {
        let Self { config: _, sync_ctx } = self;
        NudIpHandler::<I, C>::flush_neighbor_table(sync_ctx, ctx, device_id)
    }
}
