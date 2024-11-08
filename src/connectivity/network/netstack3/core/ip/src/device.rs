// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An IP device.

pub(crate) mod api;
pub(crate) mod config;
pub(crate) mod dad;
pub(crate) mod nud;
pub(crate) mod opaque_iid;
pub(crate) mod route_discovery;
pub(crate) mod router_solicitation;
pub(crate) mod slaac;
pub(crate) mod state;

use alloc::vec::Vec;
use core::fmt::{Debug, Display};
use core::hash::Hash;
use core::num::NonZeroU8;

use derivative::Derivative;
use log::info;
use net_types::ip::{
    AddrSubnet, GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Ipv6SourceAddr, Mtu,
    Subnet,
};
use net_types::{MulticastAddr, SpecifiedAddr, UnicastAddr, Witness};
use netstack3_base::{
    AnyDevice, CounterContext, DeferredResourceRemovalContext, DeviceIdContext, EventContext,
    ExistsError, HandleableTimer, Inspectable, Instant, InstantBindingsTypes, InstantContext,
    IpDeviceAddr, IpExt, Ipv4DeviceAddr, Ipv6DeviceAddr, NotFoundError,
    RemoveResourceResultWithContext, RngContext, SendFrameError, StrongDeviceIdentifier,
    TimerBindingsTypes, TimerContext, TimerHandler, WeakDeviceIdentifier,
};
use netstack3_filter::ProofOfEgressCheck;
use packet::{BufferMut, Serializer};
use packet_formats::icmp::mld::MldPacket;
use packet_formats::icmp::ndp::options::NdpNonce;
use packet_formats::icmp::ndp::NonZeroNdpLifetime;
use packet_formats::utils::NonZeroDuration;
use zerocopy::SplitByteSlice;

use crate::internal::base::{
    DeviceIpLayerMetadata, IpCounters, IpDeviceMtuContext, IpPacketDestination,
};
use crate::internal::device::config::{
    IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
};
use crate::internal::device::dad::{DadAddressStateLookupResult, DadHandler, DadTimerId};
use crate::internal::device::nud::NudIpHandler;
use crate::internal::device::route_discovery::{
    Ipv6DiscoveredRoute, Ipv6DiscoveredRouteTimerId, RouteDiscoveryHandler,
};
use crate::internal::device::router_solicitation::{RsHandler, RsTimerId};
use crate::internal::device::slaac::{SlaacHandler, SlaacTimerId};
use crate::internal::device::state::{
    IpDeviceConfiguration, IpDeviceFlags, IpDeviceState, IpDeviceStateBindingsTypes,
    IpDeviceStateIpExt, Ipv4AddrConfig, Ipv4AddressState, Ipv4DeviceConfiguration,
    Ipv4DeviceConfigurationAndFlags, Ipv4DeviceState, Ipv6AddrConfig, Ipv6AddrManualConfig,
    Ipv6AddrSlaacConfig, Ipv6AddressFlags, Ipv6AddressState, Ipv6DeviceConfiguration,
    Ipv6DeviceConfigurationAndFlags, Ipv6DeviceState, Ipv6NetworkLearnedParameters, Lifetime,
};
use crate::internal::gmp::igmp::{IgmpPacketHandler, IgmpTimerId};
use crate::internal::gmp::mld::{MldPacketHandler, MldTimerId};
use crate::internal::gmp::{GmpHandler, GmpQueryHandler, GroupJoinResult, GroupLeaveResult};

/// An IP device timer.
///
/// This timer is an indirection to the real types defined by the
/// [`IpDeviceIpExt`] trait. Having a concrete type parameterized over IP allows
/// us to provide implementations generic on I for outer timer contexts that
/// handle `IpDeviceTimerId` timers.
#[derive(Derivative, GenericOverIp)]
#[derivative(
    Clone(bound = ""),
    Eq(bound = ""),
    PartialEq(bound = ""),
    Hash(bound = ""),
    Debug(bound = "")
)]
#[generic_over_ip(I, Ip)]
pub struct IpDeviceTimerId<I: IpDeviceIpExt, D: WeakDeviceIdentifier, A: IpAddressIdSpec>(
    I::Timer<D, A>,
);

/// A timer ID for IPv4 devices.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct Ipv4DeviceTimerId<D: WeakDeviceIdentifier>(IgmpTimerId<D>);

impl<D: WeakDeviceIdentifier> Ipv4DeviceTimerId<D> {
    /// Gets the device ID from this timer IFF the device hasn't been destroyed.
    fn device_id(&self) -> Option<D::Strong> {
        let Self(this) = self;
        this.device_id().upgrade()
    }

    /// Transforms this timer ID into the common [`IpDeviceTimerId`] version.
    pub fn into_common<S: IpAddressIdSpec>(self) -> IpDeviceTimerId<Ipv4, D, S> {
        self.into()
    }
}

impl<D: WeakDeviceIdentifier, A: IpAddressIdSpec> From<IpDeviceTimerId<Ipv4, D, A>>
    for Ipv4DeviceTimerId<D>
{
    fn from(IpDeviceTimerId(inner): IpDeviceTimerId<Ipv4, D, A>) -> Self {
        inner
    }
}

impl<D: WeakDeviceIdentifier, A: IpAddressIdSpec> From<Ipv4DeviceTimerId<D>>
    for IpDeviceTimerId<Ipv4, D, A>
{
    fn from(value: Ipv4DeviceTimerId<D>) -> Self {
        Self(value)
    }
}

impl<D: WeakDeviceIdentifier> From<IgmpTimerId<D>> for Ipv4DeviceTimerId<D> {
    fn from(id: IgmpTimerId<D>) -> Ipv4DeviceTimerId<D> {
        Ipv4DeviceTimerId(id)
    }
}

impl<D: WeakDeviceIdentifier, BC: TimerBindingsTypes, CC: TimerHandler<BC, IgmpTimerId<D>>>
    HandleableTimer<CC, BC> for Ipv4DeviceTimerId<D>
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, timer: BC::UniqueTimerId) {
        let Self(id) = self;
        core_ctx.handle_timer(bindings_ctx, id, timer);
    }
}

impl<I, CC, BC, A> HandleableTimer<CC, BC> for IpDeviceTimerId<I, CC::WeakDeviceId, A>
where
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
    A: IpAddressIdSpec,
    for<'a> CC::WithIpDeviceConfigurationInnerCtx<'a>:
        TimerHandler<BC, I::Timer<CC::WeakDeviceId, A>>,
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, timer: BC::UniqueTimerId) {
        let Self(id) = self;
        let Some(device_id) = I::timer_device_id(&id) else {
            return;
        };
        core_ctx.with_ip_device_configuration(&device_id, |_state, mut core_ctx| {
            TimerHandler::handle_timer(&mut core_ctx, bindings_ctx, id, timer)
        })
    }
}

/// A timer ID for IPv6 devices.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
#[allow(missing_docs)]
pub enum Ipv6DeviceTimerId<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> {
    Mld(MldTimerId<D>),
    Dad(DadTimerId<D, A>),
    Rs(RsTimerId<D>),
    RouteDiscovery(Ipv6DiscoveredRouteTimerId<D>),
    Slaac(SlaacTimerId<D>),
}

impl<D: WeakDeviceIdentifier, A: IpAddressIdSpec> From<IpDeviceTimerId<Ipv6, D, A>>
    for Ipv6DeviceTimerId<D, A::WeakV6>
{
    fn from(IpDeviceTimerId(inner): IpDeviceTimerId<Ipv6, D, A>) -> Self {
        inner
    }
}

impl<D: WeakDeviceIdentifier, A: IpAddressIdSpec> From<Ipv6DeviceTimerId<D, A::WeakV6>>
    for IpDeviceTimerId<Ipv6, D, A>
{
    fn from(value: Ipv6DeviceTimerId<D, A::WeakV6>) -> Self {
        Self(value)
    }
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> Ipv6DeviceTimerId<D, A> {
    /// Gets the device ID from this timer IFF the device hasn't been destroyed.
    fn device_id(&self) -> Option<D::Strong> {
        match self {
            Self::Mld(id) => id.device_id(),
            Self::Dad(id) => id.device_id(),
            Self::Rs(id) => id.device_id(),
            Self::RouteDiscovery(id) => id.device_id(),
            Self::Slaac(id) => id.device_id(),
        }
        .upgrade()
    }

    /// Transforms this timer ID into the common [`IpDeviceTimerId`] version.
    pub fn into_common<S: IpAddressIdSpec<WeakV6 = A>>(self) -> IpDeviceTimerId<Ipv6, D, S> {
        self.into()
    }
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> From<MldTimerId<D>>
    for Ipv6DeviceTimerId<D, A>
{
    fn from(id: MldTimerId<D>) -> Ipv6DeviceTimerId<D, A> {
        Ipv6DeviceTimerId::Mld(id)
    }
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> From<DadTimerId<D, A>>
    for Ipv6DeviceTimerId<D, A>
{
    fn from(id: DadTimerId<D, A>) -> Ipv6DeviceTimerId<D, A> {
        Ipv6DeviceTimerId::Dad(id)
    }
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> From<RsTimerId<D>>
    for Ipv6DeviceTimerId<D, A>
{
    fn from(id: RsTimerId<D>) -> Ipv6DeviceTimerId<D, A> {
        Ipv6DeviceTimerId::Rs(id)
    }
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> From<Ipv6DiscoveredRouteTimerId<D>>
    for Ipv6DeviceTimerId<D, A>
{
    fn from(id: Ipv6DiscoveredRouteTimerId<D>) -> Ipv6DeviceTimerId<D, A> {
        Ipv6DeviceTimerId::RouteDiscovery(id)
    }
}

impl<D: WeakDeviceIdentifier, A: WeakIpAddressId<Ipv6Addr>> From<SlaacTimerId<D>>
    for Ipv6DeviceTimerId<D, A>
{
    fn from(id: SlaacTimerId<D>) -> Ipv6DeviceTimerId<D, A> {
        Ipv6DeviceTimerId::Slaac(id)
    }
}

impl<
        D: WeakDeviceIdentifier,
        A: WeakIpAddressId<Ipv6Addr>,
        BC: TimerBindingsTypes,
        CC: TimerHandler<BC, RsTimerId<D>>
            + TimerHandler<BC, Ipv6DiscoveredRouteTimerId<D>>
            + TimerHandler<BC, MldTimerId<D>>
            + TimerHandler<BC, SlaacTimerId<D>>
            + TimerHandler<BC, DadTimerId<D, A>>,
    > HandleableTimer<CC, BC> for Ipv6DeviceTimerId<D, A>
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, timer: BC::UniqueTimerId) {
        match self {
            Ipv6DeviceTimerId::Mld(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            Ipv6DeviceTimerId::Dad(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            Ipv6DeviceTimerId::Rs(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            Ipv6DeviceTimerId::RouteDiscovery(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
            Ipv6DeviceTimerId::Slaac(id) => core_ctx.handle_timer(bindings_ctx, id, timer),
        }
    }
}

/// An extension trait adding IP device properties.
pub trait IpDeviceIpExt: IpDeviceStateIpExt {
    /// IP layer state kept by the device.
    type State<BT: IpDeviceStateBindingsTypes>: AsRef<IpDeviceState<Self, BT>>
        + AsMut<IpDeviceState<Self, BT>>;
    /// IP layer configuration kept by the device.
    type Configuration: AsRef<IpDeviceConfiguration> + Clone;
    /// High level IP device timer.
    type Timer<D: WeakDeviceIdentifier, A: IpAddressIdSpec>: Into<IpDeviceTimerId<Self, D, A>>
        + From<IpDeviceTimerId<Self, D, A>>
        + Clone
        + Eq
        + PartialEq
        + Debug
        + Hash;
    /// The witness type for assigned addresses.
    type AssignedWitness: Witness<Self::Addr>
        + Copy
        + Eq
        + PartialEq
        + Debug
        + Display
        + Hash
        + Into<SpecifiedAddr<Self::Addr>>;
    /// Device address configuration.
    type AddressConfig<I: Instant>: Default + Debug;
    /// Manual device address configuration (user-initiated).
    type ManualAddressConfig<I: Instant>: Default + Debug + Into<Self::AddressConfig<I>>;
    /// Device address associated state.
    type AddressState<I: Instant>: 'static + Inspectable;
    /// Device configuration update request.
    type ConfigurationUpdate: From<IpDeviceConfigurationUpdate>
        + AsRef<IpDeviceConfigurationUpdate>
        + Debug;
    /// IP layer configuration and flags.
    type ConfigurationAndFlags: From<(Self::Configuration, IpDeviceFlags)>
        + AsRef<IpDeviceConfiguration>
        + AsRef<IpDeviceFlags>
        + AsMut<IpDeviceConfiguration>
        + PartialEq
        + Debug;

    /// Gets the lifetime of an address from its configuration.
    fn get_valid_until<I: Instant>(config: &Self::AddressConfig<I>) -> Lifetime<I>;

    /// Returns whether the address is currently assigned based on state.
    fn is_addr_assigned<I: Instant>(addr_state: &Self::AddressState<I>) -> bool;

    /// Extracts the device ID from a device timer.
    fn timer_device_id<D: WeakDeviceIdentifier, A: IpAddressIdSpec>(
        timer: &Self::Timer<D, A>,
    ) -> Option<D::Strong>;

    /// Extracts the address configuration from the state in preparation for
    /// removal.
    fn take_addr_config_for_removal<I: Instant>(
        addr_state: &mut Self::AddressState<I>,
    ) -> Option<Self::AddressConfig<I>>;
}

impl IpDeviceIpExt for Ipv4 {
    type State<BT: IpDeviceStateBindingsTypes> = Ipv4DeviceState<BT>;
    type Configuration = Ipv4DeviceConfiguration;
    type Timer<D: WeakDeviceIdentifier, A: IpAddressIdSpec> = Ipv4DeviceTimerId<D>;
    type AssignedWitness = Ipv4DeviceAddr;
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

    fn timer_device_id<D: WeakDeviceIdentifier, A: IpAddressIdSpec>(
        timer: &Self::Timer<D, A>,
    ) -> Option<D::Strong> {
        timer.device_id()
    }

    fn take_addr_config_for_removal<I: Instant>(
        addr_state: &mut Self::AddressState<I>,
    ) -> Option<Self::AddressConfig<I>> {
        addr_state.config.take()
    }
}

impl IpDeviceIpExt for Ipv6 {
    type State<BT: IpDeviceStateBindingsTypes> = Ipv6DeviceState<BT>;
    type Configuration = Ipv6DeviceConfiguration;
    type Timer<D: WeakDeviceIdentifier, A: IpAddressIdSpec> = Ipv6DeviceTimerId<D, A::WeakV6>;
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

    fn timer_device_id<D: WeakDeviceIdentifier, A: IpAddressIdSpec>(
        timer: &Self::Timer<D, A>,
    ) -> Option<D::Strong> {
        timer.device_id()
    }

    fn take_addr_config_for_removal<I: Instant>(
        addr_state: &mut Self::AddressState<I>,
    ) -> Option<Self::AddressConfig<I>> {
        addr_state.config.take()
    }
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

impl<DeviceId, I: Ip, Instant> IpDeviceEvent<DeviceId, I, Instant> {
    /// Changes the device id type with `map`.
    pub fn map_device<N, F: FnOnce(DeviceId) -> N>(self, map: F) -> IpDeviceEvent<N, I, Instant> {
        match self {
            IpDeviceEvent::AddressAdded { device, addr, state, valid_until } => {
                IpDeviceEvent::AddressAdded { device: map(device), addr, state, valid_until }
            }
            IpDeviceEvent::AddressRemoved { device, addr, reason } => {
                IpDeviceEvent::AddressRemoved { device: map(device), addr, reason }
            }
            IpDeviceEvent::AddressStateChanged { device, addr, state } => {
                IpDeviceEvent::AddressStateChanged { device: map(device), addr, state }
            }
            IpDeviceEvent::EnabledChanged { device, ip_enabled } => {
                IpDeviceEvent::EnabledChanged { device: map(device), ip_enabled }
            }
            IpDeviceEvent::AddressPropertiesChanged { device, addr, valid_until } => {
                IpDeviceEvent::AddressPropertiesChanged { device: map(device), addr, valid_until }
            }
        }
    }
}

/// The bindings execution context for IP devices.
pub trait IpDeviceBindingsContext<I: IpDeviceIpExt, D: StrongDeviceIdentifier>:
    IpDeviceStateBindingsTypes
    + DeferredResourceRemovalContext
    + TimerContext
    + RngContext
    + EventContext<IpDeviceEvent<D, I, <Self as InstantBindingsTypes>::Instant>>
{
}
impl<
        D: StrongDeviceIdentifier,
        I: IpDeviceIpExt,
        BC: IpDeviceStateBindingsTypes
            + DeferredResourceRemovalContext
            + TimerContext
            + RngContext
            + EventContext<IpDeviceEvent<D, I, <Self as InstantBindingsTypes>::Instant>>,
    > IpDeviceBindingsContext<I, D> for BC
{
}

/// An IP address ID.
pub trait IpAddressId<A: IpAddress>: Clone + Eq + Debug + Hash {
    /// The weak version of this ID.
    type Weak: WeakIpAddressId<A>;

    /// Downgrades this ID to a weak reference.
    fn downgrade(&self) -> Self::Weak;

    /// Returns the address this ID represents.
    fn addr(&self) -> IpDeviceAddr<A>;

    /// Returns the address subnet this ID represents.
    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness>
    where
        A::Version: IpDeviceIpExt;
}

/// A weak IP address ID.
pub trait WeakIpAddressId<A: IpAddress>: Clone + Eq + Debug + Hash {
    /// The strong version of this ID.
    type Strong: IpAddressId<A>;

    /// Attempts to upgrade this ID to the strong version.
    ///
    /// Upgrading fails if this is no longer a valid assigned IP address.
    fn upgrade(&self) -> Option<Self::Strong>;
}

/// Provides the execution context related to address IDs.
pub trait IpDeviceAddressIdContext<I: IpDeviceIpExt>: DeviceIdContext<AnyDevice> {
    /// The strong address identifier.
    type AddressId: IpAddressId<I::Addr, Weak = Self::WeakAddressId>;
    /// The weak address identifier.
    type WeakAddressId: WeakIpAddressId<I::Addr, Strong = Self::AddressId>;
}

/// A marker trait for a spec of address IDs.
///
/// This allows us to write types that are GenericOverIp that take the spec from
/// some type.
pub trait IpAddressIdSpec {
    /// The weak V4 address ID.
    type WeakV4: WeakIpAddressId<Ipv4Addr>;
    /// The weak V6 address ID.
    type WeakV6: WeakIpAddressId<Ipv6Addr>;
}

/// Ties an [`IpAddressIdSpec`] to a core context implementation.
pub trait IpAddressIdSpecContext:
    IpDeviceAddressIdContext<Ipv4> + IpDeviceAddressIdContext<Ipv6>
{
    /// The address ID spec for this context.
    type AddressIdSpec: IpAddressIdSpec<
        WeakV4 = <Self as IpDeviceAddressIdContext<Ipv4>>::WeakAddressId,
        WeakV6 = <Self as IpDeviceAddressIdContext<Ipv6>>::WeakAddressId,
    >;
}

/// The core context providing access to device IP address state.
pub trait IpDeviceAddressContext<I: IpDeviceIpExt, BT: InstantBindingsTypes>:
    IpDeviceAddressIdContext<I>
{
    /// Calls the callback with a reference to the address state `addr_id` on
    /// `device_id`.
    fn with_ip_address_state<O, F: FnOnce(&I::AddressState<BT::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O;

    /// Calls the callback with a mutable reference to the address state
    /// `addr_id` on `device_id`.
    fn with_ip_address_state_mut<O, F: FnOnce(&mut I::AddressState<BT::Instant>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        addr_id: &Self::AddressId,
        cb: F,
    ) -> O;
}

/// Accessor for IP device state.
pub trait IpDeviceStateContext<I: IpDeviceIpExt, BT: IpDeviceStateBindingsTypes>:
    IpDeviceAddressContext<I, BT>
{
    /// Inner accessor context.
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
    ) -> RemoveResourceResultWithContext<AddrSubnet<I::Addr>, BT>;

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
pub trait WithIpDeviceConfigurationMutInner<I: IpDeviceIpExt, BT: IpDeviceStateBindingsTypes>:
    DeviceIdContext<AnyDevice>
{
    /// The inner device state context.
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
>: IpDeviceStateContext<I, BC> + IpDeviceMtuContext<I> + DeviceIdContext<AnyDevice>
{
    /// The iterator provided by this context.
    type DevicesIter<'s>: Iterator<Item = Self::DeviceId> + 's;
    /// The inner configuration context.
    type WithIpDeviceConfigurationInnerCtx<'s>: IpDeviceStateContext<I, BC, DeviceId = Self::DeviceId, AddressId = Self::AddressId>
        + GmpHandler<I, BC>
        + NudIpHandler<I, BC>
        + DadHandler<I, BC>
        + IpAddressRemovalHandler<I, BC>
        + IpDeviceMtuContext<I>
        + 's;
    /// The inner mutable configuration context.
    type WithIpDeviceConfigurationMutInner<'s>: WithIpDeviceConfigurationMutInner<I, BC, DeviceId = Self::DeviceId>
        + 's;
    /// Provides access to device state.
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

    /// Returns the ID of the loopback interface, if one exists on the system
    /// and is initialized.
    fn loopback_id(&mut self) -> Option<Self::DeviceId>;
}

/// The context provided to the callback passed to
/// [`Ipv6DeviceConfigurationContext::with_ipv6_device_configuration_mut`].
pub trait WithIpv6DeviceConfigurationMutInner<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
    WithIpDeviceConfigurationMutInner<Ipv6, BC>
{
    /// The inner IPv6 device state context.
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

/// The core context for IPv6 device configuration.
pub trait Ipv6DeviceConfigurationContext<BC: IpDeviceBindingsContext<Ipv6, Self::DeviceId>>:
    IpDeviceConfigurationContext<Ipv6, BC>
{
    /// The context available while holding device configuration.
    type Ipv6DeviceStateCtx<'s>: Ipv6DeviceContext<BC, DeviceId = Self::DeviceId, AddressId = Self::AddressId>
        + GmpHandler<Ipv6, BC>
        + MldPacketHandler<BC, Self::DeviceId>
        + NudIpHandler<Ipv6, BC>
        + DadHandler<Ipv6, BC>
        + RsHandler<BC>
        + SlaacHandler<BC>
        + RouteDiscoveryHandler<BC>
        + 's;
    /// The context available while holding mutable device configuration.
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
pub trait IpDeviceHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
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
        is_ip_unicast_forwarding_enabled(self, device_id)
    }

    fn set_default_hop_limit(&mut self, device_id: &Self::DeviceId, hop_limit: NonZeroU8) {
        self.with_default_hop_limit_mut(device_id, |default_hop_limit| {
            *default_hop_limit = hop_limit
        })
    }
}

/// Handles receipt of an IGMP packet on `device`.
pub fn receive_igmp_packet<CC, BC, B>(
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
pub trait Ipv6DeviceHandler<BC>: IpDeviceHandler<Ipv6, BC> {
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

    /// Handles a received neighbor solicitation that has been determined to be
    /// due to a node performing duplicate address detection for `addr`. Removes
    /// `addr` from `device_id` if it is determined `addr` is a duplicate
    /// tentative address.
    ///
    /// Returns the assignment state of `addr` on the interface, if there was
    /// one before any action was taken.
    fn handle_received_dad_neighbor_solicitation(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
        nonce: Option<NdpNonce<&'_ [u8]>>,
    ) -> IpAddressState;

    /// Handles a received neighbor advertisement.
    ///
    /// Takes action in response to a received neighbor advertisement for the
    /// specified address. Returns the assignment state of the address on the
    /// given interface, if there was one before any action was taken. That is,
    /// this method returns `Some(Tentative {..})` when the address was
    /// tentatively assigned (and now removed), `Some(Assigned)` if the address
    /// was assigned (and so not removed), otherwise `None`.
    fn handle_received_neighbor_advertisement(
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
    fn receive_mld_packet<B: SplitByteSlice>(
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
        CC: Ipv6DeviceContext<BC>
            + Ipv6DeviceConfigurationContext<BC>
            + CounterContext<IpCounters<Ipv6>>,
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

    fn handle_received_dad_neighbor_solicitation(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: UnicastAddr<Ipv6Addr>,
        nonce: Option<NdpNonce<&'_ [u8]>>,
    ) -> IpAddressState {
        let addr_id = match self.get_address_id(device_id, addr.into_specified()) {
            Ok(o) => o,
            Err(NotFoundError) => return IpAddressState::Unavailable,
        };

        match self.with_ipv6_device_configuration(device_id, |_config, mut core_ctx| {
            core_ctx.handle_incoming_dad_neighbor_solicitation(
                bindings_ctx,
                device_id,
                &addr_id,
                nonce,
            )
        }) {
            DadAddressStateLookupResult::Assigned => IpAddressState::Assigned,
            DadAddressStateLookupResult::Tentative { matched_nonce: true } => {
                self.increment(|counters| &counters.version_rx.drop_looped_back_dad_probe);

                // Per RFC 7527 section 4.2, "the receiver compares the nonce
                // included in the message, with any stored nonce on the
                // receiving interface. If a match is found, the node SHOULD log
                // a system management message, SHOULD update any statistics
                // counter, and MUST drop the received message."

                // The matched nonce indicates the neighbor solicitation was
                // looped back to us, so don't remove the address.
                IpAddressState::Tentative
            }
            DadAddressStateLookupResult::Uninitialized
            | DadAddressStateLookupResult::Tentative { matched_nonce: false } => {
                // Per RFC 7527 section 4.2, "If the received NS(DAD) message
                // includes a nonce and no match is found with any stored nonce,
                // the node SHOULD log a system management message for a
                // DAD-failed state and SHOULD update any statistics counter."
                // -- meaning that we should treat this as an indication that we
                // have detected a duplicate address.

                match del_ip_addr(
                    self,
                    bindings_ctx,
                    device_id,
                    DelIpAddr::AddressId(addr_id),
                    AddressRemovedReason::DadFailed,
                ) {
                    Ok(result) => {
                        bindings_ctx.defer_removal_result(result);
                        IpAddressState::Tentative
                    }
                    Err(NotFoundError) => {
                        // We may have raced with user removal of this address.
                        IpAddressState::Unavailable
                    }
                }
            }
        }
    }

    fn handle_received_neighbor_advertisement(
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
            |Ipv6AddressState { flags: Ipv6AddressFlags { assigned }, config: _ }| *assigned,
        );

        if assigned {
            IpAddressState::Assigned
        } else {
            match del_ip_addr(
                self,
                bindings_ctx,
                device_id,
                DelIpAddr::AddressId(addr_id),
                AddressRemovedReason::DadFailed,
            ) {
                Ok(result) => {
                    bindings_ctx.defer_removal_result(result);
                    IpAddressState::Tentative
                }
                Err(NotFoundError) => {
                    // We may have raced with user removal of this address.
                    IpAddressState::Unavailable
                }
            }
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

    fn receive_mld_packet<B: SplitByteSlice>(
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
pub trait IpDeviceSendContext<I: IpExt, BC>: DeviceIdContext<AnyDevice> {
    /// Sends an IP packet through the device.
    fn send_ip_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        destination: IpPacketDestination<I, &Self::DeviceId>,
        ip_layer_metadata: DeviceIpLayerMetadata,
        body: S,
        egress_proof: ProofOfEgressCheck,
    ) -> Result<(), SendFrameError<S>>
    where
        S: Serializer,
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
    if !config.ip_config.unicast_forwarding_enabled {
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
            if config == Some(Ipv6AddrConfig::SLAAC_LINK_LOCAL) {
                del_ip_addr_inner_and_notify_handler(
                    core_ctx,
                    bindings_ctx,
                    device_id,
                    DelIpAddr::AddressId(addr_id),
                    AddressRemovedReason::Manual,
                    device_config,
                )
                .map(|remove_result| {
                    bindings_ctx.defer_removal_result(remove_result);
                })
                .unwrap_or_else(|NotFoundError| {
                    // We're not holding locks on the addresses anymore we must
                    // allow a NotFoundError since the address can be removed as
                    // we release the lock.
                })
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

/// Gets a single IPv4 address and subnet for a device.
pub fn get_ipv4_addr_subnet<BT: IpDeviceStateBindingsTypes, CC: IpDeviceStateContext<Ipv4, BT>>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> Option<AddrSubnet<Ipv4Addr, Ipv4DeviceAddr>> {
    core_ctx.with_address_ids(device_id, |mut addrs, _core_ctx| addrs.next().map(|a| a.addr_sub()))
}

/// Gets the hop limit for new IPv6 packets that will be sent out from `device`.
pub fn get_ipv6_hop_limit<BT: IpDeviceStateBindingsTypes, CC: IpDeviceStateContext<Ipv6, BT>>(
    core_ctx: &mut CC,
    device: &CC::DeviceId,
) -> NonZeroU8 {
    core_ctx.with_default_hop_limit(device, Clone::clone)
}

/// Is IP packet unicast forwarding enabled?
pub fn is_ip_unicast_forwarding_enabled<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> bool {
    core_ctx.with_ip_device_configuration(device_id, |state, _ctx| {
        AsRef::<IpDeviceConfiguration>::as_ref(state).unicast_forwarding_enabled
    })
}

/// Is IP packet multicast forwarding enabled?
pub fn is_ip_multicast_forwarding_enabled<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceConfigurationContext<I, BC>,
>(
    core_ctx: &mut CC,
    device_id: &CC::DeviceId,
) -> bool {
    core_ctx.with_ip_device_configuration(device_id, |state, _ctx| {
        AsRef::<IpDeviceConfiguration>::as_ref(state).multicast_forwarding_enabled
    })
}

/// Joins the multicast group `multicast_addr` on `device_id`.
///
/// `_config` is not used but required to make sure that the caller is currently
/// holding a a reference to the IP device's IP configuration as a way to prove
/// that caller has synchronized this operation with other accesses to the IP
/// device configuration.
pub fn join_ip_multicast_with_config<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
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
pub fn join_ip_multicast<
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

/// Leaves the multicast group `multicast_addr` on `device_id`.
///
/// `_config` is not used but required to make sure that the caller is currently
/// holding a a reference to the IP device's IP configuration as a way to prove
/// that caller has synchronized this operation with other accesses to the IP
/// device configuration.
pub fn leave_ip_multicast_with_config<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
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
pub fn leave_ip_multicast<
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

/// Adds `addr_sub` to `device_id` with configuration `addr_config`.
///
/// `_device_config` is not used but required to make sure that the caller is
/// currently holding a a reference to the IP device's IP configuration as a way
/// to prove that caller has synchronized this operation with other accesses to
/// the IP device configuration.
pub fn add_ip_addr_subnet_with_config<
    I: IpDeviceIpExt,
    BC: IpDeviceBindingsContext<I, CC::DeviceId>,
    CC: IpDeviceStateContext<I, BC> + GmpHandler<I, BC> + DadHandler<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    addr_sub: AddrSubnet<I::Addr, I::AssignedWitness>,
    addr_config: I::AddressConfig<BC::Instant>,
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
        _addr_sub: AddrSubnet<Ipv4Addr, Ipv4DeviceAddr>,
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
            Ipv6AddrConfig::Slaac(Ipv6AddrSlaacConfig { inner, preferred_lifetime: _ }) => {
                SlaacHandler::on_address_removed(
                    self,
                    bindings_ctx,
                    device_id,
                    addr_sub,
                    inner,
                    reason,
                )
            }
            Ipv6AddrConfig::Manual(_manual_config) => (),
        }
    }
}

/// Possible representations of an IP address that is valid for deletion.
#[allow(missing_docs)]
pub enum DelIpAddr<Id, A> {
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

/// Deletes an IP address from a device, returning the address and its
/// configuration if it was removed.
pub fn del_ip_addr_inner<
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
) -> Result<
    (
        AddrSubnet<I::Addr, I::AssignedWitness>,
        I::AddressConfig<BC::Instant>,
        RemoveResourceResultWithContext<AddrSubnet<I::Addr>, BC>,
    ),
    NotFoundError,
> {
    let addr_id = match addr {
        DelIpAddr::SpecifiedAddr(addr) => core_ctx.get_address_id(device_id, addr)?,
        DelIpAddr::AddressId(id) => id,
    };
    DadHandler::stop_duplicate_address_detection(core_ctx, bindings_ctx, device_id, &addr_id);
    // Extract the configuration out of the address to properly mark it as ready
    // for deletion. If the configuration has already been taken, consider as if
    // the address is already removed.
    let addr_config = core_ctx
        .with_ip_address_state_mut(device_id, &addr_id, |addr_state| {
            I::take_addr_config_for_removal(addr_state)
        })
        .ok_or(NotFoundError)?;

    let addr_sub = addr_id.addr_sub();
    let result = core_ctx.remove_ip_address(device_id, addr_id);

    bindings_ctx.on_event(IpDeviceEvent::AddressRemoved {
        device: device_id.clone(),
        addr: addr_sub.addr().into(),
        reason,
    });

    Ok((addr_sub, addr_config, result))
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
) -> Result<RemoveResourceResultWithContext<AddrSubnet<I::Addr>, BC>, NotFoundError> {
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
) -> Result<RemoveResourceResultWithContext<AddrSubnet<I::Addr>, BC>, NotFoundError> {
    del_ip_addr_inner(core_ctx, bindings_ctx, device_id, addr, reason, config).map(
        |(addr_sub, config, result)| {
            core_ctx.on_address_removed(bindings_ctx, device_id, addr_sub, config, reason);
            result
        },
    )
}

/// Returns whether `device_id` is enabled for IP version `I`.
pub fn is_ip_device_enabled<
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
pub fn clear_ipv4_device_state<
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
pub fn clear_ipv6_device_state<
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

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use alloc::boxed::Box;

    use super::*;

    /// Calls the callback with an iterator of the IPv4 addresses assigned to
    /// `device_id`.
    pub fn with_assigned_ipv4_addr_subnets<
        BT: IpDeviceStateBindingsTypes,
        CC: IpDeviceStateContext<Ipv4, BT>,
        O,
        F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<Ipv4Addr, Ipv4DeviceAddr>> + '_>) -> O,
    >(
        core_ctx: &mut CC,
        device_id: &CC::DeviceId,
        cb: F,
    ) -> O {
        core_ctx.with_address_ids(device_id, |addrs, _core_ctx| {
            cb(Box::new(addrs.map(|a| a.addr_sub())))
        })
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
    pub fn with_assigned_ipv6_addr_subnets<
        BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId>,
        CC: Ipv6DeviceContext<BC>,
        O,
        F: FnOnce(Box<dyn Iterator<Item = AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>> + '_>) -> O,
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
                        |Ipv6AddressState { flags: Ipv6AddressFlags { assigned }, config: _ }| {
                            *assigned
                        },
                    )
                    .then(|| addr_id.addr_sub())
            })))
        })
    }

    #[derive(Clone, Debug, Hash, Eq, PartialEq)]
    pub struct FakeWeakAddressId<T>(pub T);

    impl<A: IpAddress, T: IpAddressId<A>> WeakIpAddressId<A> for FakeWeakAddressId<T> {
        type Strong = T;

        fn upgrade(&self) -> Option<Self::Strong> {
            let Self(inner) = self;
            Some(inner.clone())
        }
    }

    impl<A: IpAddress> IpAddressId<A> for AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness>
    where
        A::Version: IpDeviceIpExt,
        SpecifiedAddr<A>: From<<A::Version as IpDeviceIpExt>::AssignedWitness>,
    {
        type Weak = FakeWeakAddressId<Self>;

        fn downgrade(&self) -> Self::Weak {
            FakeWeakAddressId(self.clone())
        }

        fn addr(&self) -> IpDeviceAddr<A> {
            #[derive(GenericOverIp)]
            #[generic_over_ip(I, Ip)]
            struct WrapIn<I: IpDeviceIpExt>(I::AssignedWitness);
            A::Version::map_ip(
                WrapIn(self.addr()),
                |WrapIn(v4_addr)| IpDeviceAddr::new_from_witness(v4_addr),
                |WrapIn(v6_addr)| IpDeviceAddr::new_from_ipv6_device_addr(v6_addr),
            )
        }

        fn addr_sub(&self) -> AddrSubnet<A, <A::Version as IpDeviceIpExt>::AssignedWitness> {
            self.clone()
        }
    }
}
