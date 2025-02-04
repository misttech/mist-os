// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use log::info;

use crate::NetstackVersion;

/// The number of failed healthchecks before we decide Netstack3 isn't working
/// and should roll back to Netstack2.
#[allow(dead_code)]
const MAX_FAILED_HEALTHCHECKS: usize = 5;

/// The in-memory state machine for the Netstack3 rollback system.
///
/// All update methods return a modified [`State`] for easier testing.
///
/// Communication with other systems (e.g. performing healthchecks and
/// scheduling reboots) must be handled by a higher-level system.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum State {
    /// We are currently running Netstack2, so no functionality should be
    /// enabled for the current boot.
    ///
    /// This state is entered at boot and will never be left.
    Netstack2,

    /// We are running Netstack3 and should check whether connectivity is live.
    ///
    /// If the number of failed checks reaches [`MAX_FAILED_HEALTHCHECKS`], a
    /// reboot is scheduled and the next boot will be forced to use Netstack2 in
    /// order to regain connectivity.
    ///
    /// State transitions:
    ///
    /// - On a successful healthcheck, transition to Success.
    /// - On a failed healthchek, add one to the number of failed checks, and
    ///   re-enter Checking.
    /// - When the desired netstack becomes Netstack2, enter Canceled.
    Checking(usize),

    /// The migration was cancelled while we were already running Netstack3.
    ///
    /// State transitions depend on the inner value. This is to handle the case
    /// where the desired netstack version returns to Netstack3 after the
    /// migration is canceled.
    ///
    /// - When the desired netstack becomes Netstack3:
    ///   - If [`Canceled::FromChecking`], return to Checking with the contained value.
    ///   - If [`Canceled::FromSuccess`], return to Success.
    Canceled(Canceled),

    /// Netstack3 healthchecked successfully and so we assume we can safely
    /// continue running Netstack3.
    ///
    /// State transitions:
    ///
    /// - When the desired netstack becomes Netstack2, enter Canceled.
    Success,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Canceled {
    FromChecking(usize),
    FromSuccess,
}

#[allow(dead_code)]
impl State {
    /// Create a new state based on the persisted rollback state as well as the
    /// currently-running Netstack version.
    fn new(persisted: Option<Persisted>, current_boot: NetstackVersion) -> Self {
        match (persisted, current_boot) {
            (_, NetstackVersion::Netstack2) => State::Netstack2,
            (None, NetstackVersion::Netstack3) => State::Checking(0),
            (Some(Persisted::HealthcheckFailures(failures)), NetstackVersion::Netstack3) => {
                State::Checking(failures)
            }
            (Some(Persisted::Success), NetstackVersion::Netstack3) => State::Success,
        }
    }

    /// Called when a new desired netstack version is selected. Returns the new
    /// [`State`].
    fn on_desired_version_change(self, desired_version: NetstackVersion) -> Self {
        let old = self.clone();
        let new = match (self, desired_version) {
            (State::Netstack2, _) => self,

            (State::Checking(failures), NetstackVersion::Netstack2) => {
                State::Canceled(Canceled::FromChecking(failures))
            }
            (State::Checking(_), NetstackVersion::Netstack3) => self,

            (State::Success, NetstackVersion::Netstack2) => State::Canceled(Canceled::FromSuccess),
            (State::Success, NetstackVersion::Netstack3) => self,

            (State::Canceled(_), NetstackVersion::Netstack2) => self,
            (State::Canceled(inner), NetstackVersion::Netstack3) => match inner {
                Canceled::FromChecking(failures) => State::Checking(failures),
                Canceled::FromSuccess => State::Success,
            },
        };

        if new != old {
            info!("on_desired_version_change: Rollback state changed from {old:?} to {new:?}");
        }
        new
    }

    /// Called after a healthcheck. Returns the new [`State`].
    fn on_healthcheck(self, result: HealthcheckResult) -> Self {
        let old = self.clone();
        let new = match self {
            // None of these should be reachable in practice.
            State::Netstack2 | State::Success | State::Canceled(_) => self,

            State::Checking(failures) => match result {
                HealthcheckResult::Success => State::Success,
                HealthcheckResult::Failure => State::Checking(failures + 1),
            },
        };

        if new != old {
            info!("on_healthcheck: Rollback state changed from {old:?} to {new:?}");
        }
        new
    }

    fn should_healthcheck(&self) -> bool {
        match self {
            State::Checking(_) => true,
            State::Netstack2 | State::Success | State::Canceled(_) => false,
        }
    }

    /// Transforms the in-memory state into what should be persisted to disk.
    fn persisted(&self) -> Persisted {
        match self {
            State::Netstack2 => Persisted::HealthcheckFailures(0),
            State::Checking(failures) => Persisted::HealthcheckFailures(*failures),

            State::Success => Persisted::Success,
            State::Canceled(_) => Persisted::HealthcheckFailures(0),
        }
    }
}

/// A very simplified version of the in-memory state that's persisted to disk.
#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Persisted {
    /// We have attempted to check connectivity while running Netstack3 this
    /// many times without falling back to Netstack2.
    HealthcheckFailures(usize),

    /// We successfully healthchecked against Netstack3 and will no longer
    /// perform a rollback.
    Success,
}

#[allow(dead_code)]
enum HealthcheckResult {
    Success,
    Failure,
}

#[cfg(test)]
mod test {
    use super::*;

    use test_case::test_case;

    use crate::NetstackVersion;

    #[test_case(None, NetstackVersion::Netstack2 => State::Netstack2)]
    #[test_case(None, NetstackVersion::Netstack3 => State::Checking(0))]
    #[test_case(
        Some(Persisted::HealthcheckFailures(10)),
        NetstackVersion::Netstack2 => State::Netstack2
    )]
    #[test_case(
        Some(Persisted::HealthcheckFailures(10)),
        NetstackVersion::Netstack3 => State::Checking(10)
    )]
    #[test_case(Some(Persisted::Success), NetstackVersion::Netstack2 => State::Netstack2)]
    #[test_case(Some(Persisted::Success), NetstackVersion::Netstack3 => State::Success)]
    fn test_state_construction(
        persisted: Option<Persisted>,
        current_boot: NetstackVersion,
    ) -> State {
        State::new(persisted, current_boot)
    }

    #[test_case(State::Netstack2, NetstackVersion::Netstack2 => State::Netstack2)]
    #[test_case(State::Netstack2, NetstackVersion::Netstack3 => State::Netstack2)]
    #[test_case(
        State::Checking(1),
        NetstackVersion::Netstack2 => State::Canceled(Canceled::FromChecking(1)))]
    #[test_case(
        State::Checking(1),
        NetstackVersion::Netstack3 => State::Checking(1))]
    #[test_case(
        State::Checking(MAX_FAILED_HEALTHCHECKS+1),
        NetstackVersion::Netstack2 =>
            State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1))
    )]
    #[test_case(
        State::Checking(MAX_FAILED_HEALTHCHECKS+1),
        NetstackVersion::Netstack3 => State::Checking(MAX_FAILED_HEALTHCHECKS+1)
    )]
    #[test_case(
        State::Canceled(Canceled::FromChecking(1)),
        NetstackVersion::Netstack2 => State::Canceled(Canceled::FromChecking(1)))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(1)),
        NetstackVersion::Netstack3 => State::Checking(1))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)),
        NetstackVersion::Netstack2 =>
            State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1))
    )]
    #[test_case(
        State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)),
        NetstackVersion::Netstack3 => State::Checking(MAX_FAILED_HEALTHCHECKS+1)
    )]
    #[test_case(
        State::Canceled(Canceled::FromSuccess),
        NetstackVersion::Netstack2 => State::Canceled(Canceled::FromSuccess))]
    #[test_case(
        State::Canceled(Canceled::FromSuccess),
        NetstackVersion::Netstack3 => State::Success)]
    #[test_case(
        State::Success,
        NetstackVersion::Netstack2 => State::Canceled(Canceled::FromSuccess))]
    #[test_case(State::Success, NetstackVersion::Netstack3 => State::Success)]
    fn test_on_desired_version_change(state: State, desired_version: NetstackVersion) -> State {
        state.on_desired_version_change(desired_version)
    }

    #[test_case(State::Netstack2, HealthcheckResult::Success => State::Netstack2)]
    #[test_case(State::Netstack2, HealthcheckResult::Failure => State::Netstack2)]
    #[test_case(State::Checking(1), HealthcheckResult::Success => State::Success)]
    #[test_case(State::Checking(1), HealthcheckResult::Failure => State::Checking(2))]
    #[test_case(
        State::Checking(MAX_FAILED_HEALTHCHECKS+1),
        HealthcheckResult::Success => State::Success)]
    #[test_case(
        State::Checking(MAX_FAILED_HEALTHCHECKS+1),
        HealthcheckResult::Failure => State::Checking(MAX_FAILED_HEALTHCHECKS+2))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(1)),
        HealthcheckResult::Success => State::Canceled(Canceled::FromChecking(1)))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(1)),
        HealthcheckResult::Failure => State::Canceled(Canceled::FromChecking(1)))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)),
        HealthcheckResult::Success =>
            State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)),
        HealthcheckResult::Failure =>
            State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)))]
    #[test_case(
        State::Canceled(Canceled::FromSuccess),
        HealthcheckResult::Success => State::Canceled(Canceled::FromSuccess))]
    #[test_case(
        State::Canceled(Canceled::FromSuccess),
        HealthcheckResult::Failure => State::Canceled(Canceled::FromSuccess))]
    #[test_case(State::Success, HealthcheckResult::Success => State::Success)]
    #[test_case(State::Success, HealthcheckResult::Failure => State::Success)]
    fn test_on_healthcheck(state: State, helthcheck_result: HealthcheckResult) -> State {
        state.on_healthcheck(helthcheck_result)
    }

    #[test_case(State::Netstack2 => false)]
    #[test_case(State::Checking(1) => true)]
    #[test_case(State::Checking(MAX_FAILED_HEALTHCHECKS+1) => true)]
    #[test_case(State::Canceled(Canceled::FromChecking(1)) => false)]
    #[test_case(State::Canceled(Canceled::FromSuccess) => false)]
    #[test_case(State::Success => false)]
    fn test_should_healthcheck(state: State) -> bool {
        state.should_healthcheck()
    }

    #[test_case(
        State::Netstack2 =>
            Persisted::HealthcheckFailures(0))]
    #[test_case(
        State::Checking(1) =>
            Persisted::HealthcheckFailures(1))]
    #[test_case(
        State::Checking(MAX_FAILED_HEALTHCHECKS+1) =>
            Persisted::HealthcheckFailures(MAX_FAILED_HEALTHCHECKS+1))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(1)) =>
            Persisted::HealthcheckFailures(0))]
    #[test_case(
        State::Canceled(Canceled::FromChecking(MAX_FAILED_HEALTHCHECKS+1)) =>
            Persisted::HealthcheckFailures(0))]
    #[test_case(
        State::Canceled(Canceled::FromSuccess) =>
            Persisted::HealthcheckFailures(0))]
    #[test_case(
        State::Success =>
            Persisted::Success)]
    fn test_persisted(state: State) -> Persisted {
        state.persisted()
    }
}
