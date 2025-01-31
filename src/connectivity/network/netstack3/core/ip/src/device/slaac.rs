// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IPv6 Stateless Address Autoconfiguration (SLAAC) as defined by [RFC 4862]
//! and temporary address extensions for SLAAC as defined by [RFC 8981].
//!
//! [RFC 4862]: https://datatracker.ietf.org/doc/html/rfc4862
//! [RFC 8981]: https://datatracker.ietf.org/doc/html/rfc8981

use alloc::vec::Vec;
use core::marker::PhantomData;
use core::num::NonZeroU16;
use core::ops::ControlFlow;
use core::time::Duration;

use assert_matches::assert_matches;
use log::{debug, error, trace};
use net_types::ip::{AddrSubnet, Ip as _, IpAddress, Ipv6, Ipv6Addr, Subnet};
use net_types::Witness as _;
use netstack3_base::{
    AnyDevice, CoreTimerContext, Counter, CounterContext, DeviceIdContext, DeviceIdentifier,
    EventContext, ExistsError, HandleableTimer, Instant, InstantBindingsTypes, InstantContext,
    LocalTimerHeap, NotFoundError, RngContext, TimerBindingsTypes, TimerContext,
    WeakDeviceIdentifier,
};
use packet_formats::icmp::ndp::NonZeroNdpLifetime;
use packet_formats::utils::NonZeroDuration;
use rand::distributions::Uniform;
use rand::Rng;

use crate::device::Ipv6AddrSlaacConfig;
use crate::internal::device::opaque_iid::{IidSecret, OpaqueIid, OpaqueIidNonce};
use crate::internal::device::state::{
    Lifetime, PreferredLifetime, SlaacConfig, TemporarySlaacConfig,
};
use crate::internal::device::{AddressRemovedReason, IpDeviceEvent, Ipv6DeviceAddr};

/// Minimum Valid Lifetime value to actually update an address's valid lifetime.
///
/// 2 hours.
const MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE: NonZeroDuration =
    NonZeroDuration::new(Duration::from_secs(7200)).unwrap();

/// Required prefix length for SLAAC.
///
/// We need 64 bits in the prefix because the interface identifier is 64 bits,
/// and IPv6 addresses are 128 bits.
const REQUIRED_PREFIX_BITS: u8 = 64;

/// Internal SLAAC timer ID key for [`SlaacState`]'s `LocalTimerHeap`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[allow(missing_docs)]
pub enum InnerSlaacTimerId {
    /// Timer to deprecate an address configured via SLAAC.
    DeprecateSlaacAddress { addr: Ipv6DeviceAddr },
    /// Timer to invalidate an address configured via SLAAC.
    InvalidateSlaacAddress { addr: Ipv6DeviceAddr },
    /// Timer to generate a new temporary SLAAC address before an existing one
    /// expires.
    RegenerateTemporaryAddress { addr_subnet: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> },
}

/// Global Slaac state on a device.
pub struct SlaacState<BT: SlaacBindingsTypes> {
    timers: LocalTimerHeap<InnerSlaacTimerId, (), BT>,
}

impl<BC: SlaacBindingsTypes + TimerContext> SlaacState<BC> {
    /// Constructs a new SLAAC state for `device_id`.
    pub fn new<D: WeakDeviceIdentifier, CC: CoreTimerContext<SlaacTimerId<D>, BC>>(
        bindings_ctx: &mut BC,
        device_id: D,
    ) -> Self {
        Self {
            timers: LocalTimerHeap::new_with_context::<_, CC>(
                bindings_ctx,
                SlaacTimerId { device_id },
            ),
        }
    }

    /// Provides direct access to the internal timer heap.
    #[cfg(any(test, feature = "testutils"))]
    pub fn timers(&self) -> &LocalTimerHeap<InnerSlaacTimerId, (), BC> {
        &self.timers
    }
}

/// A timer ID for SLAAC.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct SlaacTimerId<D: WeakDeviceIdentifier> {
    device_id: D,
}

impl<D: WeakDeviceIdentifier> SlaacTimerId<D> {
    pub(super) fn device_id(&self) -> &D {
        let Self { device_id } = self;
        device_id
    }

    /// Creates a new SLAAC timer id for `device_id`.
    #[cfg(any(test, feature = "testutils"))]
    pub fn new(device_id: D) -> Self {
        Self { device_id }
    }
}

/// The state associated with a SLAAC address.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SlaacAddressEntry<Instant> {
    /// The address and the subnet.
    pub addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    /// The address' SLAAC configuration.
    pub config: Ipv6AddrSlaacConfig<Instant>,
}

/// A mutable view into state associated with a SLAAC address's mutable state.
pub struct SlaacAddressEntryMut<'a, Instant> {
    /// The address and the subnet.
    pub addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    /// Mutable access to the address' SLAAC configuration.
    pub config: &'a mut Ipv6AddrSlaacConfig<Instant>,
}

/// Abstracts iteration over a device's SLAAC addresses.
pub trait SlaacAddresses<BT: SlaacBindingsTypes> {
    /// Returns an iterator providing a mutable view of mutable SLAAC address
    /// state.
    fn for_each_addr_mut<F: FnMut(SlaacAddressEntryMut<'_, BT::Instant>)>(&mut self, cb: F);

    /// The iterator provided to `with_addrs`.
    type AddrsIter<'x>: Iterator<Item = SlaacAddressEntry<BT::Instant>>;

    /// Calls the callback with an iterator over the addresses.
    fn with_addrs<O, F: FnOnce(Self::AddrsIter<'_>) -> O>(&mut self, cb: F) -> O;

    /// Adds `addr_sub` with `config` to the device and calls `and_then` with
    /// the newly added entry.
    fn add_addr_sub_and_then<O, F: FnOnce(SlaacAddressEntryMut<'_, BT::Instant>, &mut BT) -> O>(
        &mut self,
        bindings_ctx: &mut BT,
        addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        config: Ipv6AddrSlaacConfig<BT::Instant>,
        and_then: F,
    ) -> Result<O, ExistsError>;

    /// Removes a SLAAC address.
    ///
    /// # Panics
    ///
    /// May panic if `addr` is not an address recognized.
    fn remove_addr(
        &mut self,
        bindings_ctx: &mut BT,
        addr: &Ipv6DeviceAddr,
    ) -> Result<(AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>, SlaacConfig<BT::Instant>), NotFoundError>;
}

/// Supports [`SlaacContext::with_slaac_addrs_mut_and_config`].
///
/// Contains the fields necessary for the SLAAC state machine.
pub struct SlaacAddrsMutAndConfig<'a, BT: SlaacBindingsTypes, A: SlaacAddresses<BT>> {
    /// The device's SLAAC address.
    pub addrs: &'a mut A,
    /// The current config for the device.
    pub config: SlaacConfiguration,
    /// The configured number of DAD transmits.
    pub dad_transmits: Option<NonZeroU16>,
    /// THhe configured retransmission timer (can be learned from the network).
    pub retrans_timer: Duration,
    /// The device's interface identifier.
    pub interface_identifier: [u8; 8],
    /// Secret key for generating temporary addresses.
    pub temp_secret_key: IidSecret,
    #[allow(missing_docs)]
    pub _marker: PhantomData<BT>,
}

/// The execution context for SLAAC.
pub trait SlaacContext<BC: SlaacBindingsContext<Self::DeviceId>>:
    DeviceIdContext<AnyDevice>
{
    /// The inner [`SlaacAddresses`] impl.
    type SlaacAddrs<'a>: SlaacAddresses<BC> + CounterContext<SlaacCounters> + 'a;

    /// Calls `cb` with access to the SLAAC addresses and configuration for
    /// `device_id`.
    fn with_slaac_addrs_mut_and_configs<
        O,
        F: FnOnce(SlaacAddrsMutAndConfig<'_, BC, Self::SlaacAddrs<'_>>, &mut SlaacState<BC>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls `cb` with access to the SLAAC addresses for `device_id`.
    fn with_slaac_addrs_mut<O, F: FnOnce(&mut Self::SlaacAddrs<'_>, &mut SlaacState<BC>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_slaac_addrs_mut_and_configs(
            device_id,
            |SlaacAddrsMutAndConfig {
                 addrs,
                 config: _,
                 dad_transmits: _,
                 retrans_timer: _,
                 interface_identifier: _,
                 temp_secret_key: _,
                 _marker,
             },
             state| cb(addrs, state),
        )
    }
}

/// Counters for SLAAC.
#[derive(Default)]
pub struct SlaacCounters {
    /// Count of already exists errors when adding a generated SLAAC address.
    pub generated_slaac_addr_exists: Counter,
}

/// Update the instant at which an address configured via SLAAC is no longer
/// valid.
///
/// A `None` value for `valid_until` indicates that the address is valid
/// forever; `Some` indicates valid for some finite lifetime.
///
/// # Panics
///
/// May panic if `addr` is not an address configured via SLAAC on
/// `device_id`.
fn update_slaac_addr_valid_until<I: Instant>(
    slaac_config: &mut SlaacConfig<I>,
    valid_until: Lifetime<I>,
) {
    match slaac_config {
        SlaacConfig::Stable { valid_until: v } => *v = valid_until,
        SlaacConfig::Temporary(TemporarySlaacConfig {
            valid_until: v,
            desync_factor: _,
            creation_time: _,
            dad_counter: _,
        }) => {
            *v = match valid_until {
                Lifetime::Finite(v) => v,
                Lifetime::Infinite => panic!("temporary addresses may not be valid forever"),
            }
        }
    };
}

/// The bindings types for SLAAC.
pub trait SlaacBindingsTypes: InstantBindingsTypes + TimerBindingsTypes {}
impl<BT> SlaacBindingsTypes for BT where BT: InstantBindingsTypes + TimerBindingsTypes {}

/// The bindings execution context for SLAAC.
pub trait SlaacBindingsContext<D>:
    RngContext + TimerContext + EventContext<IpDeviceEvent<D, Ipv6, Self::Instant>> + SlaacBindingsTypes
{
}
impl<D, BC> SlaacBindingsContext<D> for BC where
    BC: RngContext
        + TimerContext
        + EventContext<IpDeviceEvent<D, Ipv6, Self::Instant>>
        + SlaacBindingsTypes
{
}

/// An implementation of SLAAC.
pub trait SlaacHandler<BC: InstantContext>: DeviceIdContext<AnyDevice> {
    /// Executes the algorithm in [RFC 4862 Section 5.5.3], with the extensions
    /// from [RFC 8981 Section 3.4] for temporary addresses, for a given prefix
    /// advertised by a router.
    ///
    /// This function updates all stable and temporary SLAAC addresses for the
    /// given prefix and adds new ones if necessary.
    ///
    /// [RFC 4862 Section 5.5.3]: http://tools.ietf.org/html/rfc4862#section-5.5.3
    /// [RFC 8981 Section 3.4]: https://tools.ietf.org/html/rfc8981#section-3.4
    fn apply_slaac_update(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        prefix: Subnet<Ipv6Addr>,
        preferred_lifetime: Option<NonZeroNdpLifetime>,
        valid_lifetime: Option<NonZeroNdpLifetime>,
    );

    /// Generates a link-local SLAAC address for the given interface.
    fn generate_link_local_address(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId);

    /// Handles SLAAC specific aspects of address removal.
    ///
    /// Must only be called after the address is removed from the interface.
    fn on_address_removed(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        state: SlaacConfig<BC::Instant>,
        reason: AddressRemovedReason,
    );

    /// Removes all SLAAC addresses assigned to the device.
    fn remove_all_slaac_addresses(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId);
}

impl<BC: SlaacBindingsContext<CC::DeviceId>, CC: SlaacContext<BC>> SlaacHandler<BC> for CC {
    fn apply_slaac_update(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        subnet: Subnet<Ipv6Addr>,
        preferred_lifetime: Option<NonZeroNdpLifetime>,
        valid_lifetime: Option<NonZeroNdpLifetime>,
    ) {
        if preferred_lifetime > valid_lifetime {
            // If the preferred lifetime is greater than the valid lifetime,
            // silently ignore the Prefix Information option, as per RFC 4862
            // section 5.5.3.
            trace!("receive_ndp_packet: autonomous prefix's preferred lifetime is greater than valid lifetime, ignoring");
            return;
        }

        let mut seen_stable = false;
        let mut seen_temporary = false;

        let now = bindings_ctx.now();
        self.with_slaac_addrs_mut_and_configs(device_id, |addrs_config, slaac_state| {
            let SlaacAddrsMutAndConfig {
                addrs: slaac_addrs,
                config,
                dad_transmits,
                retrans_timer,
                interface_identifier: iid,
                temp_secret_key,
                _marker,
            } = addrs_config;
            // Apply the update to each existing address, stable or temporary, for the
            // prefix.
            slaac_addrs.for_each_addr_mut(|address_entry| {
                let slaac_type = match apply_slaac_update_to_addr(
                    address_entry,
                    slaac_state,
                    subnet,
                    device_id,
                    valid_lifetime,
                    preferred_lifetime,
                    &config,
                    now,
                    retrans_timer,
                    dad_transmits,
                    bindings_ctx,
                ) {
                    ControlFlow::Break(()) => return,
                    ControlFlow::Continue(slaac_type) => slaac_type,
                };
                // Mark the SLAAC address type as existing so we know not to
                // generate an address for the type later.
                //
                // Note that SLAAC addresses are never invalidated/removed
                // in response to a prefix update and addresses types never
                // change after the address is added.
                match slaac_type {
                    SlaacType::Stable => seen_stable = true,
                    SlaacType::Temporary => seen_temporary = true,
                }
            });

            // As per RFC 4862 section 5.5.3.e, if the prefix advertised is not equal to
            // the prefix of an address configured by stateless autoconfiguration
            // already in the list of addresses associated with the interface, and if
            // the Valid Lifetime is not 0, form an address (and add it to the list) by
            // combining the advertised prefix with an interface identifier of the link
            // as follows:
            //
            // |    128 - N bits    |        N bits          |
            // +--------------------+------------------------+
            // |    link prefix     |  interface identifier  |
            // +---------------------------------------------+
            let valid_lifetime = match valid_lifetime {
                Some(valid_lifetime) => valid_lifetime,
                None => {
                    trace!(
                        "receive_ndp_packet: autonomous prefix has valid \
                            lifetime = 0, ignoring"
                    );
                    return;
                }
            };

            let address_types_to_add = (!seen_stable)
                .then(|| {
                    // As per RFC 4862 Section 5.5.3.d,
                    //
                    //
                    //   If the prefix advertised is not equal to the prefix of an
                    //   address configured by stateless autoconfiguration already
                    //   in the list of addresses associated with the interface
                    //   (where 'equal' means the two prefix lengths are the same
                    //   and the first prefix- length bits of the prefixes are
                    //   identical), and if the Valid Lifetime is not 0, form an
                    //   address [...].
                    SlaacType::Stable
                })
                .into_iter()
                .chain((!seen_temporary).then(|| {
                    // As per RFC 8981 Section 3.4.3,
                    //
                    //   If the host has not configured any temporary
                    //   address for the corresponding prefix, the host
                    //   SHOULD create a new temporary address for such
                    //   prefix.
                    SlaacType::Temporary
                }));

            for slaac_type in address_types_to_add {
                add_slaac_addr_sub::<_, CC>(
                    slaac_addrs,
                    slaac_state,
                    bindings_ctx,
                    device_id,
                    now,
                    SlaacInitConfig::new(slaac_type),
                    valid_lifetime,
                    preferred_lifetime,
                    &subnet,
                    config,
                    dad_transmits,
                    retrans_timer,
                    iid,
                    temp_secret_key,
                );
            }
        });
    }

    fn generate_link_local_address(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId) {
        let now = bindings_ctx.now();
        self.with_slaac_addrs_mut_and_configs(device_id, |addrs_config, slaac_state| {
            let SlaacAddrsMutAndConfig {
                addrs,
                config,
                dad_transmits,
                retrans_timer,
                interface_identifier,
                temp_secret_key,
                _marker,
            } = addrs_config;

            // Configure a link-local address via SLAAC.
            //
            // Per [RFC 4862 Section 5.3]: "A link-local address has an infinite preferred
            // and valid lifetime; it is never timed out."
            //
            // [RFC 4862 Section 5.3]: https://tools.ietf.org/html/rfc4862#section-5.3
            let link_local_subnet =
                Subnet::new(Ipv6::LINK_LOCAL_UNICAST_SUBNET.network(), REQUIRED_PREFIX_BITS)
                    .expect("link local subnet should be valid");
            add_slaac_addr_sub::<_, CC>(
                addrs,
                slaac_state,
                bindings_ctx,
                device_id,
                now,
                SlaacInitConfig::new(SlaacType::Stable),
                NonZeroNdpLifetime::Infinite,       /* valid_lifetime */
                Some(NonZeroNdpLifetime::Infinite), /* preferred_lifetime */
                &link_local_subnet,
                config,
                dad_transmits,
                retrans_timer,
                interface_identifier,
                temp_secret_key,
            );
        });
    }

    fn on_address_removed(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
        state: SlaacConfig<BC::Instant>,
        reason: AddressRemovedReason,
    ) {
        self.with_slaac_addrs_mut_and_configs(device_id, |addrs_config, slaac_state| {
            on_address_removed_inner::<_, CC>(
                bindings_ctx,
                addr_sub,
                device_id,
                addrs_config,
                slaac_state,
                state,
                reason,
            )
        });
    }

    fn remove_all_slaac_addresses(&mut self, bindings_ctx: &mut BC, device_id: &Self::DeviceId) {
        self.with_slaac_addrs_mut(device_id, |slaac_addrs, _| {
            slaac_addrs
                .with_addrs(|addrs| addrs.map(|a| a.addr_sub.addr()).collect::<Vec<_>>())
                .into_iter()
                .filter_map(|addr| {
                    slaac_addrs.remove_addr(bindings_ctx, &addr).map(Some).unwrap_or_else(
                        |NotFoundError| {
                            // We're not holding locks on the assigned addresses
                            // here, so we can't assume a race is impossible with
                            // something else removing the address. Just assume that
                            // it is gone.
                            None
                        },
                    )
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .for_each(|(addr, config)| {
            self.on_address_removed(
                bindings_ctx,
                device_id,
                addr,
                config,
                AddressRemovedReason::Manual,
            )
        })
    }
}

fn on_address_removed_inner<BC: SlaacBindingsContext<CC::DeviceId>, CC: SlaacContext<BC>>(
    bindings_ctx: &mut BC,
    addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
    device_id: &CC::DeviceId,
    addrs_config: SlaacAddrsMutAndConfig<'_, BC, CC::SlaacAddrs<'_>>,
    slaac_state: &mut SlaacState<BC>,
    state: SlaacConfig<BC::Instant>,
    reason: AddressRemovedReason,
) {
    let SlaacState { timers } = slaac_state;
    let preferred_until = timers
        .cancel(bindings_ctx, &InnerSlaacTimerId::DeprecateSlaacAddress { addr: addr_sub.addr() })
        .map(|(t, ())| t);
    let _valid_until: Option<(BC::Instant, ())> = timers
        .cancel(bindings_ctx, &InnerSlaacTimerId::InvalidateSlaacAddress { addr: addr_sub.addr() });

    let TemporarySlaacConfig { valid_until, creation_time, desync_factor, dad_counter } =
        match state {
            SlaacConfig::Temporary(temporary_config) => {
                let _regen_at: Option<(BC::Instant, ())> = timers.cancel(
                    bindings_ctx,
                    &InnerSlaacTimerId::RegenerateTemporaryAddress { addr_subnet: addr_sub },
                );
                temporary_config
            }
            SlaacConfig::Stable { .. } => {
                // TODO(https://fxbug.dev/42148800): Re-generate stable
                // addresses if we're not using hardware-derived IIDs.
                return;
            }
        };

    match reason {
        AddressRemovedReason::Manual => return,
        AddressRemovedReason::DadFailed => {
            // Attempt to regenerate the address.
        }
    }

    let SlaacAddrsMutAndConfig {
        addrs: slaac_addrs,
        config,
        dad_transmits,
        retrans_timer,
        interface_identifier: iid,
        temp_secret_key,
        _marker,
    } = addrs_config;
    let SlaacConfiguration { enable_stable_addresses: _, temporary_address_configuration } = config;
    let temp_valid_lifetime = match temporary_address_configuration {
        TemporarySlaacAddressConfiguration::Enabled {
            temp_idgen_retries,
            temp_valid_lifetime,
            temp_preferred_lifetime: _,
        } => {
            if dad_counter >= temp_idgen_retries {
                return;
            }
            temp_valid_lifetime
        }
        TemporarySlaacAddressConfiguration::Disabled => return,
    };

    // Compute the original preferred lifetime for the removed address so that
    // it can be used for the new address being generated. If, when the address
    // was created, the prefix's preferred lifetime was less than
    // `temporary_address_configuration.temp_preferred_lifetime`, then that's
    // what will be calculated here. That's fine because it's a lower bound on
    // the prefix's value, which means the prefix's value is still being
    // respected.
    let preferred_for = match preferred_until.map(|preferred_until| {
        preferred_until.saturating_duration_since(creation_time) + desync_factor
    }) {
        Some(preferred_for) => preferred_for,
        // If the address is already deprecated, a new address should already
        // have been generated, so ignore this one.
        None => return,
    };

    let now = bindings_ctx.now();
    // It's possible this `valid_for` value is larger than `temp_valid_lifetime`
    // (e.g. if the NDP configuration was changed since this address was
    // generated). That's okay, because `add_slaac_addr_sub` will apply the
    // current maximum valid lifetime when called below.
    let valid_for = NonZeroDuration::new(valid_until.saturating_duration_since(creation_time))
        .unwrap_or(temp_valid_lifetime);

    add_slaac_addr_sub::<_, CC>(
        slaac_addrs,
        slaac_state,
        bindings_ctx,
        device_id,
        now,
        SlaacInitConfig::Temporary { dad_count: dad_counter + 1 },
        NonZeroNdpLifetime::Finite(valid_for),
        NonZeroDuration::new(preferred_for).map(NonZeroNdpLifetime::Finite),
        &addr_sub.subnet(),
        config,
        dad_transmits,
        retrans_timer,
        iid,
        temp_secret_key,
    )
}

fn apply_slaac_update_to_addr<D: DeviceIdentifier, BC: SlaacBindingsContext<D>>(
    address_entry: SlaacAddressEntryMut<'_, BC::Instant>,
    state: &mut SlaacState<BC>,
    subnet: Subnet<Ipv6Addr>,
    device_id: &D,
    valid_lifetime: Option<NonZeroNdpLifetime>,
    addr_preferred_lifetime: Option<NonZeroNdpLifetime>,
    config: &SlaacConfiguration,
    now: <BC as InstantBindingsTypes>::Instant,
    retrans_timer: Duration,
    dad_transmits: Option<NonZeroU16>,
    bindings_ctx: &mut BC,
) -> ControlFlow<(), SlaacType> {
    let SlaacAddressEntryMut {
        addr_sub,
        config: Ipv6AddrSlaacConfig { inner: slaac_config, preferred_lifetime },
    } = address_entry;
    let SlaacState { timers } = state;
    if addr_sub.subnet() != subnet {
        return ControlFlow::Break(());
    }
    let addr = addr_sub.addr();
    let slaac_type = SlaacType::from(&*slaac_config);
    trace!(
        "receive_ndp_packet: already have a {:?} SLAAC address {:?} configured on device {:?}",
        slaac_type,
        addr_sub,
        device_id
    );

    /// Encapsulates a lifetime bound and where it came from.
    #[derive(Copy, Clone)]
    enum ValidLifetimeBound {
        FromPrefix(Option<NonZeroNdpLifetime>),
        FromMaxBound(Duration),
    }
    impl ValidLifetimeBound {
        /// Unwraps the object and returns the wrapped duration.
        fn get(self) -> Option<NonZeroNdpLifetime> {
            match self {
                Self::FromPrefix(d) => d,
                Self::FromMaxBound(d) => NonZeroDuration::new(d).map(NonZeroNdpLifetime::Finite),
            }
        }
    }
    let (valid_for, entry_valid_until, preferred_for_and_regen_at) = match slaac_config {
        SlaacConfig::Stable { valid_until: entry_valid_until } => (
            ValidLifetimeBound::FromPrefix(valid_lifetime),
            *entry_valid_until,
            addr_preferred_lifetime.map(|p| (p, None)),
        ),
        // Select valid_for and preferred_for according to RFC 8981
        // Section 3.4.
        SlaacConfig::Temporary(TemporarySlaacConfig {
            valid_until: entry_valid_until,
            creation_time,
            desync_factor,
            dad_counter: _,
        }) => {
            let SlaacConfiguration { enable_stable_addresses: _, temporary_address_configuration } =
                config;
            let (valid_for, preferred_for, entry_valid_until) =
                match temporary_address_configuration {
                    // Since it's possible to change NDP configuration for a
                    // device during runtime, we can end up here, with a
                    // temporary address on an interface even though temporary
                    // addressing is disabled. Don't update the valid or
                    // preferred lifetimes in this case.
                    TemporarySlaacAddressConfiguration::Disabled => {
                        (ValidLifetimeBound::FromMaxBound(Duration::ZERO), None, *entry_valid_until)
                    }
                    TemporarySlaacAddressConfiguration::Enabled {
                        temp_preferred_lifetime,
                        temp_valid_lifetime,
                        temp_idgen_retries: _,
                    } => {
                        // RFC 8981 Section 3.4.2:
                        //   When updating the preferred lifetime of an existing
                        //   temporary address, it would be set to expire at
                        //   whichever time is earlier: the time indicated by
                        //   the received lifetime or (CREATION_TIME +
                        //   TEMP_PREFERRED_LIFETIME - DESYNC_FACTOR). A similar
                        //   approach can be used with the valid lifetime.
                        let preferred_for =
                            addr_preferred_lifetime.and_then(|preferred_lifetime| {
                                temp_preferred_lifetime
                                    .get()
                                    .checked_sub(now.saturating_duration_since(*creation_time))
                                    .and_then(|p| p.checked_sub(*desync_factor))
                                    .and_then(NonZeroDuration::new)
                                    .map(|d| preferred_lifetime.min_finite_duration(d))
                            });
                        // Per RFC 8981 Section 3.4.1, `desync_factor` is only
                        // used for preferred lifetime:
                        //   [...] with the overall constraint that no temporary
                        //   addresses should ever remain "valid" or "preferred"
                        //   for a time longer than (TEMP_VALID_LIFETIME) or
                        //   (TEMP_PREFERRED_LIFETIME - DESYNC_FACTOR),
                        //   respectively.
                        let since_creation = now.saturating_duration_since(*creation_time);
                        let configured_max_lifetime = temp_valid_lifetime.get();
                        let max_valid_lifetime = if since_creation > configured_max_lifetime {
                            Duration::ZERO
                        } else {
                            configured_max_lifetime - since_creation
                        };

                        let valid_for = valid_lifetime.map_or(
                            ValidLifetimeBound::FromPrefix(None),
                            |d| match d {
                                NonZeroNdpLifetime::Infinite => {
                                    ValidLifetimeBound::FromMaxBound(max_valid_lifetime)
                                }
                                NonZeroNdpLifetime::Finite(d) => {
                                    if max_valid_lifetime <= d.get() {
                                        ValidLifetimeBound::FromMaxBound(max_valid_lifetime)
                                    } else {
                                        ValidLifetimeBound::FromPrefix(valid_lifetime)
                                    }
                                }
                            },
                        );

                        (valid_for, preferred_for, *entry_valid_until)
                    }
                };

            let preferred_for_and_regen_at = preferred_for.map(|preferred_for| {
                let SlaacConfiguration {
                    enable_stable_addresses: _,
                    temporary_address_configuration,
                } = config;

                let regen_at = match temporary_address_configuration {
                    TemporarySlaacAddressConfiguration::Disabled => None,
                    TemporarySlaacAddressConfiguration::Enabled {
                        temp_idgen_retries,
                        temp_preferred_lifetime: _,
                        temp_valid_lifetime: _,
                    } => {
                        let regen_advance = regen_advance(
                            *temp_idgen_retries,
                            retrans_timer,
                            dad_transmits.map_or(0, NonZeroU16::get),
                        )
                        .get();
                        // Per RFC 8981 Section 3.6:
                        //
                        //   Hosts following this specification SHOULD
                        //   generate new temporary addresses over time.
                        //   This can be achieved by generating a new
                        //   temporary address REGEN_ADVANCE time units
                        //   before a temporary address becomes deprecated.
                        //
                        // It's possible for regen_at to be before the
                        // current time. In that case, set it to `now` so
                        // that a new address is generated after the current
                        // prefix information is handled.
                        preferred_for
                            .get()
                            .checked_sub(regen_advance)
                            .map_or(Some(now), |d| now.checked_add(d))
                    }
                };

                (NonZeroNdpLifetime::Finite(preferred_for), regen_at)
            });

            (valid_for, Lifetime::Finite(entry_valid_until), preferred_for_and_regen_at)
        }
    };

    // `Some` iff the remaining lifetime is a positive non-zero lifetime.
    let remaining_lifetime = match entry_valid_until {
        Lifetime::Infinite => Some(Lifetime::Infinite),
        Lifetime::Finite(entry_valid_until) => entry_valid_until
            .checked_duration_since(now)
            .and_then(NonZeroDuration::new)
            .map(|d| Lifetime::Finite(d)),
    };

    // As per RFC 4862 section 5.5.3.e, if the advertised prefix is equal to the
    // prefix of an address configured by stateless autoconfiguration in the
    // list, the preferred lifetime of the address is reset to the Preferred
    // Lifetime in the received advertisement.

    // Update the preferred lifetime for this address.
    let preferred_lifetime_updated = match preferred_for_and_regen_at {
        None => {
            if preferred_lifetime.is_deprecated() {
                false
            } else {
                *preferred_lifetime = PreferredLifetime::Deprecated;
                let _: Option<(BC::Instant, ())> =
                    timers.cancel(bindings_ctx, &InnerSlaacTimerId::DeprecateSlaacAddress { addr });
                let _: Option<(BC::Instant, ())> = timers.cancel(
                    bindings_ctx,
                    &InnerSlaacTimerId::RegenerateTemporaryAddress { addr_subnet: addr_sub },
                );
                true
            }
        }
        Some((preferred_for, regen_at)) => {
            let timer_id = InnerSlaacTimerId::DeprecateSlaacAddress { addr };
            let preferred_instant = Lifetime::from_ndp(now, preferred_for);
            match preferred_instant {
                Lifetime::Finite(instant) => {
                    let _previously_scheduled_instant: Option<(BC::Instant, ())> =
                        timers.schedule_instant(bindings_ctx, timer_id, (), instant);
                }
                Lifetime::Infinite => {
                    let _previously_scheduled_instant: Option<(BC::Instant, ())> =
                        timers.cancel(bindings_ctx, &timer_id);
                }
            };
            let new_lifetime = PreferredLifetime::Preferred(preferred_instant);
            let updated = core::mem::replace(preferred_lifetime, new_lifetime) != new_lifetime;
            let timer_id = InnerSlaacTimerId::RegenerateTemporaryAddress { addr_subnet: addr_sub };
            let _prev_regen_at: Option<(BC::Instant, ())> = match regen_at {
                Some(regen_at) => timers.schedule_instant(bindings_ctx, timer_id, (), regen_at),
                None => timers.cancel(bindings_ctx, &timer_id),
            };
            updated
        }
    };

    // As per RFC 4862 section 5.5.3.e, the specific action to perform for the
    // valid lifetime of the address depends on the Valid Lifetime in the
    // received advertisement and the remaining time to the valid lifetime
    // expiration of the previously autoconfigured address:
    let valid_for_to_update = match valid_for {
        ValidLifetimeBound::FromMaxBound(valid_for) => {
            // If the maximum lifetime for the address is smaller than the
            // lifetime specified for the prefix, then it must be applied.
            NonZeroDuration::new(valid_for).map(NonZeroNdpLifetime::Finite)
        }
        ValidLifetimeBound::FromPrefix(valid_for) => {
            // If the received Valid Lifetime is greater than 2 hours or
            // greater than RemainingLifetime, set the valid lifetime of
            // the corresponding address to the advertised Valid
            // Lifetime.
            match valid_for {
                Some(NonZeroNdpLifetime::Infinite) => Some(NonZeroNdpLifetime::Infinite),
                Some(NonZeroNdpLifetime::Finite(v))
                    if v > MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE
                        || remaining_lifetime.map_or(true, |r| r < Lifetime::Finite(v)) =>
                {
                    Some(NonZeroNdpLifetime::Finite(v))
                }
                None | Some(NonZeroNdpLifetime::Finite(_)) => {
                    if remaining_lifetime.map_or(true, |r| {
                        r <= Lifetime::Finite(MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE)
                    }) {
                        // If RemainingLifetime is less than or equal to 2 hours,
                        // ignore the Prefix Information option with regards to the
                        // valid lifetime, unless the Router Advertisement from
                        // which this option was obtained has been authenticated
                        // (e.g., via Secure Neighbor Discovery [RFC3971]).  If the
                        // Router Advertisement was authenticated, the valid
                        // lifetime of the corresponding address should be set to
                        // the Valid Lifetime in the received option.
                        //
                        // TODO(ghanan): If the NDP packet this prefix option is in
                        //               was authenticated, update the valid
                        //               lifetime of the address to the valid
                        //               lifetime in the received option, as per RFC
                        //               4862 section 5.5.3.e.
                        None
                    } else {
                        // Otherwise, reset the valid lifetime of the corresponding
                        // address to 2 hours.
                        Some(NonZeroNdpLifetime::Finite(MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE))
                    }
                }
            }
        }
    };

    match valid_for_to_update {
        Some(valid_for) => match Lifetime::from_ndp(now, valid_for) {
            Lifetime::Finite(valid_until) => {
                trace!(
                    "receive_ndp_packet: updating valid lifetime to {:?} for SLAAC address {:?} on device {:?}",
                    valid_until,
                    addr,
                    device_id
                );

                // Set the valid lifetime for this address.
                update_slaac_addr_valid_until(slaac_config, Lifetime::Finite(valid_until));

                let _: Option<(BC::Instant, ())> = timers.schedule_instant(
                    bindings_ctx,
                    InnerSlaacTimerId::InvalidateSlaacAddress { addr },
                    (),
                    valid_until,
                );
            }
            Lifetime::Infinite => {
                // Set the valid lifetime for this address.
                update_slaac_addr_valid_until(slaac_config, Lifetime::Infinite);

                let _: Option<(BC::Instant, ())> = timers
                    .cancel(bindings_ctx, &InnerSlaacTimerId::InvalidateSlaacAddress { addr });
            }
        },
        None => {
            trace!(
                "receive_ndp_packet: not updating valid lifetime for SLAAC address {:?} on device {:?} as remaining lifetime is less than 2 hours and new valid lifetime ({:?}) is less than remaining lifetime",
                addr,
                device_id,
                valid_for.get()
            );
        }
    }

    if preferred_lifetime_updated || valid_for_to_update.is_some() {
        bindings_ctx.on_event(IpDeviceEvent::AddressPropertiesChanged {
            device: device_id.clone(),
            addr: addr_sub.addr().into(),
            valid_until: slaac_config.valid_until(),
            preferred_lifetime: *preferred_lifetime,
        });
    }
    ControlFlow::Continue(slaac_type)
}

impl<BC: SlaacBindingsContext<CC::DeviceId>, CC: SlaacContext<BC>> HandleableTimer<CC, BC>
    for SlaacTimerId<CC::WeakDeviceId>
{
    fn handle(self, core_ctx: &mut CC, bindings_ctx: &mut BC, _: BC::UniqueTimerId) {
        let Self { device_id } = self;
        let Some(device_id) = device_id.upgrade() else {
            return;
        };
        core_ctx.with_slaac_addrs_mut_and_configs(&device_id, |slaac_addrs_config, slaac_state| {
            let Some((timer_id, ())) = slaac_state.timers.pop(bindings_ctx) else {
                return;
            };
            match timer_id {
                InnerSlaacTimerId::DeprecateSlaacAddress { addr } => slaac_addrs_config
                    .addrs
                    .for_each_addr_mut(|SlaacAddressEntryMut { addr_sub, config }| {
                        if addr_sub.addr() == addr {
                            config.preferred_lifetime = PreferredLifetime::Deprecated;

                            bindings_ctx.on_event(IpDeviceEvent::AddressPropertiesChanged {
                                device: device_id.clone(),
                                addr: addr_sub.addr().into(),
                                valid_until: config.inner.valid_until(),
                                preferred_lifetime: config.preferred_lifetime,
                            });
                        }
                    }),
                InnerSlaacTimerId::InvalidateSlaacAddress { addr } => {
                    let (addr, config) =
                        match slaac_addrs_config.addrs.remove_addr(bindings_ctx, &addr) {
                            Ok(addr_config) => addr_config,
                            Err(NotFoundError) => {
                                // Even though when a user removes an address we
                                // get notified, we could still race with our
                                // own timer here. This is a tight enough race
                                // that we can log at warn to call out in case
                                // something else is wrong. It should certainly
                                // not happen in tests, however.
                                #[cfg(test)]
                                panic!("Failed to remove address {addr} on invalidation");
                                #[cfg(not(test))]
                                {
                                    log::warn!(
                                        "failed to remove SLAAC address {addr}, assuming raced \
                                        with user removal"
                                    );
                                    return;
                                }
                            }
                        };

                    on_address_removed_inner::<_, CC>(
                        bindings_ctx,
                        addr,
                        &device_id,
                        slaac_addrs_config,
                        slaac_state,
                        config,
                        AddressRemovedReason::Manual,
                    );
                }
                InnerSlaacTimerId::RegenerateTemporaryAddress { addr_subnet } => {
                    regenerate_temporary_slaac_addr::<_, CC>(
                        bindings_ctx,
                        slaac_addrs_config,
                        slaac_state,
                        &device_id,
                        &addr_subnet,
                    );
                }
            }
        });
    }
}

/// Configuration values for SLAAC temporary addressing.
///
/// The algorithm specified in [RFC 8981 Section 3.4] references several
/// configuration parameters, which are defined in [Section 3.8] and
/// [Section 3.3.2] This struct contains the following values specified by the
/// RFC:
/// - TEMP_VALID_LIFETIME
/// - TEMP_PREFERRED_LIFETIME
/// - TEMP_IDGEN_RETRIES
/// - secret_key
///
/// [RFC 8981 Section 3.4]: http://tools.ietf.org/html/rfc8981#section-3.4
/// [Section 3.3.2]: http://tools.ietf.org/html/rfc8981#section-3.3.2
/// [Section 3.8]: http://tools.ietf.org/html/rfc8981#section-3.8
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TemporarySlaacAddressConfiguration {
    /// Temporary SLAAC address generation is enabled.
    Enabled {
        /// The maximum amount of time that a temporary address can be considered
        /// valid, from the time of its creation.
        temp_valid_lifetime: NonZeroDuration,

        /// The maximum amount of time that a temporary address can be preferred,
        /// from the time of its creation.
        temp_preferred_lifetime: NonZeroDuration,

        /// The number of times to attempt to pick a new temporary address after DAD
        /// detects a duplicate before stopping and giving up on temporary address
        /// generation for that prefix.
        temp_idgen_retries: u8,
    },
    /// Temporary SLAAC address generation is disabled.
    Disabled,
}

impl Default for TemporarySlaacAddressConfiguration {
    fn default() -> Self {
        Self::Disabled
    }
}

impl TemporarySlaacAddressConfiguration {
    /// Default TEMP_VALID_LIFETIME specified by [RFC 8981 Section 3.8].
    ///
    /// [RFC 8981 Section 3.8]: https://www.rfc-editor.org/rfc/rfc8981#section-3.8
    pub const DEFAULT_TEMP_VALID_LIFETIME: NonZeroDuration = // 2 days
        NonZeroDuration::from_secs(2 * 24 * 60 * 60u64).unwrap();

    /// Default TEMP_PREFERRED_LIFETIME specified by [RFC 8981 Section 3.8].
    ///
    /// [RFC 8981 Section 3.8]: https://www.rfc-editor.org/rfc/rfc8981#section-3.8
    pub const DEFAULT_TEMP_PREFERRED_LIFETIME: NonZeroDuration = // 1 day
        NonZeroDuration::from_secs(1 * 24 * 60 * 60u64).unwrap();

    /// Default TEMP_IDGEN_RETRIES specified by [RFC 8981 Section 3.8].
    ///
    /// [RFC 8981 Section 3.8]: https://www.rfc-editor.org/rfc/rfc8981#section-3.8
    pub const DEFAULT_TEMP_IDGEN_RETRIES: u8 = 3;

    /// Constructs a new instance with default values.
    pub fn enabled_with_rfc_defaults() -> Self {
        Self::Enabled {
            temp_valid_lifetime: Self::DEFAULT_TEMP_VALID_LIFETIME,
            temp_preferred_lifetime: Self::DEFAULT_TEMP_PREFERRED_LIFETIME,
            temp_idgen_retries: Self::DEFAULT_TEMP_IDGEN_RETRIES,
        }
    }

    /// Returns if `self` is enabled.
    pub fn is_enabled(&self) -> bool {
        match self {
            Self::Enabled { .. } => true,
            Self::Disabled => false,
        }
    }
}

/// The configuration for SLAAC.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SlaacConfiguration {
    /// Configuration to enable stable address assignment.
    pub enable_stable_addresses: bool,

    /// Configuration for temporary address assignment.
    ///
    /// If `None`, temporary addresses will not be assigned to interfaces.
    ///
    /// If Some, specifies the configuration parameters for temporary addressing,
    /// including those relating to how long temporary addresses should remain
    /// preferred and valid.
    pub temporary_address_configuration: TemporarySlaacAddressConfiguration,
}

impl SlaacConfiguration {
    /// Updates self and returns the previous values in a new update structure.
    pub fn update(
        &mut self,
        SlaacConfigurationUpdate {
        enable_stable_addresses,
        temporary_address_configuration,
    }: SlaacConfigurationUpdate,
    ) -> SlaacConfigurationUpdate {
        fn get_prev_and_update<T>(old: &mut T, update: Option<T>) -> Option<T> {
            update.map(|new| core::mem::replace(old, new))
        }
        SlaacConfigurationUpdate {
            enable_stable_addresses: get_prev_and_update(
                &mut self.enable_stable_addresses,
                enable_stable_addresses,
            ),
            temporary_address_configuration: get_prev_and_update(
                &mut self.temporary_address_configuration,
                temporary_address_configuration,
            ),
        }
    }
}

/// An update structure for [`SlaacConfiguration`].
///
/// Only fields with variant `Some` are updated.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SlaacConfigurationUpdate {
    /// Configuration to enable stable address assignment.
    pub enable_stable_addresses: Option<bool>,

    /// Update value for temporary address configuration.
    pub temporary_address_configuration: Option<TemporarySlaacAddressConfiguration>,
}

#[derive(PartialEq, Eq)]
enum SlaacType {
    Stable,
    Temporary,
}

impl core::fmt::Debug for SlaacType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SlaacType::Stable => f.write_str("stable"),
            SlaacType::Temporary => f.write_str("temporary"),
        }
    }
}

impl<'a, Instant> From<&'a SlaacConfig<Instant>> for SlaacType {
    fn from(slaac_config: &'a SlaacConfig<Instant>) -> Self {
        match slaac_config {
            SlaacConfig::Stable { .. } => SlaacType::Stable,
            SlaacConfig::Temporary { .. } => SlaacType::Temporary,
        }
    }
}

/// The minimum REGEN_ADVANCE as specified in [RFC 8981 Section 3.8].
///
/// [RFC 8981 Section 3.8]: https://datatracker.ietf.org/doc/html/rfc8981#section-3.8
// As per [RFC 8981 Section 3.8],
//
//   REGEN_ADVANCE
//      2 + (TEMP_IDGEN_RETRIES * DupAddrDetectTransmits * RetransTimer /
//      1000)
//
//      ..., such that REGEN_ADVANCE is expressed in seconds.
pub const SLAAC_MIN_REGEN_ADVANCE: NonZeroDuration = NonZeroDuration::from_secs(2).unwrap();

/// Computes REGEN_ADVANCE as specified in [RFC 8981 Section 3.8].
///
/// [RFC 8981 Section 3.8]: http://tools.ietf.org/html/rfc8981#section-3.8
fn regen_advance(
    temp_idgen_retries: u8,
    retrans_timer: Duration,
    dad_transmits: u16,
) -> NonZeroDuration {
    // Per the RFC, REGEN_ADVANCE in seconds =
    //   2 + (TEMP_IDGEN_RETRIES * DupAddrDetectTransmits * RetransTimer / 1000)
    //
    // where RetransTimer is in milliseconds. Since values here are kept as
    // Durations, there is no need to apply scale factors.
    SLAAC_MIN_REGEN_ADVANCE
        + retrans_timer
            .checked_mul(u32::from(temp_idgen_retries) * u32::from(dad_transmits))
            .unwrap_or(Duration::ZERO)
}

/// Computes the DESYNC_FACTOR as specified in [RFC 8981 section 3.8].
///
/// Per the RFC,
///
///    DESYNC_FACTOR
///       A random value within the range 0 - MAX_DESYNC_FACTOR.  It
///       is computed each time a temporary address is generated, and
///       is associated with the corresponding address.  It MUST be
///       smaller than (TEMP_PREFERRED_LIFETIME - REGEN_ADVANCE).
///
/// Returns `None` if a DESYNC_FACTOR value cannot be calculated. This will
/// occur when REGEN_ADVANCE is larger than TEMP_PREFERRED_LIFETIME as no valid
/// DESYNC_FACTOR exists that is greater than or equal to 0.
///
/// [RFC 8981 Section 3.8]: http://tools.ietf.org/html/rfc8981#section-3.8
fn desync_factor<R: Rng>(
    rng: &mut R,
    temp_preferred_lifetime: NonZeroDuration,
    regen_advance: NonZeroDuration,
) -> Option<Duration> {
    let temp_preferred_lifetime = temp_preferred_lifetime.get();

    // Per RFC 8981 Section 3.8:
    //    MAX_DESYNC_FACTOR
    //       0.4 * TEMP_PREFERRED_LIFETIME.  Upper bound on DESYNC_FACTOR.
    //
    //       |  Rationale: Setting MAX_DESYNC_FACTOR to 0.4
    //       |  TEMP_PREFERRED_LIFETIME results in addresses that have
    //       |  statistically different lifetimes, and a maximum of three
    //       |  concurrent temporary addresses when the default values
    //       |  specified in this section are employed.
    //    DESYNC_FACTOR
    //       A random value within the range 0 - MAX_DESYNC_FACTOR.  It
    //       is computed each time a temporary address is generated, and
    //       is associated with the corresponding address.  It MUST be
    //       smaller than (TEMP_PREFERRED_LIFETIME - REGEN_ADVANCE).
    temp_preferred_lifetime.checked_sub(regen_advance.get()).map(|max_desync_factor| {
        let max_desync_factor =
            core::cmp::min(max_desync_factor, (temp_preferred_lifetime * 2) / 5);
        rng.sample(Uniform::new(Duration::ZERO, max_desync_factor))
    })
}

fn regenerate_temporary_slaac_addr<BC: SlaacBindingsContext<CC::DeviceId>, CC: SlaacContext<BC>>(
    bindings_ctx: &mut BC,
    addrs_config: SlaacAddrsMutAndConfig<'_, BC, CC::SlaacAddrs<'_>>,
    slaac_state: &mut SlaacState<BC>,
    device_id: &CC::DeviceId,
    addr_subnet: &AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
) {
    let SlaacAddrsMutAndConfig {
        addrs: slaac_addrs,
        config,
        dad_transmits,
        retrans_timer,
        interface_identifier: iid,
        temp_secret_key,
        _marker,
    } = addrs_config;
    let SlaacState { timers } = slaac_state;
    let now = bindings_ctx.now();

    enum Action {
        SkipRegen,
        Regen { valid_for: NonZeroDuration, preferred_for: Duration },
    }

    let action = slaac_addrs.with_addrs(|addrs| {
        let entry = {
            let mut found_entry = None;

            for entry in addrs {
                if entry.addr_sub.subnet() != addr_subnet.subnet() {
                    continue;
                }

                // It's possible that there are multiple non-deprecated temporary
                // addresses in a subnet for this host (if prefix updates are received
                // after regen but before deprecation). Per RFC 8981 Section 3.5:
                //
                //   Note that, in normal operation, except for the transient period
                //   when a temporary address is being regenerated, at most one
                //   temporary address per prefix should be in a nondeprecated state at
                //   any given time on a given interface.
                //
                // In order to tend towards only one non-deprecated temporary address on
                // a subnet, we ignore all but the last regen timer for the
                // non-deprecated addresses in a subnet.
                if !entry.config.preferred_lifetime.is_deprecated() {
                    if let Some((entry, regen_at)) = timers
                        .get(&InnerSlaacTimerId::RegenerateTemporaryAddress {
                            addr_subnet: entry.addr_sub,
                        })
                        .map(|(instant, ())| (entry, instant))
                    {
                        debug!(
                            "ignoring regen event at {:?} for {:?} since {:?} \
                                will regenerate after at {:?}",
                            bindings_ctx.now(),
                            addr_subnet,
                            entry.addr_sub.addr(),
                            regen_at
                        );
                        return Action::SkipRegen;
                    }
                }

                if &entry.addr_sub == addr_subnet {
                    assert_matches!(found_entry, None);
                    found_entry = Some(entry);
                }
            }

            found_entry.unwrap_or_else(|| panic!("couldn't find {:?} to regenerate", addr_subnet))
        };

        assert!(
            !entry.config.preferred_lifetime.is_deprecated(),
            "can't regenerate deprecated address {:?}",
            addr_subnet
        );

        let TemporarySlaacConfig { creation_time, desync_factor, valid_until, dad_counter: _ } =
            match entry.config.inner {
                SlaacConfig::Temporary(temporary_config) => temporary_config,
                SlaacConfig::Stable { valid_until: _ } => unreachable!(
                    "can't regenerate a temporary address for {:?}, which is stable",
                    addr_subnet
                ),
            };

        let SlaacConfiguration { enable_stable_addresses: _, temporary_address_configuration } =
            config;
        let temp_valid_lifetime = match temporary_address_configuration {
            TemporarySlaacAddressConfiguration::Enabled {
                temp_valid_lifetime,
                temp_preferred_lifetime: _,
                temp_idgen_retries: _,
            } => temp_valid_lifetime,
            TemporarySlaacAddressConfiguration::Disabled => return Action::SkipRegen,
        };

        let (deprecate_at, ()) = timers
            .get(&InnerSlaacTimerId::DeprecateSlaacAddress { addr: addr_subnet.addr() })
            .unwrap_or_else(|| {
                unreachable!(
                    "temporary SLAAC address {:?} had a regen timer fire but \
                    does not have a deprecation timer",
                    addr_subnet.addr()
                )
            });
        let preferred_for = deprecate_at.saturating_duration_since(creation_time) + desync_factor;

        // It's possible this `valid_for` value is larger than `temp_valid_lifetime`
        // (e.g. if the NDP configuration was changed since this address was
        // generated). That's okay, because `add_slaac_addr_sub` will apply the
        // current maximum valid lifetime when called below.
        let valid_for = valid_until
            .checked_duration_since(creation_time)
            .and_then(NonZeroDuration::new)
            .unwrap_or(temp_valid_lifetime);

        Action::Regen { valid_for, preferred_for }
    });

    match action {
        Action::SkipRegen => {}
        Action::Regen { valid_for, preferred_for } => add_slaac_addr_sub::<_, CC>(
            slaac_addrs,
            slaac_state,
            bindings_ctx,
            device_id,
            now,
            SlaacInitConfig::Temporary { dad_count: 0 },
            NonZeroNdpLifetime::Finite(valid_for),
            NonZeroDuration::new(preferred_for).map(NonZeroNdpLifetime::Finite),
            &addr_subnet.subnet(),
            config,
            dad_transmits,
            retrans_timer,
            iid,
            temp_secret_key,
        ),
    }
}

#[derive(Copy, Clone, Debug)]
enum SlaacInitConfig {
    Stable,
    Temporary { dad_count: u8 },
}

impl SlaacInitConfig {
    fn new(slaac_type: SlaacType) -> Self {
        match slaac_type {
            SlaacType::Stable => Self::Stable,
            SlaacType::Temporary => Self::Temporary { dad_count: 0 },
        }
    }
}

/// Checks whether the address has an IID that doesn't conflict with existing
/// IANA reserved ranges.
///
/// Compares against the ranges defined by various RFCs and listed at
/// https://www.iana.org/assignments/ipv6-interface-ids/ipv6-interface-ids.xhtml
fn has_iana_allowed_iid(address: Ipv6Addr) -> bool {
    let mut iid = [0u8; 8];
    const U64_SUFFIX_LEN: usize = Ipv6Addr::BYTES as usize - u64::BITS as usize / 8;
    iid.copy_from_slice(&address.bytes()[U64_SUFFIX_LEN..]);
    let iid = u64::from_be_bytes(iid);
    match iid {
        // Subnet-Router Anycast
        0x0000_0000_0000_0000 => false,
        // Consolidated match for
        // - Ethernet Block: 0x200:5EFF:FE00:0000-0200:4EFF:FE00:5212
        // - Proxy Mobile: 0x200:5EFF:FE00:5213
        // - Ethernet Block: 0x200:5EFF:FE00:5214-0200:4EFF:FEFF:FFFF
        0x0200_5EFF_FE00_0000..=0x0200_5EFF_FEFF_FFFF => false,
        // Subnet Anycast Addresses
        0xFDFF_FFFF_FFFF_FF80..=0xFDFF_FFFF_FFFF_FFFF => false,

        // All other IIDs not in the reserved ranges
        _iid => true,
    }
}

/// Generate a stable IPv6 Address as defined by RFC 4862 section 5.5.3.d.
///
/// The generated address will be of the format:
///
/// |            128 - N bits               |       N bits           |
/// +---------------------------------------+------------------------+
/// |            link prefix                |  interface identifier  |
/// +----------------------------------------------------------------+
///
/// # Panics
///
/// Panics if a valid IPv6 unicast address cannot be formed with the provided
/// prefix and interface identifier, or if the prefix length is not a multiple
/// of 8 bits.
fn generate_stable_address(
    prefix: &Subnet<Ipv6Addr>,
    iid: &[u8],
) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
    if prefix.prefix() % 8 != 0 {
        unimplemented!(
            "generate_stable_address: not implemented for when prefix length is not a multiple of \
            8 bits"
        );
    }

    let prefix_len = prefix.prefix() / u8::try_from(u8::BITS).unwrap();

    assert_eq!(usize::from(Ipv6Addr::BYTES - prefix_len), iid.len());

    let mut address = prefix.network().ipv6_bytes();

    // TODO(https://fxbug.dev/42148800): Generate stable addresses with opaque IIDs.
    address[prefix_len.into()..].copy_from_slice(&iid);

    let address = AddrSubnet::new(Ipv6Addr::from(address), prefix.prefix()).unwrap();
    assert_eq!(address.subnet(), *prefix);

    address
}

/// Generate a temporary IPv6 Global Address.
///
/// The generated address will be of the format:
///
/// |            128 - N bits              |        N bits           |
/// +--------------------------------------+-------------------------+
/// |            link prefix               |  randomized identifier  |
/// +----------------------------------------------------------------+
///
/// # Panics
///
/// Panics if a valid IPv6 unicast address cannot be formed with the provided
/// prefix, or if the prefix length is not a multiple of 8 bits.
fn generate_global_temporary_address(
    prefix: &Subnet<Ipv6Addr>,
    iid: &[u8; 8],
    seed: u64,
    secret_key: &IidSecret,
) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
    let prefix_len = usize::from(prefix.prefix() / 8);

    assert_eq!(usize::from(Ipv6Addr::BYTES) - prefix_len, iid.len());
    let mut address = prefix.network().ipv6_bytes();

    // TODO(https://fxbug.dev/368449998): Use the algorithm in RFC 8981
    // instead of the one for stable SLAAc addresses as described in RFC 7217.
    let interface_identifier = OpaqueIid::new(
        /* prefix */ *prefix,
        /* net_iface */ iid,
        /* net_id */ None::<[_; 0]>,
        /* nonce */ OpaqueIidNonce::Random(seed),
        /* secret_key */ secret_key,
    );
    let suffix_bytes = &interface_identifier.to_be_bytes()[..(address.len() - prefix_len)];
    address[prefix_len..].copy_from_slice(suffix_bytes);

    let address = AddrSubnet::new(Ipv6Addr::from(address), prefix.prefix()).unwrap();
    assert_eq!(address.subnet(), *prefix);

    address
}

fn add_slaac_addr_sub<BC: SlaacBindingsContext<CC::DeviceId>, CC: SlaacContext<BC>>(
    slaac_addrs: &mut CC::SlaacAddrs<'_>,
    slaac_state: &mut SlaacState<BC>,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    now: BC::Instant,
    slaac_config: SlaacInitConfig,
    prefix_valid_for: NonZeroNdpLifetime,
    prefix_preferred_for: Option<NonZeroNdpLifetime>,
    subnet: &Subnet<Ipv6Addr>,
    config: SlaacConfiguration,
    dad_transmits: Option<NonZeroU16>,
    retrans_timer: Duration,
    iid: [u8; 8],
    temp_secret_key: IidSecret,
) {
    if subnet.prefix() != REQUIRED_PREFIX_BITS {
        // If the sum of the prefix length and interface identifier length does
        // not equal 128 bits, the Prefix Information option MUST be ignored, as
        // per RFC 4862 section 5.5.3.
        error!(
            "receive_ndp_packet: autonomous prefix length {:?} and interface identifier length {:?} cannot form valid IPv6 address, ignoring",
            subnet.prefix(),
            REQUIRED_PREFIX_BITS
        );
        return;
    }

    struct PreferredForAndRegenAt<Instant>(NonZeroNdpLifetime, Option<Instant>);

    let SlaacConfiguration { enable_stable_addresses, temporary_address_configuration } = config;
    let SlaacState { timers } = slaac_state;

    let (valid_until, preferred_and_regen, slaac_config, mut addresses) = match slaac_config {
        SlaacInitConfig::Stable => {
            if !enable_stable_addresses {
                trace!("stable SLAAC addresses are disabled on device {:?}", device_id);
                return;
            }

            let valid_until = Lifetime::from_ndp(now, prefix_valid_for);
            (
                valid_until,
                prefix_preferred_for.map(|p| PreferredForAndRegenAt(p, None)),
                SlaacConfig::Stable { valid_until },
                // Generate the global address as defined by RFC 4862 section 5.5.3.d.
                //
                // TODO(https://fxbug.dev/42148800): Support regenerating address.
                either::Either::Left(core::iter::once(generate_stable_address(&subnet, &iid[..]))),
            )
        }
        SlaacInitConfig::Temporary { dad_count } => {
            match temporary_address_configuration {
                TemporarySlaacAddressConfiguration::Enabled {
                    temp_valid_lifetime,
                    temp_preferred_lifetime,
                    temp_idgen_retries,
                } => {
                    let per_attempt_random_seed: u64 = bindings_ctx.rng().gen();

                    // Per RFC 8981 Section 3.4.4:
                    //    When creating a temporary address, DESYNC_FACTOR MUST be computed
                    //    and associated with the newly created address, and the address
                    //    lifetime values MUST be derived from the corresponding prefix as
                    //    follows:
                    //
                    //    *  Its valid lifetime is the lower of the Valid Lifetime of the
                    //       prefix and TEMP_VALID_LIFETIME.
                    //
                    //    *  Its preferred lifetime is the lower of the Preferred Lifetime
                    //       of the prefix and TEMP_PREFERRED_LIFETIME - DESYNC_FACTOR.
                    let valid_for = match prefix_valid_for {
                        NonZeroNdpLifetime::Finite(prefix_valid_for) => {
                            core::cmp::min(prefix_valid_for, temp_valid_lifetime)
                        }
                        NonZeroNdpLifetime::Infinite => temp_valid_lifetime,
                    };

                    let regen_advance = regen_advance(
                        temp_idgen_retries,
                        retrans_timer,
                        dad_transmits.map_or(0, NonZeroU16::get),
                    );

                    let mut seed = per_attempt_random_seed;
                    let addresses = either::Either::Right(core::iter::repeat_with(move || {
                        // RFC 8981 Section 3.3.3 specifies that
                        //
                        //   The resulting IID MUST be compared against the reserved
                        //   IPv6 IIDs and against those IIDs already employed in an
                        //   address of the same network interface and the same network
                        //   prefix.  In the event that an unacceptable identifier has
                        //   been generated, the DAD_Counter should be incremented by 1,
                        //   and the algorithm should be restarted from the first step.
                        loop {
                            let address = generate_global_temporary_address(
                                &subnet,
                                &iid,
                                seed,
                                &temp_secret_key,
                            );
                            seed = seed.wrapping_add(1);

                            if has_iana_allowed_iid(address.addr().get()) {
                                break address;
                            }
                        }
                    }));

                    let valid_until = now.saturating_add(valid_for.get());

                    let desync_factor = if let Some(d) = desync_factor(
                        &mut bindings_ctx.rng(),
                        temp_preferred_lifetime,
                        regen_advance,
                    ) {
                        d
                    } else {
                        // We only fail to calculate a desync factor when the configured
                        // maximum temporary address preferred lifetime is less than
                        // REGEN_ADVANCE and per RFC 8981 Section 3.4.5,
                        //
                        //   A temporary address is created only if this calculated
                        //   preferred lifetime is greater than REGEN_ADVANCE time
                        //   units.
                        trace!(
                            "failed to calculate DESYNC_FACTOR; \
                                temp_preferred_lifetime={:?}, regen_advance={:?}",
                            temp_preferred_lifetime,
                            regen_advance,
                        );
                        return;
                    };

                    let preferred_for = prefix_preferred_for.and_then(|prefix_preferred_for| {
                        temp_preferred_lifetime
                            .get()
                            .checked_sub(desync_factor)
                            .and_then(NonZeroDuration::new)
                            .map(|d| prefix_preferred_for.min_finite_duration(d))
                    });

                    // RFC 8981 Section 3.4.5:
                    //
                    //   A temporary address is created only if this calculated
                    //   preferred lifetime is greater than REGEN_ADVANCE time
                    //   units.
                    let preferred_for_and_regen_at = match preferred_for {
                        None => return,
                        Some(preferred_for) => {
                            match preferred_for.get().checked_sub(regen_advance.get()) {
                                Some(before_regen) => PreferredForAndRegenAt(
                                    NonZeroNdpLifetime::Finite(preferred_for),
                                    // Checked add, if we overflow it's as good
                                    // as not ever having to regenerate.
                                    now.checked_add(before_regen),
                                ),
                                None => {
                                    trace!(
                                        "receive_ndp_packet: preferred lifetime of {:?} \
                                            for subnet {:?} is too short to allow regen",
                                        preferred_for,
                                        subnet
                                    );
                                    return;
                                }
                            }
                        }
                    };

                    (
                        Lifetime::Finite(valid_until),
                        Some(preferred_for_and_regen_at),
                        SlaacConfig::Temporary(TemporarySlaacConfig {
                            desync_factor,
                            valid_until,
                            creation_time: now,
                            dad_counter: dad_count,
                        }),
                        addresses,
                    )
                }
                TemporarySlaacAddressConfiguration::Disabled => {
                    trace!(
                        "receive_ndp_packet: temporary addresses are disabled on device {:?}",
                        device_id
                    );
                    return;
                }
            }
        }
    };

    // Attempt to add the address to the device.
    loop {
        let address = match addresses.next() {
            Some(address) => address,
            // No more addresses to try - do nothing further.
            None => {
                trace!(
                    "exhausted possible SLAAC addresses without assigning on device {:?}",
                    device_id
                );
                return;
            }
        };

        // Calculate the durations to instants relative to the previously
        // recorded `now` value. This helps prevent skew in cases where this
        // task gets preempted and isn't scheduled for some period of time
        // between recording `now` and here.
        let (preferred_lifetime, regen_at) = match preferred_and_regen {
            Some(PreferredForAndRegenAt(preferred_for, regen_at)) => {
                (PreferredLifetime::preferred_for(now, preferred_for), regen_at)
            }
            None => (PreferredLifetime::Deprecated, None),
        };
        let config = Ipv6AddrSlaacConfig { inner: slaac_config, preferred_lifetime };

        // TODO(https://fxbug.dev/42172850): Should bindings be the one to actually
        // assign the address to maintain a "single source of truth"?
        let res = slaac_addrs.add_addr_sub_and_then(
            bindings_ctx,
            address,
            config,
            |SlaacAddressEntryMut { addr_sub, config: _ }, ctx| {
                // Set the valid lifetime for this address.
                //
                // Must not have reached this point if the address was already assigned
                // to a device.
                match valid_until {
                    Lifetime::Finite(valid_until) => {
                        assert_eq!(
                            timers.schedule_instant(
                                ctx,
                                InnerSlaacTimerId::InvalidateSlaacAddress { addr: addr_sub.addr() },
                                (),
                                valid_until,
                            ),
                            None
                        );
                    }
                    Lifetime::Infinite => {}
                }

                let deprecate_timer_id =
                    InnerSlaacTimerId::DeprecateSlaacAddress { addr: addr_sub.addr() };

                match preferred_lifetime {
                    PreferredLifetime::Preferred(Lifetime::Finite(instant)) => {
                        assert_eq!(
                            timers.schedule_instant(ctx, deprecate_timer_id, (), instant,),
                            None
                        );
                    }
                    PreferredLifetime::Preferred(Lifetime::Infinite) => {}
                    PreferredLifetime::Deprecated => {
                        assert_eq!(timers.cancel(ctx, &deprecate_timer_id), None);
                    }
                }

                match regen_at {
                    Some(regen_at) => assert_eq!(
                        timers.schedule_instant(
                            ctx,
                            InnerSlaacTimerId::RegenerateTemporaryAddress { addr_subnet: addr_sub },
                            (),
                            regen_at,
                        ),
                        None
                    ),
                    None => (),
                }
                addr_sub
            },
        );

        match res {
            Err(ExistsError) => {
                trace!("IPv6 SLAAC address {:?} already exists on device {:?}", address, device_id);

                // Try the next address.
                //
                // TODO(https://fxbug.dev/42050670): Limit number of attempts.
                slaac_addrs.increment(|counters| &counters.generated_slaac_addr_exists);
            }
            Ok(addr_sub) => {
                trace!("receive_ndp_packet: Successfully configured new IPv6 address {:?} on device {:?} via SLAAC", addr_sub, device_id);
                break;
            }
        }
    }
}

#[cfg(any(test, feature = "testutils"))]
pub(crate) mod testutil {
    use super::*;

    use alloc::collections::HashMap;

    use net_types::ip::Ipv6;

    use crate::internal::device::{IpDeviceBindingsContext, Ipv6DeviceConfigurationContext};

    /// Collects all the currently installed SLAAC timers for `device_id` in
    /// `core_ctx`.
    pub fn collect_slaac_timers_integration<CC, BC>(
        core_ctx: &mut CC,
        device_id: &CC::DeviceId,
    ) -> HashMap<InnerSlaacTimerId, BC::Instant>
    where
        CC: Ipv6DeviceConfigurationContext<BC>,
        for<'a> CC::Ipv6DeviceStateCtx<'a>: SlaacContext<BC>,
        BC: IpDeviceBindingsContext<Ipv6, CC::DeviceId> + SlaacBindingsContext<CC::DeviceId>,
    {
        core_ctx.with_ipv6_device_configuration(device_id, |_, mut core_ctx| {
            core_ctx.with_slaac_addrs_mut(device_id, |_, state| {
                state.timers().iter().map(|(k, (), t)| (*k, *t)).collect::<HashMap<_, _>>()
            })
        })
    }

    /// Returns the address and subnet used by SLAAC on `subnet` with interface
    /// identifier `iid`.
    pub fn calculate_slaac_addr_sub(
        subnet: Subnet<Ipv6Addr>,
        iid: [u8; 8],
    ) -> AddrSubnet<Ipv6Addr, Ipv6DeviceAddr> {
        let mut bytes = subnet.network().ipv6_bytes();
        bytes[8..].copy_from_slice(&iid);
        AddrSubnet::new(Ipv6Addr::from_bytes(bytes), subnet.prefix()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::convert::TryFrom as _;

    use net_declare::net::ip_v6;
    use netstack3_base::testutil::{
        assert_empty, FakeBindingsCtx, FakeCoreCtx, FakeCryptoRng, FakeDeviceId, FakeInstant,
        FakeTimerCtxExt as _, FakeWeakDeviceId,
    };
    use netstack3_base::{CtxPair, IntoCoreTimerCtx};
    use test_case::test_case;

    use super::*;

    struct FakeSlaacContext {
        config: SlaacConfiguration,
        dad_transmits: Option<NonZeroU16>,
        retrans_timer: Duration,
        iid: [u8; 8],
        slaac_addrs: FakeSlaacAddrs,
        slaac_state: SlaacState<FakeBindingsCtxImpl>,
    }

    type FakeCoreCtxImpl = FakeCoreCtx<FakeSlaacContext, (), FakeDeviceId>;
    type FakeBindingsCtxImpl = FakeBindingsCtx<
        SlaacTimerId<FakeWeakDeviceId<FakeDeviceId>>,
        IpDeviceEvent<FakeDeviceId, Ipv6, FakeInstant>,
        (),
        (),
    >;

    #[derive(Default)]
    struct FakeSlaacAddrs {
        slaac_addrs: Vec<SlaacAddressEntry<FakeInstant>>,
        non_slaac_addr: Option<Ipv6DeviceAddr>,
        counters: SlaacCounters,
    }

    impl<'a> CounterContext<SlaacCounters> for &'a mut FakeSlaacAddrs {
        fn with_counters<O, F: FnOnce(&SlaacCounters) -> O>(&self, cb: F) -> O {
            cb(&self.counters)
        }
    }

    impl<'a> SlaacAddresses<FakeBindingsCtxImpl> for &'a mut FakeSlaacAddrs {
        fn for_each_addr_mut<F: FnMut(SlaacAddressEntryMut<'_, FakeInstant>)>(
            &mut self,
            mut cb: F,
        ) {
            let FakeSlaacAddrs { slaac_addrs, non_slaac_addr: _, counters: _ } = self;
            slaac_addrs.iter_mut().for_each(|SlaacAddressEntry { addr_sub, config }| {
                cb(SlaacAddressEntryMut { addr_sub: *addr_sub, config })
            })
        }

        type AddrsIter<'b> =
            core::iter::Cloned<core::slice::Iter<'b, SlaacAddressEntry<FakeInstant>>>;
        fn with_addrs<O, F: FnOnce(Self::AddrsIter<'_>) -> O>(&mut self, cb: F) -> O {
            let FakeSlaacAddrs { slaac_addrs, non_slaac_addr: _, counters: _ } = self;
            cb(slaac_addrs.iter().cloned())
        }

        fn add_addr_sub_and_then<
            O,
            F: FnOnce(SlaacAddressEntryMut<'_, FakeInstant>, &mut FakeBindingsCtxImpl) -> O,
        >(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            add_addr_sub: AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>,
            config: Ipv6AddrSlaacConfig<FakeInstant>,
            and_then: F,
        ) -> Result<O, ExistsError> {
            let FakeSlaacAddrs { slaac_addrs, non_slaac_addr, counters: _ } = self;

            if non_slaac_addr.is_some_and(|a| a == add_addr_sub.addr()) {
                return Err(ExistsError);
            }

            if slaac_addrs.iter_mut().any(|e| e.addr_sub.addr() == add_addr_sub.addr()) {
                return Err(ExistsError);
            }

            slaac_addrs.push(SlaacAddressEntry { addr_sub: add_addr_sub, config });

            let SlaacAddressEntry { addr_sub, config } = slaac_addrs.iter_mut().last().unwrap();

            Ok(and_then(SlaacAddressEntryMut { addr_sub: *addr_sub, config }, bindings_ctx))
        }

        fn remove_addr(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            addr: &Ipv6DeviceAddr,
        ) -> Result<(AddrSubnet<Ipv6Addr, Ipv6DeviceAddr>, SlaacConfig<FakeInstant>), NotFoundError>
        {
            let FakeSlaacAddrs { slaac_addrs, non_slaac_addr: _, counters: _ } = self;

            slaac_addrs
                .iter()
                .enumerate()
                .find_map(|(i, a)| (&a.addr_sub.addr() == addr).then(|| i))
                .ok_or(NotFoundError)
                .map(|i| {
                    let SlaacAddressEntry { addr_sub, config: Ipv6AddrSlaacConfig { inner, .. } } =
                        slaac_addrs.remove(i);
                    (addr_sub, inner)
                })
        }
    }

    impl SlaacContext<FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type SlaacAddrs<'a>
            = &'a mut FakeSlaacAddrs
        where
            FakeCoreCtxImpl: 'a;

        fn with_slaac_addrs_mut_and_configs<
            O,
            F: FnOnce(
                SlaacAddrsMutAndConfig<'_, FakeBindingsCtxImpl, &'_ mut FakeSlaacAddrs>,
                &mut SlaacState<FakeBindingsCtxImpl>,
            ) -> O,
        >(
            &mut self,
            &FakeDeviceId: &FakeDeviceId,
            cb: F,
        ) -> O {
            let FakeSlaacContext {
                config,
                dad_transmits,
                retrans_timer,
                iid,
                slaac_addrs,
                slaac_state,
                ..
            } = &mut self.state;
            let mut slaac_addrs = slaac_addrs;
            cb(
                SlaacAddrsMutAndConfig {
                    addrs: &mut slaac_addrs,
                    config: *config,
                    dad_transmits: *dad_transmits,
                    retrans_timer: *retrans_timer,
                    interface_identifier: *iid,
                    temp_secret_key: SECRET_KEY,
                    _marker: PhantomData,
                },
                slaac_state,
            )
        }
    }

    impl FakeSlaacContext {
        fn iter_slaac_addrs(&self) -> impl Iterator<Item = SlaacAddressEntry<FakeInstant>> + '_ {
            self.slaac_addrs.slaac_addrs.iter().cloned()
        }
    }

    fn new_timer_id() -> SlaacTimerId<FakeWeakDeviceId<FakeDeviceId>> {
        SlaacTimerId { device_id: FakeWeakDeviceId(FakeDeviceId) }
    }

    fn new_context(
        config: SlaacConfiguration,
        slaac_addrs: FakeSlaacAddrs,
        dad_transmits: Option<NonZeroU16>,
        retrans_timer: Duration,
    ) -> CtxPair<FakeCoreCtxImpl, FakeBindingsCtxImpl> {
        CtxPair::with_default_bindings_ctx(|bindings_ctx| {
            FakeCoreCtxImpl::with_state(FakeSlaacContext {
                config,
                dad_transmits,
                retrans_timer,
                iid: IID,
                slaac_addrs,
                slaac_state: SlaacState::new::<_, IntoCoreTimerCtx>(
                    bindings_ctx,
                    FakeWeakDeviceId(FakeDeviceId),
                ),
            })
        })
    }

    impl<Instant> SlaacAddressEntry<Instant> {
        fn to_deprecated(self) -> Self {
            let Self { addr_sub, config: Ipv6AddrSlaacConfig { inner, preferred_lifetime: _ } } =
                self;
            Self {
                addr_sub,
                config: Ipv6AddrSlaacConfig {
                    inner,
                    preferred_lifetime: PreferredLifetime::Deprecated,
                },
            }
        }
    }

    #[test_case(ip_v6!("1:2:3:4::"), false; "subnet-router anycast")]
    #[test_case(ip_v6!("::1"), true; "allowed 1")]
    #[test_case(ip_v6!("1:2:3:4::1"), true; "allowed 2")]
    #[test_case(ip_v6!("4:4:4:4:0200:5eff:fe00:1"), false; "first ethernet block")]
    #[test_case(ip_v6!("1:1:1:1:0200:5eff:fe00:5213"), false; "proxy mobile")]
    #[test_case(ip_v6!("8:8:8:8:0200:5eff:fe00:8000"), false; "second ethernet block")]
    #[test_case(ip_v6!("a:a:a:a:fdff:ffff:ffff:ffaa"), false; "subnet anycast")]
    #[test_case(ip_v6!("c:c:c:c:fe00::"), true; "allowed 3")]
    fn test_has_iana_allowed_iid(addr: Ipv6Addr, expect_allowed: bool) {
        assert_eq!(has_iana_allowed_iid(addr), expect_allowed);
    }

    const IID: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
    const DEFAULT_RETRANS_TIMER: Duration = Duration::from_secs(1);
    const SUBNET: Subnet<Ipv6Addr> = net_declare::net_subnet_v6!("200a::/64");

    #[test_case(0, 0, true; "zero lifetimes")]
    #[test_case(2, 1, true; "preferred larger than valid")]
    #[test_case(1, 2, false; "disabled")]
    fn dont_generate_address(
        preferred_lifetime_secs: u32,
        valid_lifetime_secs: u32,
        enable_stable_addresses: bool,
    ) {
        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration { enable_stable_addresses, ..Default::default() },
            Default::default(),
            None,
            DEFAULT_RETRANS_TIMER,
        );

        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(preferred_lifetime_secs),
            NonZeroNdpLifetime::from_u32_with_infinite(valid_lifetime_secs),
        );
        assert_empty(core_ctx.state.iter_slaac_addrs());
        bindings_ctx.timers.assert_no_timers_installed();
    }

    #[test_case(0; "deprecated")]
    #[test_case(1; "preferred")]
    fn generate_stable_address(preferred_lifetime_secs: u32) {
        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration { enable_stable_addresses: true, ..Default::default() },
            Default::default(),
            None,
            DEFAULT_RETRANS_TIMER,
        );

        let valid_lifetime_secs = preferred_lifetime_secs + 1;
        let addr_sub = testutil::calculate_slaac_addr_sub(SUBNET, IID);

        // Generate a new SLAAC address.
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(preferred_lifetime_secs),
            NonZeroNdpLifetime::from_u32_with_infinite(valid_lifetime_secs),
        );
        let address_created_deprecated = preferred_lifetime_secs == 0;
        let now = bindings_ctx.now();
        let valid_until = now + Duration::from_secs(valid_lifetime_secs.into());
        let preferred_lifetime = match preferred_lifetime_secs {
            0 => PreferredLifetime::Deprecated,
            secs => PreferredLifetime::preferred_until(now + Duration::from_secs(secs.into())),
        };
        let inner = SlaacConfig::Stable { valid_until: Lifetime::Finite(valid_until) };
        let entry = SlaacAddressEntry {
            addr_sub,
            config: Ipv6AddrSlaacConfig { inner, preferred_lifetime },
        };
        assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [entry],);
        let deprecate_timer_id = InnerSlaacTimerId::DeprecateSlaacAddress { addr: addr_sub.addr() };
        let invalidate_timer_id =
            InnerSlaacTimerId::InvalidateSlaacAddress { addr: addr_sub.addr() };
        if !address_created_deprecated {
            core_ctx.state.slaac_state.timers.assert_timers([
                (deprecate_timer_id, (), now + Duration::from_secs(preferred_lifetime_secs.into())),
                (invalidate_timer_id, (), valid_until),
            ]);

            // Trigger the deprecation timer.
            assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(new_timer_id()));
            let entry = SlaacAddressEntry {
                addr_sub,
                config: Ipv6AddrSlaacConfig {
                    inner,
                    preferred_lifetime: PreferredLifetime::Deprecated,
                },
            };
            assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [entry]);
        }
        core_ctx.state.slaac_state.timers.assert_timers([(invalidate_timer_id, (), valid_until)]);

        // Trigger the invalidation timer.
        assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(new_timer_id()));
        assert_empty(core_ctx.state.iter_slaac_addrs());
        bindings_ctx.timers.assert_no_timers_installed();
    }

    #[test]
    fn stable_address_conflict() {
        let addr_sub = testutil::calculate_slaac_addr_sub(SUBNET, IID);

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration { enable_stable_addresses: true, ..Default::default() },
            FakeSlaacAddrs {
                slaac_addrs: Default::default(),
                // Consider the address we will generate as already assigned without
                // SLAAC.
                non_slaac_addr: Some(addr_sub.addr()),
                counters: Default::default(),
            },
            None,
            DEFAULT_RETRANS_TIMER,
        );

        const LIFETIME_SECS: u32 = 1;

        // Generate a new SLAAC address.
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(LIFETIME_SECS),
            NonZeroNdpLifetime::from_u32_with_infinite(LIFETIME_SECS),
        );
        assert_empty(core_ctx.state.iter_slaac_addrs());
        bindings_ctx.timers.assert_no_timers_installed();
    }

    #[test]
    fn temporary_address_conflict() {
        const ONE_HOUR: NonZeroDuration = NonZeroDuration::from_secs(60 * 60).unwrap();
        const TEMP_IDGEN_RETRIES: u8 = 0;

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration {
                temporary_address_configuration: TemporarySlaacAddressConfiguration::Enabled {
                    temp_valid_lifetime: ONE_HOUR,
                    temp_preferred_lifetime: ONE_HOUR,
                    temp_idgen_retries: TEMP_IDGEN_RETRIES,
                },
                ..Default::default()
            },
            FakeSlaacAddrs::default(),
            None,
            DEFAULT_RETRANS_TIMER,
        );

        // Consider the address we will generate as already assigned without
        // SLAAC.
        let mut dup_rng = bindings_ctx.rng().deep_clone();
        let seed = dup_rng.gen();
        let first_attempt = generate_global_temporary_address(&SUBNET, &IID, seed, &SECRET_KEY);
        core_ctx.state.slaac_addrs.non_slaac_addr = Some(first_attempt.addr());

        // Generate a new temporary SLAAC address.
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            Some(NonZeroNdpLifetime::Finite(ONE_HOUR)),
            Some(NonZeroNdpLifetime::Finite(ONE_HOUR)),
        );

        // The new address will be regenerated so that it has a unique IID by
        // incrementing the RNG seed.
        let seed = seed.wrapping_add(1);
        let addr_sub = generate_global_temporary_address(&SUBNET, &IID, seed, &SECRET_KEY);
        assert_ne!(addr_sub, first_attempt);
        let regen_advance =
            regen_advance(TEMP_IDGEN_RETRIES, DEFAULT_RETRANS_TIMER, /* dad_transmits */ 0);
        let desync_factor = desync_factor(&mut dup_rng, ONE_HOUR, regen_advance).unwrap();
        let preferred_until = {
            let d = bindings_ctx.now() + ONE_HOUR.into();
            d - desync_factor
        };
        let entry = SlaacAddressEntry {
            addr_sub,
            config: Ipv6AddrSlaacConfig {
                inner: SlaacConfig::Temporary(TemporarySlaacConfig {
                    valid_until: bindings_ctx.now() + ONE_HOUR.into(),
                    desync_factor,
                    creation_time: bindings_ctx.now(),
                    dad_counter: 0,
                }),
                preferred_lifetime: PreferredLifetime::preferred_until(preferred_until),
            },
        };
        assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [entry]);
    }

    #[test_case(AddressRemovedReason::Manual; "manual")]
    #[test_case(AddressRemovedReason::DadFailed; "dad failed")]
    fn remove_stable_address(reason: AddressRemovedReason) {
        let addr_sub = testutil::calculate_slaac_addr_sub(SUBNET, IID);

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration { enable_stable_addresses: true, ..Default::default() },
            Default::default(),
            None,
            DEFAULT_RETRANS_TIMER,
        );

        const LIFETIME_SECS: u32 = 1;

        // Generate a new SLAAC address.
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(LIFETIME_SECS),
            NonZeroNdpLifetime::from_u32_with_infinite(LIFETIME_SECS),
        );
        let now = bindings_ctx.now();
        let valid_until = now + Duration::from_secs(LIFETIME_SECS.into());
        let entry = SlaacAddressEntry {
            addr_sub,
            config: Ipv6AddrSlaacConfig {
                inner: SlaacConfig::Stable { valid_until: Lifetime::Finite(valid_until) },
                preferred_lifetime: PreferredLifetime::preferred_until(valid_until),
            },
        };
        assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [entry]);

        let deprecate_timer_id = InnerSlaacTimerId::DeprecateSlaacAddress { addr: addr_sub.addr() };
        let invalidate_timer_id =
            InnerSlaacTimerId::InvalidateSlaacAddress { addr: addr_sub.addr() };
        core_ctx.state.slaac_state.timers.assert_timers([
            (deprecate_timer_id, (), now + Duration::from_secs(LIFETIME_SECS.into())),
            (invalidate_timer_id, (), valid_until),
        ]);

        // Remove the address and let SLAAC know the address was removed
        let config = {
            let SlaacAddressEntry {
                addr_sub: got_addr_sub,
                config: Ipv6AddrSlaacConfig { inner, preferred_lifetime },
            } = core_ctx.state.slaac_addrs.slaac_addrs.remove(0);
            assert_eq!(addr_sub, got_addr_sub);
            assert_eq!(preferred_lifetime, PreferredLifetime::preferred_until(valid_until));
            inner
        };
        SlaacHandler::on_address_removed(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            addr_sub,
            config,
            reason,
        );
        bindings_ctx.timers.assert_no_timers_installed();
    }

    struct RefreshStableAddressTimersTest {
        orig_pl_secs: u32,
        orig_vl_secs: u32,
        new_pl_secs: u32,
        new_vl_secs: u32,
        effective_new_vl_secs: u32,
    }

    const ONE_HOUR_AS_SECS: u32 = 60 * 60;
    const TWO_HOURS_AS_SECS: u32 = ONE_HOUR_AS_SECS * 2;
    const THREE_HOURS_AS_SECS: u32 = ONE_HOUR_AS_SECS * 3;
    const FOUR_HOURS_AS_SECS: u32 = ONE_HOUR_AS_SECS * 4;
    const INFINITE_LIFETIME: u32 = u32::MAX;
    const MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS: u32 =
        MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE.get().as_secs() as u32;
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: 1,
        orig_vl_secs: 1,
        new_pl_secs: 1,
        new_vl_secs: 1,
        effective_new_vl_secs: 1,
    }; "do nothing")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: 1,
        orig_vl_secs: 1,
        new_pl_secs: 2,
        new_vl_secs: 2,
        effective_new_vl_secs: 2,
    }; "increase lifetimes")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: 1,
        orig_vl_secs: 1,
        new_pl_secs: 0,
        new_vl_secs: 1,
        effective_new_vl_secs: 1,
    }; "deprecate address only")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: 0,
        orig_vl_secs: 1,
        new_pl_secs: 1,
        new_vl_secs: 1,
        effective_new_vl_secs: 1,
    }; "undeprecate address")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: 1,
        orig_vl_secs: 1,
        new_pl_secs: 0,
        new_vl_secs: 0,
        effective_new_vl_secs: 1,
    }; "deprecate address only with new valid lifetime of zero")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: ONE_HOUR_AS_SECS,
        orig_vl_secs: ONE_HOUR_AS_SECS,
        new_pl_secs: ONE_HOUR_AS_SECS - 1,
        new_vl_secs: ONE_HOUR_AS_SECS - 1,
        effective_new_vl_secs: ONE_HOUR_AS_SECS,
    }; "decrease preferred lifetime and ignore new valid lifetime if less than 2 hours and remaining lifetime")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: THREE_HOURS_AS_SECS,
        orig_vl_secs: THREE_HOURS_AS_SECS,
        new_pl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS - 1,
        new_vl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS - 1,
        effective_new_vl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS,
    }; "deprecate address only and bring valid lifetime down to 2 hours at max")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: ONE_HOUR_AS_SECS - 1,
        orig_vl_secs: ONE_HOUR_AS_SECS - 1,
        new_pl_secs: ONE_HOUR_AS_SECS - 1,
        new_vl_secs: ONE_HOUR_AS_SECS,
        effective_new_vl_secs: ONE_HOUR_AS_SECS,
    }; "increase valid lifetime if more than remaining valid lifetime")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: INFINITE_LIFETIME,
        orig_vl_secs: INFINITE_LIFETIME,
        new_pl_secs: INFINITE_LIFETIME,
        new_vl_secs: INFINITE_LIFETIME,
        effective_new_vl_secs: INFINITE_LIFETIME,
    }; "infinite lifetimes")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: ONE_HOUR_AS_SECS,
        orig_vl_secs: TWO_HOURS_AS_SECS,
        new_pl_secs: TWO_HOURS_AS_SECS,
        new_vl_secs: INFINITE_LIFETIME,
        effective_new_vl_secs: INFINITE_LIFETIME,
    }; "update valid lifetime from finite to infinite")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: ONE_HOUR_AS_SECS,
        orig_vl_secs: TWO_HOURS_AS_SECS,
        new_pl_secs: INFINITE_LIFETIME,
        new_vl_secs: INFINITE_LIFETIME,
        effective_new_vl_secs: INFINITE_LIFETIME,
    }; "update both lifetimes from finite to infinite")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: TWO_HOURS_AS_SECS,
        orig_vl_secs: INFINITE_LIFETIME,
        new_pl_secs: ONE_HOUR_AS_SECS,
        new_vl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS - 1,
        effective_new_vl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS,
    }; "update valid lifetime from infinite to finite")]
    #[test_case(RefreshStableAddressTimersTest {
        orig_pl_secs: INFINITE_LIFETIME,
        orig_vl_secs: INFINITE_LIFETIME,
        new_pl_secs: ONE_HOUR_AS_SECS,
        new_vl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS - 1,
        effective_new_vl_secs: MIN_PREFIX_VALID_LIFETIME_FOR_UPDATE_AS_SECS,
    }; "update both lifetimes from infinite to finite")]
    fn stable_address_timers(
        RefreshStableAddressTimersTest {
            orig_pl_secs,
            orig_vl_secs,
            new_pl_secs,
            new_vl_secs,
            effective_new_vl_secs,
        }: RefreshStableAddressTimersTest,
    ) {
        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration { enable_stable_addresses: true, ..Default::default() },
            Default::default(),
            None,
            DEFAULT_RETRANS_TIMER,
        );

        let addr_sub = testutil::calculate_slaac_addr_sub(SUBNET, IID);

        let deprecate_timer_id = InnerSlaacTimerId::DeprecateSlaacAddress { addr: addr_sub.addr() };
        let invalidate_timer_id =
            InnerSlaacTimerId::InvalidateSlaacAddress { addr: addr_sub.addr() };

        // Generate a new SLAAC address.
        let ndp_pl = NonZeroNdpLifetime::from_u32_with_infinite(orig_pl_secs);
        let ndp_vl = NonZeroNdpLifetime::from_u32_with_infinite(orig_vl_secs);
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            ndp_pl,
            ndp_vl,
        );
        let now = bindings_ctx.now();
        let mut expected_timers = Vec::new();
        let valid_until = match ndp_vl.expect("this test expects to create an address") {
            NonZeroNdpLifetime::Finite(d) => {
                let valid_until = now + d.get();
                expected_timers.push((invalidate_timer_id, (), valid_until));
                Lifetime::Finite(valid_until)
            }
            NonZeroNdpLifetime::Infinite => Lifetime::Infinite,
        };
        match ndp_pl {
            None | Some(NonZeroNdpLifetime::Infinite) => {}
            Some(NonZeroNdpLifetime::Finite(d)) => {
                expected_timers.push((deprecate_timer_id, (), now + d.get()))
            }
        }
        let entry = SlaacAddressEntry {
            addr_sub,
            config: Ipv6AddrSlaacConfig {
                inner: SlaacConfig::Stable { valid_until },
                preferred_lifetime: PreferredLifetime::maybe_preferred_for(now, ndp_pl),
            },
        };
        assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [entry]);
        core_ctx.state.slaac_state.timers.assert_timers(expected_timers);

        // Refresh timers.
        let ndp_pl = NonZeroNdpLifetime::from_u32_with_infinite(new_pl_secs);
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            ndp_pl,
            NonZeroNdpLifetime::from_u32_with_infinite(new_vl_secs),
        );
        let mut expected_timers = Vec::new();
        let valid_until = match NonZeroNdpLifetime::from_u32_with_infinite(effective_new_vl_secs)
            .expect("this test expects to keep the address")
        {
            NonZeroNdpLifetime::Finite(d) => {
                let valid_until = now + d.get();
                expected_timers.push((invalidate_timer_id, (), valid_until));
                Lifetime::Finite(valid_until)
            }
            NonZeroNdpLifetime::Infinite => Lifetime::Infinite,
        };
        match ndp_pl {
            None | Some(NonZeroNdpLifetime::Infinite) => {}
            Some(NonZeroNdpLifetime::Finite(d)) => {
                expected_timers.push((deprecate_timer_id, (), now + d.get()))
            }
        }
        let entry = SlaacAddressEntry {
            config: Ipv6AddrSlaacConfig {
                inner: SlaacConfig::Stable { valid_until },
                preferred_lifetime: PreferredLifetime::maybe_preferred_for(now, ndp_pl),
            },
            ..entry
        };
        assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [entry]);
        core_ctx.state.slaac_state.timers.assert_timers(expected_timers);
    }

    const SECRET_KEY: IidSecret = IidSecret::ALL_ONES;

    const ONE_HOUR: NonZeroDuration = NonZeroDuration::from_secs(ONE_HOUR_AS_SECS as u64).unwrap();

    struct DontGenerateTemporaryAddressTest {
        preferred_lifetime_config: NonZeroDuration,
        preferred_lifetime_secs: u32,
        valid_lifetime_secs: u32,
        temp_idgen_retries: u8,
        dad_transmits: u16,
        retrans_timer: Duration,
        enable: bool,
    }

    impl DontGenerateTemporaryAddressTest {
        fn with_pl_less_than_regen_advance(
            dad_transmits: u16,
            retrans_timer: Duration,
            temp_idgen_retries: u8,
        ) -> Self {
            DontGenerateTemporaryAddressTest {
                preferred_lifetime_config: ONE_HOUR,
                preferred_lifetime_secs: u32::try_from(
                    (SLAAC_MIN_REGEN_ADVANCE.get()
                        + (u32::from(temp_idgen_retries)
                            * u32::from(dad_transmits)
                            * retrans_timer))
                        .as_secs(),
                )
                .unwrap()
                    - 1,
                valid_lifetime_secs: TWO_HOURS_AS_SECS,
                temp_idgen_retries,
                dad_transmits,
                retrans_timer,
                enable: true,
            }
        }
    }

    #[test_case(DontGenerateTemporaryAddressTest {
        preferred_lifetime_config: ONE_HOUR,
        preferred_lifetime_secs: ONE_HOUR_AS_SECS,
        valid_lifetime_secs: TWO_HOURS_AS_SECS,
        temp_idgen_retries: 0,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        enable: false,
    }; "disabled")]
    #[test_case(DontGenerateTemporaryAddressTest{
        preferred_lifetime_config: ONE_HOUR,
        preferred_lifetime_secs: 0,
        valid_lifetime_secs: 0,
        temp_idgen_retries: 0,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        enable: true,
    }; "zero lifetimes")]
    #[test_case(DontGenerateTemporaryAddressTest {
        preferred_lifetime_config: ONE_HOUR,
        preferred_lifetime_secs: TWO_HOURS_AS_SECS,
        valid_lifetime_secs: ONE_HOUR_AS_SECS,
        temp_idgen_retries: 0,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        enable: true,
    }; "preferred larger than valid")]
    #[test_case(DontGenerateTemporaryAddressTest {
        preferred_lifetime_config: ONE_HOUR,
        preferred_lifetime_secs: 0,
        valid_lifetime_secs: TWO_HOURS_AS_SECS,
        temp_idgen_retries: 0,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        enable: true,
    }; "not preferred")]
    #[test_case(DontGenerateTemporaryAddressTest::with_pl_less_than_regen_advance(
        0 /* dad_transmits */,
        DEFAULT_RETRANS_TIMER /* retrans_timer */,
        0 /* temp_idgen_retries */,
    ); "preferred lifetime less than regen advance with no DAD transmits")]
    #[test_case(DontGenerateTemporaryAddressTest::with_pl_less_than_regen_advance(
        1 /* dad_transmits */,
        DEFAULT_RETRANS_TIMER /* retrans_timer */,
        0 /* temp_idgen_retries */,
    ); "preferred lifetime less than regen advance with DAD transmits")]
    #[test_case(DontGenerateTemporaryAddressTest::with_pl_less_than_regen_advance(
        1 /* dad_transmits */,
        DEFAULT_RETRANS_TIMER /* retrans_timer */,
        1 /* temp_idgen_retries */,
    ); "preferred lifetime less than regen advance with DAD transmits and retries")]
    #[test_case(DontGenerateTemporaryAddressTest::with_pl_less_than_regen_advance(
        2 /* dad_transmits */,
        DEFAULT_RETRANS_TIMER + Duration::from_secs(1) /* retrans_timer */,
        3 /* temp_idgen_retries */,
    ); "preferred lifetime less than regen advance with multiple DAD transmits and multiple retries")]
    #[test_case(DontGenerateTemporaryAddressTest {
        preferred_lifetime_config: SLAAC_MIN_REGEN_ADVANCE,
        preferred_lifetime_secs: ONE_HOUR_AS_SECS,
        valid_lifetime_secs: TWO_HOURS_AS_SECS,
        temp_idgen_retries: 1,
        dad_transmits: 1,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        enable: true,
    }; "configured preferred lifetime less than regen advance")]
    fn dont_generate_temporary_address(
        DontGenerateTemporaryAddressTest {
            preferred_lifetime_config,
            preferred_lifetime_secs,
            valid_lifetime_secs,
            temp_idgen_retries,
            dad_transmits,
            retrans_timer,
            enable,
        }: DontGenerateTemporaryAddressTest,
    ) {
        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration {
                temporary_address_configuration: if enable {
                    TemporarySlaacAddressConfiguration::Enabled {
                        temp_valid_lifetime: ONE_HOUR,
                        temp_preferred_lifetime: preferred_lifetime_config,
                        temp_idgen_retries,
                    }
                } else {
                    TemporarySlaacAddressConfiguration::Disabled
                },
                ..Default::default()
            },
            Default::default(),
            NonZeroU16::new(dad_transmits),
            retrans_timer,
        );

        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(preferred_lifetime_secs),
            NonZeroNdpLifetime::from_u32_with_infinite(valid_lifetime_secs),
        );
        assert_empty(core_ctx.state.iter_slaac_addrs());
        bindings_ctx.timers.assert_no_timers_installed();
    }

    struct GenerateTemporaryAddressTest {
        pl_config: u32,
        vl_config: u32,
        dad_transmits: u16,
        retrans_timer: Duration,
        temp_idgen_retries: u8,
        pl_ra: u32,
        vl_ra: u32,
        expected_pl_addr: u32,
        expected_vl_addr: u32,
    }
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: ONE_HOUR_AS_SECS,
        vl_config: ONE_HOUR_AS_SECS,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        temp_idgen_retries: 0,
        pl_ra: ONE_HOUR_AS_SECS,
        vl_ra: ONE_HOUR_AS_SECS,
        expected_pl_addr: ONE_HOUR_AS_SECS,
        expected_vl_addr: ONE_HOUR_AS_SECS,
    }; "config and prefix same lifetimes")]
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: ONE_HOUR_AS_SECS,
        vl_config: TWO_HOURS_AS_SECS,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        temp_idgen_retries: 0,
        pl_ra: THREE_HOURS_AS_SECS,
        vl_ra: THREE_HOURS_AS_SECS,
        expected_pl_addr: ONE_HOUR_AS_SECS,
        expected_vl_addr: TWO_HOURS_AS_SECS,
    }; "config smaller than prefix lifetimes")]
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: TWO_HOURS_AS_SECS,
        vl_config: THREE_HOURS_AS_SECS,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        temp_idgen_retries: 0,
        pl_ra: ONE_HOUR_AS_SECS,
        vl_ra: TWO_HOURS_AS_SECS,
        expected_pl_addr: ONE_HOUR_AS_SECS,
        expected_vl_addr: TWO_HOURS_AS_SECS,
    }; "config larger than prefix lifetimes")]
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: TWO_HOURS_AS_SECS,
        vl_config: THREE_HOURS_AS_SECS,
        dad_transmits: 0,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        temp_idgen_retries: 0,
        pl_ra: INFINITE_LIFETIME,
        vl_ra: INFINITE_LIFETIME,
        expected_pl_addr: TWO_HOURS_AS_SECS,
        expected_vl_addr: THREE_HOURS_AS_SECS,
    }; "prefix with infinite lifetimes")]
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: TWO_HOURS_AS_SECS,
        vl_config: THREE_HOURS_AS_SECS,
        dad_transmits: 1,
        retrans_timer: DEFAULT_RETRANS_TIMER,
        temp_idgen_retries: 0,
        pl_ra: INFINITE_LIFETIME,
        vl_ra: INFINITE_LIFETIME,
        expected_pl_addr: TWO_HOURS_AS_SECS,
        expected_vl_addr: THREE_HOURS_AS_SECS,
    }; "generate_with_dad_enabled")]
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: TWO_HOURS_AS_SECS,
        vl_config: THREE_HOURS_AS_SECS,
        dad_transmits: 2,
        retrans_timer: Duration::from_secs(5),
        temp_idgen_retries: 3,
        pl_ra: INFINITE_LIFETIME,
        vl_ra: INFINITE_LIFETIME,
        expected_pl_addr: TWO_HOURS_AS_SECS,
        expected_vl_addr: THREE_HOURS_AS_SECS,
    }; "generate_with_dad_enabled_and_retries")]
    #[test_case(GenerateTemporaryAddressTest{
        pl_config: TWO_HOURS_AS_SECS,
        vl_config: THREE_HOURS_AS_SECS,
        dad_transmits: 1,
        retrans_timer: Duration::from_secs(10),
        temp_idgen_retries: 0,
        pl_ra: INFINITE_LIFETIME,
        vl_ra: INFINITE_LIFETIME,
        expected_pl_addr: TWO_HOURS_AS_SECS,
        expected_vl_addr: THREE_HOURS_AS_SECS,
    }; "generate_with_dad_enabled_but_no_retries")]
    fn generate_temporary_address(
        GenerateTemporaryAddressTest {
            pl_config,
            vl_config,
            dad_transmits,
            retrans_timer,
            temp_idgen_retries,
            pl_ra,
            vl_ra,
            expected_pl_addr,
            expected_vl_addr,
        }: GenerateTemporaryAddressTest,
    ) {
        let pl_config = Duration::from_secs(pl_config.into());
        let regen_advance = regen_advance(temp_idgen_retries, retrans_timer, dad_transmits);

        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration {
                temporary_address_configuration: TemporarySlaacAddressConfiguration::Enabled {
                    temp_valid_lifetime: NonZeroDuration::new(Duration::from_secs(
                        vl_config.into(),
                    ))
                    .unwrap(),
                    temp_preferred_lifetime: NonZeroDuration::new(pl_config).unwrap(),
                    temp_idgen_retries,
                },
                ..Default::default()
            },
            Default::default(),
            NonZeroU16::new(dad_transmits),
            retrans_timer,
        );

        let mut dup_rng = bindings_ctx.rng().deep_clone();

        struct AddrProps {
            desync_factor: Duration,
            valid_until: FakeInstant,
            preferred_until: FakeInstant,
            entry: SlaacAddressEntry<FakeInstant>,
            deprecate_timer_id: InnerSlaacTimerId,
            invalidate_timer_id: InnerSlaacTimerId,
            regenerate_timer_id: InnerSlaacTimerId,
        }

        let addr_props = |rng: &mut FakeCryptoRng<_>,
                          creation_time,
                          config_greater_than_ra_desync_factor_offset| {
            let valid_until = creation_time + Duration::from_secs(expected_vl_addr.into());
            let addr_sub = generate_global_temporary_address(&SUBNET, &IID, rng.gen(), &SECRET_KEY);
            let desync_factor =
                desync_factor(rng, NonZeroDuration::new(pl_config).unwrap(), regen_advance)
                    .unwrap();
            let preferred_until = {
                let d = creation_time + Duration::from_secs(expected_pl_addr.into());
                if pl_config.as_secs() > pl_ra.into() {
                    d + config_greater_than_ra_desync_factor_offset
                } else {
                    d - desync_factor
                }
            };

            AddrProps {
                desync_factor,
                valid_until,
                preferred_until,
                entry: SlaacAddressEntry {
                    addr_sub,
                    config: Ipv6AddrSlaacConfig {
                        inner: SlaacConfig::Temporary(TemporarySlaacConfig {
                            valid_until,
                            desync_factor,
                            creation_time,
                            dad_counter: 0,
                        }),
                        preferred_lifetime: PreferredLifetime::preferred_until(preferred_until),
                    },
                },
                deprecate_timer_id: InnerSlaacTimerId::DeprecateSlaacAddress {
                    addr: addr_sub.addr(),
                },
                invalidate_timer_id: InnerSlaacTimerId::InvalidateSlaacAddress {
                    addr: addr_sub.addr(),
                },
                regenerate_timer_id: InnerSlaacTimerId::RegenerateTemporaryAddress {
                    addr_subnet: addr_sub,
                },
            }
        };

        // Generate the first temporary SLAAC address.
        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(pl_ra),
            NonZeroNdpLifetime::from_u32_with_infinite(vl_ra),
        );
        let AddrProps {
            desync_factor: first_desync_factor,
            valid_until: first_valid_until,
            preferred_until: first_preferred_until,
            entry: first_entry,
            deprecate_timer_id: first_deprecate_timer_id,
            invalidate_timer_id: first_invalidate_timer_id,
            regenerate_timer_id: first_regenerate_timer_id,
        } = addr_props(&mut dup_rng, bindings_ctx.now(), Duration::ZERO);
        assert_eq!(core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(), [first_entry]);
        core_ctx.state.slaac_state.timers.assert_timers([
            (first_deprecate_timer_id, (), first_preferred_until),
            (first_invalidate_timer_id, (), first_valid_until),
            (first_regenerate_timer_id, (), first_preferred_until - regen_advance.get()),
        ]);

        // Trigger the regenerate timer to generate the second temporary SLAAC
        // address.
        assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(new_timer_id()),);
        let AddrProps {
            desync_factor: second_desync_factor,
            valid_until: second_valid_until,
            preferred_until: second_preferred_until,
            entry: second_entry,
            deprecate_timer_id: second_deprecate_timer_id,
            invalidate_timer_id: second_invalidate_timer_id,
            regenerate_timer_id: second_regenerate_timer_id,
        } = addr_props(&mut dup_rng, bindings_ctx.now(), first_desync_factor);
        assert_eq!(
            core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(),
            [first_entry, second_entry]
        );
        let second_regen_at = second_preferred_until - regen_advance.get();
        core_ctx.state.slaac_state.timers.assert_timers([
            (first_deprecate_timer_id, (), first_preferred_until),
            (first_invalidate_timer_id, (), first_valid_until),
            (second_deprecate_timer_id, (), second_preferred_until),
            (second_invalidate_timer_id, (), second_valid_until),
            (second_regenerate_timer_id, (), second_regen_at),
        ]);

        // Deprecate first address.
        assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(new_timer_id()),);
        let first_entry = first_entry.to_deprecated();
        assert_eq!(
            core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(),
            [first_entry, second_entry]
        );
        core_ctx.state.slaac_state.timers.assert_timers([
            (first_invalidate_timer_id, (), first_valid_until),
            (second_deprecate_timer_id, (), second_preferred_until),
            (second_invalidate_timer_id, (), second_valid_until),
            (second_regenerate_timer_id, (), second_regen_at),
        ]);

        let third_created_at = {
            let expected_timer_order = if first_valid_until > second_regen_at {
                [second_regenerate_timer_id, second_deprecate_timer_id, first_invalidate_timer_id]
            } else {
                [first_invalidate_timer_id, second_regenerate_timer_id, second_deprecate_timer_id]
            };

            let mut third_created_at = None;
            for timer_id in expected_timer_order.iter() {
                let timer_id = *timer_id;

                core_ctx.state.slaac_state.timers.assert_top(&timer_id, &());
                assert_eq!(bindings_ctx.trigger_next_timer(&mut core_ctx), Some(new_timer_id()));

                if timer_id == second_regenerate_timer_id {
                    assert_eq!(third_created_at, None);
                    third_created_at = Some(bindings_ctx.now());
                }
            }

            third_created_at.unwrap()
        };

        // Make sure we regenerated the third address, deprecated the second and
        // invalidated the first.
        let AddrProps {
            desync_factor: _,
            valid_until: third_valid_until,
            preferred_until: third_preferred_until,
            entry: third_entry,
            deprecate_timer_id: third_deprecate_timer_id,
            invalidate_timer_id: third_invalidate_timer_id,
            regenerate_timer_id: third_regenerate_timer_id,
        } = addr_props(&mut dup_rng, third_created_at, first_desync_factor + second_desync_factor);
        let second_entry = second_entry.to_deprecated();
        assert_eq!(
            core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>(),
            [second_entry, third_entry]
        );
        core_ctx.state.slaac_state.timers.assert_timers([
            (second_invalidate_timer_id, (), second_valid_until),
            (third_deprecate_timer_id, (), third_preferred_until),
            (third_invalidate_timer_id, (), third_valid_until),
            (third_regenerate_timer_id, (), third_preferred_until - regen_advance.get()),
        ]);
    }

    #[test]
    fn temporary_address_not_updated_while_disabled() {
        let want_valid_until =
            FakeInstant::default() + Duration::from_secs(THREE_HOURS_AS_SECS.into());
        let CtxPair { mut core_ctx, mut bindings_ctx } = new_context(
            SlaacConfiguration {
                enable_stable_addresses: false,
                temporary_address_configuration: TemporarySlaacAddressConfiguration::Disabled,
            },
            FakeSlaacAddrs {
                slaac_addrs: vec![SlaacAddressEntry {
                    addr_sub: testutil::calculate_slaac_addr_sub(SUBNET, IID),
                    config: Ipv6AddrSlaacConfig {
                        inner: SlaacConfig::Temporary(TemporarySlaacConfig {
                            valid_until: want_valid_until,
                            desync_factor: Duration::default(),
                            creation_time: FakeInstant::default(),
                            dad_counter: 0,
                        }),
                        preferred_lifetime: PreferredLifetime::preferred_forever(),
                    },
                }],
                ..Default::default()
            },
            None, /* dad_transmits */
            DEFAULT_RETRANS_TIMER,
        );

        SlaacHandler::apply_slaac_update(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            SUBNET,
            NonZeroNdpLifetime::from_u32_with_infinite(FOUR_HOURS_AS_SECS),
            NonZeroNdpLifetime::from_u32_with_infinite(FOUR_HOURS_AS_SECS),
        );
        let addrs = core_ctx.state.iter_slaac_addrs().collect::<Vec<_>>();
        assert_eq!(addrs.len(), 1);
        let SlaacAddressEntry { config: Ipv6AddrSlaacConfig { inner, preferred_lifetime }, .. } =
            addrs[0];
        assert_matches!(inner,SlaacConfig::Temporary(TemporarySlaacConfig {
                valid_until,
                ..
            }) => {
            assert_eq!(valid_until, want_valid_until);
        });
        // Even though we don't remove the addresses immediately, they may
        // become deprecated. So the address is not completely removed as a
        // side-effect of disabling temporary addresses, but we'll steer it away
        // from being used more.
        assert_eq!(preferred_lifetime, PreferredLifetime::Deprecated);
    }
}
