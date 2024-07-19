// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::TEST_REPO_URL;
use fuchsia_merkle::HASH_SIZE;
use fuchsia_url::{AbsolutePackageUrl, PackageName, UnpinnedAbsolutePackageUrl};
use scrutiny_collection::core::ComponentSource;
use scrutiny_collection::model::DataModel;
use scrutiny_collection::model_config::ModelConfig;
use std::str::FromStr;
use std::sync::Arc;
use tempfile::tempdir;

/// Creates a simple fake model configuration that uses an in memory uri and
/// tempdata() directories for the required build locations.
pub fn fake_model_config() -> ModelConfig {
    let dir_path = tempdir().unwrap().into_path();
    let update_package_path = dir_path.join("update.far");
    let blobs_directory = dir_path.join("blobs");
    ModelConfig {
        uri: "{memory}".to_string(),
        update_package_path,
        blobs_directory,
        config_data_package_url: AbsolutePackageUrl::Unpinned(UnpinnedAbsolutePackageUrl::new(
            TEST_REPO_URL.clone(),
            PackageName::from_str("config-data").unwrap(),
            None,
        )),
        additional_boot_args_path: "config/additional_boot_args".into(),
        component_tree_config_path: None,
        tmp_dir_path: None,
        is_empty: false,
        is_recovery: false,
    }
}

/// Constructs a simple fake data model with an in memory uri and tempdata()
/// build directory.
pub fn fake_data_model() -> Arc<DataModel> {
    Arc::new(DataModel::new(fake_model_config()).unwrap())
}

const FAKE_PKG_MERKLE: [u8; HASH_SIZE] = [0x42; HASH_SIZE];

pub fn fake_component_src_pkg() -> ComponentSource {
    ComponentSource::Package(FAKE_PKG_MERKLE.into())
}
