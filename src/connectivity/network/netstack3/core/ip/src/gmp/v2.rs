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
use packet_formats::utils::NonZeroDuration;

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
