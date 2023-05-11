// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Platform configuration options for the UI area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
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
}
