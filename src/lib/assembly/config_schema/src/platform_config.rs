// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

pub mod battery_config;
pub mod connectivity_config;
pub mod development_support_config;
pub mod diagnostics_config;
pub mod driver_framework_config;
pub mod example_config;
pub mod fonts_config;
pub mod graphics_config;
pub mod icu_config;
pub mod identity_config;
pub mod input_config;
pub mod media_config;
pub mod session_manager_config;
pub mod starnix_config;
pub mod storage_config;
pub mod swd_config;
pub mod ui_config;
pub mod virtualization_config;

/// Platform configuration options.  These are the options that pertain to the
/// platform itself, not anything provided by the product.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
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

    /// Platform configuration options for the connectivity area.
    #[serde(default)]
    pub connectivity: connectivity_config::PlatformConnectivityConfig,

    /// Platform configuration options for enabling developer support.
    #[serde(default)]
    pub development_support: development_support_config::DevelopmentSupportConfig,

    /// Platform configuration options for the diagnostics area.
    #[serde(default)]
    pub diagnostics: diagnostics_config::DiagnosticsConfig,

    /// Platform configuration options for the driver framework area.
    #[serde(default)]
    pub driver_framework: driver_framework_config::DriverFrameworkConfig,

    /// Platform configuration options for graphics
    #[serde(default)]
    pub graphics: graphics_config::GraphicsConfig,

    /// Platform configuration options for the identity area.
    #[serde(default)]
    pub identity: identity_config::PlatformIdentityConfig,

    /// Platform configuration options for the input area.
    #[serde(default)]
    pub input: input_config::PlatformInputConfig,

    /// Platform configuration options for the media area.
    #[serde(default)]
    pub media: media_config::PlatformMediaConfig,

    /// Platform configuration options for the session manager.
    #[serde(default)]
    pub session: session_manager_config::PlatformSessionManagerConfig,

    /// Platform configuration options for the SWD subsystem.
    pub software_delivery: Option<swd_config::SwdConfig>,

    /// Platform configuration options for the starnix area.
    #[serde(default)]
    pub starnix: starnix_config::PlatformStarnixConfig,

    /// Platform configuration options for storage support.
    #[serde(default)]
    pub storage: storage_config::StorageConfig,

    /// Platform configuration options for the UI area.
    #[serde(default)]
    pub ui: ui_config::PlatformUiConfig,

    /// Platform configuration options for the virtualization area.
    #[serde(default)]
    pub virtualization: virtualization_config::PlatformVirtualizationConfig,

    /// Platform configuration options for ICU library choice. If not specified,
    /// then assembly should use the unflavored components.
    ///
    /// Platform components can be 'flavored' by the ICU version they're
    /// compiled to use, or be 'unflavored' and using the tree's implicit
    /// ICU version.
    ///
    /// If not specified, the 'unflavored' components are used. If the default
    /// ICU version is specified, then the 'default' flavor is used (which is
    /// a distinct set of components from the 'unflavored' components).
    /// Assemblies are being transitioned from the 'unflavored'
    /// components to 'flavored' components`.
    pub icu: Option<icu_config::ICUConfig>,

    /// Platform configuration options for fonts.
    #[serde(default)]
    pub fonts: fonts_config::FontsConfig,

    /// Assembly option triggering the inclusion of test AIBs
    ///
    /// NOTE: This is not for use by products! It's for testing assembly itself.
    #[serde(default)]
    pub example_config: example_config::ExampleConfig,
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
#[derive(Debug, Deserialize, Serialize, PartialEq, Default)]
pub enum FeatureSupportLevel {
    /// THIS IS FOR TESTING AND MIGRATIONS ONLY!
    ///
    /// It creates an assembly with no platform.
    #[serde(rename = "empty")]
    Empty,

    /// Bootable, but serial-only.  This is only the `/bootstrap` realm.  No
    /// netstack, no storage drivers, etc.  this is the smallest bootable system
    /// created by assembly, and is primarily used for board-level bringup.
    ///
    /// https://fuchsia.dev/fuchsia-src/development/build/build_system/bringup
    #[serde(rename = "bootstrap")]
    Bootstrap,

    /// This is the smallest configuration that includes the `/core` realm, and
    /// is best suited for utility-type systems such as recovery.  The "main"
    /// system for a product should not use this, and instead use the default.
    #[serde(rename = "utility")]
    Utility,

    /// This is the smallest "full Fuchsia" configuration.  This has a netstack,
    /// can update itself, and has all the subsystems that are required to
    /// ship a production-level product.
    ///
    /// This is the default level unless otherwise specified.
    #[serde(rename = "minimal")]
    #[default]
    Minimal,
}
// LINT.ThenChange(../../platform_configuration/src/common.rs)

/// The platform BuildTypes.
///
/// These control security and behavioral settings within the platform, and can
/// change the platform packages placed into the assembled product image.
///
#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub enum BuildType {
    #[serde(rename = "eng")]
    Eng,

    #[serde(rename = "userdebug")]
    UserDebug,

    #[serde(rename = "user")]
    User,
}
