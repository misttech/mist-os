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
        assert_gmp_state!(@inner $ctx, $group, MemberState::NonMember(_));
    };
    ($ctx:expr, $group:expr, Delaying) => {
        assert_gmp_state!(@inner $ctx, $group, MemberState::Delaying(_));
    };
    (@inner $ctx:expr, $group:expr, $pattern:pat) => {
        assert!(matches!($ctx.state.groups.get($group).unwrap().0.inner.as_ref().unwrap(), $pattern))
    };
}

pub(crate) mod igmp;
pub(crate) mod mld;

use alloc::vec::Vec;
use core::fmt::Debug;
use core::time::Duration;

use assert_matches::assert_matches;
use net_types::ip::{Ip, IpAddress, IpVersionMarker};
use net_types::MulticastAddr;
use netstack3_base::ref_counted_hash_map::{InsertResult, RefCountedHashMap, RemoveResult};
use netstack3_base::{
    AnyDevice, CoreTimerContext, DeviceIdContext, Instant, InstantBindingsTypes, LocalTimerHeap,
    RngContext, TimerBindingsTypes, TimerContext, WeakDeviceIdentifier,
};
use packet_formats::utils::NonZeroDuration;
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

/// Actions to take as a consequence of joining a group.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct JoinGroupActions<P> {
    send_report_and_schedule_timer: Option<(P, Duration)>,
}

impl<P> JoinGroupActions<P> {
    const NOOP: Self = Self { send_report_and_schedule_timer: None };
}

/// Actions to take as a consequence of leaving a group.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct LeaveGroupActions {
    send_leave: bool,
    stop_timer: bool,
}

impl LeaveGroupActions {
    const NOOP: Self = Self { send_leave: false, stop_timer: false };
}

/// Actions to take as a consequence of handling a received report message.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct ReportReceivedActions {
    stop_timer: bool,
}

impl ReportReceivedActions {
    const NOOP: Self = Self { stop_timer: false };
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum QueryReceivedGenericAction<P> {
    ScheduleTimer(Duration),
    StopTimerAndSendReport(P),
}

/// Actions to take as a consequence of receiving a query message.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct QueryReceivedActions<P: ProtocolSpecific> {
    generic: Option<QueryReceivedGenericAction<P>>,
    protocol_specific: Option<P::Actions>,
}

impl<P: ProtocolSpecific> QueryReceivedActions<P> {
    const NOOP: Self = Self { generic: None, protocol_specific: None };
}

/// Actions to take as a consequence of a report timer expiring.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct ReportTimerExpiredActions<P> {
    send_report: P,
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
    ) -> GroupJoinResult<JoinGroupActions<P>>
    where
        T: From<GmpStateMachine<I, P>>,
        P::Config: Default,
    {
        self.join_group_with(group, || {
            let (state, actions) = GmpStateMachine::join_group(rng, now, gmp_disabled);
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
    ) -> GroupLeaveResult<LeaveGroupActions>
    where
        T: Into<GmpStateMachine<I, P>>,
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
        gmp_handle_maybe_enabled(self, bindings_ctx, device)
    }

    fn gmp_handle_disabled(&mut self, bindings_ctx: &mut BC, device: &Self::DeviceId) {
        gmp_handle_disabled(self, bindings_ctx, device)
    }

    fn gmp_join_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupJoinResult {
        gmp_join_group(self, bindings_ctx, device, group_addr)
    }

    fn gmp_leave_group(
        &mut self,
        bindings_ctx: &mut BC,
        device: &CC::DeviceId,
        group_addr: MulticastAddr<I::Addr>,
    ) -> GroupLeaveResult {
        gmp_leave_group(self, bindings_ctx, device, group_addr)
    }
}

/// This trait is used to model the different parts of the two protocols.
///
/// Though MLD and IGMPv2 share the most part of their state machines there are
/// some subtle differences between each other.
pub trait ProtocolSpecific: Copy + Default {
    /// The type for protocol-specific actions.
    type Actions;
    /// The type for protocol-specific configs.
    type Config: Debug + Default;

    /// The maximum delay to wait to send an unsolicited report.
    fn cfg_unsolicited_report_interval(cfg: &Self::Config) -> Duration;

    /// Whether the host should send a leave message even if it is not the last
    /// host in the group.
    fn cfg_send_leave_anyway(cfg: &Self::Config) -> bool;

    /// Get the _real_ `MAX_RESP_TIME`
    ///
    /// `None` indicates that the maximum response time is zero and thus a
    /// response should be sent immediately.
    fn get_max_resp_time(resp_time: Duration) -> Option<NonZeroDuration>;

    /// Respond to a query in a protocol-specific way.
    ///
    /// When receiving a query, IGMPv2 needs to check whether the query is an
    /// IGMPv1 message and, if so, set a local "IGMPv1 Router Present" flag and
    /// set a timer. For MLD, this function is a no-op.
    fn do_query_received_specific(
        cfg: &Self::Config,
        max_resp_time: Duration,
        old: Self,
    ) -> (Self, Option<Self::Actions>);
}

/// The transition between one state and the next.
///
/// A `Transition` includes the next state to enter and any actions to take
/// while executing the transition.
struct Transition<S, P: ProtocolSpecific, Actions>(GmpHostState<S, P>, Actions);

/// This is used to represent the states that are common in both MLD and IGMPv2.
/// The state machine should behave as described on [RFC 2236 page 10] and [RFC
/// 2710 page 10].
///
/// [RFC 2236 page 10]: https://tools.ietf.org/html/rfc2236#page-10
/// [RFC 2710 page 10]: https://tools.ietf.org/html/rfc2710#page-10
#[cfg_attr(test, derive(Debug))]
struct GmpHostState<State, P: ProtocolSpecific> {
    state: State,
    /// `protocol_specific` are the value(s) you don't want the users to have a
    /// chance to modify. It is supposed to be only modified by the protocol
    /// itself.
    protocol_specific: P,
    /// `cfg` is used to store value(s) that is supposed to be modified by
    /// users.
    cfg: P::Config,
}

impl<S, P: ProtocolSpecific> GmpHostState<S, P> {
    /// Construct a `Transition` from this state into the new state `T` with the
    /// given actions.
    fn transition<T, A>(self, t: T, actions: A) -> Transition<T, P, A> {
        Transition(
            GmpHostState { state: t, protocol_specific: self.protocol_specific, cfg: self.cfg },
            actions,
        )
    }
}

// Used to write tests in the `igmp` and `mld` modules.
#[cfg(test)]
impl<S, P: ProtocolSpecific> GmpHostState<S, P> {
    fn get_protocol_specific(&self) -> P {
        self.protocol_specific
    }

    fn get_state(&self) -> &S {
        &self.state
    }
}

/// Represents Non Member-specific state variables.
///
/// Memberships may be a non-member when joined locally but are not performing
/// GMP.
///
/// Note that the special all-nodes addresses 224.0.0.1 and ff02::1 are modelled
/// as permanently in `NonMember` state instead of `Idle` state in NS3.
#[cfg_attr(test, derive(Debug))]
struct NonMember;

/// Represents Delaying Member-specific state variables.
#[cfg_attr(test, derive(Debug))]
struct DelayingMember<I: Instant> {
    /// The expiration time for the current timer. Useful to check if the timer
    /// needs to be reset when a query arrives.
    timer_expiration: I,

    /// Used to indicate whether we need to send out a Leave message when we are
    /// leaving the group. This flag will become false once we heard about
    /// another reporter.
    last_reporter: bool,
}

/// Represents Idle Member-specific state variables.
#[cfg_attr(test, derive(Debug))]
struct IdleMember {
    /// Used to indicate whether we need to send out a Leave message when we are
    /// leaving the group.
    last_reporter: bool,
}

/// The state for a multicast group membership.
///
/// The terms used here are biased towards [IGMPv2]. In [MLD], their names are
/// {Non, Delaying, Idle}-Listener instead.
///
/// [IGMPv2]: https://tools.ietf.org/html/rfc2236
/// [MLD]: https://tools.ietf.org/html/rfc2710
#[cfg_attr(test, derive(Debug))]
enum MemberState<I: Instant, P: ProtocolSpecific> {
    NonMember(GmpHostState<NonMember, P>),
    Delaying(GmpHostState<DelayingMember<I>, P>),
    Idle(GmpHostState<IdleMember, P>),
}

impl<I: Instant, P: ProtocolSpecific> From<GmpHostState<NonMember, P>> for MemberState<I, P> {
    fn from(s: GmpHostState<NonMember, P>) -> Self {
        MemberState::NonMember(s)
    }
}

impl<I: Instant, P: ProtocolSpecific> From<GmpHostState<DelayingMember<I>, P>>
    for MemberState<I, P>
{
    fn from(s: GmpHostState<DelayingMember<I>, P>) -> Self {
        MemberState::Delaying(s)
    }
}

impl<I: Instant, P: ProtocolSpecific> From<GmpHostState<IdleMember, P>> for MemberState<I, P> {
    fn from(s: GmpHostState<IdleMember, P>) -> Self {
        MemberState::Idle(s)
    }
}

impl<S, P: ProtocolSpecific, A> Transition<S, P, A> {
    fn into_state_actions<I: Instant>(self) -> (MemberState<I, P>, A)
    where
        MemberState<I, P>: From<GmpHostState<S, P>>,
    {
        (self.0.into(), self.1)
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

/// Compute the next state and actions to take for a member state (Delaying or
/// Idle member) that has received a query message.
///
/// # Arguments
/// * `last_reporter` indicates if the last report was sent by this node.
/// * `timer_expiration` is `None` if there are currently no timers, otherwise
///   `Some(t)` where `t` is the old instant when the currently installed timer
///   should fire. That is, `None` if an Idle member and `Some` if a Delaying
///   member.
/// * `max_resp_time` is the maximum response time required by Query message.
fn member_query_received<P: ProtocolSpecific, R: Rng, I: Instant>(
    rng: &mut R,
    last_reporter: bool,
    timer_expiration: Option<I>,
    max_resp_time: Duration,
    now: I,
    cfg: P::Config,
    ps: P,
) -> (MemberState<I, P>, QueryReceivedActions<P>) {
    let (protocol_specific, ps_actions) = P::do_query_received_specific(&cfg, max_resp_time, ps);

    let (transition, generic_actions) = match P::get_max_resp_time(max_resp_time) {
        None => (
            GmpHostState { state: IdleMember { last_reporter }, protocol_specific, cfg }.into(),
            Some(QueryReceivedGenericAction::StopTimerAndSendReport(protocol_specific)),
        ),
        Some(max_resp_time) => {
            let max_resp_time = max_resp_time.get();
            let new_deadline = now.checked_add(max_resp_time).unwrap();

            let (timer_expiration, action) = match timer_expiration {
                Some(old) if new_deadline >= old => (old, None),
                None | Some(_) => {
                    let delay = random_report_timeout(rng, max_resp_time);
                    (
                        now.checked_add(delay).unwrap(),
                        Some(QueryReceivedGenericAction::ScheduleTimer(delay)),
                    )
                }
            };

            (
                GmpHostState {
                    state: DelayingMember { last_reporter, timer_expiration },
                    protocol_specific,
                    cfg,
                }
                .into(),
                action,
            )
        }
    };

    (transition, QueryReceivedActions { generic: generic_actions, protocol_specific: ps_actions })
}

impl<P: ProtocolSpecific> GmpHostState<NonMember, P> {
    fn join_group<I: Instant, R: Rng>(
        self,
        rng: &mut R,
        now: I,
    ) -> Transition<DelayingMember<I>, P, JoinGroupActions<P>> {
        let duration = P::cfg_unsolicited_report_interval(&self.cfg);
        let delay = random_report_timeout(rng, duration);
        let actions = JoinGroupActions {
            send_report_and_schedule_timer: Some((self.protocol_specific, delay)),
        };
        self.transition(
            DelayingMember {
                last_reporter: true,
                timer_expiration: now.checked_add(delay).expect("timer expiration overflowed"),
            },
            actions,
        )
    }

    fn leave_group(self) -> Transition<NonMember, P, LeaveGroupActions> {
        self.transition(NonMember, LeaveGroupActions::NOOP)
    }
}

impl<I: Instant, P: ProtocolSpecific> GmpHostState<DelayingMember<I>, P> {
    fn query_received<R: Rng>(
        self,
        rng: &mut R,
        max_resp_time: Duration,
        now: I,
    ) -> (MemberState<I, P>, QueryReceivedActions<P>) {
        let GmpHostState {
            state: DelayingMember { last_reporter, timer_expiration },
            protocol_specific,
            cfg,
        } = self;
        member_query_received(
            rng,
            last_reporter,
            Some(timer_expiration),
            max_resp_time,
            now,
            cfg,
            protocol_specific,
        )
    }

    fn leave_group(self) -> Transition<NonMember, P, LeaveGroupActions> {
        let actions = LeaveGroupActions {
            send_leave: self.state.last_reporter || P::cfg_send_leave_anyway(&self.cfg),
            stop_timer: true,
        };
        self.transition(NonMember, actions)
    }

    fn report_received(self) -> Transition<IdleMember, P, ReportReceivedActions> {
        self.transition(
            IdleMember { last_reporter: false },
            ReportReceivedActions { stop_timer: true },
        )
    }

    fn report_timer_expired(self) -> Transition<IdleMember, P, ReportTimerExpiredActions<P>> {
        let actions = ReportTimerExpiredActions { send_report: self.protocol_specific };
        self.transition(IdleMember { last_reporter: true }, actions)
    }
}

impl<P: ProtocolSpecific> GmpHostState<IdleMember, P> {
    fn query_received<I: Instant, R: Rng>(
        self,
        rng: &mut R,
        max_resp_time: Duration,
        now: I,
    ) -> (MemberState<I, P>, QueryReceivedActions<P>) {
        let GmpHostState { state: IdleMember { last_reporter }, protocol_specific, cfg } = self;
        member_query_received(rng, last_reporter, None, max_resp_time, now, cfg, protocol_specific)
    }

    fn leave_group(self) -> Transition<NonMember, P, LeaveGroupActions> {
        let actions = LeaveGroupActions {
            send_leave: self.state.last_reporter || P::cfg_send_leave_anyway(&self.cfg),
            stop_timer: false,
        };
        self.transition(NonMember, actions)
    }
}

impl<I: Instant, P: ProtocolSpecific> MemberState<I, P> {
    /// Performs the "join group" transition, producing a new `MemberState` and
    /// set of actions to execute.
    fn join_group<R: Rng>(
        protocol_specific: P,
        cfg: P::Config,
        rng: &mut R,
        now: I,
        gmp_disabled: bool,
    ) -> (MemberState<I, P>, JoinGroupActions<P>) {
        let non_member = GmpHostState { protocol_specific, cfg, state: NonMember };
        if gmp_disabled {
            (non_member.into(), JoinGroupActions::NOOP)
        } else {
            non_member.join_group(rng, now).into_state_actions()
        }
    }

    /// Performs the "leave group" transition, consuming the state by value, and
    /// returning the next state and a set of actions to execute.
    fn leave_group(self) -> (MemberState<I, P>, LeaveGroupActions) {
        // Rust can infer these types, but since we're just discarding `_state`,
        // we explicitly make sure it's the state we expect in case we introduce
        // a bug.
        match self {
            MemberState::NonMember(state) => state.leave_group(),
            MemberState::Delaying(state) => state.leave_group(),
            MemberState::Idle(state) => state.leave_group(),
        }
        .into_state_actions()
    }

    fn query_received<R: Rng>(
        self,
        rng: &mut R,
        max_resp_time: Duration,
        now: I,
    ) -> (MemberState<I, P>, QueryReceivedActions<P>) {
        match self {
            state @ MemberState::NonMember(_) => (state, QueryReceivedActions::NOOP),
            MemberState::Delaying(state) => state.query_received(rng, max_resp_time, now),
            MemberState::Idle(state) => state.query_received(rng, max_resp_time, now),
        }
    }

    fn report_received(self) -> (MemberState<I, P>, ReportReceivedActions) {
        match self {
            state @ MemberState::Idle(_) | state @ MemberState::NonMember(_) => {
                (state, ReportReceivedActions::NOOP)
            }
            MemberState::Delaying(state) => state.report_received().into_state_actions(),
        }
    }

    fn report_timer_expired(self) -> (MemberState<I, P>, ReportTimerExpiredActions<P>) {
        match self {
            MemberState::Idle(_) | MemberState::NonMember(_) => {
                unreachable!("got report timer in non-delaying state")
            }
            MemberState::Delaying(state) => state.report_timer_expired().into_state_actions(),
        }
    }
}

#[cfg_attr(test, derive(Debug))]
pub struct GmpStateMachine<I: Instant, P: ProtocolSpecific> {
    // Invariant: `inner` is always `Some`. It is stored as an `Option` so that
    // methods can `.take()` the `MemberState` in order to perform transitions
    // that consume `MemberState` by value. However, a new `MemberState` is
    // always put back in its place so that `inner` is `Some` by the time the
    // methods return.
    inner: Option<MemberState<I, P>>,
}

impl<I: Instant, P: ProtocolSpecific + Default> GmpStateMachine<I, P>
where
    P::Config: Default,
{
    /// When a "join group" command is received.
    ///
    /// `join_group` initializes a new state machine in the Non-Member state and
    /// then immediately executes the "join group" transition. The new state
    /// machine is returned along with any actions to take.
    fn join_group<R: Rng>(
        rng: &mut R,
        now: I,
        gmp_disabled: bool,
    ) -> (GmpStateMachine<I, P>, JoinGroupActions<P>) {
        let (state, actions) =
            MemberState::join_group(P::default(), P::Config::default(), rng, now, gmp_disabled);
        (GmpStateMachine { inner: Some(state) }, actions)
    }
}

impl<I: Instant, P: ProtocolSpecific> GmpStateMachine<I, P> {
    /// Attempts to join the group if the group is currently in the non-member
    /// state.
    ///
    /// If the group is in a member state (delaying/idle), this method does
    /// nothing.
    fn join_if_non_member<R: Rng>(&mut self, rng: &mut R, now: I) -> JoinGroupActions<P> {
        self.update(|s| match s {
            MemberState::NonMember(s) => s.join_group(rng, now).into_state_actions(),
            state @ MemberState::Delaying(_) | state @ MemberState::Idle(_) => {
                (state, JoinGroupActions::NOOP)
            }
        })
    }

    /// Leaves the group if the group is in a member state.
    ///
    /// Does nothing if the group is in a non-member state.
    fn leave_if_member(&mut self) -> LeaveGroupActions {
        self.update(|s| s.leave_group())
    }

    /// When a "leave group" command is received.
    ///
    /// `leave_group` consumes the state machine by value since we don't allow
    /// storing a state machine in the Non-Member state.
    fn leave_group(self) -> LeaveGroupActions {
        // This `unwrap` is safe because we maintain the invariant that `inner`
        // is always `Some`.
        let (_state, actions) = self.inner.unwrap().leave_group();
        actions
    }

    /// When a query is received, and we have to respond within max_resp_time.
    fn query_received<R: Rng>(
        &mut self,
        rng: &mut R,
        max_resp_time: Duration,
        now: I,
    ) -> QueryReceivedActions<P> {
        self.update(|s| s.query_received(rng, max_resp_time, now))
    }

    /// We have received a report from another host on our local network.
    fn report_received(&mut self) -> ReportReceivedActions {
        self.update(MemberState::report_received)
    }

    /// The timer installed has expired.
    fn report_timer_expired(&mut self) -> ReportTimerExpiredActions<P> {
        self.update(MemberState::report_timer_expired)
    }

    /// Update the state with no argument.
    fn update<A, F: FnOnce(MemberState<I, P>) -> (MemberState<I, P>, A)>(&mut self, f: F) -> A {
        let (s, a) = f(self.inner.take().unwrap());
        self.inner = Some(s);
        a
    }

    /// Update the state with a new protocol-specific value.
    fn update_with_protocol_specific(&mut self, ps: P) {
        self.update(|s| {
            (
                match s {
                    MemberState::NonMember(GmpHostState { state, cfg, protocol_specific: _ }) => {
                        MemberState::NonMember(GmpHostState { state, cfg, protocol_specific: ps })
                    }
                    MemberState::Delaying(GmpHostState { state, cfg, protocol_specific: _ }) => {
                        MemberState::Delaying(GmpHostState { state, cfg, protocol_specific: ps })
                    }
                    MemberState::Idle(GmpHostState { state, cfg, protocol_specific: _ }) => {
                        MemberState::Idle(GmpHostState { state, cfg, protocol_specific: ps })
                    }
                },
                (),
            )
        })
    }

    #[cfg(test)]
    fn get_inner(&self) -> &MemberState<I, P> {
        self.inner.as_ref().unwrap()
    }
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
    type GroupState: From<GmpStateMachine<BT::Instant, Self::ProtocolSpecific>>
        + Into<GmpStateMachine<BT::Instant, Self::ProtocolSpecific>>
        + AsMut<GmpStateMachine<BT::Instant, Self::ProtocolSpecific>>;
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
trait GmpContext<I: IpExt, BC: GmpBindingsContext>: GmpTypeLayout<I, BC> {
    type Err;

    /// Calls the function with a boolean indicating whether GMP is disabled and
    /// an mutable reference to GMP state.
    fn with_gmp_state_mut<O, F: FnOnce(GmpStateRef<'_, I, Self, BC>) -> O>(
        &mut self,
        device: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Sends a GMP message.
    // TODO(https://fxbug.dev/336871374): We should consider changing the shape
    // of this trait to force linearization of sent messages by requiring that
    // the GMP state lock is held to send messages and running subprotocol
    // actions.
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
        actions: <Self::ProtocolSpecific as ProtocolSpecific>::Actions,
    );

    fn not_a_member_err(addr: I::Addr) -> Self::Err;
}

fn gmp_handle_timer<I, BC, CC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    timer: GmpDelayedReportTimerId<I, CC::WeakDeviceId>,
) where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    let GmpDelayedReportTimerId { device, _marker: IpVersionMarker { .. } } = timer;
    let Some(device) = device.upgrade() else {
        return;
    };
    let actions =
        core_ctx.with_gmp_state_mut(&device, |GmpStateRef { enabled: _, groups, gmp }| {
            let (group_addr, ()) = gmp.timers.pop(bindings_ctx)?;
            let actions = groups
                .get_mut(&group_addr)
                .expect("get state for group with expired report timer")
                .as_mut()
                .report_timer_expired();
            Some((group_addr, actions))
        });

    if let Some((group_addr, ReportTimerExpiredActions { send_report })) = actions {
        core_ctx.send_message(
            bindings_ctx,
            &device,
            group_addr,
            GmpMessageType::Report(send_report),
        );
    }
}

trait GmpMessage<I: Ip> {
    fn group_addr(&self) -> I::Addr;
}

fn handle_report_message<I, BC, CC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
) -> Result<(), CC::Err>
where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    core_ctx.with_gmp_state_mut(device, |GmpStateRef { enabled: _, groups, gmp }| {
        let ReportReceivedActions { stop_timer } = groups
            .get_mut(&group_addr)
            .ok_or_else(|| CC::not_a_member_err(*group_addr))
            .map(|a| a.as_mut().report_received())?;
        if stop_timer {
            assert_matches!(gmp.timers.cancel(bindings_ctx, &group_addr), Some(_));
        }
        Ok(())
    })
}

/// The group targeted in a query message.
enum QueryTarget<A> {
    Unspecified,
    Specified(MulticastAddr<A>),
}

fn handle_query_message<I, BC, CC>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    target: QueryTarget<I::Addr>,
    max_response_time: Duration,
) -> Result<(), CC::Err>
where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    let addr_and_actions =
        core_ctx.with_gmp_state_mut(device, |GmpStateRef { enabled: _, groups, gmp }| {
            let now = bindings_ctx.now();

            let iter = match target {
                QueryTarget::Unspecified => {
                    either::Either::Left(groups.iter_mut().map(|(addr, state)| (*addr, state)))
                }
                QueryTarget::Specified(group_addr) => either::Either::Right(core::iter::once((
                    group_addr,
                    groups.get_mut(&group_addr).ok_or_else(|| CC::not_a_member_err(*group_addr))?,
                ))),
            };

            Ok(iter
                .filter_map(|(group_addr, state)| {
                    let QueryReceivedActions { generic, protocol_specific } = state
                        .as_mut()
                        .query_received(&mut bindings_ctx.rng(), max_response_time, now);
                    let send_msg = generic.and_then(|generic| match generic {
                        QueryReceivedGenericAction::ScheduleTimer(delay) => {
                            let _: Option<(BC::Instant, ())> =
                                gmp.timers.schedule_after(bindings_ctx, group_addr, (), delay);
                            None
                        }
                        QueryReceivedGenericAction::StopTimerAndSendReport(protocol_specific) => {
                            let _: Option<(BC::Instant, ())> =
                                gmp.timers.cancel(bindings_ctx, &group_addr);
                            Some(GmpMessageType::Report(protocol_specific))
                        }
                    });
                    match (send_msg, protocol_specific) {
                        (None, None) => None,
                        // If we have anything else to do, collect to perform
                        // outside of locking.
                        (send_msg, protocol_specific) => {
                            Some((group_addr, send_msg, protocol_specific))
                        }
                    }
                })
                .collect::<Vec<_>>())
        })?;

    for (group_addr, send_msg, ps_actions) in addr_and_actions {
        if let Some(msg) = send_msg {
            core_ctx.send_message(bindings_ctx, device, group_addr, msg);
        };

        if let Some(ps_actions) = ps_actions {
            core_ctx.run_actions(bindings_ctx, device, ps_actions);
        }
    }

    Ok(())
}

fn gmp_handle_maybe_enabled<BC, CC, I>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
) where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    let messages = core_ctx.with_gmp_state_mut(device, |GmpStateRef { enabled, groups, gmp }| {
        if !enabled {
            return Vec::new();
        }

        let now = bindings_ctx.now();

        groups
            .iter_mut()
            .filter_map(|(group_addr, state)| {
                let group_addr = *group_addr;
                I::should_perform_gmp(group_addr)
                    .then(|| {
                        let JoinGroupActions { send_report_and_schedule_timer } =
                            state.as_mut().join_if_non_member(&mut bindings_ctx.rng(), now);
                        let (protocol_specific, delay) = send_report_and_schedule_timer?;
                        assert_matches!(
                            gmp.timers.schedule_after(bindings_ctx, group_addr, (), delay),
                            None
                        );
                        Some((group_addr, GmpMessageType::Report(protocol_specific)))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>()
    });

    for (group_addr, msg) in messages {
        core_ctx.send_message(bindings_ctx, device, group_addr, msg);
    }
}

fn gmp_handle_disabled<BC, CC, I>(core_ctx: &mut CC, bindings_ctx: &mut BC, device: &CC::DeviceId)
where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    let leave_groups =
        core_ctx.with_gmp_state_mut(device, |GmpStateRef { enabled: _, groups, gmp }| {
            groups
                .groups_mut()
                .filter_map(|(group_addr, state)| {
                    let LeaveGroupActions { send_leave, stop_timer } =
                        state.as_mut().leave_if_member();
                    if stop_timer {
                        assert_matches!(gmp.timers.cancel(bindings_ctx, group_addr), Some(_));
                    }
                    send_leave.then(|| *group_addr)
                })
                .collect::<Vec<_>>()
        });

    for group_addr in leave_groups {
        core_ctx.send_message(bindings_ctx, device, group_addr, GmpMessageType::Leave);
    }
}

fn gmp_join_group<BC, CC, I>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
) -> GroupJoinResult
where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    core_ctx
        .with_gmp_state_mut(device, |GmpStateRef { enabled, groups, gmp }| {
            let now = bindings_ctx.now();

            let result = groups.join_group_gmp(
                !enabled || !I::should_perform_gmp(group_addr),
                group_addr,
                &mut bindings_ctx.rng(),
                now,
            );
            result.map(|JoinGroupActions { send_report_and_schedule_timer }| {
                send_report_and_schedule_timer.map(|(protocol_specific, delay)| {
                    assert_matches!(
                        gmp.timers.schedule_after(bindings_ctx, group_addr, (), delay),
                        None
                    );
                    GmpMessageType::Report(protocol_specific)
                })
            })
        })
        .map(|msg| {
            if let Some(msg) = msg {
                core_ctx.send_message(bindings_ctx, device, group_addr, msg);
            }
        })
}

fn gmp_leave_group<BC, CC, I>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
) -> GroupLeaveResult
where
    BC: GmpBindingsContext,
    CC: GmpContext<I, BC>,
    I: IpExt,
{
    core_ctx
        .with_gmp_state_mut(device, |GmpStateRef { enabled: _, groups, gmp }| {
            groups.leave_group_gmp(group_addr).map(
                |LeaveGroupActions { send_leave, stop_timer }| {
                    if stop_timer {
                        assert_matches!(gmp.timers.cancel(bindings_ctx, &group_addr), Some(_));
                    }
                    send_leave
                },
            )
        })
        .map(|send_leave| {
            if send_leave {
                core_ctx.send_message(bindings_ctx, device, group_addr, GmpMessageType::Leave);
            }
        })
}
#[cfg(test)]
mod test {
    use core::convert::Infallible as Never;

    use assert_matches::assert_matches;
    use netstack3_base::testutil::{new_rng, FakeInstant};

    use super::*;

    const DEFAULT_UNSOLICITED_REPORT_INTERVAL: Duration = Duration::from_secs(10);

    /// Fake `ProtocolSpecific` for test purposes.
    #[derive(PartialEq, Eq, Copy, Clone, Debug, Default)]
    struct FakeProtocolSpecific;

    impl ProtocolSpecific for FakeProtocolSpecific {
        /// Tests for generic state machine should not know anything about
        /// protocol specific actions.
        type Actions = Never;

        /// Whether to send leave group message if our flag is not set.
        type Config = bool;

        fn cfg_unsolicited_report_interval(_cfg: &Self::Config) -> Duration {
            DEFAULT_UNSOLICITED_REPORT_INTERVAL
        }

        fn cfg_send_leave_anyway(cfg: &Self::Config) -> bool {
            *cfg
        }

        fn get_max_resp_time(resp_time: Duration) -> Option<NonZeroDuration> {
            NonZeroDuration::new(resp_time)
        }

        fn do_query_received_specific(
            _cfg: &Self::Config,
            _max_resp_time: Duration,
            old: Self,
        ) -> (Self, Option<Never>) {
            (old, None)
        }
    }

    impl<P: ProtocolSpecific> GmpStateMachine<FakeInstant, P> {
        pub(crate) fn get_config_mut(&mut self) -> &mut P::Config {
            match self.inner.as_mut().unwrap() {
                MemberState::NonMember(s) => &mut s.cfg,
                MemberState::Delaying(s) => &mut s.cfg,
                MemberState::Idle(s) => &mut s.cfg,
            }
        }
    }

    type FakeGmpStateMachine = GmpStateMachine<FakeInstant, FakeProtocolSpecific>;

    #[test]
    fn test_gmp_state_non_member_to_delay_should_set_flag() {
        let (s, _actions) =
            FakeGmpStateMachine::join_group(&mut new_rng(0), FakeInstant::default(), false);
        match s.get_inner() {
            MemberState::Delaying(s) => assert!(s.get_state().last_reporter),
            _ => panic!("Wrong State!"),
        }
    }

    #[test]
    fn test_gmp_state_non_member_to_delay_actions() {
        let (_state, actions) =
            FakeGmpStateMachine::join_group(&mut new_rng(0), FakeInstant::default(), false);
        assert_matches!(
            actions,
            JoinGroupActions { send_report_and_schedule_timer: Some((FakeProtocolSpecific, d)) } if d <= DEFAULT_UNSOLICITED_REPORT_INTERVAL
        );
    }

    #[test]
    fn test_gmp_state_delay_no_reset_timer() {
        let mut rng = new_rng(0);
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_eq!(
            s.query_received(
                &mut rng,
                DEFAULT_UNSOLICITED_REPORT_INTERVAL + Duration::from_secs(1),
                FakeInstant::default(),
            ),
            QueryReceivedActions { generic: None, protocol_specific: None }
        );
    }

    #[test]
    fn test_gmp_state_delay_reset_timer() {
        let mut rng = new_rng(0);
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_eq!(
            s.query_received(&mut rng, Duration::from_millis(1), FakeInstant::default()),
            QueryReceivedActions {
                generic: Some(QueryReceivedGenericAction::ScheduleTimer(Duration::from_micros(1))),
                protocol_specific: None
            }
        );
    }

    #[test]
    fn test_gmp_state_delay_to_idle_with_report_no_flag() {
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut new_rng(0), FakeInstant::default(), false);
        assert_eq!(s.report_received(), ReportReceivedActions { stop_timer: true });
        match s.get_inner() {
            MemberState::Idle(s) => {
                assert!(!s.get_state().last_reporter);
            }
            _ => panic!("Wrong State!"),
        }
    }

    #[test]
    fn test_gmp_state_delay_to_idle_without_report_set_flag() {
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut new_rng(0), FakeInstant::default(), false);
        assert_eq!(
            s.report_timer_expired(),
            ReportTimerExpiredActions { send_report: FakeProtocolSpecific }
        );
        match s.get_inner() {
            MemberState::Idle(s) => {
                assert!(s.get_state().last_reporter);
            }
            _ => panic!("Wrong State!"),
        }
    }

    #[test]
    fn test_gmp_state_leave_should_send_leave() {
        let mut rng = new_rng(0);
        let (s, _actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_eq!(s.leave_group(), LeaveGroupActions { send_leave: true, stop_timer: true },);
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_eq!(
            s.report_timer_expired(),
            ReportTimerExpiredActions { send_report: FakeProtocolSpecific }
        );
        assert_eq!(s.leave_group(), LeaveGroupActions { send_leave: true, stop_timer: false });
    }

    #[test]
    fn test_gmp_state_delay_to_other_states_should_stop_timer() {
        let mut rng = new_rng(0);
        let (s, _actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_eq!(s.leave_group(), LeaveGroupActions { send_leave: true, stop_timer: true },);
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_eq!(s.report_received(), ReportReceivedActions { stop_timer: true });
    }

    #[test]
    fn test_gmp_state_other_states_to_delay_should_schedule_timer() {
        let mut rng = new_rng(0);
        let (mut s, actions) =
            FakeGmpStateMachine::join_group(&mut rng, FakeInstant::default(), false);
        assert_matches!(
            actions,
            JoinGroupActions { send_report_and_schedule_timer: Some((FakeProtocolSpecific, d)) } if d <= DEFAULT_UNSOLICITED_REPORT_INTERVAL
        );
        assert_eq!(s.report_received(), ReportReceivedActions { stop_timer: true });
        assert_eq!(
            s.query_received(&mut rng, Duration::from_secs(1), FakeInstant::default()),
            QueryReceivedActions {
                generic: Some(QueryReceivedGenericAction::ScheduleTimer(Duration::from_micros(1))),
                protocol_specific: None
            }
        );
    }

    #[test]
    fn test_gmp_state_leave_send_anyway_do_send() {
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut new_rng(0), FakeInstant::default(), false);
        *s.get_config_mut() = true;
        assert_eq!(s.report_received(), ReportReceivedActions { stop_timer: true });
        match s.get_inner() {
            MemberState::Idle(s) => assert!(!s.get_state().last_reporter),
            _ => panic!("Wrong State!"),
        }
        assert_eq!(s.leave_group(), LeaveGroupActions { send_leave: true, stop_timer: false });
    }

    #[test]
    fn test_gmp_state_leave_not_the_last_do_nothing() {
        let (mut s, _actions) =
            FakeGmpStateMachine::join_group(&mut new_rng(0), FakeInstant::default(), false);
        assert_eq!(s.report_received(), ReportReceivedActions { stop_timer: true });
        assert_eq!(s.leave_group(), LeaveGroupActions { send_leave: false, stop_timer: false })
    }
}
