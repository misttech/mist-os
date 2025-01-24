// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Representation of the product_bundle metadata.

mod v2;

use crate::{VirtualDevice, VirtualDeviceManifest, VirtualDeviceV1};
use anyhow::{anyhow, bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fuchsia_repo::repository::FileSystemRepository;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufRead;
use std::ops::Deref;
use v2::Canonicalizer;
use zip::read::ZipArchive;

pub use v2::{ProductBundleV2, Repository, Type};

fn try_load_product_bundle(r: impl BufRead) -> Result<ProductBundle> {
    let helper: SerializationHelper =
        serde_json::from_reader(r).context("parsing product bundle")?;
    match helper {
        SerializationHelper::V1 { schema_id: _ } => {
            bail!("Product Bundle v1 is no longer supported")
        }
        SerializationHelper::V2(SerializationHelperVersioned::V2(data)) => {
            Ok(ProductBundle::V2(data))
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ZipLoadedProductBundle {
    product_bundle: ProductBundle,
}

impl ZipLoadedProductBundle {
    pub fn try_load_from(product_bundle_zip_path: impl AsRef<Utf8Path>) -> Result<Self> {
        let path = product_bundle_zip_path.as_ref();
        let file =
            File::open(path).with_context(|| format!("opening product bundle zip: {:?}", &path))?;
        let zip =
            ZipArchive::new(file).with_context(|| format!("loading zip file: {:?}", &path))?;
        Self::load_from(zip)
    }

    pub fn load_from(mut zip: ZipArchive<File>) -> Result<Self> {
        let product_bundle_manifest_name = zip
            .file_names()
            .find(|x| x == &"product_bundle.json" || x.ends_with("/product_bundle.json"))
            .ok_or_else(|| anyhow!("finding file 'product_bundle.json' in zip archive"))?
            .to_owned();

        let product_bundle_parent_path =
            product_bundle_manifest_name.strip_suffix("product_bundle.json").ok_or_else(|| anyhow!("despite the product_bundle.json being found, it's path did not include it as a suffix"))?;

        let product_bundle_manifest = zip
            .by_name(&product_bundle_manifest_name)
            .with_context(|| format!("getting 'product_bundle.json' in zip archive"))?;
        let product_bundle_manifest = std::io::BufReader::new(product_bundle_manifest);
        // Still need to canonicalize paths as the path to the product bundle'suffix
        // parent directory may be arbitrarily deep in the zip file
        match try_load_product_bundle(product_bundle_manifest)? {
            ProductBundle::V2(data) => {
                let mut data = data;
                let mut canonicalizer = ZipCanonicalizer::new(product_bundle_parent_path);
                data.canonicalize_paths_with(product_bundle_parent_path, &mut canonicalizer)
                    .with_context(|| {
                        format!("Canonicalizing paths from {:?}", product_bundle_parent_path)
                    })?;
                Ok(Self::new(ProductBundle::V2(data)))
            }
        }
    }

    pub fn new(product_bundle: ProductBundle) -> Self {
        Self { product_bundle }
    }
}

impl Deref for ZipLoadedProductBundle {
    type Target = ProductBundle;
    fn deref(&self) -> &Self::Target {
        &self.product_bundle
    }
}

impl Into<ProductBundle> for ZipLoadedProductBundle {
    fn into(self) -> ProductBundle {
        self.product_bundle
    }
}

struct ZipCanonicalizer {
    product_bundle_dir: Utf8PathBuf,
}

impl Canonicalizer for ZipCanonicalizer {
    fn root_path(&self) -> &Utf8PathBuf {
        &self.product_bundle_dir
    }

    fn canonicalize_path(
        &self,
        path: impl AsRef<Utf8Path>,
        _image_types: Vec<Type>,
    ) -> Utf8PathBuf {
        self.root_path().join(path)
    }
}

impl ZipCanonicalizer {
    fn new(product_bundle_dir: impl Into<Utf8PathBuf>) -> Self {
        Self { product_bundle_dir: product_bundle_dir.into() }
    }
}

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
    /// Load a ProductBundle from a directory containing product_bundle.json
    /// on disk. This method will return a LoadedProductBundle which keeps
    /// track of where it was loaded from.
    pub fn try_load_from(path: impl AsRef<Utf8Path>) -> Result<Self> {
        if !path.as_ref().is_dir() {
            anyhow::bail!("{} is not a directory", path.as_ref().as_str());
        }
        let product_bundle_path = path.as_ref().join("product_bundle.json");
        let file = File::open(&product_bundle_path)
            .map_err(|e| anyhow!("{e}: {product_bundle_path:?}"))?;
        let file = std::io::BufReader::new(file);

        match try_load_product_bundle(file)? {
            ProductBundle::V2(data) => {
                let mut data = data;
                data.canonicalize_paths(path.as_ref())
                    .with_context(|| format!("Canonicalizing paths from {:?}", path.as_ref()))?;
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
    V2(ProductBundleV2),
}

/// Private helper for serializing the ProductBundle. A ProductBundle cannot be deserialized
/// without going through `try_from_path` in order to require that we use this helper, and the
/// `directory` field gets populated.
// TODO(https://fxbug.dev/324167674): fix.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
enum SerializationHelper {
    V1 { schema_id: String },
    V2(SerializationHelperVersioned),
}

/// Helper for serializing the new system of versioning product bundles using the "version" tag.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "version")]
enum SerializationHelperVersioned {
    #[serde(rename = "2")]
    V2(ProductBundleV2),
}

impl ProductBundle {
    pub fn try_load_from(path: impl AsRef<Utf8Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.is_file() && path.extension() == Some("zip") {
            ZipLoadedProductBundle::try_load_from(path).map(|v| v.into())
        } else {
            LoadedProductBundle::try_load_from(path).map(|v| v.into())
        }
    }

    /// Write a product bundle to a directory on disk at `path`.
    /// Note that this only writes the manifest file, and not the artifacts, images, blobs.
    pub fn write(&self, path: impl AsRef<Utf8Path>) -> Result<()> {
        let helper = match self {
            Self::V2(data) => {
                let mut data = data.clone();
                data.relativize_paths(path.as_ref())?;
                SerializationHelper::V2(SerializationHelperVersioned::V2(data))
            }
        };
        let product_bundle_path = path.as_ref().join("product_bundle.json");
        let file = File::create(product_bundle_path).context("creating product bundle file")?;
        serde_json::to_writer_pretty(file, &helper).context("writing product bundle file")?;
        Ok(())
    }

    /// Get the list of logical device names.
    pub fn device_refs(&self) -> Result<Vec<String>> {
        match self {
            Self::V2(data) => {
                let path = data.get_virtual_devices_path();
                let manifest =
                    VirtualDeviceManifest::from_path(&path).context("manifest from_path")?;
                Ok(manifest.device_names())
            }
        }
    }

    /// Get the product bundle name
    pub fn get_product_bundle_name(&self) -> String {
        match self {
            Self::V2(pb) => pb.product_name.clone(),
        }
    }

    /// Attempts to load a `VirtualDeviceV1` from the product bundle with the
    /// given `device` name.
    /// If `device` is empty, loads the default recommended device instead.
    /// If `device` does not exist within the product bundle, `device` is
    /// instead interpreted as a virtual device file path.
    pub fn get_device(&self, device: &Option<String>) -> Result<VirtualDeviceV1> {
        let Self::V2(pb) = self;

        // Determine the correct device name from the user, or default to the "recommended"
        // device, if one is provided in the product bundle.
        let path = pb.get_virtual_devices_path();
        let manifest = VirtualDeviceManifest::from_path(&path).context("manifest from_path")?;
        let result = match device.as_deref() {
            // If no device is given, return the default specified in the manifest.
            None | Some("") => manifest.default_device(),

            // Otherwise, find the virtual device by name in the product bundle.
            Some(device) => manifest
                .device(device)
                .or_else(|name_err| {
                    // If we cannot find it in the product bundle, attempt to parse
                    // `device` as a virtual device file path.
                    VirtualDevice::try_load_from(Utf8Path::new(device)).map_err(|file_err| {
                        anyhow!(
                            "No virtual device matches '{}': {}\n\
                            We were also not able to parse '{}' as a virtual device file: {}",
                            device,
                            name_err,
                            device,
                            file_err
                        )
                    })
                })
                .map(|d| Some(d)),
        }?;
        match result {
            Some(VirtualDevice::V1(virtual_device)) => Ok(virtual_device),
            None => bail!("No default virtual device is available, please specify one by name."),
        }
    }
}

/// Construct a Vec<FileSystemRepository> from product bundle.
pub fn get_repositories(product_bundle_dir: Utf8PathBuf) -> Result<Vec<FileSystemRepository>> {
    let pb = match ProductBundle::try_load_from(&product_bundle_dir)
        .with_context(|| format!("loading {}", product_bundle_dir))?
    {
        ProductBundle::V2(pb) => pb,
    };

    let mut repos = Vec::<FileSystemRepository>::new();
    for repo in pb.repositories {
        let repo_builder = FileSystemRepository::builder(
            repo.metadata_path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {:?}", repo.metadata_path))?
                .try_into()?,
            repo.blobs_path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {:?}", repo.blobs_path))?
                .try_into()?,
        )
        .alias(repo.name)
        .delivery_blob_type(repo.delivery_blob_type.try_into()?);
        repos.push(repo_builder.build());
    }
    Ok(repos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::FileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const VIRTUAL_DEVICE_VALID: &str =
        include_str!("../../../../build/sdk/meta/test_data/virtual_device.json");

    fn make_sample_pbv1(name: &str) -> serde_json::Value {
        json!({
            "schema_id": "http://fuchsia.com/schemas/sdk/product_bundle-6320eef1.json",
            "data": {
                "name": name,
                "type": "product_bundle",
                "device_refs": [name],
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
        })
    }

    /// Macro to create a v1 product bundle in the tmp directory
    macro_rules! make_pb_v1_in {
        ($dir:expr,$name:expr) => {{
            let pb_dir = Utf8Path::from_path($dir.path()).unwrap();

            let pb_file = File::create(pb_dir.join("product_bundle.json")).unwrap();
            serde_json::to_writer(&pb_file, &make_sample_pbv1($name)).unwrap();

            pb_dir
        }};
    }

    fn make_sample_pbv2(name: &str, virtual_devices_path: Option<String>) -> serde_json::Value {
        json!({
            "version": "2",
            "product_name": name,
            "product_version": "fake.pb-version",
            "sdk_version": "fake.sdk-version",
            "partitions": {
                "hardware_revision": "board",
                "bootstrap_partitions": [],
                "bootloader_partitions": [],
                "partitions": [],
                "unlock_credentials": [],
            },
            "virtual_devices_path": virtual_devices_path,
        })
    }
    /// Macro to create a v1 product bundle in the tmp directory
    macro_rules! make_pb_v2_in {
        ($dir:expr,$name:expr) => {{
            let pb_dir = Utf8Path::from_path($dir.path()).unwrap();

            let pb_file = File::create(pb_dir.join("product_bundle.json")).unwrap();
            serde_json::to_writer(&pb_file, &make_sample_pbv2($name, Some("virtual_device_manifest.json".into()))).unwrap();

            let dev_manifest = pb_dir.join("virtual_device_manifest.json");
            fs::write(&dev_manifest,r#"
            {"recommended":"virtual_device_1","device_paths":{"virtual_device_1":"virtual_device_1.json","virtual_device_2":"virtual_device_2.json"}}
            "#).unwrap();

            fs::write(pb_dir.join("virtual_device_1.json"), VIRTUAL_DEVICE_VALID)
                .expect("writing device json");

            pb_dir
        }};
    }

    #[test]
    fn test_parse_v1() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v1_in!(tmp, "generic-x64");
        assert!(LoadedProductBundle::try_load_from(pb_dir).is_err());
    }

    #[test]
    fn test_parse_v2() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = Utf8Path::from_path(tmp.path()).unwrap();

        let pb_file = File::create(pb_dir.join("product_bundle.json")).unwrap();
        serde_json::to_writer(&pb_file, &make_sample_pbv2(&"fake.pb-name", None)).unwrap();
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        assert!(matches!(pb.deref(), &ProductBundle::V2 { .. }));
    }

    #[test]
    fn test_loaded_from_path() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v2_in!(tmp, "generic-x64");
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        assert_eq!(pb_dir, pb.loaded_from_path());
    }

    #[test]
    fn test_loaded_product_bundle_into() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v2_in!(tmp, "generic-x64");
        let pb: ProductBundle = LoadedProductBundle::try_load_from(pb_dir).unwrap().into();
        assert!(matches!(pb, ProductBundle::V2 { .. }));
    }

    #[test]
    fn test_loaded_from_product_bundle_deref() {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v2_in!(tmp, "generic-x64");
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();

        fn check_deref(_inner_pb: &ProductBundle) {
            // Just make sure we have a compile time check.
            assert!(true);
        }

        check_deref(&pb);
        assert!(matches!(*pb.deref(), ProductBundle::V2 { .. }));
    }

    #[test]
    fn test_zip_loaded() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();

        let pb = make_sample_pbv2("generic-x64", None);
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;

        let _ = ZipLoadedProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap())?;

        Ok(())
    }

    #[test]
    fn test_zip_product_bundle_into() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();
        let pb = make_sample_pbv2("generic-x64", None);
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;
        let pb: ProductBundle =
            ZipLoadedProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap())?
                .into();
        assert!(matches!(pb, ProductBundle::V2 { .. }));
        Ok(())
    }

    #[test]
    fn test_zip_from_product_bundle_deref() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();
        let pb = make_sample_pbv2("generic-x64", None);
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;

        let pb = ZipLoadedProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap())?;

        fn check_deref(_inner_pb: &ProductBundle) {
            // Just make sure we have a compile time check.
            assert!(true);
        }

        check_deref(&pb);
        assert!(matches!(*pb.deref(), ProductBundle::V2 { .. }));
        Ok(())
    }

    #[test]
    fn test_product_bundle_try_load_from_for_zip() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();

        let pb = make_sample_pbv2("generic-x64", None);
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;

        // This should detect zip file and load from the zip
        let _ = ProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap())?;

        Ok(())
    }

    #[test]
    fn test_no_file_fail_zip() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();

        let pb = make_sample_pbv2("generic-x64", None);
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("for_sure_not_a_product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;

        // This should detect zip file and load from the zip
        assert!(ProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap()).is_err());

        Ok(())
    }

    #[test]
    fn test_product_bundle_try_load_from_for_zip_deep_path() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();

        let pb = make_sample_pbv2("generic-x64", None);
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        // Now start the file deeper in the tree
        zip.start_file("foo/bar/baz/biz/product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;

        // This should detect zip file and load from the zip
        let _ = ProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap())?;

        Ok(())
    }

    #[test]
    fn test_parse_v1_from_zip_fails() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();
        let pb = make_sample_pbv1("generic-x64");
        let pb_filename = tmp.into_path().join("pb.zip");
        let pb_file = File::create(pb_filename.clone())?;

        let mut zip = ZipWriter::new(pb_file);
        let options = FileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("for_sure_not_a_product_bundle.json", options)?;
        let buf = serde_json::to_vec(&pb)?;
        let _ = zip.write(&buf)?;
        zip.flush()?;
        let _ = zip.finish()?;

        // This should fail as pbv1 is no longer supported.
        assert!(ProductBundle::try_load_from(Utf8Path::from_path(&pb_filename).unwrap()).is_err());
        Ok(())
    }

    #[test]
    fn test_product_bundle_try_load_from_for_dir() -> anyhow::Result<()> {
        let tmp = TempDir::new().unwrap();
        let pb_dir = make_pb_v2_in!(tmp, "generic-x64");
        let _ = ProductBundle::try_load_from(pb_dir).unwrap();
        Ok(())
    }

    #[test]
    fn test_get_device_from_path_absolute() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let pb_dir = make_pb_v2_in!(temp_dir, "generic-x64");

        let VirtualDevice::V1(expected) =
            VirtualDevice::try_load_from(pb_dir.join("virtual_device_1.json")).unwrap();

        let absolute_path = pb_dir.join("virtual_device_1.json");
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        let actual = pb.get_device(&Some(absolute_path.into_string()));

        assert!(actual.is_ok());
        assert_eq!(expected, actual?);
        Ok(())
    }

    #[test]
    fn test_get_device_from_path_relative() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let pb_dir = make_pb_v2_in!(temp_dir, "generic-x64");

        let VirtualDevice::V1(expected) =
            VirtualDevice::try_load_from(pb_dir.join("virtual_device_1.json")).unwrap();

        let absolute_path = pb_dir.join("virtual_device_1.json");
        let relative_path =
            pathdiff::diff_paths(absolute_path, std::env::current_dir().unwrap()).unwrap();
        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        let actual = pb.get_device(&Some(relative_path.to_str().unwrap().to_string()));

        assert!(actual.is_ok());
        assert_eq!(expected, actual?);
        Ok(())
    }

    #[test]
    fn test_get_device_from_name() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let pb_dir = make_pb_v2_in!(temp_dir, "generic-x64");

        let VirtualDevice::V1(expected) =
            VirtualDevice::try_load_from(pb_dir.join("virtual_device_1.json")).unwrap();

        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        let actual = pb.get_device(&Some("virtual_device_1".into()));

        assert!(actual.is_ok());
        assert_eq!(expected, actual?);
        Ok(())
    }

    #[test]
    fn test_get_device_from_name_nondefault_device() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let pb_dir = make_pb_v2_in!(temp_dir, "generic-x64");
        fs::rename(pb_dir.join("virtual_device_1.json"), pb_dir.join("virtual_device_2.json"))
            .expect("remove default virtual device, create non-default virtual device");

        let VirtualDevice::V1(expected) =
            VirtualDevice::try_load_from(pb_dir.join("virtual_device_2.json")).unwrap();

        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        let actual = pb.get_device(&Some("virtual_device_2".into()));

        assert!(actual.is_ok());
        assert_eq!(expected, actual?);
        Ok(())
    }

    #[test]
    fn test_get_device_default_device() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let pb_dir = make_pb_v2_in!(temp_dir, "generic-x64");

        let VirtualDevice::V1(expected) =
            VirtualDevice::try_load_from(pb_dir.join("virtual_device_1.json")).unwrap();

        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        let actual = pb.get_device(&None);

        assert!(actual.is_ok());
        assert_eq!(expected, actual?);
        Ok(())
    }

    #[test]
    fn test_get_device_from_invalid_name() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let pb_dir = make_pb_v2_in!(temp_dir, "generic-x64");

        let pb = LoadedProductBundle::try_load_from(pb_dir).unwrap();
        let actual = pb.get_device(&Some("invalid_device".into()));

        assert!(actual.is_err());
        Ok(())
    }
}
