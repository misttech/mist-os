// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// Diagnostics configuration options for the diagnostics area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    #[serde(default)]
    pub archivist: Option<ArchivistConfig>,
    #[serde(default)]
    pub additional_serial_log_components: Vec<String>,
}

/// Diagnostics configuration options for the archivist configuration area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum ArchivistConfig {
    Default,
    LowMem,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_config::PlatformConfig;

    use assembly_util as util;

    #[test]
    fn test_diagnostics_archivist_default_service() {
        let json5 = r#""default""#;
        let mut cursor = std::io::Cursor::new(json5);
        let archivist: ArchivistConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(archivist, ArchivistConfig::Default);
    }

    #[test]
    fn test_diagnostics_archivist_low_mem() {
        let json5 = r#""low-mem""#;
        let mut cursor = std::io::Cursor::new(json5);
        let archivist: ArchivistConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(archivist, ArchivistConfig::LowMem);
    }

    #[test]
    fn test_diagnostics_missing() {
        let json5 = r#"
        {
          build_type: "eng",
          feature_set_level: "minimal",
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: PlatformConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            config.diagnostics,
            DiagnosticsConfig { archivist: None, additional_serial_log_components: Vec::new() }
        );
    }

    #[test]
    fn test_diagnostics_empty() {
        let json5 = r#"
        {
          build_type: "eng",
          feature_set_level: "minimal",
          diagnostics: {}
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: PlatformConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            config.diagnostics,
            DiagnosticsConfig { archivist: None, additional_serial_log_components: Vec::new() }
        );
    }

    #[test]
    fn test_diagnostics_with_additional_log_tags() {
        let json5 = r#"
        {
          build_type: "eng",
          feature_set_level: "minimal",
          diagnostics: {
            additional_serial_log_components: [
                "/foo",
            ]
          }
        }
    "#;

        let mut cursor = std::io::Cursor::new(json5);
        let config: PlatformConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(
            config.diagnostics,
            DiagnosticsConfig {
                archivist: None,
                additional_serial_log_components: vec!["/foo".to_string()]
            }
        );
    }
}
