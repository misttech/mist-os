// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fuchsia_url::AbsolutePackageUrl;
use scrutiny::prelude::{DataCollector, DataModel};
use scrutiny_config::ModelConfig;
use scrutiny_plugins::core::collection::{Component, Components, Package, Packages};
use scrutiny_plugins::core::controller::package_extract::PackageExtractController;
use scrutiny_plugins::unified_plugin::UnifiedCollector;
use scrutiny_plugins::verify::controller::structured_config::{
    ExtractStructuredConfigController, ExtractStructuredConfigResponse,
};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

pub struct Scrutiny {
    model: Arc<DataModel>,
}

impl Scrutiny {
    pub fn from_product_bundle(path: impl AsRef<Path>) -> Result<Self> {
        let model_config = ModelConfig::from_product_bundle(path)?;
        Self::from_model_config(model_config)
    }

    pub fn from_product_bundle_recovery(path: impl AsRef<Path>) -> Result<Self> {
        let model_config = ModelConfig::from_product_bundle_recovery(path)?;
        Self::from_model_config(model_config)
    }

    fn from_model_config(model_config: ModelConfig) -> Result<Self> {
        let model = Arc::new(DataModel::new(model_config)?);
        let collector = UnifiedCollector::default();
        collector.collect(model.clone())?;
        Ok(Self { model })
    }

    pub fn get_components(&self) -> Result<Vec<Component>> {
        let mut components = self.model.get::<Components>()?.entries.clone();
        components.sort_by(|a, b| a.url.partial_cmp(&b.url).unwrap());
        Ok(components)
    }

    pub fn get_package(&self, url: AbsolutePackageUrl) -> Result<Option<Package>> {
        let packages = &self.model.get::<Packages>()?.entries;
        for package in packages.iter() {
            if package.matches_url(&url) {
                return Ok(Some(package.clone()));
            }
        }
        return Ok(None);
    }

    pub fn extract_package(
        &self,
        url: AbsolutePackageUrl,
        output: impl AsRef<Path>,
    ) -> Result<Value> {
        PackageExtractController::extract(self.model.clone(), url, output)
    }

    pub fn extract_structured_config(&self) -> Result<ExtractStructuredConfigResponse> {
        ExtractStructuredConfigController::extract(self.model.clone())
    }
}
