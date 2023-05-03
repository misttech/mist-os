// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Assembly platform configuratio schema for the Fonts subsystem.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FontsConfig {
    /// Whether to allow verbose logging (true) or not (false).
    #[serde(default)]
    pub verbose_logging: bool,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_config_deserialize() {
        let json = serde_json::json!({
            "verbose_logging": true,
        });

        let parsed: FontsConfig = serde_json::from_value(json).unwrap();
        let expected = FontsConfig { verbose_logging: true };

        assert_eq!(parsed, expected);
    }
}
