// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use input_device_constants::InputDeviceType as PlatformInputDeviceType;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the input area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformInputConfig {
    /// The relevant input device bindings from which to install appropriate
    /// input handlers. Default to an empty set.
    #[serde(default)]
    pub supported_input_devices: Vec<InputDeviceType>,
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
