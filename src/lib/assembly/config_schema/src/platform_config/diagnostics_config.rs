// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Diagnostics configuration options for the diagnostics area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    #[serde(default)]
    pub archivist: Option<ArchivistConfig>,
    #[serde(default)]
    pub archivist_pipelines: Vec<ArchivistPipeline>,
    #[serde(default)]
    pub additional_serial_log_components: Vec<String>,
    #[serde(default)]
    pub sampler: SamplerConfig,
    #[serde(default)]
    pub memory_monitor: MemoryMonitorConfig,
}

/// Diagnostics configuration options for the archivist configuration area.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum ArchivistConfig {
    Default,
    LowMem,
}

/// A single archivist pipeline config.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArchivistPipeline {
    pub name: String,
    pub files: Vec<Utf8PathBuf>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename = "snake_case")]
pub enum ArchivistPipelineName {
    Feedback,
    Lowpan,
    LegacyMetrics,
}

/// Diagnostics configuration options for the sampler configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SamplerConfig {
    /// The metrics configs to pass to sampler.
    #[serde(default)]
    pub metrics_configs: Vec<Utf8PathBuf>,
    /// The fire configs to pass to sampler.
    #[serde(default)]
    pub fire_configs: Vec<Utf8PathBuf>,
}

/// Diagnostics configuration options for the memory monitor configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MemoryMonitorConfig {
    /// The memory buckets config file to provide to memory monitor.
    #[serde(default)]
    pub buckets: Option<Utf8PathBuf>,
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
            DiagnosticsConfig {
                archivist: None,
                archivist_pipelines: Vec::new(),
                additional_serial_log_components: Vec::new(),
                sampler: SamplerConfig::default(),
                memory_monitor: MemoryMonitorConfig::default(),
            }
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
            DiagnosticsConfig {
                archivist: None,
                archivist_pipelines: Vec::new(),
                additional_serial_log_components: Vec::new(),
                sampler: SamplerConfig::default(),
                memory_monitor: MemoryMonitorConfig::default(),
            }
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
                archivist_pipelines: Vec::new(),
                additional_serial_log_components: vec!["/foo".to_string()],
                sampler: SamplerConfig::default(),
                memory_monitor: MemoryMonitorConfig::default(),
            }
        );
    }
}
