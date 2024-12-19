// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State for an IP device.

use alloc::vec::Vec;
use core::fmt::Debug;
use core::hash::Hash;
use core::num::{NonZeroU16, NonZeroU8};
use core::ops::{Deref, DerefMut};
use core::time::Duration;

use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::ip::{
    AddrSubnet, GenericOverIp, Ip, IpAddress, IpMarked, IpVersionMarker, Ipv4, Ipv4Addr, Ipv6,
    Ipv6Addr,
};
use netstack3_base::sync::{Mutex, PrimaryRc, RwLock, StrongRc, WeakRc};
use netstack3_base::{
    AssignedAddrIpExt, BroadcastIpExt, CoreTimerContext, ExistsError, Inspectable,
    InspectableValue, Inspector, Instant, InstantBindingsTypes, IpAddressId,
    NestedIntoCoreTimerCtx, NotFoundError, ReferenceNotifiers, TimerBindingsTypes, TimerContext,
    WeakDeviceIdentifier,
};
use packet_formats::icmp::ndp::NonZeroNdpLifetime;
use packet_formats::utils::NonZeroDuration;

use crate::internal::device::dad::DadBindingsTypes;
use crate::internal::device::route_discovery::Ipv6RouteDiscoveryState;
use crate::internal::device::router_solicitation::RsState;
use crate::internal::device::slaac::{SlaacConfiguration, SlaacState};
use crate::internal::device::{
    IpAddressIdSpec, IpDeviceAddr, IpDeviceTimerId, Ipv4DeviceAddr, Ipv4DeviceTimerId,
    Ipv6DeviceAddr, Ipv6DeviceTimerId, WeakIpAddressId,
};
use crate::internal::gmp::igmp::{IgmpConfig, IgmpTimerId, IgmpTypeLayout};
use crate::internal::gmp::mld::{MldConfig, MldTimerId, MldTypeLayout};
use crate::internal::gmp::{GmpGroupState, GmpState, GmpTimerId, GmpTypeLayout, MulticastGroupSet};
use crate::internal::types::RawMetric;

use super::dad::NonceCollection;

/// The default value for *RetransTimer* as defined in [RFC 4861 section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
pub const RETRANS_TIMER_DEFAULT: NonZeroDuration = NonZeroDuration::from_secs(1).unwrap();

/// The default value for the default hop limit to be used when sending IP
/// packets.
const DEFAULT_HOP_LIMIT: NonZeroU8 = NonZeroU8::new(64).unwrap();

/// An `Ip` extension trait adding IP device state properties.
pub trait IpDeviceStateIpExt: BroadcastIpExt {
    /// Information stored about an IP address assigned to an interface.
    type AssignedAddressState<BT: IpDeviceStateBindingsTypes>: AssignedAddressState<Self::Addr>
        + Debug;
    /// The GMP protocol-specific configuration.
    type GmpProtoConfig: Default;
    /// The GMP type layout used by IP-version specific state.
    type GmpTypeLayout<BT: IpDeviceStateBindingsTypes>: GmpTypeLayout<Self, BT>;
    /// The timer id for GMP timers.
    type GmpTimerId<D: WeakDeviceIdentifier>: From<GmpTimerId<Self, D>>;
}

impl IpDeviceStateIpExt for Ipv4 {
    type AssignedAddressState<BT: IpDeviceStateBindingsTypes> = Ipv4AddressEntry<BT>;
    type GmpTimerId<D: WeakDeviceIdentifier> = IgmpTimerId<D>;
    type GmpProtoConfig = IgmpConfig;
    type GmpTypeLayout<BT: IpDeviceStateBindingsTypes> = IgmpTypeLayout;
}

impl IpDeviceStateIpExt for Ipv6 {
    type AssignedAddressState<BT: IpDeviceStateBindingsTypes> = Ipv6AddressEntry<BT>;
    type GmpTimerId<D: WeakDeviceIdentifier> = MldTimerId<D>;
    type GmpProtoConfig = MldConfig;
    type GmpTypeLayout<BT: IpDeviceStateBindingsTypes> = MldTypeLayout;
}

/// The state associated with an IP address assigned to an IP device.
pub trait AssignedAddressState<A: IpAddress>: Debug + Send + Sync + 'static {
    /// Gets the address.
    fn addr(&self) -> IpDeviceAddr<A>;

    /// Gets the address subnet this ID represents.
    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as AssignedAddrIpExt>::AssignedWitness>
    where
        A::Version: AssignedAddrIpExt;
}

impl<BT: IpDeviceStateBindingsTypes> AssignedAddressState<Ipv4Addr> for Ipv4AddressEntry<BT> {
    fn addr(&self) -> IpDeviceAddr<Ipv4Addr> {
        IpDeviceAddr::new_from_witness(self.addr_sub().addr())
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv4Addr, Ipv4DeviceAddr> {
        *self.addr_sub()
    }
}

impl<BT: IpDeviceStateBindingsTypes> AssignedAddressState<Ipv6Addr> for Ipv6AddressEntry<BT> {
    fn addr(&self) -> IpDeviceAddr<Ipv6Addr> {
        IpDeviceAddr::new_from_ipv6_device_addr(self.addr_sub().addr())
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        *self.addr_sub()
    }
}

/// The primary reference to the state associated with an IP address assigned
/// to an IP device.
//
// TODO(https://fxbug.dev/382093426): manually implement `Debug`.
#[derive(Derivative)]
#[derivative(Debug(bound = "S: Debug"))]
pub struct PrimaryAddressId<S>(PrimaryRc<S>);

impl<S> Deref for PrimaryAddressId<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        let Self(inner) = self;
        inner.deref()
    }
}

impl<S> PrimaryAddressId<S> {
    /// Creates a new primary reference to the provided state.
    fn new_with_strong_clone(addr: S) -> (Self, AddressId<S>) {
        let primary = PrimaryRc::new(addr);
        let strong = PrimaryRc::clone_strong(&primary);
        (Self(primary), AddressId(strong))
    }

    /// Clones a strongly-held reference.
    pub fn clone_strong(&self) -> AddressId<S> {
        let Self(inner) = self;
        AddressId(PrimaryRc::clone_strong(inner))
    }

    /// Checks for equality with the provided strongly-held reference.
    pub fn ptr_eq(&self, other: &AddressId<S>) -> bool {
        let Self(inner) = self;
        let AddressId(other) = other;
        PrimaryRc::ptr_eq(inner, other)
    }

    /// Consumes `self` and returns the inner [`PrimaryRc`].
    pub fn into_inner(self) -> PrimaryRc<S> {
        self.0
    }
}

impl<A, S> AssignedAddressState<A> for PrimaryAddressId<S>
where
    A: IpAddress<Version: AssignedAddrIpExt>,
    S: AssignedAddressState<A>,
{
    fn addr(&self) -> IpDeviceAddr<A> {
        let Self(inner) = self;
        inner.addr()
    }

    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as AssignedAddrIpExt>::AssignedWitness> {
        let Self(inner) = self;
        inner.addr_sub()
    }
}

/// A strongly-held reference to an IP address assigned to a device.
//
// TODO(https://fxbug.dev/382093426): manually implement `Debug`.
#[derive(Derivative)]
#[derivative(
    Debug(bound = "S: Debug"),
    Clone(bound = ""),
    Eq(bound = ""),
    Hash(bound = ""),
    PartialEq(bound = "")
)]
pub struct AddressId<S>(StrongRc<S>);

impl<S> Deref for AddressId<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        let Self(inner) = self;
        inner.deref()
    }
}

impl<A, S> IpAddressId<A> for AddressId<S>
where
    A: IpAddress<Version: AssignedAddrIpExt>,
    S: AssignedAddressState<A>,
{
    type Weak = WeakAddressId<S>;

    fn downgrade(&self) -> Self::Weak {
        let Self(inner) = self;
        WeakAddressId(StrongRc::downgrade(inner))
    }

    fn addr(&self) -> IpDeviceAddr<A> {
        let Self(inner) = self;
        inner.addr()
    }

    fn addr_sub(&self) -> AddrSubnet<A, <A::Version as AssignedAddrIpExt>::AssignedWitness> {
        let Self(inner) = self;
        inner.addr_sub()
    }
}

/// A weakly-held reference to an IP address assigned to a device.
//
// TODO(https://fxbug.dev/382093426): manually implement `Debug`.
#[derive(Derivative)]
#[derivative(
    Debug(bound = "S: Debug"),
    Clone(bound = ""),
    Eq(bound = ""),
    Hash(bound = ""),
    PartialEq(bound = "")
)]
pub struct WeakAddressId<S>(WeakRc<S>);

impl<A, S> WeakIpAddressId<A> for WeakAddressId<S>
where
    A: IpAddress<Version: AssignedAddrIpExt>,
    S: AssignedAddressState<A>,
{
    type Strong = AddressId<S>;

    fn upgrade(&self) -> Option<Self::Strong> {
        let Self(inner) = self;
        inner.upgrade().map(AddressId)
    }

    fn is_assigned(&self) -> bool {
        let Self(inner) = self;
        inner.strong_count() != 0
    }
}

/// The flags for an IP device.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct IpDeviceFlags {
    /// Is the device enabled?
    pub ip_enabled: bool,
}

/// The state kept for each device to handle multicast group membership.
pub struct IpDeviceMulticastGroups<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> {
    /// Multicast groups this device has joined.
    pub groups: MulticastGroupSet<I::Addr, GmpGroupState<I, BT>>,
    /// GMP state.
    pub gmp: GmpState<I, I::GmpTypeLayout<BT>, BT>,
    /// GMP protocol-specific configuration.
    pub gmp_config: I::GmpProtoConfig,
}

/// A container for the default hop limit kept by [`IpDeviceState`].
///
/// This type makes the [`OrderedLockAccess`] implementation clearer by
/// newtyping the `NonZeroU8` value and adding a version marker.
#[derive(Copy, Clone, Debug)]
pub struct DefaultHopLimit<I: Ip>(NonZeroU8, IpVersionMarker<I>);

impl<I: Ip> Deref for DefaultHopLimit<I> {
    type Target = NonZeroU8;
    fn deref(&self) -> &NonZeroU8 {
        let Self(value, IpVersionMarker { .. }) = self;
        value
    }
}

impl<I: Ip> DerefMut for DefaultHopLimit<I> {
    fn deref_mut(&mut self) -> &mut NonZeroU8 {
        let Self(value, IpVersionMarker { .. }) = self;
        value
    }
}

impl<I: Ip> Default for DefaultHopLimit<I> {
    fn default() -> Self {
        Self(DEFAULT_HOP_LIMIT, IpVersionMarker::new())
    }
}

/// The state common to all IP devices.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct IpDeviceState<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> {
    /// IP addresses assigned to this device.
    ///
    /// IPv6 addresses may be tentative (performing NDP's Duplicate Address
    /// Detection).
    ///
    /// Does not contain any duplicates.
    addrs: RwLock<IpDeviceAddresses<I, BT>>,

    /// Multicast groups and GMP handling state.
    multicast_groups: RwLock<IpDeviceMulticastGroups<I, BT>>,

    /// The default TTL (IPv4) or hop limit (IPv6) for outbound packets sent
    /// over this device.
    default_hop_limit: RwLock<DefaultHopLimit<I>>,

    /// The flags for this device.
    flags: Mutex<IpMarked<I, IpDeviceFlags>>,
}

impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes>
    OrderedLockAccess<IpDeviceAddresses<I, BT>> for DualStackIpDeviceState<BT>
{
    type Lock = RwLock<IpDeviceAddresses<I, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ip_state::<I>().addrs)
    }
}

impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes>
    OrderedLockAccess<IpDeviceMulticastGroups<I, BT>> for DualStackIpDeviceState<BT>
{
    type Lock = RwLock<IpDeviceMulticastGroups<I, BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ip_state::<I>().multicast_groups)
    }
}

impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> OrderedLockAccess<DefaultHopLimit<I>>
    for DualStackIpDeviceState<BT>
{
    type Lock = RwLock<DefaultHopLimit<I>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ip_state::<I>().default_hop_limit)
    }
}

impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes>
    OrderedLockAccess<IpMarked<I, IpDeviceFlags>> for DualStackIpDeviceState<BT>
{
    type Lock = Mutex<IpMarked<I, IpDeviceFlags>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ip_state::<I>().flags)
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<SlaacState<BT>>
    for DualStackIpDeviceState<BT>
{
    type Lock = Mutex<SlaacState<BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ipv6.slaac_state)
    }
}

impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> IpDeviceState<I, BT> {
    /// A direct accessor to `IpDeviceAddresses` available in tests.
    #[cfg(any(test, feature = "testutils"))]
    pub fn addrs(&self) -> &RwLock<IpDeviceAddresses<I, BT>> {
        &self.addrs
    }
}

impl<I: IpDeviceStateIpExt, BC: IpDeviceStateBindingsTypes + TimerContext> IpDeviceState<I, BC> {
    fn new<D: WeakDeviceIdentifier, CC: CoreTimerContext<I::GmpTimerId<D>, BC>>(
        bindings_ctx: &mut BC,
        device_id: D,
    ) -> IpDeviceState<I, BC> {
        IpDeviceState {
            addrs: Default::default(),
            multicast_groups: RwLock::new(IpDeviceMulticastGroups {
                groups: Default::default(),
                gmp: GmpState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx, device_id),
                gmp_config: Default::default(),
            }),
            default_hop_limit: Default::default(),
            flags: Default::default(),
        }
    }
}

/// A device's IP addresses for IP version `I`.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug))]
pub struct IpDeviceAddresses<I: Ip + IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> {
    addrs: Vec<PrimaryAddressId<I::AssignedAddressState<BT>>>,
}

// TODO(https://fxbug.dev/42165707): Once we figure out what invariants we want to
// hold regarding the set of IP addresses assigned to a device, ensure that all
// of the methods on `IpDeviceAddresses` uphold those invariants.
impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> IpDeviceAddresses<I, BT> {
    /// Iterates over the addresses assigned to this device.
    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &PrimaryAddressId<I::AssignedAddressState<BT>>>
           + ExactSizeIterator
           + Clone {
        self.addrs.iter()
    }

    /// Iterates over strong clones of addresses assigned to this device.
    pub fn strong_iter(&self) -> AddressIdIter<'_, I, BT> {
        AddressIdIter(self.addrs.iter())
    }

    /// Adds an IP address to this interface.
    pub fn add(
        &mut self,
        addr: I::AssignedAddressState<BT>,
    ) -> Result<AddressId<I::AssignedAddressState<BT>>, ExistsError> {
        if self.iter().any(|a| a.addr() == addr.addr()) {
            return Err(ExistsError);
        }
        let (primary, strong) = PrimaryAddressId::new_with_strong_clone(addr);
        self.addrs.push(primary);
        Ok(strong)
    }

    /// Removes the address.
    pub fn remove(
        &mut self,
        addr: &I::Addr,
    ) -> Result<PrimaryAddressId<I::AssignedAddressState<BT>>, NotFoundError> {
        let (index, _entry): (_, &PrimaryAddressId<I::AssignedAddressState<BT>>) = self
            .addrs
            .iter()
            .enumerate()
            .find(|(_, entry)| &entry.addr().addr() == addr)
            .ok_or(NotFoundError)?;
        Ok(self.addrs.remove(index))
    }
}

/// An iterator over address StrongIds. Created from `IpDeviceAddresses`.
pub struct AddressIdIter<'a, I: Ip + IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes>(
    core::slice::Iter<'a, PrimaryAddressId<I::AssignedAddressState<BT>>>,
);

impl<'a, I: Ip + IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> Iterator
    for AddressIdIter<'a, I, BT>
{
    type Item = AddressId<I::AssignedAddressState<BT>>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self(inner) = self;
        inner.next().map(|addr| addr.clone_strong())
    }
}

/// The state common to all IPv4 devices.
pub struct Ipv4DeviceState<BT: IpDeviceStateBindingsTypes> {
    ip_state: IpDeviceState<Ipv4, BT>,
    config: RwLock<Ipv4DeviceConfiguration>,
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv4DeviceConfiguration>
    for DualStackIpDeviceState<BT>
{
    type Lock = RwLock<Ipv4DeviceConfiguration>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ipv4.config)
    }
}

impl<BC: IpDeviceStateBindingsTypes + TimerContext> Ipv4DeviceState<BC> {
    fn new<D: WeakDeviceIdentifier, CC: CoreTimerContext<Ipv4DeviceTimerId<D>, BC>>(
        bindings_ctx: &mut BC,
        device_id: D,
    ) -> Ipv4DeviceState<BC> {
        Ipv4DeviceState {
            ip_state: IpDeviceState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(
                bindings_ctx,
                device_id,
            ),
            config: Default::default(),
        }
    }
}

impl<BT: IpDeviceStateBindingsTypes> AsRef<IpDeviceState<Ipv4, BT>> for Ipv4DeviceState<BT> {
    fn as_ref(&self) -> &IpDeviceState<Ipv4, BT> {
        &self.ip_state
    }
}

impl<BT: IpDeviceStateBindingsTypes> AsMut<IpDeviceState<Ipv4, BT>> for Ipv4DeviceState<BT> {
    fn as_mut(&mut self) -> &mut IpDeviceState<Ipv4, BT> {
        &mut self.ip_state
    }
}

/// Configurations common to all IP devices.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct IpDeviceConfiguration {
    /// Is a Group Messaging Protocol (GMP) enabled for this device?
    ///
    /// If `gmp_enabled` is false, multicast groups will still be added to
    /// `multicast_groups`, but we will not inform the network of our membership
    /// in those groups using a GMP.
    ///
    /// Default: `false`.
    pub gmp_enabled: bool,

    /// A flag indicating whether forwarding of unicast IP packets not destined
    /// for this device is enabled.
    ///
    /// This flag controls whether or not packets can be forwarded from this
    /// device. That is, when a packet arrives at a device it is not destined
    /// for, the packet can only be forwarded if the device it arrived at has
    /// forwarding enabled and there exists another device that has a path to
    /// the packet's destination, regardless of the other device's forwarding
    /// ability.
    ///
    /// Default: `false`.
    pub unicast_forwarding_enabled: bool,

    /// A flag indicating whether forwarding of multicast IP packets received on
    /// this device is enabled.
    ///
    /// This flag controls whether or not packets can be forwarded from this
    /// device. That is, when a multicast packet arrives at this device, the
    /// multicast routing table will be consulted and the packet will be
    /// forwarded out of the matching route's corresponding outbound devices
    /// (regardless of the outbound device's forwarding ability). Enabling
    /// multicast forwarding does not disrupt local delivery: the packet will
    /// both be forwarded and delivered locally.
    ///
    /// Default: `false`.
    pub multicast_forwarding_enabled: bool,
}

/// Configuration common to all IPv4 devices.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Ipv4DeviceConfiguration {
    /// The configuration common to all IP devices.
    pub ip_config: IpDeviceConfiguration,
}

impl AsRef<IpDeviceConfiguration> for Ipv4DeviceConfiguration {
    fn as_ref(&self) -> &IpDeviceConfiguration {
        &self.ip_config
    }
}

impl AsMut<IpDeviceConfiguration> for Ipv4DeviceConfiguration {
    fn as_mut(&mut self) -> &mut IpDeviceConfiguration {
        &mut self.ip_config
    }
}

/// Configuration common to all IPv6 devices.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Ipv6DeviceConfiguration {
    /// The value for NDP's DupAddrDetectTransmits parameter as defined by
    /// [RFC 4862 section 5.1].
    ///
    /// A value of `None` means DAD will not be performed on the interface.
    ///
    /// [RFC 4862 section 5.1]: https://datatracker.ietf.org/doc/html/rfc4862#section-5.1
    // TODO(https://fxbug.dev/42077260): Move to a common place when IPv4
    // supports DAD.
    pub dad_transmits: Option<NonZeroU16>,

    /// Value for NDP's `MAX_RTR_SOLICITATIONS` parameter to configure how many
    /// router solicitation messages to send when solicing routers.
    ///
    /// A value of `None` means router solicitation will not be performed.
    ///
    /// See [RFC 4861 section 6.3.7] for details.
    ///
    /// [RFC 4861 section 6.3.7]: https://datatracker.ietf.org/doc/html/rfc4861#section-6.3.7
    pub max_router_solicitations: Option<NonZeroU8>,

    /// The configuration for SLAAC.
    pub slaac_config: SlaacConfiguration,

    /// The configuration common to all IP devices.
    pub ip_config: IpDeviceConfiguration,
}

impl Ipv6DeviceConfiguration {
    /// The default `MAX_RTR_SOLICITATIONS` value from [RFC 4861 section 10].
    ///
    /// [RFC 4861 section 10]: https://datatracker.ietf.org/doc/html/rfc4861#section-10
    pub const DEFAULT_MAX_RTR_SOLICITATIONS: NonZeroU8 = NonZeroU8::new(3).unwrap();

    /// The default `DupAddrDetectTransmits` value from [RFC 4862 Section 5.1]
    ///
    /// [RFC 4862 Section 5.1]: https://www.rfc-editor.org/rfc/rfc4862#section-5.1
    pub const DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS: NonZeroU16 =
        NonZeroU16::new(1).unwrap();
}

impl AsRef<IpDeviceConfiguration> for Ipv6DeviceConfiguration {
    fn as_ref(&self) -> &IpDeviceConfiguration {
        &self.ip_config
    }
}

impl AsMut<IpDeviceConfiguration> for Ipv6DeviceConfiguration {
    fn as_mut(&mut self) -> &mut IpDeviceConfiguration {
        &mut self.ip_config
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv6NetworkLearnedParameters>
    for DualStackIpDeviceState<BT>
{
    type Lock = RwLock<Ipv6NetworkLearnedParameters>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ipv6.learned_params)
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv6RouteDiscoveryState<BT>>
    for DualStackIpDeviceState<BT>
{
    type Lock = Mutex<Ipv6RouteDiscoveryState<BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ipv6.route_discovery)
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<RsState<BT>> for DualStackIpDeviceState<BT> {
    type Lock = Mutex<RsState<BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ipv6.router_solicitations)
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv6DeviceConfiguration>
    for DualStackIpDeviceState<BT>
{
    type Lock = RwLock<Ipv6DeviceConfiguration>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.ipv6.config)
    }
}

/// IPv6 device parameters that can be learned from router advertisements.
#[derive(Default)]
pub struct Ipv6NetworkLearnedParameters {
    /// The time between retransmissions of Neighbor Solicitation messages to a
    /// neighbor when resolving the address or when probing the reachability of
    /// a neighbor.
    ///
    ///
    /// See RetransTimer in [RFC 4861 section 6.3.2] for more details.
    ///
    /// [RFC 4861 section 6.3.2]: https://tools.ietf.org/html/rfc4861#section-6.3.2
    pub retrans_timer: Option<NonZeroDuration>,
}

impl Ipv6NetworkLearnedParameters {
    /// Returns the learned retransmission timer or the default stack value if a
    /// network-learned one isn't available.
    pub fn retrans_timer_or_default(&self) -> NonZeroDuration {
        self.retrans_timer.clone().unwrap_or(RETRANS_TIMER_DEFAULT)
    }
}

/// The state common to all IPv6 devices.
pub struct Ipv6DeviceState<BT: IpDeviceStateBindingsTypes> {
    learned_params: RwLock<Ipv6NetworkLearnedParameters>,
    route_discovery: Mutex<Ipv6RouteDiscoveryState<BT>>,
    router_solicitations: Mutex<RsState<BT>>,
    ip_state: IpDeviceState<Ipv6, BT>,
    config: RwLock<Ipv6DeviceConfiguration>,
    slaac_state: Mutex<SlaacState<BT>>,
}

impl<BC: IpDeviceStateBindingsTypes + TimerContext> Ipv6DeviceState<BC> {
    pub fn new<
        D: WeakDeviceIdentifier,
        A: WeakIpAddressId<Ipv6Addr>,
        CC: CoreTimerContext<Ipv6DeviceTimerId<D, A>, BC>,
    >(
        bindings_ctx: &mut BC,
        device_id: D,
    ) -> Self {
        Ipv6DeviceState {
            learned_params: Default::default(),
            route_discovery: Mutex::new(Ipv6RouteDiscoveryState::new::<
                _,
                NestedIntoCoreTimerCtx<CC, _>,
            >(bindings_ctx, device_id.clone())),
            router_solicitations: Default::default(),
            ip_state: IpDeviceState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(
                bindings_ctx,
                device_id.clone(),
            ),
            config: Default::default(),
            slaac_state: Mutex::new(SlaacState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(
                bindings_ctx,
                device_id,
            )),
        }
    }
}

impl<BT: IpDeviceStateBindingsTypes> AsRef<IpDeviceState<Ipv6, BT>> for Ipv6DeviceState<BT> {
    fn as_ref(&self) -> &IpDeviceState<Ipv6, BT> {
        &self.ip_state
    }
}

impl<BT: IpDeviceStateBindingsTypes> AsMut<IpDeviceState<Ipv6, BT>> for Ipv6DeviceState<BT> {
    fn as_mut(&mut self) -> &mut IpDeviceState<Ipv6, BT> {
        &mut self.ip_state
    }
}

/// Bindings types required for IP device state.
pub trait IpDeviceStateBindingsTypes:
    InstantBindingsTypes + TimerBindingsTypes + ReferenceNotifiers + 'static
{
}
impl<BT> IpDeviceStateBindingsTypes for BT where
    BT: InstantBindingsTypes + TimerBindingsTypes + ReferenceNotifiers + 'static
{
}

/// IPv4 and IPv6 state combined.
pub struct DualStackIpDeviceState<BT: IpDeviceStateBindingsTypes> {
    /// IPv4 state.
    ipv4: Ipv4DeviceState<BT>,

    /// IPv6 state.
    ipv6: Ipv6DeviceState<BT>,

    /// The device's routing metric.
    metric: RawMetric,
}

impl<BC: IpDeviceStateBindingsTypes + TimerContext> DualStackIpDeviceState<BC> {
    /// Constructs a new `DualStackIpDeviceState` for `device_id`.
    ///
    /// `metric` is the default metric used by routes through this device.
    pub fn new<
        D: WeakDeviceIdentifier,
        A: IpAddressIdSpec,
        CC: CoreTimerContext<IpDeviceTimerId<Ipv6, D, A>, BC>
            + CoreTimerContext<IpDeviceTimerId<Ipv4, D, A>, BC>,
    >(
        bindings_ctx: &mut BC,
        device_id: D,
        metric: RawMetric,
    ) -> Self {
        Self {
            ipv4: Ipv4DeviceState::new::<D, NestedIntoCoreTimerCtx<CC, IpDeviceTimerId<Ipv4, D, A>>>(
                bindings_ctx,
                device_id.clone(),
            ),
            ipv6: Ipv6DeviceState::new::<
                D,
                A::WeakV6,
                NestedIntoCoreTimerCtx<CC, IpDeviceTimerId<Ipv6, D, A>>,
            >(bindings_ctx, device_id),
            metric,
        }
    }
}

impl<BT: IpDeviceStateBindingsTypes> DualStackIpDeviceState<BT> {
    /// Returns the [`RawMetric`] for this device.
    pub fn metric(&self) -> &RawMetric {
        &self.metric
    }

    /// Returns the IP device state for version `I`.
    pub fn ip_state<I: IpDeviceStateIpExt>(&self) -> &IpDeviceState<I, BT> {
        I::map_ip_out(
            self,
            |dual_stack| &dual_stack.ipv4.ip_state,
            |dual_stack| &dual_stack.ipv6.ip_state,
        )
    }
}

/// The various states DAD may be in for an address.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub enum Ipv6DadState<BT: DadBindingsTypes> {
    /// The address is assigned to an interface and can be considered bound to
    /// it (all packets destined to the address will be accepted).
    Assigned,

    /// The address is considered unassigned to an interface for normal
    /// operations, but has the intention of being assigned in the future (e.g.
    /// once NDP's Duplicate Address Detection is completed).
    ///
    /// When `dad_transmits_remaining` is `None`, then no more DAD messages need
    /// to be sent and DAD may be resolved.
    #[allow(missing_docs)]
    Tentative {
        dad_transmits_remaining: Option<NonZeroU16>,
        timer: BT::Timer,
        nonces: NonceCollection,
        /// Initialized to false, and exists as a sentinel so that extra
        /// transmits are added only after the first looped-back probe is
        /// detected.
        added_extra_transmits_after_detecting_looped_back_ns: bool,
    },

    /// The address has not yet been initialized.
    Uninitialized,
}

/// Configuration for a temporary IPv6 address assigned via SLAAC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemporarySlaacConfig<Instant> {
    /// The time at which the address is no longer valid.
    pub valid_until: Instant,
    /// The per-address DESYNC_FACTOR specified in RFC 8981 Section 3.4.
    pub desync_factor: Duration,
    /// The time at which the address was created.
    pub creation_time: Instant,
    /// The DAD_Counter parameter specified by RFC 8981 Section 3.3.2.1. This is
    /// used to track the number of retries that occurred prior to picking this
    /// address.
    pub dad_counter: u8,
}

/// A lifetime that may be forever/infinite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lifetime<I> {
    /// A finite lifetime.
    Finite(I),
    /// An infinite lifetime.
    Infinite,
}

impl<I: Instant> InspectableValue for Lifetime<I> {
    fn record<N: Inspector>(&self, name: &str, inspector: &mut N) {
        match self {
            Self::Finite(instant) => inspector.record_inspectable_value(name, instant),
            Self::Infinite => inspector.record_str(name, "infinite"),
        }
    }
}

impl<I: Instant> Lifetime<I> {
    /// Creates a new `Lifetime` `duration` from `now`, saturating to
    /// `Infinite`.
    pub fn from_ndp(now: I, duration: NonZeroNdpLifetime) -> Self {
        match duration {
            NonZeroNdpLifetime::Finite(d) => Self::after(now, d.get()),
            NonZeroNdpLifetime::Infinite => Self::Infinite,
        }
    }

    /// Creates a new `Lifetime` `duration` from `now`, saturating to
    /// `Infinite`.
    pub fn after(now: I, duration: Duration) -> Self {
        match now.checked_add(duration) {
            Some(i) => Self::Finite(i),
            None => Self::Infinite,
        }
    }
}

impl<Instant> Lifetime<Instant> {
    /// Maps the instant value in this `Lifetime` with `f`.
    pub fn map_instant<N, F: FnOnce(Instant) -> N>(self, f: F) -> Lifetime<N> {
        match self {
            Self::Infinite => Lifetime::Infinite,
            Self::Finite(i) => Lifetime::Finite(f(i)),
        }
    }
}

/// An address' preferred lifetime information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PreferredLifetime<Instant> {
    /// Address is preferred. It can be used for new connections.
    ///
    /// `Lifetime` indicates until when the address is expected to remain in
    /// preferred state.
    Preferred(Lifetime<Instant>),
    /// Address is deprecated, it should not be used for new connections.
    Deprecated,
}

impl<Instant> PreferredLifetime<Instant> {
    /// Creates a new `PreferredLifetime` that is preferred until `instant`.
    pub const fn preferred_until(instant: Instant) -> Self {
        Self::Preferred(Lifetime::Finite(instant))
    }

    /// Creates a new `PreferredLifetime` that is preferred with an infinite
    /// lifetime.
    pub const fn preferred_forever() -> Self {
        Self::Preferred(Lifetime::Infinite)
    }

    /// Returns true if the address is deprecated.
    pub const fn is_deprecated(&self) -> bool {
        match self {
            Self::Preferred(_) => false,
            Self::Deprecated => true,
        }
    }

    /// Maps the instant value in this `PreferredLifetime` with `f`.
    pub fn map_instant<N, F: FnOnce(Instant) -> N>(self, f: F) -> PreferredLifetime<N> {
        match self {
            Self::Deprecated => PreferredLifetime::Deprecated,
            Self::Preferred(l) => PreferredLifetime::Preferred(l.map_instant(f)),
        }
    }
}

impl<I: Instant> PreferredLifetime<I> {
    /// Creates a new `PreferredLifetime` that is preferred for `duration` after
    /// `now`.
    pub fn preferred_for(now: I, duration: NonZeroNdpLifetime) -> Self {
        Self::Preferred(Lifetime::from_ndp(now, duration))
    }

    /// Creates a new `PreferredLifetime` that is preferred for `duration` after
    /// `now` if it is `Some`, `deprecated` otherwise.
    pub fn maybe_preferred_for(now: I, duration: Option<NonZeroNdpLifetime>) -> Self {
        match duration {
            Some(d) => Self::preferred_for(now, d),
            None => Self::Deprecated,
        }
    }
}

impl<Instant> Default for PreferredLifetime<Instant> {
    fn default() -> Self {
        Self::Preferred(Lifetime::Infinite)
    }
}

impl<I: Instant> InspectableValue for PreferredLifetime<I> {
    fn record<N: Inspector>(&self, name: &str, inspector: &mut N) {
        match self {
            Self::Deprecated => inspector.record_str(name, "deprecated"),
            Self::Preferred(lifetime) => inspector.record_inspectable_value(name, lifetime),
        }
    }
}

/// Address properties common to IPv4 and IPv6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommonAddressProperties<Instant> {
    /// The lifetime for which the address is valid.
    pub valid_until: Lifetime<Instant>,
    /// The address' preferred lifetime.
    ///
    /// Address preferred status is used in IPv6 source address selection. IPv4
    /// addresses simply keep this information on behalf of bindings.
    ///
    /// Note that core does not deprecate manual addresses automatically. Manual
    /// addresses are fully handled outside of core.
    pub preferred_lifetime: PreferredLifetime<Instant>,
}

impl<I> Default for CommonAddressProperties<I> {
    fn default() -> Self {
        Self {
            valid_until: Lifetime::Infinite,
            preferred_lifetime: PreferredLifetime::preferred_forever(),
        }
    }
}

impl<Inst: Instant> Inspectable for CommonAddressProperties<Inst> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { valid_until, preferred_lifetime } = self;
        inspector.record_inspectable_value("ValidUntil", valid_until);
        inspector.record_inspectable_value("PreferredLifetime", preferred_lifetime);
    }
}

/// The configuration for an IPv4 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv4AddrConfig<Instant> {
    /// IP version agnostic properties.
    pub common: CommonAddressProperties<Instant>,
}

impl<I> Default for Ipv4AddrConfig<I> {
    fn default() -> Self {
        Self { common: Default::default() }
    }
}

/// Data associated with an IPv4 address on an interface.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Ipv4AddressEntry<BT: IpDeviceStateBindingsTypes> {
    addr_sub: AddrSubnet<Ipv4Addr, Ipv4DeviceAddr>,
    state: RwLock<Ipv4AddressState<BT::Instant>>,
}

impl<BT: IpDeviceStateBindingsTypes> Ipv4AddressEntry<BT> {
    /// Constructs a new `Ipv4AddressEntry`.
    pub fn new(
        addr_sub: AddrSubnet<Ipv4Addr, Ipv4DeviceAddr>,
        config: Ipv4AddrConfig<BT::Instant>,
    ) -> Self {
        Self { addr_sub, state: RwLock::new(Ipv4AddressState { config: Some(config) }) }
    }

    /// This entry's address and subnet.
    pub fn addr_sub(&self) -> &AddrSubnet<Ipv4Addr, Ipv4DeviceAddr> {
        &self.addr_sub
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv4AddressState<BT::Instant>>
    for Ipv4AddressEntry<BT>
{
    type Lock = RwLock<Ipv4AddressState<BT::Instant>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.state)
    }
}

/// The state associated with an IPv4 device address.
#[derive(Debug)]
pub struct Ipv4AddressState<Instant> {
    pub(crate) config: Option<Ipv4AddrConfig<Instant>>,
}

impl<Inst: Instant> Inspectable for Ipv4AddressState<Inst> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { config } = self;
        if let Some(Ipv4AddrConfig { common }) = config {
            inspector.delegate_inspectable(common);
        }
    }
}

/// Configuration for an IPv6 address assigned via SLAAC that varies based on
/// whether the address is static or temporary
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlaacConfig<Instant> {
    /// The address is static.
    Static {
        /// The lifetime of the address.
        valid_until: Lifetime<Instant>,
    },
    /// The address is a temporary address, as specified by [RFC 8981].
    ///
    /// [RFC 8981]: https://tools.ietf.org/html/rfc8981
    Temporary(TemporarySlaacConfig<Instant>),
}

impl<Instant: Copy> SlaacConfig<Instant> {
    /// The lifetime for which the address is valid.
    pub fn valid_until(&self) -> Lifetime<Instant> {
        match self {
            SlaacConfig::Static { valid_until } => *valid_until,
            SlaacConfig::Temporary(TemporarySlaacConfig {
                valid_until,
                desync_factor: _,
                creation_time: _,
                dad_counter: _,
            }) => Lifetime::Finite(*valid_until),
        }
    }
}

/// The configuration for an IPv6 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ipv6AddrConfig<Instant> {
    /// Configured by stateless address autoconfiguration.
    Slaac(Ipv6AddrSlaacConfig<Instant>),

    /// Manually configured.
    Manual(Ipv6AddrManualConfig<Instant>),
}

impl<Instant> Default for Ipv6AddrConfig<Instant> {
    fn default() -> Self {
        Self::Manual(Default::default())
    }
}

/// The common configuration for a SLAAC-assigned IPv6 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv6AddrSlaacConfig<Instant> {
    /// The inner slaac config, tracking static and temporary behavior.
    pub inner: SlaacConfig<Instant>,
    /// The address' preferred lifetime.
    pub preferred_lifetime: PreferredLifetime<Instant>,
}

/// The configuration for a manually-assigned IPv6 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv6AddrManualConfig<Instant> {
    /// IP version agnostic properties.
    pub common: CommonAddressProperties<Instant>,
    /// True if the address is temporary.
    ///
    /// Used in source address selection.
    pub temporary: bool,
}

impl<Instant> Default for Ipv6AddrManualConfig<Instant> {
    fn default() -> Self {
        Self { common: Default::default(), temporary: false }
    }
}

impl<Instant> From<Ipv6AddrManualConfig<Instant>> for Ipv6AddrConfig<Instant> {
    fn from(value: Ipv6AddrManualConfig<Instant>) -> Self {
        Self::Manual(value)
    }
}

impl<Instant: Copy> Ipv6AddrConfig<Instant> {
    /// The configuration for a link-local address configured via SLAAC.
    ///
    /// Per [RFC 4862 Section 5.3]: "A link-local address has an infinite preferred and valid
    /// lifetime; it is never timed out."
    ///
    /// [RFC 4862 Section 5.3]: https://tools.ietf.org/html/rfc4862#section-5.3
    pub(crate) const SLAAC_LINK_LOCAL: Self = Self::Slaac(Ipv6AddrSlaacConfig {
        inner: SlaacConfig::Static { valid_until: Lifetime::Infinite },
        preferred_lifetime: PreferredLifetime::Preferred(Lifetime::Infinite),
    });

    /// The lifetime for which the address is valid.
    pub fn valid_until(&self) -> Lifetime<Instant> {
        match self {
            Ipv6AddrConfig::Slaac(Ipv6AddrSlaacConfig { inner, .. }) => inner.valid_until(),
            Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { common, .. }) => common.valid_until,
        }
    }

    /// Returns the preferred lifetime for this address.
    pub fn preferred_lifetime(&self) -> PreferredLifetime<Instant> {
        match self {
            Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { common, .. }) => {
                common.preferred_lifetime
            }
            Ipv6AddrConfig::Slaac(Ipv6AddrSlaacConfig { preferred_lifetime, .. }) => {
                *preferred_lifetime
            }
        }
    }

    /// Returns true if the address is deprecated.
    pub fn is_deprecated(&self) -> bool {
        self.preferred_lifetime().is_deprecated()
    }

    /// Returns true if the address is temporary.
    pub fn is_temporary(&self) -> bool {
        match self {
            Ipv6AddrConfig::Slaac(Ipv6AddrSlaacConfig { inner, .. }) => match inner {
                SlaacConfig::Static { .. } => false,
                SlaacConfig::Temporary(_) => true,
            },
            Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { temporary, .. }) => *temporary,
        }
    }
}

/// Flags associated with an IPv6 device address.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ipv6AddressFlags {
    /// True if the address is assigned.
    pub assigned: bool,
}

/// The state associated with an IPv6 device address.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ipv6AddressState<Instant> {
    /// The address' flags.
    pub flags: Ipv6AddressFlags,
    /// The address' configuration. `None` when the address is being removed.
    pub config: Option<Ipv6AddrConfig<Instant>>,
}

impl<Inst: Instant> Inspectable for Ipv6AddressState<Inst> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { flags: Ipv6AddressFlags { assigned }, config } = self;
        inspector.record_bool("Assigned", *assigned);

        if let Some(config) = config {
            let is_slaac = match config {
                Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { .. }) => false,
                Ipv6AddrConfig::Slaac(Ipv6AddrSlaacConfig { inner, preferred_lifetime: _ }) => {
                    match inner {
                        SlaacConfig::Static { valid_until: _ } => {}
                        SlaacConfig::Temporary(TemporarySlaacConfig {
                            valid_until: _,
                            desync_factor,
                            creation_time,
                            dad_counter,
                        }) => {
                            // Record the extra temporary slaac configuration before
                            // returning.
                            inspector
                                .record_double("DesyncFactorSecs", desync_factor.as_secs_f64());
                            inspector.record_uint("DadCounter", *dad_counter);
                            inspector.record_inspectable_value("CreationTime", creation_time);
                        }
                    }
                    true
                }
            };
            inspector.record_bool("IsSlaac", is_slaac);
            inspector.record_inspectable_value("ValidUntil", &config.valid_until());
            inspector.record_inspectable_value("PreferredLifetime", &config.preferred_lifetime());
            inspector.record_bool("Temporary", config.is_temporary());
        }
    }
}

/// Data associated with an IPv6 address on an interface.
// TODO(https://fxbug.dev/42173351): Should this be generalized for loopback?
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Ipv6AddressEntry<BT: IpDeviceStateBindingsTypes> {
    addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    dad_state: Mutex<Ipv6DadState<BT>>,
    state: RwLock<Ipv6AddressState<BT::Instant>>,
}

impl<BT: IpDeviceStateBindingsTypes> Ipv6AddressEntry<BT> {
    /// Constructs a new `Ipv6AddressEntry`.
    pub fn new(
        addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        dad_state: Ipv6DadState<BT>,
        config: Ipv6AddrConfig<BT::Instant>,
    ) -> Self {
        let assigned = match dad_state {
            Ipv6DadState::Assigned => true,
            Ipv6DadState::Tentative { .. } | Ipv6DadState::Uninitialized => false,
        };

        Self {
            addr_sub,
            dad_state: Mutex::new(dad_state),
            state: RwLock::new(Ipv6AddressState {
                config: Some(config),
                flags: Ipv6AddressFlags { assigned },
            }),
        }
    }

    /// This entry's address and subnet.
    pub fn addr_sub(&self) -> &AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        &self.addr_sub
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv6DadState<BT>> for Ipv6AddressEntry<BT> {
    type Lock = Mutex<Ipv6DadState<BT>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.dad_state)
    }
}

impl<BT: IpDeviceStateBindingsTypes> OrderedLockAccess<Ipv6AddressState<BT::Instant>>
    for Ipv6AddressEntry<BT>
{
    type Lock = RwLock<Ipv6AddressState<BT::Instant>>;
    fn ordered_lock_access(&self) -> OrderedLockRef<'_, Self::Lock> {
        OrderedLockRef::new(&self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use netstack3_base::testutil::{FakeBindingsCtx, FakeInstant};
    use test_case::test_case;

    type FakeBindingsCtxImpl = FakeBindingsCtx<(), (), (), ()>;

    #[test_case(Lifetime::Infinite ; "with infinite valid_until")]
    #[test_case(Lifetime::Finite(FakeInstant::from(Duration::from_secs(1))); "with finite valid_until")]
    fn test_add_addr_ipv4(valid_until: Lifetime<FakeInstant>) {
        const ADDRESS: Ipv4Addr = Ipv4Addr::new([1, 2, 3, 4]);
        const PREFIX_LEN: u8 = 8;

        let mut ipv4 = IpDeviceAddresses::<Ipv4, FakeBindingsCtxImpl>::default();
        let config = Ipv4AddrConfig {
            common: CommonAddressProperties { valid_until, ..Default::default() },
        };

        let _: AddressId<_> = ipv4
            .add(Ipv4AddressEntry::new(AddrSubnet::new(ADDRESS, PREFIX_LEN).unwrap(), config))
            .unwrap();
        // Adding the same address with different prefix should fail.
        assert_eq!(
            ipv4.add(Ipv4AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN + 1).unwrap(),
                config,
            ))
            .unwrap_err(),
            ExistsError
        );
    }

    #[test_case(Lifetime::Infinite ; "with infinite valid_until")]
    #[test_case(Lifetime::Finite(FakeInstant::from(Duration::from_secs(1))); "with finite valid_until")]
    fn test_add_addr_ipv6(valid_until: Lifetime<FakeInstant>) {
        const ADDRESS: Ipv6Addr =
            Ipv6Addr::from_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4, 5, 6]);
        const PREFIX_LEN: u8 = 8;

        let mut ipv6 = IpDeviceAddresses::<Ipv6, FakeBindingsCtxImpl>::default();

        let mut bindings_ctx = FakeBindingsCtxImpl::default();

        let _: AddressId<_> = ipv6
            .add(Ipv6AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN).unwrap(),
                Ipv6DadState::Tentative {
                    dad_transmits_remaining: None,
                    timer: bindings_ctx.new_timer(()),
                    nonces: Default::default(),
                    added_extra_transmits_after_detecting_looped_back_ns: false,
                },
                Ipv6AddrConfig::Slaac(Ipv6AddrSlaacConfig {
                    inner: SlaacConfig::Static { valid_until },
                    preferred_lifetime: PreferredLifetime::Preferred(Lifetime::Infinite),
                }),
            ))
            .unwrap();
        // Adding the same address with different prefix and configuration
        // should fail.
        assert_eq!(
            ipv6.add(Ipv6AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN + 1).unwrap(),
                Ipv6DadState::Assigned,
                Ipv6AddrConfig::Manual(Ipv6AddrManualConfig {
                    common: CommonAddressProperties { valid_until, ..Default::default() },
                    ..Default::default()
                }),
            ))
            .unwrap_err(),
            ExistsError,
        );
    }
}
