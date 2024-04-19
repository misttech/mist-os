// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! FFX plugin for the info of repository inside product bundle.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use ffx_core::ffx_plugin;
use ffx_product_get_repository_args::GetRepositoryCommand;
use ffx_writer::Writer;
use fidl_fuchsia_developer_ffx_ext::RepositoryConfig;
use sdk_metadata::ProductBundle;
use serde::{Deserialize, Serialize};
use std::io::Write;
use utf8_path::path_relative_from;

/// This plugin will get the info of repository inside product bundle.
#[ffx_plugin()]
pub async fn pb_get_repository(
    cmd: GetRepositoryCommand,
    #[ffx(machine = Vec<RepositoryConfig>)] mut writer: Writer,
) -> Result<()> {
    let product_bundle = ProductBundle::try_load_from(&cmd.product_bundle)
        .context("Failed to load product bundle")?;
    let info = extract_repository_info(product_bundle, cmd)?;
    if writer.is_machine() {
        writer.machine(&info)?;
    } else {
        writeln!(writer, "{:#?}", info)?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepositoryInfo {
    name: String,
    target_json: Utf8PathBuf,
    blobs_dir: Utf8PathBuf,
    delivery_blob_type: u32,
}

fn extract_repository_info(
    product_bundle: ProductBundle,
    cmd: GetRepositoryCommand,
) -> Result<Vec<RepositoryInfo>> {
    let product_bundle = match product_bundle {
        ProductBundle::V2(pb) => pb,
    };
    let mut repository_infos = Vec::new();
    for repository in &product_bundle.repositories {
        let target_json = repository.metadata_path.join("targets.json");
        let blobs_dir = repository.blobs_path.clone();
        repository_infos.push(RepositoryInfo {
            name: repository.name.clone(),
            target_json: path_relative_from(target_json, &cmd.product_bundle)?,
            blobs_dir: path_relative_from(blobs_dir, &cmd.product_bundle)?,
            delivery_blob_type: repository.delivery_blob_type,
        })
    }
    Ok(repository_infos)
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_partitions_config::PartitionsConfig;

    use camino::Utf8Path;
    use sdk_metadata::ProductBundleV2;
    use sdk_metadata::Repository;

    #[test]
    fn test_get_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();
        let product_bundle_dir = dir.join("product_bundle");
        let blobs_dir = product_bundle_dir.join("blobs");

        let fuchsia_metadata_dir = product_bundle_dir.join("repository");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "test".into(),
            product_version: "test-product-version".into(),
            partitions: PartitionsConfig::default(),
            sdk_version: "test-sdk-version".into(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![Repository {
                name: "fuchsia.com".into(),
                metadata_path: fuchsia_metadata_dir.clone(),
                blobs_path: blobs_dir.clone(),
                delivery_blob_type: 1,
                root_private_key_path: None,
                targets_private_key_path: None,
                snapshot_private_key_path: None,
                timestamp_private_key_path: None,
            }],
            update_package_hash: None,
            virtual_devices_path: None,
        });

        let cmd = GetRepositoryCommand { product_bundle: product_bundle_dir };
        let info = extract_repository_info(pb.clone(), cmd).unwrap();
        let expected_info = vec![RepositoryInfo {
            name: String::from("fuchsia.com"),
            target_json: Utf8PathBuf::from("repository/targets.json"),
            blobs_dir: Utf8PathBuf::from("blobs"),
            delivery_blob_type: 1,
        }];
        assert_eq!(expected_info, info);
    }
}
