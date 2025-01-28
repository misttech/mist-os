// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assembly_container::WalkPaths;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use input_device_constants::InputDeviceType;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the UI area.
#[derive(
    Debug, Deserialize, Serialize, PartialEq, JsonSchema, SupportsFileRelativePaths, WalkPaths,
)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformUiConfig {
    /// Whether UI should be enabled on the product.
    pub enabled: bool,

    /// The sensor config to provide to the input pipeline.
    #[schemars(schema_with = "crate::option_path_schema")]
    #[file_relative_paths]
    #[walk_paths]
    pub sensor_config: Option<FileRelativePathBuf>,

    /// The minimum frame duration for frame scheduler.
    pub frame_scheduler_min_predicted_frame_duration_in_us: u64,

    /// Scenic shifts focus from view to view as the user interacts with the UI.
    /// Set to false for Smart displays, as they use a different programmatic focus change scheme.
    pub pointer_auto_focus: bool,

    /// Scenic attempts to delegate composition of client images to the display controller, with
    /// GPU/Vulkan composition as the fallback. If false, GPU/Vulkan composition is always used.
    pub display_composition: bool,

    /// The relevant input device bindings from which to install appropriate
    /// input handlers. Default to an empty set.
    pub supported_input_devices: Vec<InputDeviceType>,

    // The rotation of the display, counter-clockwise, in 90-degree increments.
    pub display_rotation: u64,

    // TODO(132584): change to float when supported in structured config.
    // The density of the display, in pixels per mm.
    pub display_pixel_density: String,

    // The expected viewing distance for the display.
    pub viewing_distance: ViewingDistance,

    /// Whether to include brightness manager, and the relevant configs.
    pub brightness_manager: Option<BrightnessManager>,

    /// Set with_synthetic_device_support true to include input-helper to ui.
    pub with_synthetic_device_support: bool,

    /// The renderer Scenic should use.
    pub renderer: RendererType,

    // The constraints on the display mode
    pub display_mode: DisplayModeConfig,

    /// Set visual_debugging_level to enable visual debugging features.
    pub visual_debugging_level: VisualDebuggingLevel,

    // Attaches a11y view in SceneManager
    pub attach_a11y_view: bool,
}

impl Default for PlatformUiConfig {
    fn default() -> Self {
        Self {
            enabled: Default::default(),
            sensor_config: Default::default(),
            frame_scheduler_min_predicted_frame_duration_in_us: Default::default(),
            pointer_auto_focus: true,
            display_composition: false,
            supported_input_devices: Default::default(),
            display_rotation: Default::default(),
            display_pixel_density: Default::default(),
            viewing_distance: Default::default(),
            brightness_manager: Default::default(),
            with_synthetic_device_support: Default::default(),
            renderer: Default::default(),
            display_mode: Default::default(),
            visual_debugging_level: Default::default(),
            attach_a11y_view: true,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum ViewingDistance {
    Handheld,
    Close,
    Near,
    Midrange,
    Far,
    #[default]
    Unknown,
}

impl AsRef<str> for ViewingDistance {
    fn as_ref(&self) -> &str {
        match &self {
            Self::Handheld => "handheld",
            Self::Close => "close",
            Self::Near => "near",
            Self::Midrange => "midrange",
            Self::Far => "far",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayModeConfig {
    /// The constraints on the display mode horizontal resolution, in pixels.
    pub horizontal_resolution_px_range: UnsignedIntegerRangeInclusive,

    /// The constraints on the display mode vertical resolution, in pixels.
    pub vertical_resolution_px_range: UnsignedIntegerRangeInclusive,

    /// The constraints on the display mode refresh rate, in millihertz (10^-3 Hz).
    pub refresh_rate_millihertz_range: UnsignedIntegerRangeInclusive,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub struct BrightnessManager {
    pub with_display_power: bool,
}

// LINT.IfChange
/// Options for Scenic renderers that may be supported.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum RendererType {
    Cpu,
    Null,
    #[default]
    Vulkan,
}
// LINT.ThenChange(/src/ui/scenic/bin/app.h)

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct UnsignedIntegerRangeInclusive {
    /// The inclusive lower bound of the range. If None, the range is unbounded.
    pub start: Option<u32>,

    /// The inclusive upper bound of the range. If None, the range is unbounded.
    pub end: Option<u32>,
}

/// VisualDebuggingLevel used to enable visualized debug features.
/// It has 3 level for now:
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum VisualDebuggingLevel {
    /// None (0): disable all visual debugging features.
    #[default]
    None,
    /// InfoProduct (1): enable colorful blackscreen.
    InfoProduct,
    /// InfoPlatform (2): enable platform related debug: scenic tint.
    InfoPlatform,
}

impl std::convert::From<VisualDebuggingLevel> for u8 {
    fn from(val: VisualDebuggingLevel) -> Self {
        match val {
            VisualDebuggingLevel::None => 0,
            VisualDebuggingLevel::InfoProduct => 1,
            VisualDebuggingLevel::InfoPlatform => 2,
        }
    }
}
