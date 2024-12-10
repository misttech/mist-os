// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! GMP v2 common implementation.
//!
//! GMPv2 is the common implementation of a fictitious GMP protocol that covers
//! the common parts of MLDv2 ([RFC 3810]) and IGMPv3 ([RFC 3376]).
//!
//! [RFC 3810]: https://datatracker.ietf.org/doc/html/rfc3810
//! [RFC 3376]: https://datatracker.ietf.org/doc/html/rfc3376

use core::num::NonZeroU8;

use alloc::collections::HashSet;
use const_unwrap::const_unwrap_option;
use core::time::Duration;
use net_types::ip::Ip;
use net_types::{MulticastAddr, Witness as _};
use netstack3_base::Instant as _;
use packet_formats::gmp::GroupRecordType;
use packet_formats::utils::NonZeroDuration;

use crate::internal::gmp::{
    self, GmpBindingsContext, GmpContext, GmpContextInner, GmpGroupState, GmpMode, GmpStateRef,
    GmpTypeLayout, GroupJoinResult, GroupLeaveResult, IpExt, NotAMemberErr, QueryTarget,
};

/// The default value for Query Response Interval defined in [RFC 3810
/// section 9.3] and [RFC 3376 section 8.3].
///
/// [RFC 3810 section 9.3]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-9.3
/// [RFC 3376 section 8.3]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-8.3
pub(super) const DEFAULT_QUERY_RESPONSE_INTERVAL: NonZeroDuration =
    const_unwrap_option(NonZeroDuration::from_secs(10));

/// The default value for the Robustness Variable defined in [RFC 3810
/// section 9.1] and [RFC 3376 section 8.1].
///
/// [RFC 3810 section 9.1]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-9.1
/// [RFC 3376 section 8.1]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-8.1
pub(super) const DEFAULT_ROBUSTNESS_VARIABLE: NonZeroU8 = const_unwrap_option(NonZeroU8::new(2));

/// The default value for the Query Interval defined in [RFC 3810
/// section 9.2] and [RFC 3376 section 8.2].
///
/// [RFC 3810 section 9.2]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-9.2
/// [RFC 3376 section 8.2]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-8.2
pub(super) const DEFAULT_QUERY_INTERVAL: NonZeroDuration =
    const_unwrap_option(NonZeroDuration::from_secs(125));

#[cfg_attr(test, derive(Debug))]
pub(super) struct GroupState<I: Ip> {
    recorded_sources: HashSet<I::Addr>,
}

impl<I: Ip> GroupState<I> {
    pub(super) fn new_for_mode_transition() -> Self {
        Self { recorded_sources: Default::default() }
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub(super) enum TimerId<I: Ip> {
    GeneralQuery,
    MulticastAddress(MulticastAddr<I::Addr>),
}

/// Global protocol state required for v2 support.
///
/// This is kept always available in protocol-global state since we need to
/// store some possibly network-learned values when entering v1 compat mode (for
/// timers).
#[derive(Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub(super) struct ProtocolState {
    /// The robustness variable on the link.
    ///
    /// Defined in [RFC 3810 section 9.1] and [RFC 3376 section 8.1].
    ///
    /// It starts with a default value and may be learned from queriers in the
    /// network.
    ///
    /// [RFC 3810 section 9.1]: https://datatracker.ietf.org/doc/html/rfc3810#section-9.1
    /// [RFC 3376 section 8.1]: https://datatracker.ietf.org/doc/html/rfc3376#section-8.1
    pub robustness_variable: NonZeroU8,
    /// The query interval on the link.
    ///
    /// Defined in [RFC 3810 section 9.2] and [RFC 3376 section 8.2].
    ///
    /// It starts with a default value and may be learned from queriers in the
    /// network.
    ///
    /// [RFC 3810 section 9.2]: https://datatracker.ietf.org/doc/html/rfc3810#section-9.2
    /// [RFC 3376 section 8.2]: https://datatracker.ietf.org/doc/html/rfc3376#section-8.2
    pub query_interval: NonZeroDuration,
}

impl Default for ProtocolState {
    fn default() -> Self {
        Self {
            robustness_variable: DEFAULT_ROBUSTNESS_VARIABLE,
            query_interval: DEFAULT_QUERY_INTERVAL,
        }
    }
}

impl ProtocolState {
    /// Calculates the Older Version Querier Present Timeout.
    ///
    /// From [RFC 3810 section 9.12] and [RFC 3376 section 8.12]:
    ///
    /// > This value MUST be ([Robustness Variable] times (the [Query Interval]
    /// > in the last Query received)) plus ([Query Response Interval]).
    ///
    /// [RFC 3810 section 9.12]: https://datatracker.ietf.org/doc/html/rfc3810#section-9.12
    /// [RFC 3376 section 8.12]: https://datatracker.ietf.org/doc/html/rfc3376#section-8.12
    pub(super) fn older_version_querier_present_timeout<C: ProtocolConfig>(
        &self,
        config: &C,
    ) -> NonZeroDuration {
        self.query_interval
            .saturating_mul(self.robustness_variable.into())
            .saturating_add(config.query_response_interval().into())
    }
}

/// V2 protocol-specific configuration.
///
/// This trait abstracts over the storage of configurations specified in [RFC
/// 3810] and [RFC 3376] that can be administratively changed.
///
/// [RFC 3810]: https://datatracker.ietf.org/doc/html/rfc3810
/// [RFC 3376]: https://datatracker.ietf.org/doc/html/rfc3376
pub trait ProtocolConfig {
    /// The Query Response Interval defined in [RFC 3810 section 9.3] and [RFC
    /// 3376 section 8.3].
    ///
    /// Note that the RFCs mostly define this value in terms of the maximum
    /// response code sent by queriers (routers), but later text references this
    /// configuration to calculate timeouts.
    ///
    /// [RFC 3810 section 9.3]:
    ///     https://datatracker.ietf.org/doc/html/rfc3810#section-9.3
    /// [RFC 3376 section 8.3]:
    ///     https://datatracker.ietf.org/doc/html/rfc3376#section-8.3
    fn query_response_interval(&self) -> NonZeroDuration;
}

/// Trait abstracting a GMPv2 query.
///
/// The getters in this trait represent fields in the membership query messages
/// defined in [RFC 3376 section 4.1] and [RFC 3810 section 5.1].
///
/// [RFC 3376 section 4.1]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-4.1
/// [RFC 3810 section 5.1]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-5.1
pub(super) trait QueryMessage<I: Ip> {
    /// Reinterprets this as a v1 query message.
    fn as_v1(&self) -> impl gmp::v1::QueryMessage<I> + '_;

    /// Gets the Querier's Robustness Variable (QRV).
    fn robustness_variable(&self) -> u8;

    /// Gets the Querier's Query Interval Code (QQIC) interpreted as a duration.
    fn query_interval(&self) -> Duration;

    /// Gets the group address.
    fn group_address(&self) -> I::Addr;

    /// Gets the maximum response time.
    fn max_response_time(&self) -> Duration;

    /// Gets an iterator to the source addresses being queried.
    fn sources(&self) -> impl Iterator<Item = I::Addr> + '_;
}

/// Handles a query message from the network.
///
/// The RFC algorithm is specified on [RFC 3376 section 5.2] and [RFC 3810
/// section 6.2].
///
/// [RFC 3376 section 5.2]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-5.2
/// [RFC 3810 section 6.2]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-6.2
pub(super) fn handle_query_message<
    I: IpExt,
    CC: GmpContext<I, BC>,
    BC: GmpBindingsContext,
    Q: QueryMessage<I>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    query: &Q,
) -> Result<(), NotAMemberErr<I>> {
    core_ctx.with_gmp_state_mut_and_ctx(device, |mut core_ctx, state| {
        match &state.gmp.mode {
            GmpMode::V1 { .. } => {
                return gmp::v1::handle_query_message_inner(
                    &mut core_ctx,
                    bindings_ctx,
                    device,
                    state,
                    &query.as_v1(),
                );
            }
            GmpMode::V2 => {}
        }
        let GmpStateRef { enabled: _, groups, gmp, config: _ } = state;
        // Update parameters if non zero given in query.
        if let Some(qrv) = NonZeroU8::new(query.robustness_variable()) {
            gmp.v2_proto.robustness_variable = qrv;
        }
        if let Some(qqic) = NonZeroDuration::new(query.query_interval()) {
            gmp.v2_proto.query_interval = qqic;
        }

        let target = query.group_address();
        let target = QueryTarget::new(target).ok_or_else(|| NotAMemberErr(target))?;

        // Common early bailout.
        let target = match target {
            // General query.
            QueryTarget::Unspecified => {
                // RFC: When a new valid General Query arrives on an interface,
                // the node checks whether it has any per-interface listening
                // state record to report on, or not.
                if groups.is_empty() {
                    return Ok(());
                }

                // None target from now on marks a general query.
                None
            }
            // Group-Specific or Group-And-Source-Specific query.
            QueryTarget::Specified(multicast_addr) => {
                // RFC: Similarly, when a new valid Multicast Address (and
                // Source) Specific Query arrives on an interface, the node
                // checks whether it has a per-interface listening state record
                // that corresponds to the queried multicast address (and
                // source), or not.

                // TODO(https://fxbug.dev/381241191): We should also consider
                // source lists here when we support SSM.

                let group = groups
                    .get_mut(&multicast_addr)
                    .ok_or_else(|| NotAMemberErr(multicast_addr.get()))?;

                // `Some` target marks a specific query.
                Some((group.v2_mut(), multicast_addr))
            }
        };

        // RFC: If it does, a delay for a response is randomly selected
        // in the range (0, [Maximum Response Delay]).
        let now = bindings_ctx.now();
        let delay = now.saturating_add(gmp::random_report_timeout(
            &mut bindings_ctx.rng(),
            query.max_response_time(),
        ));

        // RFC: If there is a pending response to a previous General Query
        // scheduled sooner than the selected delay, no additional response
        // needs to be scheduled.
        match gmp.timers.get(&TimerId::GeneralQuery.into()) {
            Some((instant, ())) => {
                if instant <= delay {
                    return Ok(());
                }
            }
            None => {}
        }

        let (group, addr) = match target {
            // RFC: If the received Query is a General Query, the Interface
            // Timer is used to schedule a response to the General Query after
            // the selected delay.  Any previously pending response to a General
            // Query is canceled.
            None => {
                let _: Option<_> = gmp.timers.schedule_instant(
                    bindings_ctx,
                    TimerId::GeneralQuery.into(),
                    (),
                    delay,
                );
                return Ok(());
            }
            Some(specific) => specific,
        };

        // The RFC quote for the next part is a bit long-winded but the
        // algorithm is simple. Full quote:
        //
        //  If the received Query is a Multicast Address Specific Query or a
        //  Multicast Address and Source Specific Query and there is no pending
        //  response to a previous Query for this multicast address, then the
        //  Multicast Address Timer is used to schedule a report.  If the
        //  received Query is a Multicast Address and Source Specific Query, the
        //  list of queried sources is recorded to be used when generating a
        //  response.
        //
        //  If there is already a pending response to a previous Query scheduled
        //  for this multicast address, and either the new Query is a Multicast
        //  Address Specific Query or the recorded source list associated with
        //  the multicast address is empty, then the multicast address source
        //  list is cleared and a single response is scheduled, using the
        //  Multicast Address Timer.  The new response is scheduled to be sent
        //  at the earliest of the remaining time for the pending report and the
        //  selected delay.
        //
        //  If the received Query is a Multicast Address and Source Specific
        //  Query and there is a pending response for this multicast address
        //  with a non-empty source list, then the multicast address source list
        //  is augmented to contain the list of sources in the new Query, and a
        //  single response is scheduled using the Multicast Address Timer.  The
        //  new response is scheduled to be sent at the earliest of the
        //  remaining time for the pending report and the selected delay.

        let timer_id = TimerId::MulticastAddress(addr).into();
        let scheduled = gmp.timers.get(&timer_id);
        let mut sources = query.sources().peekable();

        let (delay, clear_sources) = match scheduled {
            // There is a scheduled report.
            Some((t, ())) => {
                // Only reschedule the timer if scheduling for earlier.
                let delay = (delay < t).then_some(delay);
                // Per the second paragraph above, clear sources if address
                // query or if the pending report is already for an empty source
                // list (meaning we don't restrict the old report to the new
                // sources).
                let is_address_query = sources.peek().is_none();
                let clear_sources = group.recorded_sources.is_empty() || is_address_query;
                (delay, clear_sources)
            }
            // No scheduled report, use new delay and record sources.
            None => (Some(delay), false),
        };

        if clear_sources {
            group.recorded_sources = Default::default();
        } else {
            group.recorded_sources.extend(sources);
        }

        if let Some(delay) = delay {
            let _: Option<_> = gmp.timers.schedule_instant(bindings_ctx, timer_id, (), delay);
        }

        Ok(())
    })
}

pub(super) fn join_group<
    I: IpExt,
    CC: GmpContextInner<I, BC>,
    BC: GmpBindingsContext,
    T: GmpTypeLayout<I, BC>,
>(
    _core_ctx: &mut CC,
    _bindings_ctx: &mut BC,
    _device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
    state: GmpStateRef<'_, I, T, BC>,
) -> GroupJoinResult {
    let GmpStateRef { enabled, groups, gmp, config: _ } = state;
    debug_assert!(gmp.mode.is_v2());
    let gmp_enabled = enabled && I::should_perform_gmp(group_addr);
    groups.join_group_with(group_addr, || {
        let state = GroupState { recorded_sources: Default::default() };
        if gmp_enabled {
            // TODO(https://fxbug.dev/42071006): Operate unsolicited reports.
        }

        (GmpGroupState::new_v2(state), ())
    })
}

pub(super) fn leave_group<
    I: IpExt,
    CC: GmpContextInner<I, BC>,
    BC: GmpBindingsContext,
    T: GmpTypeLayout<I, BC>,
>(
    _core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    _device: &CC::DeviceId,
    group_addr: MulticastAddr<I::Addr>,
    state: GmpStateRef<'_, I, T, BC>,
) -> GroupLeaveResult {
    let GmpStateRef { enabled, groups, gmp, config: _ } = state;
    debug_assert!(gmp.mode.is_v2());
    let gmp_enabled = enabled && I::should_perform_gmp(group_addr);
    groups.leave_group(group_addr).map(|_state| {
        // Cancel existing query timers since we've left the group.
        let _: Option<_> =
            gmp.timers.cancel(bindings_ctx, &TimerId::MulticastAddress(group_addr).into());
        if gmp_enabled {
            // TODO(https://fxbug.dev/42071006): Operate unsolicited reports.
        }
    })
}

/// Handles an expire timer.
///
/// The timer expiration algorithm is described in [RFC 3376 section 5.1] and
/// [RFC 3376 section 5.2] for IGMP and [RFC 3810 section 6.3] for MLD.
///
/// [RFC 3376 section 5.1]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-5.1
/// [RFC 3376 section 5.2]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-5.2
/// [RFC 3810 section 6.3]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-6.3
pub(super) fn handle_timer<
    I: IpExt,
    CC: GmpContextInner<I, BC>,
    BC: GmpBindingsContext,
    T: GmpTypeLayout<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    timer: TimerId<I>,
    state: GmpStateRef<'_, I, T, BC>,
) {
    match timer {
        TimerId::GeneralQuery => handle_general_query_timer(core_ctx, bindings_ctx, device, state),
        TimerId::MulticastAddress(multicast_addr) => {
            handle_multicast_address_timer(core_ctx, bindings_ctx, device, multicast_addr, state)
        }
    }
}

/// Handles general query timers.
///
/// Quote from RFC 3810:
///
/// > If the expired timer is the Interface Timer (i.e., there is a pending
/// > response to a General Query), then one Current State Record is sent for
/// > each multicast address for which the specified interface has listening
/// > state [...]. The Current State Record carries the multicast address and
/// > its associated filter mode (MODE_IS_INCLUDE or MODE_IS_EXCLUDE) and Source
/// > list. Multiple Current State Records are packed into individual Report
/// > messages, to the extent possible.
fn handle_general_query_timer<
    I: IpExt,
    CC: GmpContextInner<I, BC>,
    BC: GmpBindingsContext,
    T: GmpTypeLayout<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    state: GmpStateRef<'_, I, T, BC>,
) {
    let GmpStateRef { enabled: _, groups, gmp: _, config: _ } = state;
    let report = groups.iter().map(|(addr, state)| {
        // TODO(https://fxbug.dev/381241191): Update to include SSM in group
        // records.
        let _ = state;

        // Given we don't support SSM, all the groups we're currently joined
        // should be reported in exclude mode with an empty source list.
        //
        // See https://datatracker.ietf.org/doc/html/rfc3810#section-5.2.12 and
        // https://datatracker.ietf.org/doc/html/rfc3376#section-4.2.12 for
        // group record type descriptions.
        (*addr, GroupRecordType::ModeIsExclude, core::iter::empty::<I::Addr>())
    });
    core_ctx.send_report_v2(bindings_ctx, device, report)
}

/// Handles a multicast address timer for `multicast_addr`.
///
/// RFC 3810 quote:
///
/// > If the expired timer is a Multicast Address Timer and the list of recorded
/// > sources for that multicast address is empty (i.e., there is a pending
/// > response to a Multicast Address Specific Query), then if, and only if, the
/// > interface has listening state for that multicast address, a single Current
/// > State Record is sent for that address. The Current State Record carries
/// > the multicast address and its associated filter mode (MODE_IS_INCLUDE or
/// > MODE_IS_EXCLUDE) and source list, if any.
/// >
/// > If the expired timer is a Multicast Address Timer and the list of recorded
/// > sources for that multicast address is non-empty (i.e., there is a pending
/// > response to a Multicast Address and Source Specific Query), then if, and
/// > only if, the interface has listening state for that multicast address, the
/// > contents of the corresponding Current State Record are determined from the
/// > per- interface state and the pending response record, as specified in the
/// > following table:
/// >
/// >                        set of sources in the
/// > per-interface state  pending response record  Current State Record
/// > -------------------  -----------------------  --------------------
/// >  INCLUDE (A)                   B                IS_IN (A*B)
/// >
/// >  EXCLUDE (A)                   B                IS_IN (B-A)
fn handle_multicast_address_timer<
    I: IpExt,
    CC: GmpContextInner<I, BC>,
    BC: GmpBindingsContext,
    T: GmpTypeLayout<I, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device: &CC::DeviceId,
    multicast_addr: MulticastAddr<I::Addr>,
    state: GmpStateRef<'_, I, T, BC>,
) {
    let GmpStateRef { enabled: _, groups, gmp: _, config: _ } = state;
    // Invariant: multicast address timers are removed when we remove interest
    // from the group.
    let state = groups
        .get_mut(&multicast_addr)
        .expect("multicast timer fired for removed address")
        .v2_mut();
    let recorded_sources = core::mem::take(&mut state.recorded_sources);

    let (mode, sources) = if recorded_sources.is_empty() {
        // Multicast Address Specific Query.

        // TODO(https://fxbug.dev/381241191): Update to include SSM-enabled
        // filter mode. For now, ModeIsExclude is all that needs to be reported
        // for any group we're a member of.

        (GroupRecordType::ModeIsExclude, either::Either::Left(core::iter::empty::<&I::Addr>()))
    } else {
        // Multicast Address And Source Specific Query. The mode is always
        // include.

        // TODO(https://fxbug.dev/381241191): Actually calculate set
        // intersection or union when SSM is available.

        (GroupRecordType::ModeIsInclude, either::Either::Right(recorded_sources.iter()))
    };
    core_ctx.send_report_v2(
        bindings_ctx,
        device,
        core::iter::once((multicast_addr, mode, sources)),
    );
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_types::Witness as _;
    use netstack3_base::testutil::{FakeDeviceId, FakeTimerCtxExt, FakeWeakDeviceId};
    use netstack3_base::InstantContext as _;
    use test_case::test_matrix;

    use super::*;
    use crate::gmp::GmpTimerId;
    use crate::internal::gmp::testutil::{self, FakeCtx, FakeV2Query, TestIpExt};
    use crate::internal::gmp::{GmpHandler as _, GroupJoinResult};

    #[derive(Debug, Eq, PartialEq)]
    enum SpecificQuery {
        Multicast,
        MulticastAndSource,
    }

    fn join_and_ignore_unsolicited<I: IpExt>(
        ctx: &mut FakeCtx<I>,
        groups: impl IntoIterator<Item = MulticastAddr<I::Addr>>,
    ) {
        let FakeCtx { core_ctx, bindings_ctx } = ctx;
        for group in groups {
            assert_eq!(
                core_ctx.gmp_join_group(bindings_ctx, &FakeDeviceId, group),
                GroupJoinResult::Joined(())
            );
        }
        while !core_ctx.gmp.timers.is_empty() {
            assert_eq!(
                bindings_ctx.trigger_next_timer(core_ctx),
                Some(GmpTimerId::new(FakeWeakDeviceId(FakeDeviceId)))
            );
        }
        core_ctx.inner.v2_messages.clear();
    }

    #[ip_test(I)]
    fn v2_query_handoff_in_v1_mode<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V1 { compat: true });
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR1),
            GroupJoinResult::Joined(())
        );
        assert_eq!(
            bindings_ctx.trigger_next_timer(&mut core_ctx),
            Some(GmpTimerId::new(FakeWeakDeviceId(FakeDeviceId)))
        );
        // v1 group should be idle now.
        assert_matches!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().v1().get_inner(),
            gmp::v1::MemberState::Idle(_)
        );
        handle_query_message(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &FakeV2Query { group_addr: I::GROUP_ADDR1.get(), ..Default::default() },
        )
        .expect("handle query");
        // v1 group reacts to the query.
        assert_matches!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().v1().get_inner(),
            gmp::v1::MemberState::Delaying(_)
        );
    }

    #[ip_test(I)]
    fn general_query_ignored_if_no_groups<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V2);
        handle_query_message(
            &mut core_ctx,
            &mut bindings_ctx,
            &FakeDeviceId,
            &FakeV2Query { group_addr: I::UNSPECIFIED_ADDRESS, ..Default::default() },
        )
        .expect("handle query");
        assert_eq!(core_ctx.gmp.timers.get(&TimerId::GeneralQuery.into()), None);
    }

    #[ip_test(I)]
    fn query_errors_if_not_multicast<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V2);
        let query = FakeV2Query { group_addr: I::LOOPBACK_ADDRESS.get(), ..Default::default() };
        assert_eq!(
            handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query,),
            Err(NotAMemberErr(query.group_addr))
        );
    }

    #[ip_test(I)]
    fn general_query_scheduled<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V2);
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR1),
            GroupJoinResult::Joined(())
        );
        let query = FakeV2Query { group_addr: I::UNSPECIFIED_ADDRESS, ..Default::default() };

        let general_query_timer = TimerId::GeneralQuery.into();

        handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query)
            .expect("handle query");
        let now = bindings_ctx.now();
        let (scheduled, ()) = core_ctx.gmp.timers.assert_range_single(
            &general_query_timer,
            now..=now.panicking_add(query.max_response_time),
        );

        // Any further queries are ignored  if we have a pending general query
        // in the past.

        // Advance time enough to guarantee we can't pick an earlier time.
        bindings_ctx.timers.instant.sleep(query.max_response_time);

        let query = FakeV2Query { group_addr: I::UNSPECIFIED_ADDRESS, ..Default::default() };
        handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query)
            .expect("handle query");
        assert_eq!(core_ctx.gmp.timers.get(&general_query_timer), Some((scheduled, &())));

        let query = FakeV2Query { group_addr: I::GROUP_ADDR1.get(), ..Default::default() };
        handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query)
            .expect("handle query");
        assert_eq!(
            core_ctx.gmp.timers.get(&TimerId::MulticastAddress(I::GROUP_ADDR1).into()),
            None
        );
    }

    #[ip_test(I)]
    fn specific_query_ignored_if_not_member<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V2);
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR2),
            GroupJoinResult::Joined(())
        );
        let query = FakeV2Query { group_addr: I::GROUP_ADDR1.get(), ..Default::default() };
        assert_eq!(
            handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query),
            Err(NotAMemberErr(query.group_addr))
        );
    }

    #[ip_test(I)]
    fn leave_group_cancels_multicast_address_timer<I: TestIpExt>() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V2);
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR1),
            GroupJoinResult::Joined(())
        );
        let query = FakeV2Query { group_addr: I::GROUP_ADDR1.get(), ..Default::default() };
        handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query)
            .expect("handle query");
        assert_matches!(
            core_ctx.gmp.timers.get(&TimerId::MulticastAddress(I::GROUP_ADDR1).into()),
            Some(_)
        );
        assert_eq!(
            core_ctx.gmp_leave_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR1),
            GroupLeaveResult::Left(())
        );
        assert_matches!(
            core_ctx.gmp.timers.get(&TimerId::MulticastAddress(I::GROUP_ADDR1).into()),
            None
        );
    }

    #[ip_test(I)]
    #[test_matrix(
        [SpecificQuery::Multicast, SpecificQuery::MulticastAndSource],
        [SpecificQuery::Multicast, SpecificQuery::MulticastAndSource]
    )]
    fn schedule_specific_query<I: TestIpExt>(first: SpecificQuery, second: SpecificQuery) {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            testutil::new_context_with_mode::<I>(GmpMode::V2);
        assert_eq!(
            core_ctx.gmp_join_group(&mut bindings_ctx, &FakeDeviceId, I::GROUP_ADDR1),
            GroupJoinResult::Joined(())
        );

        let sources = match first {
            SpecificQuery::Multicast => Default::default(),
            SpecificQuery::MulticastAndSource => {
                (1..3).map(|i| I::get_other_ip_address(i).get()).collect()
            }
        };

        let query1 =
            FakeV2Query { group_addr: I::GROUP_ADDR1.get(), sources, ..Default::default() };
        handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query1)
            .expect("handle query");
        // Sources are recorded.
        assert_eq!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().v2().recorded_sources,
            query1.sources.iter().copied().collect()
        );
        // Timer is scheduled.
        let now = bindings_ctx.now();
        let (scheduled, ()) = core_ctx.gmp.timers.assert_range_single(
            &TimerId::MulticastAddress(I::GROUP_ADDR1).into(),
            now..=now.panicking_add(query1.max_response_time),
        );

        let sources = match second {
            SpecificQuery::Multicast => Default::default(),
            SpecificQuery::MulticastAndSource => {
                (3..5).map(|i| I::get_other_ip_address(i).get()).collect()
            }
        };
        let query2 = FakeV2Query {
            group_addr: I::GROUP_ADDR1.get(),
            // Send a follow up query on a shorter timeline.
            max_response_time: DEFAULT_QUERY_RESPONSE_INTERVAL.get() / 2,
            sources,
            ..Default::default()
        };
        handle_query_message(&mut core_ctx, &mut bindings_ctx, &FakeDeviceId, &query2)
            .expect("handle query");

        let (new_scheduled, ()) = core_ctx.gmp.timers.assert_range_single(
            &TimerId::MulticastAddress(I::GROUP_ADDR1).into(),
            now..=now.panicking_add(query2.max_response_time),
        );
        // Scheduled time is allowed to change, but always to an earlier time.
        assert!(new_scheduled <= scheduled, "{new_scheduled:?} <= {scheduled:?}");
        // Now check the group state.
        let recorded_sources = &core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().v2().recorded_sources;
        match (first, second) {
            (SpecificQuery::Multicast, _) | (_, SpecificQuery::Multicast) => {
                // If any of the queries is multicast-specific then:
                // - Never added any sources.
                // - Newer sources must not override previous
                //   multicast-specific.
                // - New multicast-specific overrides previous sources.
                assert_eq!(recorded_sources, &HashSet::new());
            }
            (SpecificQuery::MulticastAndSource, SpecificQuery::MulticastAndSource) => {
                // List is augmented with the union.
                assert_eq!(
                    recorded_sources,
                    &query1.sources.iter().chain(query2.sources.iter()).copied().collect()
                );
            }
        }
    }

    #[ip_test(I)]
    fn send_general_query_response<I: TestIpExt>() {
        let mut ctx = testutil::new_context_with_mode::<I>(GmpMode::V2);
        join_and_ignore_unsolicited(&mut ctx, [I::GROUP_ADDR1, I::GROUP_ADDR2]);
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        handle_query_message(core_ctx, bindings_ctx, &FakeDeviceId, &FakeV2Query::default())
            .expect("handle query");
        assert_eq!(
            bindings_ctx.trigger_next_timer(core_ctx),
            Some(GmpTimerId::new(FakeWeakDeviceId(FakeDeviceId)))
        );
        assert_eq!(
            core_ctx.inner.v2_messages,
            vec![vec![
                (I::GROUP_ADDR1, GroupRecordType::ModeIsExclude, vec![]),
                (I::GROUP_ADDR2, GroupRecordType::ModeIsExclude, vec![]),
            ]]
        );
    }

    #[ip_test(I)]
    fn send_multicast_address_specific_query_response<I: TestIpExt>() {
        let mut ctx = testutil::new_context_with_mode::<I>(GmpMode::V2);
        join_and_ignore_unsolicited(&mut ctx, [I::GROUP_ADDR1]);
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        handle_query_message(
            core_ctx,
            bindings_ctx,
            &FakeDeviceId,
            &FakeV2Query { group_addr: I::GROUP_ADDR1.get(), ..Default::default() },
        )
        .expect("handle query");
        assert_eq!(
            bindings_ctx.trigger_next_timer(core_ctx),
            Some(GmpTimerId::new(FakeWeakDeviceId(FakeDeviceId)))
        );
        assert_eq!(
            core_ctx.inner.v2_messages,
            vec![vec![(I::GROUP_ADDR1, GroupRecordType::ModeIsExclude, vec![])]]
        );
    }

    #[ip_test(I)]
    fn send_multicast_address_and_source_specific_query_response<I: TestIpExt>() {
        let mut ctx = testutil::new_context_with_mode::<I>(GmpMode::V2);
        join_and_ignore_unsolicited(&mut ctx, [I::GROUP_ADDR1]);
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        let query = FakeV2Query {
            group_addr: I::GROUP_ADDR1.get(),
            sources: vec![I::get_other_ip_address(1).get(), I::get_other_ip_address(2).get()],
            ..Default::default()
        };
        handle_query_message(core_ctx, bindings_ctx, &FakeDeviceId, &query).expect("handle query");
        assert_eq!(
            bindings_ctx.trigger_next_timer(core_ctx),
            Some(GmpTimerId::new(FakeWeakDeviceId(FakeDeviceId)))
        );
        assert_eq!(
            core_ctx.inner.v2_messages,
            vec![vec![(I::GROUP_ADDR1, GroupRecordType::ModeIsInclude, query.sources)]]
        );
    }
}
