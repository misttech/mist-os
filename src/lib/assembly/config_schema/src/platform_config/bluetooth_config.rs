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
// TODO(b/324894109): Add profile-specific arguments
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct A2dpConfig {
    pub enabled: bool,
}

/// Configuration options for Bluetooth media info and controls (bt-avrcp).
// TODO(b/324894109): Add profile-specific arguments
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct AvrcpConfig {
    pub enabled: bool,
}

/// Configuration options for Bluetooth Device Identification profile (bt-device-id).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct DeviceIdConfig {
    /// Enable the device identification profile (`bt-device-id`).
    pub enabled: bool,
    /// Uniquely identifies the Vendor of the device.
    /// Mandatory if `enabled` is true.
    pub vendor_id: u16,
    /// Uniquely identifies the product - typically a value assigned by the Vendor.
    /// Mandatory if `enabled` is true.
    pub product_id: u16,
    /// Device release number.
    /// Mandatory if `enabled` is true.
    pub version: u16,
    /// If `true`, designates this identification as the primary service record for this device.
    /// Mandatory if `enabled` is true.
    pub primary: bool,
    /// A human-readable description of the service.
    /// Optional if `enabled` is true.
    pub service_description: Option<String>,
}

/// HFP Audio Gateway Features
/// See HFP v1.9 Page 100 for details.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HfpAudioGatewayFeature {
    ThreeWayCalling,
    EchoCancelingAndNoiseReduction,
    VoiceRecognition,
    InbandRingtone,
    AttachPhoneNumberVoiceTag,
    RejectIncomingCall,
    EnhancedCallControl,
    EnhancedVoiceRecognitionStatus,
    VoiceRecognitionText,
}

impl std::fmt::Display for HfpAudioGatewayFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HfpAudioGatewayFeature::ThreeWayCalling => write!(f, "three_way_calling"),
            HfpAudioGatewayFeature::EchoCancelingAndNoiseReduction => {
                write!(f, "echo_canceling_and_noise_reduction")
            }
            HfpAudioGatewayFeature::VoiceRecognition => write!(f, "voice_recognition"),
            HfpAudioGatewayFeature::InbandRingtone => write!(f, "inband_ringtone"),
            HfpAudioGatewayFeature::AttachPhoneNumberVoiceTag => {
                write!(f, "attach_phone_number_voice_tag")
            }
            HfpAudioGatewayFeature::RejectIncomingCall => write!(f, "reject_incoming_call"),
            HfpAudioGatewayFeature::EnhancedCallControl => write!(f, "enhanced_call_control"),
            HfpAudioGatewayFeature::EnhancedVoiceRecognitionStatus => {
                write!(f, "enhanced_voice_recognition_status")
            }
            HfpAudioGatewayFeature::VoiceRecognitionText => write!(f, "voice_recognition_text"),
        }
    }
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

/// Configuration options for Bluetooth hands free (bt-hfp).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct HfpConfig {
    /// Enable hands free calling audio gateway (`bt-hfp-audio-gateway`).
    pub enabled: bool,

    /// The set of AudioGateway features that are enabled.
    /// Features not included are disabled by default, with the exception of the
    /// following which are always enabled:
    ///  - Enhanced Call Status
    ///  - Extended Error Result Codes
    ///  - Codec Negotiation
    ///  - HF Indicators
    ///  - eSCO S4
    pub audio_gateway: Vec<HfpAudioGatewayFeature>,

    /// The set of codecs that are enabled to use.
    /// If MSBC is enabled, Wide Band Speech will be enabled
    /// If LC3 is enabled, Super Wide Band will be enabled
    /// By default, all codecs supported (either by the controller as specified below) will be enabled.
    pub codecs_supported: Vec<HfpCodecId>,
    /// Set of codec ids that the Bluetooth controller can encode.
    /// Codecs not supported will be ignored.
    /// Codecs not in this list but in codecs_supported will be encoded locally and sent inband.
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
    pub a2dp: A2dpConfig,

    /// Specifies the configuration for `bt-avrcp`.
    pub avrcp: AvrcpConfig,

    /// Specifies the configuration for `bt-device-id`.
    pub did: DeviceIdConfig,

    /// Specifies the configuration for `bt-hfp`.
    pub hfp: HfpConfig,

    /// Specifies the configuration for `bt-map`.
    pub map: MapConfig,
}

/// Platform configuration for Bluetooth core features.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct BluetoothCoreConfig {
    /// Enable BR/EDR legacy pairing.
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
        profiles: BluetoothProfilesConfig,

        /// Configuration for Bluetooth core.
        #[serde(default)]
        core: BluetoothCoreConfig,

        /// Configuration for `bt-snoop`.
        #[serde(default)]
        snoop: Snoop,
    },
    /// The coreless Bluetooth configuration omits the "core" set of Bluetooth components and only
    /// includes any specified standalone packages.
    /// This is typically reserved for testing or special scenarios in which minimal BT things are
    /// needed.
    Coreless {
        /// Configuration for `bt-snoop`.
        #[serde(default)]
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
                    "enabled": true,
                    "audio_gateway": [
                        "voice_recognition",
                        "three_way_calling",
                        "inband_ringtone",
                        "echo_canceling_and_noise_reduction",
                        "attach_phone_number_voice_tag",
                        "reject_incoming_call",
                        "enhanced_call_control",
                        "enhanced_voice_recognition_status",
                        "voice_recognition_text"],
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
                enabled: true,
                audio_gateway: vec![
                    HfpAudioGatewayFeature::VoiceRecognition,
                    HfpAudioGatewayFeature::ThreeWayCalling,
                    HfpAudioGatewayFeature::InbandRingtone,
                    HfpAudioGatewayFeature::EchoCancelingAndNoiseReduction,
                    HfpAudioGatewayFeature::AttachPhoneNumberVoiceTag,
                    HfpAudioGatewayFeature::RejectIncomingCall,
                    HfpAudioGatewayFeature::EnhancedCallControl,
                    HfpAudioGatewayFeature::EnhancedVoiceRecognitionStatus,
                    HfpAudioGatewayFeature::VoiceRecognitionText,
                ],
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
