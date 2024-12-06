// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU8;

use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the connectivity area.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformConnectivityConfig {
    #[file_relative_paths]
    pub network: PlatformNetworkConfig,
    pub wlan: PlatformWlanConfig,
    #[file_relative_paths]
    pub mdns: MdnsConfig,
    pub thread: ThreadConfig,
    pub weave: WeaveConfig,
    pub netpol: NetpolConfig,
    pub location: LocationConfig,
}

/// Platform configuration options for the network area.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformNetworkConfig {
    /// Only used to control networking for the `utility` and `minimal`
    /// feature_set_levels.
    pub networking: Option<NetworkingConfig>,

    pub netstack_version: NetstackVersion,

    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub netcfg_config_path: Option<FileRelativePathBuf>,

    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub netstack_config_path: Option<FileRelativePathBuf>,

    /// Number of threads used by Netstack3.
    ///
    /// Platform default will be used if unspecified.
    ///
    /// NOTE: An error will be thrown if this is set and Netstack2 is selected
    /// as the system netstack.
    pub netstack_thread_count: Option<NetstackThreadCount>,

    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub google_maps_api_key_path: Option<FileRelativePathBuf>,

    /// Controls how long the http client will wait when it is idle before it
    /// escrows its FIDL connections back to the component framework and exits.
    /// If the value is negative or left out, then the http client will not
    /// stop after it idles.
    pub http_client_stop_on_idle_timeout_millis: Option<i64>,

    /// Controls whether the unified binary for networking should be used.
    ///
    /// The unified binary provides space savings for space-constrained
    /// products, trading off multiple small binaries for one large binary that
    /// is smaller than the sum of its separate parts thanks to linking
    /// optimizations.
    pub use_unified_binary: bool,

    /// Whether to include network-tun.
    pub include_tun: bool,
}

#[derive(Debug, Serialize, Copy, Clone, Deserialize, JsonSchema, PartialEq)]
#[serde(try_from = "u8")]
pub struct NetstackThreadCount(NonZeroU8);

impl TryFrom<u8> for NetstackThreadCount {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        NonZeroU8::new(value)
            .map(NetstackThreadCount)
            .ok_or("netstack thread count must be nonzero")
    }
}

impl Default for NetstackThreadCount {
    fn default() -> Self {
        Self(NonZeroU8::new(4).unwrap())
    }
}

impl NetstackThreadCount {
    pub fn get(&self) -> u8 {
        let Self(v) = self;
        v.get()
    }
}

/// Network stack version to use.
#[derive(Debug, Default, Copy, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetstackVersion {
    Netstack2,
    #[default]
    Netstack3,
    NetstackMigration,
}

/// Which networking type to use (standard or basic).
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NetworkingConfig {
    /// The standard network configuration
    #[default]
    Standard,

    /// A more-basic networking configuration for constrained devices
    Basic,
}

/// Platform configuration options for the wlan area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformWlanConfig {
    /// Enable the use of legacy security types like WEP and/or WPA1
    pub legacy_privacy_support: bool,
    pub recovery_profile: Option<WlanRecoveryProfile>,
    pub recovery_enabled: bool,
    /// Defines roaming behavior for device.
    pub roaming_policy: WlanRoamingPolicy,
    pub policy_layer: WlanPolicyLayer,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanPolicyLayer {
    /// Use the Fuchsia platform's built-in policy layer, `wlancfg`, to configure and manage the
    /// Fuchsia WLAN stack.
    #[default]
    Platform,
    /// Use an external policy layer, which uses `wlanix` to communicate with the Fuchsia WLAN
    /// stack.
    ViaWlanix,
    /// Do not include a policy layer. Intended for testing only and only valid on Eng
    /// products.
    None,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanRecoveryProfile {
    ThresholdedRecovery,
}

// LINT.IfChange
// Configures whether any roaming behavior is enabled.
#[derive(Copy, Clone, Default, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanRoamingPolicy {
    #[default]
    Disabled,
    Enabled {
        #[serde(default)]
        profile: WlanRoamingProfile,
        #[serde(default)]
        mode: WlanRoamingMode,
    },
}

// Configures implementation details for roaming (e.g. what data is considered,
// thresholds, maximum scan frequency, etc.).
#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanRoamingProfile {
    #[default]
    Stationary,
}

// Configures what roaming behavior is allowed for enabled platform roaming. Defaults to
// 'CanRoam', which does not restrict any roaming behavior in the profile.
#[derive(Copy, Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WlanRoamingMode {
    MetricsOnly,
    #[default]
    CanRoam,
}
// LINT.ThenChange(//src/connectivity/wlan/wlancfg/src/client/roaming/lib.rs)

/// Platform configuration options to use for the mdns area.
#[derive(
    Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths,
)]
#[serde(deny_unknown_fields)]
pub struct MdnsConfig {
    /// Enable a wired service so that ffx can discover the device.
    /// If nothing is set, a reasonable default is chosen based on the build type.
    pub publish_fuchsia_dev_wired_service: Option<bool>,

    /// Service config file.
    #[file_relative_paths]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub config: Option<FileRelativePathBuf>,
}

/// Platform configuration options to use for the thread area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ThreadConfig {
    /// Include the LoWPAN service.
    pub include_lowpan: bool,
}

/// Platform configuration options to use for the weave area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WeaveConfig {
    /// The URL of the weave component.
    ///   e.g. fuchsia-pkg://fuchsia.com/weavestack#meta/weavestack.cm
    pub component_url: Option<String>,
}

/// Platform configuration options to use for the Network Policy area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NetpolConfig {
    /// Whether to include network-socket-proxy.
    pub include_socket_proxy: bool,
}

/// Platform configuration options for the location services
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LocationConfig {
    pub enable_emergency_location_provider: bool,
}
