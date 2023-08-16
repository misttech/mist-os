// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the connectivity area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformConnectivityConfig {
    #[serde(default)]
    pub network: PlatformNetworkConfig,
    #[serde(default)]
    pub wlan: PlatformWlanConfig,
    #[serde(default)]
    pub mdns: MdnsConfig,
}

/// Platform configuration options for the network area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformNetworkConfig {
    /// Only used to control networking for the `utility` and `minimal`
    /// feature_set_levels.
    pub networking: Option<NetworkingConfig>,

    #[serde(default)]
    pub netstack_version: NetstackVersion,

    #[serde(default)]
    pub netcfg_config_path: Option<Utf8PathBuf>,
}

/// Network stack version to use.
#[derive(Debug, Default, Copy, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
// TODO(https://fxbug.dev/131101): Introduce migration version option.
pub enum NetstackVersion {
    #[default]
    Netstack2,
    Netstack3,
}

/// Which networking type to use (standard or basic).
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkingConfig {
    /// The standard network configuration
    #[default]
    Standard,

    /// A more-basic networking configuration for constrained devices
    Basic,
}

/// Platform configuration options for the wlan area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformWlanConfig {
    /// Enable the use of legacy security types like WEP and/or WPA1.
    #[serde(default)]
    pub legacy_privacy_support: bool,
}

/// Platform configuration options to use for the mdns area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MdnsConfig {
    /// Enable a wired service so that ffx can discover the device.
    pub publish_fuchsia_dev_wired_service: Option<bool>,
}
