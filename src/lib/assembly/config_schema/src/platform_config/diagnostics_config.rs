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
}

/// Diagnostics configuration options for the archivist configuration area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum ArchivistConfig {
    NoDetectService,
    NoService,
    Bringup,
    DefaultService,
    #[serde(rename = "low-mem-default-service-config")]
    LowMem,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_config::PlatformConfig;

    use assembly_util as util;

    #[test]
    fn test_diagnostics_archivist_no_service() {
        let json5 = r#""no-service""#;
        let mut cursor = std::io::Cursor::new(json5);
        let archivist: ArchivistConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(archivist, ArchivistConfig::NoService);
    }

    #[test]
    fn test_diagnostics_archivist_no_detect_service() {
        let json5 = r#""no-detect-service""#;
        let mut cursor = std::io::Cursor::new(json5);
        let archivist: ArchivistConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(archivist, ArchivistConfig::NoDetectService);
    }

    #[test]
    fn test_diagnostics_archivist_default_service() {
        let json5 = r#""default-service""#;
        let mut cursor = std::io::Cursor::new(json5);
        let archivist: ArchivistConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(archivist, ArchivistConfig::DefaultService);
    }

    #[test]
    fn test_diagnostics_archivist_bringup() {
        let json5 = r#""bringup""#;
        let mut cursor = std::io::Cursor::new(json5);
        let archivist: ArchivistConfig = util::from_reader(&mut cursor).unwrap();
        assert_eq!(archivist, ArchivistConfig::Bringup);
    }

    #[test]
    fn test_diagnostics_archivist_low_mem() {
        let json5 = r#""low-mem-default-service-config""#;
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
        assert!(config.diagnostics.archivist.is_none());
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
        assert_eq!(config.diagnostics, DiagnosticsConfig { archivist: None });
    }
}
