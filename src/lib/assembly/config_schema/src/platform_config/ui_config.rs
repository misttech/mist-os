// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use input_device_constants::InputDeviceType as PlatformInputDeviceType;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the UI area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformUiConfig {
    /// Whether UI should be enabled on the product.
    #[serde(default)]
    pub enabled: bool,

    /// The sensor config to provide to the input pipeline.
    #[serde(default)]
    pub sensor_config: Option<Utf8PathBuf>,

    /// The minimum frame duration for frame scheduler.
    #[serde(default)]
    pub frame_scheduler_min_predicted_frame_duration_in_us: u64,

    /// Scenic shifts focus from view to view as the user interacts with the UI.
    /// Set to false for Smart displays, as they use a different programmatic focus change scheme.
    #[serde(default)]
    pub pointer_auto_focus: bool,

    /// Scenic attempts to delegate composition of client images to the display controller, with
    /// GPU/Vulkan composition as the fallback. If false, GPU/Vulkan composition is always used.
    #[serde(default)]
    pub display_composition: bool,

    /// The relevant input device bindings from which to install appropriate
    /// input handlers. Default to an empty set.
    #[serde(default)]
    pub supported_input_devices: Vec<InputDeviceType>,

    // The rotation of the display, counter-clockwise, in 90-degree increments.
    #[serde(default)]
    pub display_rotation: i64,

    // TODO(132584): change to float when supported in structured config.
    // The density of the display, in pixels per mm.
    #[serde(default)]
    pub display_pixel_density: String,

    // The expected viewing distance for the display.
    #[serde(default)]
    pub viewing_distance: ViewingDistance,
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
        }
    }
}

// LINT.IfChange
/// Options for input devices that may be supported.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum InputDeviceType {
    Button,
    Keyboard,
    LightSensor,
    Mouse,
    Touchscreen,
}

// This impl verifies that the platform and assembly enums are kept in sync.
impl From<InputDeviceType> for PlatformInputDeviceType {
    fn from(src: InputDeviceType) -> PlatformInputDeviceType {
        match src {
            InputDeviceType::Button => PlatformInputDeviceType::ConsumerControls,
            InputDeviceType::Keyboard => PlatformInputDeviceType::Keyboard,
            InputDeviceType::LightSensor => PlatformInputDeviceType::LightSensor,
            InputDeviceType::Mouse => PlatformInputDeviceType::Mouse,
            InputDeviceType::Touchscreen => PlatformInputDeviceType::Touch,
        }
    }
}
// LINT.ThenChange(/src/ui/lib/input-device-constants/src/lib.rs)

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum ViewingDistance {
    Handheld,
    Close,
    #[default]
    Near,
    Midrange,
    Far,
}

impl AsRef<str> for ViewingDistance {
    fn as_ref(&self) -> &str {
        match &self {
            Self::Handheld => "handheld",
            Self::Close => "close",
            Self::Near => "near",
            Self::Midrange => "midrange",
            Self::Far => "far",
        }
    }
}
