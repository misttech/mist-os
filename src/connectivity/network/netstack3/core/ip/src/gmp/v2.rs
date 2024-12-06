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

use const_unwrap::const_unwrap_option;
use net_types::ip::Ip;
use packet_formats::utils::NonZeroDuration;

use crate::internal::gmp::{self, GmpBindingsContext, GmpContext, GmpMode, IpExt, NotAMemberErr};

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
const DEFAULT_ROBUSTNESS_VARIABLE: NonZeroU8 = const_unwrap_option(NonZeroU8::new(2));

/// The default value for the Query Interval defined in [RFC 3810
/// section 9.2] and [RFC 3376 section 8.2].
///
/// [RFC 3810 section 9.2]:
///     https://datatracker.ietf.org/doc/html/rfc3810#section-9.2
/// [RFC 3376 section 8.2]:
///     https://datatracker.ietf.org/doc/html/rfc3376#section-8.2
const DEFAULT_QUERY_INTERVAL: NonZeroDuration =
    const_unwrap_option(NonZeroDuration::from_secs(125));

#[cfg_attr(test, derive(Debug))]
pub(super) struct GroupState;

impl GroupState {
    pub(super) fn new_for_mode_transition() -> Self {
        Self
    }
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
pub(super) trait QueryMessage<I: Ip> {
    /// Reinterprets this as a v1 query message.
    fn as_v1(&self) -> impl gmp::v1::QueryMessage<I> + '_;
}

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
        // TODO(https://fxbug.dev/42071006): Handle v2 queries.
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_types::Witness as _;
    use netstack3_base::testutil::{FakeDeviceId, FakeTimerCtxExt, FakeWeakDeviceId};

    use super::*;
    use crate::gmp::GmpTimerId;
    use crate::internal::gmp::testutil::{self, FakeCtx, FakeV2Query, TestIpExt};
    use crate::internal::gmp::{GmpHandler as _, GroupJoinResult};

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
            &FakeV2Query {
                group_addr: I::GROUP_ADDR1.get(),
                max_response_time: Duration::from_secs(1),
            },
        )
        .expect("handle query");
        // v1 group reacts to the query.
        assert_matches!(
            core_ctx.groups.get(&I::GROUP_ADDR1).unwrap().v1().get_inner(),
            gmp::v1::MemberState::Delaying(_)
        );
    }
}
