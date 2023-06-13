// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::base_package::BasePackage;

use anyhow::{Context, Result};
use assembly_config_schema::ImageAssemblyConfig;
use assembly_fxfs::FxfsBuilder;
use assembly_images_config::Fxfs;
use assembly_manifest::BlobfsContents;
use camino::{Utf8Path, Utf8PathBuf};
use std::collections::HashMap;

/// Constructs an Fxfs image.  Returns the image path and the blob contents in the image.
pub async fn construct_fxfs(
    outdir: impl AsRef<Utf8Path>,
    gendir: impl AsRef<Utf8Path>,
    image_config: &ImageAssemblyConfig,
    base_package: &BasePackage,
    fxfs_config: &Fxfs,
) -> Result<(Utf8PathBuf, BlobfsContents)> {
    let mut contents = BlobfsContents::default();
    let mut fxfs_builder = FxfsBuilder::new();
    fxfs_builder.set_size(fxfs_config.size_bytes);

    // Add the base and cache packages.
    for package_manifest_path in &image_config.base {
        fxfs_builder.add_package_from_path(package_manifest_path)?;
    }
    for package_manifest_path in &image_config.cache {
        fxfs_builder.add_package_from_path(package_manifest_path)?;
    }

    // Add the base package and its contents.
    fxfs_builder.add_package_from_path(&base_package.manifest_path)?;

    // Build the fxfs and store the merkle to size map.
    let fxfs_path = outdir.as_ref().join("fxfs.blk");
    let blobs_json_path =
        fxfs_builder.build(gendir, &fxfs_path).await.context("Failed to build the Fxfs image")?;
    let merkle_size_map = assembly_fxfs::read_blobs_json(blobs_json_path)
        .map(|blobs_json| {
            blobs_json
                .iter()
                .map(|e| (e.merkle.to_string(), e.used_space_in_blobfs))
                .collect::<HashMap<String, u64>>()
        })
        .context("Failed to parse blobs JSON")?;
    for package_manifest_path in &image_config.base {
        contents.add_base_package(package_manifest_path, &merkle_size_map)?;
    }
    for package_manifest_path in &image_config.cache {
        contents.add_cache_package(package_manifest_path, &merkle_size_map)?;
    }
    contents.add_base_package(&base_package.manifest_path, &merkle_size_map)?;
    Ok((fxfs_path, contents))
}

#[cfg(test)]
mod tests {
    use super::construct_fxfs;
    use crate::base_package::construct_base_package;
    use assembly_config_schema::ImageAssemblyConfig;
    use assembly_images_config::Fxfs;
    use assembly_manifest::AssemblyManifest;
    use camino::{Utf8Path, Utf8PathBuf};
    use serde_json::json;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;
    use utf8_path::path_relative_from_current_dir;

    // Generates a package manifest to be used for testing. The file is written
    // into `dir`, and the location is returned. The `name` is used in the blob
    // file names to make each manifest somewhat unique.
    // TODO(fxbug.dev/76993): See if we can share this with BasePackage.
    pub fn generate_test_manifest_file(
        dir: impl AsRef<Utf8Path>,
        name: impl AsRef<str>,
    ) -> Utf8PathBuf {
        // Create a data file for the package.
        let data_file_name = format!("{}_data.txt", name.as_ref());
        let data_path = dir.as_ref().join(&data_file_name);
        let data_file = File::create(&data_path).unwrap();
        write!(&data_file, "bleh").unwrap();

        // Create the manifest.
        let manifest_path = dir.as_ref().join(format!("{}.json", name.as_ref()));
        let manifest_file = File::create(&manifest_path).unwrap();
        serde_json::to_writer(
            &manifest_file,
            &json!({
                    "version": "1",
                    "repository": "testrepository.com",
                    "package": {
                        "name": name.as_ref(),
                        "version": "1",
                    },
                    "blobs": [
                        {
                            "source_path": format!("path/to/{}/meta.far", name.as_ref()),
                            "path": "meta/",
                            "merkle":
                                "0000000000000000000000000000000000000000000000000000000000000000",
                            "size": 1
                        },
                        {
                            "source_path": &data_path,
                            "path": &data_file_name,
                            "merkle":
                                "1111111111111111111111111111111111111111111111111111111111111111",
                            "size": 1
                        },
                    ],
                }
            ),
        )
        .unwrap();
        manifest_path
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn construct() {
        let tmp = tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();

        let image_config = ImageAssemblyConfig::new_for_testing("kernel", 0);

        // Create a fake base package.
        let system_manifest = generate_test_manifest_file(dir, "extra_base");
        let base_manifest = generate_test_manifest_file(dir, "test_static");
        let cache_manifest = generate_test_manifest_file(dir, "test_cache");
        let mut product_config = ImageAssemblyConfig::new_for_testing("kernel", 0);
        product_config.system.push(system_manifest);
        product_config.base.push(base_manifest);
        product_config.cache.push(cache_manifest);

        // Construct the base package.
        let mut assembly_manifest = AssemblyManifest::default();
        let base_package = construct_base_package(
            &mut assembly_manifest,
            dir,
            dir,
            "system_image",
            &product_config,
        )
        .unwrap();
        assert_eq!(
            base_package.path,
            path_relative_from_current_dir(dir.join("base/meta.far")).unwrap()
        );

        let (image_path, blobs) = construct_fxfs(
            dir,
            dir,
            &image_config,
            &base_package,
            &Fxfs { size_bytes: 32 * 1024 * 1024 },
        )
        .await
        .unwrap();

        // Ensure something was created.
        assert!(image_path.exists());
        assert_eq!(32 * 1024 * 1024, std::fs::metadata(image_path).unwrap().len());

        // Ensure the blobs match expectations.
        let blobs = blobs.relativize(dir).unwrap();
        assert!(!blobs.packages.base.0.is_empty());
        assert!(blobs.packages.cache.0.is_empty());
    }
}
