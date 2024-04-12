// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State for an IP device.

use alloc::vec::Vec;
use core::{
    fmt::Debug,
    hash::Hash,
    num::{NonZeroU16, NonZeroU8},
    time::Duration,
};

use const_unwrap::const_unwrap_option;
use derivative::Derivative;
use lock_order::lock::{LockFor, RwLockFor, UnlockedAccess};
use net_types::{
    ip::{AddrSubnet, GenericOverIp, Ip, IpAddress, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr},
    SpecifiedAddr,
};
use packet_formats::utils::NonZeroDuration;

use crate::{
    context::InstantBindingsTypes,
    inspect::{Inspectable, InspectableValue, Inspector},
    ip::{
        device::{
            route_discovery::Ipv6RouteDiscoveryState, slaac::SlaacConfiguration, IpAddressId,
            IpDeviceAddr, Ipv6DeviceAddr,
        },
        gmp::{igmp::IgmpGroupState, mld::MldGroupState, MulticastGroupSet},
        types::{IpTypesIpExt, RawMetric},
    },
    sync::{Mutex, PrimaryRc, RwLock, StrongRc},
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
    type AssignedAddress<I: Instant>: AssignedAddress<Self::Addr> + Debug;

    /// The state kept by the Group Messaging Protocol (GMP) used to announce
    /// membership in an IP multicast group for this version of IP.
    ///
    /// Note that a GMP is only used when membership must be explicitly
    /// announced. For example, a GMP is not used in the context of a loopback
    /// device (because there are no remote hosts) or in the context of an IPsec
    /// device (because multicast is not supported).
    type GmpState<I: Instant>;
}

impl IpDeviceStateIpExt for Ipv4 {
    type AssignedAddress<I: Instant> = Ipv4AddressEntry<I>;
    type GmpState<I: Instant> = IgmpGroupState<I>;
}

impl<I: Instant> IpAddressId<Ipv4Addr> for StrongRc<Ipv4AddressEntry<I>> {
    fn addr(&self) -> IpDeviceAddr<Ipv4Addr> {
        IpDeviceAddr::new_ipv4_specified(self.addr_sub.addr())
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv4Addr> {
        self.addr_sub
    }
}

impl<I: Instant> IpAddressId<Ipv6Addr> for StrongRc<Ipv6AddressEntry<I>> {
    fn addr(&self) -> IpDeviceAddr<Ipv6Addr> {
        IpDeviceAddr::new_from_ipv6_device_addr(self.addr_sub.addr())
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        self.addr_sub
    }
}

impl IpDeviceStateIpExt for Ipv6 {
    type AssignedAddress<I: Instant> = Ipv6AddressEntry<I>;
    type GmpState<I: Instant> = MldGroupState<I>;
}

/// The state associated with an IP address assigned to an IP device.
pub trait AssignedAddress<A: IpAddress> {
    /// Gets the address.
    fn addr(&self) -> IpDeviceAddr<A>;
}

impl<I: Instant> AssignedAddress<Ipv4Addr> for Ipv4AddressEntry<I> {
    fn addr(&self) -> IpDeviceAddr<Ipv4Addr> {
        IpDeviceAddr::new_ipv4_specified(self.addr_sub().addr())
    }
}

impl<I: Instant> AssignedAddress<Ipv6Addr> for Ipv6AddressEntry<I> {
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

/// The state common to all IP devices.
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[cfg_attr(test, derive(Debug))]
pub struct IpDeviceState<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> {
    /// IP addresses assigned to this device.
    ///
    /// IPv6 addresses may be tentative (performing NDP's Duplicate Address
    /// Detection).
    ///
    /// Does not contain any duplicates.
    pub addrs: RwLock<IpDeviceAddresses<BT::Instant, I>>,

    /// Multicast groups this device has joined.
    pub multicast_groups: RwLock<MulticastGroupSet<I::Addr, I::GmpState<BT::Instant>>>,

    /// The default TTL (IPv4) or hop limit (IPv6) for outbound packets sent
    /// over this device.
    pub default_hop_limit: RwLock<NonZeroU8>,

    /// The flags for this device.
    flags: Mutex<IpDeviceFlags>,
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceAddresses<Ipv4>>
    for DualStackIpDeviceState<BT>
{
    type Data = IpDeviceAddresses<BT::Instant, Ipv4>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, IpDeviceAddresses<BT::Instant, Ipv4>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, IpDeviceAddresses<BT::Instant, Ipv4>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv4.ip_state.addrs.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv4.ip_state.addrs.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceGmp<Ipv4>>
    for DualStackIpDeviceState<BT>
{
    type Data = MulticastGroupSet<Ipv4Addr, IgmpGroupState<BT::Instant>>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, MulticastGroupSet<Ipv4Addr, IgmpGroupState<BT::Instant>>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, MulticastGroupSet<Ipv4Addr, IgmpGroupState<BT::Instant>>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv4.ip_state.multicast_groups.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv4.ip_state.multicast_groups.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv4>>
    for DualStackIpDeviceState<BT>
{
    type Data = NonZeroU8;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, NonZeroU8>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, NonZeroU8>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv4.ip_state.default_hop_limit.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv4.ip_state.default_hop_limit.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> LockFor<crate::lock_ordering::IpDeviceFlags<Ipv4>>
    for DualStackIpDeviceState<BT>
{
    type Data = IpDeviceFlags;
    type Guard<'l> = crate::sync::LockGuard<'l, IpDeviceFlags>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv4.ip_state.flags.lock()
    }
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceAddresses<Ipv6>>
    for DualStackIpDeviceState<BT>
{
    type Data = IpDeviceAddresses<BT::Instant, Ipv6>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, IpDeviceAddresses<BT::Instant, Ipv6>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, IpDeviceAddresses<BT::Instant, Ipv6>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.ip_state.addrs.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.ip_state.addrs.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceGmp<Ipv6>>
    for DualStackIpDeviceState<BT>
{
    type Data = MulticastGroupSet<Ipv6Addr, MldGroupState<BT::Instant>>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, MulticastGroupSet<Ipv6Addr, MldGroupState<BT::Instant>>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, MulticastGroupSet<Ipv6Addr, MldGroupState<BT::Instant>>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.ip_state.multicast_groups.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.ip_state.multicast_groups.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv6>>
    for DualStackIpDeviceState<BT>
{
    type Data = NonZeroU8;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, NonZeroU8>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, NonZeroU8>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.ip_state.default_hop_limit.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.ip_state.default_hop_limit.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> LockFor<crate::lock_ordering::IpDeviceFlags<Ipv6>>
    for DualStackIpDeviceState<BT>
{
    type Data = IpDeviceFlags;
    type Guard<'l> = crate::sync::LockGuard<'l, IpDeviceFlags>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv6.ip_state.flags.lock()
    }
}

impl<BT: IpDeviceStateBindingsTypes> UnlockedAccess<crate::lock_ordering::RoutingMetric>
    for DualStackIpDeviceState<BT>
{
    type Data = RawMetric;

    type Guard<'l> = &'l RawMetric
    where
        Self: 'l;

    fn access(&self) -> Self::Guard<'_> {
        &self.metric
    }
}

impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> Default for IpDeviceState<I, BT> {
    fn default() -> IpDeviceState<I, BT> {
        IpDeviceState {
            addrs: Default::default(),
            multicast_groups: Default::default(),
            default_hop_limit: RwLock::new(DEFAULT_HOP_LIMIT),
            flags: Default::default(),
        }
    }
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug))]
pub struct IpDeviceAddresses<Instant: crate::Instant, I: Ip + IpDeviceStateIpExt> {
    addrs: Vec<PrimaryRc<I::AssignedAddress<Instant>>>,
}

// TODO(https://fxbug.dev/42165707): Once we figure out what invariants we want to
// hold regarding the set of IP addresses assigned to a device, ensure that all
// of the methods on `IpDeviceAddresses` uphold those invariants.
impl<Instant: crate::Instant, I: IpDeviceStateIpExt> IpDeviceAddresses<Instant, I> {
    /// Iterates over the addresses assigned to this device.
    pub(crate) fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &PrimaryRc<I::AssignedAddress<Instant>>> + ExactSizeIterator + Clone
    {
        self.addrs.iter()
    }

    /// Finds the entry for `addr` if any.
    #[cfg(test)]
    pub(crate) fn find(&self, addr: &I::Addr) -> Option<&PrimaryRc<I::AssignedAddress<Instant>>> {
        self.addrs.iter().find(|entry| &entry.addr().addr() == addr)
    }

    /// Adds an IP address to this interface.
    pub(crate) fn add(
        &mut self,
        addr: I::AssignedAddress<Instant>,
    ) -> Result<StrongRc<I::AssignedAddress<Instant>>, crate::error::ExistsError> {
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
    ) -> Result<PrimaryRc<I::AssignedAddress<Instant>>, crate::error::NotFoundError> {
        let (index, _entry): (_, &PrimaryRc<I::AssignedAddress<Instant>>) = self
            .addrs
            .iter()
            .enumerate()
            .find(|(_, entry)| &entry.addr().addr() == addr)
            .ok_or(crate::error::NotFoundError)?;
        Ok(self.addrs.remove(index))
    }
}

/// The state common to all IPv4 devices.
pub struct Ipv4DeviceState<BT: IpDeviceStateBindingsTypes> {
    pub(crate) ip_state: IpDeviceState<Ipv4, BT>,
    pub(super) config: RwLock<Ipv4DeviceConfiguration>,
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>
    for DualStackIpDeviceState<BT>
{
    type Data = Ipv4DeviceConfiguration;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Ipv4DeviceConfiguration>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Ipv4DeviceConfiguration>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv4.config.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv4.config.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> Default for Ipv4DeviceState<BT> {
    fn default() -> Ipv4DeviceState<BT> {
        Ipv4DeviceState { ip_state: Default::default(), config: Default::default() }
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

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::Ipv6DeviceLearnedParams>
    for DualStackIpDeviceState<BT>
{
    type Data = Ipv6NetworkLearnedParameters;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Ipv6NetworkLearnedParameters>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Ipv6NetworkLearnedParameters>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.learned_params.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.learned_params.write()
    }
}

impl<BT: IpDeviceStateBindingsTypes> LockFor<crate::lock_ordering::Ipv6DeviceRouteDiscovery>
    for DualStackIpDeviceState<BT>
{
    type Data = Ipv6RouteDiscoveryState;
    type Guard<'l> = crate::sync::LockGuard<'l, Ipv6RouteDiscoveryState>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv6.route_discovery.lock()
    }
}

impl<BT: IpDeviceStateBindingsTypes> LockFor<crate::lock_ordering::Ipv6DeviceRouterSolicitations>
    for DualStackIpDeviceState<BT>
{
    type Data = Option<NonZeroU8>;
    type Guard<'l> = crate::sync::LockGuard<'l, Option<NonZeroU8>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv6.router_soliciations_remaining.lock()
    }
}

impl<BT: IpDeviceStateBindingsTypes> RwLockFor<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>
    for DualStackIpDeviceState<BT>
{
    type Data = Ipv6DeviceConfiguration;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Ipv6DeviceConfiguration>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Ipv6DeviceConfiguration>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.config.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.config.write()
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
    pub(super) learned_params: RwLock<Ipv6NetworkLearnedParameters>,
    pub(super) route_discovery: Mutex<Ipv6RouteDiscoveryState>,
    pub(super) router_soliciations_remaining: Mutex<Option<NonZeroU8>>,
    pub(crate) ip_state: IpDeviceState<Ipv6, BT>,
    pub(crate) config: RwLock<Ipv6DeviceConfiguration>,
}

impl<BT: IpDeviceStateBindingsTypes> Default for Ipv6DeviceState<BT> {
    fn default() -> Ipv6DeviceState<BT> {
        Ipv6DeviceState {
            learned_params: Default::default(),
            route_discovery: Default::default(),
            router_soliciations_remaining: Default::default(),
            ip_state: Default::default(),
            config: Default::default(),
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
pub trait IpDeviceStateBindingsTypes: InstantBindingsTypes {}
impl<BT> IpDeviceStateBindingsTypes for BT where BT: InstantBindingsTypes {}

/// IPv4 and IPv6 state combined.
pub(crate) struct DualStackIpDeviceState<BT: IpDeviceStateBindingsTypes> {
    /// IPv4 state.
    pub ipv4: Ipv4DeviceState<BT>,

    /// IPv6 state.
    pub ipv6: Ipv6DeviceState<BT>,

    /// The device's routing metric.
    pub metric: RawMetric,
}

impl<BT: InstantBindingsTypes> DualStackIpDeviceState<BT> {
    pub(crate) fn new(metric: RawMetric) -> Self {
        Self { ipv4: Default::default(), ipv6: Default::default(), metric }
    }
}

/// The various states DAD may be in for an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ipv6DadState {
    /// The address is assigned to an interface and can be considered bound to
    /// it (all packets destined to the address will be accepted).
    Assigned,

    /// The address is considered unassigned to an interface for normal
    /// operations, but has the intention of being assigned in the future (e.g.
    /// once NDP's Duplicate Address Detection is completed).
    ///
    /// When `dad_transmits_remaining` is `None`, then no more DAD messages need
    /// to be sent and DAD may be resolved.
    Tentative { dad_transmits_remaining: Option<NonZeroU16> },

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
#[derive(Debug)]
pub struct Ipv4AddressEntry<Instant> {
    pub(crate) addr_sub: AddrSubnet<Ipv4Addr>,
    pub(crate) state: RwLock<Ipv4AddressState<Instant>>,
}

impl<Instant> Ipv4AddressEntry<Instant> {
    pub(crate) fn new(addr_sub: AddrSubnet<Ipv4Addr>, config: Ipv4AddrConfig<Instant>) -> Self {
        Self { addr_sub, state: RwLock::new(Ipv4AddressState { config }) }
    }

    pub(crate) fn addr_sub(&self) -> &AddrSubnet<Ipv4Addr> {
        &self.addr_sub
    }

    pub(crate) fn addr(&self) -> SpecifiedAddr<Ipv4Addr> {
        self.addr_sub.addr()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::Ipv4DeviceAddressState> for Ipv4AddressEntry<I> {
    type Data = Ipv4AddressState<I>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Ipv4AddressState<I>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Ipv4AddressState<I>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.write()
    }
}

#[derive(Debug)]
pub struct Ipv4AddressState<Instant> {
    pub(crate) config: Ipv4AddrConfig<Instant>,
}

impl<Instant: crate::Instant> Inspectable for Ipv4AddressState<Instant> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { config: Ipv4AddrConfig { valid_until } } = self;
        inspector.record_inspectable_value("ValidUntil", valid_until)
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
    pub(crate) config: Ipv6AddrConfig<Instant>,
}

impl<Instant: crate::Instant> Inspectable for Ipv6AddressState<Instant> {
    fn record<I: Inspector>(&self, inspector: &mut I) {
        let Self { flags: Ipv6AddressFlags { deprecated, assigned }, config } = self;
        inspector.record_bool("Deprecated", *deprecated);
        inspector.record_bool("Assigned", *assigned);
        let (is_slaac, valid_until) = match config {
            Ipv6AddrConfig::Manual(Ipv6AddrManualConfig { valid_until }) => (false, *valid_until),
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

/// Data associated with an IPv6 address on an interface.
// TODO(https://fxbug.dev/42173351): Should this be generalized for loopback?
#[derive(Debug)]
pub struct Ipv6AddressEntry<Instant> {
    pub(crate) addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    pub(crate) dad_state: Mutex<Ipv6DadState>,
    pub(crate) state: RwLock<Ipv6AddressState<Instant>>,
}

impl<Instant> Ipv6AddressEntry<Instant> {
    pub(crate) fn new(
        addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        dad_state: Ipv6DadState,
        config: Ipv6AddrConfig<Instant>,
    ) -> Self {
        let assigned = match dad_state {
            Ipv6DadState::Assigned => true,
            Ipv6DadState::Tentative { dad_transmits_remaining: _ }
            | Ipv6DadState::Uninitialized => false,
        };

        Self {
            addr_sub,
            dad_state: Mutex::new(dad_state),
            state: RwLock::new(Ipv6AddressState {
                config,
                flags: Ipv6AddressFlags { deprecated: false, assigned },
            }),
        }
    }

    pub(crate) fn addr_sub(&self) -> &AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        &self.addr_sub
    }
}

impl<I: Instant> LockFor<crate::lock_ordering::Ipv6DeviceAddressDad> for Ipv6AddressEntry<I> {
    type Data = Ipv6DadState;
    type Guard<'l> = crate::sync::LockGuard<'l, Ipv6DadState>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.dad_state.lock()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::Ipv6DeviceAddressState> for Ipv6AddressEntry<I> {
    type Data = Ipv6AddressState<I>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, Ipv6AddressState<I>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, Ipv6AddressState<I>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.state.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.state.write()
    }
}
#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    use net_types::{ip::IpInvariant, Witness as _};

    impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> AsRef<Self> for IpDeviceState<I, BT> {
        fn as_ref(&self) -> &Self {
            self
        }
    }

    impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> AsRef<IpDeviceState<I, BT>>
        for DualStackIpDeviceState<BT>
    {
        fn as_ref(&self) -> &IpDeviceState<I, BT> {
            I::map_ip(
                IpInvariant(self),
                |IpInvariant(dual_stack)| &dual_stack.ipv4.ip_state,
                |IpInvariant(dual_stack)| &dual_stack.ipv6.ip_state,
            )
        }
    }

    impl<I: IpDeviceStateIpExt, BT: IpDeviceStateBindingsTypes> AsMut<IpDeviceState<I, BT>>
        for DualStackIpDeviceState<BT>
    {
        fn as_mut(&mut self) -> &mut IpDeviceState<I, BT> {
            I::map_ip(
                IpInvariant(self),
                |IpInvariant(dual_stack)| &mut dual_stack.ipv4.ip_state,
                |IpInvariant(dual_stack)| &mut dual_stack.ipv6.ip_state,
            )
        }
    }

    /// Adds an address and route for the size-1 subnet containing the address.
    pub(crate) fn add_addr_subnet<A: IpAddress, BT: IpDeviceStateBindingsTypes>(
        device_state: &mut IpDeviceState<A::Version, BT>,
        ip: SpecifiedAddr<A>,
    ) where
        A::Version: IpDeviceStateIpExt,
    {
        #[derive(GenericOverIp)]
        #[generic_over_ip(I, Ip)]
        struct Wrap<I: IpDeviceStateIpExt, Instant: crate::Instant>(I::AssignedAddress<Instant>);
        let Wrap(entry) = <A::Version as Ip>::map_ip(
            ip.get(),
            |ip| {
                Wrap(Ipv4AddressEntry::new(
                    AddrSubnet::new(ip, 32).unwrap(),
                    Ipv4AddrConfig::default(),
                ))
            },
            |ip| {
                Wrap(Ipv6AddressEntry::new(
                    AddrSubnet::new(ip, 128).unwrap(),
                    Ipv6DadState::Assigned,
                    Ipv6AddrConfig::default(),
                ))
            },
        );
        let _addr_id = device_state.addrs.get_mut().add(entry).expect("add address");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{context::testutil::FakeInstant, error::ExistsError};

    use test_case::test_case;

    #[test_case(Lifetime::Infinite ; "with infinite valid_until")]
    #[test_case(Lifetime::Finite(FakeInstant::from(Duration::from_secs(1))); "with finite valid_until")]
    fn test_add_addr_ipv4(valid_until: Lifetime<FakeInstant>) {
        const ADDRESS: Ipv4Addr = Ipv4Addr::new([1, 2, 3, 4]);
        const PREFIX_LEN: u8 = 8;

        let mut ipv4 = IpDeviceAddresses::<FakeInstant, Ipv4>::default();

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

        let mut ipv6 = IpDeviceAddresses::<FakeInstant, Ipv6>::default();

        let _: StrongRc<_> = ipv6
            .add(Ipv6AddressEntry::new(
                AddrSubnet::new(ADDRESS, PREFIX_LEN).unwrap(),
                Ipv6DadState::Tentative { dad_transmits_remaining: None },
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
