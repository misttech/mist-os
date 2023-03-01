// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for the connectivity area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformConnectivityConfig {
    #[serde(default)]
    pub network: PlatformNetworkConfig,
    #[serde(default)]
    pub wlan: PlatformWlanConfig,
}

/// Platform configuration options for the network area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformNetworkConfig {
    #[serde(default)]
    pub force_netstack3: bool,
}

/// Platform configuration options for the wlan area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformWlanConfig {
    /// Enable the use of legacy security types like WEP and/or WPA1.
    #[serde(default)]
    pub legacy_privacy_support: bool,
    /// Only include WLAN AIBs if set to true. Temporary flag to help with
    /// migration to assembly.
    #[serde(default)]
    pub include_wlan_aibs: bool,
}
