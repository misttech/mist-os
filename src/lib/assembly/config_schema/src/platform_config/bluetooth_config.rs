// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Specifies the configuration for the Bluetooth Snoop component (`bt-snoop`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
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

// TODO(b/292109810): Add configuration options for Profiles.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct BluetoothProfilesConfig;

/// Platform configuration options for Bluetooth.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum BluetoothConfig {
    /// The standard Bluetooth configuration includes the "core" set of components that provide
    /// basic Bluetooth functionality (GATT, Advertising, etc.) and optional profiles and tools.
    /// This is expected to be the default and most common configuration used in the platform.
    Standard {
        /// Configuration for Bluetooth profiles.
        /// `Some<T>` if any BT profiles are needed, `None` otherwise.
        profiles: Option<BluetoothProfilesConfig>,
        /// Configuration for `bt-snoop`.
        snoop: Snoop,
    },
    /// The coreless Bluetooth configuration omits the "core" set of Bluetooth components and only
    /// includes any specified standalone packages.
    /// This is typically reserved for testing or special scenarios in which minimal BT things are
    /// needed.
    Coreless {
        /// Configuration for `bt-snoop`.
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
        let expected = BluetoothConfig::Standard { profiles: None, snoop: Snoop::Lazy };

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
