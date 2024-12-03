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
        assert!(matches!($ctx.state.groups().get($group).unwrap().v1().inner.as_ref().unwrap(), $pattern))
    };
}

pub(crate) mod igmp;
pub(crate) mod mld;
#[cfg(test)]
mod testutil;
mod v1;
mod v2;

use core::fmt::Debug;
use core::time::Duration;

use net_types::ip::{Ip, IpAddress, IpVersionMarker};
use net_types::MulticastAddr;
use netstack3_base::ref_counted_hash_map::{InsertResult, RefCountedHashMap, RemoveResult};
use netstack3_base::{
    AnyDevice, CoreTimerContext, DeviceIdContext, InstantBindingsTypes, LocalTimerHeap, RngContext,
    TimerBindingsTypes, TimerContext, WeakDeviceIdentifier,
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

    fn leave_group(&mut self, group: MulticastAddr<A>) -> GroupLeaveResult<T> {
        self.inner.remove(group).into()
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
            match &state.gmp.mode {
                GmpMode::V1 { compat: _ } => {
                    v1::handle_enabled(&mut core_ctx, bindings_ctx, device, state);
                }
                GmpMode::V2 => {
                    todo!("https://fxbug.dev/42071006 handle GMPv2 enabled")
                }
            }
        })
    }

    fn gmp_handle_disabled(&mut self, bindings_ctx: &mut BC, device: &Self::DeviceId) {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, mut state| match state.gmp.mode {
            GmpMode::V1 { compat } => {
                v1::handle_disabled(&mut core_ctx, bindings_ctx, device, state.as_mut());
                if compat {
                    enter_mode(&mut core_ctx, bindings_ctx, device, state, GmpMode::V2);
                }
            }
            GmpMode::V2 => {
                todo!("https://fxbug.dev/42071006 handle GMPv2 disabled")
            }
        })
    }

    fn gmp_join_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupJoinResult {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| match &state.gmp.mode {
            GmpMode::V1 { compat: _ } => {
                v1::join_group(&mut core_ctx, bindings_ctx, device, group_addr, state)
            }
            GmpMode::V2 => {
                todo!("https://fxbug.dev/42071006 handle GMPv2 join group")
            }
        })
    }

    fn gmp_leave_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupLeaveResult {
        self.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| match &state.gmp.mode {
            GmpMode::V1 { compat: _ } => {
                v1::leave_group(&mut core_ctx, bindings_ctx, device, group_addr, state)
            }
            GmpMode::V2 => {
                todo!("https://fxbug.dev/42071006 handle GMPv2 leave group")
            }
        })
    }
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
pub struct GmpTimerId<I: Ip, D: WeakDeviceIdentifier> {
    pub(crate) device: D,
    pub(crate) _marker: IpVersionMarker<I>,
}

impl<I: Ip, D: WeakDeviceIdentifier> GmpTimerId<I, D> {
    fn device_id(&self) -> &D {
        let Self { device, _marker: IpVersionMarker { .. } } = self;
        device
    }
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

/// The timer id kept in [`GmpState`]'s local timer heap.
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
enum TimerIdInner<I: Ip> {
    V1(v1::DelayedReportTimerId<I>),
}

impl<I: Ip> From<v1::DelayedReportTimerId<I>> for TimerIdInner<I> {
    fn from(value: v1::DelayedReportTimerId<I>) -> Self {
        Self::V1(value)
    }
}

#[cfg_attr(test, derive(Debug))]
pub struct GmpState<I: Ip, BT: GmpBindingsTypes> {
    timers: LocalTimerHeap<TimerIdInner<I>, (), BT>,
    mode: GmpMode,
}

// NB: This block is not bound on GmpBindingsContext because we don't need
// RngContext to construct GmpState.
impl<I: Ip, BC: GmpBindingsTypes + TimerContext> GmpState<I, BC> {
    /// Constructs a new `GmpState` for `device`.
    pub fn new<D: WeakDeviceIdentifier, CC: CoreTimerContext<GmpTimerId<I, D>, BC>>(
        bindings_ctx: &mut BC,
        device: D,
    ) -> Self {
        Self {
            timers: LocalTimerHeap::new_with_context::<_, CC>(
                bindings_ctx,
                GmpTimerId { device, _marker: Default::default() },
            ),
            mode: Default::default(),
        }
    }
}

/// A reference to a device's GMP state.
pub struct GmpStateRef<'a, I: IpExt, CC: GmpTypeLayout<I, BT>, BT: GmpBindingsTypes> {
    /// True if GMP is enabled for the device.
    pub enabled: bool,
    /// Mutable reference to the multicast groups on a device.
    pub groups: &'a mut MulticastGroupSet<I::Addr, GmpGroupState<BT>>,
    /// Mutable reference to the device's GMP state.
    pub gmp: &'a mut GmpState<I, BT>,
    /// Protocol specific configuration.
    pub config: &'a CC::Config,
}

impl<'a, I: IpExt, CC: GmpTypeLayout<I, BT>, BT: GmpBindingsTypes> GmpStateRef<'a, I, CC, BT> {
    fn as_mut(&mut self) -> GmpStateRef<'_, I, CC, BT> {
        let Self { enabled, groups, gmp, config } = self;
        GmpStateRef { enabled: *enabled, groups, gmp, config }
    }
}

/// Provides IP-specific associated types for GMP.
pub trait GmpTypeLayout<I: IpExt, BT: GmpBindingsTypes>: DeviceIdContext<AnyDevice> {
    /// The type for protocol-specific actions.
    type Actions;
    /// The type for protocol-specific configs.
    type Config: Debug + v1::ProtocolConfig<QuerySpecificActions = Self::Actions>;
}

/// The state kept by each muitlcast group the host is a member of.
pub struct GmpGroupState<BT: GmpBindingsTypes> {
    version_specific: GmpGroupStateByVersion<BT>,
    // TODO(https://fxbug.dev/381241191): When we support SSM, each group should
    // keep track of the source interest and filter modes.
}

impl<BT: GmpBindingsTypes> GmpGroupState<BT> {
    /// Retrieves a mutable borrow to the v1 state machine value.
    ///
    /// # Panics
    ///
    /// Panics if the state machine is not in the v1 state. When switching
    /// modes, GMP is responsible for updating all group states to the
    /// appropriate version.
    fn v1_mut(&mut self) -> &mut v1::GmpStateMachine<BT::Instant> {
        match &mut self.version_specific {
            GmpGroupStateByVersion::V1(v1) => return v1,
            GmpGroupStateByVersion::V2(_) => {
                panic!("expected GMP v1")
            }
        }
    }

    /// Like [`GmpGroupState::v1_mut`] but returns a non mutable borrow.
    #[cfg(test)]
    fn v1(&self) -> &v1::GmpStateMachine<BT::Instant> {
        match &self.version_specific {
            GmpGroupStateByVersion::V1(v1) => v1,
            GmpGroupStateByVersion::V2(_) => panic!("group not in v1 mode"),
        }
    }

    /// Equivalent to [`GmpGroupState::v1_mut`] but drops all remaining state.
    ///
    /// # Panics
    ///
    /// See [`GmpGroupState::v1`].
    fn into_v1(self) -> v1::GmpStateMachine<BT::Instant> {
        let Self { version_specific } = self;
        match version_specific {
            GmpGroupStateByVersion::V1(v1) => v1,
            GmpGroupStateByVersion::V2(_) => panic!("expected GMP v1"),
        }
    }

    /// Creates a new `GmpGroupState` with associated v1 state machine.
    fn new_v1(v1: v1::GmpStateMachine<BT::Instant>) -> Self {
        Self { version_specific: GmpGroupStateByVersion::V1(v1) }
    }
}

/// GMP Compatibility mode.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum GmpMode {
    /// GMP operating in v1 mode. This is the mode supporting MLDv1 and IGMPv2.
    V1 {
        /// Compat indicates v1 mode behavior.
        ///
        /// It is `true` if v1 is entered due to receiving a query from a v1
        /// router. It is `false` if v1 is entered due to administrative action
        /// (i.e. "forcing" v1 mode).
        ///
        /// It effectively controls whether we can revert back to v2 on
        /// interface disable <-> re-enable.
        compat: bool,
    },
    /// GMP operating in v2 mode. This is the mode supporting MLDv2 and IGMPv3.
    V2,
}

impl GmpMode {
    fn is_v1(&self) -> bool {
        match self {
            Self::V1 { .. } => true,
            Self::V2 => false,
        }
    }
}

impl Default for GmpMode {
    fn default() -> Self {
        // TODO(https://fxbug.dev/42071006): Default to V2 once ready.
        Self::V1 { compat: false }
    }
}

#[cfg_attr(test, derive(derivative::Derivative))]
#[cfg_attr(test, derivative(Debug(bound = "")))]
enum GmpGroupStateByVersion<BT: GmpBindingsTypes> {
    V1(v1::GmpStateMachine<BT::Instant>),
    V2(v2::GroupState),
}

/// Provides immutable access to GMP state.
trait GmpStateContext<I: IpExt, BT: GmpBindingsTypes>: GmpTypeLayout<I, BT> {
    /// Calls the function with immutable access to the [`MulticastGroupSet`].
    fn with_gmp_state<O, F: FnOnce(&MulticastGroupSet<I::Addr, GmpGroupState<BT>>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// Provides common functionality for GMP context implementations.
///
/// This trait implements portions of a group management protocol.
trait GmpContext<I: IpExt, BC: GmpBindingsContext>: GmpTypeLayout<I, BC> + Sized {
    /// The inner context given to `with_gmp_state_mut_and_ctx`.
    type Inner<'a>: GmpContextInner<
            I,
            BC,
            Config = Self::Config,
            Actions = Self::Actions,
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
}

/// The inner GMP context.
///
/// Provides access to external actions while holding the GMP state lock.
trait GmpContextInner<I: IpExt, BC: GmpBindingsContext>: GmpTypeLayout<I, BC> {
    /// Sends a GMP message.
    fn send_message_v1(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
        msg_type: v1::GmpMessageType,
    );

    /// Runs protocol-specific actions.
    fn run_actions(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        actions: Self::Actions,
    );

    /// Called whenever the GMP mode changes.
    ///
    /// `new_mode` is the new mode GMP is operating on.
    ///
    /// The caller ensures that the generic GMP state is updated before hand
    /// (all GMP timers are cleared, GMP state knows about `new_mode`).
    /// Implementers are expected to only take protocol-specific actions here.
    /// Notably, IGMP should reset its own IGMPv1 compatibility mode knowledge.
    fn handle_mode_change(
        &mut self,
        bindings_ctx: &mut BC,
        device: &Self::DeviceId,
        new_mode: GmpMode,
    );
}

fn handle_timer<I, BC, CC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    timer: GmpTimerId<I, CC::WeakDeviceId>,
) where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    let GmpTimerId { device, _marker: IpVersionMarker { .. } } = timer;
    let Some(device) = device.upgrade() else {
        return;
    };
    core_ctx.with_gmp_state_mut_and_ctx(&device, |mut core_ctx, state| {
        let Some((timer_id, ())) = state.gmp.timers.pop(bindings_ctx) else {
            return;
        };
        match (timer_id, &state.gmp.mode) {
            (TimerIdInner::V1(v1), GmpMode::V1 { .. }) => {
                v1::handle_timer(&mut core_ctx, bindings_ctx, &device, state, v1);
            }
            bad @ (TimerIdInner::V1(_), GmpMode::V2) => {
                panic!("incompatible timer fired {bad:?}")
            }
        }
    });
}

/// Enters `mode` in the state referenced by `state`.
///
/// Mode changes cause all timers to be canceled, and the group state to be
/// updated to the appropriate GMP version.
///
/// No-op if `new_mode` is current.
fn enter_mode<
    I: IpExt,
    CC: GmpContextInner<I, BC>,
    BC: GmpBindingsContext,
    T: GmpTypeLayout<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    state: GmpStateRef<'_, I, T, BC>,
    new_mode: GmpMode,
) {
    match (&mut state.gmp.mode, &new_mode) {
        (GmpMode::V1 { compat }, GmpMode::V1 { compat: new_compat }) => {
            // While in v1 mode, we only allow exiting compat mode, not entering
            // it again. This allows the logic handling v1 queries to always
            // attempt a v1 compat mode enter, but this change will only be
            // applied if we are currently in v2.
            if !*new_compat {
                *compat = *new_compat;
            }
            return;
        }
        (GmpMode::V2, GmpMode::V2) => {
            // Same mode.
            return;
        }
        (GmpMode::V1 { compat: _ }, GmpMode::V2) => {
            // Transition to v2.
            //
            // Update the group state in each group to a default value which
            // will not trigger any unsolicited reports and is ready to respond
            // to incoming queries.
            for (_, GmpGroupState { version_specific }) in state.groups.iter_mut() {
                *version_specific =
                    GmpGroupStateByVersion::V2(v2::GroupState::new_for_mode_transition())
            }
        }
        (GmpMode::V2, GmpMode::V1 { compat: _ }) => {
            // Transition to v1.
            //
            // Update the state machine in each group to the appropriate idle
            // definition in GMPv1. This is a state with no timers that just
            // waits to respond to queries.
            for (_, GmpGroupState { version_specific }) in state.groups.iter_mut() {
                *version_specific =
                    GmpGroupStateByVersion::V1(v1::GmpStateMachine::new_for_mode_transition())
            }
        }
    };
    state.gmp.timers.clear(bindings_ctx);
    state.gmp.mode = new_mode;
    core_ctx.handle_mode_change(bindings_ctx, device, new_mode);
}

/// Error returned when operating queries but the host is not a member.
#[cfg_attr(test, derive(Debug, Eq, PartialEq))]
struct NotAMemberErr<I: Ip>(I::Addr);

#[cfg(test)]
mod tests {
    use alloc::vec;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use netstack3_base::testutil::FakeDeviceId;

    use testutil::{FakeCtx, FakeGmpContextInner, TestIpExt};

    use super::*;

    #[ip_test(I)]
    fn mode_change_state_clearing<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V1 { compat: false });

        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR1),
            GroupJoinResult::Joined(())
        );
        // Drop the group join message so we can assert no more messages are
        // sent after this.
        core_ctx.inner.v1_messages.clear();

        // We should now have timers installed and v1 state in groups.
        assert!(core_ctx.gmp.timers.iter().next().is_some());
        assert_matches!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().version_specific,
            GmpGroupStateByVersion::V1(_)
        );

        core_ctx.with_gmp_state_mut_and_ctx(&FakeDeviceId, |mut core_ctx, mut state| {
            enter_mode(
                &mut core_ctx,
                &mut bindings_ctx,
                &FakeDeviceId,
                state.as_mut(),
                GmpMode::V2,
            );
            assert_eq!(state.gmp.mode, GmpMode::V2);
        });
        // Timers were removed and state is now v2.
        core_ctx.gmp.timers.assert_timers([]);
        assert_matches!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().version_specific,
            GmpGroupStateByVersion::V2(_)
        );

        // Moving back moves the state back to v1.
        core_ctx.with_gmp_state_mut_and_ctx(&FakeDeviceId, |mut core_ctx, mut state| {
            enter_mode(
                &mut core_ctx,
                &mut bindings_ctx,
                &FakeDeviceId,
                state.as_mut(),
                GmpMode::V1 { compat: false },
            );
            assert_eq!(state.gmp.mode, GmpMode::V1 { compat: false });
        });
        assert_matches!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().version_specific,
            GmpGroupStateByVersion::V1(_)
        );

        // Throughout we should've generated no traffic.
        let FakeGmpContextInner { v1_messages } = &core_ctx.inner;
        assert_eq!(v1_messages, &vec![]);
    }

    #[ip_test(I)]
    fn cant_enter_v1_compat<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V1 { compat: false });
        core_ctx.with_gmp_state_mut_and_ctx(&FakeDeviceId, |mut core_ctx, mut state| {
            enter_mode(
                &mut core_ctx,
                &mut bindings_ctx,
                &FakeDeviceId,
                state.as_mut(),
                GmpMode::V1 { compat: true },
            );
            // Mode doesn't change because we can't go from non compat to
            // compat.
            assert_eq!(state.gmp.mode, GmpMode::V1 { compat: false });
        });
        // The opposite, however, is allowed.
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V1 { compat: true });
        core_ctx.with_gmp_state_mut_and_ctx(&FakeDeviceId, |mut core_ctx, mut state| {
            enter_mode(
                &mut core_ctx,
                &mut bindings_ctx,
                &FakeDeviceId,
                state.as_mut(),
                GmpMode::V1 { compat: false },
            );
            assert_eq!(state.gmp.mode, GmpMode::V1 { compat: false });
        });
    }

    #[ip_test(I)]
    fn disable_exits_compat<I: TestIpExt>() {
        // Disabling in compat mode returns to v2.
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V1 { compat: true });
        core_ctx.gmp_handle_disabled(&mut bindings_ctx, &FakeDeviceId);
        assert_eq!(core_ctx.gmp.mode, GmpMode::V2);

        // Same is not true for not compat v1.
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V1 { compat: false });
        core_ctx.gmp_handle_disabled(&mut bindings_ctx, &FakeDeviceId);
        assert_eq!(core_ctx.gmp.mode, GmpMode::V1 { compat: false });
    }
}
