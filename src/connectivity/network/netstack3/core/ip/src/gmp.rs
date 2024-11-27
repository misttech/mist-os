// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Group Management Protocols (GMPs).
//!
//! This module provides implementations of the Internet Group Management Protocol
//! (IGMP) and the Multicast Listener Discovery (MLD) protocol. These allow
//! hosts to join IPv4 and IPv6 multicast groups respectively.
//!
//! The term "Group Management Protocol" is defined in [RFC 4606]:
//!
//! > Due to the commonality of function, the term "Group Management Protocol",
//! > or "GMP", will be used to refer to both IGMP and MLD.
//!
//! [RFC 4606]: https://tools.ietf.org/html/rfc4604

// This macro is used by tests in both the `igmp` and `mld` modules.

/// Assert that the GMP state machine for `$group` is in the given state.
///
/// `$ctx` is a `context::testutil::FakeCtx` whose state contains a `groups:
/// MulticastGroupSet` field.
#[cfg(test)]
macro_rules! assert_gmp_state {
    ($ctx:expr, $group:expr, NonMember) => {
        assert_gmp_state!(@inner $ctx, $group, crate::internal::gmp::v1::MemberState::NonMember(_));
    };
    ($ctx:expr, $group:expr, Delaying) => {
        assert_gmp_state!(@inner $ctx, $group, crate::internal::gmp::v1::MemberState::Delaying(_));
    };
    (@inner $ctx:expr, $group:expr, $pattern:pat) => {
        assert!(matches!($ctx.state.groups().get($group).unwrap().0.inner.as_ref().unwrap(), $pattern))
    };
}

pub(crate) mod igmp;
pub(crate) mod mld;
mod v1;

use core::fmt::Debug;
use core::time::Duration;

use net_types::ip::{Ip, IpAddress, IpVersionMarker};
use net_types::MulticastAddr;
use netstack3_base::ref_counted_hash_map::{InsertResult, RefCountedHashMap, RemoveResult};
use netstack3_base::{
    AnyDevice, CoreTimerContext, DeviceIdContext, Instant, InstantBindingsTypes, LocalTimerHeap,
    RngContext, TimerBindingsTypes, TimerContext, WeakDeviceIdentifier,
};
use rand::Rng;

/// The result of joining a multicast group.
///
/// `GroupJoinResult` is the result of joining a multicast group in a
/// [`MulticastGroupSet`].
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub enum GroupJoinResult<O = ()> {
    /// We were not previously a member of the group, so we joined the
    /// group.
    Joined(O),
    /// We were already a member of the group, so we incremented the group's
    /// reference count.
    AlreadyMember,
}

impl<O> GroupJoinResult<O> {
    /// Maps a [`GroupJoinResult::Joined`] variant to another type.
    ///
    /// If `self` is [`GroupJoinResult::AlreadyMember`], it is left as-is.
    pub(crate) fn map<P, F: FnOnce(O) -> P>(self, f: F) -> GroupJoinResult<P> {
        match self {
            GroupJoinResult::Joined(output) => GroupJoinResult::Joined(f(output)),
            GroupJoinResult::AlreadyMember => GroupJoinResult::AlreadyMember,
        }
    }
}

impl<O> From<InsertResult<O>> for GroupJoinResult<O> {
    fn from(result: InsertResult<O>) -> Self {
        match result {
            InsertResult::Inserted(output) => GroupJoinResult::Joined(output),
            InsertResult::AlreadyPresent => GroupJoinResult::AlreadyMember,
        }
    }
}

/// The result of leaving a multicast group.
///
/// `GroupLeaveResult` is the result of leaving a multicast group in
/// [`MulticastGroupSet`].
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
pub enum GroupLeaveResult<T = ()> {
    /// The reference count reached 0, so we left the group.
    Left(T),
    /// The reference count did not reach 0, so we are still a member of the
    /// group.
    StillMember,
    /// We were not a member of the group.
    NotMember,
}

impl<T> GroupLeaveResult<T> {
    /// Maps a [`GroupLeaveResult::Left`] variant to another type.
    ///
    /// If `self` is [`GroupLeaveResult::StillMember`] or
    /// [`GroupLeaveResult::NotMember`], it is left as-is.
    pub(crate) fn map<U, F: FnOnce(T) -> U>(self, f: F) -> GroupLeaveResult<U> {
        match self {
            GroupLeaveResult::Left(value) => GroupLeaveResult::Left(f(value)),
            GroupLeaveResult::StillMember => GroupLeaveResult::StillMember,
            GroupLeaveResult::NotMember => GroupLeaveResult::NotMember,
        }
    }
}

impl<T> From<RemoveResult<T>> for GroupLeaveResult<T> {
    fn from(result: RemoveResult<T>) -> Self {
        match result {
            RemoveResult::Removed(value) => GroupLeaveResult::Left(value),
            RemoveResult::StillPresent => GroupLeaveResult::StillMember,
            RemoveResult::NotPresent => GroupLeaveResult::NotMember,
        }
    }
}

/// A set of reference-counted multicast groups and associated data.
///
/// `MulticastGroupSet` is a set of multicast groups, each with associated data
/// `T`. Each group is reference-counted, only being removed once its reference
/// count reaches zero.
#[cfg_attr(test, derive(Debug))]
pub struct MulticastGroupSet<A: IpAddress, T> {
    inner: RefCountedHashMap<MulticastAddr<A>, T>,
}

impl<A: IpAddress, T> Default for MulticastGroupSet<A, T> {
    fn default() -> MulticastGroupSet<A, T> {
        MulticastGroupSet { inner: RefCountedHashMap::default() }
    }
}

impl<A: IpAddress, T> MulticastGroupSet<A, T> {
    fn groups_mut(&mut self) -> impl Iterator<Item = (&MulticastAddr<A>, &mut T)> + '_ {
        self.inner.iter_mut()
    }

    fn join_group_with<O, F: FnOnce() -> (T, O)>(
        &mut self,
        group: MulticastAddr<A>,
        f: F,
    ) -> GroupJoinResult<O> {
        self.inner.insert_with(group, f).into()
    }

    /// Joins a multicast group and initializes it with a GMP state machine.
    ///
    /// `join_group_gmp` joins the multicast group `group`. If the group was not
    /// already joined, then a new instance of [`GmpStateMachine`] is generated
    /// using [`GmpStateMachine::join_group`], it is inserted with a reference
    /// count of 1, and the list of actions returned by `join_group` is
    /// returned. Otherwise, if the group was already joined, its reference
    /// count is incremented.
    fn join_group_gmp<I: Instant, P: ProtocolSpecific + Default, R: Rng>(
        &mut self,
        gmp_disabled: bool,
        group: MulticastAddr<A>,
        rng: &mut R,
        now: I,
    ) -> GroupJoinResult<v1::JoinGroupActions<P>>
    where
        T: From<v1::GmpStateMachine<I, P>>,
        P::Config: Default,
    {
        self.join_group_with(group, || {
            let (state, actions) = v1::GmpStateMachine::join_group(rng, now, gmp_disabled);
            (T::from(state), actions)
        })
    }

    fn leave_group(&mut self, group: MulticastAddr<A>) -> GroupLeaveResult<T> {
        self.inner.remove(group).into()
    }

    /// Leaves a multicast group.
    ///
    /// `leave_group_gmp` leaves the multicast group `group` by decrementing the
    /// reference count on the group. If the reference count reaches 0, the
    /// group is left using [`GmpStateMachine::leave_group`] and the list of
    /// actions returned by `leave_group` is returned.
    fn leave_group_gmp<I: Instant, P: ProtocolSpecific>(
        &mut self,
        group: MulticastAddr<A>,
    ) -> GroupLeaveResult<v1::LeaveGroupActions>
    where
        T: Into<v1::GmpStateMachine<I, P>>,
    {
        self.leave_group(group).map(|state| state.into().leave_group())
    }

    /// Does the set contain the given group?
    pub(crate) fn contains(&self, group: &MulticastAddr<A>) -> bool {
        self.inner.contains_key(group)
    }

    #[cfg(test)]
    fn get(&self, group: &MulticastAddr<A>) -> Option<&T> {
        self.inner.get(group)
    }

    fn get_mut(&mut self, group: &MulticastAddr<A>) -> Option<&mut T> {
        self.inner.get_mut(group)
    }

    fn iter_mut<'a>(&'a mut self) -> impl 'a + Iterator<Item = (&'a MulticastAddr<A>, &'a mut T)> {
        self.inner.iter_mut()
    }
}

/// An implementation of query operations on a Group Management Protocol (GMP).
pub trait GmpQueryHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
    /// Returns true if the device is a member of the group.
    fn gmp_is_in_group(
        &mut self,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> bool;
}

/// An implementation of a Group Management Protocol (GMP) such as the Internet
/// Group Management Protocol, Version 2 (IGMPv2) for IPv4 or the Multicast
/// Listener Discovery (MLD) protocol for IPv6.
pub trait GmpHandler<I: Ip, BC>: DeviceIdContext<AnyDevice> {
    /// Handles GMP potentially being enabled.
    ///
    /// Attempts to transition memberships in the non-member state to a member
    /// state. Should be called anytime a configuration change occurs which
    /// results in GMP potentially being enabled. E.g. when IP or GMP
    /// transitions to being enabled.
    fn gmp_handle_maybe_enabled(&mut self, bindings_ctx: &mut BC, device: &Self::DeviceId);

    /// Handles GMP being disabled.
    ///
    /// All joined groups will transition to the non-member state but still
    /// remain locally joined.
    fn gmp_handle_disabled(&mut self, bindings_ctx: &mut BC, device: &Self::DeviceId);

    /// Joins the given multicast group.
    fn gmp_join_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupJoinResult;

    /// Leaves the given multicast group.
    fn gmp_leave_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupLeaveResult;
}

impl<I: IpExt, BT: GmpBindingsTypes, CC: GmpStateContext<I, BT>> GmpQueryHandler<I, BT> for CC {
    fn gmp_is_in_group(
        &mut self,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> bool {
        self.with_gmp_state(device, |groups| groups.contains(&group_addr))
    }
}

impl<I: IpExt, BC: GmpBindingsContext, CC: GmpContext<I, BC>> GmpHandler<I, BC> for CC {
    fn gmp_handle_maybe_enabled(&mut self, bindings_ctx: &mut BC, device: &Self::DeviceId) {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| {
            if !state.enabled {
                return;
            }
            v1::handle_enabled(&mut core_ctx, bindings_ctx, device, state);
        })
    }

    fn gmp_handle_disabled(&mut self, bindings_ctx: &mut BC, device: &Self::DeviceId) {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| {
            v1::handle_disabled(&mut core_ctx, bindings_ctx, device, state)
        })
    }

    fn gmp_join_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupJoinResult {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| {
            v1::join_group(&mut core_ctx, bindings_ctx, device, group_addr, state)
        })
    }

    fn gmp_leave_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupLeaveResult {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| {
            v1::leave_group(&mut core_ctx, bindings_ctx, device, group_addr, state)
        })
    }
}

/// This trait is used to model the different parts of the two protocols.
///
/// Though MLD and IGMPv2 share the most part of their state machines there are
/// some subtle differences between each other.
pub trait ProtocolSpecific: v1::ProtocolSpecific {}

impl<P> ProtocolSpecific for P where P: v1::ProtocolSpecific {}

pub trait ProtocolSpecificTypes: Copy + Default {
    /// The type for protocol-specific actions.
    type Actions;
    /// The type for protocol-specific configs.
    type Config: Debug + Default;
}

/// Randomly generates a timeout in (0, period].
///
/// # Panics
///
/// `random_report_timeout` may panic if `period.as_micros()` overflows `u64`.
fn random_report_timeout<R: Rng>(rng: &mut R, period: Duration) -> Duration {
    let micros = rng.gen_range(0..u64::try_from(period.as_micros()).unwrap()) + 1;
    // u64 will be enough here because the only input of the function is from
    // the `MaxRespTime` field of the GMP query packets. The representable
    // number of microseconds is bounded by 2^33.
    Duration::from_micros(micros)
}

/// A timer ID for GMP to send a report.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub struct GmpDelayedReportTimerId<I: Ip, D: WeakDeviceIdentifier> {
    pub(crate) device: D,
    pub(crate) _marker: IpVersionMarker<I>,
}

impl<I: Ip, D: WeakDeviceIdentifier> GmpDelayedReportTimerId<I, D> {
    fn device_id(&self) -> &D {
        let Self { device, _marker: IpVersionMarker { .. } } = self;
        device
    }
}

/// A type of GMP message.
#[derive(Debug)]
enum GmpMessageType<P> {
    Report(P),
    Leave,
}

/// The bindings types for GMP.
pub trait GmpBindingsTypes: InstantBindingsTypes + TimerBindingsTypes {}
impl<BT> GmpBindingsTypes for BT where BT: InstantBindingsTypes + TimerBindingsTypes {}

/// The bindings execution context for GMP.
pub trait GmpBindingsContext: RngContext + TimerContext + GmpBindingsTypes {}
impl<BC> GmpBindingsContext for BC where BC: RngContext + TimerContext + GmpBindingsTypes {}

/// An extension trait to [`Ip`].
pub trait IpExt: Ip {
    /// Returns true iff GMP should be performed for the multicast group.
    fn should_perform_gmp(addr: MulticastAddr<Self::Addr>) -> bool;
}

#[cfg_attr(test, derive(Debug))]
pub struct GmpState<I: Ip, BT: GmpBindingsTypes> {
    timers: LocalTimerHeap<MulticastAddr<I::Addr>, (), BT>,
}

// NB: This block is not bound on GmpBindingsContext because we don't need
// RngContext to construct GmpState.
impl<I: Ip, BC: GmpBindingsTypes + TimerContext> GmpState<I, BC> {
    /// Constructs a new `GmpState` for `device`.
    pub fn new<D: WeakDeviceIdentifier, CC: CoreTimerContext<GmpDelayedReportTimerId<I, D>, BC>>(
        bindings_ctx: &mut BC,
        device: D,
    ) -> Self {
        Self {
            timers: LocalTimerHeap::new_with_context::<_, CC>(
                bindings_ctx,
                GmpDelayedReportTimerId { device, _marker: Default::default() },
            ),
        }
    }
}

/// A reference to a device's GMP state.
pub struct GmpStateRef<'a, I: IpExt, CC: GmpTypeLayout<I, BT>, BT: GmpBindingsTypes> {
    /// True if GMP is enabled for the device.
    pub enabled: bool,
    /// Mutable reference to the multicast groups on a device.
    pub groups: &'a mut MulticastGroupSet<I::Addr, CC::GroupState>,
    /// Mutable reference to the device's GMP state.
    pub gmp: &'a mut GmpState<I, BT>,
}

/// Provides IP-specific associated types for GMP.
pub trait GmpTypeLayout<I: IpExt, BT: GmpBindingsTypes>: DeviceIdContext<AnyDevice> {
    type ProtocolSpecific: ProtocolSpecific;
    type GroupState: From<v1::GmpStateMachine<BT::Instant, Self::ProtocolSpecific>>
        + Into<v1::GmpStateMachine<BT::Instant, Self::ProtocolSpecific>>
        + AsMut<v1::GmpStateMachine<BT::Instant, Self::ProtocolSpecific>>;
}

/// Provides immutable access to GMP state.
trait GmpStateContext<I: IpExt, BT: GmpBindingsTypes>: GmpTypeLayout<I, BT> {
    /// Calls the function with immutable access to the [`MulticastGroupSet`].
    fn with_gmp_state<O, F: FnOnce(&MulticastGroupSet<I::Addr, Self::GroupState>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// Provides common functionality for GMP context implementations.
///
/// This trait implements portions of a group management protocol.
trait GmpContext<I: IpExt, BC: GmpBindingsContext>: GmpTypeLayout<I, BC> + Sized {
    /// The context-specific error type.
    type Err;
    /// The inner context given to `with_gmp_state_mut_and_ctx`.
    type Inner<'a>: GmpContextInner<
            I,
            BC,
            ProtocolSpecific = Self::ProtocolSpecific,
            GroupState = Self::GroupState,
            DeviceId = Self::DeviceId,
        > + 'a;

    /// Calls the function with mutable access to GMP state in [`GmpStateRef`]
    /// and access to a [`GmpContextInner`] context.
    fn with_gmp_state_mut_and_ctx<
        O,
        F: FnOnce(Self::Inner<'_>, GmpStateRef<'_, I, Self, BC>) -> O,
    >(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Calls the function with mutable access to GMP state in [`GmpStateRef`].
    fn with_gmp_state_mut<O, F: FnOnce(GmpStateRef<'_, I, Self, BC>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O {
        self.with_gmp_state_mut_and_ctx(device, |_core_ctx, state| cb(state))
    }

    /// Returns the context-specific error for not a member.
    fn not_a_member_err(addr: I::Addr) -> Self::Err;
}

/// The inner GMP context.
///
/// Provides access to external actions while holding the GMP state lock.
trait GmpContextInner<I: IpExt, BC: GmpBindingsContext>: GmpTypeLayout<I, BC> {
    /// Sends a GMP message.
    fn send_message(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
        msg_type: GmpMessageType<Self::ProtocolSpecific>,
    );

    /// Runs protocol-specific actions.
    fn run_actions(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        actions: <Self::ProtocolSpecific as ProtocolSpecificTypes>::Actions,
    );
}

trait GmpMessage<I: Ip> {
    fn group_addr(&self) -> I::Addr;
}
