// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;
use sdk_metadata::ProductBundle;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The DataModel is a required feature of the Scrutiny runtime. Every
/// configuration must include a model configuration. This configuration should
/// include all global configuration in Fuchsia that model collectors should
/// utilize about system state. Instead of collectors hard coding paths these
/// should be tracked here so it is easy to modify all collectors if these
/// paths or urls change in future releases.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
// TODO(https://fxbug.dev/42164596): Borrow instead of clone() and allow clients to clone
// only when necessary.
pub struct ModelConfig {
    /// Path to the Fuchsia update package.
    pub update_package_path: PathBuf,
    /// Path to a directory of blobs.
    pub blobs_directory: PathBuf,
    /// Optional path to a component tree configuration used for customizing
    /// component tree data collection.
    pub component_tree_config_path: Option<PathBuf>,
    /// Whether the model is is based on recovery-mode build artifacts such as the `/recovery` file
    /// in an update package, which is the ZBI installed for booting into recovery mode when
    /// installing an update.
    pub is_recovery: bool,
}

impl ModelConfig {
    /// Build a model based on the contents of a product bundle.
    pub fn from_product_bundle(product_bundle_path: impl AsRef<Path>) -> Result<Self> {
        Self::from_product_bundle_and_recovery(product_bundle_path, false)
    }

    /// Build a model based on the contents of a product bundle using recovery-mode artifacts.
    pub fn from_product_bundle_recovery(product_bundle_path: impl AsRef<Path>) -> Result<Self> {
        Self::from_product_bundle_and_recovery(product_bundle_path, true)
    }

    fn from_product_bundle_and_recovery(
        product_bundle_path: impl AsRef<Path>,
        is_recovery: bool,
    ) -> Result<Self> {
        let product_bundle_path = product_bundle_path.as_ref().to_path_buf();
        let product_bundle_path =
            Utf8PathBuf::try_from(product_bundle_path).context("Converting Path to Utf8Path")?;
        let product_bundle = ProductBundle::try_load_from(&product_bundle_path)?;
        let product_bundle = match product_bundle {
            ProductBundle::V2(pb) => pb,
        };

        let repository = product_bundle
            .repositories
            .get(0)
            .ok_or_else(|| anyhow!("The product bundle must have at least one repository"))?;
        let blobs_directory = repository.blobs_path.clone().into_std_path_buf();

        let update_package_hash = product_bundle
            .update_package_hash
            .ok_or_else(|| anyhow!("An update package must exist inside the product bundle"))?;
        let update_package_path = blobs_directory.join(update_package_hash.to_string());

        Ok(ModelConfig {
            update_package_path,
            blobs_directory,
            component_tree_config_path: None,
            is_recovery,
        })
    }

    /// Path to the Fuchsia update package.
    pub fn update_package_path(&self) -> PathBuf {
        self.update_package_path.clone()
    }
    /// Paths to blobs directory that contain Fuchsia packages and their
    /// contents.
    pub fn blobs_directory(&self) -> PathBuf {
        self.blobs_directory.clone()
    }
    /// Whether the model is based on recovery-mode build artifacts.
    pub fn is_recovery(&self) -> bool {
        self.is_recovery
    }
}
