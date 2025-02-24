// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use sampler_component_config::Config as ComponentConfig;
use sampler_config::runtime::ProjectConfig;
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
        let ComponentConfig { minimum_sample_rate_sec, project_configs } = config;
        let project_configs = project_configs
            .into_iter()
            .map(|config| {
                let config: ProjectConfig = serde_json::from_str(&config)?;
                Ok(Arc::new(config))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self { project_configs, minimum_sample_rate_sec })
    }
}
