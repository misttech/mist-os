// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! GMP V1 common implementation.
//!
//! GMPv1 is the common implementation of a fictitious GMP protocol that covers
//! the common parts of MLDv1 and IGMPv1, IGMPv2.

use core::time::Duration;

use assert_matches::assert_matches;
use net_types::ip::IpVersionMarker;
use net_types::MulticastAddr;
use netstack3_base::{Instant, WeakDeviceIdentifier};
use packet_formats::utils::NonZeroDuration;
use rand::Rng;

use crate::internal::gmp::{
    self, GmpBindingsContext, GmpContext, GmpContextInner, GmpDelayedReportTimerId, GmpMessageType,
    GmpStateRef, GmpTypeLayout, GroupJoinResult, GroupLeaveResult, IpExt, ProtocolSpecificTypes,
};

/// Actions to take as a consequence of joining a group.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct JoinGroupActions<P> {
    send_report_and_schedule_timer: Option<(P, Duration)>,
}

impl<P> JoinGroupActions<P> {
    const NOOP: Self = Self { send_report_and_schedule_timer: None };
}

/// Actions to take as a consequence of leaving a group.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct LeaveGroupActions {
    send_leave: bool,
    stop_timer: bool,
}

impl LeaveGroupActions {
    const NOOP: Self = Self { send_leave: false, stop_timer: false };
}

/// Actions to take as a consequence of handling a received report message.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct ReportReceivedActions {
    pub(super) stop_timer: bool,
}

impl ReportReceivedActions {
    const NOOP: Self = Self { stop_timer: false };
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) enum QueryReceivedGenericAction<P> {
    ScheduleTimer(Duration),
    StopTimerAndSendReport(P),
}

/// Actions to take as a consequence of receiving a query message.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct QueryReceivedActions<P: ProtocolSpecific> {
    pub(super) generic: Option<QueryReceivedGenericAction<P>>,
    pub(super) protocol_specific: Option<P::Actions>,
}

impl<P: ProtocolSpecific> QueryReceivedActions<P> {
    const NOOP: Self = Self { generic: None, protocol_specific: None };
}

/// Actions to take as a consequence of a report timer expiring.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct ReportTimerExpiredActions<P> {
    pub(super) send_report: P,
}

/// This trait is used to model the different parts of MLDv1 and IGMPv1/v2.
pub trait ProtocolSpecific: ProtocolSpecificTypes {
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
pub(super) struct GmpHostState<State, P: ProtocolSpecific> {
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
pub(super) struct NonMember;

/// Represents Delaying Member-specific state variables.
#[cfg_attr(test, derive(Debug))]
pub(super) struct DelayingMember<I: Instant> {
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
pub(super) struct IdleMember {
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
pub(super) enum MemberState<I: Instant, P: ProtocolSpecific> {
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
                    let delay = gmp::random_report_timeout(rng, max_resp_time);
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
        let delay = gmp::random_report_timeout(rng, duration);
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
    pub(super) inner: Option<MemberState<I, P>>,
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
    pub(super) fn join_group<R: Rng>(
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
    pub(super) fn leave_group(self) -> LeaveGroupActions {
        // This `unwrap` is safe because we maintain the invariant that `inner`
        // is always `Some`.
        let (_state, actions) = self.inner.unwrap().leave_group();
        actions
    }

    /// When a query is received, and we have to respond within max_resp_time.
    pub(super) fn query_received<R: Rng>(
        &mut self,
        rng: &mut R,
        max_resp_time: Duration,
        now: I,
    ) -> QueryReceivedActions<P> {
        self.update(|s| s.query_received(rng, max_resp_time, now))
    }

    /// We have received a report from another host on our local network.
    pub(super) fn report_received(&mut self) -> ReportReceivedActions {
        self.update(MemberState::report_received)
    }

    /// The timer installed has expired.
    pub(super) fn report_timer_expired(&mut self) -> ReportTimerExpiredActions<P> {
        self.update(MemberState::report_timer_expired)
    }

    /// Update the state with no argument.
    fn update<A, F: FnOnce(MemberState<I, P>) -> (MemberState<I, P>, A)>(&mut self, f: F) -> A {
        let (s, a) = f(self.inner.take().unwrap());
        self.inner = Some(s);
        a
    }

    #[cfg(test)]
    pub(super) fn get_inner(&self) -> &MemberState<I, P> {
        self.inner.as_ref().unwrap()
    }
}

pub(super) fn handle_timer<I, BC, CC>(
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
    core_ctx.with_gmp_state_mut_and_ctx(
        &device,
        |mut core_ctx, GmpStateRef { enabled: _, groups, gmp }| {
            let Some((group_addr, ())) = gmp.timers.pop(bindings_ctx) else {
                return;
            };
            let ReportTimerExpiredActions { send_report } = groups
                .get_mut(&group_addr)
                .expect("get state for group with expired report timer")
                .as_mut()
                .report_timer_expired();

            core_ctx.send_message(
                bindings_ctx,
                &device,
                group_addr,
                GmpMessageType::Report(send_report),
            );
        },
    );
}

pub(super) fn handle_report_message<I, BC, CC>(
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
pub(super) enum QueryTarget<A> {
    Unspecified,
    Specified(MulticastAddr<A>),
}

pub(super) fn handle_query_message<I, BC, CC>(
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
    core_ctx.with_gmp_state_mut_and_ctx(
        device,
        |mut core_ctx, GmpStateRef { enabled: _, groups, gmp }| {
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

            for (group_addr, state) in iter {
                let QueryReceivedActions { generic, protocol_specific } =
                    state.as_mut().query_received(&mut bindings_ctx.rng(), max_response_time, now);
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

                // NB: Run actions before sending messages, which allows IGMP to
                // understand it should be operating in v1 compatibility mode.
                if let Some(ps_actions) = protocol_specific {
                    core_ctx.run_actions(bindings_ctx, device, ps_actions);
                }
                if let Some(msg) = send_msg {
                    core_ctx.send_message(bindings_ctx, device, group_addr, msg);
                }
            }

            Ok(())
        },
    )
}

pub(super) fn handle_enabled<I, CC, BC, T>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    state: GmpStateRef<'_, I, T, BC>,
) where
    BC: GmpBindingsContext,
    CC: GmpContextInner<I, BC, ProtocolSpecific = T::ProtocolSpecific>,
    I: IpExt,
    T: GmpTypeLayout<I, BC>,
{
    let GmpStateRef { enabled: _, groups, gmp } = state;

    let now = bindings_ctx.now();

    for (group_addr, state) in groups.iter_mut() {
        let group_addr = *group_addr;
        if !I::should_perform_gmp(group_addr) {
            continue;
        }

        let JoinGroupActions { send_report_and_schedule_timer } =
            state.as_mut().join_if_non_member(&mut bindings_ctx.rng(), now);
        let Some((protocol_specific, delay)) = send_report_and_schedule_timer else {
            continue;
        };
        assert_matches!(gmp.timers.schedule_after(bindings_ctx, group_addr, (), delay), None);
        core_ctx.send_message(
            bindings_ctx,
            device,
            group_addr,
            GmpMessageType::Report(protocol_specific),
        );
    }
}

pub(super) fn handle_disabled<I, CC, BC, T>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    state: GmpStateRef<'_, I, T, BC>,
) where
    BC: GmpBindingsContext,
    CC: GmpContextInner<I, BC>,
    I: IpExt,
    T: GmpTypeLayout<I, BC>,
{
    let GmpStateRef { enabled: _, groups, gmp } = state;

    for (group_addr, state) in groups.groups_mut() {
        let LeaveGroupActions { send_leave, stop_timer } = state.as_mut().leave_if_member();
        if stop_timer {
            assert_matches!(gmp.timers.cancel(bindings_ctx, group_addr), Some(_));
        }
        if send_leave {
            core_ctx.send_message(bindings_ctx, device, *group_addr, GmpMessageType::Leave);
        }
    }
}

pub(super) fn join_group<I, CC, BC, T>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
    state: GmpStateRef<'_, I, T, BC>,
) -> GroupJoinResult
where
    BC: GmpBindingsContext,
    CC: GmpContextInner<I, BC, ProtocolSpecific = T::ProtocolSpecific>,
    I: IpExt,
    T: GmpTypeLayout<I, BC>,
{
    let GmpStateRef { enabled, groups, gmp } = state;
    let now = bindings_ctx.now();

    let result = groups.join_group_gmp(
        !enabled || !I::should_perform_gmp(group_addr),
        group_addr,
        &mut bindings_ctx.rng(),
        now,
    );
    result.map(|JoinGroupActions { send_report_and_schedule_timer }| {
        if let Some((protocol_specific, delay)) = send_report_and_schedule_timer {
            assert_matches!(gmp.timers.schedule_after(bindings_ctx, group_addr, (), delay), None);

            core_ctx.send_message(
                bindings_ctx,
                device,
                group_addr,
                GmpMessageType::Report(protocol_specific),
            );
        }
    })
}

pub(super) fn leave_group<I, CC, BC, T>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
    state: GmpStateRef<'_, I, T, BC>,
) -> GroupLeaveResult
where
    BC: GmpBindingsContext,
    CC: GmpContextInner<I, BC>,
    I: IpExt,
    T: GmpTypeLayout<I, BC>,
{
    let GmpStateRef { enabled: _, groups, gmp } = state;
    groups.leave_group_gmp(group_addr).map(|LeaveGroupActions { send_leave, stop_timer }| {
        if stop_timer {
            assert_matches!(gmp.timers.cancel(bindings_ctx, &group_addr), Some(_));
        }
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

    impl ProtocolSpecificTypes for FakeProtocolSpecific {
        /// Tests for generic state machine should not know anything about
        /// protocol specific actions.
        type Actions = Never;

        /// Whether to send leave group message if our flag is not set.
        type Config = bool;
    }

    impl ProtocolSpecific for FakeProtocolSpecific {
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
