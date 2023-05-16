// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Assembly platform configuratio schema for the Fonts subsystem.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FontsConfig {
    /// If true, use assembly to configure fonts.
    ///
    /// Otherwise, assembly configuration is skipped, and we assume that
    /// the configuration is done some other way.
    /// Prod configurations will want to set this to false.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

// Most configs want to enable fonts configuration, so the default here
// is `true`.
fn default_enabled() -> bool {
    true
}

impl Default for FontsConfig {
    fn default() -> Self {
        FontsConfig { enabled: default_enabled() }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn default_fonts_config() {
        let json = serde_json::json!({});
        let config: FontsConfig = serde_json::from_value(json).unwrap();
        // Checks that the default for the "fonts enabled" flag is true (i.e.
        // different from the "regular" zero type.)
        assert!(config.enabled);
    }
}
