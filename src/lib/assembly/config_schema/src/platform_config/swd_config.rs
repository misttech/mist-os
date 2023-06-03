// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// A representative struct of all the configurable details of the software delivery system made
/// available to a product owner
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
#[serde(rename(serialize = "software_delivery"))]
#[serde(rename(deserialize = "software_delivery"))]
pub struct SwdConfig {
    pub policy: Option<PolicyLabels>,
    pub update_checker: Option<UpdateChecker>,
    pub on_verification_failure: VerificationFailureAction,
    pub tuf_config_path: Option<Utf8PathBuf>,
}

/// The SWD Policies are laid out in
/// [RFC-0118](https://https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0118_swd_policy_at_image_assembly_rfc)
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyLabels {
    BaseComponentsOnly,
    LocalDynamicConfig,
    Unrestricted,
}

/// The UpdateChecker enum represents the particular implementation of the
/// update-checker tool on the target that the `update` package depends on
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChecker {
    /// Omaha-client is the default update checker on userdebug and user builds.
    OmahaClient(OtaConfigs),
    /// “platform” version of an updater
    SystemUpdateChecker,
}

/// Defines the behavior of the system-update-committer package when update
/// verification fails
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum VerificationFailureAction {
    #[default]
    Reboot,
    Disabled,
}

/// Configuration for the Omaha Client
#[derive(Default, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct OtaConfigs {
    /// Deserializes to the ChannelConfig struct defined by SWD in
    /// //src/sys/pkg/lib/channel-config
    pub channels_path: Option<Utf8PathBuf>,
    /// If not specified, the hard-coded value in omaha-client-bin will be
    /// used
    pub server_url: Option<ServerUrl>,

    #[serde(default)]
    pub policy_config: PolicyConfig,
}

type ServerUrl = String;

/// Allows the product owner to define the values that the Omaha Client's
/// FuchsiaPolicy implementation is configured with.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub allow_reboot_when_idle: bool,
    pub startup_delay_seconds: u32,
    pub periodic_interval_minutes: u32,
    pub fuzz_percentage_range: u32,
    pub retry_delay_seconds: u32,
}

impl Default for PolicyConfig {
    fn default() -> PolicyConfig {
        PolicyConfig {
            allow_reboot_when_idle: true,
            startup_delay_seconds: 60,
            periodic_interval_minutes: 60,
            fuzz_percentage_range: 25,
            retry_delay_seconds: 300,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_config::PlatformConfig;

    use anyhow::Result;
    use assembly_util as util;
    use camino::Utf8PathBuf;
    use std::str::FromStr;

    #[test]
    fn test_empty_deserialization() {
        let json5 = r#"
            {}
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.update_checker, None);
        assert_eq!(config.policy, None);
        assert_eq!(config.on_verification_failure, VerificationFailureAction::default());
    }

    // Test full deserialization of the SWD Config
    #[test]
    fn test_swd_config_from_json5() {
        let json5 = r#"
            {
              "update_checker": {
                "omaha_client": {
                  "channels_path": "/path/to/channel_config.json",
                  "server_url": "http://localhost:5000",
                  "policy_config": {
                    "allow_reboot_when_idle": false,
                    "startup_delay_seconds": 42,
                    "periodic_interval_minutes": 55,
                  }
                }
              },
              "policy": "unrestricted",
              "on_verification_failure": "reboot",
              "tuf_config_path": "/path/to/tuf_config.json",
            }
        "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            config,
            SwdConfig {
                policy: Some(PolicyLabels::Unrestricted),
                update_checker: Some(UpdateChecker::OmahaClient(OtaConfigs {
                    channels_path: Some(
                        Utf8PathBuf::from_str("/path/to/channel_config.json").unwrap()
                    ),
                    server_url: Some("http://localhost:5000".to_string()),
                    policy_config: PolicyConfig {
                        allow_reboot_when_idle: false,
                        startup_delay_seconds: 42,
                        periodic_interval_minutes: 55,
                        ..Default::default()
                    }
                })),
                on_verification_failure: VerificationFailureAction::Reboot,
                tuf_config_path: Some(Utf8PathBuf::from_str("/path/to/tuf_config.json").unwrap())
            }
        );
    }

    #[test]
    fn test_invalid_name() {
        let platform_config_json5 = r#"
            {
              build_type: "eng",
              swd_config: {}
            }
        "#;

        let mut cursor = std::io::Cursor::new(platform_config_json5);
        let result: Result<PlatformConfig> = util::from_reader(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_policy_label_is_base_components_only() {
        let json5 = r#"
            {
              "policy": "base_components_only",
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.policy, Some(PolicyLabels::BaseComponentsOnly));
    }

    #[test]
    fn test_policy_label_is_local_dynamic_config() {
        let json5 = r#"
            {
              "policy": "local_dynamic_config",
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.policy, Some(PolicyLabels::LocalDynamicConfig));
    }

    #[test]
    fn test_policy_label_is_unrestricted() {
        let json5 = r#"
            {
              "policy": "unrestricted",
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.policy, Some(PolicyLabels::Unrestricted));
    }

    #[test]
    fn test_policy_label_invalid_name() {
        let json5 = r#"
            {
              "policy": "frobinator",
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let result: Result<SwdConfig> = util::from_reader(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_checker_is_system_update_checker() {
        let json5 = r#"
            {
              "update_checker": "system_update_checker",
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.update_checker, Some(UpdateChecker::SystemUpdateChecker));
    }

    #[test]
    fn test_update_checker_is_omaha_client() {
        let json5 = r#"
            {
              "update_checker": {
                "omaha_client": {},
              }
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.update_checker, Some(UpdateChecker::OmahaClient(OtaConfigs::default())));
    }

    #[test]
    fn test_update_checker_invalid() {
        let json5 = r#"
            {
              "update_checker": "encabulator"
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let result: Result<SwdConfig> = util::from_reader(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_ota_configs_full() {
        let json5 = r#"
            {
              "update_checker": {
                "omaha_client": {
                    "channels_path": "/path/to/channel_config.json",
                    "server_url": "http://localhost:5000",
                    "policy_config": {
                        "allow_reboot_when_idle": false,
                        "startup_delay_seconds": 42,
                        "periodic_interval_minutes": 55,
                    }
                },
              }
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            config.update_checker,
            Some(UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: Some(Utf8PathBuf::from_str("/path/to/channel_config.json").unwrap()),
                server_url: Some("http://localhost:5000".to_string()),
                policy_config: PolicyConfig {
                    allow_reboot_when_idle: false,
                    startup_delay_seconds: 42,
                    periodic_interval_minutes: 55,
                    ..Default::default()
                }
            }))
        );
    }

    #[test]
    fn test_ota_configs_without_policy_config() {
        let json5 = r#"
            {
              "update_checker": {
                "omaha_client": {
                    "channels_path": "/path/to/channel_config.json",
                    "server_url": "http://localhost:5000",
                },
              }
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            config.update_checker,
            Some(UpdateChecker::OmahaClient(OtaConfigs {
                channels_path: Some(Utf8PathBuf::from_str("/path/to/channel_config.json").unwrap()),
                server_url: Some("http://localhost:5000".to_string()),
                policy_config: PolicyConfig::default()
            }))
        );
    }

    #[test]
    fn test_ota_configs_without_policy_config_invalid_field() {
        let json5 = r#"
            {
              "update_checker": {
                "omaha_client": {
                    "encabulator": "/path/to/channel_config.json",
                    "server_url": "http://localhost:5000",
                },
              }
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let result: Result<SwdConfig> = util::from_reader(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_ota_configs_policy_config_defaults() {
        assert_eq!(
            PolicyConfig::default(),
            PolicyConfig {
                allow_reboot_when_idle: true,
                startup_delay_seconds: 60,
                periodic_interval_minutes: 60,
                ..Default::default()
            }
        )
    }

    #[test]
    fn test_verification_failure_action_is_reboot() {
        let json5 = r#"
            {
              "on_verification_failure": "reboot"
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.on_verification_failure, VerificationFailureAction::Reboot);
    }

    #[test]
    fn test_verification_failure_action_is_disabled() {
        let json5 = r#"
            {
              "on_verification_failure": "disabled"
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let config: SwdConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(config.on_verification_failure, VerificationFailureAction::Disabled);
    }

    #[test]
    fn test_verification_failure_action_invalid() {
        let json5 = r#"
            {
              "on_verification_failure": "frobinator"
            }
        "#;
        let mut cursor = std::io::Cursor::new(json5);
        let result: Result<SwdConfig> = util::from_reader(&mut cursor);
        assert!(result.is_err());
    }
}
