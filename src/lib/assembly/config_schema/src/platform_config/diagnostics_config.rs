// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

/// Diagnostics configuration options for the diagnostics area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    #[serde(default)]
    pub archivist: Option<ArchivistConfig>,
    /// The set of pipeline config files to supply to archivist.
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
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum ArchivistConfig {
    Default,
    LowMem,
}

/// A single archivist pipeline config.
#[derive(Debug, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchivistPipeline {
    /// The name of the pipeline.
    pub name: PipelineType,
    /// The files to add to the pipeline.
    /// Zero files is not valid.
    #[schemars(schema_with = "crate::vec_path_schema")]
    pub files: Vec<Utf8PathBuf>,
}

#[derive(Debug, PartialEq, Clone, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(into = "String")]
pub enum PipelineType {
    /// A pipeline for feedback data.
    Feedback,
    /// A custom pipeline.
    Custom(String),
}

impl Serialize for PipelineType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Feedback => serializer.serialize_str("feedback"),
            Self::Custom(s) => serializer.serialize_str(&s),
        }
    }
}

impl<'de> Deserialize<'de> for PipelineType {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let variant = String::deserialize(de)?.to_lowercase();
        match variant.as_str() {
            "feedback" => Ok(PipelineType::Feedback),
            "lowpan" => Ok(PipelineType::Custom("lowpan".into())),
            "legacy_metrics" => Ok(PipelineType::Custom("legacy_metrics".into())),
            other => {
                Err(D::Error::unknown_variant(other, &["feedback", "lowpan", "legacy_metrics"]))
            }
        }
    }
}

impl std::fmt::Display for PipelineType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Feedback => "feedback",
                Self::Custom(name) => &name,
            }
        )
    }
}

impl From<PipelineType> for String {
    fn from(t: PipelineType) -> Self {
        t.to_string()
    }
}

/// Diagnostics configuration options for the sampler configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SamplerConfig {
    /// The metrics configs to pass to sampler.
    #[serde(default)]
    #[schemars(schema_with = "crate::vec_path_schema")]
    pub metrics_configs: Vec<Utf8PathBuf>,
    /// The fire configs to pass to sampler.
    #[serde(default)]
    #[schemars(schema_with = "crate::vec_path_schema")]
    pub fire_configs: Vec<Utf8PathBuf>,
}

/// Diagnostics configuration options for the memory monitor configuration area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryMonitorConfig {
    /// The memory buckets config file to provide to memory monitor.
    #[serde(default)]
    #[schemars(schema_with = "crate::option_path_schema")]
    pub buckets: Option<Utf8PathBuf>,
    /// Control whether a pressure change should trigger a capture.
    #[serde(default)]
    pub capture_on_pressure_change: Option<bool>,
    /// Expected delay between scheduled captures upon imminent OOM, in
    /// seconds.
    #[serde(default)]
    pub imminent_oom_capture_delay_s: Option<u32>,
    /// Expected delay between scheduled captures when the memory
    /// pressure is critical, in seconds.
    #[serde(default)]
    pub critical_capture_delay_s: Option<u32>,
    /// Expected delay between scheduled captures when the memory
    /// pressure is abnormal, in seconds.
    #[serde(default)]
    pub warning_capture_delay_s: Option<u32>,
    /// Expected delay between scheduled captures when the memory
    /// pressure is normal, in seconds.
    #[serde(default)]
    pub normal_capture_delay_s: Option<u32>,
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
