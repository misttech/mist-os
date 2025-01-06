// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::configuration::Configuration;
use super::errors::{
    BootManagerError, BootManagerResultExt, PolicyError, VerifyError, VerifyErrors, VerifySource,
};
use crate::config::{Config as ComponentConfig, Mode};
use fidl_fuchsia_paver as paver;
use log::{info, warn};
use zx::Status;

/// After gathering state from the BootManager, the PolicyEngine can answer whether we
/// should verify and commit.
#[derive(Debug)]
pub struct PolicyEngine(State);

#[derive(Debug)]
enum State {
    // If no verification or committing is necessary, i.e. if any of:
    //   * ABR is not supported
    //   * the current config is Recovery
    //   * the current config status is Healthy
    NoOp,
    Active {
        current_config: Configuration,
        // None if the value is erroneously missing from QueryConfigurationStatusAndBootAttempts.
        boot_attempts: Option<u8>,
    },
}

impl PolicyEngine {
    /// Gathers system state from the BootManager.
    pub async fn build(boot_manager: &paver::BootManagerProxy) -> Result<Self, PolicyError> {
        let current_config = match boot_manager
            .query_current_configuration()
            .await
            .into_boot_manager_result("query_current_configuration")
        {
            Err(BootManagerError::Fidl {
                error: fidl::Error::ClientChannelClosed { status: Status::NOT_SUPPORTED, .. },
                ..
            }) => {
                info!("ABR not supported: skipping health verification and boot metadata updates");
                return Ok(Self(State::NoOp));
            }
            Err(e) => return Err(PolicyError::Build(e)),
            Ok(paver::Configuration::Recovery) => {
                info!("System in recovery: skipping health verification and boot metadata updates");
                return Ok(Self(State::NoOp));
            }
            Ok(paver::Configuration::A) => Configuration::A,
            Ok(paver::Configuration::B) => Configuration::B,
        };

        let status_and_boot_attempts = boot_manager
            .query_configuration_status_and_boot_attempts((&current_config).into())
            .await
            .into_boot_manager_result("query_configuration_status")
            .map_err(PolicyError::Build)?;
        match status_and_boot_attempts
            .status
            .ok_or(PolicyError::Build(BootManagerError::StatusNotSet))?
        {
            paver::ConfigurationStatus::Healthy => {
                return Ok(Self(State::NoOp));
            }
            paver::ConfigurationStatus::Pending => {}
            paver::ConfigurationStatus::Unbootable => {
                return Err(PolicyError::CurrentConfigurationUnbootable((&current_config).into()));
            }
        };

        let boot_attempts = status_and_boot_attempts.boot_attempts;
        if boot_attempts.is_none() {
            warn!("Current config status is pending but boot attempts was not set");
        }

        Ok(Self(State::Active { current_config, boot_attempts }))
    }

    /// Determines if we should verify and commit.
    /// * If we should (e.g. if the system is pending commit), return
    ///   `Some((slot_to_act_on, boot_attempts))`.
    /// * If we shouldn't (e.g. if the system is already committed), return `None`.
    pub fn should_verify_and_commit(&self) -> Option<(&Configuration, Option<u8>)> {
        match &self.0 {
            State::Active { current_config, boot_attempts } => {
                Some((current_config, *boot_attempts))
            }
            State::NoOp => None,
        }
    }

    /// Filters out any failed verifications if the config says to ignore them.
    pub fn apply_config(
        res: Result<(), VerifyErrors>,
        config: &ComponentConfig,
    ) -> Result<(), VerifyErrors> {
        match res {
            Ok(()) => Ok(()),
            Err(VerifyErrors::VerifyErrors(v)) => {
                // For any existing verification errors,
                let errors: Vec<_> = v
                    .into_iter()
                    .filter(|VerifyError::VerifyError(source, _, _)| {
                        // filter out the ones which config says to ignore.
                        match source {
                            VerifySource::Blobfs => config.blobfs() != &Mode::Ignore,
                            VerifySource::Netstack => config.netstack() != &Mode::Ignore,
                        }
                    })
                    .collect();

                // If there are any remaining verification errors, pass them on.
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(VerifyErrors::VerifyErrors(errors))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::errors::VerifyFailureReason;
    use assert_matches::assert_matches;
    use mock_paver::{hooks as mphooks, MockPaverServiceBuilder, PaverEvent};
    use std::sync::Arc;
    use {fidl_fuchsia_update_verify as verify, fuchsia_async as fasync};

    /// Test we should NOT verify and commit when when the device is in recovery.
    #[fasync::run_singlethreaded(test)]
    async fn test_skip_when_device_in_recovery() {
        let paver = Arc::new(
            MockPaverServiceBuilder::new()
                .current_config(paver::Configuration::Recovery)
                .insert_hook(mphooks::config_status_and_boot_attempts(|_| {
                    Ok((paver::ConfigurationStatus::Healthy, None))
                }))
                .build(),
        );
        let engine = PolicyEngine::build(&paver.spawn_boot_manager_service()).await.unwrap();

        assert_eq!(engine.should_verify_and_commit(), None);

        assert_eq!(paver.take_events(), vec![PaverEvent::QueryCurrentConfiguration]);
    }

    /// Test we should NOT verify and commit when the device does not support ABR.
    #[fasync::run_singlethreaded(test)]
    async fn test_skip_when_device_does_not_support_abr() {
        let paver = Arc::new(
            MockPaverServiceBuilder::new()
                .boot_manager_close_with_epitaph(Status::NOT_SUPPORTED)
                .build(),
        );
        let engine = PolicyEngine::build(&paver.spawn_boot_manager_service()).await.unwrap();

        assert_eq!(engine.should_verify_and_commit(), None);

        assert_eq!(paver.take_events(), vec![]);
    }

    /// Helper fn to verify we should NOT verify and commit when current is healthy.
    async fn test_skip_when_current_is_healthy(current_config: &Configuration) {
        let paver = Arc::new(
            MockPaverServiceBuilder::new()
                .current_config(current_config.into())
                .insert_hook(mphooks::config_status_and_boot_attempts(|_| {
                    Ok((paver::ConfigurationStatus::Healthy, None))
                }))
                .build(),
        );
        let engine = PolicyEngine::build(&paver.spawn_boot_manager_service()).await.unwrap();

        assert_eq!(engine.should_verify_and_commit(), None);

        assert_eq!(
            paver.take_events(),
            vec![
                PaverEvent::QueryCurrentConfiguration,
                PaverEvent::QueryConfigurationStatusAndBootAttempts {
                    configuration: current_config.into()
                },
            ]
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_skip_when_current_is_healthy_a() {
        test_skip_when_current_is_healthy(&Configuration::A).await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_skip_when_current_is_healthy_b() {
        test_skip_when_current_is_healthy(&Configuration::B).await
    }

    /// Helper fn to verify we should verify and commit when current is pending.
    async fn test_verify_and_commit_when_current_is_pending(current_config: &Configuration) {
        let paver = Arc::new(
            MockPaverServiceBuilder::new()
                .current_config(current_config.into())
                .insert_hook(mphooks::config_status_and_boot_attempts(|_| {
                    Ok((paver::ConfigurationStatus::Pending, Some(1)))
                }))
                .build(),
        );
        let engine = PolicyEngine::build(&paver.spawn_boot_manager_service()).await.unwrap();

        assert_eq!(engine.should_verify_and_commit(), Some((current_config, Some(1))));

        assert_eq!(
            paver.take_events(),
            vec![
                PaverEvent::QueryCurrentConfiguration,
                PaverEvent::QueryConfigurationStatusAndBootAttempts {
                    configuration: current_config.into()
                },
            ]
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_verify_and_commit_when_current_is_pending_a() {
        test_verify_and_commit_when_current_is_pending(&Configuration::A).await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_verify_and_commit_when_current_is_pending_b() {
        test_verify_and_commit_when_current_is_pending(&Configuration::B).await
    }

    /// Helper fn to verify an error is returned if current is unbootable.
    async fn test_returns_error_when_current_unbootable(current_config: &Configuration) {
        let paver = Arc::new(
            MockPaverServiceBuilder::new()
                .current_config(current_config.into())
                .insert_hook(mphooks::config_status_and_boot_attempts(|_| {
                    Ok((paver::ConfigurationStatus::Unbootable, None))
                }))
                .build(),
        );

        assert_matches!(
            PolicyEngine::build(&paver.spawn_boot_manager_service()).await,
            Err(PolicyError::CurrentConfigurationUnbootable(cc)) if cc == current_config.into()
        );

        assert_eq!(
            paver.take_events(),
            vec![
                PaverEvent::QueryCurrentConfiguration,
                PaverEvent::QueryConfigurationStatusAndBootAttempts {
                    configuration: current_config.into()
                },
            ]
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_returns_error_when_current_unbootable_a() {
        test_returns_error_when_current_unbootable(&Configuration::A).await
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_returns_error_when_current_unbootable_b() {
        test_returns_error_when_current_unbootable(&Configuration::B).await
    }

    /// Test the build fn fails on a standard paver error.
    #[fasync::run_singlethreaded(test)]
    async fn test_build_fails_when_paver_fails() {
        let paver = Arc::new(
            MockPaverServiceBuilder::new()
                .insert_hook(mphooks::return_error(|e| match e {
                    PaverEvent::QueryCurrentConfiguration { .. } => Status::OUT_OF_RANGE,
                    _ => Status::OK,
                }))
                .build(),
        );

        assert_matches!(
            PolicyEngine::build(&paver.spawn_boot_manager_service()).await,
            Err(PolicyError::Build(BootManagerError::Status {
                method_name: "query_current_configuration",
                status: Status::OUT_OF_RANGE
            }))
        );

        assert_eq!(paver.take_events(), vec![PaverEvent::QueryCurrentConfiguration]);
    }

    fn test_blobfs_verify_errors(config: ComponentConfig, expect_err: bool) {
        let duration = std::time::Duration::from_secs(1);
        let timeout_err = Err(VerifyErrors::VerifyErrors(vec![VerifyError::VerifyError(
            VerifySource::Blobfs,
            VerifyFailureReason::Timeout,
            duration,
        )]));
        let verify_err = Err(VerifyErrors::VerifyErrors(vec![VerifyError::VerifyError(
            VerifySource::Blobfs,
            VerifyFailureReason::Verify(verify::VerifyError::Internal),
            duration,
        )]));
        let fidl_err = Err(VerifyErrors::VerifyErrors(vec![VerifyError::VerifyError(
            VerifySource::Blobfs,
            VerifyFailureReason::Fidl(fidl::Error::OutOfRange),
            duration,
        )]));

        assert_eq!(PolicyEngine::apply_config(timeout_err, &config).is_err(), expect_err);
        assert_eq!(PolicyEngine::apply_config(verify_err, &config).is_err(), expect_err);
        assert_eq!(PolicyEngine::apply_config(fidl_err, &config).is_err(), expect_err);
    }

    /// Blobfs errors should be ignored if the config says so.
    #[test]
    fn test_blobfs_errors_ignored() {
        test_blobfs_verify_errors(ComponentConfig::builder().blobfs(Mode::Ignore).build(), false);
    }

    #[test]
    fn test_errors_all_ignored() {
        let duration = std::time::Duration::from_secs(1);
        let ve1 =
            VerifyError::VerifyError(VerifySource::Blobfs, VerifyFailureReason::Timeout, duration);
        let ve2 = VerifyError::VerifyError(
            VerifySource::Blobfs,
            VerifyFailureReason::Verify(verify::VerifyError::Internal),
            duration,
        );

        let config = ComponentConfig::builder().blobfs(Mode::Ignore).build();

        // TODO(https://fxbug.dev/42156562): When there are multiple VerifySource
        // types, test heterogeneous VerifyErrors lists.
        assert_matches!(
            PolicyEngine::apply_config(Err(VerifyErrors::VerifyErrors(vec![ve1, ve2])), &config),
            Ok(())
        );
    }

    #[test]
    fn test_errors_none_ignored() {
        let duration = std::time::Duration::from_secs(1);
        let ve1 =
            VerifyError::VerifyError(VerifySource::Blobfs, VerifyFailureReason::Timeout, duration);
        let ve2 = VerifyError::VerifyError(
            VerifySource::Blobfs,
            VerifyFailureReason::Verify(verify::VerifyError::Internal),
            duration,
        );

        let config = ComponentConfig::builder().blobfs(Mode::RebootOnFailure).build();

        let filtered_errors = assert_matches!(
            PolicyEngine::apply_config(Err(VerifyErrors::VerifyErrors(vec![ve1, ve2])), &config),
            Err(VerifyErrors::VerifyErrors(s)) => s);

        assert_matches!(
            &filtered_errors[..],
            [
                VerifyError::VerifyError(VerifySource::Blobfs, VerifyFailureReason::Timeout, _),
                VerifyError::VerifyError(
                    VerifySource::Blobfs,
                    VerifyFailureReason::Verify(verify::VerifyError::Internal),
                    _
                )
            ]
        );
    }

    /// Blobfs errors should NOT be ignored if the config says to reboot on failure.
    #[test]
    fn test_blobfs_errors_reboot_on_failure() {
        test_blobfs_verify_errors(
            ComponentConfig::builder().blobfs(Mode::RebootOnFailure).build(),
            true,
        );
    }
}
