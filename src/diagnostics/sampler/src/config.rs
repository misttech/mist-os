// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use sampler_component_config::Config as ComponentConfig;
use sampler_config::runtime::ProjectConfig;
use std::path::PathBuf;
use std::sync::Arc;

/// Container for all configurations needed to instantiate the Sampler infrastructure.
/// Includes:
///      - Project configurations.
///      - Whether to configure the ArchiveReader for tests (e.g. longer timeouts)
///      - Minimum sample rate.
#[derive(Debug)]
pub struct SamplerConfig {
    pub project_configs: Vec<Arc<ProjectConfig>>,
    pub minimum_sample_rate_sec: i64,
}

impl SamplerConfig {
    pub fn new(config: ComponentConfig) -> Result<Self, Error> {
        let root = PathBuf::from(config.configs_path);
        let mut sampler_config_dir = root.clone();
        sampler_config_dir.push("metrics");
        let mut fire_config_dir = root;
        fire_config_dir.push("fire");
        let project_configs =
            sampler_config::load_project_configs(sampler_config_dir, Some(fire_config_dir))?
                .into_iter()
                .map(Arc::new)
                .collect();
        Ok(Self { project_configs, minimum_sample_rate_sec: config.minimum_sample_rate_sec })
    }
}
