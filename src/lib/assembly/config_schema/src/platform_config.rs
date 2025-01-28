// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use assembly_file_relative_path::SupportsFileRelativePaths;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod battery_config;
pub mod bluetooth_config;
pub mod connectivity_config;
pub mod development_support_config;
pub mod diagnostics_config;
pub mod driver_framework_config;
pub mod example_config;
pub mod fonts_config;
pub mod forensics_config;
pub mod graphics_config;
pub mod health_check_config;
pub mod icu_config;
pub mod intl_config;
pub mod kernel_config;
pub mod media_config;
pub mod memory_monitor_config;
pub mod paravirtualization_config;
pub mod power_config;
pub mod recovery_config;
pub mod session_config;
pub mod setui_config;
pub mod starnix_config;
pub mod storage_config;
pub mod swd_config;
pub mod sysmem_config;
pub mod timekeeper_config;
pub mod ui_config;
pub mod usb_config;
pub mod virtualization_config;

/// Platform configuration options.  These are the options that pertain to the
/// platform itself, not anything provided by the product.
#[derive(
    Debug, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths, WalkPaths,
)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    /// The minimum service-level that the platform will provide, or the main
    /// set of platform features that are necessary (or desired) by the product.
    ///
    /// This is the most-significant determination of the availability of major
    /// subsystems.
    #[serde(default)]
    pub feature_set_level: FeatureSupportLevel,

    /// The RFC-0115 Build Type of the assembled product + platform.
    ///
    /// https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0115_build_types
    ///
    /// After the FeatureSupportLevel, this is the next most-influential
    /// determinant of the makeup of the platform.  It selects platform
    /// components and configuration, and is used to disallow various platform
    /// configuration settings when producing Userdebug and User images.
    pub build_type: BuildType,

    /// List of logging tags to forward to the serial console.
    ///
    /// Appended to the list of tags defined for the platform.
    #[serde(default)]
    pub additional_serial_log_tags: Vec<String>,

    /// Platform configuration options for the battery.
    #[serde(default)]
    pub battery: battery_config::BatteryConfig,

    /// Platform configuration options for the bluetooth area.
    #[serde(default)]
    pub bluetooth: bluetooth_config::BluetoothConfig,

    /// Platform configuration options for the connectivity area.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub connectivity: connectivity_config::PlatformConnectivityConfig,

    /// Platform configuration options for enabling developer support.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub development_support: development_support_config::DevelopmentSupportConfig,

    /// Platform configuration options for the diagnostics area.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub diagnostics: diagnostics_config::DiagnosticsConfig,

    /// Platform configuration options for the driver framework area.
    #[serde(default)]
    pub driver_framework: driver_framework_config::DriverFrameworkConfig,

    /// Platform configuration options for the forensics area.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub forensics: forensics_config::ForensicsConfig,

    /// Platform configuration options for graphics
    #[serde(default)]
    pub graphics: graphics_config::GraphicsConfig,

    /// Platform configuration options for the media area.
    #[serde(default)]
    pub media: media_config::PlatformMediaConfig,

    /// Platform configuration options for the memory monitor area.
    #[serde(default)]
    pub memory_monitor: memory_monitor_config::PlatformMemoryMonitorConfig,

    /// Platform configuration options for paravirtualization.
    #[serde(default)]
    pub paravirtualization: paravirtualization_config::PlatformParavirtualizationConfig,

    /// Platform configuration options for recovery.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub recovery: recovery_config::RecoveryConfig,

    /// Platform configuration options for the session.
    #[serde(default)]
    pub session: session_config::PlatformSessionConfig,

    /// Platform configuration options for the SWD subsystem.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub software_delivery: swd_config::SwdConfig,

    /// Platform configuration options for the starnix area.
    #[serde(default)]
    pub starnix: starnix_config::PlatformStarnixConfig,

    /// Platform configuration options for storage support.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub storage: storage_config::StorageConfig,

    /// Platform configuration options for sysmem (contiguous/protected memory
    /// support). These override (field-by-field) any values set in
    /// sysmem_defaults in the board config. See also
    /// BoardProvidedConfig.sysmem_format_costs which can be specified for the
    /// board.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub sysmem: sysmem_config::PlatformSysmemConfig,

    /// Platform configuration options for the UI area.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub ui: ui_config::PlatformUiConfig,

    /// Platform configuration options for the virtualization area.
    #[serde(default)]
    pub virtualization: virtualization_config::PlatformVirtualizationConfig,

    /// Platform configuration options for ICU library choice. Platform components are 'flavored'
    /// by the ICU version they're compiled to use.
    #[serde(default)]
    pub icu: icu_config::ICUConfig,

    /// Platform configuration options for fonts.
    #[serde(default)]
    pub fonts: fonts_config::FontsConfig,

    /// Platform configuration options for internationalization.
    #[serde(default)]
    pub intl: intl_config::IntlConfig,

    /// SetUi configuration.
    ///
    /// SetUI is added to the platform config on all Standard systems.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub setui: setui_config::SetUiConfig,

    /// Assembly option triggering the inclusion of test AIBs
    ///
    /// NOTE: This is not for use by products! It's for testing assembly itself.
    #[serde(default)]
    pub example_config: example_config::ExampleConfig,

    #[serde(default)]
    pub kernel: kernel_config::PlatformKernelConfig,

    /// Platform configuration options for the power area.
    #[serde(default)]
    #[file_relative_paths]
    #[walk_paths]
    pub power: power_config::PowerConfig,

    /// Platform configuration options for time maintenance and timekeeping.
    #[serde(default)]
    pub timekeeper: timekeeper_config::TimekeeperConfig,

    /// Platform configuration options for USB peripheral.
    #[serde(default)]
    pub usb: usb_config::UsbConfig,

    /// Platform configuration options for OTA Health Checks.
    #[serde(default)]
    pub health_check: health_check_config::HealthCheckConfig,
}

// LINT.IfChange
/// The platform's base service level.
///
/// This is the basis for the contract with the product as to what the minimal
/// set of services that are available in the platform will be.  Features can
/// be enabled on top of this most-basic level, but some features will require
/// a higher basic level of support.
///
/// These were initially based on the product definitions that are used to
/// provide the basis for all other products:
///
/// bringup.gni  (Bootstrap)
///   +--> minimal.gni  (Minimal)
///         +--> core.gni
///               +--> (everything else)
///
/// The `Utility` level is between `Bootstrap` and `Minimal`, adding the `/core`
/// realm and those children of `/core` needed by all systems that include
/// `/core`.
///
/// The standard (default) level is `Minimal`. It is the level that should be
/// used by products' main system.
#[derive(Debug, Deserialize, Serialize, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FeatureSupportLevel {
    /// THIS IS FOR TESTING AND MIGRATIONS ONLY!
    ///
    /// It creates an assembly with no platform.
    Empty,

    /// This is a small build of fuchsia which is not meant to support
    /// self-updates, but rather be updated externally. It is meant for truly
    /// memory constrained environments where fuchsia does not need to driver a
    /// large amount of hardware. It includes a minimal subset of bootstrap and
    /// doesn't bring in any of core.
    Embeddable,

    /// Bootable, but serial-only.  This is only the `/bootstrap` realm.  No
    /// netstack, no storage drivers, etc.  This is one of the smallest bootable
    /// systems created by assembly, and is primarily used for board-level bringup.
    ///
    /// https://fuchsia.dev/fuchsia-src/development/build/build_system/bringup
    Bootstrap,

    /// This is the smallest configuration that includes the `/core` realm, and
    /// is best suited for utility-type systems such as recovery.  The "main"
    /// system for a product should not use this, and instead use the default.
    Utility,

    /// This is the smallest "full Fuchsia" configuration.  This has a netstack,
    /// can update itself, and has all the subsystems that are required to
    /// ship a production-level product.
    ///
    /// This is the default level unless otherwise specified.
    #[default]
    Standard,
}
// LINT.ThenChange(../../platform_configuration/src/common.rs)

/// The platform BuildTypes.
///
/// These control security and behavioral settings within the platform, and can
/// change the platform packages placed into the assembled product image.
///
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BuildType {
    Eng,
    UserDebug,
    User,
}

impl std::fmt::Display for BuildType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildType::Eng => f.write_str("eng"),
            BuildType::UserDebug => f.write_str("userdebug"),
            BuildType::User => f.write_str("user"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseBuildTypeError;

impl std::str::FromStr for BuildType {
    type Err = ParseBuildTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "eng" => Ok(Self::Eng),
            "userdebug" => Ok(Self::UserDebug),
            "user" => Ok(Self::User),
            _ => Err(ParseBuildTypeError),
        }
    }
}

impl std::fmt::Display for ParseBuildTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BuildType cannot be parsed")
    }
}
