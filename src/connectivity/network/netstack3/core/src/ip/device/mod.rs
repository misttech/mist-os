// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An IP device.

pub mod dad;
pub(crate) mod integration;
pub mod nud;
pub mod route_discovery;
pub(crate) mod router_solicitation;
pub mod slaac;
pub mod state;

use alloc::{boxed::Box, vec::Vec};
use core::num::NonZeroU8;

use net_types::{
    ip::{
        AddrSubnet, AddrSubnetEither, Ip, IpAddress as _, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
        Ipv6SourceAddr, Mtu, Subnet,
    },
    MulticastAddr, SpecifiedAddr, UnicastAddr,
};
use packet::{BufferMut, Serializer};
use packet_formats::{
    icmp::{mld::MldPacket, ndp::NonZeroNdpLifetime},
    utils::NonZeroDuration,
};
use zerocopy::ByteSlice;

use crate::{
    context::{
        CounterContext, EventContext, InstantContext, RngContext, TimerContext, TimerHandler,
    },
    device::Id as _,
    error::{ExistsError, NotFoundError, NotSupportedError},
    ip::{
        device::{
            dad::{DadEvent, DadHandler, DadTimerId},
            nud::NudIpHandler,
            route_discovery::{
                Ipv6DiscoveredRoute, Ipv6DiscoveredRouteTimerId, RouteDiscoveryHandler,
            },
            router_solicitation::{RsHandler, RsTimerId},
            slaac::{SlaacConfiguration, SlaacHandler, SlaacTimerId},
            state::{
                AddrConfig, DelIpv6AddrReason, IpDeviceAddresses, IpDeviceConfiguration,
                IpDeviceState, IpDeviceStateIpExt, Ipv4DeviceConfiguration, Ipv4DeviceState,
                Ipv6AddressEntry, Ipv6DadState, Ipv6DeviceConfiguration, Ipv6DeviceState,
            },
        },
        gmp::{
            igmp::{IgmpPacketHandler, IgmpTimerId},
            mld::{MldDelayedReportTimerId, MldPacketHandler},
            GmpHandler, GmpQueryHandler, GroupJoinResult, GroupLeaveResult,
        },
        AnyDevice, DeviceIdContext,
    },
    Instant,
};

/// A timer ID for IPv4 devices.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub(crate) struct Ipv4DeviceTimerId<DeviceId>(IgmpTimerId<DeviceId>);

impl<DeviceId> Ipv4DeviceTimerId<DeviceId> {
    fn device_id(&self) -> &DeviceId {
        let Self(this) = self;
        this.device_id()
    }
}

impl<DeviceId> From<IgmpTimerId<DeviceId>> for Ipv4DeviceTimerId<DeviceId> {
    fn from(id: IgmpTimerId<DeviceId>) -> Ipv4DeviceTimerId<DeviceId> {
        Ipv4DeviceTimerId(id)
    }
}

// If we are provided with an impl of `TimerContext<Ipv4DeviceTimerId<_>>`, then
// we can in turn provide an impl of `TimerContext` for IGMP.
impl_timer_context!(
    DeviceId,
    Ipv4DeviceTimerId<DeviceId>,
    IgmpTimerId<DeviceId>,
    Ipv4DeviceTimerId(id),
    id
);

impl<DeviceId, C, SC: TimerHandler<C, IgmpTimerId<DeviceId>>>
    TimerHandler<C, Ipv4DeviceTimerId<DeviceId>> for SC
{
    fn handle_timer(&mut self, ctx: &mut C, Ipv4DeviceTimerId(id): Ipv4DeviceTimerId<DeviceId>) {
        TimerHandler::handle_timer(self, ctx, id)
    }
}

/// Handle an IPv4 device timer firing.
pub(crate) fn handle_ipv4_timer<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: Ipv4DeviceTimerId<SC::DeviceId>,
) {
    let device_id = id.device_id().clone();
    sync_ctx.with_ip_device_configuration(&device_id, |_state, mut sync_ctx| {
        TimerHandler::handle_timer(&mut sync_ctx, ctx, id)
    })
}

/// A timer ID for IPv6 devices.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub(crate) enum Ipv6DeviceTimerId<DeviceId> {
    Mld(MldDelayedReportTimerId<DeviceId>),
    Dad(DadTimerId<DeviceId>),
    Rs(RsTimerId<DeviceId>),
    RouteDiscovery(Ipv6DiscoveredRouteTimerId<DeviceId>),
    Slaac(SlaacTimerId<DeviceId>),
}

impl<DeviceId> Ipv6DeviceTimerId<DeviceId> {
    fn device_id(&self) -> &DeviceId {
        match self {
            Self::Mld(id) => id.device_id(),
            Self::Dad(id) => id.device_id(),
            Self::Rs(id) => id.device_id(),
            Self::RouteDiscovery(id) => id.device_id(),
            Self::Slaac(id) => id.device_id(),
        }
    }
}

impl<DeviceId> From<MldDelayedReportTimerId<DeviceId>> for Ipv6DeviceTimerId<DeviceId> {
    fn from(id: MldDelayedReportTimerId<DeviceId>) -> Ipv6DeviceTimerId<DeviceId> {
        Ipv6DeviceTimerId::Mld(id)
    }
}

impl<DeviceId> From<DadTimerId<DeviceId>> for Ipv6DeviceTimerId<DeviceId> {
    fn from(id: DadTimerId<DeviceId>) -> Ipv6DeviceTimerId<DeviceId> {
        Ipv6DeviceTimerId::Dad(id)
    }
}

impl<DeviceId> From<RsTimerId<DeviceId>> for Ipv6DeviceTimerId<DeviceId> {
    fn from(id: RsTimerId<DeviceId>) -> Ipv6DeviceTimerId<DeviceId> {
        Ipv6DeviceTimerId::Rs(id)
    }
}

impl<DeviceId> From<Ipv6DiscoveredRouteTimerId<DeviceId>> for Ipv6DeviceTimerId<DeviceId> {
    fn from(id: Ipv6DiscoveredRouteTimerId<DeviceId>) -> Ipv6DeviceTimerId<DeviceId> {
        Ipv6DeviceTimerId::RouteDiscovery(id)
    }
}

impl<DeviceId> From<SlaacTimerId<DeviceId>> for Ipv6DeviceTimerId<DeviceId> {
    fn from(id: SlaacTimerId<DeviceId>) -> Ipv6DeviceTimerId<DeviceId> {
        Ipv6DeviceTimerId::Slaac(id)
    }
}

// If we are provided with an impl of `TimerContext<Ipv6DeviceTimerId<_>>`, then
// we can in turn provide an impl of `TimerContext` for MLD and DAD.
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    MldDelayedReportTimerId<DeviceId>,
    Ipv6DeviceTimerId::Mld(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    DadTimerId<DeviceId>,
    Ipv6DeviceTimerId::Dad(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    RsTimerId<DeviceId>,
    Ipv6DeviceTimerId::Rs(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    Ipv6DiscoveredRouteTimerId<DeviceId>,
    Ipv6DeviceTimerId::RouteDiscovery(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    SlaacTimerId<DeviceId>,
    Ipv6DeviceTimerId::Slaac(id),
    id
);

impl<
        DeviceId,
        C,
        SC: TimerHandler<C, RsTimerId<DeviceId>>
            + TimerHandler<C, Ipv6DiscoveredRouteTimerId<DeviceId>>
            + TimerHandler<C, MldDelayedReportTimerId<DeviceId>>
            + TimerHandler<C, SlaacTimerId<DeviceId>>
            + TimerHandler<C, DadTimerId<DeviceId>>,
    > TimerHandler<C, Ipv6DeviceTimerId<DeviceId>> for SC
{
    fn handle_timer(&mut self, ctx: &mut C, id: Ipv6DeviceTimerId<DeviceId>) {
        match id {
            Ipv6DeviceTimerId::Mld(id) => TimerHandler::handle_timer(self, ctx, id),
            Ipv6DeviceTimerId::Dad(id) => TimerHandler::handle_timer(self, ctx, id),
            Ipv6DeviceTimerId::Rs(id) => TimerHandler::handle_timer(self, ctx, id),
            Ipv6DeviceTimerId::RouteDiscovery(id) => TimerHandler::handle_timer(self, ctx, id),
            Ipv6DeviceTimerId::Slaac(id) => TimerHandler::handle_timer(self, ctx, id),
        }
    }
}

/// Handle an IPv6 device timer firing.
pub(crate) fn handle_ipv6_timer<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv6, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    id: Ipv6DeviceTimerId<SC::DeviceId>,
) {
    let device_id = id.device_id().clone();
    sync_ctx.with_ip_device_configuration(&device_id, |_state, mut sync_ctx| {
        TimerHandler::handle_timer(&mut sync_ctx, ctx, id)
    })
}

/// An extension trait adding IP device properties.
pub(crate) trait IpDeviceIpExt: IpDeviceStateIpExt {
    type State<I: Instant>: AsRef<IpDeviceState<I, Self>> + AsMut<IpDeviceState<I, Self>>;
    type Configuration: AsRef<IpDeviceConfiguration>;
    type Timer<DeviceId>;
}

impl IpDeviceIpExt for Ipv4 {
    type State<I: Instant> = Ipv4DeviceState<I>;
    type Configuration = Ipv4DeviceConfiguration;
    type Timer<DeviceId> = Ipv4DeviceTimerId<DeviceId>;
}

impl IpDeviceIpExt for Ipv6 {
    type State<I: Instant> = Ipv6DeviceState<I>;
    type Configuration = Ipv6DeviceConfiguration;
    type Timer<DeviceId> = Ipv6DeviceTimerId<DeviceId>;
}

/// IP address assignment states.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IpAddressState {
    /// The address is unavailable because it's interface is not IP enabled.
    Unavailable,
    /// The address is assigned to an interface and can be considered bound to
    /// it (all packets destined to the address will be accepted).
    Assigned,
    /// The address is considered unassigned to an interface for normal
    /// operations, but has the intention of being assigned in the future (e.g.
    /// once Duplicate Address Detection is completed).
    Tentative,
}

/// The reason an address was removed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RemovedReason {
    /// The address was removed in response to external action.
    Manual,
    /// The address was removed because it was detected as a duplicate via DAD.
    DadFailed,
}

impl From<DelIpv6AddrReason> for RemovedReason {
    fn from(reason: DelIpv6AddrReason) -> Self {
        match reason {
            DelIpv6AddrReason::ManualAction => Self::Manual,
            DelIpv6AddrReason::DadFailed => Self::DadFailed,
        }
    }
}

#[derive(Debug, Eq, Hash, PartialEq)]
/// Events emitted from IP devices.
pub enum IpDeviceEvent<DeviceId, I: Ip> {
    /// Address was assigned.
    AddressAdded {
        /// The device.
        device: DeviceId,
        /// The new address.
        addr: AddrSubnet<I::Addr>,
        /// Initial address state.
        state: IpAddressState,
    },
    /// Address was unassigned.
    AddressRemoved {
        /// The device.
        device: DeviceId,
        /// The removed address.
        addr: SpecifiedAddr<I::Addr>,
        /// The reason the address was removed.
        reason: RemovedReason,
    },
    /// Address state changed.
    AddressStateChanged {
        /// The device.
        device: DeviceId,
        /// The address whose state was changed.
        addr: SpecifiedAddr<I::Addr>,
        /// The new address state.
        state: IpAddressState,
    },
    /// IP was enabled/disabled on the device
    EnabledChanged {
        /// The device.
        device: DeviceId,
        /// `true` if IP was enabled on the device; `false` if IP was disabled.
        ip_enabled: bool,
    },
}

impl<DeviceId, C: EventContext<IpDeviceEvent<DeviceId, Ipv6>>> EventContext<DadEvent<DeviceId>>
    for C
{
    fn on_event(&mut self, event: DadEvent<DeviceId>) {
        match event {
            DadEvent::AddressAssigned { device, addr } => C::on_event(
                self,
                IpDeviceEvent::AddressStateChanged {
                    device,
                    addr: addr.into_specified(),
                    state: IpAddressState::Assigned,
                },
            ),
        }
    }
}

pub(crate) struct DualStackDeviceStateRef<'a, I: Instant> {
    pub(crate) ipv4: &'a IpDeviceAddresses<I, Ipv4>,
    pub(crate) ipv6: &'a IpDeviceAddresses<I, Ipv6>,
}

/// The non-synchronized execution context for dual-stack devices.
pub(crate) trait DualStackDeviceNonSyncContext: InstantContext {}
impl<C: InstantContext> DualStackDeviceNonSyncContext for C {}

/// The synchronized execution context for dual-stack devices.
pub(crate) trait DualStackDeviceContext<C: DualStackDeviceNonSyncContext>:
    DeviceIdContext<AnyDevice>
{
    /// Calls the function with an immutable view into the dual-stack device's
    /// state.
    fn with_dual_stack_device_state<O, F: FnOnce(DualStackDeviceStateRef<'_, C::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// An implementation of dual-stack devices.
pub(crate) trait DualStackDeviceHandler<C>: DeviceIdContext<AnyDevice> {
    /// Get all IPv4 and IPv6 address/subnet pairs configured on a device.
    fn get_all_ip_addr_subnets(&mut self, device_id: &Self::DeviceId) -> Vec<AddrSubnetEither>;
}

impl<C: DualStackDeviceNonSyncContext, SC: DualStackDeviceContext<C>> DualStackDeviceHandler<C>
    for SC
{
    fn get_all_ip_addr_subnets(&mut self, device_id: &Self::DeviceId) -> Vec<AddrSubnetEither> {
        self.with_dual_stack_device_state(device_id, |DualStackDeviceStateRef { ipv4, ipv6 }| {
            let addrs_v4 =
                ipv4.iter().filter_map(<Ipv4 as IpDeviceStateIpExt>::assigned_addr::<C::Instant>);
            let addrs_v6 =
                ipv6.iter().filter_map(<Ipv6 as IpDeviceStateIpExt>::assigned_addr::<C::Instant>);

            addrs_v4.map(AddrSubnetEither::V4).chain(addrs_v6.map(AddrSubnetEither::V6)).collect()
        })
    }
}

/// The non-synchronized execution context for IP devices.
pub(crate) trait IpDeviceNonSyncContext<I: IpDeviceIpExt, DeviceId>:
    RngContext
    + TimerContext<I::Timer<DeviceId>>
    + EventContext<IpDeviceEvent<DeviceId, I>>
    + CounterContext
{
}
impl<
        DeviceId,
        I: IpDeviceIpExt,
        C: RngContext
            + TimerContext<I::Timer<DeviceId>>
            + EventContext<IpDeviceEvent<DeviceId, I>>
            + CounterContext,
    > IpDeviceNonSyncContext<I, DeviceId> for C
{
}

/// Accessor for IP device state.
pub(crate) trait IpDeviceStateContext<I: IpDeviceIpExt, C: InstantContext>:
    DeviceIdContext<AnyDevice>
{
    /// Calls the function with an immutable reference to the device's IP
    /// addresses.
    fn with_ip_device_addresses<O, F: FnOnce(&IpDeviceAddresses<C::Instant, I>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the device's IP
    /// addresses.
    fn with_ip_device_addresses_mut<O, F: FnOnce(&mut IpDeviceAddresses<C::Instant, I>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with an immutable reference to the device's default
    /// hop limit for this IP version.
    fn with_default_hop_limit<O, F: FnOnce(&NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the device's default
    /// hop limit for this IP version.
    fn with_default_hop_limit_mut<O, F: FnOnce(&mut NonZeroU8) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Joins the link-layer multicast group associated with the given IP
    /// multicast group.
    fn join_link_multicast_group(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    );

    /// Leaves the link-layer multicast group associated with the given IP
    /// multicast group.
    fn leave_link_multicast_group(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    );
}

/// The context provided to the callback passed to
/// [`IpDeviceConfigurationContext::with_ip_device_configuration_mut`].
pub(crate) trait WithIpDeviceConfigurationMutInner<I: IpDeviceIpExt, C: InstantContext>:
    DeviceIdContext<AnyDevice>
{
    type IpDeviceStateCtx<'s>: IpDeviceStateContext<I, C, DeviceId = Self::DeviceId>
        + GmpHandler<I, C>
        + NudIpHandler<I, C>
        + 's
    where
        Self: 's;

    /// Returns an immutable reference to a device's IP configuration and an
    /// `IpDeviceStateCtx`.
    fn ip_device_configuration_and_ctx(
        &mut self,
    ) -> (&I::Configuration, Self::IpDeviceStateCtx<'_>);

    /// Returns a mutable reference to a device's IP configuration.
    fn configuration_mut(&mut self) -> &mut I::Configuration;
}

/// The execution context for IP devices.
pub(crate) trait IpDeviceConfigurationContext<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, Self::DeviceId>,
>: IpDeviceStateContext<I, C> + DeviceIdContext<AnyDevice>
{
    type DevicesIter<'s>: Iterator<Item = Self::DeviceId> + 's;
    type WithIpDeviceConfigurationInnerCtx<'s>: IpDeviceStateContext<I, C, DeviceId = Self::DeviceId>
        + GmpHandler<I, C>
        + NudIpHandler<I, C>
        + TimerHandler<C, I::Timer<Self::DeviceId>>
        + 's;
    type WithIpDeviceConfigurationMutInner<'s>: WithIpDeviceConfigurationMutInner<I, C, DeviceId = Self::DeviceId>
        + 's;
    type DeviceAddressAndGroupsAccessor<'s>: IpDeviceStateContext<I, C, DeviceId = Self::DeviceId>
        + GmpQueryHandler<I, C>
        + 's;

    /// Calls the function with an immutable reference to the IP device
    /// configuration and a `WithIpDeviceConfigurationInnerCtx`.
    fn with_ip_device_configuration<
        O,
        F: FnOnce(&I::Configuration, Self::WithIpDeviceConfigurationInnerCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a `WithIpDeviceConfigurationMutInner`.
    fn with_ip_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpDeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with an [`Iterator`] of IDs for all initialized
    /// devices and an accessor for device state.
    fn with_devices_and_state<
        O,
        F: FnOnce(Self::DevicesIter<'_>, Self::DeviceAddressAndGroupsAccessor<'_>) -> O,
    >(
        &mut self,
        cb: F,
    ) -> O;

    /// Gets the MTU for a device.
    ///
    /// The MTU is the maximum size of an IP packet.
    fn get_mtu(&mut self, device_id: &Self::DeviceId) -> Mtu;

    /// Returns the ID of the loopback interface, if one exists on the system
    /// and is initialized.
    fn loopback_id(&mut self) -> Option<Self::DeviceId>;
}

/// The context provided to the callback passed to
/// [`Ipv6DeviceConfigurationContext::with_ipv6_device_configuration_mut`].
pub(crate) trait WithIpv6DeviceConfigurationMutInner<
    C: IpDeviceNonSyncContext<Ipv6, Self::DeviceId>,
>: WithIpDeviceConfigurationMutInner<Ipv6, C>
{
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<C, DeviceId = Self::DeviceId>
        + GmpHandler<Ipv6, C>
        + NudIpHandler<Ipv6, C>
        + RsHandler<C>
        + DadHandler<C>
        + SlaacHandler<C>
        + RouteDiscoveryHandler<C>
        + 's
    where
        Self: 's;

    /// Returns an immutable reference to a device's IPv6 configuration and an
    /// `Ipv6DeviceStateCtx`.
    fn ipv6_device_configuration_and_ctx(
        &mut self,
    ) -> (&Ipv6DeviceConfiguration, Self::Ipv6DeviceStateCtx<'_>);
}

pub(crate) trait Ipv6DeviceConfigurationContext<C: IpDeviceNonSyncContext<Ipv6, Self::DeviceId>>:
    IpDeviceConfigurationContext<Ipv6, C>
{
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<C, DeviceId = Self::DeviceId>
        + GmpHandler<Ipv6, C>
        + MldPacketHandler<C, Self::DeviceId>
        + NudIpHandler<Ipv6, C>
        + DadHandler<C>
        + RsHandler<C>
        + SlaacHandler<C>
        + RouteDiscoveryHandler<C>
        + 's;
    type WithIpv6DeviceConfigurationMutInner<'s>: WithIpv6DeviceConfigurationMutInner<C, DeviceId = Self::DeviceId>
        + 's;

    /// Calls the function with an immutable reference to the IPv6 device
    /// configuration and an `Ipv6DeviceStateCtx`.
    fn with_ipv6_device_configuration<
        O,
        F: FnOnce(&Ipv6DeviceConfiguration, Self::Ipv6DeviceStateCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a `WithIpv6DeviceConfigurationMutInner`.
    fn with_ipv6_device_configuration_mut<
        O,
        F: FnOnce(Self::WithIpv6DeviceConfigurationMutInner<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// Provides access to an IPv4 device configuration, with a buffer.
pub(crate) trait BufferIpv4DeviceConfigurationContext<
    C: IpDeviceNonSyncContext<Ipv4, Self::DeviceId>,
    B: BufferMut,
>: IpDeviceConfigurationContext<Ipv4, C>
{
    type Ipv4DeviceStateCtx<'s>: IpDeviceStateContext<Ipv4, C, DeviceId = Self::DeviceId>
        + IgmpPacketHandler<C, Self::DeviceId, B>
        + 's;

    /// Calls the function with an immutable reference to the IPv4 device
    /// configuration and an `Ipv4DeviceStateCtx`.
    fn with_ipv4_device_configuration<
        O,
        F: FnOnce(&Ipv4DeviceConfiguration, Self::Ipv4DeviceStateCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

impl<
        C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
        B: BufferMut,
        SC: IpDeviceConfigurationContext<Ipv4, C>,
    > BufferIpv4DeviceConfigurationContext<C, B> for SC
where
    for<'s> SC::WithIpDeviceConfigurationInnerCtx<'s>: IpDeviceStateContext<Ipv4, C, DeviceId = SC::DeviceId>
        + IgmpPacketHandler<C, SC::DeviceId, B>
        + 's,
{
    type Ipv4DeviceStateCtx<'s> = SC::WithIpDeviceConfigurationInnerCtx<'s>;

    fn with_ipv4_device_configuration<
        O,
        F: FnOnce(&Ipv4DeviceConfiguration, Self::Ipv4DeviceStateCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_ip_device_configuration(device_id, cb)
    }
}

/// The execution context for an IPv6 device.
pub(crate) trait Ipv6DeviceContext<C: IpDeviceNonSyncContext<Ipv6, Self::DeviceId>>:
    IpDeviceStateContext<Ipv6, C>
{
    /// A link-layer address.
    type LinkLayerAddr: AsRef<[u8]>;

    /// Gets the device's link-layer address bytes, if the device supports
    /// link-layer addressing.
    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Self::LinkLayerAddr>;

    /// Gets the device's EUI-64 based interface identifier.
    ///
    /// A `None` value indicates the device does not have an EUI-64 based
    /// interface identifier.
    fn get_eui64_iid(&mut self, device_id: &Self::DeviceId) -> Option<[u8; 8]>;

    /// Sets the link MTU for the device.
    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu);

    /// Calls the function with an immutable reference to the retransmit timer.
    fn with_retrans_timer<O, F: FnOnce(&NonZeroDuration) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the retransmit timer.
    fn with_retrans_timer_mut<O, F: FnOnce(&mut NonZeroDuration) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// An implementation of an IP device.
pub(crate) trait IpDeviceHandler<I: Ip, C>: DeviceIdContext<AnyDevice> {
    fn is_router_device(&mut self, device_id: &Self::DeviceId) -> bool;

    fn set_default_hop_limit(&mut self, device_id: &Self::DeviceId, hop_limit: NonZeroU8);
}

impl<
        I: IpDeviceIpExt,
        C: IpDeviceNonSyncContext<I, SC::DeviceId>,
        SC: IpDeviceConfigurationContext<I, C>,
    > IpDeviceHandler<I, C> for SC
{
    fn is_router_device(&mut self, device_id: &Self::DeviceId) -> bool {
        is_ip_forwarding_enabled(self, device_id)
    }

    fn set_default_hop_limit(&mut self, device_id: &Self::DeviceId, hop_limit: NonZeroU8) {
        self.with_default_hop_limit_mut(device_id, |default_hop_limit| {
            *default_hop_limit = hop_limit
        })
    }
}

/// An implementation of an IPv4 device with a buffer, `B`.
pub(crate) trait BufferIpv4DeviceHandler<C, B>: IpDeviceHandler<Ipv4, C> {
    /// Receive an IGMP packet.
    fn receive_igmp_packet(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        buffer: B,
    );
}

impl<
        C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
        SC: IpDeviceHandler<Ipv4, C> + BufferIpv4DeviceConfigurationContext<C, B>,
        B: BufferMut,
    > BufferIpv4DeviceHandler<C, B> for SC
{
    fn receive_igmp_packet(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        src_ip: Ipv4Addr,
        dst_ip: SpecifiedAddr<Ipv4Addr>,
        buffer: B,
    ) {
        self.with_ipv4_device_configuration(device, |_config, mut sync_ctx| {
            IgmpPacketHandler::receive_igmp_packet(
                &mut sync_ctx,
                ctx,
                device,
                src_ip,
                dst_ip,
                buffer,
            )
        })
    }
}

/// An implementation of an IPv6 device.
pub(crate) trait Ipv6DeviceHandler<C>: IpDeviceHandler<Ipv6, C> {
    /// A link-layer address.
    type LinkLayerAddr: AsRef<[u8]>;

    /// Gets the device's link-layer address bytes, if the device supports
    /// link-layer addressing.
    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<Self::LinkLayerAddr>;

    /// Sets the discovered retransmit timer for the device.
    fn set_discovered_retrans_timer(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        retrans_timer: NonZeroDuration,
    );

    /// Handles a received neighbor advertisement.
    ///
    /// Takes action in response to a received neighbor advertisement for the
    /// specified address. Returns the assignment state of the address on the
    /// given interface, if there was one before any action was taken. That is,
    /// this method returns `Some(Tentative {..})` when the address was
    /// tentatively assigned (and now removed), `Some(Assigned)` if the address
    /// was assigned (and so not removed), otherwise `None`.
    fn remove_duplicate_tentative_address(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
    ) -> Option<Ipv6DadState>;

    /// Sets the link MTU for the device.
    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu);

    /// Updates a discovered IPv6 route.
    fn update_discovered_ipv6_route(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
        lifetime: Option<NonZeroNdpLifetime>,
    );

    /// Applies a SLAAC update.
    fn apply_slaac_update(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        prefix: Subnet<Ipv6Addr>,
        preferred_lifetime: Option<NonZeroNdpLifetime>,
        valid_lifetime: Option<NonZeroNdpLifetime>,
    );

    /// Receives an MLD packet for processing.
    fn receive_mld_packet<B: ByteSlice>(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    );
}

impl<
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: Ipv6DeviceContext<C> + Ipv6DeviceConfigurationContext<C>,
    > Ipv6DeviceHandler<C> for SC
{
    type LinkLayerAddr = SC::LinkLayerAddr;

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<SC::LinkLayerAddr> {
        Ipv6DeviceContext::get_link_layer_addr_bytes(self, device_id)
    }

    fn set_discovered_retrans_timer(
        &mut self,
        _ctx: &mut C,
        device_id: &Self::DeviceId,
        retrans_timer: NonZeroDuration,
    ) {
        self.with_retrans_timer_mut(device_id, |state| *state = retrans_timer)
    }

    fn remove_duplicate_tentative_address(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
    ) -> Option<Ipv6DadState> {
        let address_state = self.with_ip_device_addresses(device_id, |addrs| {
            addrs.iter().find_map(
                |Ipv6AddressEntry { addr_sub, state: address_state, config: _, deprecated: _ }| {
                    (addr_sub.addr() == addr).then(|| *address_state)
                },
            )
        })?;

        match address_state {
            Ipv6DadState::Uninitialized
            | Ipv6DadState::Tentative { dad_transmits_remaining: _ } => {
                del_ipv6_addr_with_reason(
                    self,
                    ctx,
                    device_id,
                    &addr.into_specified(),
                    DelIpv6AddrReason::DadFailed,
                )
                .unwrap();
            }
            Ipv6DadState::Assigned => (),
        }
        Some(address_state)
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        Ipv6DeviceContext::set_link_mtu(self, device_id, mtu)
    }

    fn update_discovered_ipv6_route(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
        lifetime: Option<NonZeroNdpLifetime>,
    ) {
        self.with_ipv6_device_configuration(device_id, |_config, mut sync_ctx| {
            RouteDiscoveryHandler::update_route(&mut sync_ctx, ctx, device_id, route, lifetime)
        })
    }

    fn apply_slaac_update(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        prefix: Subnet<Ipv6Addr>,
        preferred_lifetime: Option<NonZeroNdpLifetime>,
        valid_lifetime: Option<NonZeroNdpLifetime>,
    ) {
        self.with_ipv6_device_configuration(device_id, |_config, mut sync_ctx| {
            SlaacHandler::apply_slaac_update(
                &mut sync_ctx,
                ctx,
                device_id,
                prefix,
                preferred_lifetime,
                valid_lifetime,
            )
        })
    }

    fn receive_mld_packet<B: ByteSlice>(
        &mut self,
        ctx: &mut C,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    ) {
        self.with_ipv6_device_configuration(device, |_config, mut sync_ctx| {
            MldPacketHandler::receive_mld_packet(&mut sync_ctx, ctx, device, src_ip, dst_ip, packet)
        })
    }
}

/// The execution context for an IP device with a buffer.
pub(crate) trait BufferIpDeviceContext<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, Self::DeviceId>,
    B: BufferMut,
>: DeviceIdContext<AnyDevice>
{
    /// Sends an IP packet through the device.
    fn send_ip_frame<S: Serializer<Buffer = B>>(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        local_addr: SpecifiedAddr<I::Addr>,
        body: S,
    ) -> Result<(), S>;
}

fn enable_ipv6_device_with_config<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceContext<C> + GmpHandler<Ipv6, C> + RsHandler<C> + DadHandler<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    config: &Ipv6DeviceConfiguration,
) {
    // All nodes should join the all-nodes multicast group.
    join_ip_multicast_with_config(
        sync_ctx,
        ctx,
        device_id,
        Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS,
        config,
    );
    GmpHandler::gmp_handle_maybe_enabled(sync_ctx, ctx, device_id);

    // Perform DAD for all addresses when enabling a device.
    //
    // We have to do this for all addresses (including ones that had DAD
    // performed) as while the device was disabled, another node could have
    // assigned the address and we wouldn't have responded to its DAD
    // solicitations.
    sync_ctx
        .with_ip_device_addresses(device_id, |addrs| {
            addrs
                .iter()
                .map(|Ipv6AddressEntry { addr_sub, state: _, config: _, deprecated: _ }| {
                    addr_sub.ipv6_unicast_addr()
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .for_each(|addr| {
            ctx.on_event(IpDeviceEvent::AddressStateChanged {
                device: device_id.clone(),
                addr: addr.into_specified(),
                state: IpAddressState::Tentative,
            });
            DadHandler::start_duplicate_address_detection(sync_ctx, ctx, device_id, addr);
        });

    // TODO(https://fxbug.dev/95946): Generate link-local address with opaque
    // IIDs.
    if let Some(iid) = sync_ctx.get_eui64_iid(device_id) {
        let link_local_addr_sub = {
            let mut addr = [0; 16];
            addr[0..2].copy_from_slice(&[0xfe, 0x80]);
            addr[(Ipv6::UNICAST_INTERFACE_IDENTIFIER_BITS / 8) as usize..].copy_from_slice(&iid);

            AddrSubnet::new(
                Ipv6Addr::from(addr),
                Ipv6Addr::BYTES * 8 - Ipv6::UNICAST_INTERFACE_IDENTIFIER_BITS,
            )
            .expect("valid link-local address")
        };

        match add_ipv6_addr_subnet_with_config(
            sync_ctx,
            ctx,
            device_id,
            link_local_addr_sub,
            AddrConfig::SLAAC_LINK_LOCAL,
            config,
        ) {
            Ok(()) => {}
            Err(ExistsError) => {
                // The address may have been added by admin action so it is safe
                // to swallow the exists error.
            }
        }
    }

    // As per RFC 4861 section 6.3.7,
    //
    //    A host sends Router Solicitations to the all-routers multicast
    //    address.
    //
    // If we are operating as a router, we do not solicit routers.
    if !config.ip_config.forwarding_enabled {
        RsHandler::start_router_solicitation(sync_ctx, ctx, device_id);
    }
}

fn disable_ipv6_device_with_config<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceContext<C>
        + GmpHandler<Ipv6, C>
        + RsHandler<C>
        + DadHandler<C>
        + RouteDiscoveryHandler<C>
        + SlaacHandler<C>
        + NudIpHandler<Ipv6, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    device_config: &Ipv6DeviceConfiguration,
) {
    NudIpHandler::flush_neighbor_table(sync_ctx, ctx, device_id);

    SlaacHandler::remove_all_slaac_addresses(sync_ctx, ctx, device_id);

    RouteDiscoveryHandler::invalidate_routes(sync_ctx, ctx, device_id);

    RsHandler::stop_router_solicitation(sync_ctx, ctx, device_id);

    // Delete the link-local address generated when enabling the device and stop
    // DAD on the other addresses.
    sync_ctx
        .with_ip_device_addresses(device_id, |addrs| {
            addrs
                .iter()
                .map(|Ipv6AddressEntry { addr_sub, state: _, config, deprecated: _ }| {
                    (addr_sub.ipv6_unicast_addr(), *config)
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .for_each(|(addr, config)| {
            if config == AddrConfig::SLAAC_LINK_LOCAL {
                del_ipv6_addr_with_reason_with_config(
                    sync_ctx,
                    ctx,
                    device_id,
                    &addr.into_specified(),
                    DelIpv6AddrReason::ManualAction,
                    device_config,
                )
                .expect("delete listed address")
            } else {
                DadHandler::stop_duplicate_address_detection(sync_ctx, ctx, device_id, addr);
                ctx.on_event(IpDeviceEvent::AddressStateChanged {
                    device: device_id.clone(),
                    addr: addr.into_specified(),
                    state: IpAddressState::Unavailable,
                });
            }
        });

    GmpHandler::gmp_handle_disabled(sync_ctx, ctx, device_id);
    leave_ip_multicast_with_config(
        sync_ctx,
        ctx,
        device_id,
        Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS,
        device_config,
    );
}

fn enable_ipv4_device_with_config<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceStateContext<Ipv4, C> + GmpHandler<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    config: &Ipv4DeviceConfiguration,
) {
    // All systems should join the all-systems multicast group.
    join_ip_multicast_with_config(
        sync_ctx,
        ctx,
        device_id,
        Ipv4::ALL_SYSTEMS_MULTICAST_ADDRESS,
        config,
    );
    GmpHandler::gmp_handle_maybe_enabled(sync_ctx, ctx, device_id);
    sync_ctx.with_ip_device_addresses(device_id, |addrs| {
        addrs.iter().for_each(|addr| {
            ctx.on_event(IpDeviceEvent::AddressStateChanged {
                device: device_id.clone(),
                addr: addr.addr(),
                state: IpAddressState::Assigned,
            });
        })
    });
}

fn disable_ipv4_device_with_config<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceStateContext<Ipv4, C> + GmpHandler<Ipv4, C> + NudIpHandler<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    config: &Ipv4DeviceConfiguration,
) {
    NudIpHandler::flush_neighbor_table(sync_ctx, ctx, device_id);
    GmpHandler::gmp_handle_disabled(sync_ctx, ctx, device_id);
    leave_ip_multicast_with_config(
        sync_ctx,
        ctx,
        device_id,
        Ipv4::ALL_SYSTEMS_MULTICAST_ADDRESS,
        config,
    );
    sync_ctx.with_ip_device_addresses(device_id, |addrs| {
        addrs.iter().for_each(|addr| {
            ctx.on_event(IpDeviceEvent::AddressStateChanged {
                device: device_id.clone(),
                addr: addr.addr(),
                state: IpAddressState::Unavailable,
            });
        })
    });
}

pub(crate) fn with_assigned_addr_subnets<
    I: Ip + IpDeviceIpExt,
    C: InstantContext,
    SC: IpDeviceStateContext<I, C>,
    O,
    F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<I::Addr>> + '_>) -> O,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
    cb: F,
) -> O {
    sync_ctx.with_ip_device_addresses(device_id, |addrs| {
        cb(Box::new(addrs.iter().filter_map(I::assigned_addr)))
    })
}

/// Gets the IPv4 address and subnet pairs associated with this device.
///
/// Returns an [`Iterator`] of `AddrSubnet`.
pub(crate) fn with_assigned_ipv4_addr_subnets<
    C: InstantContext,
    SC: IpDeviceStateContext<Ipv4, C>,
    O,
    F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<Ipv4Addr>> + '_>) -> O,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
    cb: F,
) -> O {
    with_assigned_addr_subnets::<Ipv4, _, _, _, _>(sync_ctx, device_id, cb)
}

/// Gets a single IPv4 address and subnet for a device.
pub(crate) fn get_ipv4_addr_subnet<C: InstantContext, SC: IpDeviceStateContext<Ipv4, C>>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
) -> Option<AddrSubnet<Ipv4Addr>> {
    with_assigned_ipv4_addr_subnets(sync_ctx, device_id, |mut addrs| addrs.nth(0))
}

/// Gets the hop limit for new IPv6 packets that will be sent out from `device`.
pub(crate) fn get_ipv6_hop_limit<C: InstantContext, SC: IpDeviceStateContext<Ipv6, C>>(
    sync_ctx: &mut SC,
    device: &SC::DeviceId,
) -> NonZeroU8 {
    sync_ctx.with_default_hop_limit(device, Clone::clone)
}

/// Is IP packet forwarding enabled?
pub(crate) fn is_ip_forwarding_enabled<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<I, C>,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
) -> bool {
    sync_ctx.with_ip_device_configuration(device_id, |state, _ctx| {
        AsRef::<IpDeviceConfiguration>::as_ref(state).forwarding_enabled
    })
}

fn join_ip_multicast_with_config<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: IpDeviceStateContext<I, C> + GmpHandler<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
    // Not used but required to make sure that the caller is currently holding a
    // a reference to the IP device's IP configuration as a way to prove that
    // caller has synchronized this operation with other accesses to the IP
    // device configuration.
    _config: &I::Configuration,
) {
    match sync_ctx.gmp_join_group(ctx, device_id, multicast_addr) {
        GroupJoinResult::Joined(()) => {
            sync_ctx.join_link_multicast_group(ctx, device_id, multicast_addr)
        }
        GroupJoinResult::AlreadyMember => {}
    }
}

/// Adds `device_id` to a multicast group `multicast_addr`.
///
/// Calling `join_ip_multicast` multiple times is completely safe. A counter
/// will be kept for the number of times `join_ip_multicast` has been called
/// with the same `device_id` and `multicast_addr` pair. To completely leave a
/// multicast group, [`leave_ip_multicast`] must be called the same number of
/// times `join_ip_multicast` has been called for the same `device_id` and
/// `multicast_addr` pair. The first time `join_ip_multicast` is called for a
/// new `device` and `multicast_addr` pair, the device will actually join the
/// multicast group.
pub(crate) fn join_ip_multicast<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
) {
    sync_ctx.with_ip_device_configuration(device_id, |config, mut sync_ctx| {
        join_ip_multicast_with_config(&mut sync_ctx, ctx, device_id, multicast_addr, config)
    })
}

fn leave_ip_multicast_with_config<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: IpDeviceStateContext<I, C> + GmpHandler<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
    // Not used but required to make sure that the caller is currently holding a
    // a reference to the IP device's IP configuration as a way to prove that
    // caller has synchronized this operation with other accesses to the IP
    // device configuration.
    _config: &I::Configuration,
) {
    match sync_ctx.gmp_leave_group(ctx, device_id, multicast_addr) {
        GroupLeaveResult::Left(()) => {
            sync_ctx.leave_link_multicast_group(ctx, device_id, multicast_addr)
        }
        GroupLeaveResult::StillMember => {}
        GroupLeaveResult::NotMember => panic!(
            "attempted to leave IP multicast group we were not a member of: {}",
            multicast_addr,
        ),
    }
}

/// Removes `device_id` from a multicast group `multicast_addr`.
///
/// `leave_ip_multicast` will attempt to remove `device_id` from a multicast
/// group `multicast_addr`. `device_id` may have "joined" the same multicast
/// address multiple times, so `device_id` will only leave the multicast group
/// once `leave_ip_multicast` has been called for each corresponding
/// [`join_ip_multicast`]. That is, if `join_ip_multicast` gets called 3
/// times and `leave_ip_multicast` gets called two times (after all 3
/// `join_ip_multicast` calls), `device_id` will still be in the multicast
/// group until the next (final) call to `leave_ip_multicast`.
///
/// # Panics
///
/// If `device_id` is not currently in the multicast group `multicast_addr`.
pub(crate) fn leave_ip_multicast<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<I, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
) {
    sync_ctx.with_ip_device_configuration(device_id, |config, mut sync_ctx| {
        leave_ip_multicast_with_config(&mut sync_ctx, ctx, device_id, multicast_addr, config)
    })
}

/// Adds an IPv4 address and associated subnet to this device.
pub(crate) fn add_ipv4_addr_subnet<
    SC: IpDeviceConfigurationContext<Ipv4, C>,
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr_sub: AddrSubnet<Ipv4Addr>,
) -> Result<(), ExistsError> {
    sync_ctx.with_ip_device_configuration(device_id, |config, mut sync_ctx| {
        let address_state = match config.ip_config.ip_enabled {
            true => IpAddressState::Assigned,
            false => IpAddressState::Unavailable,
        };

        sync_ctx.with_ip_device_addresses_mut(device_id, |addrs| {
            addrs.add(addr_sub).map(|()| {
                ctx.on_event(IpDeviceEvent::AddressAdded {
                    device: device_id.clone(),
                    addr: addr_sub,
                    state: address_state,
                })
            })
        })
    })
}

/// Adds an IPv6 address (with duplicate address detection) and associated
/// subnet to this device and joins the address's solicited-node multicast
/// group.
///
/// `config` is the way this address is being configured. See [`AddrConfig`]
/// for more details.
pub(crate) fn add_ipv6_addr_subnet<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceConfigurationContext<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr_sub: AddrSubnet<Ipv6Addr>,
    addr_config: AddrConfig<C::Instant>,
) -> Result<(), ExistsError> {
    sync_ctx.with_ipv6_device_configuration(device_id, |config, mut sync_ctx| {
        add_ipv6_addr_subnet_with_config(
            &mut sync_ctx,
            ctx,
            device_id,
            addr_sub,
            addr_config,
            config,
        )
    })
}

fn add_ipv6_addr_subnet_with_config<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: IpDeviceStateContext<Ipv6, C> + GmpHandler<Ipv6, C> + DadHandler<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr_sub: AddrSubnet<Ipv6Addr>,
    addr_config: AddrConfig<C::Instant>,
    device_config: &Ipv6DeviceConfiguration,
) -> Result<(), ExistsError> {
    let addr_sub = addr_sub.to_unicast();

    sync_ctx.with_ip_device_addresses_mut(device_id, |addrs| {
        addrs.add(Ipv6AddressEntry::new(addr_sub, Ipv6DadState::Uninitialized, addr_config))
    })?;

    // As per RFC 4861 section 5.6.2,
    //
    //   Before sending a Neighbor Solicitation, an interface MUST join
    //   the all-nodes multicast address and the solicited-node
    //   multicast address of the tentative address.
    //
    // Note that we join the all-nodes multicast address on interface
    // enable.
    join_ip_multicast_with_config(
        sync_ctx,
        ctx,
        device_id,
        addr_sub.addr().to_solicited_node_address(),
        device_config,
    );

    let Ipv6DeviceConfiguration {
        dad_transmits: _,
        max_router_solicitations: _,
        slaac_config: _,
        ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled: _, forwarding_enabled: _ },
    } = device_config;

    let state = match ip_enabled {
        true => IpAddressState::Tentative,
        false => IpAddressState::Unavailable,
    };

    ctx.on_event(IpDeviceEvent::AddressAdded {
        device: device_id.clone(),
        addr: addr_sub.to_witness(),
        state,
    });

    if *ip_enabled {
        // NB: We don't start DAD if the device is disabled. DAD will be
        // performed when the device is enabled for all addressed.
        DadHandler::start_duplicate_address_detection(sync_ctx, ctx, device_id, addr_sub.addr())
    }

    Ok(())
}

/// Removes an IPv4 address and associated subnet from this device.
pub(crate) fn del_ipv4_addr<
    SC: IpDeviceConfigurationContext<Ipv4, C>,
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr: &SpecifiedAddr<Ipv4Addr>,
) -> Result<(), NotFoundError> {
    sync_ctx.with_ip_device_addresses_mut(device_id, |addrs| {
        addrs.remove(&addr).map(|addr| {
            ctx.on_event(IpDeviceEvent::AddressRemoved {
                device: device_id.clone(),
                addr: addr.addr(),
                reason: RemovedReason::Manual,
            })
        })
    })
}

fn del_ipv6_addr_with_config<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceContext<C> + GmpHandler<Ipv6, C> + DadHandler<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr: &SpecifiedAddr<Ipv6Addr>,
    reason: RemovedReason,
    config: &Ipv6DeviceConfiguration,
) -> Result<Ipv6AddressEntry<C::Instant>, NotFoundError> {
    let entry = sync_ctx.with_ip_device_addresses_mut(device_id, |addrs| addrs.remove(&addr))?;
    let addr = entry.addr_sub.addr();

    DadHandler::stop_duplicate_address_detection(sync_ctx, ctx, device_id, addr);

    leave_ip_multicast_with_config(
        sync_ctx,
        ctx,
        device_id,
        addr.to_solicited_node_address(),
        config,
    );

    ctx.on_event(IpDeviceEvent::AddressRemoved {
        device: device_id.clone(),
        addr: addr.into_specified(),
        reason,
    });

    Ok(entry)
}

/// Removes an IPv6 address and associated subnet from this device.
pub(crate) fn del_ipv6_addr_with_reason<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceConfigurationContext<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr: &SpecifiedAddr<Ipv6Addr>,
    reason: DelIpv6AddrReason,
) -> Result<(), NotFoundError> {
    sync_ctx.with_ipv6_device_configuration(device_id, |config, mut sync_ctx| {
        del_ipv6_addr_with_reason_with_config(&mut sync_ctx, ctx, device_id, addr, reason, config)
    })
}

/// Removes an IPv6 address and associated subnet from this device.
fn del_ipv6_addr_with_reason_with_config<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceContext<C> + GmpHandler<Ipv6, C> + DadHandler<C> + SlaacHandler<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    addr: &SpecifiedAddr<Ipv6Addr>,
    reason: DelIpv6AddrReason,
    config: &Ipv6DeviceConfiguration,
) -> Result<(), NotFoundError> {
    del_ipv6_addr_with_config(sync_ctx, ctx, device_id, addr, reason.into(), config).map(
        |Ipv6AddressEntry { addr_sub, state: _, config, deprecated: _ }| match config {
            AddrConfig::Slaac(s) => {
                SlaacHandler::on_address_removed(sync_ctx, ctx, device_id, addr_sub, s, reason)
            }
            AddrConfig::Manual => (),
        },
    )
}

/// Sends an IP packet through the device.
pub(crate) fn send_ip_frame<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: BufferIpDeviceContext<I, C, B>,
    B: BufferMut,
    S: Serializer<Buffer = B>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    local_addr: SpecifiedAddr<I::Addr>,
    body: S,
) -> Result<(), S> {
    sync_ctx.send_ip_frame(ctx, device_id, local_addr, body)
}

pub(crate) fn get_ipv4_configuration<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
) -> Ipv4DeviceConfiguration {
    sync_ctx.with_ip_device_configuration(device_id, |config, _ctx| config.clone())
}

pub(crate) fn get_ipv6_configuration<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv6, C>,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
) -> Ipv6DeviceConfiguration {
    sync_ctx.with_ip_device_configuration(device_id, |config, _ctx| config.clone())
}

/// An update to IP device configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IpDeviceConfigurationUpdate {
    /// A change in IP enabled.
    pub ip_enabled: Option<bool>,
    /// A change in forwarding enabled.
    pub forwarding_enabled: Option<bool>,
    /// A change in Group Messaging Protocol (GMP) enabled.
    pub gmp_enabled: Option<bool>,
}

/// An update to IPv4 device configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ipv4DeviceConfigurationUpdate {
    /// A change in the IP device configuration.
    pub ip_config: Option<IpDeviceConfigurationUpdate>,
}

fn get_prev_and_update<T>(field: &mut T, new: Option<T>) -> Option<T> {
    new.map(|v| core::mem::replace(field, v))
}

/// Updates the IPv4 configuration for the device.
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
pub(crate) fn update_ipv4_configuration<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    Ipv4DeviceConfigurationUpdate { ip_config }: Ipv4DeviceConfigurationUpdate,
) -> Result<Ipv4DeviceConfigurationUpdate, NotSupportedError> {
    update_ipv4_configuration_inner(sync_ctx, ctx, device_id, |config| {
        Ipv4DeviceConfigurationUpdate {
            ip_config: ip_config.map(
                |IpDeviceConfigurationUpdate { ip_enabled, forwarding_enabled, gmp_enabled }| {
                    IpDeviceConfigurationUpdate {
                        ip_enabled: get_prev_and_update(
                            &mut config.ip_config.ip_enabled,
                            ip_enabled,
                        ),
                        gmp_enabled: get_prev_and_update(
                            &mut config.ip_config.gmp_enabled,
                            gmp_enabled,
                        ),
                        forwarding_enabled: get_prev_and_update(
                            &mut config.ip_config.forwarding_enabled,
                            forwarding_enabled,
                        ),
                    }
                },
            ),
        }
    })
}

/// An update to IPv6 device configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ipv6DeviceConfigurationUpdate {
    /// A change in DAD transmits.
    pub dad_transmits: Option<Option<NonZeroU8>>,
    /// A change in maximum router solicitations.
    pub max_router_solicitations: Option<Option<NonZeroU8>>,
    /// A change in SLAAC configuration.
    pub slaac_config: Option<SlaacConfiguration>,
    /// A change in the IP device configuration.
    pub ip_config: Option<IpDeviceConfigurationUpdate>,
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
pub(crate) fn update_ipv6_configuration<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceConfigurationContext<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    Ipv6DeviceConfigurationUpdate {
        dad_transmits,
        max_router_solicitations,
        slaac_config,
        ip_config,
    }: Ipv6DeviceConfigurationUpdate,
) -> Result<Ipv6DeviceConfigurationUpdate, NotSupportedError> {
    update_ipv6_configuration_inner(sync_ctx, ctx, device_id, |config| {
        Ipv6DeviceConfigurationUpdate {
            dad_transmits: get_prev_and_update(&mut config.dad_transmits, dad_transmits),
            max_router_solicitations: get_prev_and_update(
                &mut config.max_router_solicitations,
                max_router_solicitations,
            ),
            slaac_config: get_prev_and_update(&mut config.slaac_config, slaac_config),
            ip_config: ip_config.map(
                |IpDeviceConfigurationUpdate { ip_enabled, forwarding_enabled, gmp_enabled }| {
                    IpDeviceConfigurationUpdate {
                        ip_enabled: get_prev_and_update(
                            &mut config.ip_config.ip_enabled,
                            ip_enabled,
                        ),
                        gmp_enabled: get_prev_and_update(
                            &mut config.ip_config.gmp_enabled,
                            gmp_enabled,
                        ),
                        forwarding_enabled: get_prev_and_update(
                            &mut config.ip_config.forwarding_enabled,
                            forwarding_enabled,
                        ),
                    }
                },
            ),
        }
    })
}

/// Updates the IPv4 Configuration for the device.
///
/// The device's configuration will be left unchanged when `Err(_)` is returned.
fn update_ipv4_configuration_inner<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv4, C>,
    O,
    F: FnOnce(&mut Ipv4DeviceConfiguration) -> O,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    update_cb: F,
) -> Result<O, NotSupportedError> {
    sync_ctx.with_ip_device_configuration_mut(device_id, |mut inner| {
        let (ret, prev_config) = {
            let config = inner.configuration_mut();
            let prev_config = *config;

            let ret = update_cb(config);

            if device_id.is_loopback() {
                let Ipv4DeviceConfiguration {
                    ip_config:
                        IpDeviceConfiguration { ip_enabled: _, gmp_enabled: _, forwarding_enabled },
                } = config;

                if *forwarding_enabled {
                    *config = prev_config;
                    return Err(NotSupportedError);
                }
            }

            (ret, prev_config)
        };

        let Ipv4DeviceConfiguration {
            ip_config:
                IpDeviceConfiguration {
                    ip_enabled: prev_ip_enabled,
                    gmp_enabled: prev_gmp_enabled,
                    forwarding_enabled: _,
                },
        } = prev_config;
        let (config, mut sync_ctx) = inner.ip_device_configuration_and_ctx();
        let Ipv4DeviceConfiguration {
            ip_config:
                IpDeviceConfiguration {
                    ip_enabled: next_ip_enabled,
                    gmp_enabled: next_gmp_enabled,
                    forwarding_enabled: _,
                },
        } = *config;
        let sync_ctx = &mut sync_ctx;

        if prev_ip_enabled != next_ip_enabled {
            if next_ip_enabled {
                enable_ipv4_device_with_config(sync_ctx, ctx, device_id, config);
            } else {
                disable_ipv4_device_with_config(sync_ctx, ctx, device_id, config);
            }

            ctx.on_event(IpDeviceEvent::EnabledChanged {
                device: device_id.clone(),
                ip_enabled: next_ip_enabled,
            });
        }

        if prev_gmp_enabled != next_gmp_enabled {
            if next_gmp_enabled {
                GmpHandler::gmp_handle_maybe_enabled(sync_ctx, ctx, device_id);
            } else {
                GmpHandler::gmp_handle_disabled(sync_ctx, ctx, device_id);
            }
        }

        Ok(ret)
    })
}

pub(super) fn is_ip_device_enabled<
    I: IpDeviceIpExt,
    C: IpDeviceNonSyncContext<I, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<I, C>,
>(
    sync_ctx: &mut SC,
    device_id: &SC::DeviceId,
) -> bool {
    sync_ctx.with_ip_device_configuration(device_id, |state, _ctx| {
        AsRef::<IpDeviceConfiguration>::as_ref(state).ip_enabled
    })
}

/// Updates the IPv6 Configuration for the device.
///
/// The device's configuration will be left unchanged when `Err(_)` is returned.
fn update_ipv6_configuration_inner<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceConfigurationContext<C>,
    O,
    F: FnOnce(&mut Ipv6DeviceConfiguration) -> O,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
    update_cb: F,
) -> Result<O, NotSupportedError> {
    sync_ctx.with_ipv6_device_configuration_mut(device_id, |mut inner| {
        let (ret, prev_config) = {
            let config = inner.configuration_mut();
            let prev_config = *config;
            let ret = update_cb(config);

            if device_id.is_loopback() {
                let Ipv6DeviceConfiguration {
                    dad_transmits: _,
                    max_router_solicitations: _,
                    slaac_config: _,
                    ip_config:
                        IpDeviceConfiguration { ip_enabled: _, gmp_enabled: _, forwarding_enabled },
                } = config;

                if *forwarding_enabled {
                    *config = prev_config;
                    return Err(NotSupportedError);
                }
            }

            (ret, prev_config)
        };

        let Ipv6DeviceConfiguration {
            dad_transmits: _,
            max_router_solicitations: _,
            slaac_config: _,
            ip_config:
                IpDeviceConfiguration {
                    ip_enabled: prev_ip_enabled,
                    gmp_enabled: prev_gmp_enabled,
                    forwarding_enabled: prev_forwarding_enabled,
                },
        } = prev_config;
        let (config, mut sync_ctx) = inner.ipv6_device_configuration_and_ctx();
        let Ipv6DeviceConfiguration {
            dad_transmits: _,
            max_router_solicitations: _,
            slaac_config: _,
            ip_config:
                IpDeviceConfiguration {
                    ip_enabled: next_ip_enabled,
                    gmp_enabled: next_gmp_enabled,
                    forwarding_enabled: next_forwarding_enabled,
                },
        } = *config;
        let sync_ctx = &mut sync_ctx;

        if prev_ip_enabled != next_ip_enabled {
            if next_ip_enabled {
                enable_ipv6_device_with_config(sync_ctx, ctx, device_id, config);
            } else {
                disable_ipv6_device_with_config(sync_ctx, ctx, device_id, config);
            }

            ctx.on_event(IpDeviceEvent::EnabledChanged {
                device: device_id.clone(),
                ip_enabled: next_ip_enabled,
            });
        }

        if prev_gmp_enabled != next_gmp_enabled {
            if next_gmp_enabled {
                GmpHandler::gmp_handle_maybe_enabled(sync_ctx, ctx, device_id);
            } else {
                GmpHandler::gmp_handle_disabled(sync_ctx, ctx, device_id);
            }
        }

        if prev_forwarding_enabled != next_forwarding_enabled {
            if next_forwarding_enabled {
                RsHandler::stop_router_solicitation(sync_ctx, ctx, device_id);
                join_ip_multicast_with_config(
                    sync_ctx,
                    ctx,
                    device_id,
                    Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                    config,
                );
            } else {
                leave_ip_multicast_with_config(
                    sync_ctx,
                    ctx,
                    device_id,
                    Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                    config,
                );
                RsHandler::start_router_solicitation(sync_ctx, ctx, device_id);
            }
        }

        Ok(ret)
    })
}

/// Removes IPv4 state for the device without emitting events.
pub(crate) fn clear_ipv4_device_state<
    C: IpDeviceNonSyncContext<Ipv4, SC::DeviceId>,
    SC: IpDeviceConfigurationContext<Ipv4, C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
) {
    sync_ctx.with_ip_device_configuration(device_id, |config, mut sync_ctx| {
        let Ipv4DeviceConfiguration {
            ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled, forwarding_enabled: _ },
        } = config;

        let sync_ctx = &mut sync_ctx;

        if *ip_enabled {
            disable_ipv4_device_with_config(sync_ctx, ctx, device_id, config);
        }
        if *gmp_enabled {
            GmpHandler::gmp_handle_disabled(sync_ctx, ctx, device_id);
        }
    })
}

/// Removes IPv6 state for the device without emitting events.
pub(crate) fn clear_ipv6_device_state<
    C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
    SC: Ipv6DeviceConfigurationContext<C>,
>(
    sync_ctx: &mut SC,
    ctx: &mut C,
    device_id: &SC::DeviceId,
) {
    sync_ctx.with_ipv6_device_configuration(device_id, |config, mut sync_ctx| {
        let Ipv6DeviceConfiguration {
            dad_transmits: _,
            max_router_solicitations: _,
            slaac_config: _,
            ip_config: IpDeviceConfiguration { ip_enabled, gmp_enabled, forwarding_enabled: _ },
        } = config;

        let sync_ctx = &mut sync_ctx;

        if *ip_enabled {
            disable_ipv6_device_with_config(sync_ctx, ctx, device_id, config);
        }

        if *gmp_enabled {
            GmpHandler::gmp_handle_disabled(sync_ctx, ctx, device_id);
        }
    })
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use core::fmt::Debug;
    use lock_order::Locked;
    use net_types::{ip::Ipv6Scope, ScopeableAddress as _};

    use crate::{
        device::{update_ipv4_configuration, update_ipv6_configuration, DeviceId},
        testutil::{FakeNonSyncCtx, FakeSyncCtx},
        NonSyncContext, SyncCtx,
    };

    pub(crate) fn get_global_ipv6_addrs<C: NonSyncContext>(
        sync_ctx: &SyncCtx<C>,
        device_id: &DeviceId<C>,
    ) -> Vec<Ipv6AddressEntry<C::Instant>> {
        IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
            &mut Locked::new(sync_ctx),
            device_id,
            |addrs| {
                addrs
                    .iter()
                    .filter(|entry| match entry.addr_sub.addr().scope() {
                        Ipv6Scope::Global => true,
                        Ipv6Scope::InterfaceLocal
                        | Ipv6Scope::LinkLocal
                        | Ipv6Scope::AdminLocal
                        | Ipv6Scope::SiteLocal
                        | Ipv6Scope::OrganizationLocal
                        | Ipv6Scope::Reserved(_)
                        | Ipv6Scope::Unassigned(_) => false,
                    })
                    .cloned()
                    .collect()
            },
        )
    }

    /// Gets the IPv6 address and subnet pairs associated with this device which are
    /// in the assigned state.
    ///
    /// Tentative IP addresses (addresses which are not yet fully bound to a device)
    /// and deprecated IP addresses (addresses which have been assigned but should
    /// no longer be used for new connections) will not be returned by
    /// `get_assigned_ipv6_addr_subnets`.
    ///
    /// Returns an [`Iterator`] of `AddrSubnet`.
    ///
    /// See [`Tentative`] and [`AddrSubnet`] for more information.
    pub(crate) fn with_assigned_ipv6_addr_subnets<
        C: IpDeviceNonSyncContext<Ipv6, SC::DeviceId>,
        SC: IpDeviceConfigurationContext<Ipv6, C>,
        O,
        F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<Ipv6Addr>> + '_>) -> O,
    >(
        sync_ctx: &mut SC,
        device_id: &SC::DeviceId,
        cb: F,
    ) -> O {
        with_assigned_addr_subnets::<Ipv6, _, _, _, _>(sync_ctx, device_id, cb)
    }

    pub(crate) trait UpdateIpDeviceConfigurationTestIpExt: Ip {
        type IpDeviceConfiguration: AsRef<IpDeviceConfiguration>
            + AsMut<IpDeviceConfiguration>
            + PartialEq
            + Debug;

        fn get_ip_configuration(
            sync_ctx: &FakeSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
        ) -> Self::IpDeviceConfiguration;

        fn update_ip_configuration(
            sync_ctx: &FakeSyncCtx,
            ctx: &mut FakeNonSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
            config: IpDeviceConfigurationUpdate,
        ) -> Result<IpDeviceConfigurationUpdate, NotSupportedError>;

        fn set_ip_device_enabled(
            sync_ctx: &FakeSyncCtx,
            ctx: &mut FakeNonSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
            enabled: bool,
            prev_enabled: bool,
        ) {
            let make_update = |enabled| IpDeviceConfigurationUpdate {
                ip_enabled: Some(enabled),
                ..Default::default()
            };

            assert_eq!(
                Self::update_ip_configuration(sync_ctx, ctx, device, make_update(enabled)),
                Ok(make_update(prev_enabled)),
            );
        }
    }

    impl UpdateIpDeviceConfigurationTestIpExt for Ipv4 {
        type IpDeviceConfiguration = Ipv4DeviceConfiguration;

        fn get_ip_configuration(
            sync_ctx: &FakeSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
        ) -> Self::IpDeviceConfiguration {
            crate::device::get_ipv4_configuration(sync_ctx, device)
        }

        fn update_ip_configuration(
            sync_ctx: &FakeSyncCtx,
            ctx: &mut FakeNonSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
            config: IpDeviceConfigurationUpdate,
        ) -> Result<IpDeviceConfigurationUpdate, NotSupportedError> {
            update_ipv4_configuration(
                sync_ctx,
                ctx,
                device,
                Ipv4DeviceConfigurationUpdate { ip_config: Some(config), ..Default::default() },
            )
            .map(|Ipv4DeviceConfigurationUpdate { ip_config }| ip_config.unwrap())
        }
    }

    impl UpdateIpDeviceConfigurationTestIpExt for Ipv6 {
        type IpDeviceConfiguration = Ipv6DeviceConfiguration;

        fn get_ip_configuration(
            sync_ctx: &FakeSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
        ) -> Self::IpDeviceConfiguration {
            crate::device::get_ipv6_configuration(sync_ctx, device)
        }

        fn update_ip_configuration(
            sync_ctx: &FakeSyncCtx,
            ctx: &mut FakeNonSyncCtx,
            device: &DeviceId<FakeNonSyncCtx>,
            config: IpDeviceConfigurationUpdate,
        ) -> Result<IpDeviceConfigurationUpdate, NotSupportedError> {
            update_ipv6_configuration(
                sync_ctx,
                ctx,
                device,
                Ipv6DeviceConfigurationUpdate { ip_config: Some(config), ..Default::default() },
            )
            .map(
                |Ipv6DeviceConfigurationUpdate {
                     dad_transmits: _,
                     max_router_solicitations: _,
                     slaac_config: _,
                     ip_config,
                 }| ip_config.unwrap(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec;
    use fakealloc::collections::HashSet;
    use ip_test_macro::ip_test;

    use lock_order::Locked;
    use net_declare::net_ip_v6;
    use net_types::{ip::Ipv6, LinkLocalAddr};

    use crate::{
        device::{ethernet, update_ipv4_configuration, update_ipv6_configuration, DeviceId},
        ip::{
            device::testutil::UpdateIpDeviceConfigurationTestIpExt, gmp::GmpDelayedReportTimerId,
        },
        testutil::{
            assert_empty, Ctx, DispatchedEvent, FakeCtx, FakeNonSyncCtx, FakeSyncCtx,
            TestIpExt as _, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        StackStateBuilder, TimerId, TimerIdInner,
    };

    #[test]
    fn enable_disable_ipv4() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        let sync_ctx = &sync_ctx;
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv4::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            sync_ctx,
            local_mac,
            ethernet::MaxFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        assert_eq!(non_sync_ctx.take_events()[..], []);

        let set_ipv4_enabled =
            |ctx: &mut crate::testutil::FakeNonSyncCtx, enabled, expected_prev| {
                Ipv4::set_ip_device_enabled(sync_ctx, ctx, &device_id, enabled, expected_prev)
            };

        set_ipv4_enabled(&mut non_sync_ctx, true, false);

        let weak_device_id = device_id.downgrade();
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            })]
        );

        set_ipv4_enabled(&mut non_sync_ctx, false, true);
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: false,
            })]
        );

        let ipv4_addr_subnet = AddrSubnet::new(Ipv4Addr::new([192, 168, 0, 1]), 24).unwrap();
        add_ipv4_addr_subnet(
            &mut Locked::new(sync_ctx),
            &mut non_sync_ctx,
            &device_id,
            ipv4_addr_subnet.clone(),
        )
        .expect("failed to add IPv4 Address");
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.clone(),
                state: IpAddressState::Unavailable,
            })]
        );

        set_ipv4_enabled(&mut non_sync_ctx, true, false);
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ipv4_addr_subnet.addr().into(),
                    state: IpAddressState::Assigned,
                }),
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                }),
            ]
        );
        // Verify that a redundant "enable" does not generate any events.
        set_ipv4_enabled(&mut non_sync_ctx, true, true);
        assert_eq!(non_sync_ctx.take_events()[..], []);

        set_ipv4_enabled(&mut non_sync_ctx, false, true);
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ipv4_addr_subnet.addr().into(),
                    state: IpAddressState::Unavailable,
                }),
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: false,
                }),
            ]
        );
        // Verify that a redundant "disable" does not generate any events.
        set_ipv4_enabled(&mut non_sync_ctx, false, false);
        assert_eq!(non_sync_ctx.take_events()[..], []);
    }

    fn enable_ipv6_device(
        sync_ctx: &mut &FakeSyncCtx,
        non_sync_ctx: &mut FakeNonSyncCtx,
        device_id: &DeviceId<FakeNonSyncCtx>,
        ll_addr: AddrSubnet<Ipv6Addr, LinkLocalAddr<UnicastAddr<Ipv6Addr>>>,
        expected_prev: bool,
    ) {
        Ipv6::set_ip_device_enabled(sync_ctx, non_sync_ctx, device_id, true, expected_prev);

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
                &mut Locked::new(*sync_ctx),
                device_id,
                |addrs| {
                    addrs
                        .iter()
                        .map(|Ipv6AddressEntry { addr_sub, state: _, config: _, deprecated: _ }| {
                            addr_sub.ipv6_unicast_addr()
                        })
                        .collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "enabled device expected to generate link-local address"
        );
    }

    #[test]
    fn enable_disable_ipv6() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        let mut sync_ctx = &sync_ctx;
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            sync_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        let ll_addr = local_mac.to_ipv6_link_local();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                // Doesn't matter as long as we perform DAD and router
                // solicitation.
                dad_transmits: Some(NonZeroU8::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(1)),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    gmp_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(non_sync_ctx.take_events()[..], []);

        // Enable the device and observe an auto-generated link-local address,
        // router solicitation and DAD for the auto-generated address.
        let test_enable_device = |sync_ctx: &mut &FakeSyncCtx,
                                  non_sync_ctx: &mut FakeNonSyncCtx,
                                  extra_group,
                                  expected_prev| {
            enable_ipv6_device(sync_ctx, non_sync_ctx, &device_id, ll_addr, expected_prev);
            let mut timers = vec![
                (
                    TimerId(TimerIdInner::Ipv6Device(Ipv6DeviceTimerId::Rs(RsTimerId {
                        device_id: device_id.clone(),
                    }))),
                    ..,
                ),
                (
                    TimerId(TimerIdInner::Ipv6Device(Ipv6DeviceTimerId::Dad(DadTimerId {
                        device_id: device_id.clone(),
                        addr: ll_addr.ipv6_unicast_addr(),
                    }))),
                    ..,
                ),
                (
                    TimerId(TimerIdInner::Ipv6Device(Ipv6DeviceTimerId::Mld(
                        MldDelayedReportTimerId(GmpDelayedReportTimerId {
                            device: device_id.clone(),
                            group_addr: ll_addr.addr().to_solicited_node_address(),
                        })
                        .into(),
                    ))),
                    ..,
                ),
            ];
            if let Some(group_addr) = extra_group {
                timers.push((
                    TimerId(TimerIdInner::Ipv6Device(Ipv6DeviceTimerId::Mld(
                        MldDelayedReportTimerId(GmpDelayedReportTimerId {
                            device: device_id.clone(),
                            group_addr,
                        })
                        .into(),
                    ))),
                    ..,
                ))
            }
            non_sync_ctx.timer_ctx().assert_timers_installed(timers);
        };
        test_enable_device(&mut sync_ctx, &mut non_sync_ctx, None, false);
        let weak_device_id = device_id.downgrade();
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                    device: weak_device_id.clone(),
                    addr: ll_addr.to_witness(),
                    state: IpAddressState::Tentative,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                })
            ]
        );

        let test_disable_device = |sync_ctx: &mut &FakeSyncCtx,
                                   non_sync_ctx: &mut FakeNonSyncCtx,
                                   expected_prev| {
            Ipv6::set_ip_device_enabled(sync_ctx, non_sync_ctx, &device_id, false, expected_prev);
            non_sync_ctx.timer_ctx().assert_no_timers_installed();
        };
        test_disable_device(&mut sync_ctx, &mut non_sync_ctx, true);
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
                    device: weak_device_id.clone(),
                    addr: ll_addr.addr().into(),
                    reason: RemovedReason::Manual,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: false,
                })
            ]
        );

        IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
            &mut Locked::new(sync_ctx),
            &device_id,
            |addrs| {
                assert_empty(addrs.iter());
            },
        );

        let multicast_addr = Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS;
        join_ip_multicast::<Ipv6, _, _>(
            &mut Locked::new(sync_ctx),
            &mut non_sync_ctx,
            &device_id,
            multicast_addr,
        );
        add_ipv6_addr_subnet(
            &mut Locked::new(sync_ctx),
            &mut non_sync_ctx,
            &device_id,
            ll_addr.to_witness(),
            AddrConfig::Manual,
        )
        .expect("add MAC based IPv6 link-local address");
        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
                &mut Locked::new(sync_ctx),
                &device_id,
                |addrs| {
                    addrs
                        .iter()
                        .map(|Ipv6AddressEntry { addr_sub, state: _, config: _, deprecated: _ }| {
                            addr_sub.ipv6_unicast_addr()
                        })
                        .collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()])
        );
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ll_addr.to_witness(),
                state: IpAddressState::Unavailable,
            })]
        );

        test_enable_device(&mut sync_ctx, &mut non_sync_ctx, Some(multicast_addr), false);
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ll_addr.addr().into(),
                    state: IpAddressState::Tentative,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                })
            ]
        );
        test_disable_device(&mut sync_ctx, &mut non_sync_ctx, true);
        // The address was manually added, don't expect it to be removed.
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ll_addr.addr().into(),
                    state: IpAddressState::Unavailable,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: false,
                })
            ]
        );

        // Verify that a redundant "disable" does not generate any events.
        test_disable_device(&mut sync_ctx, &mut non_sync_ctx, false);
        assert_eq!(non_sync_ctx.take_events()[..], []);

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
                &mut Locked::new(sync_ctx),
                &device_id,
                |addrs| {
                    addrs
                        .iter()
                        .map(|Ipv6AddressEntry { addr_sub, state: _, config: _, deprecated: _ }| {
                            addr_sub.ipv6_unicast_addr()
                        })
                        .collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "manual addresses should not be removed on device disable"
        );

        leave_ip_multicast::<Ipv6, _, _>(
            &mut Locked::new(sync_ctx),
            &mut non_sync_ctx,
            &device_id,
            multicast_addr,
        );
        test_enable_device(&mut sync_ctx, &mut non_sync_ctx, None, false);
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ll_addr.addr().into(),
                    state: IpAddressState::Tentative,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                })
            ]
        );

        // Verify that a redundant "enable" does not generate any events.
        test_enable_device(&mut sync_ctx, &mut non_sync_ctx, None, true);
        assert_eq!(non_sync_ctx.take_events()[..], []);
    }

    #[test]
    fn notify_on_dad_failure_ipv6() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        let mut sync_ctx = &sync_ctx;
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            sync_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            sync_ctx,
            &mut non_sync_ctx,
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(NonZeroU8::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(1)),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    gmp_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        let ll_addr = local_mac.to_ipv6_link_local();

        enable_ipv6_device(&mut sync_ctx, &mut non_sync_ctx, &device_id, ll_addr, false);
        let weak_device_id = device_id.downgrade();
        assert_eq!(
            non_sync_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                    device: weak_device_id.clone(),
                    addr: ll_addr.to_witness(),
                    state: IpAddressState::Tentative,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                }),
            ]
        );

        let assigned_addr = AddrSubnet::new(net_ip_v6!("fe80::1"), 64).unwrap();
        add_ipv6_addr_subnet(
            &mut Locked::new(sync_ctx),
            &mut non_sync_ctx,
            &device_id,
            assigned_addr,
            AddrConfig::Manual,
        )
        .expect("add succeeds");

        assert_eq!(
            non_sync_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: assigned_addr.to_witness(),
                state: IpAddressState::Tentative,
            }),]
        );

        // When DAD fails, an event should be emitted and the address should be
        // removed.
        assert_eq!(
            Ipv6DeviceHandler::remove_duplicate_tentative_address(
                &mut Locked::new(sync_ctx),
                &mut non_sync_ctx,
                &device_id,
                assigned_addr.ipv6_unicast_addr()
            ),
            Some(Ipv6DadState::Tentative { dad_transmits_remaining: None })
        );

        assert_eq!(
            non_sync_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
                device: weak_device_id.clone(),
                addr: assigned_addr.addr(),
                reason: RemovedReason::DadFailed,
            }),]
        );

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_ip_device_addresses(
                &mut Locked::new(sync_ctx),
                &device_id,
                |addrs| {
                    addrs
                        .iter()
                        .map(|Ipv6AddressEntry { addr_sub, state: _, config: _, deprecated: _ }| {
                            addr_sub.ipv6_unicast_addr()
                        })
                        .collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "manual addresses should be removed on DAD failure"
        );
    }

    #[ip_test]
    fn update_ip_device_configuration_err<I: Ip + UpdateIpDeviceConfigurationTestIpExt>() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } = FakeCtx::default();
        let sync_ctx = &sync_ctx;

        let loopback_device_id = crate::device::add_loopback_device(
            sync_ctx,
            Mtu::new(u16::MAX as u32),
            DEFAULT_INTERFACE_METRIC,
        )
        .expect("create the loopback interface")
        .into();

        let original_state = I::get_ip_configuration(sync_ctx, &loopback_device_id);
        assert_eq!(
            I::update_ip_configuration(
                sync_ctx,
                &mut non_sync_ctx,
                &loopback_device_id,
                IpDeviceConfigurationUpdate {
                    ip_enabled: Some(!original_state.as_ref().ip_enabled),
                    gmp_enabled: Some(!original_state.as_ref().gmp_enabled),
                    forwarding_enabled: Some(true),
                },
            ),
            Err(NotSupportedError),
        );
        assert_eq!(original_state, I::get_ip_configuration(sync_ctx, &loopback_device_id));
    }

    #[test]
    fn update_ipv4_configuration_return() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv4::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            &sync_ctx,
            local_mac,
            ethernet::MaxFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        // Perform no update.
        assert_eq!(
            update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv4DeviceConfigurationUpdate::default()
            ),
            Ok(Ipv4DeviceConfigurationUpdate::default()),
        );

        // Enable all but forwarding. All features are initially disabled.
        assert_eq!(
            update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(true),
                    }),
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(false),
                    forwarding_enabled: Some(false),
                    gmp_enabled: Some(false),
                }),
            }),
        );

        // Change forwarding config.
        assert_eq!(
            update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(true),
                        gmp_enabled: None,
                    }),
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(false),
                    gmp_enabled: None,
                }),
            }),
        );

        // No update to anything (GMP enabled set to already set
        // value).
        assert_eq!(
            update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: None,
                        forwarding_enabled: None,
                        gmp_enabled: Some(true),
                    }),
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: None,
                    forwarding_enabled: None,
                    gmp_enabled: Some(true),
                }),
            }),
        );

        // Disable/change everything.
        assert_eq!(
            update_ipv4_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(false),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(false),
                    }),
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(true),
                    gmp_enabled: Some(true),
                }),
            }),
        );
    }

    #[test]
    fn update_ipv6_configuration_return() {
        let FakeCtx { sync_ctx, mut non_sync_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        non_sync_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            &sync_ctx,
            local_mac,
            ethernet::MaxFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        // Perform no update.
        assert_eq!(
            update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv6DeviceConfigurationUpdate::default()
            ),
            Ok(Ipv6DeviceConfigurationUpdate::default()),
        );

        // Enable all but forwarding. All features are initially disabled.
        assert_eq!(
            update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: Some(NonZeroU8::new(1)),
                    max_router_solicitations: Some(NonZeroU8::new(2)),
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        temporary_address_configuration: None
                    }),
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(true),
                    }),
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(None),
                max_router_solicitations: Some(None),
                slaac_config: Some(SlaacConfiguration::default()),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(false),
                    forwarding_enabled: Some(false),
                    gmp_enabled: Some(false),
                }),
            }),
        );

        // Change forwarding config.
        assert_eq!(
            update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: None,
                    max_router_solicitations: None,
                    slaac_config: None,
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(true),
                        gmp_enabled: None,
                    }),
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: None,
                max_router_solicitations: None,
                slaac_config: None,
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(false),
                    gmp_enabled: None,
                }),
            }),
        );

        // No update to anything (GMP enabled set to already set
        // value).
        assert_eq!(
            update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: None,
                    max_router_solicitations: None,
                    slaac_config: None,
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: None,
                        forwarding_enabled: None,
                        gmp_enabled: Some(true),
                    }),
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: None,
                max_router_solicitations: None,
                slaac_config: None,
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: None,
                    forwarding_enabled: None,
                    gmp_enabled: Some(true),
                }),
            }),
        );

        // Disable/change everything.
        assert_eq!(
            update_ipv6_configuration(
                &sync_ctx,
                &mut non_sync_ctx,
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: Some(None),
                    max_router_solicitations: Some(None),
                    slaac_config: Some(SlaacConfiguration::default()),
                    ip_config: Some(IpDeviceConfigurationUpdate {
                        ip_enabled: Some(false),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(false),
                    }),
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(NonZeroU8::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(2)),
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    temporary_address_configuration: None
                }),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(true),
                    gmp_enabled: Some(true),
                }),
            }),
        );
    }
}
