// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::operations::size_check::common::PackageSizeInfo;
use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use serde::Serialize;
use std::fs;

// The tree structure that needs to be generated for the HTML visualization.
// See "tree_data" in template/D3BlobTreeMap.js.
#[derive(Serialize)]
struct VisualizationRootNode {
    #[serde(rename = "n")]
    name: String,
    children: Vec<VisualizationPackageNode>,
    #[serde(rename = "k")]
    kind: String,
}

#[derive(Serialize)]
struct VisualizationPackageNode {
    #[serde(rename = "n")]
    name: String,
    children: Vec<VisualizationBlobNode>,
    #[serde(rename = "k")]
    kind: String,
}

#[derive(Serialize)]
struct VisualizationBlobNode {
    #[serde(rename = "n")]
    name: String,
    #[serde(rename = "k")]
    kind: String,
    #[serde(rename = "t")]
    uniqueness: String,
    #[serde(rename = "value")]
    proportional_size: u64,
    #[serde(rename = "originalSize")]
    original_size: u64,
    #[serde(rename = "c")]
    share_count: u64,
}

pub fn generate_visualization(
    visualization_dir: &Utf8PathBuf,
    package_sizes: &Vec<PackageSizeInfo>,
) -> Result<()> {
    fs::create_dir_all(visualization_dir.join("d3_v3"))
        .context("creating d3_v3 directory for visualization")?;
    fs::write(
        visualization_dir.join("d3_v3").join("LICENSE"),
        include_bytes!("../../../../../../../../scripts/third_party/d3_v3/LICENSE"),
    )
    .context("creating LICENSE file for visualization")?;
    fs::write(
        visualization_dir.join("d3_v3").join("d3.js"),
        include_bytes!("../../../../../../../../scripts/third_party/d3_v3/d3.js"),
    )
    .context("creating d3.js file for visualization")?;
    fs::write(visualization_dir.join("D3BlobTreeMap.js"), include_bytes!("D3BlobTreeMap.js"))
        .context("creating D3BlobTreeMap.js file for visualization")?;
    fs::write(visualization_dir.join("index.html"), include_bytes!("index.html"))
        .context("creating index.html file for visualization")?;
    fs::write(
        visualization_dir.join("data.js"),
        format!(
            "var tree_data={}",
            serde_json::to_string(&generate_visualization_tree(&package_sizes))?
        ),
    )
    .context("creating data.js for visualization")
}

#[allow(clippy::ptr_arg)]
fn generate_visualization_tree(package_sizes: &Vec<PackageSizeInfo>) -> VisualizationRootNode {
    VisualizationRootNode {
        name: "packages".to_string(),
        kind: "p".to_string(),
        children: package_sizes
            .iter()
            .map(|package_size| VisualizationPackageNode {
                name: package_size.name.clone(),
                kind: "p".to_string(),
                children: package_size
                    .blobs
                    .iter()
                    .map(|blob| VisualizationBlobNode {
                        name: blob.path_in_package.clone(),
                        kind: "s".to_string(),
                        uniqueness: if blob.share_count == 1 {
                            "unique".to_string()
                        } else {
                            "shared".to_string()
                        },
                        proportional_size: blob.used_space_in_blobfs / blob.share_count,
                        original_size: blob.used_space_in_blobfs,
                        share_count: blob.share_count,
                    })
                    .collect::<Vec<_>>(),
            })
            .collect::<Vec<_>>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::size_check::common::PackageBlobSizeInfo;
    use fuchsia_hash::Hash;
    use serde_json::json;
    use std::str::FromStr;

    #[test]
    fn verify_visualization_tree_test() -> Result<()> {
        let blob1_info = PackageBlobSizeInfo {
            merkle: Hash::from_str(
                "7ddff816740d5803358dd4478d8437585e8d5c984b4361817d891807a16ff581",
            )?,
            path_in_package: "bin/defg".to_string(),
            used_space_in_blobfs: 20,
            share_count: 1,
            absolute_share_count: 0,
        };
        let blob2_info = PackageBlobSizeInfo {
            merkle: Hash::from_str(
                "8cb3466c6e66592c8decaeaa3e399652fbe71dad5c3df1a5e919743a33815568",
            )?,
            path_in_package: "lib/ghij".to_string(),
            used_space_in_blobfs: 60,
            share_count: 2,
            absolute_share_count: 0,
        };
        let blob3_info = PackageBlobSizeInfo {
            merkle: Hash::from_str(
                "eabdb84d26416c1821fd8972e0d835eedaf7468e5a9ebe01e5944462411aec71",
            )?,
            path_in_package: "abcd/".to_string(),
            used_space_in_blobfs: 40,
            share_count: 1,
            absolute_share_count: 0,
        };
        let package_size_infos = vec![
            PackageSizeInfo {
                name: "package1".to_string(),
                used_space_in_blobfs: 80,
                proportional_size: 50,
                blobs: vec![blob1_info, blob2_info.clone()],
            },
            PackageSizeInfo {
                name: "package2".to_string(),
                used_space_in_blobfs: 100,
                proportional_size: 70,
                blobs: vec![blob2_info, blob3_info],
            },
        ];
        let visualization_tree = generate_visualization_tree(&package_size_infos);
        assert_eq!(
            serde_json::to_value(visualization_tree)?,
            json!(
                {
                    "n": "packages",
                    "children": [
                        {
                            "n": "package1",
                            "children": [
                                {
                                    "n": "bin/defg",
                                    "k": "s",
                                    "t": "unique",
                                    "value": 20,
                                    "originalSize": 20,
                                    "c": 1
                                },
                                {
                                    "n": "lib/ghij",
                                    "k": "s",
                                    "t": "shared",
                                    "value": 30,
                                    "originalSize": 60,
                                    "c": 2
                                }
                            ],
                            "k": "p"
                        },
                        {
                            "n": "package2",
                            "children": [
                                {
                                    "n": "lib/ghij",
                                    "k": "s",
                                    "t": "shared",
                                    "value": 30,
                                    "originalSize": 60,
                                    "c": 2
                                },
                                {
                                    "n": "abcd/",
                                    "k": "s",
                                    "t": "unique",
                                    "value": 40,
                                    "originalSize": 40,
                                    "c": 1
                                }
                            ],
                            "k": "p"
                        }
                    ],
                    "k": "p"
                }
            )
        );
        Ok(())
    }
}
