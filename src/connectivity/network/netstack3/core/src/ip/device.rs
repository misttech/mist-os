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
use core::{fmt::Debug, num::NonZeroU8, ops::Deref};

use net_types::{
    ip::{
        AddrSubnet, AddrSubnetEither, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr,
        Ipv6SourceAddr, Mtu, Subnet,
    },
    MulticastAddr, NonMappedAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{BufferMut, Serializer};
use packet_formats::{
    icmp::{mld::MldPacket, ndp::NonZeroNdpLifetime},
    utils::NonZeroDuration,
};
use zerocopy::ByteSlice;

use crate::{
    context::{
        EventContext, InstantBindingsTypes, InstantContext, RngContext, TimerContext, TimerHandler,
    },
    device::{AnyDevice, DeviceIdContext},
    error::{ExistsError, NotFoundError, SetIpAddressPropertiesError},
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
                DelIpv6AddrReason, IpDeviceAddresses, IpDeviceConfiguration, IpDeviceFlags,
                IpDeviceState, IpDeviceStateIpExt, Ipv4AddrConfig, Ipv4AddressState,
                Ipv4DeviceConfiguration, Ipv4DeviceConfigurationAndFlags, Ipv4DeviceState,
                Ipv6AddrConfig, Ipv6AddrManualConfig, Ipv6AddressFlags, Ipv6AddressState,
                Ipv6DeviceConfiguration, Ipv6DeviceConfigurationAndFlags, Ipv6DeviceState,
                Lifetime,
            },
        },
        gmp::{
            igmp::{IgmpPacketHandler, IgmpTimerId},
            mld::{MldDelayedReportTimerId, MldPacketHandler},
            GmpHandler, GmpQueryHandler, GroupJoinResult, GroupLeaveResult,
        },
    },
    CoreCtx, Instant,
};

use self::state::Ipv6NetworkLearnedParameters;

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
    IgmpTimerId::<DeviceId>,
    Ipv4DeviceTimerId(id),
    id
);

impl<DeviceId, BC, CC: TimerHandler<BC, IgmpTimerId<DeviceId>>>
    TimerHandler<BC, Ipv4DeviceTimerId<DeviceId>> for CC
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        Ipv4DeviceTimerId(id): Ipv4DeviceTimerId<DeviceId>,
    ) {
        TimerHandler::handle_timer(self, bindings_ctx, id)
    }
}

/// Handle an IPv4 device timer firing.
pub(crate) fn handle_ipv4_timer<
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: Ipv4DeviceTimerId<CC::DeviceId>,
) {
    let device_id = id.device_id().clone();
    core_ctx.with_ip_device_configuration(&device_id, |_state, mut core_ctx| {
        TimerHandler::handle_timer(&mut core_ctx, bindings_ctx, id)
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
    MldDelayedReportTimerId::<DeviceId>,
    Ipv6DeviceTimerId::Mld(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    DadTimerId::<DeviceId>,
    Ipv6DeviceTimerId::Dad(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    RsTimerId::<DeviceId>,
    Ipv6DeviceTimerId::Rs(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    Ipv6DiscoveredRouteTimerId::<DeviceId>,
    Ipv6DeviceTimerId::RouteDiscovery(id),
    id
);
impl_timer_context!(
    DeviceId,
    Ipv6DeviceTimerId<DeviceId>,
    SlaacTimerId::<DeviceId>,
    Ipv6DeviceTimerId::Slaac(id),
    id
);

impl<
        DeviceId,
        BC,
        CC: TimerHandler<BC, RsTimerId<DeviceId>>
            + TimerHandler<BC, Ipv6DiscoveredRouteTimerId<DeviceId>>
            + TimerHandler<BC, MldDelayedReportTimerId<DeviceId>>
            + TimerHandler<BC, SlaacTimerId<DeviceId>>
            + TimerHandler<BC, DadTimerId<DeviceId>>,
    > TimerHandler<BC, Ipv6DeviceTimerId<DeviceId>> for CC
{
    fn handle_timer(&mut self, bindings_ctx: &mut BC, id: Ipv6DeviceTimerId<DeviceId>) {
        match id {
            Ipv6DeviceTimerId::Mld(id) => TimerHandler::handle_timer(self, bindings_ctx, id),
            Ipv6DeviceTimerId::Dad(id) => TimerHandler::handle_timer(self, bindings_ctx, id),
            Ipv6DeviceTimerId::Rs(id) => TimerHandler::handle_timer(self, bindings_ctx, id),
            Ipv6DeviceTimerId::RouteDiscovery(id) => {
                TimerHandler::handle_timer(self, bindings_ctx, id)
            }
            Ipv6DeviceTimerId::Slaac(id) => TimerHandler::handle_timer(self, bindings_ctx, id),
        }
    }
}

/// Handle an IPv6 device timer firing.
pub(crate) fn handle_ipv6_timer<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv6, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    id: Ipv6DeviceTimerId<CC::DeviceId>,
) {
    let device_id = id.device_id().clone();
    core_ctx.with_ip_device_configuration(&device_id, |_state, mut core_ctx| {
        TimerHandler::handle_timer(&mut core_ctx, bindings_ctx, id)
    })
}

/// An extension trait adding IP device properties.
pub(crate) trait IpDeviceIpExt: IpDeviceStateIpExt {
    type State<I: Instant>: AsRef<IpDeviceState<I, Self>> + AsMut<IpDeviceState<I, Self>>;
    type Configuration: AsRef<IpDeviceConfiguration>;
    type Timer<DeviceId>;
    type AssignedWitness: Witness<Self::Addr> + Copy + PartialEq + Debug;
    type AddressConfig<I>: Default;
    type ManualAddressConfig<I>: Default;
    type AddressState<I>;
}

impl IpDeviceIpExt for Ipv4 {
    type State<I: Instant> = Ipv4DeviceState<I>;
    type Configuration = Ipv4DeviceConfiguration;
    type Timer<DeviceId> = Ipv4DeviceTimerId<DeviceId>;
    type AssignedWitness = SpecifiedAddr<Ipv4Addr>;
    type AddressConfig<I> = Ipv4AddrConfig<I>;
    type ManualAddressConfig<I> = Ipv4AddrConfig<I>;
    type AddressState<I> = Ipv4AddressState<I>;
}

impl IpDeviceIpExt for Ipv6 {
    type State<I: Instant> = Ipv6DeviceState<I>;
    type Configuration = Ipv6DeviceConfiguration;
    type Timer<DeviceId> = Ipv6DeviceTimerId<DeviceId>;
    type AssignedWitness = Ipv6DeviceAddr;
    type AddressConfig<I> = Ipv6AddrConfig<I>;
    type ManualAddressConfig<I> = Ipv6AddrManualConfig<I>;
    type AddressState<I> = Ipv6AddressState<I>;
}

/// An IPv6 address that witnesses properties needed to be assigned to a device.
pub(crate) type Ipv6DeviceAddr = NonMappedAddr<UnicastAddr<Ipv6Addr>>;

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
pub enum AddressRemovedReason {
    /// The address was removed in response to external action.
    Manual,
    /// The address was removed because it was detected as a duplicate via DAD.
    DadFailed,
}

impl From<DelIpv6AddrReason> for AddressRemovedReason {
    fn from(reason: DelIpv6AddrReason) -> Self {
        match reason {
            DelIpv6AddrReason::ManualAction => Self::Manual,
            DelIpv6AddrReason::DadFailed => Self::DadFailed,
        }
    }
}

#[derive(Debug, Eq, Hash, PartialEq)]
/// Events emitted from IP devices.
pub enum IpDeviceEvent<DeviceId, I: Ip, Instant> {
    /// Address was assigned.
    AddressAdded {
        /// The device.
        device: DeviceId,
        /// The new address.
        addr: AddrSubnet<I::Addr>,
        /// Initial address state.
        state: IpAddressState,
        /// The lifetime for which the address is valid.
        valid_until: Lifetime<Instant>,
    },
    /// Address was unassigned.
    AddressRemoved {
        /// The device.
        device: DeviceId,
        /// The removed address.
        addr: SpecifiedAddr<I::Addr>,
        /// The reason the address was removed.
        reason: AddressRemovedReason,
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
    /// Address properties changed.
    AddressPropertiesChanged {
        /// The device.
        device: DeviceId,
        /// The address whose properties were changed.
        addr: SpecifiedAddr<I::Addr>,
        /// The new `valid_until` lifetime.
        valid_until: Lifetime<Instant>,
    },
    /// IP was enabled/disabled on the device
    EnabledChanged {
        /// The device.
        device: DeviceId,
        /// `true` if IP was enabled on the device; `false` if IP was disabled.
        ip_enabled: bool,
    },
}

impl<
        DeviceId,
        BC: InstantContext
            + EventContext<IpDeviceEvent<DeviceId, Ipv6, <BC as InstantBindingsTypes>::Instant>>,
    > EventContext<DadEvent<DeviceId>> for BC
{
    fn on_event(&mut self, event: DadEvent<DeviceId>) {
        match event {
            DadEvent::AddressAssigned { device, addr } => BC::on_event(
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

/// The bindings execution context for dual-stack devices.
pub(crate) trait DualStackDeviceBindingsContext: InstantContext {}
impl<BC: InstantContext> DualStackDeviceBindingsContext for BC {}

/// The core execution context for dual-stack devices.
pub(crate) trait DualStackDeviceContext<BC: DualStackDeviceBindingsContext>:
    DeviceIdContext<AnyDevice>
{
    /// Calls the function with an immutable view into the dual-stack device's
    /// state.
    fn with_dual_stack_device_state<O, F: FnOnce(DualStackDeviceStateRef<'_, BC::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// An implementation of dual-stack devices.
pub(crate) trait DualStackDeviceHandler<BC>: DeviceIdContext<AnyDevice> {
    /// Get all IPv4 and IPv6 address/subnet pairs configured on a device.
    fn get_all_ip_addr_subnets(&mut self, device_id: &Self::DeviceId) -> Vec<AddrSubnetEither>;
}

impl<BC: DualStackDeviceBindingsContext, CC: DualStackDeviceContext<BC>> DualStackDeviceHandler<BC>
    for CC
{
    fn get_all_ip_addr_subnets(&mut self, device_id: &Self::DeviceId) -> Vec<AddrSubnetEither> {
        self.with_dual_stack_device_state(device_id, |DualStackDeviceStateRef { ipv4, ipv6 }| {
            let addrs_v4 = ipv4
                .iter()
                .map(Deref::deref)
                .filter_map(<Ipv4 as IpDeviceStateIpExt>::assigned_addr::<BC::Instant>);
            let addrs_v6 = ipv6
                .iter()
                .map(Deref::deref)
                .filter_map(<Ipv6 as IpDeviceStateIpExt>::assigned_addr::<BC::Instant>);

            addrs_v4.map(AddrSubnetEither::V4).chain(addrs_v6.map(AddrSubnetEither::V6)).collect()
        })
    }
}

/// The bindings execution context for IP devices.
pub(crate) trait IpDeviceBindingsContext<I: IpDeviceIpExt, DeviceId>:
    RngContext
    + TimerContext<I::Timer<DeviceId>>
    + EventContext<IpDeviceEvent<DeviceId, I, <Self as InstantBindingsTypes>::Instant>>
{
}
impl<
        DeviceId,
        I: IpDeviceIpExt,
        BC: RngContext
            + TimerContext<I::Timer<DeviceId>>
            + EventContext<IpDeviceEvent<DeviceId, I, <Self as InstantBindingsTypes>::Instant>>,
    > IpDeviceBindingsContext<I, DeviceId> for BC
{
}

/// An IP address ID.
pub(crate) trait IpAddressId<A: IpAddress>: Clone + Debug {
    /// Returns the specified address this ID represents.
    fn addr(&self) -> SpecifiedAddr<A>;

    /// Returns the address subnet this ID represents.
    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness>
    where
        A::Version: IpDeviceIpExt;
}

impl<A: IpAddress> IpAddressId<A> for AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness>
where
    A::Version: IpDeviceIpExt,
    SpecifiedAddr<A>: From<<A::Version as IpDeviceIpExt>::AssignedWitness>,
{
    fn addr(&self) -> SpecifiedAddr<A> {
        self.to_witness::<SpecifiedAddr<A>>().addr()
    }

    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness> {
        self.clone()
    }
}

/// Provides the execution context related to address IDs.
pub(crate) trait IpDeviceAddressIdContext<I: IpDeviceIpExt>:
    DeviceIdContext<AnyDevice>
{
    type AddressId: IpAddressId<I::Addr>;
}

pub(crate) trait IpDeviceAddressContext<I: IpDeviceIpExt, BC: InstantContext>:
    IpDeviceAddressIdContext<I>
{
    fn with_ip_address_state<O, F: FnOnce(&I::AddressState<BC::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O;

    fn with_ip_address_state_mut<O, F: FnOnce(&mut I::AddressState<BC::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O;
}

/// Accessor for IP device state.
pub(crate) trait IpDeviceStateContext<I: IpDeviceIpExt, BC: InstantContext>:
    IpDeviceAddressContext<I, BC>
{
    type IpDeviceAddressCtx<'a>: IpDeviceAddressContext<
        I,
        BC,
        DeviceId = Self::DeviceId,
        AddressId = Self::AddressId,
    >;

    /// Calls the function with immutable access to the device's flags.
    ///
    /// Note that this trait should only provide immutable access to the flags.
    /// Changes to the IP device flags must only be performed while synchronizing
    /// with the IP device configuration, so mutable access to the flags is through
    /// `WithIpDeviceConfigurationMutInner::with_configuration_and_flags_mut`.
    fn with_ip_device_flags<O, F: FnOnce(&IpDeviceFlags) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Adds an IP address for the device.
    fn add_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: AddrSubnet<I::Addr, I::AssignedWitness>,
        config: I::AddressConfig<BC::Instant>,
    ) -> Result<Self::AddressId, ExistsError>;

    /// Removes an address from the device identified by the ID.
    fn remove_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: Self::AddressId,
    ) -> (AddrSubnet<I::Addr, I::AssignedWitness>, I::AddressConfig<BC::Instant>);

    /// Returns the address ID for the given address value.
    fn get_address_id(
        &mut self,
        device_id: &Self::DeviceId,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Result<Self::AddressId, NotFoundError>;

    /// Calls the function with an iterator over all the address IDs associated
    /// with the device.
    // TODO(https://fxbug.dev/107842): Avoid dynamic dispatch.
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
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    );

    /// Leaves the link-layer multicast group associated with the given IP
    /// multicast group.
    fn leave_link_multicast_group(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    );
}

/// The context provided to the callback passed to
/// [`IpDeviceConfigurationContext::with_ip_device_configuration_mut`].
pub(crate) trait WithIpDeviceConfigurationMutInner<I: IpDeviceIpExt, BC: InstantContext>:
    DeviceIdContext<AnyDevice>
{
    type IpDeviceStateCtx<'s>: IpDeviceStateContext<I, BC, DeviceId = Self::DeviceId>
        + GmpHandler<I, BC>
        + NudIpHandler<I, BC>
        + 's
    where
        Self: 's;

    /// Returns an immutable reference to a device's IP configuration and an
    /// `IpDeviceStateCtx`.
    fn ip_device_configuration_and_ctx(
        &mut self,
    ) -> (&I::Configuration, Self::IpDeviceStateCtx<'_>);

    /// Calls the function with a mutable reference to a device's IP
    /// configuration and flags.
    fn with_configuration_and_flags_mut<
        O,
        F: FnOnce(&mut I::Configuration, &mut IpDeviceFlags) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// The execution context for IP devices.
pub(crate) trait IpDeviceConfigurationContext<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, Self::DeviceId>,
>: IpDeviceStateContext<I, BC> + DeviceIdContext<AnyDevice>
{
    type DevicesIter<'s>: Iterator<Item = Self::DeviceId> + 's;
    type WithIpDeviceConfigurationInnerCtx<'s>: IpDeviceStateContext<I, BC, DeviceId = Self::DeviceId>
        + GmpHandler<I, BC>
        + NudIpHandler<I, BC>
        + TimerHandler<BC, I::Timer<Self::DeviceId>>
        + 's;
    type WithIpDeviceConfigurationMutInner<'s>: WithIpDeviceConfigurationMutInner<I, BC, DeviceId = Self::DeviceId>
        + 's;
    type DeviceAddressAndGroupsAccessor<'s>: IpDeviceStateContext<I, BC, DeviceId = Self::DeviceId>
        + GmpQueryHandler<I, BC>
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
    BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>,
>: WithIpDeviceConfigurationMutInner<Ipv6, BC>
{
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<BC, DeviceId = Self::DeviceId>
        + GmpHandler<Ipv6, BC>
        + NudIpHandler<Ipv6, BC>
        + RsHandler<BC>
        + DadHandler<BC>
        + SlaacHandler<BC>
        + RouteDiscoveryHandler<BC>
        + 's
    where
        Self: 's;

    /// Returns an immutable reference to a device's IPv6 configuration and an
    /// `Ipv6DeviceStateCtx`.
    fn ipv6_device_configuration_and_ctx(
        &mut self,
    ) -> (&Ipv6DeviceConfiguration, Self::Ipv6DeviceStateCtx<'_>);
}

pub(crate) trait Ipv6DeviceConfigurationContext<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
    IpDeviceConfigurationContext<Ipv6, BC>
{
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<BC, DeviceId = Self::DeviceId, AddressId = Self::AddressId>
        + GmpHandler<Ipv6, BC>
        + MldPacketHandler<BC, Self::DeviceId>
        + NudIpHandler<Ipv6, BC>
        + DadHandler<BC>
        + RsHandler<BC>
        + SlaacHandler<BC>
        + RouteDiscoveryHandler<BC>
        + 's;
    type WithIpv6DeviceConfigurationMutInner<'s>: WithIpv6DeviceConfigurationMutInner<BC, DeviceId = Self::DeviceId>
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

/// The execution context for an IPv6 device.
pub(crate) trait Ipv6DeviceContext<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
    IpDeviceStateContext<Ipv6, BC>
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
    fn with_network_learned_parameters<O, F: FnOnce(&Ipv6NetworkLearnedParameters) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with a mutable reference to the retransmit timer.
    fn with_network_learned_parameters_mut<O, F: FnOnce(&mut Ipv6NetworkLearnedParameters) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// An implementation of an IP device.
pub(crate) trait IpDeviceHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
    fn is_router_device(&mut self, device_id: &Self::DeviceId) -> bool;

    fn set_default_hop_limit(&mut self, device_id: &Self::DeviceId, hop_limit: NonZeroU8);
}

impl<
        I: IpDeviceIpExt,
        BC: IpDeviceBindingsContext<I, CC::DeviceId>,
        CC: IpDeviceConfigurationContext<I, BC>,
    > IpDeviceHandler<I, BC> for CC
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

pub(crate) fn receive_igmp_packet<CC, BC, B>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    src_ip: Ipv4Addr,
    dst_ip: SpecifiedAddr<Ipv4Addr>,
    buffer: B,
) where
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    for<'a> CC::WithIpDeviceConfigurationInnerCtx<'a>: IpDeviceStateContext<Ipv4, BC, DeviceId = CC::DeviceId>
        + IgmpPacketHandler<BC, CC::DeviceId>,
    B: BufferMut,
{
    core_ctx.with_ip_device_configuration(device, |_config, mut core_ctx| {
        IgmpPacketHandler::receive_igmp_packet(
            &mut core_ctx,
            bindings_ctx,
            device,
            src_ip,
            dst_ip,
            buffer,
        )
    })
}

/// An implementation of an IPv6 device.
pub(crate) trait Ipv6DeviceHandler<BC>: IpDeviceHandler<Ipv6, BC> {
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
        bindings_ctx: &mut BC,
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
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
    ) -> IpAddressState;

    /// Sets the link MTU for the device.
    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu);

    /// Updates a discovered IPv6 route.
    fn update_discovered_ipv6_route(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
        lifetime: Option<NonZeroNdpLifetime>,
    );

    /// Applies a SLAAC update.
    fn apply_slaac_update(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        prefix: Subnet<Ipv6Addr>,
        preferred_lifetime: Option<NonZeroNdpLifetime>,
        valid_lifetime: Option<NonZeroNdpLifetime>,
    );

    /// Receives an MLD packet for processing.
    fn receive_mld_packet<B: ByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    );
}

impl<
        BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
        CC: Ipv6DeviceContext<BC> + Ipv6DeviceConfigurationContext<BC>,
    > Ipv6DeviceHandler<BC> for CC
{
    type LinkLayerAddr = CC::LinkLayerAddr;

    fn get_link_layer_addr_bytes(
        &mut self,
        device_id: &Self::DeviceId,
    ) -> Option<CC::LinkLayerAddr> {
        Ipv6DeviceContext::get_link_layer_addr_bytes(self, device_id)
    }

    fn set_discovered_retrans_timer(
        &mut self,
        _bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        retrans_timer: NonZeroDuration,
    ) {
        self.with_network_learned_parameters_mut(device_id, |state| {
            state.retrans_timer = Some(retrans_timer)
        })
    }

    fn remove_duplicate_tentative_address(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
    ) -> IpAddressState {
        let addr_id = match self.get_address_id(device_id, addr.into_specified()) {
            Ok(o) => o,
            Err(NotFoundError) => return IpAddressState::Unavailable,
        };

        let assigned = self.with_ip_address_state(
            device_id,
            &addr_id,
            |Ipv6AddressState {
                 flags: Ipv6AddressFlags { deprecated: _, assigned },
                 config: _,
             }| { *assigned },
        );

        if assigned {
            IpAddressState::Assigned
        } else {
            del_ipv6_addr_with_reason(
                self,
                bindings_ctx,
                device_id,
                DelIpv6Addr::AddressId(addr_id),
                DelIpv6AddrReason::DadFailed,
            )
            .unwrap();
            IpAddressState::Tentative
        }
    }

    fn set_link_mtu(&mut self, device_id: &Self::DeviceId, mtu: Mtu) {
        Ipv6DeviceContext::set_link_mtu(self, device_id, mtu)
    }

    fn update_discovered_ipv6_route(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        route: Ipv6DiscoveredRoute,
        lifetime: Option<NonZeroNdpLifetime>,
    ) {
        self.with_ipv6_device_configuration(device_id, |_config, mut core_ctx| {
            RouteDiscoveryHandler::update_route(
                &mut core_ctx,
                bindings_ctx,
                device_id,
                route,
                lifetime,
            )
        })
    }

    fn apply_slaac_update(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        prefix: Subnet<Ipv6Addr>,
        preferred_lifetime: Option<NonZeroNdpLifetime>,
        valid_lifetime: Option<NonZeroNdpLifetime>,
    ) {
        self.with_ipv6_device_configuration(device_id, |_config, mut core_ctx| {
            SlaacHandler::apply_slaac_update(
                &mut core_ctx,
                bindings_ctx,
                device_id,
                prefix,
                preferred_lifetime,
                valid_lifetime,
            )
        })
    }

    fn receive_mld_packet<B: ByteSlice>(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        src_ip: Ipv6SourceAddr,
        dst_ip: SpecifiedAddr<Ipv6Addr>,
        packet: MldPacket<B>,
    ) {
        self.with_ipv6_device_configuration(device, |_config, mut core_ctx| {
            MldPacketHandler::receive_mld_packet(
                &mut core_ctx,
                bindings_ctx,
                device,
                src_ip,
                dst_ip,
                packet,
            )
        })
    }
}

/// The execution context for an IP device with a buffer.
pub(crate) trait IpDeviceSendContext<I: Ip, BC>: DeviceIdContext<AnyDevice> {
    /// Sends an IP packet through the device.
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        local_addr: SpecifiedAddr<I::Addr>,
        body: S,
    ) -> Result<(), S>
    where
        S: Serializer,
        S::Buffer: BufferMut;
}

fn enable_ipv6_device_with_config<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceContext<BC> + GmpHandler<Ipv6, BC> + RsHandler<BC> + DadHandler<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    config: &Ipv6DeviceConfiguration,
) {
    // All nodes should join the all-nodes multicast group.
    join_ip_multicast_with_config(
        core_ctx,
        bindings_ctx,
        device_id,
        Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS,
        config,
    );
    GmpHandler::gmp_handle_maybe_enabled(core_ctx, bindings_ctx, device_id);

    // Perform DAD for all addresses when enabling a device.
    //
    // We have to do this for all addresses (including ones that had DAD
    // performed) as while the device was disabled, another node could have
    // assigned the address and we wouldn't have responded to its DAD
    // solicitations.
    core_ctx
        .with_address_ids(device_id, |addrs, _core_ctx| addrs.collect::<Vec<_>>())
        .into_iter()
        .for_each(|addr_id| {
            bindings_ctx.on_event(IpDeviceEvent::AddressStateChanged {
                device: device_id.clone(),
                addr: addr_id.addr(),
                state: IpAddressState::Tentative,
            });
            DadHandler::start_duplicate_address_detection(
                core_ctx,
                bindings_ctx,
                device_id,
                &addr_id,
            );
        });

    // TODO(https://fxbug.dev/95946): Generate link-local address with opaque
    // IIDs.
    if config.slaac_config.enable_stable_addresses {
        if let Some(iid) = core_ctx.get_eui64_iid(device_id) {
            let link_local_addr_sub = {
                let mut addr = [0; 16];
                addr[0..2].copy_from_slice(&[0xfe, 0x80]);
                addr[(Ipv6::UNICAST_INTERFACE_IDENTIFIER_BITS / 8) as usize..]
                    .copy_from_slice(&iid);

                AddrSubnet::new(
                    Ipv6Addr::from(addr),
                    Ipv6Addr::BYTES * 8 - Ipv6::UNICAST_INTERFACE_IDENTIFIER_BITS,
                )
                .expect("valid link-local address")
            };

            match add_ipv6_addr_subnet_with_config(
                core_ctx,
                bindings_ctx,
                device_id,
                link_local_addr_sub,
                Ipv6AddrConfig::SLAAC_LINK_LOCAL,
                config,
            ) {
                Ok(_) => {}
                Err(ExistsError) => {
                    // The address may have been added by admin action so it is safe
                    // to swallow the exists error.
                }
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
        RsHandler::start_router_solicitation(core_ctx, bindings_ctx, device_id);
    }
}

fn disable_ipv6_device_with_config<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceContext<BC>
        + GmpHandler<Ipv6, BC>
        + RsHandler<BC>
        + DadHandler<BC>
        + RouteDiscoveryHandler<BC>
        + SlaacHandler<BC>
        + NudIpHandler<Ipv6, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    device_config: &Ipv6DeviceConfiguration,
) {
    NudIpHandler::flush_neighbor_table(core_ctx, bindings_ctx, device_id);

    SlaacHandler::remove_all_slaac_addresses(core_ctx, bindings_ctx, device_id);

    RouteDiscoveryHandler::invalidate_routes(core_ctx, bindings_ctx, device_id);

    RsHandler::stop_router_solicitation(core_ctx, bindings_ctx, device_id);

    // Delete the link-local address generated when enabling the device and stop
    // DAD on the other addresses.
    core_ctx
        .with_address_ids(device_id, |addrs, core_ctx| {
            addrs
                .map(|addr_id| {
                    core_ctx.with_ip_address_state(
                        device_id,
                        &addr_id,
                        |Ipv6AddressState { flags: _, config }| (addr_id.clone(), *config),
                    )
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .for_each(|(addr_id, config)| {
            if config == Ipv6AddrConfig::SLAAC_LINK_LOCAL {
                del_ipv6_addr_with_reason_with_config(
                    core_ctx,
                    bindings_ctx,
                    device_id,
                    DelIpv6Addr::AddressId(addr_id),
                    DelIpv6AddrReason::ManualAction,
                    device_config,
                )
                .expect("delete listed address")
            } else {
                DadHandler::stop_duplicate_address_detection(
                    core_ctx,
                    bindings_ctx,
                    device_id,
                    &addr_id,
                );
                bindings_ctx.on_event(IpDeviceEvent::AddressStateChanged {
                    device: device_id.clone(),
                    addr: addr_id.addr(),
                    state: IpAddressState::Unavailable,
                });
            }
        });

    GmpHandler::gmp_handle_disabled(core_ctx, bindings_ctx, device_id);
    leave_ip_multicast_with_config(
        core_ctx,
        bindings_ctx,
        device_id,
        Ipv6::ALL_NODES_LINK_LOCAL_MULTICAST_ADDRESS,
        device_config,
    );
}

fn enable_ipv4_device_with_config<
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpDeviceStateContext<Ipv4, BC> + GmpHandler<Ipv4, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    config: &Ipv4DeviceConfiguration,
) {
    // All systems should join the all-systems multicast group.
    join_ip_multicast_with_config(
        core_ctx,
        bindings_ctx,
        device_id,
        Ipv4::ALL_SYSTEMS_MULTICAST_ADDRESS,
        config,
    );
    GmpHandler::gmp_handle_maybe_enabled(core_ctx, bindings_ctx, device_id);
    core_ctx.with_address_ids(device_id, |addrs, _core_ctx| {
        addrs.for_each(|addr| {
            bindings_ctx.on_event(IpDeviceEvent::AddressStateChanged {
                device: device_id.clone(),
                addr: addr.addr(),
                state: IpAddressState::Assigned,
            });
        })
    })
}

fn disable_ipv4_device_with_config<
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpDeviceStateContext<Ipv4, BC> + GmpHandler<Ipv4, BC> + NudIpHandler<Ipv4, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    config: &Ipv4DeviceConfiguration,
) {
    NudIpHandler::flush_neighbor_table(core_ctx, bindings_ctx, device_id);
    GmpHandler::gmp_handle_disabled(core_ctx, bindings_ctx, device_id);
    leave_ip_multicast_with_config(
        core_ctx,
        bindings_ctx,
        device_id,
        Ipv4::ALL_SYSTEMS_MULTICAST_ADDRESS,
        config,
    );
    core_ctx.with_address_ids(device_id, |addrs, _core_ctx| {
        addrs.for_each(|addr| {
            bindings_ctx.on_event(IpDeviceEvent::AddressStateChanged {
                device: device_id.clone(),
                addr: addr.addr(),
                state: IpAddressState::Unavailable,
            });
        })
    })
}

/// Gets the IPv4 address and subnet pairs associated with this device.
///
/// Returns an [`Iterator`] of `AddrSubnet`.
pub(crate) fn with_assigned_ipv4_addr_subnets<
    BC: InstantContext,
    CC: IpDeviceStateContext<Ipv4, BC>,
    O,
    F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<Ipv4Addr>> + '_>) -> O,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
    cb: F,
) -> O {
    core_ctx
        .with_address_ids(device_id, |addrs, _core_ctx| cb(Box::new(addrs.map(|a| a.addr_sub()))))
}

/// Gets a single IPv4 address and subnet for a device.
pub(crate) fn get_ipv4_addr_subnet<BC: InstantContext, CC: IpDeviceStateContext<Ipv4, BC>>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Option<AddrSubnet<Ipv4Addr>> {
    with_assigned_ipv4_addr_subnets(core_ctx, device_id, |mut addrs| addrs.nth(0))
}

/// Gets the hop limit for new IPv6 packets that will be sent out from `device`.
pub(crate) fn get_ipv6_hop_limit<BC: InstantContext, CC: IpDeviceStateContext<Ipv6, BC>>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
) -> NonZeroU8 {
    core_ctx.with_default_hop_limit(device, Clone::clone)
}

/// Is IP packet forwarding enabled?
pub(crate) fn is_ip_forwarding_enabled<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> bool {
    core_ctx.with_ip_device_configuration(device_id, |state, _ctx| {
        AsRef::<IpDeviceConfiguration>::as_ref(state).forwarding_enabled
    })
}

fn join_ip_multicast_with_config<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
    // Not used but required to make sure that the caller is currently holding a
    // a reference to the IP device's IP configuration as a way to prove that
    // caller has synchronized this operation with other accesses to the IP
    // device configuration.
    _config: &I::Configuration,
) {
    match core_ctx.gmp_join_group(bindings_ctx, device_id, multicast_addr) {
        GroupJoinResult::Joined(()) => {
            core_ctx.join_link_multicast_group(bindings_ctx, device_id, multicast_addr)
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
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
) {
    core_ctx.with_ip_device_configuration(device_id, |config, mut core_ctx| {
        join_ip_multicast_with_config(
            &mut core_ctx,
            bindings_ctx,
            device_id,
            multicast_addr,
            config,
        )
    })
}

fn leave_ip_multicast_with_config<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
    // Not used but required to make sure that the caller is currently holding a
    // a reference to the IP device's IP configuration as a way to prove that
    // caller has synchronized this operation with other accesses to the IP
    // device configuration.
    _config: &I::Configuration,
) {
    match core_ctx.gmp_leave_group(bindings_ctx, device_id, multicast_addr) {
        GroupLeaveResult::Left(()) => {
            core_ctx.leave_link_multicast_group(bindings_ctx, device_id, multicast_addr)
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
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
) {
    core_ctx.with_ip_device_configuration(device_id, |config, mut core_ctx| {
        leave_ip_multicast_with_config(
            &mut core_ctx,
            bindings_ctx,
            device_id,
            multicast_addr,
            config,
        )
    })
}

/// Adds an IPv4 address and associated subnet to this device.
pub(crate) fn add_ipv4_addr_subnet<
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr_sub: AddrSubnet<Ipv4Addr>,
    addr_config: Ipv4AddrConfig<BC::Instant>,
) -> Result<(), ExistsError> {
    core_ctx.with_ip_device_configuration(device_id, |_config, mut core_ctx| {
        let address_state = match core_ctx
            .with_ip_device_flags(device_id, |IpDeviceFlags { ip_enabled }| *ip_enabled)
        {
            true => IpAddressState::Assigned,
            false => IpAddressState::Unavailable,
        };

        let Ipv4AddrConfig { valid_until } = addr_config;

        core_ctx.add_ip_address(device_id, addr_sub, addr_config).map(|id| {
            assert_eq!(id.addr(), addr_sub.addr());
            bindings_ctx.on_event(IpDeviceEvent::AddressAdded {
                device: device_id.clone(),
                addr: addr_sub,
                state: address_state,
                valid_until,
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
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceConfigurationContext<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    addr_config: Ipv6AddrManualConfig<BC::Instant>,
) -> Result<(), ExistsError> {
    core_ctx.with_ipv6_device_configuration(device_id, |config, mut core_ctx| {
        add_ipv6_addr_subnet_with_config(
            &mut core_ctx,
            bindings_ctx,
            device_id,
            addr_sub,
            Ipv6AddrConfig::Manual(addr_config),
            config,
        )
        .map(|_address_id| ())
    })
}

fn add_ipv6_addr_subnet_with_config<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpDeviceStateContext<Ipv6, BC> + GmpHandler<Ipv6, BC> + DadHandler<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    addr_config: Ipv6AddrConfig<BC::Instant>,
    // Not used but required to make sure that the caller is currently holding a
    // a reference to the IP device's IP configuration as a way to prove that
    // caller has synchronized this operation with other accesses to the IP
    // device configuration.
    _device_config: &Ipv6DeviceConfiguration,
) -> Result<CC::AddressId, ExistsError> {
    let addr_id = core_ctx.add_ip_address(device_id, addr_sub, addr_config)?;
    assert_eq!(addr_id.addr(), addr_sub.addr().into_specified());

    let ip_enabled =
        core_ctx.with_ip_device_flags(device_id, |IpDeviceFlags { ip_enabled }| *ip_enabled);

    let state = match ip_enabled {
        true => IpAddressState::Tentative,
        false => IpAddressState::Unavailable,
    };

    bindings_ctx.on_event(IpDeviceEvent::AddressAdded {
        device: device_id.clone(),
        addr: addr_sub.to_witness(),
        state,
        valid_until: addr_config.valid_until(),
    });

    if ip_enabled {
        // NB: We don't start DAD if the device is disabled. DAD will be
        // performed when the device is enabled for all addressed.
        DadHandler::start_duplicate_address_detection(core_ctx, bindings_ctx, device_id, &addr_id)
    }

    Ok(addr_id)
}

pub(crate) fn set_ipv4_addr_properties<
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    address: SpecifiedAddr<Ipv4Addr>,
    next_valid_until: Lifetime<BC::Instant>,
) -> Result<(), SetIpAddressPropertiesError> {
    let address_id = core_ctx.get_address_id(device, address)?;
    core_ctx.with_ip_address_state_mut(
        device,
        &address_id,
        |Ipv4AddressState { config: Ipv4AddrConfig { valid_until } }| {
            if core::mem::replace(valid_until, next_valid_until) != next_valid_until {
                bindings_ctx.on_event(IpDeviceEvent::AddressPropertiesChanged {
                    device: device.clone(),
                    addr: address,
                    valid_until: next_valid_until,
                });
            }
        },
    );
    Ok(())
}

pub(crate) fn set_ipv6_addr_properties<
    CC: IpDeviceConfigurationContext<Ipv6, BC>,
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    address: SpecifiedAddr<Ipv6Addr>,
    next_valid_until: Lifetime<BC::Instant>,
) -> Result<(), SetIpAddressPropertiesError> {
    let address_id = core_ctx.get_address_id(device, address)?;
    core_ctx.with_ip_address_state_mut(
        device,
        &address_id,
        |Ipv6AddressState { config, flags: _ }| match config {
            Ipv6AddrConfig::Slaac(_) => Err(SetIpAddressPropertiesError::NotManual),
            Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { valid_until }) => {
                if core::mem::replace(valid_until, next_valid_until) != next_valid_until {
                    bindings_ctx.on_event(IpDeviceEvent::AddressPropertiesChanged {
                        device: device.clone(),
                        addr: address,
                        valid_until: next_valid_until,
                    });
                }
                Ok(())
            }
        },
    )
}

/// Removes an IPv4 address and associated subnet from this device.
pub(crate) fn del_ipv4_addr<
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: &SpecifiedAddr<Ipv4Addr>,
) -> Result<(), NotFoundError> {
    let addr_id = core_ctx.get_address_id(device_id, *addr)?;
    let (addr_sub, config) = core_ctx.remove_ip_address(device_id, addr_id);
    let _: Ipv4AddrConfig<_> = config;
    bindings_ctx.on_event(IpDeviceEvent::AddressRemoved {
        device: device_id.clone(),
        addr: addr_sub.addr(),
        reason: AddressRemovedReason::Manual,
    });
    Ok(())
}

pub(crate) enum DelIpv6Addr<A> {
    SpecifiedAddr(SpecifiedAddr<Ipv6Addr>),
    AddressId(A),
}

fn del_ipv6_addr_with_config<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceContext<BC> + GmpHandler<Ipv6, BC> + DadHandler<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: DelIpv6Addr<CC::AddressId>,
    reason: AddressRemovedReason,
    _config: &Ipv6DeviceConfiguration,
) -> Result<(AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>, Ipv6AddrConfig<BC::Instant>), NotFoundError> {
    let addr_id = match addr {
        DelIpv6Addr::SpecifiedAddr(addr) => core_ctx.get_address_id(device_id, addr)?,
        DelIpv6Addr::AddressId(id) => id,
    };

    DadHandler::stop_duplicate_address_detection(core_ctx, bindings_ctx, device_id, &addr_id);
    let (addr_sub, addr_config) = core_ctx.remove_ip_address(device_id, addr_id);
    let addr = addr_sub.addr();

    bindings_ctx.on_event(IpDeviceEvent::AddressRemoved {
        device: device_id.clone(),
        addr: addr.into_specified(),
        reason,
    });

    Ok((addr_sub, addr_config))
}

/// Removes an IPv6 address and associated subnet from this device.
pub(crate) fn del_ipv6_addr_with_reason<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceConfigurationContext<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: DelIpv6Addr<CC::AddressId>,
    reason: DelIpv6AddrReason,
) -> Result<(), NotFoundError> {
    core_ctx.with_ipv6_device_configuration(device_id, |config, mut core_ctx| {
        del_ipv6_addr_with_reason_with_config(
            &mut core_ctx,
            bindings_ctx,
            device_id,
            addr,
            reason,
            config,
        )
    })
}

/// Removes an IPv6 address and associated subnet from this device.
fn del_ipv6_addr_with_reason_with_config<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceContext<BC> + GmpHandler<Ipv6, BC> + DadHandler<BC> + SlaacHandler<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: DelIpv6Addr<CC::AddressId>,
    reason: DelIpv6AddrReason,
    config: &Ipv6DeviceConfiguration,
) -> Result<(), NotFoundError> {
    del_ipv6_addr_with_config(core_ctx, bindings_ctx, device_id, addr, reason.into(), config).map(
        |(addr_sub, config)| match config {
            Ipv6AddrConfig::Slaac(s) => SlaacHandler::on_address_removed(
                core_ctx,
                bindings_ctx,
                device_id,
                addr_sub,
                s,
                reason,
            ),
            Ipv6AddrConfig::Manual(_manual_config) => (),
        },
    )
}

pub(crate) fn get_ipv4_configuration_and_flags<
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Ipv4DeviceConfigurationAndFlags {
    core_ctx.with_ip_device_configuration(device_id, |config, mut core_ctx| {
        core_ctx.with_ip_device_flags(device_id, |flags| Ipv4DeviceConfigurationAndFlags {
            config: config.clone(),
            flags: flags.clone(),
        })
    })
}

pub(crate) fn get_ipv6_configuration_and_flags<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv6, BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Ipv6DeviceConfigurationAndFlags {
    core_ctx.with_ip_device_configuration(device_id, |config, mut core_ctx| {
        core_ctx.with_ip_device_flags(device_id, |flags| Ipv6DeviceConfigurationAndFlags {
            config: config.clone(),
            flags: flags.clone(),
        })
    })
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

struct Delta<T> {
    prev: T,
    next: T,
}

fn get_prev_next_and_update<T: Copy>(field: &mut T, next: Option<T>) -> Option<Delta<T>> {
    next.map(|next| Delta { prev: core::mem::replace(field, next), next })
}

fn handle_change_and_get_prev<T: PartialEq>(
    delta: Option<Delta<T>>,
    f: impl FnOnce(T),
) -> Option<T> {
    delta.map(|Delta { prev, next }| {
        if prev != next {
            f(next)
        }
        prev
    })
}

fn dont_handle_change_and_get_prev<T: PartialEq>(delta: Option<Delta<T>>) -> Option<T> {
    handle_change_and_get_prev(delta, |_: T| {})
}

/// Errors observed from updating a device's IP configuration.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum UpdateIpConfigurationError {
    /// Forwarding is not supported in the target interface.
    ForwardingNotSupported,
}

/// A validated and pending IPv4 device configuration update.
///
/// This type is a witness for a valid IPv4 configuration for a device ID `D`.
pub struct PendingIpv4DeviceConfigurationUpdate<'a, D>(Ipv4DeviceConfigurationUpdate, &'a D);

// TODO(https://fxbug.dev/42083910): Get rid of this impl block and make the inner
// function publicly accessible once we can safely expose sealed traits.
impl<'a, BC: crate::BindingsContext>
    PendingIpv4DeviceConfigurationUpdate<'a, crate::device::DeviceId<BC>>
{
    /// Applies this pending device configuration.
    pub fn apply(
        self,
        core_ctx: &crate::SyncCtx<BC>,
        bindings_ctx: &mut BC,
    ) -> Ipv4DeviceConfigurationUpdate {
        self.apply_inner(&mut CoreCtx::new_deprecated(core_ctx), bindings_ctx)
    }
}

fn validate_common_ip_config<D: crate::device::Id>(
    device_id: &D,
    config: &IpDeviceConfigurationUpdate,
) -> Result<(), UpdateIpConfigurationError> {
    let IpDeviceConfigurationUpdate { ip_enabled: _, gmp_enabled: _, forwarding_enabled } = config;
    if device_id.is_loopback() {
        if forwarding_enabled.unwrap_or(false) {
            return Err(UpdateIpConfigurationError::ForwardingNotSupported);
        }
    }
    Ok(())
}

impl<'a, D> PendingIpv4DeviceConfigurationUpdate<'a, D>
where
    D: crate::device::Id,
{
    /// Creates a new IPv4 configuration request for the device.
    ///
    /// Each field in [`Ipv4DeviceConfigurationUpdate`] represents an optionally
    /// updateable configuration. If the field has a `Some(_)` value, then an
    /// attempt will be made to update that configuration on the device. A
    /// `None` value indicates that an update for the configuration is not
    /// requested.
    ///
    /// Note that some fields have the type `Option<Option<T>>`. In this case,
    /// as long as the outer `Option` is `Some`, then an attempt will be made to
    /// update the configuration.
    ///
    /// The IP device configuration is only applied when [`apply`] is called on
    /// the returned `Ok` value.
    pub(crate) fn new(
        device_id: &'a D,
        config: Ipv4DeviceConfigurationUpdate,
    ) -> Result<Self, UpdateIpConfigurationError> {
        let Ipv4DeviceConfigurationUpdate { ip_config } = &config;
        ip_config
            .as_ref()
            .map(|ip_config| validate_common_ip_config(device_id, ip_config))
            .unwrap_or(Ok(()))?;
        Ok(Self(config, device_id))
    }

    pub(crate) fn apply_inner<CC, BC>(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
    ) -> Ipv4DeviceConfigurationUpdate
    where
        CC: IpDeviceConfigurationContext<Ipv4, BC, DeviceId = D>,
        BC: IpDeviceBindingsContext<Ipv4, D>,
    {
        let Self(Ipv4DeviceConfigurationUpdate { ip_config }, device_id) = self;
        let device_id: &CC::DeviceId = device_id;
        core_ctx.with_ip_device_configuration_mut(device_id, |mut inner| {
            let ip_config_updates =
                inner.with_configuration_and_flags_mut(device_id, |config, flags| {
                    ip_config.map(
                        |IpDeviceConfigurationUpdate {
                             ip_enabled,
                             gmp_enabled,
                             forwarding_enabled,
                         }| {
                            (
                                get_prev_next_and_update(&mut flags.ip_enabled, ip_enabled),
                                get_prev_next_and_update(
                                    &mut config.ip_config.gmp_enabled,
                                    gmp_enabled,
                                ),
                                get_prev_next_and_update(
                                    &mut config.ip_config.forwarding_enabled,
                                    forwarding_enabled,
                                ),
                            )
                        },
                    )
                });

            let (config, mut core_ctx) = inner.ip_device_configuration_and_ctx();
            let core_ctx = &mut core_ctx;
            Ipv4DeviceConfigurationUpdate {
                ip_config: ip_config_updates.map(
                    |(ip_enabled_updates, gmp_enabled_updates, forwarding_enabled_updates)| {
                        IpDeviceConfigurationUpdate {
                            ip_enabled: handle_change_and_get_prev(ip_enabled_updates, |next| {
                                if next {
                                    enable_ipv4_device_with_config(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                        config,
                                    )
                                } else {
                                    disable_ipv4_device_with_config(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                        config,
                                    )
                                }

                                bindings_ctx.on_event(IpDeviceEvent::EnabledChanged {
                                    device: device_id.clone(),
                                    ip_enabled: next,
                                })
                            }),
                            gmp_enabled: handle_change_and_get_prev(gmp_enabled_updates, |next| {
                                if next {
                                    GmpHandler::gmp_handle_maybe_enabled(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                    )
                                } else {
                                    GmpHandler::gmp_handle_disabled(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                    )
                                }
                            }),
                            forwarding_enabled: dont_handle_change_and_get_prev(
                                forwarding_enabled_updates,
                            ),
                        }
                    },
                ),
            }
        })
    }
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

/// A validated and pending IPv6 device configuration update.
///
/// This type is a witness for a valid IPv6 configuration for a device ID `D`.
pub struct PendingIpv6DeviceConfigurationUpdate<'a, D>(Ipv6DeviceConfigurationUpdate, &'a D);

// TODO(https://fxbug.dev/42083910): Get rid of this impl block and make the inner
// function publicly accessible once we can safely expose sealed traits.
impl<'a, BC: crate::BindingsContext>
    PendingIpv6DeviceConfigurationUpdate<'a, crate::device::DeviceId<BC>>
{
    /// Applies this pending device configuration.
    pub fn apply(
        self,
        core_ctx: &crate::SyncCtx<BC>,
        bindings_ctx: &mut BC,
    ) -> Ipv6DeviceConfigurationUpdate {
        self.apply_inner(&mut CoreCtx::new_deprecated(core_ctx), bindings_ctx)
    }
}

impl<'a, D> PendingIpv6DeviceConfigurationUpdate<'a, D>
where
    D: crate::device::Id,
{
    /// Creates a new IPv6 configuration request for the device.
    ///
    /// Each field in [`Ipv6DeviceConfigurationUpdate`] represents an optionally
    /// updateable configuration. If the field has a `Some(_)` value, then an
    /// attempt will be made to update that configuration on the device. A
    /// `None` value indicates that an update for the configuration is not
    /// requested.
    ///
    /// Note that some fields have the type `Option<Option<T>>`. In this case,
    /// as long as the outer `Option` is `Some`, then an attempt will be made to
    /// update the configuration.
    ///
    /// The IP device configuration is only applied when [`apply`] is called on
    /// the returned `Ok` value.
    pub(crate) fn new(
        device_id: &'a D,
        config: Ipv6DeviceConfigurationUpdate,
    ) -> Result<Self, UpdateIpConfigurationError> {
        let Ipv6DeviceConfigurationUpdate {
            dad_transmits: _,
            max_router_solicitations: _,
            slaac_config: _,
            ip_config,
        } = &config;
        ip_config
            .as_ref()
            .map(|ip_config| validate_common_ip_config(device_id, ip_config))
            .unwrap_or(Ok(()))?;
        Ok(Self(config, device_id))
    }

    pub(crate) fn apply_inner<CC, BC>(
        self,
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
    ) -> Ipv6DeviceConfigurationUpdate
    where
        CC: Ipv6DeviceConfigurationContext<BC, DeviceId = D>,
        BC: IpDeviceBindingsContext<Ipv6, D>,
    {
        let Self(
            Ipv6DeviceConfigurationUpdate {
                dad_transmits,
                max_router_solicitations,
                slaac_config,
                ip_config,
            },
            device_id,
        ) = self;
        let device_id: &CC::DeviceId = device_id;
        core_ctx.with_ipv6_device_configuration_mut(device_id, |mut inner| {
            let (
                dad_transmits_updates,
                max_router_solicitations_updates,
                slaac_config_updates,
                ip_config_updates,
            ) = inner.with_configuration_and_flags_mut(device_id, |config, flags| {
                (
                    get_prev_next_and_update(&mut config.dad_transmits, dad_transmits),
                    get_prev_next_and_update(
                        &mut config.max_router_solicitations,
                        max_router_solicitations,
                    ),
                    get_prev_next_and_update(&mut config.slaac_config, slaac_config),
                    ip_config.map(
                        |IpDeviceConfigurationUpdate {
                             ip_enabled,
                             gmp_enabled,
                             forwarding_enabled,
                         }| {
                            (
                                get_prev_next_and_update(&mut flags.ip_enabled, ip_enabled),
                                get_prev_next_and_update(
                                    &mut config.ip_config.gmp_enabled,
                                    gmp_enabled,
                                ),
                                get_prev_next_and_update(
                                    &mut config.ip_config.forwarding_enabled,
                                    forwarding_enabled,
                                ),
                            )
                        },
                    ),
                )
            });

            let (config, mut core_ctx) = inner.ipv6_device_configuration_and_ctx();
            let core_ctx = &mut core_ctx;
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: dont_handle_change_and_get_prev(dad_transmits_updates),
                max_router_solicitations: dont_handle_change_and_get_prev(
                    max_router_solicitations_updates,
                ),
                slaac_config: dont_handle_change_and_get_prev(slaac_config_updates),
                ip_config: ip_config_updates.map(
                    |(ip_enabled_updates, gmp_enabled_updates, forwarding_enabled_updates)| {
                        IpDeviceConfigurationUpdate {
                            ip_enabled: handle_change_and_get_prev(ip_enabled_updates, |next| {
                                if next {
                                    enable_ipv6_device_with_config(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                        config,
                                    )
                                } else {
                                    disable_ipv6_device_with_config(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                        config,
                                    )
                                }

                                bindings_ctx.on_event(IpDeviceEvent::EnabledChanged {
                                    device: device_id.clone(),
                                    ip_enabled: next,
                                })
                            }),
                            gmp_enabled: handle_change_and_get_prev(gmp_enabled_updates, |next| {
                                if next {
                                    GmpHandler::gmp_handle_maybe_enabled(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                    )
                                } else {
                                    GmpHandler::gmp_handle_disabled(
                                        core_ctx,
                                        bindings_ctx,
                                        device_id,
                                    )
                                }
                            }),
                            forwarding_enabled: handle_change_and_get_prev(
                                forwarding_enabled_updates,
                                |next| {
                                    if next {
                                        RsHandler::stop_router_solicitation(
                                            core_ctx,
                                            bindings_ctx,
                                            device_id,
                                        );
                                        join_ip_multicast_with_config(
                                            core_ctx,
                                            bindings_ctx,
                                            device_id,
                                            Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                                            config,
                                        );
                                    } else {
                                        leave_ip_multicast_with_config(
                                            core_ctx,
                                            bindings_ctx,
                                            device_id,
                                            Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS,
                                            config,
                                        );
                                        RsHandler::start_router_solicitation(
                                            core_ctx,
                                            bindings_ctx,
                                            device_id,
                                        );
                                    }
                                },
                            ),
                        }
                    },
                ),
            }
        })
    }
}

pub(super) fn is_ip_device_enabled<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> bool {
    core_ctx.with_ip_device_flags(device_id, |flags| flags.ip_enabled)
}

/// Removes IPv4 state for the device without emitting events.
pub(crate) fn clear_ipv4_device_state<
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
) {
    core_ctx.with_ip_device_configuration_mut(device_id, |mut core_ctx| {
        let ip_enabled = core_ctx.with_configuration_and_flags_mut(device_id, |_config, flags| {
            // Start by force-disabling IPv4 so we're sure we won't handle
            // any more packets.
            let IpDeviceFlags { ip_enabled } = flags;
            core::mem::replace(ip_enabled, false)
        });

        let (config, mut core_ctx) = core_ctx.ip_device_configuration_and_ctx();
        let core_ctx = &mut core_ctx;
        if ip_enabled {
            disable_ipv4_device_with_config(core_ctx, bindings_ctx, device_id, config);
        }
    })
}

/// Removes IPv6 state for the device without emitting events.
pub(crate) fn clear_ipv6_device_state<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceConfigurationContext<BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
) {
    core_ctx.with_ipv6_device_configuration_mut(device_id, |mut core_ctx| {
        let ip_enabled = core_ctx.with_configuration_and_flags_mut(device_id, |_config, flags| {
            // Start by force-disabling IPv6 so we're sure we won't handle
            // any more packets.
            let IpDeviceFlags { ip_enabled } = flags;
            core::mem::replace(ip_enabled, false)
        });

        let (config, mut core_ctx) = core_ctx.ipv6_device_configuration_and_ctx();
        let core_ctx = &mut core_ctx;
        if ip_enabled {
            disable_ipv6_device_with_config(core_ctx, bindings_ctx, device_id, config);
        }
    })
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use core::fmt::Debug;

    use crate::{
        device::{
            testutil::{update_ipv4_configuration, update_ipv6_configuration},
            DeviceId,
        },
        testutil::{FakeBindingsCtx, FakeCoreCtx},
    };

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
        BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
        CC: Ipv6DeviceContext<BC>,
        O,
        F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<Ipv6Addr>> + '_>) -> O,
    >(
        core_ctx: &mut CC,
        device_id: &CC::DeviceId,
        cb: F,
    ) -> O {
        core_ctx.with_address_ids(device_id, |addrs, core_ctx| {
            cb(Box::new(addrs.filter_map(|addr_id| {
                core_ctx
                    .with_ip_address_state(
                        device_id,
                        &addr_id,
                        |Ipv6AddressState {
                             flags: Ipv6AddressFlags { deprecated: _, assigned },
                             config: _,
                         }| { *assigned },
                    )
                    .then(|| addr_id.addr_sub().to_witness())
            })))
        })
    }

    pub(crate) trait UpdateIpDeviceConfigurationAndFlagsTestIpExt: Ip {
        type IpDeviceConfigurationAndFlags: AsRef<IpDeviceConfiguration>
            + AsRef<IpDeviceFlags>
            + AsMut<IpDeviceConfiguration>
            + PartialEq
            + Debug;

        fn get_ip_configuration_and_flags(
            core_ctx: &FakeCoreCtx,
            device: &DeviceId<FakeBindingsCtx>,
        ) -> Self::IpDeviceConfigurationAndFlags;

        fn update_ip_configuration(
            core_ctx: &FakeCoreCtx,
            bindings_ctx: &mut FakeBindingsCtx,
            device: &DeviceId<FakeBindingsCtx>,
            config: IpDeviceConfigurationUpdate,
        ) -> Result<IpDeviceConfigurationUpdate, UpdateIpConfigurationError>;

        fn set_ip_device_enabled(
            core_ctx: &FakeCoreCtx,
            bindings_ctx: &mut FakeBindingsCtx,
            device: &DeviceId<FakeBindingsCtx>,
            enabled: bool,
            prev_enabled: bool,
        ) {
            let make_update = |enabled| IpDeviceConfigurationUpdate {
                ip_enabled: Some(enabled),
                ..Default::default()
            };

            assert_eq!(
                Self::update_ip_configuration(core_ctx, bindings_ctx, device, make_update(enabled)),
                Ok(make_update(prev_enabled)),
            );
        }
    }

    impl UpdateIpDeviceConfigurationAndFlagsTestIpExt for Ipv4 {
        type IpDeviceConfigurationAndFlags = Ipv4DeviceConfigurationAndFlags;

        fn get_ip_configuration_and_flags(
            core_ctx: &FakeCoreCtx,
            device: &DeviceId<FakeBindingsCtx>,
        ) -> Self::IpDeviceConfigurationAndFlags {
            crate::device::get_ipv4_configuration_and_flags(core_ctx, device)
        }

        fn update_ip_configuration(
            core_ctx: &FakeCoreCtx,
            bindings_ctx: &mut FakeBindingsCtx,
            device: &DeviceId<FakeBindingsCtx>,
            config: IpDeviceConfigurationUpdate,
        ) -> Result<IpDeviceConfigurationUpdate, UpdateIpConfigurationError> {
            update_ipv4_configuration(
                core_ctx,
                bindings_ctx,
                device,
                Ipv4DeviceConfigurationUpdate { ip_config: Some(config), ..Default::default() },
            )
            .map(|Ipv4DeviceConfigurationUpdate { ip_config }| ip_config.unwrap())
        }
    }

    impl UpdateIpDeviceConfigurationAndFlagsTestIpExt for Ipv6 {
        type IpDeviceConfigurationAndFlags = Ipv6DeviceConfigurationAndFlags;

        fn get_ip_configuration_and_flags(
            core_ctx: &FakeCoreCtx,
            device: &DeviceId<FakeBindingsCtx>,
        ) -> Self::IpDeviceConfigurationAndFlags {
            crate::device::get_ipv6_configuration_and_flags(core_ctx, device)
        }

        fn update_ip_configuration(
            core_ctx: &FakeCoreCtx,
            bindings_ctx: &mut FakeBindingsCtx,
            device: &DeviceId<FakeBindingsCtx>,
            config: IpDeviceConfigurationUpdate,
        ) -> Result<IpDeviceConfigurationUpdate, UpdateIpConfigurationError> {
            update_ipv6_configuration(
                core_ctx,
                bindings_ctx,
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
    use core::time::Duration;

    use assert_matches::assert_matches;
    use fakealloc::collections::HashSet;
    use ip_test_macro::ip_test;
    use test_case::test_case;

    use net_declare::{net_ip_v4, net_ip_v6, net_mac};
    use net_types::{ip::Ipv6, LinkLocalAddr};

    use crate::{
        context::testutil::FakeInstant,
        device::{
            ethernet::MaxEthernetFrameSize,
            testutil::{update_ipv4_configuration, update_ipv6_configuration},
            DeviceId,
        },
        ip::{
            device::{
                nud::{LinkResolutionResult, NudHandler},
                state::Lifetime,
                testutil::UpdateIpDeviceConfigurationAndFlagsTestIpExt,
            },
            gmp::GmpDelayedReportTimerId,
        },
        state::StackStateBuilder,
        testutil::{
            assert_empty, Ctx, DispatchedEvent, FakeBindingsCtx, FakeCoreCtx, FakeCtx,
            TestIpExt as _, DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        time::TimerIdInner,
        TimerId,
    };

    #[test]
    fn enable_disable_ipv4() {
        let FakeCtx { core_ctx, mut bindings_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        let core_ctx = &core_ctx;
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv4::FAKE_CONFIG.local_mac;
        let ethernet_device_id = crate::device::add_ethernet_device(
            core_ctx,
            local_mac,
            MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        );
        let device_id = ethernet_device_id.clone().into();

        assert_eq!(bindings_ctx.take_events()[..], []);

        let set_ipv4_enabled =
            |ctx: &mut crate::testutil::FakeBindingsCtx, enabled, expected_prev| {
                Ipv4::set_ip_device_enabled(core_ctx, ctx, &device_id, enabled, expected_prev)
            };

        set_ipv4_enabled(&mut bindings_ctx, true, false);

        let weak_device_id = device_id.downgrade();
        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            })]
        );

        let addr = SpecifiedAddr::new(net_ip_v4!("192.0.2.1")).expect("addr should be unspecified");
        let mac = net_mac!("01:23:45:67:89:ab");
        NudHandler::<Ipv4, _, _>::set_static_neighbor(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &ethernet_device_id,
            addr,
            mac,
        );
        assert_eq!(
            bindings_ctx.take_events(),
            [DispatchedEvent::NeighborIpv4(nud::Event {
                device: ethernet_device_id.downgrade(),
                addr,
                kind: nud::EventKind::Added(nud::EventState::Static(mac)),
                at: bindings_ctx.now(),
            })]
        );
        assert_matches!(
            NudHandler::<Ipv4, _, _>::resolve_link_addr(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &ethernet_device_id,
                &addr,
            ),
            LinkResolutionResult::Resolved(got) => assert_eq!(got, mac)
        );

        set_ipv4_enabled(&mut bindings_ctx, false, true);
        assert_eq!(
            bindings_ctx.take_events(),
            [
                DispatchedEvent::NeighborIpv4(nud::Event {
                    device: ethernet_device_id.downgrade(),
                    addr,
                    kind: nud::EventKind::Removed,
                    at: bindings_ctx.now(),
                }),
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: false,
                })
            ]
        );

        // Assert that static ARP entries are flushed on link down.
        nud::testutil::assert_neighbor_unknown::<Ipv4, _, _, _>(
            &mut CoreCtx::new_deprecated(core_ctx),
            ethernet_device_id,
            addr,
        );

        let ipv4_addr_subnet = AddrSubnet::new(Ipv4Addr::new([192, 168, 0, 1]), 24).unwrap();
        add_ipv4_addr_subnet(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            ipv4_addr_subnet.clone(),
            Ipv4AddrConfig::default(),
        )
        .expect("failed to add IPv4 Address");
        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.clone(),
                state: IpAddressState::Unavailable,
                valid_until: Lifetime::Infinite,
            })]
        );

        set_ipv4_enabled(&mut bindings_ctx, true, false);
        assert_eq!(
            bindings_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ipv4_addr_subnet.addr(),
                    state: IpAddressState::Assigned,
                }),
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                }),
            ]
        );
        // Verify that a redundant "enable" does not generate any events.
        set_ipv4_enabled(&mut bindings_ctx, true, true);
        assert_eq!(bindings_ctx.take_events()[..], []);

        let valid_until = Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)));
        set_ipv4_addr_properties(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            ipv4_addr_subnet.addr(),
            valid_until,
        )
        .expect("set properties should succeed");
        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressPropertiesChanged {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.addr(),
                valid_until
            })]
        );

        // Verify that a redundant "set properties" does not generate any events.
        set_ipv4_addr_properties(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            ipv4_addr_subnet.addr(),
            valid_until,
        )
        .expect("set properties should succeed");
        assert_eq!(bindings_ctx.take_events()[..], []);

        set_ipv4_enabled(&mut bindings_ctx, false, true);
        assert_eq!(
            bindings_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ipv4_addr_subnet.addr(),
                    state: IpAddressState::Unavailable,
                }),
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id,
                    ip_enabled: false,
                }),
            ]
        );
        // Verify that a redundant "disable" does not generate any events.
        set_ipv4_enabled(&mut bindings_ctx, false, false);
        assert_eq!(bindings_ctx.take_events()[..], []);
    }

    fn enable_ipv6_device(
        core_ctx: &mut &FakeCoreCtx,
        bindings_ctx: &mut FakeBindingsCtx,
        device_id: &DeviceId<FakeBindingsCtx>,
        ll_addr: AddrSubnet<Ipv6Addr, LinkLocalAddr<UnicastAddr<Ipv6Addr>>>,
        expected_prev: bool,
    ) {
        Ipv6::set_ip_device_enabled(core_ctx, bindings_ctx, device_id, true, expected_prev);

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut CoreCtx::new_deprecated(*core_ctx),
                device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "enabled device expected to generate link-local address"
        );
    }

    #[test]
    fn enable_disable_ipv6() {
        let FakeCtx { core_ctx, mut bindings_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        let mut core_ctx = &core_ctx;
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let ethernet_device_id = crate::device::add_ethernet_device(
            core_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        );
        let device_id = ethernet_device_id.clone().into();
        let ll_addr = local_mac.to_ipv6_link_local();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            core_ctx,
            &mut bindings_ctx,
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                // Doesn't matter as long as we perform DAD and router
                // solicitation.
                dad_transmits: Some(NonZeroU8::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(1)),
                // Auto-generate a link-local address.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    gmp_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(bindings_ctx.take_events()[..], []);

        // Enable the device and observe an auto-generated link-local address,
        // router solicitation and DAD for the auto-generated address.
        let test_enable_device = |core_ctx: &mut &FakeCoreCtx,
                                  bindings_ctx: &mut FakeBindingsCtx,
                                  extra_group,
                                  expected_prev| {
            enable_ipv6_device(core_ctx, bindings_ctx, &device_id, ll_addr, expected_prev);
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
                        }),
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
                        }),
                    ))),
                    ..,
                ))
            }
            bindings_ctx.timer_ctx().assert_timers_installed(timers);
        };
        test_enable_device(&mut core_ctx, &mut bindings_ctx, None, false);
        let weak_device_id = device_id.downgrade();
        assert_eq!(
            bindings_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                    device: weak_device_id.clone(),
                    addr: ll_addr.to_witness(),
                    state: IpAddressState::Tentative,
                    valid_until: Lifetime::Infinite,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                })
            ]
        );

        // Because the added address is from SLAAC, setting its lifetime should fail.
        let valid_until = Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)));
        assert_matches!(
            set_ipv6_addr_properties(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &device_id,
                ll_addr.addr().into(),
                valid_until,
            ),
            Err(SetIpAddressPropertiesError::NotManual)
        );

        let addr =
            SpecifiedAddr::new(net_ip_v6!("2001:db8::1")).expect("addr should be unspecified");
        let mac = net_mac!("01:23:45:67:89:ab");
        NudHandler::<Ipv6, _, _>::set_static_neighbor(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &ethernet_device_id,
            addr,
            mac,
        );
        assert_eq!(
            bindings_ctx.take_events(),
            [DispatchedEvent::NeighborIpv6(nud::Event {
                device: ethernet_device_id.downgrade(),
                addr,
                kind: nud::EventKind::Added(nud::EventState::Static(mac)),
                at: bindings_ctx.now(),
            })]
        );
        assert_matches!(
            NudHandler::<Ipv6, _, _>::resolve_link_addr(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &ethernet_device_id,
                &addr,
            ),
            LinkResolutionResult::Resolved(got) => assert_eq!(got, mac)
        );

        let test_disable_device = |core_ctx: &mut &FakeCoreCtx,
                                   bindings_ctx: &mut FakeBindingsCtx,
                                   expected_prev| {
            Ipv6::set_ip_device_enabled(core_ctx, bindings_ctx, &device_id, false, expected_prev);
            bindings_ctx.timer_ctx().assert_no_timers_installed();
        };
        test_disable_device(&mut core_ctx, &mut bindings_ctx, true);
        assert_eq!(
            bindings_ctx.take_events(),
            [
                DispatchedEvent::NeighborIpv6(nud::Event {
                    device: ethernet_device_id.downgrade(),
                    addr,
                    kind: nud::EventKind::Removed,
                    at: bindings_ctx.now(),
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
                    device: weak_device_id.clone(),
                    addr: ll_addr.addr().into(),
                    reason: AddressRemovedReason::Manual,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: false,
                })
            ]
        );

        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            &mut CoreCtx::new_deprecated(core_ctx),
            &device_id,
            |addrs, _core_ctx| {
                assert_empty(addrs);
            },
        );

        // Assert that static NDP entry was removed on link down.
        nud::testutil::assert_neighbor_unknown::<Ipv6, _, _, _>(
            &mut CoreCtx::new_deprecated(core_ctx),
            ethernet_device_id,
            addr,
        );

        let multicast_addr = Ipv6::ALL_ROUTERS_LINK_LOCAL_MULTICAST_ADDRESS;
        join_ip_multicast::<Ipv6, _, _>(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            multicast_addr,
        );
        add_ipv6_addr_subnet(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            ll_addr.to_unicast().add_witness::<NonMappedAddr<_>>().unwrap(),
            Ipv6AddrManualConfig::default(),
        )
        .expect("add MAC based IPv6 link-local address");
        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut CoreCtx::new_deprecated(core_ctx),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()])
        );
        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ll_addr.to_witness(),
                state: IpAddressState::Unavailable,
                valid_until: Lifetime::Infinite,
            })]
        );

        test_enable_device(&mut core_ctx, &mut bindings_ctx, Some(multicast_addr), false);
        assert_eq!(
            bindings_ctx.take_events()[..],
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

        set_ipv6_addr_properties(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            ll_addr.addr().into(),
            valid_until,
        )
        .expect("set properties should succeed");
        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressPropertiesChanged {
                device: weak_device_id.clone(),
                addr: ll_addr.addr().into(),
                valid_until
            })]
        );

        // Verify that a redundant "set properties" does not generate any events.
        set_ipv6_addr_properties(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            ll_addr.addr().into(),
            valid_until,
        )
        .expect("set properties should succeed");
        assert_eq!(bindings_ctx.take_events()[..], []);

        test_disable_device(&mut core_ctx, &mut bindings_ctx, true);
        // The address was manually added, don't expect it to be removed.
        assert_eq!(
            bindings_ctx.take_events()[..],
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
        test_disable_device(&mut core_ctx, &mut bindings_ctx, false);
        assert_eq!(bindings_ctx.take_events()[..], []);

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut CoreCtx::new_deprecated(core_ctx),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "manual addresses should not be removed on device disable"
        );

        leave_ip_multicast::<Ipv6, _, _>(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            multicast_addr,
        );
        test_enable_device(&mut core_ctx, &mut bindings_ctx, None, false);
        assert_eq!(
            bindings_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressStateChanged {
                    device: weak_device_id.clone(),
                    addr: ll_addr.addr().into(),
                    state: IpAddressState::Tentative,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id,
                    ip_enabled: true,
                })
            ]
        );

        // Verify that a redundant "enable" does not generate any events.
        test_enable_device(&mut core_ctx, &mut bindings_ctx, None, true);
        assert_eq!(bindings_ctx.take_events()[..], []);
    }

    #[test]
    fn notify_on_dad_failure_ipv6() {
        let FakeCtx { core_ctx, mut bindings_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        let mut core_ctx = &core_ctx;
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            core_ctx,
            local_mac,
            IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
            DEFAULT_INTERFACE_METRIC,
        )
        .into();
        let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
            core_ctx,
            &mut bindings_ctx,
            &device_id,
            Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(NonZeroU8::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(1)),
                // Auto-generate a link-local address.
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    ..Default::default()
                }),
                ip_config: Some(IpDeviceConfigurationUpdate {
                    gmp_enabled: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let ll_addr = local_mac.to_ipv6_link_local();

        enable_ipv6_device(&mut core_ctx, &mut bindings_ctx, &device_id, ll_addr, false);
        let weak_device_id = device_id.downgrade();
        assert_eq!(
            bindings_ctx.take_events()[..],
            [
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                    device: weak_device_id.clone(),
                    addr: ll_addr.to_witness(),
                    state: IpAddressState::Tentative,
                    valid_until: Lifetime::Infinite,
                }),
                DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: true,
                }),
            ]
        );

        let assigned_addr = AddrSubnet::new(net_ip_v6!("fe80::1"), 64).unwrap();
        add_ipv6_addr_subnet(
            &mut CoreCtx::new_deprecated(core_ctx),
            &mut bindings_ctx,
            &device_id,
            assigned_addr,
            Ipv6AddrManualConfig::default(),
        )
        .expect("add succeeds");

        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: assigned_addr.to_witness(),
                state: IpAddressState::Tentative,
                valid_until: Lifetime::Infinite,
            }),]
        );

        // When DAD fails, an event should be emitted and the address should be
        // removed.
        assert_eq!(
            Ipv6DeviceHandler::remove_duplicate_tentative_address(
                &mut CoreCtx::new_deprecated(core_ctx),
                &mut bindings_ctx,
                &device_id,
                assigned_addr.ipv6_unicast_addr()
            ),
            IpAddressState::Tentative,
        );

        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
                device: weak_device_id,
                addr: assigned_addr.addr().into(),
                reason: AddressRemovedReason::DadFailed,
            }),]
        );

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut CoreCtx::new_deprecated(core_ctx),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "manual addresses should be removed on DAD failure"
        );
    }

    #[ip_test]
    fn update_ip_device_configuration_err<I: Ip + UpdateIpDeviceConfigurationAndFlagsTestIpExt>() {
        let FakeCtx { core_ctx, mut bindings_ctx } = FakeCtx::default();
        let core_ctx = &core_ctx;

        let loopback_device_id = crate::device::add_loopback_device(
            core_ctx,
            Mtu::new(u16::MAX as u32),
            DEFAULT_INTERFACE_METRIC,
        )
        .expect("create the loopback interface")
        .into();

        let original_state = I::get_ip_configuration_and_flags(core_ctx, &loopback_device_id);
        assert_eq!(
            I::update_ip_configuration(
                core_ctx,
                &mut bindings_ctx,
                &loopback_device_id,
                IpDeviceConfigurationUpdate {
                    ip_enabled: Some(!AsRef::<IpDeviceFlags>::as_ref(&original_state).ip_enabled),
                    gmp_enabled: Some(
                        !AsRef::<IpDeviceConfiguration>::as_ref(&original_state).gmp_enabled
                    ),
                    forwarding_enabled: Some(true),
                },
            ),
            Err(UpdateIpConfigurationError::ForwardingNotSupported),
        );
        assert_eq!(
            original_state,
            I::get_ip_configuration_and_flags(core_ctx, &loopback_device_id)
        );
    }

    #[test]
    fn update_ipv4_configuration_return() {
        let FakeCtx { core_ctx, mut bindings_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv4::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            &core_ctx,
            local_mac,
            MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        // Perform no update.
        assert_eq!(
            update_ipv4_configuration(
                &core_ctx,
                &mut bindings_ctx,
                &device_id,
                Ipv4DeviceConfigurationUpdate::default()
            ),
            Ok(Ipv4DeviceConfigurationUpdate::default()),
        );

        // Enable all but forwarding. All features are initially disabled.
        assert_eq!(
            update_ipv4_configuration(
                &core_ctx,
                &mut bindings_ctx,
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
                &core_ctx,
                &mut bindings_ctx,
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
                &core_ctx,
                &mut bindings_ctx,
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
                &core_ctx,
                &mut bindings_ctx,
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
        let FakeCtx { core_ctx, mut bindings_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            &core_ctx,
            local_mac,
            MaxEthernetFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        // Perform no update.
        assert_eq!(
            update_ipv6_configuration(
                &core_ctx,
                &mut bindings_ctx,
                &device_id,
                Ipv6DeviceConfigurationUpdate::default()
            ),
            Ok(Ipv6DeviceConfigurationUpdate::default()),
        );

        // Enable all but forwarding. All features are initially disabled.
        assert_eq!(
            update_ipv6_configuration(
                &core_ctx,
                &mut bindings_ctx,
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
                &core_ctx,
                &mut bindings_ctx,
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
                &core_ctx,
                &mut bindings_ctx,
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
                &core_ctx,
                &mut bindings_ctx,
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

    #[test_case(false; "stable addresses enabled generates link local")]
    #[test_case(true; "stable addresses disabled does not generate link local")]
    fn configure_link_local_address_generation(enable_stable_addresses: bool) {
        let FakeCtx { core_ctx, mut bindings_ctx } =
            Ctx::new_with_builder(StackStateBuilder::default());
        bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = crate::device::add_ethernet_device(
            &core_ctx,
            local_mac,
            MaxEthernetFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU).unwrap(),
            DEFAULT_INTERFACE_METRIC,
        )
        .into();

        let new_config = Ipv6DeviceConfigurationUpdate {
            slaac_config: Some(SlaacConfiguration {
                enable_stable_addresses,
                ..Default::default()
            }),
            ..Default::default()
        };
        let _prev = update_ipv6_configuration(&core_ctx, &mut bindings_ctx, &device_id, new_config);
        Ipv6::set_ip_device_enabled(&core_ctx, &mut bindings_ctx, &device_id, true, false);

        let expected_addrs = if enable_stable_addresses {
            HashSet::from([local_mac.to_ipv6_link_local().ipv6_unicast_addr()])
        } else {
            HashSet::new()
        };
        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut CoreCtx::new_deprecated(&core_ctx),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            expected_addrs,
        );
    }
}
