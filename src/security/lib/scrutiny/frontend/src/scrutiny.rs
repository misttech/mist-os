// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::scrutiny_artifacts::ScrutinyArtifacts;

use scrutiny_collector::unified_collector::UnifiedCollector;

use anyhow::Result;
use scrutiny_collection::model::DataModel;
use scrutiny_collection::model_config::ModelConfig;
use std::path::Path;
use std::sync::Arc;

pub struct Scrutiny {
    model_config: ModelConfig,
}

impl Scrutiny {
    pub fn from_product_bundle(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { model_config: ModelConfig::from_product_bundle(path)? })
    }

    pub fn from_product_bundle_recovery(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self { model_config: ModelConfig::from_product_bundle_recovery(path)? })
    }

    pub fn set_component_tree_config_path(&mut self, path: impl AsRef<Path>) {
        self.model_config.component_tree_config_path = Some(path.as_ref().to_path_buf());
    }

    pub fn collect(self) -> Result<ScrutinyArtifacts> {
        let model = Arc::new(DataModel::new(self.model_config)?);
        let collector = UnifiedCollector::default();
        collector.collect(model.clone())?;
        Ok(ScrutinyArtifacts { model })
    }
}
