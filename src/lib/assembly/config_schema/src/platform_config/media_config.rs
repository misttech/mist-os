// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the starnix area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformMediaConfig {
    pub audio: Option<AudioConfig>,

    pub camera: CameraConfig,

    pub multizone_leader: MultizoneConfig,

    /// Enable platform-provided video and audio decoders and encoders.
    pub enable_codecs: bool,

    /// Enable a platform-provided service that allows active media players (sessions) to be
    /// published and discovered, primarily for user control of those sessiosn.
    pub enable_sessions: bool,
}

/// The audio stack to use in the platform.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioConfig {
    /// Use the full AudioCore stack.
    FullStack(AudioCoreConfig),

    /// Use the partial AudioDeviceRegistry stack.
    PartialStack,
}

/// Configuration options for the AudioCore stack.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AudioCoreConfig {
    /// Route the ADC device to audio_core.
    pub use_adc_device: bool,
}

/// The camera settings for the platform.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct CameraConfig {
    pub enabled: bool,
}

/// The multizone_leader settings for the platform.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(default)]
pub struct MultizoneConfig {
    /// The component url for the multizone leader component.
    /// The component should expose these capabilities:
    ///   fuchsia.media.SessionAudioConsumerFactory
    ///   google.cast.multizone.Leader
    pub component_url: Option<String>,
}
