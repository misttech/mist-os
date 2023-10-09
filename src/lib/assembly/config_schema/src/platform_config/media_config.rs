// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformMediaConfig {
    /// Enable Audio Device Registry.
    /// TODO: Remove this once all clients are using the audio config.
    #[serde(default)]
    pub audio_device_registry_enabled: bool,

    #[serde(default)]
    pub audio: Option<AudioConfig>,
}

/// The audio stack to use in the platform.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AudioConfig {
    /// Use the full AudioCore stack.
    FullStack(AudioCoreConfig),

    /// Use the partial AudioDeviceRegistry stack.
    PartialStack,
}

/// Configuration options for the AudioCore stack.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AudioCoreConfig {
    /// Route the ADC device to audio_core.
    #[serde(default)]
    pub use_adc_device: bool,

    /// Expect an audio_core package to be provided by the product and do not
    /// include a generic one from the platform.
    ///
    /// The product-provided audio_core must have the package url:
    ///     fuchsia-pkg://fuchsia.com/audio_core#meta/audio_core.cm
    ///
    #[serde(default)]
    pub product_provides_audio_core: bool,
}
