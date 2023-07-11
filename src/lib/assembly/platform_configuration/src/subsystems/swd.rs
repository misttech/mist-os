// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::platform_config::swd_config::{
    OtaConfigs, PolicyConfig, PolicyLabels, SwdConfig, UpdateChecker, VerificationFailureAction,
};

#[allow(dead_code)]
const FUZZ_PERCENTAGE_RANGE: u32 = 25;
#[allow(dead_code)]
const RETRY_DELAY_SECONDS: u32 = 300;

/// SWD Configuration implementation detail which clarifies different subsystem behavior, derived
/// from the PolicyLabels
#[allow(dead_code)]
#[derive(Debug)]
pub struct PolicyLabelDetails {
    enable_dynamic_configuration: bool,
    persisted_repos_dir: bool,
    disable_executability_restrictions: bool,
}

#[allow(dead_code)]
impl PolicyLabelDetails {
    fn from_policy_labels(policy_labels: &PolicyLabels) -> Self {
        match policy_labels {
            PolicyLabels::BaseComponentsOnly => PolicyLabelDetails {
                enable_dynamic_configuration: false,
                disable_executability_restrictions: false,
                persisted_repos_dir: false,
            },
            PolicyLabels::LocalDynamicConfig => PolicyLabelDetails {
                enable_dynamic_configuration: true,
                disable_executability_restrictions: false,
                persisted_repos_dir: false,
            },
            PolicyLabels::Unrestricted => PolicyLabelDetails {
                enable_dynamic_configuration: true,
                disable_executability_restrictions: true,
                persisted_repos_dir: true,
            },
        }
    }
}

impl DefaultByBuildType for SwdConfig {
    fn default_by_build_type(build_type: &BuildType) -> Self {
        SwdConfig {
            policy: Some(PolicyLabels::default_by_build_type(build_type)),
            update_checker: Some(UpdateChecker::default_by_build_type(build_type)),
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_path: None,
        }
    }
}

pub(crate) struct SwdSubsystemConfig;
impl DefineSubsystemConfiguration<Option<SwdConfig>> for SwdSubsystemConfig {
    /// Configures the SWD system. If |subsystem_config| is None, no SWD system
    /// is configured by this call.
    /// Configures the SWD system. If |subsystem_config| is None, the default
    /// is set based on the build type and/or feature set level.
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        subsystem_config: &Option<SwdConfig>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        match subsystem_config {
            // Add the checker according to the configuration
            Some(SwdConfig { update_checker: Some(update_checker), .. }) => {
                Self::set_update_checker(Some(update_checker), builder)?;
            }
            // No checker is set or there is no SWD config at all, set based on
            // feature set level
            Some(SwdConfig { update_checker: None, .. }) | None => {
                match context.feature_set_level {
                    // Minimal has an update checker
                    FeatureSupportLevel::Minimal => {
                        let update_checker =
                            UpdateChecker::default_by_build_type(context.build_type);
                        Self::set_update_checker(Some(&update_checker), builder)?;
                    }
                    // Utility has no update checker
                    FeatureSupportLevel::Utility => {
                        Self::set_update_checker(None, builder)?;
                    }
                    // Bootstrap has neither an update checker nor the system-update realm,
                    // so do not include `no_update_checker` AIB that requires the realm.
                    FeatureSupportLevel::Bootstrap => {}
                };
            }
        }

        Ok(())
    }
}

impl SwdSubsystemConfig {
    /// Configure which AIB to select based on the UpdateChecker
    fn set_update_checker(
        update_checker: Option<&UpdateChecker>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        match update_checker {
            Some(UpdateChecker::OmahaClient(OtaConfigs { policy_config, .. })) => {
                builder.platform_bundle("omaha_client");
                let mut omaha_config =
                    builder.package("omaha-client").component("meta/omaha-client-service.cm")?;
                omaha_config
                    .field("periodic_interval_minutes", policy_config.periodic_interval_minutes)?
                    .field("startup_delay_seconds", policy_config.startup_delay_seconds)?
                    .field("allow_reboot_when_idle", policy_config.allow_reboot_when_idle)?
                    .field("retry_delay_seconds", policy_config.retry_delay_seconds)?
                    .field("fuzz_percentage_range", policy_config.fuzz_percentage_range)?;
            }
            Some(UpdateChecker::SystemUpdateChecker) => {
                builder.platform_bundle("system_update_checker");
            }
            None => {
                builder.platform_bundle("no_update_checker");
            }
        }
        Ok(())
    }
}

impl DefaultByBuildType for UpdateChecker {
    fn default_by_build_type(build_type: &BuildType) -> Self {
        match build_type {
            BuildType::Eng => UpdateChecker::SystemUpdateChecker,
            BuildType::UserDebug => UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default(),
            }),
            BuildType::User => UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default(),
            }),
        }
    }
}
impl DefaultByBuildType for PolicyLabels {
    fn default_by_build_type(build_type: &BuildType) -> Self {
        match build_type {
            BuildType::Eng => PolicyLabels::Unrestricted,
            BuildType::UserDebug => PolicyLabels::LocalDynamicConfig,
            BuildType::User => PolicyLabels::BaseComponentsOnly,
        }
    }
}

/// SWD Configuration implementation detail which allows the product owner to define custom failure
/// recovery rules for the system-update-committer
#[derive(Clone, Debug, PartialEq)]
struct VerificationFailureConfigDetails {
    enable: bool,
    blobfs: String,
}

impl VerificationFailureConfigDetails {
    #[allow(dead_code)]
    fn from_verification_failure_config(vfc: VerificationFailureAction) -> Self {
        match vfc {
            VerificationFailureAction::Reboot => VerificationFailureConfigDetails {
                enable: true,
                blobfs: "reboot_on_failure".to_string(),
            },
            VerificationFailureAction::Disabled => {
                VerificationFailureConfigDetails { enable: false, blobfs: "ignore".to_string() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swd_config_default_from_build_type_eng() {
        let build_type = &BuildType::Eng;
        let config = SwdConfig {
            policy: None,
            update_checker: None,
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_path: None,
        };
        let policy = config.policy.value_or_default_from_build_type(build_type);
        let update_checker = config.update_checker.value_or_default_from_build_type(build_type);
        let on_verification_failure = VerificationFailureAction::default();

        assert_eq!(policy, PolicyLabels::Unrestricted);
        assert_eq!(update_checker, UpdateChecker::SystemUpdateChecker);
        assert_eq!(on_verification_failure, VerificationFailureAction::Reboot);
    }
    #[test]
    fn test_swd_config_default_from_build_type_userdebug() {
        let build_type = &BuildType::UserDebug;
        let config = SwdConfig {
            policy: None,
            update_checker: None,
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_path: None,
        };
        let policy = config.policy.value_or_default_from_build_type(build_type);
        let update_checker = config.update_checker.value_or_default_from_build_type(build_type);
        let on_verification_failure = VerificationFailureAction::default();

        assert_eq!(policy, PolicyLabels::LocalDynamicConfig);
        assert_eq!(
            update_checker,
            UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default()
            })
        );
        assert_eq!(on_verification_failure, VerificationFailureAction::Reboot);
    }
    #[test]
    fn test_swd_config_default_from_build_type_user() {
        let build_type = &BuildType::User;
        let config = SwdConfig {
            policy: None,
            update_checker: None,
            on_verification_failure: VerificationFailureAction::default(),
            tuf_config_path: None,
        };
        let policy = config.policy.value_or_default_from_build_type(build_type);
        let update_checker = config.update_checker.value_or_default_from_build_type(build_type);
        let on_verification_failure = VerificationFailureAction::default();

        assert_eq!(policy, PolicyLabels::BaseComponentsOnly);
        assert_eq!(
            update_checker,
            UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: None,
                server_url: None,
                policy_config: PolicyConfig::default()
            })
        );
        assert_eq!(on_verification_failure, VerificationFailureAction::Reboot);
    }
}
