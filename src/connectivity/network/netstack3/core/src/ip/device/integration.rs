// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The integrations for protocols built on top of an IP device.

use alloc::boxed::Box;
use core::{marker::PhantomData, num::NonZeroU8, time::Duration};

use net_types::{
    ip::{AddrSubnet, GenericOverIp, Ip, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    LinkLocalUnicastAddr, MulticastAddr, SpecifiedAddr, UnicastAddr, Witness as _,
};
use packet::{BufferMut, EmptyBuf, Serializer};
use packet_formats::icmp::{
    ndp::{NeighborSolicitation, RouterSolicitation},
    IcmpUnusedCode,
};

use crate::{
    context::FrameContext,
    device::Mtu,
    error::{ExistsError, NotFoundError},
    ip::{
        self,
        device::{
            self, add_ipv6_addr_subnet,
            dad::{DadHandler, Ipv6DeviceDadContext, Ipv6LayerDadContext},
            del_ipv6_addr, get_ipv4_addr_subnet, get_ipv6_hop_limit, is_ip_device_enabled,
            is_ip_routing_enabled,
            route_discovery::{Ipv6RouteDiscoveryState, Ipv6RouteDiscoveryStateContext},
            router_solicitation::{Ipv6DeviceRsContext, Ipv6LayerRsContext},
            send_ip_frame,
            slaac::{
                SlaacAddressEntry, SlaacAddressEntryMut, SlaacAddresses, SlaacAddrsMutAndConfig,
                SlaacStateContext,
            },
            state::{
                AddrConfig, AddressState, IpDeviceConfiguration, IpDeviceState, IpDeviceStateIpExt,
                Ipv4DeviceConfiguration, Ipv4DeviceState, Ipv6AddressEntry,
                Ipv6DeviceConfiguration, Ipv6DeviceState, SlaacConfig,
            },
            IpDeviceIpExt, IpDeviceNonSyncContext, IpDeviceStateAccessor,
        },
        gmp::{
            igmp::{IgmpContext, IgmpGroupState, IgmpPacketMetadata},
            mld::{MldContext, MldFrameMetadata, MldGroupState},
            GmpHandler, GmpState,
        },
        AddressStatus, IpLayerIpExt, IpLayerNonSyncContext, Ipv4PresentAddressStatus,
        Ipv6PresentAddressStatus, DEFAULT_TTL,
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
    SC::with_ip_device_state_mut(sync_ctx, device_id, |state| {
        cb(Box::new(state.ip_state.iter_addrs_mut().filter_map(
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
        sync_ctx.with_ip_device_state(device_id, |state| {
            cb(Box::new(state.ip_state.iter_addrs().cloned().filter_map(
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
        del_ipv6_addr(*sync_ctx, ctx, device_id, &addr.into_specified()).map(
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
        let (config, dad_transmits, retrans_timer) =
            self.with_ip_device_state(&device_id, |state| {
                let Ipv6DeviceState {
                    retrans_timer,
                    route_discovery: _,
                    router_soliciations_remaining: _,
                    ip_state: _,
                    config:
                        Ipv6DeviceConfiguration {
                            dad_transmits,
                            max_router_solicitations: _,
                            slaac_config,
                            ip_config: _,
                        },
                } = state;

                (*slaac_config, *dad_transmits, retrans_timer.get())
            });
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

impl<C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>, SC: device::IpDeviceContext<Ipv6, C>>
    Ipv6RouteDiscoveryStateContext<C> for SC
{
    fn with_discovered_routes_mut<F: FnOnce(&mut Ipv6RouteDiscoveryState)>(
        &mut self,
        device_id: &SC::DeviceId,
        cb: F,
    ) {
        self.with_ip_device_state_mut(&device_id, |state| cb(&mut state.route_discovery))
    }
}

impl<
        C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<Ipv4, C, EmptyBuf>,
    > IgmpContext<C> for SC
{
    fn get_ip_addr_subnet(&mut self, device: &SC::DeviceId) -> Option<AddrSubnet<Ipv4Addr>> {
        get_ipv4_addr_subnet(self, device)
    }

    fn with_igmp_state_mut<
        O,
        F: FnOnce(GmpState<'_, Ipv4Addr, IgmpGroupState<C::Instant>>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_ip_device_state_mut(device, |state| {
            let Ipv4DeviceState {
                ip_state,
                config:
                    Ipv4DeviceConfiguration {
                        ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled },
                    },
            } = state;

            let enabled = *ip_enabled && *gmp_enabled;
            cb(GmpState { enabled, groups: &mut ip_state.multicast_groups })
        })
    }
}

impl<
        C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<Ipv4, C, EmptyBuf>,
    > FrameContext<C, EmptyBuf, IgmpPacketMetadata<SC::DeviceId>> for SC
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

impl<
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<Ipv6, C, EmptyBuf>,
    > MldContext<C> for SC
{
    fn get_ipv6_link_local_addr(
        &mut self,
        device: &SC::DeviceId,
    ) -> Option<LinkLocalUnicastAddr<Ipv6Addr>> {
        self.with_ip_device_state(device, |state| {
            state.ip_state.iter_addrs().find_map(|a| {
                if a.state.is_assigned() {
                    LinkLocalUnicastAddr::new(a.addr_sub().addr())
                } else {
                    None
                }
            })
        })
    }

    fn with_mld_state_mut<O, F: FnOnce(GmpState<'_, Ipv6Addr, MldGroupState<C::Instant>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_ip_device_state_mut(device, |state| {
            let Ipv6DeviceState {
                retrans_timer: _,
                route_discovery: _,
                router_soliciations_remaining: _,
                ip_state,
                config:
                    Ipv6DeviceConfiguration {
                        dad_transmits: _,
                        max_router_solicitations: _,
                        slaac_config: _,
                        ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled },
                    },
            } = state;
            let enabled = *ip_enabled && *gmp_enabled;
            cb(GmpState { enabled, groups: &mut ip_state.multicast_groups })
        })
    }
}

impl<C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>, SC: device::Ipv6DeviceContext<C>>
    Ipv6DeviceDadContext<C> for SC
{
    fn with_address_state_and_retrans_timer<
        O,
        F: FnOnce(Option<(&mut AddressState, Duration)>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
        cb: F,
    ) -> O {
        self.with_ip_device_state_mut(device_id, |state| {
            let retrans_timer = state.retrans_timer.get();
            cb(state.ip_state.find_addr_mut(&addr).map(
                |Ipv6AddressEntry { addr_sub: _, state, config: _, deprecated: _ }| {
                    (state, retrans_timer)
                },
            ))
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

impl<C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>, SC: device::Ipv6DeviceContext<C>>
    Ipv6DeviceRsContext<C> for SC
{
    type LinkLayerAddr = SC::LinkLayerAddr;

    fn with_rs_remaining_mut_and_max<
        O,
        F: FnOnce(&mut Option<NonZeroU8>, Option<NonZeroU8>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_ip_device_state_mut(&device_id, |state| {
            cb(&mut state.router_soliciations_remaining, state.config.max_router_solicitations)
        })
    }

    fn get_link_layer_addr_bytes(&mut self, device_id: &SC::DeviceId) -> Option<SC::LinkLayerAddr> {
        device::Ipv6DeviceContext::get_link_layer_addr_bytes(self, device_id)
    }
}

impl<
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId> + IpLayerNonSyncContext<Ipv6, SC::DeviceId>,
        SC: ip::BufferIpLayerHandler<Ipv6, C, EmptyBuf> + device::Ipv6DeviceContext<C>,
    > Ipv6LayerRsContext<C> for SC
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
        let src_ip = self.with_ip_device_state(device_id, |state| {
            crate::ip::socket::ipv6_source_address_selection::select_ipv6_source_address(
                dst_ip,
                device_id,
                state.ip_state.iter_addrs().map(move |a| (a, device_id.clone())),
            )
        });
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
pub(crate) struct FilterPresentWithDevices<I: Ip, Devices, Accessor, Instant> {
    devices: Devices,
    addr: SpecifiedAddr<I::Addr>,
    state_accessor: Accessor,
    _marker: PhantomData<Instant>,
}

impl<I: Ip, Devices, Accessor, Instant> FilterPresentWithDevices<I, Devices, Accessor, Instant> {
    fn new(devices: Devices, state_accessor: Accessor, addr: SpecifiedAddr<I::Addr>) -> Self {
        Self { devices, addr, state_accessor, _marker: PhantomData }
    }
}

impl<
        's,
        Instant: crate::Instant,
        I: Ip + IpLayerIpExt + IpDeviceIpExt,
        Devices: Iterator<Item = Accessor::DeviceId>,
        Accessor: IpDeviceStateAccessor<I, Instant>,
    > Iterator for FilterPresentWithDevices<I, Devices, Accessor, Instant>
where
    <I as IpDeviceIpExt>::State<Instant>: 's,
{
    type Item = (Accessor::DeviceId, I::AddressStatus);
    fn next(&mut self) -> Option<Self::Item> {
        let Self { devices, addr, state_accessor, _marker } = self;
        devices
            .filter_map(|d| {
                match state_accessor
                    .with_ip_device_state(&d, |state| assignment_state(*addr, state))
                {
                    AddressStatus::Present(status) => Some((d, status)),
                    AddressStatus::Unassigned => None,
                }
            })
            .next()
    }
}

impl<C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>, SC: device::IpDeviceContext<Ipv4, C>>
    ip::IpDeviceContext<Ipv4, C> for SC
{
    fn is_ip_device_enabled(&mut self, device_id: &SC::DeviceId) -> bool {
        is_ip_device_enabled(self, device_id)
    }

    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        _remote: SpecifiedAddr<Ipv4Addr>,
    ) -> Option<SpecifiedAddr<Ipv4Addr>> {
        self.with_ip_device_state(device_id, |state| {
            state.ip_state.iter_addrs().next().map(|subnet| subnet.addr())
        })
    }

    type DeviceAndAddressStatusIter<'a, 's> =
        FilterPresentWithDevices<Ipv4, SC::DevicesIter<'a>, SC::DeviceStateAccessor<'a>, C::Instant> where
                Self: 's;

    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<Ipv4Addr>,
        cb: F,
    ) -> R {
        self.with_devices_and_state(|devices, state| {
            cb(FilterPresentWithDevices::new(devices, state, addr))
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

        self.with_ip_device_state(device_id, |state| assignment_state_v4(dst_ip, &state.ip_state))
    }

    fn is_device_routing_enabled(&mut self, device_id: &SC::DeviceId) -> bool {
        is_ip_routing_enabled(self, device_id)
    }

    fn get_hop_limit(&mut self, _device_id: &SC::DeviceId) -> NonZeroU8 {
        DEFAULT_TTL
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        self.get_mtu(device_id)
    }
}

impl<C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>, SC: device::IpDeviceContext<Ipv6, C>>
    ip::IpDeviceContext<Ipv6, C> for SC
{
    fn is_ip_device_enabled(&mut self, device_id: &SC::DeviceId) -> bool {
        is_ip_device_enabled(self, device_id)
    }

    fn get_local_addr_for_remote(
        &mut self,
        device_id: &Self::DeviceId,
        remote: SpecifiedAddr<Ipv6Addr>,
    ) -> Option<SpecifiedAddr<Ipv6Addr>> {
        self.with_ip_device_state(device_id, |state| {
            crate::ip::socket::ipv6_source_address_selection::select_ipv6_source_address(
                remote,
                device_id,
                state.ip_state.iter_addrs().map(move |a| (a, device_id.clone())),
            )
            .map(|a| a.into_specified())
        })
    }

    type DeviceAndAddressStatusIter<'a, 's> =
        FilterPresentWithDevices<Ipv6, SC::DevicesIter<'a>, SC::DeviceStateAccessor<'a>, C::Instant> where
                Self: 's;

    fn with_address_statuses<'a, F: FnOnce(Self::DeviceAndAddressStatusIter<'_, 'a>) -> R, R>(
        &'a mut self,
        addr: SpecifiedAddr<Ipv6Addr>,
        cb: F,
    ) -> R {
        self.with_devices_and_state(|devices, state| {
            cb(FilterPresentWithDevices::new(devices, state, addr))
        })
    }

    fn address_status_for_device(
        &mut self,
        addr: SpecifiedAddr<Ipv6Addr>,
        device_id: &Self::DeviceId,
    ) -> AddressStatus<<Ipv6 as IpLayerIpExt>::AddressStatus> {
        self.with_ip_device_state(device_id, |state| assignment_state_v6(addr, &state.ip_state))
    }

    fn is_device_routing_enabled(&mut self, device_id: &SC::DeviceId) -> bool {
        is_ip_routing_enabled(self, device_id)
    }

    fn get_hop_limit(&mut self, device_id: &SC::DeviceId) -> NonZeroU8 {
        get_ipv6_hop_limit(self, device_id)
    }

    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu {
        self.get_mtu(device_id)
    }
}

fn assignment_state<I: IpLayerIpExt + IpDeviceIpExt, Instant: crate::Instant>(
    addr: SpecifiedAddr<I::Addr>,
    state: &impl AsRef<IpDeviceState<Instant, I>>,
) -> AddressStatus<I::AddressStatus> {
    let dev_state = state.as_ref();
    #[derive(GenericOverIp)]
    struct WrapDeviceState<'a, I: Ip + IpDeviceStateIpExt, Instant: crate::Instant>(
        &'a IpDeviceState<Instant, I>,
    );

    #[derive(GenericOverIp)]
    struct WrapAddressStatus<I: Ip + IpLayerIpExt>(AddressStatus<I::AddressStatus>);
    let WrapAddressStatus(status) = I::map_ip(
        (addr, WrapDeviceState(dev_state)),
        |(addr, WrapDeviceState(state))| WrapAddressStatus(assignment_state_v4(addr, state)),
        |(addr, WrapDeviceState(state))| WrapAddressStatus(assignment_state_v6(addr, state)),
    );
    status
}

fn assignment_state_v4<Instant: crate::Instant>(
    addr: SpecifiedAddr<Ipv4Addr>,
    dev_state: &IpDeviceState<Instant, Ipv4>,
) -> AddressStatus<Ipv4PresentAddressStatus> {
    if MulticastAddr::new(addr.get())
        .map_or(false, |addr| dev_state.multicast_groups.contains(&addr))
    {
        return AddressStatus::Present(Ipv4PresentAddressStatus::Multicast);
    }

    dev_state
        .iter_addrs()
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
}

fn assignment_state_v6<Instant: crate::Instant>(
    addr: SpecifiedAddr<Ipv6Addr>,
    dev_state: &IpDeviceState<Instant, Ipv6>,
) -> AddressStatus<Ipv6PresentAddressStatus> {
    if MulticastAddr::new(addr.get())
        .map_or(false, |addr| dev_state.multicast_groups.contains(&addr))
    {
        AddressStatus::Present(Ipv6PresentAddressStatus::Multicast)
    } else {
        dev_state.find_addr(&*addr).map(|addr| addr.state).map_or(
            AddressStatus::Unassigned,
            |state| match state {
                AddressState::Assigned => {
                    AddressStatus::Present(Ipv6PresentAddressStatus::UnicastAssigned)
                }
                AddressState::Tentative { dad_transmits_remaining: _ } => {
                    AddressStatus::Present(Ipv6PresentAddressStatus::UnicastTentative)
                }
            },
        )
    }
}

impl<
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: device::BufferIpDeviceContext<Ipv6, C, EmptyBuf>,
    > FrameContext<C, EmptyBuf, MldFrameMetadata<SC::DeviceId>> for SC
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
