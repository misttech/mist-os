// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State for an IP device.

use alloc::vec::Vec;
use core::{
    fmt::Debug,
    hash::Hash,
    num::{NonZeroU16, NonZeroU8},
    ops::{Deref, DerefMut},
    time::Duration,
};

use const_unwrap::const_unwrap_option;
use derivative::Derivative;
use lock_order::lock::{OrderedLockAccess, OrderedLockRef};
use net_types::{
    ip::{
        AddrSubnet, GenericOverIp, Ip, IpAddress, IpInvariant, IpMarked, IpVersionMarker, Ipv4,
        Ipv4Addr, Ipv6, Ipv6Addr,
    },
    SpecifiedAddr,
};
use packet_formats::utils::NonZeroDuration;

use crate::{
    context::{
        CoreTimerContext, InstantBindingsTypes, NestedIntoCoreTimerCtx, ReferenceNotifiers,
        TimerBindingsTypes, TimerContext,
    },
    device::WeakDeviceIdentifier,
    inspect::{Inspectable, InspectableValue, Inspector},
    ip::{
        device::{
            dad::DadBindingsTypes,
            route_discovery::Ipv6RouteDiscoveryState,
            router_solicitation::RsState,
            slaac::{SlaacConfiguration, SlaacState},
            IpAddressId, IpAddressIdSpec, IpDeviceAddr, IpDeviceTimerId, Ipv4DeviceTimerId,
            Ipv6DeviceAddr, Ipv6DeviceTimerId, WeakIpAddressId,
        },
        gmp::{
            igmp::{IgmpGroupState, IgmpState, IgmpTimerId},
            mld::{MldGroupState, MldTimerId},
            GmpDelayedReportTimerId, GmpState, MulticastGroupSet,
        },
        types::{IpTypesIpExt, RawMetric},
    },
    sync::{Mutex, PrimaryRc, RwLock, StrongRc, WeakRc},
    Instant,
};

/// The default value for *RetransTimer* as defined in [RFC 4861 section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
pub(crate) const RETRANS_TIMER_DEFAULT: NonZeroDuration =
    const_unwrap_option(NonZeroDuration::from_secs(1));

/// The default value for the default hop limit to be used when sending IP
/// packets.
const DEFAULT_HOP_LIMIT: NonZeroU8 = const_unwrap_option(NonZeroU8::new(64));

/// An `Ip` extension trait adding IP device state properties.
pub trait IpDeviceStateIpExt: Ip + IpTypesIpExt {
    /// The information stored about an IP address assigned to an interface.
    type AssignedAddress<BT: IpDeviceStateBindingsTypes>: AssignedAddress<Self::Addr> + Debug;
    /// The per-group state kept by the Group Messaging Protocol (GMP) used to announce
    /// membership in an IP multicast group for this version of IP.
    ///
    /// Note that a GMP is only used when membership must be explicitly
    /// announced. For example, a GMP is not used in the context of a loopback
    /// device (because there are no remote hosts) or in the context of an IPsec
    /// device (because multicast is not supported).
    type GmpGroupState<I: Instant>;
    /// The GMP protocol-specific state.
    type GmpProtoState<BT: IpDeviceStateBindingsTypes>;
    /// The timer id for GMP timers.
    type GmpTimerId<D: WeakDeviceIdentifier>: From<GmpDelayedReportTimerId<Self, D>>;

    /// Creates a new [`Self::GmpProtoState`].
    fn new_gmp_state<
        D: WeakDeviceIdentifier,
        CC: CoreTimerContext<Self::GmpTimerId<D>, BC>,
        BC: IpDeviceStateBindingsTypes + TimerContext,
    >(
        bindings_ctx: &mut BC,
        device_id: D,
    ) -> Self::GmpProtoState<BC>;
}

impl IpDeviceStateIpExt for Ipv4 {
    type AssignedAddress<BT: IpDeviceStateBindingsTypes> = Ipv4AddressEntry<BT>;
    type GmpProtoState<BT: IpDeviceStateBindingsTypes> = IgmpState<BT>;
    type GmpGroupState<I: Instant> = IgmpGroupState<I>;
    type GmpTimerId<D: WeakDeviceIdentifier> = IgmpTimerId<D>;

    fn new_gmp_state<
        D: WeakDeviceIdentifier,
        CC: CoreTimerContext<Self::GmpTimerId<D>, BC>,
        BC: IpDeviceStateBindingsTypes + TimerContext,
    >(
        bindings_ctx: &mut BC,
        device_id: D,
    ) -> Self::GmpProtoState<BC> {
        IgmpState::new::<_, CC>(bindings_ctx, device_id)
    }
}

impl<BT: IpDeviceStateBindingsTypes> IpAddressId<Ipv4Addr> for StrongRc<Ipv4AddressEntry<BT>> {
    type Weak = WeakRc<Ipv4AddressEntry<BT>>;

    fn downgrade(&self) -> Self::Weak {
        StrongRc::downgrade(self)
    }

    fn addr(&self) -> IpDeviceAddr<Ipv4Addr> {
        IpDeviceAddr::new_ipv4_specified(self.addr_sub.addr())
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv4Addr> {
        self.addr_sub
    }
}

impl<BT: IpDeviceStateBindingsTypes> WeakIpAddressId<Ipv4Addr> for WeakRc<Ipv4AddressEntry<BT>> {
    type Strong = StrongRc<Ipv4AddressEntry<BT>>;
    fn upgrade(&self) -> Option<Self::Strong> {
        self.upgrade()
    }
}

impl<BT: IpDeviceStateBindingsTypes> IpAddressId<Ipv6Addr> for StrongRc<Ipv6AddressEntry<BT>> {
    type Weak = WeakRc<Ipv6AddressEntry<BT>>;

    fn downgrade(&self) -> Self::Weak {
        StrongRc::downgrade(self)
    }

    fn addr(&self) -> IpDeviceAddr<Ipv6Addr> {
        IpDeviceAddr::new_from_ipv6_device_addr(self.addr_sub.addr())
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        self.addr_sub
    }
}

impl<BT: IpDeviceStateBindingsTypes> WeakIpAddressId<Ipv6Addr> for WeakRc<Ipv6AddressEntry<BT>> {
    type Strong = StrongRc<Ipv6AddressEntry<BT>>;
    fn upgrade(&self) -> Option<Self::Strong> {
        self.upgrade()
    }
}

impl IpDeviceStateIpExt for Ipv6 {
    type AssignedAddress<BT: IpDeviceStateBindingsTypes> = Ipv6AddressEntry<BT>;
    type GmpProtoState<BT: IpDeviceStateBindingsTypes> = ();
    type GmpGroupState<I: Instant> = MldGroupState<I>;
    type GmpTimerId<D: WeakDeviceIdentifier> = MldTimerId<D>;

    fn new_gmp_state<
        D: WeakDeviceIdentifier,
        CC: CoreTimerContext<Self::GmpTimerId<D>, BC>,
        BC: IpDeviceStateBindingsTypes + TimerContext,
    >(
        _bindings_ctx: &mut BC,
        _device_id: D,
    ) -> Self::GmpProtoState<BC> {
        ()
    }
}

/// The state associated with an IP address assigned to an IP device.
pub trait AssignedAddress<A: IpAddress> {
    /// Gets the address.
    fn addr(&self) -> IpDeviceAddr<A>;
}

impl<BT: IpDeviceStateBindingsTypes> AssignedAddress<Ipv4Addr> for Ipv4AddressEntry<BT> {
    fn addr(&self) -> IpDeviceAddr<Ipv4Addr> {
        IpDeviceAddr::new_ipv4_specified(self.addr_sub().addr())
    }
}

impl<BT: IpDeviceStateBindingsTypes> AssignedAddress<Ipv6Addr> for Ipv6AddressEntry<BT> {
    fn addr(&self) -> IpDeviceAddr<Ipv6Addr> {
        IpDeviceAddr::new_from_ipv6_device_addr(self.addr_sub().addr())
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
    pub groups: MulticastGroupSet<I::Addr, I::GmpGroupState<BT::Instant>>,
    /// Protocol-specific GMP state.
    pub gmp_proto: I::GmpProtoState<BT>,
    /// GMP state.
    pub gmp: GmpState<I, BT>,
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
                gmp_proto: I::new_gmp_state::<_, CC, _>(bindings_ctx, device_id.clone()),
                gmp: GmpState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(bindings_ctx, device_id),
            }),
            default_hop_limit: Default::default(),
            flags: Default::default(),
        }
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug))]
pub struct IpDeviceAddresses<I: Ip + IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> {
    addrs: Vec<PrimaryRc<I::AssignedAddress<BT>>>,
}

// TODO(https://fxbug.dev/42165707): Once we figure out what invariants we want to
// hold regarding the set of IP addresses assigned to a device, ensure that all
// of the methods on `IpDeviceAddresses` uphold those invariants.
impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> IpDeviceAddresses<I, BT> {
    /// Iterates over the addresses assigned to this device.
    pub(crate) fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &PrimaryRc<I::AssignedAddress<BT>>> + ExactSizeIterator + Clone
    {
        self.addrs.iter()
    }

    /// Iterates over strong clones of addresses assigned to this device.
    pub(crate) fn strong_iter(&self) -> AddressIdIter<'_, I, BT> {
        AddressIdIter(self.addrs.iter())
    }

    /// Adds an IP address to this interface.
    pub(crate) fn add(
        &mut self,
        addr: I::AssignedAddress<BT>,
    ) -> Result<StrongRc<I::AssignedAddress<BT>>, crate::error::ExistsError> {
        if self.iter().any(|a| a.addr() == addr.addr()) {
            return Err(crate::error::ExistsError);
        }
        let primary = PrimaryRc::new(addr);
        let strong = PrimaryRc::clone_strong(&primary);
        self.addrs.push(primary);
        Ok(strong)
    }

    /// Removes the address.
    pub(crate) fn remove(
        &mut self,
        addr: &I::Addr,
    ) -> Result<PrimaryRc<I::AssignedAddress<BT>>, crate::error::NotFoundError> {
        let (index, _entry): (_, &PrimaryRc<I::AssignedAddress<BT>>) = self
            .addrs
            .iter()
            .enumerate()
            .find(|(_, entry)| &entry.addr().addr() == addr)
            .ok_or(crate::error::NotFoundError)?;
        Ok(self.addrs.remove(index))
    }
}

/// An iterator over address StrongIds. Created from `IpDeviceAddresses`.
pub struct AddressIdIter<'a, I: Ip + IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes>(
    core::slice::Iter<'a, PrimaryRc<I::AssignedAddress<BT>>>,
);

impl<'a, I: Ip + IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> Iterator
    for AddressIdIter<'a, I, BT>
{
    type Item = StrongRc<I::AssignedAddress<BT>>;

    fn next(&mut self) -> Option<Self::Item> {
        let Self(inner) = self;
        inner.next().map(PrimaryRc::clone_strong)
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

/// IPv4 device configurations and flags.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Ipv4DeviceConfigurationAndFlags {
    /// The IPv4 device configuration.
    pub config: Ipv4DeviceConfiguration,
    /// The IPv4 device flags.
    pub flags: IpDeviceFlags,
}

impl AsRef<IpDeviceConfiguration> for Ipv4DeviceConfigurationAndFlags {
    fn as_ref(&self) -> &IpDeviceConfiguration {
        self.config.as_ref()
    }
}

impl AsMut<IpDeviceConfiguration> for Ipv4DeviceConfigurationAndFlags {
    fn as_mut(&mut self) -> &mut IpDeviceConfiguration {
        self.config.as_mut()
    }
}

impl AsRef<IpDeviceFlags> for Ipv4DeviceConfigurationAndFlags {
    fn as_ref(&self) -> &IpDeviceFlags {
        &self.flags
    }
}

impl From<(Ipv4DeviceConfiguration, IpDeviceFlags)> for Ipv4DeviceConfigurationAndFlags {
    fn from((config, flags): (Ipv4DeviceConfiguration, IpDeviceFlags)) -> Self {
        Self { config, flags }
    }
}

/// IPv6 device configurations and flags.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Ipv6DeviceConfigurationAndFlags {
    /// The IPv6 device configuration.
    pub config: Ipv6DeviceConfiguration,
    /// The IPv6 device flags.
    pub flags: IpDeviceFlags,
}

impl AsRef<IpDeviceConfiguration> for Ipv6DeviceConfigurationAndFlags {
    fn as_ref(&self) -> &IpDeviceConfiguration {
        self.config.as_ref()
    }
}

impl AsMut<IpDeviceConfiguration> for Ipv6DeviceConfigurationAndFlags {
    fn as_mut(&mut self) -> &mut IpDeviceConfiguration {
        self.config.as_mut()
    }
}

impl AsRef<IpDeviceFlags> for Ipv6DeviceConfigurationAndFlags {
    fn as_ref(&self) -> &IpDeviceFlags {
        &self.flags
    }
}

impl From<(Ipv6DeviceConfiguration, IpDeviceFlags)> for Ipv6DeviceConfigurationAndFlags {
    fn from((config, flags): (Ipv6DeviceConfiguration, IpDeviceFlags)) -> Self {
        Self { config, flags }
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

    /// A flag indicating whether forwarding of IP packets not destined for this
    /// device is enabled.
    ///
    /// This flag controls whether or not packets can be forwarded from this
    /// device. That is, when a packet arrives at a device it is not destined
    /// for, the packet can only be forwarded if the device it arrived at has
    /// forwarding enabled and there exists another device that has a path to
    /// the packet's destination, regardless of the other device's forwarding
    /// ability.
    ///
    /// Default: `false`.
    pub forwarding_enabled: bool,
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
    pub const DEFAULT_MAX_RTR_SOLICITATIONS: NonZeroU8 = const_unwrap_option(NonZeroU8::new(3));

    /// The default `DupAddrDetectTransmits` value from [RFC 4862 Section 5.1]
    ///
    /// [RFC 4862 Section 5.1]: https://www.rfc-editor.org/rfc/rfc4862#section-5.1
    pub const DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS: NonZeroU16 =
        const_unwrap_option(NonZeroU16::new(1));
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
pub(crate) struct Ipv6NetworkLearnedParameters {
    /// The time between retransmissions of Neighbor Solicitation messages to a
    /// neighbor when resolving the address or when probing the reachability of
    /// a neighbor.
    ///
    ///
    /// See RetransTimer in [RFC 4861 section 6.3.2] for more details.
    ///
    /// [RFC 4861 section 6.3.2]: https://tools.ietf.org/html/rfc4861#section-6.3.2
    pub(crate) retrans_timer: Option<NonZeroDuration>,
}

impl Ipv6NetworkLearnedParameters {
    pub(crate) fn retrans_timer_or_default(&self) -> NonZeroDuration {
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
            router_solicitations: Mutex::new(RsState::new::<_, NestedIntoCoreTimerCtx<CC, _>>(
                bindings_ctx,
                device_id.clone(),
            )),
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
    InstantBindingsTypes + TimerBindingsTypes + ReferenceNotifiers
{
}
impl<BT> IpDeviceStateBindingsTypes for BT where
    BT: InstantBindingsTypes + TimerBindingsTypes + ReferenceNotifiers
{
}

/// IPv4 and IPv6 state combined.
pub(crate) struct DualStackIpDeviceState<BT: IpDeviceStateBindingsTypes> {
    /// IPv4 state.
    ipv4: Ipv4DeviceState<BT>,

    /// IPv6 state.
    ipv6: Ipv6DeviceState<BT>,

    /// The device's routing metric.
    metric: RawMetric,
}

impl<BC: IpDeviceStateBindingsTypes + TimerContext> DualStackIpDeviceState<BC> {
    pub(crate) fn new<
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

    pub(crate) fn ip_state<I: IpDeviceStateIpExt>(&self) -> &IpDeviceState<I, BT> {
        I::map_ip(
            IpInvariant(self),
            |IpInvariant(dual_stack)| &dual_stack.ipv4.ip_state,
            |IpInvariant(dual_stack)| &dual_stack.ipv6.ip_state,
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
    Tentative { dad_transmits_remaining: Option<NonZeroU16>, timer: BT::Timer },

    /// The address has not yet been initialized.
    Uninitialized,
}

/// Configuration for a temporary IPv6 address assigned via SLAAC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TemporarySlaacConfig<Instant> {
    /// The time at which the address is no longer valid.
    pub(crate) valid_until: Instant,
    /// The per-address DESYNC_FACTOR specified in RFC 8981 Section 3.4.
    pub(crate) desync_factor: Duration,
    /// The time at which the address was created.
    pub(crate) creation_time: Instant,
    /// The DAD_Counter parameter specified by RFC 8981 Section 3.3.2.1. This is
    /// used to track the number of retries that occurred prior to picking this
    /// address.
    pub(crate) dad_counter: u8,
}

/// A lifetime that may be forever/infinite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lifetime<I> {
    /// A finite lifetime.
    Finite(I),
    /// An infinite lifetime.
    Infinite,
}

impl<I: crate::Instant> InspectableValue for Lifetime<I> {
    fn record<N: Inspector>(&self, name: &str, inspector: &mut N) {
        match self {
            Self::Finite(instant) => inspector.record_inspectable_value(name, instant),
            Self::Infinite => inspector.record_str(name, "infinite"),
        }
    }
}

/// The configuration for an IPv4 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv4AddrConfig<Instant> {
    /// The lifetime for which the address is valid.
    pub valid_until: Lifetime<Instant>,
}

impl<I> Default for Ipv4AddrConfig<I> {
    fn default() -> Self {
        Self { valid_until: Lifetime::Infinite }
    }
}

/// Data associated with an IPv4 address on an interface.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Ipv4AddressEntry<BT: IpDeviceStateBindingsTypes> {
    addr_sub: AddrSubnet<Ipv4Addr>,
    state: RwLock<Ipv4AddressState<BT::Instant>>,
}

impl<BT: IpDeviceStateBindingsTypes> Ipv4AddressEntry<BT> {
    pub(crate) fn new(addr_sub: AddrSubnet<Ipv4Addr>, config: Ipv4AddrConfig<BT::Instant>) -> Self {
        Self { addr_sub, state: RwLock::new(Ipv4AddressState { config: Some(config) }) }
    }

    pub(crate) fn addr_sub(&self) -> &AddrSubnet<Ipv4Addr> {
        &self.addr_sub
    }

    pub(crate) fn addr(&self) -> SpecifiedAddr<Ipv4Addr> {
        self.addr_sub.addr()
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

#[derive(Debug)]
pub struct Ipv4AddressState<Instant> {
    pub(crate) config: Option<Ipv4AddrConfig<Instant>>,
}

impl<Instant: crate::Instant> Inspectable for Ipv4AddressState<Instant> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { config } = self;
        if let Some(Ipv4AddrConfig { valid_until }) = config {
            inspector.record_inspectable_value("ValidUntil", valid_until)
        }
    }
}

/// Configuration for an IPv6 address assigned via SLAAC.
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

/// The configuration for an IPv6 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ipv6AddrConfig<Instant> {
    /// Configured by stateless address autoconfiguration.
    Slaac(SlaacConfig<Instant>),

    /// Manually configured.
    Manual(Ipv6AddrManualConfig<Instant>),
}

impl<Instant> Default for Ipv6AddrConfig<Instant> {
    fn default() -> Self {
        Self::Manual(Default::default())
    }
}

/// The configuration for a manually-assigned IPv6 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv6AddrManualConfig<Instant> {
    /// The lifetime for which the address is valid.
    pub valid_until: Lifetime<Instant>,
}

impl<Instant> Default for Ipv6AddrManualConfig<Instant> {
    fn default() -> Self {
        Self { valid_until: Lifetime::Infinite }
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
    pub(crate) const SLAAC_LINK_LOCAL: Self =
        Self::Slaac(SlaacConfig::Static { valid_until: Lifetime::Infinite });

    /// The lifetime for which the address is valid.
    pub fn valid_until(&self) -> Lifetime<Instant> {
        match self {
            Ipv6AddrConfig::Slaac(slaac_config) => match slaac_config {
                SlaacConfig::Static { valid_until } => *valid_until,
                SlaacConfig::Temporary(TemporarySlaacConfig {
                    valid_until,
                    desync_factor: _,
                    creation_time: _,
                    dad_counter: _,
                }) => Lifetime::Finite(*valid_until),
            },
            Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { valid_until }) => *valid_until,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Ipv6AddressFlags {
    pub(crate) deprecated: bool,
    pub(crate) assigned: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ipv6AddressState<Instant> {
    pub(crate) flags: Ipv6AddressFlags,
    pub(crate) config: Option<Ipv6AddrConfig<Instant>>,
}

impl<Instant: crate::Instant> Inspectable for Ipv6AddressState<Instant> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { flags: Ipv6AddressFlags { deprecated, assigned }, config } = self;
        inspector.record_bool("Deprecated", *deprecated);
        inspector.record_bool("Assigned", *assigned);

        if let Some(config) = config {
            let (is_slaac, valid_until) = match config {
                Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { valid_until }) => {
                    (false, *valid_until)
                }
                Ipv6AddrConfig::Slaac(SlaacConfig::Static { valid_until }) => (true, *valid_until),
                Ipv6AddrConfig::Slaac(SlaacConfig::Temporary(TemporarySlaacConfig {
                    valid_until,
                    desync_factor,
                    creation_time,
                    dad_counter,
                })) => {
                    // Record the extra temporary slaac configuration before
                    // returning.
                    inspector.record_double("DesyncFactorSecs", desync_factor.as_secs_f64());
                    inspector.record_uint("DadCounter", *dad_counter);
                    inspector.record_inspectable_value("CreationTime", creation_time);
                    (true, Lifetime::Finite(*valid_until))
                }
            };
            inspector.record_bool("IsSlaac", is_slaac);
            inspector.record_inspectable_value("ValidUntil", &valid_until);
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
    pub(crate) fn new(
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
                flags: Ipv6AddressFlags { deprecated: false, assigned },
            }),
        }
    }

    pub(crate) fn addr_sub(&self) -> &AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
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

    use crate::{context::testutil::FakeInstant, error::ExistsError};

    use test_case::test_case;

    type FakeBindingsCtxImpl = crate::context::testutil::FakeBindingsCtx<(), (), (), ()>;

    #[test_case(Lifetime::Infinite ; "with infinite valid_until")]
    #[test_case(Lifetime::Finite(FakeInstant::from(Duration::from_secs(1))); "with finite valid_until")]
    fn test_add_addr_ipv4(valid_until: Lifetime<FakeInstant>) {
        const ADDRESS: Ipv4Addr = Ipv4Addr::new([1, 2, 3, 4]);
        const PREFIX_LEN: u8 = 8;

        let mut ipv4 = IpDeviceAddresses::<Ipv4, FakeBindingsCtxImpl>::default();

        let _: StrongRc<_> = ipv4
            .add(Ipv4AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN).unwrap(),
                Ipv4AddrConfig { valid_until },
            ))
            .unwrap();
        // Adding the same address with different prefix should fail.
        assert_eq!(
            ipv4.add(Ipv4AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN + 1).unwrap(),
                Ipv4AddrConfig { valid_until },
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

        let _: StrongRc<_> = ipv6
            .add(Ipv6AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN).unwrap(),
                Ipv6DadState::Tentative {
                    dad_transmits_remaining: None,
                    timer: bindings_ctx.new_timer(()),
                },
                Ipv6AddrConfig::Slaac(SlaacConfig::Static { valid_until }),
            ))
            .unwrap();
        // Adding the same address with different prefix and configuration
        // should fail.
        assert_eq!(
            ipv6.add(Ipv6AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN + 1).unwrap(),
                Ipv6DadState::Assigned,
                Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { valid_until }),
            ))
            .unwrap_err(),
            ExistsError,
        );
    }
}
