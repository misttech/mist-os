// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! State for an IP device.

use alloc::vec::Vec;
use core::{fmt::Debug, hash::Hash, num::NonZeroU8, time::Duration};

use derivative::Derivative;
use lock_order::lock::{LockFor, RwLockFor};
use net_types::{
    ip::{
        AddrSubnet, AddrSubnetEither, GenericOverIp, Ip, IpAddress, IpInvariant, Ipv4, Ipv4Addr,
        Ipv6, Ipv6Addr,
    },
    SpecifiedAddr, UnicastAddr, Witness,
};
use nonzero_ext::nonzero;
use packet_formats::utils::NonZeroDuration;

use crate::{
    ip::{
        device::{
            route_discovery::Ipv6RouteDiscoveryState, slaac::SlaacConfiguration, IpAddressId,
            IpDeviceIpExt,
        },
        gmp::{igmp::IgmpGroupState, mld::MldGroupState, MulticastGroupSet},
    },
    sync::{Mutex, PrimaryRc, RwLock, StrongRc},
    Instant,
};

/// The default value for *RetransTimer* as defined in [RFC 4861 section 10].
///
/// [RFC 4861 section 10]: https://tools.ietf.org/html/rfc4861#section-10
const RETRANS_TIMER_DEFAULT: NonZeroDuration = NonZeroDuration::from_nonzero_secs(nonzero!(1u64));

/// The default value for the default hop limit to be used when sending IP
/// packets.
const DEFAULT_HOP_LIMIT: NonZeroU8 = nonzero!(64u8);

#[derive(Clone, Copy, Debug)]
pub(crate) enum DelIpv6AddrReason {
    ManualAction,
    DadFailed,
}

/// An `Ip` extension trait adding IP device state properties.
pub(crate) trait IpDeviceStateIpExt: Ip {
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

    /// Examines the address and returns its subnet if it is assigned.
    ///
    /// Otherwise returns `None`.
    fn assigned_addr<I: Instant>(addr: &Self::AssignedAddress<I>)
        -> Option<AddrSubnet<Self::Addr>>;
}

impl IpDeviceStateIpExt for Ipv4 {
    type AssignedAddress<I: Instant> = Ipv4AddressEntry<I>;
    type GmpState<I: Instant> = IgmpGroupState<I>;

    fn assigned_addr<I: Instant>(
        addr: &Self::AssignedAddress<I>,
    ) -> Option<AddrSubnet<Self::Addr>> {
        Some(*addr.addr_sub())
    }
}

impl<I: Instant> IpAddressId<Ipv4Addr> for StrongRc<Ipv4AddressEntry<I>> {
    fn addr(&self) -> SpecifiedAddr<Ipv4Addr> {
        self.addr_sub.addr()
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv4Addr> {
        self.addr_sub
    }
}

impl<I: Instant> IpAddressId<Ipv6Addr> for StrongRc<Ipv6AddressEntry<I>> {
    fn addr(&self) -> SpecifiedAddr<Ipv6Addr> {
        self.addr_sub.addr().into_specified()
    }

    fn addr_sub(&self) -> AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>> {
        self.addr_sub
    }
}

impl IpDeviceStateIpExt for Ipv6 {
    type AssignedAddress<I: Instant> = Ipv6AddressEntry<I>;
    type GmpState<I: Instant> = MldGroupState<I>;

    fn assigned_addr<I: Instant>(
        addr: &Self::AssignedAddress<I>,
    ) -> Option<AddrSubnet<Self::Addr>> {
        // Tentative IP addresses (addresses which are not yet fully bound to a
        // device) and deprecated IP addresses (addresses which have been
        // assigned but should no longer be used for new connections) will not
        // be returned.
        addr.state.read().flags.assigned.then_some((*addr.addr_sub()).to_witness())
    }
}

/// The state associated with an IP address assigned to an IP device.
pub trait AssignedAddress<A: IpAddress> {
    /// Gets the address.
    fn addr(&self) -> SpecifiedAddr<A>;
}

impl<I: Instant> AssignedAddress<Ipv4Addr> for Ipv4AddressEntry<I> {
    fn addr(&self) -> SpecifiedAddr<Ipv4Addr> {
        self.addr_sub().addr()
    }
}

impl<I: Instant> AssignedAddress<Ipv6Addr> for Ipv6AddressEntry<I> {
    fn addr(&self) -> SpecifiedAddr<Ipv6Addr> {
        self.addr_sub().addr().into_specified()
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
#[cfg_attr(test, derive(Debug))]
pub(crate) struct IpDeviceState<Instant: crate::Instant, I: Ip + IpDeviceStateIpExt> {
    /// IP addresses assigned to this device.
    ///
    /// IPv6 addresses may be tentative (performing NDP's Duplicate Address
    /// Detection).
    ///
    /// Does not contain any duplicates.
    pub addrs: RwLock<IpDeviceAddresses<Instant, I>>,

    /// Multicast groups this device has joined.
    pub multicast_groups: RwLock<MulticastGroupSet<I::Addr, I::GmpState<Instant>>>,

    /// The default TTL (IPv4) or hop limit (IPv6) for outbound packets sent
    /// over this device.
    pub default_hop_limit: RwLock<NonZeroU8>,

    /// The flags for this device.
    flags: Mutex<IpDeviceFlags>,
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceAddresses<Ipv4>>
    for DualStackIpDeviceState<I>
{
    type Data = IpDeviceAddresses<I, Ipv4>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, IpDeviceAddresses<I, Ipv4>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, IpDeviceAddresses<I, Ipv4>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv4.ip_state.addrs.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv4.ip_state.addrs.write()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceGmp<Ipv4>> for DualStackIpDeviceState<I> {
    type Data = MulticastGroupSet<Ipv4Addr, IgmpGroupState<I>>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, MulticastGroupSet<Ipv4Addr, IgmpGroupState<I>>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, MulticastGroupSet<Ipv4Addr, IgmpGroupState<I>>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv4.ip_state.multicast_groups.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv4.ip_state.multicast_groups.write()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv4>>
    for DualStackIpDeviceState<I>
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

impl<I: Instant> LockFor<crate::lock_ordering::IpDeviceFlags<Ipv4>> for DualStackIpDeviceState<I> {
    type Data = IpDeviceFlags;
    type Guard<'l> = crate::sync::LockGuard<'l, IpDeviceFlags>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv4.ip_state.flags.lock()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceAddresses<Ipv6>>
    for DualStackIpDeviceState<I>
{
    type Data = IpDeviceAddresses<I, Ipv6>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, IpDeviceAddresses<I, Ipv6>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, IpDeviceAddresses<I, Ipv6>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.ip_state.addrs.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.ip_state.addrs.write()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceGmp<Ipv6>> for DualStackIpDeviceState<I> {
    type Data = MulticastGroupSet<Ipv6Addr, MldGroupState<I>>;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, MulticastGroupSet<Ipv6Addr, MldGroupState<I>>>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, MulticastGroupSet<Ipv6Addr, MldGroupState<I>>>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.ip_state.multicast_groups.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.ip_state.multicast_groups.write()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceDefaultHopLimit<Ipv6>>
    for DualStackIpDeviceState<I>
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

impl<I: Instant> LockFor<crate::lock_ordering::IpDeviceFlags<Ipv6>> for DualStackIpDeviceState<I> {
    type Data = IpDeviceFlags;
    type Guard<'l> = crate::sync::LockGuard<'l, IpDeviceFlags>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv6.ip_state.flags.lock()
    }
}

impl<Instant: crate::Instant, I: IpDeviceStateIpExt> Default for IpDeviceState<Instant, I> {
    fn default() -> IpDeviceState<Instant, I> {
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
pub(crate) struct IpDeviceAddresses<Instant: crate::Instant, I: Ip + IpDeviceStateIpExt> {
    addrs: Vec<PrimaryRc<I::AssignedAddress<Instant>>>,
}

// TODO(https://fxbug.dev/84871): Once we figure out what invariants we want to
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
        self.addrs.iter().find(|entry| &entry.addr().get() == addr)
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
            .find(|(_, entry)| &entry.addr().get() == addr)
            .ok_or(crate::error::NotFoundError)?;
        Ok(self.addrs.remove(index))
    }
}

/// The state common to all IPv4 devices.
pub(crate) struct Ipv4DeviceState<I: Instant> {
    pub(crate) ip_state: IpDeviceState<I, Ipv4>,
    pub(super) config: RwLock<Ipv4DeviceConfiguration>,
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceConfiguration<Ipv4>>
    for DualStackIpDeviceState<I>
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

impl<I: Instant> Default for Ipv4DeviceState<I> {
    fn default() -> Ipv4DeviceState<I> {
        Ipv4DeviceState { ip_state: Default::default(), config: Default::default() }
    }
}

impl<I: Instant> AsRef<IpDeviceState<I, Ipv4>> for Ipv4DeviceState<I> {
    fn as_ref(&self) -> &IpDeviceState<I, Ipv4> {
        &self.ip_state
    }
}

impl<I: Instant> AsMut<IpDeviceState<I, Ipv4>> for Ipv4DeviceState<I> {
    fn as_mut(&mut self) -> &mut IpDeviceState<I, Ipv4> {
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
    pub dad_transmits: Option<NonZeroU8>,

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
    pub const DEFAULT_MAX_RTR_SOLICITATIONS: NonZeroU8 = nonzero!(3u8);

    /// The default `DupAddrDetectTransmits` value from [RFC 4862 Section 5.1]
    ///
    /// [RFC 4862 Section 5.1]: https://www.rfc-editor.org/rfc/rfc4862#section-5.1
    pub const DEFAULT_DUPLICATE_ADDRESS_DETECTION_TRANSMITS: NonZeroU8 = nonzero!(1u8);
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

impl<I: Instant> RwLockFor<crate::lock_ordering::Ipv6DeviceRetransTimeout>
    for DualStackIpDeviceState<I>
{
    type Data = NonZeroDuration;
    type ReadGuard<'l> = crate::sync::RwLockReadGuard<'l, NonZeroDuration>
        where
            Self: 'l;
    type WriteGuard<'l> = crate::sync::RwLockWriteGuard<'l, NonZeroDuration>
        where
            Self: 'l;
    fn read_lock(&self) -> Self::ReadGuard<'_> {
        self.ipv6.retrans_timer.read()
    }
    fn write_lock(&self) -> Self::WriteGuard<'_> {
        self.ipv6.retrans_timer.write()
    }
}

impl<I: Instant> LockFor<crate::lock_ordering::Ipv6DeviceRouteDiscovery>
    for DualStackIpDeviceState<I>
{
    type Data = Ipv6RouteDiscoveryState;
    type Guard<'l> = crate::sync::LockGuard<'l, Ipv6RouteDiscoveryState>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv6.route_discovery.lock()
    }
}

impl<I: Instant> LockFor<crate::lock_ordering::Ipv6DeviceRouterSolicitations>
    for DualStackIpDeviceState<I>
{
    type Data = Option<NonZeroU8>;
    type Guard<'l> = crate::sync::LockGuard<'l, Option<NonZeroU8>>
        where
            Self: 'l;
    fn lock(&self) -> Self::Guard<'_> {
        self.ipv6.router_soliciations_remaining.lock()
    }
}

impl<I: Instant> RwLockFor<crate::lock_ordering::IpDeviceConfiguration<Ipv6>>
    for DualStackIpDeviceState<I>
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

/// The state common to all IPv6 devices.
pub(crate) struct Ipv6DeviceState<I: Instant> {
    /// The time between retransmissions of Neighbor Solicitation messages to a
    /// neighbor when resolving the address or when probing the reachability of
    /// a neighbor.
    ///
    /// Default: [`RETRANS_TIMER_DEFAULT`].
    ///
    /// See RetransTimer in [RFC 4861 section 6.3.2] for more details.
    ///
    /// [RFC 4861 section 6.3.2]: https://tools.ietf.org/html/rfc4861#section-6.3.2
    pub(crate) retrans_timer: RwLock<NonZeroDuration>,
    pub(super) route_discovery: Mutex<Ipv6RouteDiscoveryState>,
    pub(super) router_soliciations_remaining: Mutex<Option<NonZeroU8>>,
    pub(crate) ip_state: IpDeviceState<I, Ipv6>,
    pub(crate) config: RwLock<Ipv6DeviceConfiguration>,
}

impl<I: Instant> Default for Ipv6DeviceState<I> {
    fn default() -> Ipv6DeviceState<I> {
        Ipv6DeviceState {
            retrans_timer: RwLock::new(RETRANS_TIMER_DEFAULT),
            route_discovery: Default::default(),
            router_soliciations_remaining: Default::default(),
            ip_state: Default::default(),
            config: Default::default(),
        }
    }
}

impl<I: Instant> AsRef<IpDeviceState<I, Ipv6>> for Ipv6DeviceState<I> {
    fn as_ref(&self) -> &IpDeviceState<I, Ipv6> {
        &self.ip_state
    }
}

impl<I: Instant> AsMut<IpDeviceState<I, Ipv6>> for Ipv6DeviceState<I> {
    fn as_mut(&mut self) -> &mut IpDeviceState<I, Ipv6> {
        &mut self.ip_state
    }
}

/// IPv4 and IPv6 state combined.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct DualStackIpDeviceState<I: Instant> {
    /// IPv4 state.
    pub ipv4: Ipv4DeviceState<I>,

    /// IPv6 state.
    pub ipv6: Ipv6DeviceState<I>,
}

/// The various states DAD may be in for an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ipv6DadState {
    /// The address is assigned to an interface and can be considered bound to
    /// it (all packets destined to the address will be accepted).
    Assigned,

    /// The address is considered unassigned to an interface for normal
    /// operations, but has the intention of being assigned in the future (e.g.
    /// once NDP's Duplicate Address Detection is completed).
    ///
    /// When `dad_transmits_remaining` is `None`, then no more DAD messages need
    /// to be sent and DAD may be resolved.
    Tentative { dad_transmits_remaining: Option<NonZeroU8> },

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
pub(crate) struct Ipv4AddressEntry<Instant> {
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
pub(crate) struct Ipv4AddressState<Instant> {
    pub(crate) config: Ipv4AddrConfig<Instant>,
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

/// An AddrSubnet together with configuration specified for it when adding it
/// to the stack.
#[derive(Debug)]
pub enum AddrSubnetAndManualConfigEither<Instant> {
    /// Variant for an Ipv4 AddrSubnet.
    V4(AddrSubnet<Ipv4Addr>, Ipv4AddrConfig<Instant>),
    /// Variant for an Ipv6 AddrSubnet.
    V6(AddrSubnet<Ipv6Addr>, Ipv6AddrManualConfig<Instant>),
}

impl<Instant> AddrSubnetAndManualConfigEither<Instant> {
    /// Constructs an `AddrSubnetAndManualConfigEither`.
    pub(crate) fn new<I: Ip + IpDeviceIpExt>(
        addr_subnet: AddrSubnet<I::Addr>,
        config: I::ManualAddressConfig<Instant>,
    ) -> Self {
        #[derive(GenericOverIp)]
        struct AddrSubnetAndConfig<I: Ip + IpDeviceIpExt, Instant> {
            addr_subnet: AddrSubnet<I::Addr>,
            config: I::ManualAddressConfig<Instant>,
        }

        let IpInvariant(result) = I::map_ip(
            AddrSubnetAndConfig { addr_subnet, config },
            |AddrSubnetAndConfig { addr_subnet, config }| {
                IpInvariant(AddrSubnetAndManualConfigEither::V4(addr_subnet, config))
            },
            |AddrSubnetAndConfig { addr_subnet, config }| {
                IpInvariant(AddrSubnetAndManualConfigEither::V6(addr_subnet, config))
            },
        );
        result
    }

    /// Extracts the `AddrSubnetEither`.
    pub fn addr_subnet_either(&self) -> AddrSubnetEither {
        match self {
            Self::V4(addr_subnet, _) => AddrSubnetEither::V4(*addr_subnet),
            Self::V6(addr_subnet, _) => AddrSubnetEither::V6(*addr_subnet),
        }
    }
}

impl<Instant> From<AddrSubnetEither> for AddrSubnetAndManualConfigEither<Instant> {
    fn from(value: AddrSubnetEither) -> Self {
        match value {
            AddrSubnetEither::V4(addr_subnet) => {
                AddrSubnetAndManualConfigEither::new::<Ipv4>(addr_subnet, Default::default())
            }
            AddrSubnetEither::V6(addr_subnet) => {
                AddrSubnetAndManualConfigEither::new::<Ipv6>(addr_subnet, Default::default())
            }
        }
    }
}

impl<Instant, I: IpAddress> From<AddrSubnet<I>> for AddrSubnetAndManualConfigEither<Instant> {
    fn from(value: AddrSubnet<I>) -> Self {
        AddrSubnetEither::from(value).into()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Ipv6AddressFlags {
    pub(crate) deprecated: bool,
    pub(crate) assigned: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Ipv6AddressState<Instant> {
    pub(crate) flags: Ipv6AddressFlags,
    pub(crate) config: Ipv6AddrConfig<Instant>,
}

/// Data associated with an IPv6 address on an interface.
// TODO(https://fxbug.dev/91753): Should this be generalized for loopback?
#[derive(Debug)]
pub(crate) struct Ipv6AddressEntry<Instant> {
    pub(crate) addr_sub: AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>,
    pub(crate) dad_state: Mutex<Ipv6DadState>,
    pub(crate) state: RwLock<Ipv6AddressState<Instant>>,
}

impl<Instant> Ipv6AddressEntry<Instant> {
    pub(crate) fn new(
        addr_sub: AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>>,
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

    pub(crate) fn addr_sub(&self) -> &AddrSubnet<Ipv6Addr, UnicastAddr<Ipv6Addr>> {
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
