// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Representation of the product_bundle metadata.

mod v1;
mod v2;

use crate::VirtualDeviceManifest;
use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fuchsia_repo::repository::FileSystemRepository;
use serde::{Deserialize, Serialize};
use std::{fs::File, ops::Deref};

pub use v1::*;
pub use v2::{ProductBundleV2, Repository};

/// Returns a representation of a ProductBundle that has been loaded from disk.
///
/// The loaded product bundle holds a reference to the path that it was loaded
/// from so it can be referenced later. This helps when understanding how a
/// product bundle was loaded when it might have come from a default path.
///
/// Most users of the product bundle will not need to know, or care, where it
/// came from so they can just convert into a Product bundle using into().
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedProductBundle {
    product_bundle: ProductBundle,
    from_path: Utf8PathBuf,
}

impl LoadedProductBundle {
    /// Load a ProductBundle from a path on disk. This method will return a
    /// LoadedProductBundle which keeps track of where it was loaded from.
    pub fn try_load_from(path: impl AsRef<Utf8Path>) -> Result<Self> {
        let product_bundle_path = path.as_ref().join("product_bundle.json");
        let file = File::open(&product_bundle_path)
            .with_context(|| format!("opening product bundle: {:?}", &product_bundle_path))?;
        let helper: SerializationHelper =
            serde_json::from_reader(file).context("parsing product bundle")?;
        match helper {
            SerializationHelper::V1 { schema_id: _, data } => {
                Ok(LoadedProductBundle::new(ProductBundle::V1(data), path))
            }
            SerializationHelper::V2(SerializationHelperVersioned::V2(data)) => {
                let mut data = data.clone();
                data.canonicalize_paths(path.as_ref())?;
                Ok(LoadedProductBundle::new(ProductBundle::V2(data), path))
            }
        }
    }

    /// Creates a new LoadedProductBundle.
    ///
    /// Users should prefer the try_load_from method over creating this struct
    /// directly.
    pub fn new(product_bundle: ProductBundle, from_path: impl AsRef<Utf8Path>) -> Self {
        LoadedProductBundle { product_bundle, from_path: from_path.as_ref().into() }
    }

    /// Returns the path which the bundle was loaded from.
    pub fn loaded_from_path(&self) -> &Utf8Path {
        self.from_path.as_path()
    }
}

impl Deref for LoadedProductBundle {
    type Target = ProductBundle;
    fn deref(&self) -> &Self::Target {
        &self.product_bundle
    }
}

impl Into<ProductBundle> for LoadedProductBundle {
    fn into(self) -> ProductBundle {
        self.product_bundle
    }
}

/// Versioned product bundle.
#[derive(Clone, Debug, PartialEq)]
pub enum ProductBundle {
    V1(ProductBundleV1),
    V2(ProductBundleV2),
}

/// Private helper for serializing the ProductBundle. A ProductBundle cannot be deserialized
/// without going through `try_from_path` in order to require that we use this helper, and the
/// `directory` field gets populated.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
enum SerializationHelper {
    V1 { schema_id: String, data: ProductBundleV1 },
    V2(SerializationHelperVersioned),
}

/// Helper for serializing the new system of versioning product bundles using the "version" tag.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "version")]
enum SerializationHelperVersioned {
    #[serde(rename = "2")]
    V2(ProductBundleV2),
}

const PRODUCT_BUNDLE_SCHEMA_V1: &str =
    "http://fuchsia.com/schemas/sdk/product_bundle-6320eef1.json";

impl ProductBundle {
    pub fn try_load_from(path: impl AsRef<Utf8Path>) -> Result<Self> {
        LoadedProductBundle::try_load_from(path).map(|v| v.into())
    }

    /// Write a product bundle to a directory on disk at `path`.
    /// Note that this only writes the manifest file, and not the artifacts, images, blobs.
    pub fn write(&self, path: impl AsRef<Utf8Path>) -> Result<()> {
        let helper = match self {
            Self::V1(data) => SerializationHelper::V1 {
                schema_id: PRODUCT_BUNDLE_SCHEMA_V1.into(),
                data: data.clone(),
            },
            Self::V2(data) => {
                let mut data = data.clone();
                data.relativize_paths(path.as_ref())?;
                SerializationHelper::V2(SerializationHelperVersioned::V2(data))
            }
        };
        let product_bundle_path = path.as_ref().join("product_bundle.json");
        let file = File::create(product_bundle_path).context("creating product bundle file")?;
        serde_json::to_writer(file, &helper).context("writing product bundle file")?;
        Ok(())
    }

    /// Returns ProductBundle entry name.
    pub fn name(&self) -> &str {
        match self {
            Self::V1(data) => &data.name.as_str(),
            Self::V2(_) => panic!("no product name"),
        }
    }

    /// Get the list of logical device names.
    pub fn device_refs(&self) -> Result<Vec<String>> {
        match self {
            Self::V1(data) => Ok(data.device_refs.clone()),
            Self::V2(data) => {
                let path = data.get_virtual_devices_path();
                let manifest =
                    VirtualDeviceManifest::from_path(&path).context("manifest from_path")?;
                Ok(manifest.device_names())
            }
        }
    }

    /// Manifest for the emulator, if present.
    pub fn emu_manifest(&self) -> &Option<EmuManifest> {
        match self {
            Self::V1(data) => &data.manifests.emu,
            Self::V2(_) => panic!("no emu_manifest"),
        }
    }
}

/// Construct a Vec<FileSystemRepository> from product bundle.
pub fn get_repositories(product_bundle_dir: Utf8PathBuf) -> Result<Vec<FileSystemRepository>> {
    let pb = match ProductBundle::try_load_from(&product_bundle_dir)
        .with_context(|| format!("loading {}", product_bundle_dir))?
    {
        ProductBundle::V1(_) => {
            panic!(
                "This command does not support product bundle v1, \
                please use `ffx product-bundle get` to set up the repository."
            )
        }
        ProductBundle::V2(pb) => pb,
    };

    let mut repos = Vec::<FileSystemRepository>::new();
    for repo in pb.repositories {
        let mut repo_builder = FileSystemRepository::builder(
            repo.metadata_path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {:?}", repo.metadata_path))?
                .try_into()?,
            repo.blobs_path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {:?}", repo.blobs_path))?
                .try_into()?,
        )
        .alias(repo.name);
        if let Some(blob_type) = repo.delivery_blob_type {
            repo_builder = repo_builder.delivery_blob_type(Some(blob_type.try_into()?));
        }
        repos.push(repo_builder.build());
    }
    Ok(repos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Macro to create a v1 product bundle in the tmp directory
    macro_rules! make_pb_v1_in {
        ($dir:expr,$name:expr)=>{
            {
                let pb_dir = Utf8Path::from_path($dir.path()).unwrap();

                    let pb_file = File::create(pb_dir.join("product_bundle.json")).unwrap();
                    serde_json::to_writer(&pb_file, &json!({
                        "schema_id": "http://fuchsia.com/schemas/sdk/product_bundle-6320eef1.json",
                        "data": {
                            "name": $name,
                            "type": "product_bundle",
                            "device_refs": [$name],
                            "images": [{
                                "base_uri": "file://fuchsia/development/0.20201216.2.1/images/generic-x64.tgz",
                                "format": "tgz"
                            }],
                            "manifests": {
                            },
                            "packages": [{
                                "format": "tgz",
                                "repo_uri": "file://fuchsia/development/0.20201216.2.1/packages/generic-x64.tar.gz"
                            }]
                        }
                    })).unwrap();

                pb_dir
            }
        }
    }

    #[test]
    fn test_parse_v1() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v1_in!(tmp, "generic-x64");
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        assert!(matches!(pb.deref(), &ProductBundle::V1 { .. }));
    }

    #[test]
    fn test_parse_v2() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = Utf8Path::from_path(tmp.path()).unwrap();

        let pb_file = File::create(pb_dir.join("product_bundle.json")).unwrap();
        serde_json::to_writer(
            &pb_file,
            &json!({
                "version": "2",
                "product_name": "fake.pb-name",
                "product_version": "fake.pb-version",
                "sdk_version": "fake.sdk-version",
                "partitions": {
                    "hardware_revision": "board",
                    "bootstrap_partitions": [],
                    "bootloader_partitions": [],
                    "partitions": [],
                    "unlock_credentials": [],
                },
            }),
        )
        .unwrap();
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        assert!(matches!(pb.deref(), &ProductBundle::V2 { .. }));
    }

    #[test]
    fn test_loaded_from_path() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v1_in!(tmp, "generic-x64");
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        assert_eq!(pb_dir, pb.loaded_from_path());
    }

    #[test]
    fn test_loaded_product_bundle_into() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v1_in!(tmp, "generic-x64");
        let pb: ProductBundle = LoadedProductBundle::try_load_from(pb_dir).unwrap().into();
        assert!(matches!(pb, ProductBundle::V1 { .. }));
    }

    #[test]
    fn test_loaded_from_product_bundle_deref() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v1_in!(tmp, "generic-x64");
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();

        fn check_deref(_inner_pb: &ProductBundle) {
            // Just make sure we have a compile time check.
            assert!(true);
        }

        check_deref(&pb);
        assert!(matches!(*pb.deref(), ProductBundle::V1 { .. }));
    }
}
