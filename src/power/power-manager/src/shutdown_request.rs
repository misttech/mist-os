// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::marker::SourceBreaking;
use {
    fidl_fuchsia_hardware_power_statecontrol as fpower,
    fidl_fuchsia_system_state as fdevice_manager,
};

/// Represents the available shutdown types of the system. These are intended to mirror the
/// supported shutdown APIs of fuchsia.hardware.power.statecontrol.Admin.
#[derive(Debug, PartialEq, PartialOrd, Clone)]
pub enum ShutdownRequest {
    PowerOff,
    Reboot(RebootReasons),
    RebootBootloader,
    RebootRecovery,
    SuspendToRam,
}

/// The reasons of a reboot.
///
/// This type acts as a witness that the provided reasons are valid (i.e. at
/// least one reason was provided).
#[derive(Debug, PartialEq, PartialOrd, Clone)]
pub struct RebootReasons(Vec<fpower::RebootReason2>);

impl RebootReasons {
    /// Construct a new `RebootReasons` with the given reason.
    pub fn new(reason: fpower::RebootReason2) -> Self {
        Self(vec![reason])
    }

    /// Construct a new `RebootReasons` from the given deprecated
    /// `RebootReason`.
    // TODO(https://fxbug.dev/385742868): Remove this function once
    // `RebootReason` is removed from the API.
    pub(crate) fn from_deprecated(reason: &fpower::RebootReason) -> Self {
        let reason = match reason {
            fpower::RebootReason::UserRequest => fpower::RebootReason2::UserRequest,
            fpower::RebootReason::SystemUpdate => fpower::RebootReason2::SystemUpdate,
            fpower::RebootReason::RetrySystemUpdate => fpower::RebootReason2::RetrySystemUpdate,
            fpower::RebootReason::HighTemperature => fpower::RebootReason2::HighTemperature,
            fpower::RebootReason::FactoryDataReset => fpower::RebootReason2::FactoryDataReset,
            fpower::RebootReason::SessionFailure => fpower::RebootReason2::SessionFailure,
            fpower::RebootReason::SysmgrFailure => fpower::RebootReason2::SysmgrFailure,
            fpower::RebootReason::CriticalComponentFailure => {
                fpower::RebootReason2::CriticalComponentFailure
            }
            fpower::RebootReason::ZbiSwap => fpower::RebootReason2::ZbiSwap,
            fpower::RebootReason::OutOfMemory => fpower::RebootReason2::OutOfMemory,
        };
        Self::new(reason)
    }

    /// Convert this set of `RebootReasons` into a deprecated `RebootReason`.
    /// It's a backwards compatible implementation.
    /// * If multiple `RebootReason2` are provided, prefer reasons with an
    ///   equivalent deprecated `RebootReason` representation.
    /// * Then, if multiple reasons are provided, prefer the first.
    /// * Then, if the reason has no equivalent deprecated `RebootReason`, do a
    ///   best-effort translation.
    // TODO(https://fxbug.dev/385742868): Remove this function once
    // `RebootReason` is removed from the API.
    pub(crate) fn to_deprecated(&self) -> fpower::RebootReason {
        enum FoldState {
            Direct(fpower::RebootReason),
            Indirect(fpower::RebootReason),
            None,
        }
        let state = self.0.iter().fold(FoldState::None, |state, reason| {
            match (&state, &reason) {
                // We already have a direct state; keep it.
                (FoldState::Direct(_), _) => state,
                // For reasons that have a direct backwards translation, use it.
                (_, fpower::RebootReason2::UserRequest) => {
                    FoldState::Direct(fpower::RebootReason::UserRequest)
                }
                (_, fpower::RebootReason2::SystemUpdate) => {
                    FoldState::Direct(fpower::RebootReason::SystemUpdate)
                }
                (_, fpower::RebootReason2::RetrySystemUpdate) => {
                    FoldState::Direct(fpower::RebootReason::RetrySystemUpdate)
                }
                (_, fpower::RebootReason2::HighTemperature) => {
                    FoldState::Direct(fpower::RebootReason::HighTemperature)
                }
                (_, fpower::RebootReason2::FactoryDataReset) => {
                    FoldState::Direct(fpower::RebootReason::FactoryDataReset)
                }
                (_, fpower::RebootReason2::SessionFailure) => {
                    FoldState::Direct(fpower::RebootReason::SessionFailure)
                }
                (_, fpower::RebootReason2::SysmgrFailure) => {
                    FoldState::Direct(fpower::RebootReason::SysmgrFailure)
                }
                (_, fpower::RebootReason2::CriticalComponentFailure) => {
                    FoldState::Direct(fpower::RebootReason::CriticalComponentFailure)
                }
                (_, fpower::RebootReason2::ZbiSwap) => {
                    FoldState::Direct(fpower::RebootReason::ZbiSwap)
                }
                (_, fpower::RebootReason2::OutOfMemory) => {
                    FoldState::Direct(fpower::RebootReason::OutOfMemory)
                }
                // If we already have an indirect reason, don't overwrite it
                // with a new indirect reason.
                (FoldState::Indirect(_), fpower::RebootReason2::NetstackMigration) => state,
                // Translate `NetstackMigration` to `SystemUpdate`.
                (FoldState::None, fpower::RebootReason2::NetstackMigration) => {
                    FoldState::Indirect(fpower::RebootReason::SystemUpdate)
                }
                (_, fpower::RebootReason2::__SourceBreaking { unknown_ordinal: _ }) => {
                    unreachable!()
                }
            }
        });
        match state {
            FoldState::Direct(reason) | FoldState::Indirect(reason) => reason,
            FoldState::None => {
                unreachable!("RebootReasons is guaranteed to have at least 1 reason.")
            }
        }
    }
}

impl AsRef<Vec<fpower::RebootReason2>> for RebootReasons {
    fn as_ref(&self) -> &Vec<fpower::RebootReason2> {
        &self.0
    }
}

impl From<RebootReasons> for fpower::RebootOptions {
    fn from(RebootReasons(reasons): RebootReasons) -> Self {
        fpower::RebootOptions { reasons: Some(reasons), __source_breaking: SourceBreaking }
    }
}

/// The reasons a `fpower::RebootOptions` may be invalid.
#[derive(Debug, PartialEq)]
pub enum InvalidRebootOptions {
    /// No reasons were provided.
    NoReasons,
}

impl TryFrom<fpower::RebootOptions> for RebootReasons {
    type Error = InvalidRebootOptions;
    fn try_from(options: fpower::RebootOptions) -> Result<Self, Self::Error> {
        let fpower::RebootOptions { reasons, __source_breaking } = options;
        if let Some(reasons) = reasons {
            if !reasons.is_empty() {
                return Ok(RebootReasons(reasons));
            }
        }

        Err(InvalidRebootOptions::NoReasons)
    }
}

/// Converts a ShutdownRequest into a fuchsia.hardware.power.statecontrol.SystemPowerState value.
impl Into<fdevice_manager::SystemPowerState> for ShutdownRequest {
    fn into(self) -> fdevice_manager::SystemPowerState {
        match self {
            ShutdownRequest::PowerOff => fdevice_manager::SystemPowerState::Poweroff,
            ShutdownRequest::Reboot(reasons) => {
                if reasons.as_ref().contains(&fpower::RebootReason2::OutOfMemory) {
                    fdevice_manager::SystemPowerState::RebootKernelInitiated
                } else {
                    fdevice_manager::SystemPowerState::Reboot
                }
            }
            ShutdownRequest::RebootBootloader => {
                fdevice_manager::SystemPowerState::RebootBootloader
            }
            ShutdownRequest::RebootRecovery => fdevice_manager::SystemPowerState::RebootRecovery,
            ShutdownRequest::SuspendToRam => fdevice_manager::SystemPowerState::SuspendRam,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(None => Err(InvalidRebootOptions::NoReasons); "no_reasons")]
    #[test_case(Some(vec![]) => Err(InvalidRebootOptions::NoReasons); "empty_reasons")]
    #[test_case(Some(vec![fpower::RebootReason2::UserRequest]) => Ok(()); "success")]
    fn reboot_reasons(
        reasons: Option<Vec<fpower::RebootReason2>>,
    ) -> Result<(), InvalidRebootOptions> {
        let options = fpower::RebootOptions { reasons, __source_breaking: SourceBreaking };
        RebootReasons::try_from(options).map(|_reasons| {})
    }

    #[test_case(
        vec![fpower::RebootReason2::UserRequest, fpower::RebootReason2::SystemUpdate] =>
        fpower::RebootReason::UserRequest;
        "prefer_first_a")]
    #[test_case(
        vec![fpower::RebootReason2::SystemUpdate, fpower::RebootReason2::UserRequest] =>
        fpower::RebootReason::SystemUpdate;
        "prefer_first_b")]
    #[test_case(
        vec![fpower::RebootReason2::NetstackMigration, fpower::RebootReason2::UserRequest] =>
        fpower::RebootReason::UserRequest;
        "prefer_direct")]
    #[test_case(
        vec![fpower::RebootReason2::NetstackMigration] =>
        fpower::RebootReason::SystemUpdate;
        "netstack_migration")]
    fn reasons_to_deprecated(reasons: Vec<fpower::RebootReason2>) -> fpower::RebootReason {
        let options =
            fpower::RebootOptions { reasons: Some(reasons), __source_breaking: SourceBreaking };
        RebootReasons::try_from(options).unwrap().to_deprecated()
    }
}
