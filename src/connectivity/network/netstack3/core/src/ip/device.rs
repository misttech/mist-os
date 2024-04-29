// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An IP device.

pub(crate) mod api;
pub(crate) mod config;
pub(crate) mod dad;
pub(crate) mod integration;
pub(crate) mod nud;
pub(crate) mod route_discovery;
pub(crate) mod router_solicitation;
pub(crate) mod slaac;
pub(crate) mod state;

use alloc::{boxed::Box, vec::Vec};
use core::{
    fmt::{Debug, Display},
    num::NonZeroU8,
};

use net_types::{
    ip::{
        AddrSubnet, GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr,
        Mtu, Subnet,
    },
    MulticastAddr, NonMappedAddr, SpecifiedAddr, UnicastAddr, Witness,
};
use packet::{BufferMut, Serializer};
use packet_formats::{
    icmp::{mld::MldPacket, ndp::NonZeroNdpLifetime},
    utils::NonZeroDuration,
};
use tracing::info;
use zerocopy::ByteSlice;

use crate::{
    context::{
        EventContext, InstantBindingsTypes, InstantContext, RngContext, TimerContext, TimerHandler,
    },
    device::{self, AnyDevice, DeviceIdContext, WeakId as _},
    error::{ExistsError, NotFoundError},
    filter::{IpPacket, ProofOfEgressCheck},
    inspect::Inspectable,
    ip::{
        device::{
            config::{
                IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate,
                Ipv6DeviceConfigurationUpdate,
            },
            dad::{DadEvent, DadHandler, DadTimerId},
            nud::NudIpHandler,
            route_discovery::{
                Ipv6DiscoveredRoute, Ipv6DiscoveredRouteTimerId, RouteDiscoveryHandler,
            },
            router_solicitation::{RsHandler, RsTimerId},
            slaac::{SlaacHandler, SlaacTimerId},
            state::{
                IpDeviceConfiguration, IpDeviceFlags, IpDeviceState, IpDeviceStateBindingsTypes,
                IpDeviceStateIpExt, Ipv4AddrConfig, Ipv4AddressState, Ipv4DeviceConfiguration,
                Ipv4DeviceConfigurationAndFlags, Ipv4DeviceState, Ipv6AddrConfig,
                Ipv6AddrManualConfig, Ipv6AddressFlags, Ipv6AddressState, Ipv6DeviceConfiguration,
                Ipv6DeviceConfigurationAndFlags, Ipv6DeviceState, Lifetime,
            },
        },
        gmp::{
            igmp::{IgmpPacketHandler, IgmpTimerId},
            mld::{MldPacketHandler, MldTimerId},
            GmpHandler, GmpQueryHandler, GroupJoinResult, GroupLeaveResult,
        },
        types::IpTypesIpExt,
    },
    socket::address::SocketIpAddr,
    Instant,
};

use self::state::Ipv6NetworkLearnedParameters;

/// An IP device timer.
///
/// This timer is an indirection to the real types defined by the
/// [`IpDeviceIpExt`] trait. Having a concrete type parameterized over IP allows
/// us to provide implementations generic on I for outer timer contexts that
/// handle `IpDeviceTimerId` timers.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpDeviceTimerId<I: IpDeviceIpExt, D: device::StrongId>(I::Timer<D>);

/// A timer ID for IPv4 devices.
// TODO(https://fxbug.dev/42083407): Change to a weak device id bound once all
// internal timers are transitioned.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct Ipv4DeviceTimerId<D: device::StrongId>(IgmpTimerId<D::Weak>);

impl<D: device::StrongId> Ipv4DeviceTimerId<D> {
    /// Gets the device ID from this timer IFF the device hasn't been destroyed.
    fn device_id(&self) -> Option<D> {
        let Self(this) = self;
        this.device_id().upgrade()
    }
}

impl<D: device::StrongId> From<IpDeviceTimerId<Ipv4, D>> for Ipv4DeviceTimerId<D> {
    fn from(IpDeviceTimerId(inner): IpDeviceTimerId<Ipv4, D>) -> Self {
        inner
    }
}

impl<D: device::StrongId> From<Ipv4DeviceTimerId<D>> for IpDeviceTimerId<Ipv4, D> {
    fn from(value: Ipv4DeviceTimerId<D>) -> Self {
        Self(value)
    }
}

impl<D: device::StrongId> From<IgmpTimerId<D::Weak>> for Ipv4DeviceTimerId<D> {
    fn from(id: IgmpTimerId<D::Weak>) -> Ipv4DeviceTimerId<D> {
        Ipv4DeviceTimerId(id)
    }
}

impl<D: device::StrongId, BC, CC: TimerHandler<BC, IgmpTimerId<D::Weak>>>
    TimerHandler<BC, Ipv4DeviceTimerId<D>> for CC
{
    fn handle_timer(&mut self, bindings_ctx: &mut BC, Ipv4DeviceTimerId(id): Ipv4DeviceTimerId<D>) {
        TimerHandler::handle_timer(self, bindings_ctx, id)
    }
}

impl<CC, BC> TimerHandler<BC, IpDeviceTimerId<Ipv4, CC::DeviceId>> for CC
where
    BC: IpDeviceBindingsContext<Ipv4, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv4, BC>,
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        IpDeviceTimerId(id): IpDeviceTimerId<Ipv4, CC::DeviceId>,
    ) {
        let Some(device_id) = id.device_id() else {
            return;
        };
        self.with_ip_device_configuration(&device_id, |_state, mut core_ctx| {
            TimerHandler::handle_timer(&mut core_ctx, bindings_ctx, id)
        })
    }
}

/// A timer ID for IPv6 devices.
// TODO(https://fxbug.dev/42083407): Change to a weak device id bound once all
// internal timers are transitioned.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub enum Ipv6DeviceTimerId<D: device::StrongId> {
    Mld(MldTimerId<D::Weak>),
    Dad(DadTimerId<D::Weak>),
    Rs(RsTimerId<D::Weak>),
    RouteDiscovery(Ipv6DiscoveredRouteTimerId<D::Weak>),
    Slaac(SlaacTimerId<D::Weak>),
}

impl<D: device::StrongId> From<IpDeviceTimerId<Ipv6, D>> for Ipv6DeviceTimerId<D> {
    fn from(IpDeviceTimerId(inner): IpDeviceTimerId<Ipv6, D>) -> Self {
        inner
    }
}

impl<D: device::StrongId> From<Ipv6DeviceTimerId<D>> for IpDeviceTimerId<Ipv6, D> {
    fn from(value: Ipv6DeviceTimerId<D>) -> Self {
        Self(value)
    }
}

impl<D: device::StrongId> Ipv6DeviceTimerId<D> {
    /// Gets the device ID from this timer IFF the device hasn't been destroyed.
    fn device_id(&self) -> Option<D> {
        match self {
            Self::Mld(id) => id.device_id(),
            Self::Dad(id) => id.device_id(),
            Self::Rs(id) => id.device_id(),
            Self::RouteDiscovery(id) => id.device_id(),
            Self::Slaac(id) => id.device_id(),
        }
        .upgrade()
    }
}

impl<D: device::StrongId> From<MldTimerId<D::Weak>> for Ipv6DeviceTimerId<D> {
    fn from(id: MldTimerId<D::Weak>) -> Ipv6DeviceTimerId<D> {
        Ipv6DeviceTimerId::Mld(id)
    }
}

impl<D: device::StrongId> From<DadTimerId<D::Weak>> for Ipv6DeviceTimerId<D> {
    fn from(id: DadTimerId<D::Weak>) -> Ipv6DeviceTimerId<D> {
        Ipv6DeviceTimerId::Dad(id)
    }
}

impl<D: device::StrongId> From<RsTimerId<D::Weak>> for Ipv6DeviceTimerId<D> {
    fn from(id: RsTimerId<D::Weak>) -> Ipv6DeviceTimerId<D> {
        Ipv6DeviceTimerId::Rs(id)
    }
}

impl<D: device::StrongId> From<Ipv6DiscoveredRouteTimerId<D::Weak>> for Ipv6DeviceTimerId<D> {
    fn from(id: Ipv6DiscoveredRouteTimerId<D::Weak>) -> Ipv6DeviceTimerId<D> {
        Ipv6DeviceTimerId::RouteDiscovery(id)
    }
}

impl<D: device::StrongId> From<SlaacTimerId<D::Weak>> for Ipv6DeviceTimerId<D> {
    fn from(id: SlaacTimerId<D::Weak>) -> Ipv6DeviceTimerId<D> {
        Ipv6DeviceTimerId::Slaac(id)
    }
}

impl<
        D: device::StrongId,
        BC,
        CC: TimerHandler<BC, RsTimerId<D::Weak>>
            + TimerHandler<BC, Ipv6DiscoveredRouteTimerId<D::Weak>>
            + TimerHandler<BC, MldTimerId<D::Weak>>
            + TimerHandler<BC, SlaacTimerId<D::Weak>>
            + TimerHandler<BC, DadTimerId<D::Weak>>,
    > TimerHandler<BC, Ipv6DeviceTimerId<D>> for CC
{
    fn handle_timer(&mut self, bindings_ctx: &mut BC, id: Ipv6DeviceTimerId<D>) {
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

impl<CC, BC> TimerHandler<BC, IpDeviceTimerId<Ipv6, CC::DeviceId>> for CC
where
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<Ipv6, BC>,
{
    fn handle_timer(
        &mut self,
        bindings_ctx: &mut BC,
        IpDeviceTimerId(id): IpDeviceTimerId<Ipv6, CC::DeviceId>,
    ) {
        let Some(device_id) = id.device_id() else {
            return;
        };
        self.with_ip_device_configuration(&device_id, |_state, mut core_ctx| {
            TimerHandler::handle_timer(&mut core_ctx, bindings_ctx, id)
        })
    }
}

/// An extension trait adding IP device properties.
pub trait IpDeviceIpExt: IpDeviceStateIpExt {
    type State<BT: IpDeviceStateBindingsTypes>: AsRef<IpDeviceState<Self, BT>>
        + AsMut<IpDeviceState<Self, BT>>;
    type Configuration: AsRef<IpDeviceConfiguration> + Clone;
    type Timer<D: device::StrongId>: Into<IpDeviceTimerId<Self, D>> + From<IpDeviceTimerId<Self, D>>;
    type AssignedWitness: Witness<Self::Addr>
        + Copy
        + PartialEq
        + Debug
        + Display
        + Into<SpecifiedAddr<Self::Addr>>;
    type AddressConfig<I: Instant>: Default + Debug;
    type ManualAddressConfig<I: Instant>: Default + Debug + Into<Self::AddressConfig<I>>;
    type AddressState<I: Instant>: 'static + Inspectable;
    type ConfigurationUpdate: From<IpDeviceConfigurationUpdate>
        + AsRef<IpDeviceConfigurationUpdate>
        + Debug;
    type ConfigurationAndFlags: From<(Self::Configuration, IpDeviceFlags)>
        + AsRef<IpDeviceConfiguration>
        + AsRef<IpDeviceFlags>
        + AsMut<IpDeviceConfiguration>
        + PartialEq
        + Debug;

    fn get_valid_until<I: Instant>(config: &Self::AddressConfig<I>) -> Lifetime<I>;

    fn is_addr_assigned<I: Instant>(addr_state: &Self::AddressState<I>) -> bool;
}

impl IpDeviceIpExt for Ipv4 {
    type State<BT: IpDeviceStateBindingsTypes> = Ipv4DeviceState<BT>;
    type Configuration = Ipv4DeviceConfiguration;
    type Timer<D: device::StrongId> = Ipv4DeviceTimerId<D>;
    type AssignedWitness = SpecifiedAddr<Ipv4Addr>;
    type AddressConfig<I: Instant> = Ipv4AddrConfig<I>;
    type ManualAddressConfig<I: Instant> = Ipv4AddrConfig<I>;
    type AddressState<I: Instant> = Ipv4AddressState<I>;
    type ConfigurationUpdate = Ipv4DeviceConfigurationUpdate;
    type ConfigurationAndFlags = Ipv4DeviceConfigurationAndFlags;

    fn get_valid_until<I: Instant>(config: &Self::AddressConfig<I>) -> Lifetime<I> {
        config.valid_until
    }

    fn is_addr_assigned<I: Instant>(addr_state: &Ipv4AddressState<I>) -> bool {
        let Ipv4AddressState { config: _ } = addr_state;
        true
    }
}

impl IpDeviceIpExt for Ipv6 {
    type State<BT: IpDeviceStateBindingsTypes> = Ipv6DeviceState<BT>;
    type Configuration = Ipv6DeviceConfiguration;
    type Timer<D: device::StrongId> = Ipv6DeviceTimerId<D>;
    type AssignedWitness = Ipv6DeviceAddr;
    type AddressConfig<I: Instant> = Ipv6AddrConfig<I>;
    type ManualAddressConfig<I: Instant> = Ipv6AddrManualConfig<I>;
    type AddressState<I: Instant> = Ipv6AddressState<I>;
    type ConfigurationUpdate = Ipv6DeviceConfigurationUpdate;
    type ConfigurationAndFlags = Ipv6DeviceConfigurationAndFlags;

    fn get_valid_until<I: Instant>(config: &Self::AddressConfig<I>) -> Lifetime<I> {
        config.valid_until()
    }

    fn is_addr_assigned<I: Instant>(addr_state: &Ipv6AddressState<I>) -> bool {
        addr_state.flags.assigned
    }
}

/// An Ip address that witnesses properties needed to be assigned to a device.
pub(crate) type IpDeviceAddr<A> = SocketIpAddr<A>;

/// An IPv6 address that witnesses properties needed to be assigned to a device.
///
/// Like [`IpDeviceAddr`] but with stricter witnesses that are permitted for
/// IPv6 addresses.
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

#[derive(Debug, Eq, Hash, PartialEq, GenericOverIp)]
#[generic_over_ip(I, Ip)]
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
        BC: InstantBindingsTypes + EventContext<IpDeviceEvent<DeviceId, Ipv6, BC::Instant>>,
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

/// The bindings execution context for IP devices.
pub trait IpDeviceBindingsContext<I: IpDeviceIpExt, D: device::StrongId>:
    RngContext
    + TimerContext<IpDeviceTimerId<I, D>>
    + EventContext<IpDeviceEvent<D, I, <Self as InstantBindingsTypes>::Instant>>
{
}
impl<
        D: device::StrongId,
        I: IpDeviceIpExt,
        BC: RngContext
            + TimerContext<IpDeviceTimerId<I, D>>
            + EventContext<IpDeviceEvent<D, I, <Self as InstantBindingsTypes>::Instant>>,
    > IpDeviceBindingsContext<I, D> for BC
{
}

/// An IP address ID.
pub trait IpAddressId<A: IpAddress>: Clone + Debug {
    /// Returns the address this ID represents.
    fn addr(&self) -> IpDeviceAddr<A>;

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
    fn addr(&self) -> IpDeviceAddr<A> {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct WrapIn<I: IpDeviceIpExt>(I::AssignedWitness);
        A::Version::map_ip(
            WrapIn(self.addr()),
            |WrapIn(v4_addr)| IpDeviceAddr::new_ipv4_specified(v4_addr),
            |WrapIn(v6_addr)| IpDeviceAddr::new_from_ipv6_device_addr(v6_addr),
        )
    }

    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness> {
        self.clone()
    }
}

/// Provides the execution context related to address IDs.
pub trait IpDeviceAddressIdContext<I: IpDeviceIpExt>: DeviceIdContext<AnyDevice> {
    type AddressId: IpAddressId<I::Addr>;
}

pub trait IpDeviceAddressContext<I: IpDeviceIpExt, BT: InstantBindingsTypes>:
    IpDeviceAddressIdContext<I>
{
    fn with_ip_address_state<O, F: FnOnce(&I::AddressState<BT::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O;

    fn with_ip_address_state_mut<O, F: FnOnce(&mut I::AddressState<BT::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O;
}

/// Accessor for IP device state.
pub trait IpDeviceStateContext<I: IpDeviceIpExt, BT: InstantBindingsTypes>:
    IpDeviceAddressContext<I, BT>
{
    type IpDeviceAddressCtx<'a>: IpDeviceAddressContext<
        I,
        BT,
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
        config: I::AddressConfig<BT::Instant>,
    ) -> Result<Self::AddressId, ExistsError>;

    /// Removes an address from the device identified by the ID.
    fn remove_ip_address(
        &mut self,
        device_id: &Self::DeviceId,
        addr: Self::AddressId,
    ) -> (AddrSubnet<I::Addr, I::AssignedWitness>, I::AddressConfig<BT::Instant>);

    /// Returns the address ID for the given address value.
    fn get_address_id(
        &mut self,
        device_id: &Self::DeviceId,
        addr: SpecifiedAddr<I::Addr>,
    ) -> Result<Self::AddressId, NotFoundError>;

    /// The iterator given to `with_address_ids`.
    type AddressIdsIter<'a>: Iterator<Item = Self::AddressId> + 'a;

    /// Calls the function with an iterator over all the address IDs associated
    /// with the device.
    fn with_address_ids<
        O,
        F: FnOnce(Self::AddressIdsIter<'_>, &mut Self::IpDeviceAddressCtx<'_>) -> O,
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
        bindings_ctx: &mut BT,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    );

    /// Leaves the link-layer multicast group associated with the given IP
    /// multicast group.
    fn leave_link_multicast_group(
        &mut self,
        bindings_ctx: &mut BT,
        device_id: &Self::DeviceId,
        multicast_addr: MulticastAddr<I::Addr>,
    );
}

/// The context provided to the callback passed to
/// [`IpDeviceConfigurationContext::with_ip_device_configuration_mut`].
pub trait WithIpDeviceConfigurationMutInner<I: IpDeviceIpExt, BT: InstantBindingsTypes>:
    DeviceIdContext<AnyDevice>
{
    type IpDeviceStateCtx<'s>: IpDeviceStateContext<I, BT, DeviceId = Self::DeviceId>
        + GmpHandler<I, BT>
        + NudIpHandler<I, BT>
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
pub trait IpDeviceConfigurationContext<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, Self::DeviceId>,
>: IpDeviceStateContext<I, BC> + DeviceIdContext<AnyDevice>
{
    type DevicesIter<'s>: Iterator<Item = Self::DeviceId> + 's;
    type WithIpDeviceConfigurationInnerCtx<'s>: IpDeviceStateContext<I, BC, DeviceId = Self::DeviceId, AddressId = Self::AddressId>
        + GmpHandler<I, BC>
        + NudIpHandler<I, BC>
        + DadHandler<I, BC>
        + TimerHandler<BC, I::Timer<Self::DeviceId>>
        + IpAddressRemovalHandler<I, BC>
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
pub trait WithIpv6DeviceConfigurationMutInner<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
    WithIpDeviceConfigurationMutInner<Ipv6, BC>
{
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<BC, DeviceId = Self::DeviceId>
        + GmpHandler<Ipv6, BC>
        + NudIpHandler<Ipv6, BC>
        + DadHandler<Ipv6, BC>
        + RsHandler<BC>
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

pub trait Ipv6DeviceConfigurationContext<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
    IpDeviceConfigurationContext<Ipv6, BC>
{
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<BC, DeviceId = Self::DeviceId, AddressId = Self::AddressId>
        + GmpHandler<Ipv6, BC>
        + MldPacketHandler<BC, Self::DeviceId>
        + NudIpHandler<Ipv6, BC>
        + DadHandler<Ipv6, BC>
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
pub trait Ipv6DeviceContext<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
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
            del_ip_addr(
                self,
                bindings_ctx,
                device_id,
                DelIpAddr::AddressId(addr_id),
                AddressRemovedReason::DadFailed,
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
pub(crate) trait IpDeviceSendContext<I: IpTypesIpExt, BC>:
    DeviceIdContext<AnyDevice>
{
    /// Sends an IP packet through the device.
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        local_addr: SpecifiedAddr<I::Addr>,
        body: S,
        broadcast: Option<I::BroadcastMarker>,
        egress_proof: ProofOfEgressCheck,
    ) -> Result<(), S>
    where
        S: Serializer + IpPacket<I>,
        S::Buffer: BufferMut;
}

fn enable_ipv6_device_with_config<
    BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
    CC: Ipv6DeviceContext<BC> + GmpHandler<Ipv6, BC> + RsHandler<BC> + DadHandler<Ipv6, BC>,
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
                addr: addr_id.addr().into(),
                state: IpAddressState::Tentative,
            });
            DadHandler::start_duplicate_address_detection(
                core_ctx,
                bindings_ctx,
                device_id,
                &addr_id,
            );
        });

    // TODO(https://fxbug.dev/42178008): Generate link-local address with opaque
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

            match add_ip_addr_subnet_with_config(
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
        + DadHandler<Ipv6, BC>
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
                del_ip_addr_inner_and_notify_handler(
                    core_ctx,
                    bindings_ctx,
                    device_id,
                    DelIpAddr::AddressId(addr_id),
                    AddressRemovedReason::Manual,
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
                    addr: addr_id.addr().into(),
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
                addr: addr.addr().into(),
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
                addr: addr.addr().into(),
                state: IpAddressState::Unavailable,
            });
        })
    })
}

/// Gets the IPv4 address and subnet pairs associated with this device.
///
/// Returns an [`Iterator`] of `AddrSubnet`.
pub(crate) fn with_assigned_ipv4_addr_subnets<
    BT: InstantBindingsTypes,
    CC: IpDeviceStateContext<Ipv4, BT>,
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
pub(crate) fn get_ipv4_addr_subnet<BT: InstantBindingsTypes, CC: IpDeviceStateContext<Ipv4, BT>>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Option<AddrSubnet<Ipv4Addr>> {
    with_assigned_ipv4_addr_subnets(core_ctx, device_id, |mut addrs| addrs.nth(0))
}

/// Gets the hop limit for new IPv6 packets that will be sent out from `device`.
pub(crate) fn get_ipv6_hop_limit<BT: InstantBindingsTypes, CC: IpDeviceStateContext<Ipv6, BT>>(
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

fn add_ip_addr_subnet_with_config<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC> + DadHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr_sub: AddrSubnet<I::Addr, I::AssignedWitness>,
    addr_config: I::AddressConfig<BC::Instant>,
    // Not used but required to make sure that the caller is currently holding a
    // a reference to the IP device's IP configuration as a way to prove that
    // caller has synchronized this operation with other accesses to the IP
    // device configuration.
    _device_config: &I::Configuration,
) -> Result<CC::AddressId, ExistsError> {
    info!("adding addr {addr_sub:?} config {addr_config:?} to device {device_id:?}");
    let valid_until = I::get_valid_until(&addr_config);
    let addr_id = core_ctx.add_ip_address(device_id, addr_sub, addr_config)?;
    assert_eq!(addr_id.addr().addr(), addr_sub.addr().get());

    let ip_enabled =
        core_ctx.with_ip_device_flags(device_id, |IpDeviceFlags { ip_enabled }| *ip_enabled);

    let state = match ip_enabled {
        true => CC::INITIAL_ADDRESS_STATE,
        false => IpAddressState::Unavailable,
    };

    bindings_ctx.on_event(IpDeviceEvent::AddressAdded {
        device: device_id.clone(),
        addr: addr_sub.to_witness(),
        state,
        valid_until,
    });

    if ip_enabled {
        // NB: We don't start DAD if the device is disabled. DAD will be
        // performed when the device is enabled for all addressed.
        DadHandler::start_duplicate_address_detection(core_ctx, bindings_ctx, device_id, &addr_id)
    }

    Ok(addr_id)
}

/// A handler to abstract side-effects of removing IP device addresses.
pub trait IpAddressRemovalHandler<I: IpDeviceIpExt, BC: InstantBindingsTypes>:
    DeviceIdContext<AnyDevice>
{
    /// Notifies the handler that the addr `addr` with `config` has been removed
    /// from `device_id` with `reason`.
    fn on_address_removed(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr_sub: AddrSubnet<I::Addr, I::AssignedWitness>,
        config: I::AddressConfig<BC::Instant>,
        reason: AddressRemovedReason,
    );
}

/// There's no special action to be taken for removed IPv4 addresses.
impl<CC: DeviceIdContext<AnyDevice>, BC: InstantBindingsTypes> IpAddressRemovalHandler<Ipv4, BC>
    for CC
{
    fn on_address_removed(
        &mut self,
        _bindings_ctx: &mut BC,
        _device_id: &Self::DeviceId,
        _addr_sub: AddrSubnet<Ipv4Addr, SpecifiedAddr<Ipv4Addr>>,
        _config: Ipv4AddrConfig<BC::Instant>,
        _reason: AddressRemovedReason,
    ) {
        // Nothing to do.
    }
}

/// Provide the IPv6 implementation for all [`SlaacHandler`] implementations.
impl<CC: SlaacHandler<BC>, BC: InstantContext> IpAddressRemovalHandler<Ipv6, BC> for CC {
    fn on_address_removed(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        config: Ipv6AddrConfig<BC::Instant>,
        reason: AddressRemovedReason,
    ) {
        match config {
            Ipv6AddrConfig::Slaac(s) => {
                SlaacHandler::on_address_removed(self, bindings_ctx, device_id, addr_sub, s, reason)
            }
            Ipv6AddrConfig::Manual(_manual_config) => (),
        }
    }
}

pub(crate) enum DelIpAddr<Id, A> {
    SpecifiedAddr(SpecifiedAddr<A>),
    AddressId(Id),
}

impl<Id: IpAddressId<A>, A: IpAddress> Display for DelIpAddr<Id, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DelIpAddr::SpecifiedAddr(addr) => write!(f, "{}", *addr),
            DelIpAddr::AddressId(id) => write!(f, "{}", id.addr()),
        }
    }
}

// Deletes an IP address from a device, returning the address and its
// configuration if it was removed.
fn del_ip_addr_inner<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC> + DadHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: DelIpAddr<CC::AddressId, I::Addr>,
    reason: AddressRemovedReason,
    // Require configuration lock to do this.
    _config: &I::Configuration,
) -> Result<(AddrSubnet<I::Addr, I::AssignedWitness>, I::AddressConfig<BC::Instant>), NotFoundError>
{
    let addr_id = match addr {
        DelIpAddr::SpecifiedAddr(addr) => core_ctx.get_address_id(device_id, addr)?,
        DelIpAddr::AddressId(id) => id,
    };

    DadHandler::stop_duplicate_address_detection(core_ctx, bindings_ctx, device_id, &addr_id);
    let (addr_sub, addr_config) = core_ctx.remove_ip_address(device_id, addr_id);
    let addr = addr_sub.addr();

    bindings_ctx.on_event(IpDeviceEvent::AddressRemoved {
        device: device_id.clone(),
        addr: addr.into(),
        reason,
    });

    Ok((addr_sub, addr_config))
}

/// Removes an IP address and associated subnet from this device.
fn del_ip_addr<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: DelIpAddr<CC::AddressId, I::Addr>,
    reason: AddressRemovedReason,
) -> Result<(), NotFoundError> {
    info!("removing addr {addr} from device {device_id:?}");
    core_ctx.with_ip_device_configuration(device_id, |config, mut core_ctx| {
        del_ip_addr_inner_and_notify_handler(
            &mut core_ctx,
            bindings_ctx,
            device_id,
            addr,
            reason,
            config,
        )
    })
}

/// Removes an IP address and associated subnet from this device and notifies
/// the address removal handler.
fn del_ip_addr_inner_and_notify_handler<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC>
        + GmpHandler<I, BC>
        + DadHandler<I, BC>
        + IpAddressRemovalHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr: DelIpAddr<CC::AddressId, I::Addr>,
    reason: AddressRemovedReason,
    config: &I::Configuration,
) -> Result<(), NotFoundError> {
    del_ip_addr_inner(core_ctx, bindings_ctx, device_id, addr, reason, config).map(
        |(addr_sub, config)| {
            core_ctx.on_address_removed(bindings_ctx, device_id, addr_sub, config, reason)
        },
    )
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

    use crate::{
        device::DeviceId,
        testutil::{FakeBindingsCtx, FakeCtx},
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

    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    pub(crate) fn set_ip_device_enabled<I: crate::IpExt>(
        ctx: &mut FakeCtx,
        device: &DeviceId<FakeBindingsCtx>,
        enabled: bool,
        prev_enabled: bool,
    ) {
        let make_update = |enabled| IpDeviceConfigurationUpdate {
            ip_enabled: Some(enabled),
            ..Default::default()
        };

        let prev = ctx
            .core_api()
            .device_ip::<I>()
            .update_configuration(device, make_update(enabled).into())
            .unwrap();
        assert_eq!(prev.as_ref(), &make_update(prev_enabled),);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec;
    use core::{num::NonZeroU16, time::Duration};

    use assert_matches::assert_matches;
    use fakealloc::collections::HashSet;
    use ip_test_macro::ip_test;
    use test_case::test_case;

    use net_declare::{net_ip_v4, net_ip_v6, net_mac};
    use net_types::LinkLocalAddr;

    use crate::{
        context::testutil::FakeInstant,
        device::{
            ethernet::{EthernetCreationProperties, EthernetLinkDevice, MaxEthernetFrameSize},
            loopback::{LoopbackCreationProperties, LoopbackDevice},
            DeviceId,
        },
        ip::{
            device::{
                api::SetIpAddressPropertiesError, config::UpdateIpConfigurationError,
                nud::LinkResolutionResult, slaac::SlaacConfiguration,
                testutil::set_ip_device_enabled,
            },
            gmp::GmpDelayedReportTimerId,
        },
        state::StackStateBuilder,
        testutil::{
            assert_empty, Ctx, DispatchedEvent, FakeBindingsCtx, FakeCtx, TestIpExt as _,
            DEFAULT_INTERFACE_METRIC, IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
        },
        time::TimerIdInner,
        TimerId,
    };

    #[test]
    fn enable_disable_ipv4() {
        let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());

        let local_mac = Ipv4::FAKE_CONFIG.local_mac;
        let ethernet_device_id =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
                },
                DEFAULT_INTERFACE_METRIC,
            );
        let device_id = ethernet_device_id.clone().into();

        assert_eq!(ctx.bindings_ctx.take_events()[..], []);

        let set_ipv4_enabled = |ctx: &mut FakeCtx, enabled, expected_prev| {
            set_ip_device_enabled::<Ipv4>(ctx, &device_id, enabled, expected_prev)
        };

        set_ipv4_enabled(&mut ctx, true, false);

        let weak_device_id = device_id.downgrade();
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                device: weak_device_id.clone(),
                ip_enabled: true,
            })]
        );

        let addr = SpecifiedAddr::new(net_ip_v4!("192.0.2.1")).expect("addr should be unspecified");
        let mac = net_mac!("02:23:45:67:89:ab");

        ctx.core_api()
            .neighbor::<Ipv4, EthernetLinkDevice>()
            .insert_static_entry(&ethernet_device_id, *addr, mac)
            .unwrap();
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [DispatchedEvent::NeighborIpv4(nud::Event {
                device: ethernet_device_id.downgrade(),
                addr,
                kind: nud::EventKind::Added(nud::EventState::Static(mac)),
                at: ctx.bindings_ctx.now(),
            })]
        );
        assert_matches!(
            ctx.core_api().neighbor::<Ipv4, EthernetLinkDevice>().resolve_link_addr(
                &ethernet_device_id,
                &addr,
            ),
            LinkResolutionResult::Resolved(got) => assert_eq!(got, mac)
        );

        set_ipv4_enabled(&mut ctx, false, true);
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [
                DispatchedEvent::NeighborIpv4(nud::Event {
                    device: ethernet_device_id.downgrade(),
                    addr,
                    kind: nud::EventKind::Removed,
                    at: ctx.bindings_ctx.now(),
                }),
                DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::EnabledChanged {
                    device: weak_device_id.clone(),
                    ip_enabled: false,
                })
            ]
        );

        // Assert that static ARP entries are flushed on link down.
        nud::testutil::assert_neighbor_unknown::<Ipv4, _, _, _>(
            &mut ctx.core_ctx(),
            ethernet_device_id,
            addr,
        );

        let ipv4_addr_subnet = AddrSubnet::new(Ipv4Addr::new([192, 168, 0, 1]), 24).unwrap();
        ctx.core_api()
            .device_ip::<Ipv4>()
            .add_ip_addr_subnet(&device_id, ipv4_addr_subnet.clone())
            .expect("failed to add IPv4 Address");
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.clone(),
                state: IpAddressState::Unavailable,
                valid_until: Lifetime::Infinite,
            })]
        );

        set_ipv4_enabled(&mut ctx, true, false);
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
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
        set_ipv4_enabled(&mut ctx, true, true);
        assert_eq!(ctx.bindings_ctx.take_events()[..], []);

        let valid_until = Lifetime::Finite(FakeInstant::from(Duration::from_secs(1)));
        ctx.core_api()
            .device_ip::<Ipv4>()
            .set_addr_properties(&device_id, ipv4_addr_subnet.addr(), valid_until)
            .expect("set properties should succeed");
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv4(IpDeviceEvent::AddressPropertiesChanged {
                device: weak_device_id.clone(),
                addr: ipv4_addr_subnet.addr(),
                valid_until
            })]
        );

        // Verify that a redundant "set properties" does not generate any events.
        ctx.core_api()
            .device_ip::<Ipv4>()
            .set_addr_properties(&device_id, ipv4_addr_subnet.addr(), valid_until)
            .expect("set properties should succeed");
        assert_eq!(ctx.bindings_ctx.take_events()[..], []);

        set_ipv4_enabled(&mut ctx, false, true);
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
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
        set_ipv4_enabled(&mut ctx, false, false);
        assert_eq!(ctx.bindings_ctx.take_events()[..], []);
    }

    fn enable_ipv6_device(
        ctx: &mut FakeCtx,
        device_id: &DeviceId<FakeBindingsCtx>,
        ll_addr: AddrSubnet<Ipv6Addr, LinkLocalAddr<UnicastAddr<Ipv6Addr>>>,
        expected_prev: bool,
    ) {
        set_ip_device_enabled::<Ipv6>(ctx, device_id, true, expected_prev);

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut ctx.core_ctx(),
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
        let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let ethernet_device_id =
            ctx.core_api().device::<EthernetLinkDevice>().add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            );
        let device_id = ethernet_device_id.clone().into();
        let ll_addr = local_mac.to_ipv6_link_local();
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    // Doesn't matter as long as we perform DAD and router
                    // solicitation.
                    dad_transmits: Some(NonZeroU16::new(1)),
                    max_router_solicitations: Some(NonZeroU8::new(1)),
                    // Auto-generate a link-local address.
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config: IpDeviceConfigurationUpdate {
                        gmp_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        assert_eq!(ctx.bindings_ctx.take_events()[..], []);

        // Enable the device and observe an auto-generated link-local address,
        // router solicitation and DAD for the auto-generated address.
        let test_enable_device = |ctx: &mut FakeCtx, expected_prev| {
            enable_ipv6_device(ctx, &device_id, ll_addr, expected_prev);
            let timers = vec![
                (
                    TimerId(TimerIdInner::Ipv6Device(
                        Ipv6DeviceTimerId::Rs(RsTimerId::new(device_id.downgrade())).into(),
                    )),
                    ..,
                ),
                (
                    TimerId(TimerIdInner::Ipv6Device(
                        Ipv6DeviceTimerId::Dad(DadTimerId {
                            device_id: device_id.downgrade(),
                            addr: ll_addr.ipv6_unicast_addr(),
                        })
                        .into(),
                    )),
                    ..,
                ),
                (
                    TimerId(TimerIdInner::Ipv6Device(
                        Ipv6DeviceTimerId::Mld(MldTimerId(GmpDelayedReportTimerId {
                            device: device_id.downgrade(),
                            _marker: Default::default(),
                        }))
                        .into(),
                    )),
                    ..,
                ),
            ];
            ctx.bindings_ctx.timer_ctx().assert_timers_installed_range(timers);
        };
        test_enable_device(&mut ctx, false);
        let weak_device_id = device_id.downgrade();
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
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
            ctx.core_api().device_ip::<Ipv6>().set_addr_properties(
                &device_id,
                ll_addr.addr().into(),
                valid_until,
            ),
            Err(SetIpAddressPropertiesError::NotManual)
        );

        let addr =
            SpecifiedAddr::new(net_ip_v6!("2001:db8::1")).expect("addr should be unspecified");
        let mac = net_mac!("02:23:45:67:89:ab");
        ctx.core_api()
            .neighbor::<Ipv6, _>()
            .insert_static_entry(&ethernet_device_id, *addr, mac)
            .unwrap();
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [DispatchedEvent::NeighborIpv6(nud::Event {
                device: ethernet_device_id.downgrade(),
                addr,
                kind: nud::EventKind::Added(nud::EventState::Static(mac)),
                at: ctx.bindings_ctx.now(),
            })]
        );
        assert_matches!(
            ctx.core_api().neighbor::<Ipv6, _>().resolve_link_addr(
                &ethernet_device_id,
                &addr,
            ),
            LinkResolutionResult::Resolved(got) => assert_eq!(got, mac)
        );

        let test_disable_device = |ctx: &mut FakeCtx, expected_prev| {
            set_ip_device_enabled::<Ipv6>(ctx, &device_id, false, expected_prev);
            ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        };
        test_disable_device(&mut ctx, true);
        assert_eq!(
            ctx.bindings_ctx.take_events(),
            [
                DispatchedEvent::NeighborIpv6(nud::Event {
                    device: ethernet_device_id.downgrade(),
                    addr,
                    kind: nud::EventKind::Removed,
                    at: ctx.bindings_ctx.now(),
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

        let mut core_ctx = ctx.core_ctx();
        let core_ctx = &mut core_ctx;
        IpDeviceStateContext::<Ipv6, _>::with_address_ids(
            core_ctx,
            &device_id,
            |addrs, _core_ctx| {
                assert_empty(addrs);
            },
        );

        // Assert that static NDP entry was removed on link down.
        nud::testutil::assert_neighbor_unknown::<Ipv6, _, _, _>(core_ctx, ethernet_device_id, addr);

        ctx.core_api()
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(&device_id, ll_addr.replace_witness().unwrap())
            .expect("add MAC based IPv6 link-local address");
        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut ctx.core_ctx(),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()])
        );
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressAdded {
                device: weak_device_id.clone(),
                addr: ll_addr.to_witness(),
                state: IpAddressState::Unavailable,
                valid_until: Lifetime::Infinite,
            })]
        );

        test_enable_device(&mut ctx, false);
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
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

        ctx.core_api()
            .device_ip::<Ipv6>()
            .set_addr_properties(&device_id, ll_addr.addr().into(), valid_until)
            .expect("set properties should succeed");
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressPropertiesChanged {
                device: weak_device_id.clone(),
                addr: ll_addr.addr().into(),
                valid_until
            })]
        );

        // Verify that a redundant "set properties" does not generate any events.
        ctx.core_api()
            .device_ip::<Ipv6>()
            .set_addr_properties(&device_id, ll_addr.addr().into(), valid_until)
            .expect("set properties should succeed");
        assert_eq!(ctx.bindings_ctx.take_events()[..], []);

        test_disable_device(&mut ctx, true);
        // The address was manually added, don't expect it to be removed.
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
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
        test_disable_device(&mut ctx, false);

        let (mut core_ctx, bindings_ctx) = ctx.contexts();
        let core_ctx = &mut core_ctx;
        assert_eq!(bindings_ctx.take_events()[..], []);

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                core_ctx,
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "manual addresses should not be removed on device disable"
        );

        test_enable_device(&mut ctx, false);
        assert_eq!(
            ctx.bindings_ctx.take_events()[..],
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
        test_enable_device(&mut ctx, true);
        assert_eq!(ctx.bindings_ctx.take_events()[..], []);

        // Disable device again so timers are cancelled.
        test_disable_device(&mut ctx, true);
        let _ = ctx.bindings_ctx.take_events();
    }

    #[test]
    fn notify_on_dad_failure_ipv6() {
        let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());

        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: IPV6_MIN_IMPLIED_MAX_FRAME_SIZE,
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();
        let _: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: Some(NonZeroU16::new(1)),
                    max_router_solicitations: Some(NonZeroU8::new(1)),
                    // Auto-generate a link-local address.
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        ..Default::default()
                    }),
                    ip_config: IpDeviceConfigurationUpdate {
                        gmp_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        let ll_addr = local_mac.to_ipv6_link_local();

        enable_ipv6_device(&mut ctx, &device_id, ll_addr, false);
        let weak_device_id = device_id.downgrade();
        assert_eq!(
            &ctx.bindings_ctx.take_events()[..],
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
        ctx.core_api()
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(&device_id, assigned_addr)
            .expect("add succeeds");
        let Ctx { core_ctx, bindings_ctx } = &mut ctx;
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
                &mut core_ctx.context(),
                bindings_ctx,
                &device_id,
                assigned_addr.ipv6_unicast_addr()
            ),
            IpAddressState::Tentative,
        );

        assert_eq!(
            bindings_ctx.take_events()[..],
            [DispatchedEvent::IpDeviceIpv6(IpDeviceEvent::AddressRemoved {
                device: weak_device_id,
                addr: assigned_addr.addr(),
                reason: AddressRemovedReason::DadFailed,
            }),]
        );

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut core_ctx.context(),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            HashSet::from([ll_addr.ipv6_unicast_addr()]),
            "manual addresses should be removed on DAD failure"
        );
        // Disable device and take all events to cleanup references.
        set_ip_device_enabled::<Ipv6>(&mut ctx, &device_id, false, true);
        let _ = ctx.bindings_ctx.take_events();
    }

    #[ip_test]
    #[netstack3_macros::context_ip_bounds(I, FakeBindingsCtx, crate)]
    fn update_ip_device_configuration_err<I: Ip + crate::IpExt>() {
        let mut ctx = FakeCtx::default();

        let loopback_device_id = ctx
            .core_api()
            .device::<LoopbackDevice>()
            .add_device_with_default_state(
                LoopbackCreationProperties { mtu: Mtu::new(u16::MAX as u32) },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();

        let mut api = ctx.core_api().device_ip::<I>();
        let original_state = api.get_configuration(&loopback_device_id);
        assert_eq!(
            api.update_configuration(
                &loopback_device_id,
                IpDeviceConfigurationUpdate {
                    ip_enabled: Some(!AsRef::<IpDeviceFlags>::as_ref(&original_state).ip_enabled),
                    gmp_enabled: Some(
                        !AsRef::<IpDeviceConfiguration>::as_ref(&original_state).gmp_enabled
                    ),
                    forwarding_enabled: Some(true),
                }
                .into(),
            )
            .unwrap_err(),
            UpdateIpConfigurationError::ForwardingNotSupported,
        );
        assert_eq!(original_state, api.get_configuration(&loopback_device_id));
    }

    #[test]
    fn update_ipv4_configuration_return() {
        let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv4::FAKE_CONFIG.local_mac;
        let device_id = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();

        let mut api = ctx.core_api().device_ip::<Ipv4>();
        // Perform no update.
        assert_eq!(
            api.update_configuration(&device_id, Ipv4DeviceConfigurationUpdate::default()),
            Ok(Ipv4DeviceConfigurationUpdate::default()),
        );

        // Enable all but forwarding. All features are initially disabled.
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(true),
                    },
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(false),
                    forwarding_enabled: Some(false),
                    gmp_enabled: Some(false),
                },
            }),
        );

        // Change forwarding config.
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(true),
                        gmp_enabled: None,
                    },
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(false),
                    gmp_enabled: None,
                },
            }),
        );

        // No update to anything (GMP enabled set to already set
        // value).
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: None,
                        forwarding_enabled: None,
                        gmp_enabled: Some(true),
                    },
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: None,
                    forwarding_enabled: None,
                    gmp_enabled: Some(true),
                },
            }),
        );

        // Disable/change everything.
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(false),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(false),
                    },
                },
            ),
            Ok(Ipv4DeviceConfigurationUpdate {
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(true),
                    gmp_enabled: Some(true),
                },
            }),
        );
    }

    #[test]
    fn update_ipv6_configuration_return() {
        let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv6::MINIMUM_LINK_MTU).unwrap(),
                },
                DEFAULT_INTERFACE_METRIC,
            )
            .into();

        let mut api = ctx.core_api().device_ip::<Ipv6>();

        // Perform no update.
        assert_eq!(
            api.update_configuration(&device_id, Ipv6DeviceConfigurationUpdate::default()),
            Ok(Ipv6DeviceConfigurationUpdate::default()),
        );

        // Enable all but forwarding. All features are initially disabled.
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: Some(NonZeroU16::new(1)),
                    max_router_solicitations: Some(NonZeroU8::new(2)),
                    slaac_config: Some(SlaacConfiguration {
                        enable_stable_addresses: true,
                        temporary_address_configuration: None
                    }),
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(true),
                    },
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(None),
                max_router_solicitations: Some(None),
                slaac_config: Some(SlaacConfiguration::default()),
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(false),
                    forwarding_enabled: Some(false),
                    gmp_enabled: Some(false),
                },
            }),
        );

        // Change forwarding config.
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: None,
                    max_router_solicitations: None,
                    slaac_config: None,
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        forwarding_enabled: Some(true),
                        gmp_enabled: None,
                    },
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: None,
                max_router_solicitations: None,
                slaac_config: None,
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(false),
                    gmp_enabled: None,
                },
            }),
        );

        // No update to anything (GMP enabled set to already set
        // value).
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: None,
                    max_router_solicitations: None,
                    slaac_config: None,
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: None,
                        forwarding_enabled: None,
                        gmp_enabled: Some(true),
                    },
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: None,
                max_router_solicitations: None,
                slaac_config: None,
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: None,
                    forwarding_enabled: None,
                    gmp_enabled: Some(true),
                },
            }),
        );

        // Disable/change everything.
        assert_eq!(
            api.update_configuration(
                &device_id,
                Ipv6DeviceConfigurationUpdate {
                    dad_transmits: Some(None),
                    max_router_solicitations: Some(None),
                    slaac_config: Some(SlaacConfiguration::default()),
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(false),
                        forwarding_enabled: Some(false),
                        gmp_enabled: Some(false),
                    },
                },
            ),
            Ok(Ipv6DeviceConfigurationUpdate {
                dad_transmits: Some(NonZeroU16::new(1)),
                max_router_solicitations: Some(NonZeroU8::new(2)),
                slaac_config: Some(SlaacConfiguration {
                    enable_stable_addresses: true,
                    temporary_address_configuration: None
                }),
                ip_config: IpDeviceConfigurationUpdate {
                    ip_enabled: Some(true),
                    forwarding_enabled: Some(true),
                    gmp_enabled: Some(true),
                },
            }),
        );
    }

    #[test_case(false; "stable addresses enabled generates link local")]
    #[test_case(true; "stable addresses disabled does not generate link local")]
    fn configure_link_local_address_generation(enable_stable_addresses: bool) {
        let mut ctx = FakeCtx::new_with_builder(StackStateBuilder::default());
        ctx.bindings_ctx.timer_ctx().assert_no_timers_installed();
        let local_mac = Ipv6::FAKE_CONFIG.local_mac;
        let device_id = ctx
            .core_api()
            .device::<EthernetLinkDevice>()
            .add_device_with_default_state(
                EthernetCreationProperties {
                    mac: local_mac,
                    max_frame_size: MaxEthernetFrameSize::from_mtu(Ipv4::MINIMUM_LINK_MTU).unwrap(),
                },
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

        let _prev: Ipv6DeviceConfigurationUpdate = ctx
            .core_api()
            .device_ip::<Ipv6>()
            .update_configuration(&device_id, new_config)
            .unwrap();
        set_ip_device_enabled::<Ipv6>(&mut ctx, &device_id, true, false);

        let expected_addrs = if enable_stable_addresses {
            HashSet::from([local_mac.to_ipv6_link_local().ipv6_unicast_addr()])
        } else {
            HashSet::new()
        };

        assert_eq!(
            IpDeviceStateContext::<Ipv6, _>::with_address_ids(
                &mut ctx.core_ctx(),
                &device_id,
                |addrs, _core_ctx| {
                    addrs.map(|addr_id| addr_id.addr_sub().addr().get()).collect::<HashSet<_>>()
                }
            ),
            expected_addrs,
        );
    }
}
