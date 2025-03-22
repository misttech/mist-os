// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Specifies the configuration for the Bluetooth Snoop component (`bt-snoop`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Snoop {
    /// Don't include `bt-snoop`.
    #[default]
    None,
    /// Include `bt-snoop` with lazy startup.
    Lazy,
    /// Include `bt-snoop` with an eager startup during boot.
    Eager,
}

/// Configuration options for Bluetooth audio streaming (bt-a2dp).
// TODO(https://fxbug.dev/324894109): Add profile-specific arguments
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct A2dpConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enabled: bool,
}

/// Configuration options for Bluetooth media info and controls (bt-avrcp).
// TODO(https://fxbug.dev/324894109): Add profile-specific arguments
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct AvrcpConfig {
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enabled: bool,
}

/// Configuration options for Bluetooth Device Identification profile (bt-device-id).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct DeviceIdConfig {
    /// Enable the device identification profile (`bt-device-id`).
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enabled: bool,
    /// Uniquely identifies the Vendor of the device.
    /// Mandatory if `enabled` is true.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub vendor_id: u16,
    /// Uniquely identifies the product - typically a value assigned by the Vendor.
    /// Mandatory if `enabled` is true.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub product_id: u16,
    /// Device release number.
    /// Mandatory if `enabled` is true.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub version: u16,
    /// If `true`, designates this identification as the primary service record for this device.
    /// Mandatory if `enabled` is true.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub primary: bool,
    /// A human-readable description of the service.
    /// Optional if `enabled` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_description: Option<String>,
}

/// Codec IDs defined by the Bluetooth HFP Specification
/// See HFP v1.9 Appendix B
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HfpCodecId {
    Cvsd,
    Msbc,
    Lc3Swb,
}

/// Tri-state representation of a Bluetooth profile with optional features.
/// Allows product integrators to enable a profile without specifying any optional feature values.
#[derive(Deserialize, Default, PartialEq, Debug)]
enum BluetoothProfileDeserializer<T> {
    /// Disable the profile.
    #[default]
    #[serde(rename = "disabled")]
    Disabled,
    /// Enable the profile and use default values for the features.
    #[serde(rename = "enabled")]
    EnabledDefault,
    /// Enable the profile and use the provided input `T` values for the features.
    #[serde(untagged)]
    Enabled(T),
}

/// HFP Audio Gateway Features
/// See HFP v1.9 Page 100 for details.
/// Features not included are disabled by default, with the exception of the following which are
/// always enabled:
///  - Enhanced Call Status
///  - Extended Error Result Codes
///  - Codec Negotiation
///  - HF Indicators
///  - eSCO S4
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct AudioGatewayEnabledConfig {
    /// Enable management of of several concurrent calls.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub three_way_calling: bool,
    /// Enable echo canceling and/or noise reduction functionality.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub echo_canceling_and_noise_reduction: bool,
    /// Enable hands-free control of a device's functions through voice commands.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub voice_recognition: bool,
    /// Enable sending the ringtone for a phone call.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub inband_ringtone: bool,
    /// Enable the voice tag association feature.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub attach_phone_number_voice_tag: bool,
    /// Enable the reject incoming call feature.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub reject_incoming_call: bool,
    /// Enabled enhanced call controls (private mode & release specified call index procedures).
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enhanced_call_control: bool,
    /// Enable enhanced hands-free call controls including integration with voice assistants.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enhanced_voice_recognition_status: bool,
    /// Enable the voice-to-text feature.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub voice_recognition_text: bool,
}

/// Configuration options for the Bluetooth HFP Audio Gateway component ('bt-hfp-audio-gateway').
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(from = "BluetoothProfileDeserializer<AudioGatewayEnabledConfig>")]
pub enum AudioGatewayConfig {
    /// Disable `bt-hfp-audio-gateway`.
    #[default]
    Disabled,
    /// Enable `bt-hfp-audio-gateway`.
    Enabled(AudioGatewayEnabledConfig),
}

impl From<BluetoothProfileDeserializer<AudioGatewayEnabledConfig>> for AudioGatewayConfig {
    fn from(s: BluetoothProfileDeserializer<AudioGatewayEnabledConfig>) -> Self {
        match s {
            BluetoothProfileDeserializer::Disabled => Self::Disabled,
            BluetoothProfileDeserializer::EnabledDefault => {
                Self::Enabled(AudioGatewayEnabledConfig::default())
            }
            BluetoothProfileDeserializer::Enabled(c) => Self::Enabled(c),
        }
    }
}

/// HFP Hands Free Features
/// See HFP v1.9 Table 6.4 for the list of features and Table 3.2 for a description of the features.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct HandsFreeEnabledConfig {
    /// Enable echo canceling and/or noise reduction functionality.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub echo_canceling_and_noise_reduction: bool,
    /// Enable management of of several concurrent calls.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub three_way_calling: bool,
    /// Enable call identification for incoming calls.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub calling_line_identification: bool,
    /// Enable hands-free control of a device's functions through voice commands.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub voice_recognition: bool,
    /// Enable the remote volume control feature.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub remote_volume_control: bool,
    /// Enable enhanced hands-free call controls including integration with voice assistants.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enhanced_voice_recognition: bool,
    /// Enable the voice-to-text feature.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub voice_recognition_text: bool,
}

/// Configuration options for the Bluetooth HFP Hands Free component ('bt-hfp-hands-free').
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(from = "BluetoothProfileDeserializer<HandsFreeEnabledConfig>")]
pub enum HandsFreeConfig {
    /// Disabled `bt-hfp-hands-free`.
    #[default]
    Disabled,
    /// Enable `bt-hfp-hands-free`.
    Enabled(HandsFreeEnabledConfig),
}

impl From<BluetoothProfileDeserializer<HandsFreeEnabledConfig>> for HandsFreeConfig {
    fn from(s: BluetoothProfileDeserializer<HandsFreeEnabledConfig>) -> Self {
        match s {
            BluetoothProfileDeserializer::Disabled => Self::Disabled,
            BluetoothProfileDeserializer::EnabledDefault => {
                Self::Enabled(HandsFreeEnabledConfig::default())
            }
            BluetoothProfileDeserializer::Enabled(c) => Self::Enabled(c),
        }
    }
}

/// Configuration options for Bluetooth hands free calling.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct HfpConfig {
    /// Enable hands free calling audio gateway (`bt-hfp-audio-gateway`).
    // TODO(https://fxbug.dev/401064356): Remove this field after soft-transition is complete.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub enabled: bool,

    /// Specifies the configuration for `bt-hfp-audio-gateway`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub audio_gateway: AudioGatewayConfig,

    /// Specifies the configuration for `bt-hfp-hands-free`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub hands_free: HandsFreeConfig,

    /// The set of codecs that are enabled to use.
    /// If MSBC is enabled, Wide Band Speech will be enabled
    /// If LC3 is enabled, Super Wide Band will be enabled
    /// By default, all codecs supported (either by the controller as specified below) will be enabled.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub codecs_supported: Vec<HfpCodecId>,

    /// Set of codec ids that the Bluetooth controller can encode.
    /// Codecs not supported will be ignored.
    /// Codecs not in this list but in codecs_supported will be encoded locally and sent inband.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub controller_encodes: Vec<HfpCodecId>,
}

/// Configuration options for Bluetooth message access profile (bt-map)
/// client equipment role.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct MapConfig {
    /// Enable message access client equipment (`bt-map-mce`).
    pub mce_enabled: bool,
}

/// Platform configuration to enable Bluetooth profiles.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct BluetoothProfilesConfig {
    /// Specifies the configuration for `bt-a2dp`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub a2dp: A2dpConfig,

    /// Specifies the configuration for `bt-avrcp`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub avrcp: AvrcpConfig,

    /// Specifies the configuration for `bt-device-id`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub did: DeviceIdConfig,

    /// Specifies the configuration for `bt-hfp`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub hfp: HfpConfig,

    /// Specifies the configuration for `bt-map`.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub map: MapConfig,
}

/// Platform configuration for Bluetooth core features.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct BluetoothCoreConfig {
    /// Enable BR/EDR legacy pairing.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub legacy_pairing_enabled: bool,
}

/// Platform configuration options for Bluetooth.
/// The default platform configuration does not include any Bluetooth packages.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum BluetoothConfig {
    /// The standard Bluetooth configuration includes the "core" set of components that provide
    /// basic Bluetooth functionality (GATT, Advertising, etc.) and optional profiles and tools.
    /// This is expected to be the most common configuration used in the platform.
    Standard {
        /// Configuration for Bluetooth profiles. The default includes no profiles.
        #[serde(default)]
        #[serde(skip_serializing_if = "crate::common::is_default")]
        profiles: BluetoothProfilesConfig,

        /// Configuration for Bluetooth core.
        #[serde(default)]
        #[serde(skip_serializing_if = "crate::common::is_default")]
        core: BluetoothCoreConfig,

        /// Configuration for `bt-snoop`.
        #[serde(default)]
        #[serde(skip_serializing_if = "crate::common::is_default")]
        snoop: Snoop,
    },
    /// The coreless Bluetooth configuration omits the "core" set of Bluetooth components and only
    /// includes any specified standalone packages.
    /// This is typically reserved for testing or special scenarios in which minimal BT things are
    /// needed.
    Coreless {
        /// Configuration for `bt-snoop`.
        #[serde(default)]
        #[serde(skip_serializing_if = "crate::common::is_default")]
        snoop: Snoop,
    },
}

impl Default for BluetoothConfig {
    fn default() -> BluetoothConfig {
        // The default platform configuration does not include any Bluetooth packages.
        BluetoothConfig::Coreless { snoop: Snoop::None }
    }
}

impl BluetoothConfig {
    pub fn snoop(&self) -> Snoop {
        match &self {
            Self::Standard { snoop, .. } => *snoop,
            Self::Coreless { snoop, .. } => *snoop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_serialization() {
        crate::common::tests::default_serialization_helper::<BluetoothConfig>();
    }

    #[test]
    fn deserialize_standard_config_no_profiles() {
        let json = serde_json::json!({
            "type": "standard",
            "snoop": "lazy",
        });

        let parsed: BluetoothConfig = serde_json::from_value(json).unwrap();
        let expected = BluetoothConfig::Standard {
            profiles: BluetoothProfilesConfig::default(),
            core: BluetoothCoreConfig::default(),
            snoop: Snoop::Lazy,
        };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn deserialize_standard_config_with_profiles() {
        let json = serde_json::json!({
            "type": "standard",
            "snoop": "eager",
            "profiles": {
                "a2dp": {
                    "enabled": true,
                },
                "avrcp": {
                    "enabled": true,
                },
                "did": {
                    "enabled": true,
                    "vendor_id": 0,
                    "product_id": 1,
                    "version": 0x0100,
                    "primary": true,
                    "service_description": "foobar",
                },
                "hfp": {
                    "audio_gateway": {
                        "voice_recognition": true,
                        "three_way_calling": true,
                        "inband_ringtone": true,
                        "echo_canceling_and_noise_reduction": true,
                        "attach_phone_number_voice_tag": true,
                        "reject_incoming_call": true,
                        "enhanced_call_control": true,
                        "enhanced_voice_recognition_status": true,
                        "voice_recognition_text": true,
                    },
                    "enabled": true, // TODO(https://fxbug.dev/401064356): Remove after migration
                    "hands_free": {
                        "echo_canceling_and_noise_reduction": true,
                        "three_way_calling": true,
                        "calling_line_identification": true,
                        "voice_recognition": true,
                        "remote_volume_control": true,
                        "enhanced_voice_recognition": true,
                        "voice_recognition_text": true,
                    },
                    "codecs_supported": ["cvsd", "msbc", "lc3swb"],
                    "controller_encodes": ["cvsd", "msbc", "lc3swb"],
                },
            },
            "core": {
                "legacy_pairing_enabled": true,
            },
        });

        let parsed: BluetoothConfig = serde_json::from_value(json).unwrap();
        let expected_profiles = BluetoothProfilesConfig {
            a2dp: A2dpConfig { enabled: true },
            avrcp: AvrcpConfig { enabled: true },
            did: DeviceIdConfig {
                enabled: true,
                vendor_id: 0,
                product_id: 1,
                version: 0x0100,
                primary: true,
                service_description: Some("foobar".to_string()),
            },
            hfp: HfpConfig {
                audio_gateway: AudioGatewayConfig::Enabled(AudioGatewayEnabledConfig {
                    three_way_calling: true,
                    echo_canceling_and_noise_reduction: true,
                    voice_recognition: true,
                    inband_ringtone: true,
                    attach_phone_number_voice_tag: true,
                    reject_incoming_call: true,
                    enhanced_call_control: true,
                    enhanced_voice_recognition_status: true,
                    voice_recognition_text: true,
                }),
                enabled: true,
                hands_free: HandsFreeConfig::Enabled(HandsFreeEnabledConfig {
                    echo_canceling_and_noise_reduction: true,
                    three_way_calling: true,
                    calling_line_identification: true,
                    voice_recognition: true,
                    remote_volume_control: true,
                    enhanced_voice_recognition: true,
                    voice_recognition_text: true,
                }),
                codecs_supported: vec![HfpCodecId::Cvsd, HfpCodecId::Msbc, HfpCodecId::Lc3Swb],
                controller_encodes: vec![HfpCodecId::Cvsd, HfpCodecId::Msbc, HfpCodecId::Lc3Swb],
            },
            map: MapConfig { mce_enabled: false },
        };
        let expected_core = BluetoothCoreConfig { legacy_pairing_enabled: true };
        let expected = BluetoothConfig::Standard {
            profiles: expected_profiles,
            core: expected_core,
            snoop: Snoop::Eager,
        };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn deserialize_hfp_profiles_without_defaults() {
        let json = serde_json::json!({
            "type": "standard",
            "profiles": {
                "hfp": {
                    "audio_gateway": "enabled",
                    "hands_free": "enabled",
                },
            },
        });

        let parsed: BluetoothConfig = serde_json::from_value(json).unwrap();
        let expected_profiles = BluetoothProfilesConfig {
            hfp: HfpConfig {
                audio_gateway: AudioGatewayConfig::Enabled(AudioGatewayEnabledConfig {
                    three_way_calling: false,
                    echo_canceling_and_noise_reduction: false,
                    voice_recognition: false,
                    inband_ringtone: false,
                    attach_phone_number_voice_tag: false,
                    reject_incoming_call: false,
                    enhanced_call_control: false,
                    enhanced_voice_recognition_status: false,
                    voice_recognition_text: false,
                }),
                enabled: false, // TODO(https://fxbug.dev/401064356): Remove after migration
                hands_free: HandsFreeConfig::Enabled(HandsFreeEnabledConfig {
                    echo_canceling_and_noise_reduction: false,
                    three_way_calling: false,
                    calling_line_identification: false,
                    voice_recognition: false,
                    remote_volume_control: false,
                    enhanced_voice_recognition: false,
                    voice_recognition_text: false,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let expected = BluetoothConfig::Standard {
            profiles: expected_profiles,
            core: BluetoothCoreConfig::default(),
            snoop: Snoop::None,
        };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn deserialize_coreless_config() {
        let json = serde_json::json!({
            "type": "coreless",
            "snoop": "eager",
        });

        let parsed: BluetoothConfig = serde_json::from_value(json).unwrap();
        let expected = BluetoothConfig::Coreless { snoop: Snoop::Eager };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn deserialize_coreless_with_profiles_is_error() {
        let json = serde_json::json!({
            "type": "coreless",
            "profiles": "",
        });

        let parsed_result: Result<BluetoothConfig, _> = serde_json::from_value(json);
        assert!(parsed_result.is_err());
    }
}
