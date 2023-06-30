// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use assembly_manifest::{AssemblyManifest, PackagesMetadata};
use assembly_subpackage_blobs_package::SubpackageBlobsPackageBuilder;
use camino::{Utf8Path, Utf8PathBuf};
use fuchsia_merkle::{Hash, MerkleTree};
use fuchsia_pkg::PackageManifest;
use std::collections::BTreeMap;
use std::fs::File;
use tracing::info;
use utf8_path::path_relative_from_current_dir;

#[derive(Debug)]
pub struct SubpackageBlobsPackage {
    pub merkle: Hash,
    pub contents: BTreeMap<Utf8PathBuf, Utf8PathBuf>,
    pub path: Utf8PathBuf,
    pub manifest: PackageManifest,
    pub manifest_path: Utf8PathBuf,
}

pub fn construct_subpackage_blobs_package(
    assembly_manifest: &AssemblyManifest,
    outdir: impl AsRef<Utf8Path>,
    gendir: impl AsRef<Utf8Path>,
    name: impl AsRef<str>,
) -> Result<SubpackageBlobsPackage> {
    let outdir = outdir.as_ref().join("subpackage_blobs");
    if !outdir.exists() {
        std::fs::create_dir_all(&outdir)?;
    }

    let mut subpackage_blobs_pkg_builder = SubpackageBlobsPackageBuilder::default();

    for image in &assembly_manifest.images {
        if let Some(contents) = image.get_blobfs_contents() {
            let PackagesMetadata { base, cache } = &contents.packages;

            for package in base.0.iter().chain(cache.0.iter()) {
                let manifest = PackageManifest::try_load_from(&package.manifest)?;
                subpackage_blobs_pkg_builder.add_subpackages_from_package(manifest)?;
            }
        }
    }

    let subpackage_blobs_package_path = outdir.join("meta.far");
    let build_results = subpackage_blobs_pkg_builder
        .build(&outdir, gendir, name, &subpackage_blobs_package_path)
        .context("Failed to build the subpackage blobs package")?;

    let subpackage_blobs_package = File::open(&subpackage_blobs_package_path)
        .context("Failed to open the subpackage blobs package")?;
    let subpackage_blobs_merkle = MerkleTree::from_reader(&subpackage_blobs_package)
        .context("Failed to calculate the subpackage blobs merkle")?
        .root();
    info!("SubpackageBlobs merkle: {}", &subpackage_blobs_merkle);

    let subpackage_blobs_package_path_relative =
        path_relative_from_current_dir(subpackage_blobs_package_path)?;

    Ok(SubpackageBlobsPackage {
        merkle: subpackage_blobs_merkle,
        contents: build_results.contents,
        path: subpackage_blobs_package_path_relative,
        manifest: build_results.manifest,
        manifest_path: build_results.manifest_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use assembly_manifest::{BlobfsContents, Image};
    use fuchsia_archive::Utf8Reader;
    use fuchsia_pkg::PackageBuilder;
    use pretty_assertions::assert_eq;
    use std::collections::{BTreeSet, HashMap};
    use std::fs::File;
    use tempfile::tempdir;
    use utf8_path::path_relative_from_current_dir;

    fn create_package(
        root: &Utf8Path,
        name: &str,
        subpackages: &[(&PackageManifest, &Utf8Path)],
    ) -> (PackageManifest, Utf8PathBuf) {
        let dir = root.join(name);

        let mut builder = PackageBuilder::new(name);

        // Hardcode the ABI so it doesn't change when the ABI revision is bumped.
        builder.abi_revision(0x57904F5A17FA3B22);

        let blob_name = format!("{}-blob", name);
        builder.add_contents_as_blob(&blob_name, &blob_name, &dir).unwrap();

        let meta_name = format!("meta/{}-meta", name);
        builder.add_contents_to_far(&meta_name, &meta_name, &dir).unwrap();

        for (subpackage_manifest, subpackage_path) in subpackages {
            builder
                .add_subpackage(
                    &subpackage_manifest.name().clone().into(),
                    subpackage_manifest.hash(),
                    subpackage_path.into(),
                )
                .unwrap();
        }

        let manifest = builder.build(&dir, dir.join("meta.far")).unwrap();

        let manifest_path = dir.join("package_manifest.json");
        serde_json::to_writer(File::create(&manifest_path).unwrap(), &manifest).unwrap();

        (manifest, manifest_path)
    }

    #[test]
    fn construct() {
        let tmp = tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();

        // Prepare package manifests to add to the subpackage blobs package.
        let (base_child_manifest, base_child_path) = create_package(dir, "base_child", &[]);
        let (base_parent_manifest, base_parent_path) =
            create_package(dir, "base_parent", &[(&base_child_manifest, &base_child_path)]);

        let (cache_child_manifest, cache_child_path) = create_package(dir, "cache_child", &[]);
        let (cache_parent_manifest, cache_parent_path) =
            create_package(dir, "cache_parent", &[(&cache_child_manifest, &cache_child_path)]);

        let merkle_size_map = base_child_manifest
            .blobs()
            .iter()
            .chain(base_parent_manifest.blobs().iter())
            .chain(cache_child_manifest.blobs().iter())
            .chain(cache_parent_manifest.blobs().iter())
            .map(|blob| (blob.merkle.to_string(), blob.size))
            .collect::<HashMap<_, _>>();

        let mut blobfs_contents = BlobfsContents::default();
        blobfs_contents.add_base_package(&base_parent_path, &merkle_size_map).unwrap();
        blobfs_contents.add_cache_package(&cache_parent_path, &merkle_size_map).unwrap();

        let assembly_manifest = AssemblyManifest {
            images: vec![Image::BlobFS {
                path: "does-not-exist".into(),
                contents: blobfs_contents,
            }],
        };

        // Construct the subpackage blobs package.
        let subpackage_blobs_package =
            construct_subpackage_blobs_package(&assembly_manifest, dir, dir, "subpackage_blobs")
                .unwrap();

        assert_eq!(
            subpackage_blobs_package.path,
            path_relative_from_current_dir(dir.join("subpackage_blobs/meta.far")).unwrap()
        );

        // Collect all the expected subpackage merkles from the subpackages.
        let expected_merkles = base_child_manifest
            .blobs()
            .iter()
            .chain(cache_child_manifest.blobs().iter())
            .map(|blob| blob.merkle.to_string())
            .collect::<BTreeSet<_>>();

        // Read the base package, and assert the contents are correct.
        let subpackage_blobs_package_file = File::open(subpackage_blobs_package.path).unwrap();
        let mut far_reader = Utf8Reader::new(&subpackage_blobs_package_file).unwrap();
        let contents = far_reader.read_file("meta/contents").unwrap();
        let contents = std::str::from_utf8(&contents).unwrap();

        let mut expected_contents = String::new();
        for merkle in expected_merkles {
            expected_contents.push_str(&format!("{}={}\n", merkle, merkle));
        }
        assert_eq!(contents, expected_contents);
    }
}
