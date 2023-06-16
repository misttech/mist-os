// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the UI area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformUiConfig {
    /// Whether UI should be enabled on the product.
    #[serde(default)]
    pub enabled: bool,

    /// Use the legacy feature set.
    /// TODO(fxbug.dev/124273): This will be deleted once legacy is removed.
    #[serde(default)]
    pub legacy: bool,

    /// The sensor config to provide to the input pipeline.
    #[serde(default)]
    pub sensor_config: Option<Utf8PathBuf>,

    /// The minimum frame duration for frame scheduler.
    #[serde(default)]
    pub frame_scheduler_min_predicted_frame_duration_in_us: u64,

    // Scenic shifts focus from view to view as the user interacts with the UI.
    // Set to false for Smart displays, as they use a different programmatic focus change scheme.
    #[serde(default)]
    pub pointer_auto_focus: bool,
}

impl Default for PlatformUiConfig {
    fn default() -> Self {
        Self {
            enabled: Default::default(),
            legacy: Default::default(),
            sensor_config: Default::default(),
            frame_scheduler_min_predicted_frame_duration_in_us: Default::default(),
            pointer_auto_focus: true,
        }
    }
}
