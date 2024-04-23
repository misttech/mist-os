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
    #[serde(default)]
    pub thread: ThreadConfig,
    #[serde(default)]
    pub weave: WeaveConfig,
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

    #[serde(default)]
    pub netstack_config_path: Option<Utf8PathBuf>,

    #[serde(default)]
    pub google_maps_api_key_path: Option<Utf8PathBuf>,

    /// Controls how long the http client will wait when it is idle before it
    /// escrows its FIDL connections back to the component framework and exits.
    /// If the value is negative or left out, then the http client will not
    /// stop after it idles.
    #[serde(default)]
    pub http_client_stop_on_idle_timeout_millis: Option<i64>,

    /// Controls whether the unified binary for networking should be used.
    ///
    /// The unified binary provides space savings for space-constrained
    /// products, trading off multiple small binaries for one large binary that
    /// is smaller than the sum of its separate parts thanks to linking
    /// optimizations.
    #[serde(default)]
    pub use_unified_binary: bool,

    /// Whether to include network-tun.
    #[serde(default)]
    pub include_tun: bool,
}

/// Network stack version to use.
#[derive(Debug, Default, Copy, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NetstackVersion {
    #[default]
    Netstack2,
    Netstack3,
    NetstackMigration,
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
    #[serde(default)]
    pub recovery_profile: Option<WlanRecoveryProfile>,
    #[serde(default)]
    pub recovery_enabled: bool,
    /// Defines roaming features/triggers enabled for device.
    #[serde(default)]
    pub roaming_profile: Option<WlanRoamingProfile>,
    #[serde(default)]
    pub policy_layer: WlanPolicyLayer,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WlanPolicyLayer {
    /// Use the Fuchsia platform's built-in policy layer, `wlancfg`, to configure and manage the
    /// Fuchsia WLAN stack.
    #[default]
    Platform,
    /// Use an external policy layer, which uses `wlanix` to communicate with the Fuchsia WLAN
    /// stack.
    ViaWlanix,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WlanRecoveryProfile {
    ThresholdedRecovery,
}

#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WlanRoamingProfile {
    #[default]
    RoamingOff,
    MetricsOnly,
    StationaryRoaming,
}
/// Platform configuration options to use for the mdns area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MdnsConfig {
    /// Enable a wired service so that ffx can discover the device.
    pub publish_fuchsia_dev_wired_service: Option<bool>,

    /// Service config file.
    pub config: Option<Utf8PathBuf>,
}

/// Platform configuration options to use for the thread area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ThreadConfig {
    /// Include the LoWPAN service.
    #[serde(default)]
    pub include_lowpan: bool,
}

/// Platform configuration options to use for the weave area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WeaveConfig {
    /// The URL of the weave component.
    ///   e.g. fuchsia-pkg://fuchsia.com/weavestack#meta/weavestack.cm
    #[serde(default)]
    pub component_url: Option<String>,
}
