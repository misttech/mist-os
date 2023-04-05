// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of an IP device.

use alloc::boxed::Box;
use core::{marker::PhantomData, num::NonZeroU8, ops::Deref as _};

use lock_order::{relation::LockBefore, Locked};
use net_types::{
    ip::{AddrSubnet, Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu},
    LinkLocalUnicastAddr, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{BufferMut, EmptyBuf, Serializer};
use packet_formats::icmp::{
    ndp::{NeighborSolicitation, RouterSolicitation},
    IcmpUnusedCode,
};

use crate::{
    context::{InstantContext, SendFrameContext},
    device::{AnyDevice, DeviceIdContext},
    error::{ExistsError, NotFoundError},
    ip::{
        self,
        device::{
            self, add_ipv6_addr_subnet,
            dad::{DadHandler, DadStateRef, Ipv6DeviceDadContext, Ipv6LayerDadContext},
            del_ipv6_addr, get_ipv4_addr_subnet, get_ipv6_hop_limit, is_ip_device_enabled,
            is_ip_forwarding_enabled,
            route_discovery::{Ipv6RouteDiscoveryState, Ipv6RouteDiscoveryStateContext},
            router_solicitation::{Ipv6DeviceRsContext, Ipv6LayerRsContext},
            send_ip_frame,
            slaac::{
                SlaacAddressEntry, SlaacAddressEntryMut, SlaacAddresses, SlaacAddrsMutAndConfig,
                SlaacStateContext,
            },
            state::{
                AddrConfig, IpDeviceConfiguration, Ipv4DeviceConfiguration, Ipv6AddressEntry,
                Ipv6DadState, Ipv6DeviceConfiguration, SlaacConfig,
            },
            IpDeviceAddressesAccessor, IpDeviceIpExt, IpDeviceNonSyncContext, RemovedReason,
        },
        gmp::{
            igmp::{IgmpContext, IgmpGroupState, IgmpPacketMetadata, IgmpStateContext},
            mld::{MldContext, MldFrameMetadata, MldGroupState, MldStateContext},
            GmpHandler, GmpQueryHandler, GmpState, MulticastGroupSet,
        },
        AddressStatus, IpLayerIpExt, IpLayerNonSyncContext, Ipv4PresentAddressStatus,
        Ipv6PresentAddressStatus, NonSyncContext, SyncCtx, DEFAULT_TTL,
    },
};

pub(super) struct SlaacAddrs<
    'a,
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: device::Ipv6DeviceContext<C>,
> {
    sync_ctx: &'a mut SC,
    device_id: SC::DeviceId,
    _marker: PhantomData<C>,
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
            |Ipv6AddressEntry { addr_sub, state: _, config, deprecated }| match config {
                AddrConfig::Slaac(config) => {
                    Some(SlaacAddressEntryMut { addr_sub: *addr_sub, config, deprecated })
                }
                AddrConfig::Manual => None,
            },
        )))
    })
}

impl<
        'a,
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: device::Ipv6DeviceContext<C> + GmpHandler<Ipv6, C> + DadHandler<C>,
    > SlaacAddresses<C> for SlaacAddrs<'a, C, SC>
{
    fn with_addrs_mut<
        O,
        F: FnOnce(Box<dyn Iterator<Item = SlaacAddressEntryMut<'_, C::Instant>> + '_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let SlaacAddrs { sync_ctx, device_id, _marker } = self;
        with_iter_slaac_addrs_mut(*sync_ctx, device_id, cb)
    }

    fn with_addrs<
        O,
        F: FnOnce(Box<dyn Iterator<Item = SlaacAddressEntry<C::Instant>> + '_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O {
        let SlaacAddrs { sync_ctx, device_id, _marker } = self;
        sync_ctx.with_ip_device_addresses(device_id, |addrs| {
            cb(Box::new(addrs.iter().cloned().filter_map(
                |Ipv6AddressEntry { addr_sub, state: _, config, deprecated }| match config {
                    AddrConfig::Slaac(config) => {
                        Some(SlaacAddressEntry { addr_sub, config, deprecated })
                    }
                    AddrConfig::Manual => None,
                },
            )))
        })
    }

    fn add_addr_sub_and_then<O, F: FnOnce(SlaacAddressEntryMut<'_, C::Instant>, &mut C) -> O>(
        &mut self,
        ctx: &mut C,
        add_addr_sub: AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>,
        slaac_config: SlaacConfig<C::Instant>,
        and_then: F,
    ) -> Result<O, ExistsError> {
        let SlaacAddrs { sync_ctx, device_id, _marker } = self;

        add_ipv6_addr_subnet(
            *sync_ctx,
            ctx,
            device_id,
            add_addr_sub.to_witness(),
            AddrConfig::Slaac(slaac_config),
        )
        .map(|()| {
            with_iter_slaac_addrs_mut(*sync_ctx, device_id, |mut addrs| {
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
        let SlaacAddrs { sync_ctx, device_id, _marker } = self;
        del_ipv6_addr(*sync_ctx, ctx, device_id, &addr.into_specified(), RemovedReason::Manual).map(
            |Ipv6AddressEntry { addr_sub, state: _, config, deprecated: _ }| {
                assert_eq!(&addr_sub.addr(), addr);
                match config {
                    AddrConfig::Slaac(s) => (addr_sub, s),
                    AddrConfig::Manual => {
                        unreachable!(
                        "address {} on device {} should have been a SLAAC address; config = {:?}",
                        addr_sub,
                        *device_id,
                        config
                    );
                    }
                }
            },
        )
    }
}

impl<
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: device::Ipv6DeviceContext<C> + GmpHandler<Ipv6, C> + DadHandler<C>,
    > SlaacStateContext<C> for SC
{
    type SlaacAddrs<'a> = SlaacAddrs<'a, C, SC> where SC: 'a;

    fn with_slaac_addrs_mut_and_configs<
        O,
        F: FnOnce(SlaacAddrsMutAndConfig<'_, C, SlaacAddrs<'_, C, SC>>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        let (config, dad_transmits) =
            self.with_ip_device_configuration(&device_id, |config, _ctx| {
                let Ipv6DeviceConfiguration {
                    dad_transmits,
                    max_router_solicitations: _,
                    slaac_config,
                    ip_config: _,
                } = config;

                (*slaac_config, *dad_transmits)
            });
        let retrans_timer = self.with_retrans_timer(device_id, |retrans_timer| retrans_timer.get());
        let interface_identifier =
            SC::get_eui64_iid(self, device_id).unwrap_or_else(Default::default);

        let mut addrs =
            SlaacAddrs { sync_ctx: self, device_id: device_id.clone(), _marker: PhantomData };

        cb(SlaacAddrsMutAndConfig {
            addrs: &mut addrs,
            config,
            dad_transmits,
            retrans_timer,
            interface_identifier,
            _marker: PhantomData,
        })
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::EthernetDeviceIpState<Ipv6>>>
    Ipv6RouteDiscoveryStateContext<C> for Locked<&SyncCtx<C>, L>
{
    fn with_discovered_routes_mut<F: FnOnce(&mut Ipv6RouteDiscoveryState)>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) {
        crate::device::with_ip_device_state(self, device_id, |mut state| {
            let mut state = state.lock::<crate::lock_ordering::Ipv6DeviceRouteDiscovery>();
            cb(&mut state)
        })
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>>
    IgmpStateContext<C> for Locked<&SyncCtx<C>, L>
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

    fn with_igmp_state_mut<
        O,
        F: FnOnce(GmpState<'_, Ipv4Addr, IgmpGroupState<C::Instant>>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::with_ip_device_state(self, device, |mut state| {
            let (config, mut locked) =
                state.read_lock_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>();
            let Ipv4DeviceConfiguration {
                ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled, forwarding_enabled: _ },
            } = config.deref();
            let mut state = locked.write_lock::<crate::lock_ordering::IpDeviceGmp<Ipv4>>();

            let enabled = *ip_enabled && *gmp_enabled;
            cb(GmpState { enabled, groups: &mut state })
        })
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv4>>> IgmpContext<C>
    for Locked<&SyncCtx<C>, L>
{
    fn get_ip_addr_subnet(&mut self, device: &Self::DeviceId) -> Option<AddrSubnet<Ipv4Addr>> {
        get_ipv4_addr_subnet(self, device)
    }
}

impl<
        C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<Ipv4, C, EmptyBuf>,
    > SendFrameContext<C, EmptyBuf, IgmpPacketMetadata<SC::DeviceId>> for SC
{
    fn send_frame<S: Serializer<Buffer = EmptyBuf>>(
        &mut self,
        ctx: &mut C,
        meta: IgmpPacketMetadata<SC::DeviceId>,
        body: S,
    ) -> Result<(), S> {
        SC::send_ip_frame(self, ctx, &meta.device, meta.dst_ip.into_specified(), body)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>>
    MldStateContext<C> for Locked<&SyncCtx<C>, L>
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

    fn with_mld_state_mut<O, F: FnOnce(GmpState<'_, Ipv6Addr, MldGroupState<C::Instant>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::with_ip_device_state(self, device, |mut state| {
            let (config, mut locked) =
                state.read_lock_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>();
            let Ipv6DeviceConfiguration {
                dad_transmits: _,
                max_router_solicitations: _,
                slaac_config: _,
                ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled, forwarding_enabled: _ },
            } = config.deref();
            let mut state = locked.write_lock::<crate::lock_ordering::IpDeviceGmp<Ipv6>>();

            let enabled = *ip_enabled && *gmp_enabled;
            cb(GmpState { enabled, groups: &mut state })
        })
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv6>>> MldContext<C>
    for Locked<&SyncCtx<C>, L>
{
    fn get_ipv6_link_local_addr(
        &mut self,
        device: &Self::DeviceId,
    ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>> {
        device::IpDeviceAddressesAccessor::<Ipv6, _>::with_ip_device_addresses(
            self,
            device,
            |addrs| {
                addrs.iter().find_map(|a| {
                    if a.state.is_assigned() {
                        LinkLocalUnicastAddr::new(a.addr_sub().addr())
                    } else {
                        None
                    }
                })
            },
        )
    }
}

impl<C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>, SC: device::Ipv6DeviceContext<C>>
    Ipv6DeviceDadContext<C> for SC
{
    fn with_dad_state<O, F: FnOnce(DadStateRef<'_>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
        cb: F,
    ) -> O {
        let retrans_timer = self.with_retrans_timer(device_id, |s| *s);

        self.with_ip_device_configuration(device_id, |config, mut sync_ctx| {
            sync_ctx.with_ip_device_addresses_mut(device_id, |addrs| {
                cb(DadStateRef {
                    address_state: addrs.find_mut(&addr).map(
                        |Ipv6AddressEntry { addr_sub: _, state, config: _, deprecated: _ }| state,
                    ),
                    retrans_timer: &retrans_timer,
                    max_dad_transmits: &config.dad_transmits,
                })
            })
        })
    }
}

impl<
        C: IpLayerNonSyncContext<Ipv6, SC::DeviceId>,
        SC: ip::BufferIpLayerHandler<Ipv6, C, EmptyBuf>,
    > Ipv6LayerDadContext<C> for SC
{
    fn send_dad_packet(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        dst_ip: MulticastAddr<Ipv6Addr>,
        message: NeighborSolicitation,
    ) -> Result<(), ()> {
        crate::ip::icmp::send_ndp_packet(
            self,
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

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::DeviceLayerState>>
    Ipv6DeviceRsContext<C> for Locked<&SyncCtx<C>, L>
{
    type LinkLayerAddr = <Self as device::Ipv6DeviceContext<C>>::LinkLayerAddr;

    fn with_rs_remaining_mut_and_max<
        O,
        F: FnOnce(&mut Option<NonZeroU8>, Option<NonZeroU8>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        crate::device::with_ip_device_state(self, device_id, |mut state| {
            let (config, mut locked) =
                state.read_lock_and::<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>();
            let mut state = locked.lock::<crate::lock_ordering::Ipv6DeviceRouterSolicitations>();
            cb(&mut state, config.max_router_solicitations)
        })
    }

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Self::LinkLayerAddr> {
        device::Ipv6DeviceContext::get_link_layer_addr_bytes(self, device_id)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv6>>> Ipv6LayerRsContext<C>
    for Locked<&SyncCtx<C>, L>
{
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
        let dst_ip = Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS.into_specified();
        let src_ip = device::IpDeviceAddressesAccessor::<Ipv6, _>::with_ip_device_addresses(
            self,
            device_id,
            |addrs| {
                crate::ip::socket::ipv6_source_address_selection::select_ipv6_source_address(
                    dst_ip,
                    device_id,
                    addrs.iter().map(move |a| (a, device_id.clone())),
                )
            },
        );
        crate::ip::icmp::send_ndp_packet(
            self,
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
        Accessor: IpDeviceAddressesAccessor<I, C::Instant> + GmpQueryHandler<I, C>,
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

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv4>>>
    ip::IpDeviceContext<Ipv4, C> for Locked<&SyncCtx<C>, L>
{
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_device_enabled::<Ipv4, _, _>(self, device_id)
    }

    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        _remote: SpecifiedAddr<Ipv4Addr>,
    ) -> Option<SpecifiedAddr<Ipv4Addr>> {
        device::IpDeviceAddressesAccessor::<Ipv4, _>::with_ip_device_addresses(
            self,
            device_id,
            |addrs| addrs.iter().next().map(|subnet| subnet.addr()),
        )
    }

    type DeviceAndAddressStatusIter<'a, 's> =
        FilterPresentWithDevices<Ipv4, <Self as device::IpDeviceContext<Ipv4, C>>::DevicesIter<'a>, <Self as device::IpDeviceContext<Ipv4, C>>::DeviceAddressAndGroupsAccessor<'a>, C> where
                Self: 's;

    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<Ipv4Addr>,
        cb: F,
    ) -> R {
        device::IpDeviceContext::<Ipv4, _>::with_devices_and_state(self, |devices, state| {
            cb(FilterPresentWithDevices::new(devices, state, assignment_state_v4, addr))
        })
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

    fn is_device_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_forwarding_enabled::<Ipv4, _, _>(self, device_id)
    }

    fn get_hop_limit(&mut self, _device_id: &Self::DeviceId) -> NonZeroU8 {
        DEFAULT_TTL
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        device::IpDeviceContext::<Ipv4, _>::get_mtu(self, device_id)
    }
}

impl<C: NonSyncContext, L: LockBefore<crate::lock_ordering::IpState<Ipv6>>>
    ip::IpDeviceContext<Ipv6, C> for Locked<&SyncCtx<C>, L>
{
    fn is_ip_device_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_device_enabled::<Ipv6, _, _>(self, device_id)
    }

    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: SpecifiedAddr<Ipv6Addr>,
    ) -> Option<SpecifiedAddr<Ipv6Addr>> {
        device::IpDeviceAddressesAccessor::<Ipv6, _>::with_ip_device_addresses(
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

    type DeviceAndAddressStatusIter<'a, 's> =
        FilterPresentWithDevices<Ipv6, <Self as device::IpDeviceContext<Ipv6, C>>::DevicesIter<'a>, <Self as device::IpDeviceContext<Ipv6, C>>::DeviceAddressAndGroupsAccessor<'a>, C> where
                Self: 's;

    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<Ipv6Addr>,
        cb: F,
    ) -> R {
        device::IpDeviceContext::<Ipv6, _>::with_devices_and_state(self, |devices, state| {
            cb(FilterPresentWithDevices::new(devices, state, assignment_state_v6, addr))
        })
    }

    fn address_status_for_device(
        &mut self,
        addr: SpecifiedAddr<Ipv6Addr>,
        device_id: &Self::DeviceId,
    ) -> AddressStatus<<Ipv6 as IpLayerIpExt>::AddressStatus> {
        assignment_state_v6(self, device_id, addr)
    }

    fn is_device_forwarding_enabled(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_forwarding_enabled::<Ipv6, _, _>(self, device_id)
    }

    fn get_hop_limit(&mut self, device_id: &Self::DeviceId) -> NonZeroU8 {
        get_ipv6_hop_limit(self, device_id)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        device::IpDeviceContext::<Ipv6, _>::get_mtu(self, device_id)
    }
}

fn assignment_state_v4<
    C: InstantContext,
    SC: IpDeviceAddressesAccessor<Ipv4, C::Instant> + GmpQueryHandler<Ipv4, C>,
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
    SC: IpDeviceAddressesAccessor<Ipv6, C::Instant> + GmpQueryHandler<Ipv6, C>,
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
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<Ipv6, C, EmptyBuf>,
    > SendFrameContext<C, EmptyBuf, MldFrameMetadata<SC::DeviceId>> for SC
{
    fn send_frame<S: Serializer<Buffer = EmptyBuf>>(
        &mut self,
        ctx: &mut C,
        meta: MldFrameMetadata<SC::DeviceId>,
        body: S,
    ) -> Result<(), S> {
        SC::send_ip_frame(self, ctx, &meta.device, meta.dst_ip.into_specified(), body)
    }
}

impl<
        I: IpLayerIpExt + IpDeviceIpExt,
        B: BufferMut,
        C: IpDeviceNonSyncContext<I, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<I, C, B> + ip::IpDeviceContext<I, C>,
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
