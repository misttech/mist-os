// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO: Exercise all crate-local code in tests and/or other modules.
#![allow(dead_code)]

use super::api;
use super::blob::BlobDirectory;
use super::blob::BlobDirectoryError;
use super::blob::BlobSet;
use super::blob::CompositeBlobSet;
use super::data_source as ds;
use super::hash::Hash;
use camino::Utf8PathBuf;
use derivative::Derivative;
use once_cell::sync::OnceCell;
use sdk_metadata as sdk;
use std::path::PathBuf;
use std::rc::Rc;
use thiserror::Error;

/// A system slot under which images may be grouped in a product bundle. See
/// https://fuchsia.dev/fuchsia-src/glossary?hl=en#abr for details.
#[derive(Debug, Eq, Hash, PartialEq)]
pub enum SystemSlot {
    A,
    B,
    R,
}

/// Errors that may be encountered in [`ProductBundle::new`].
#[derive(Debug, Error)]
pub enum ProductBundleError {
    #[error("product bundle directory path is not a valid UTF8 string: {directory:?}")]
    InvalidDirectory { directory: PathBuf },
    #[error("failed to deserialize product bundle: {error}")]
    DeserializationFailure { error: anyhow::Error },
    #[error(
        "attempted to build product bundle from unsupported product bundle format version: {version}"
    )]
    InvalidVerison { version: String },
    #[error("attempted to build product bundle with no update package")]
    MissingUpdatePackage,
}

mod data_source {
    use super::super::api;
    use super::super::data_source::DataSourceInfo;

    #[derive(Clone, Debug, Eq)]
    pub(crate) struct ProductBundle {
        directory: Box<dyn api::Path>,
    }

    impl PartialEq for ProductBundle {
        fn eq(&self, other: &Self) -> bool {
            self.directory.eq(&other.directory)
        }
    }

    impl ProductBundle {
        pub fn new(directory: Box<dyn api::Path>) -> Self {
            Self { directory }
        }
    }

    impl DataSourceInfo for ProductBundle {
        fn kind(&self) -> api::DataSourceKind {
            api::DataSourceKind::ProductBundle
        }

        fn path(&self) -> Option<Box<dyn api::Path>> {
            Some(self.directory.clone())
        }

        fn version(&self) -> api::DataSourceVersion {
            // TODO: Add support for exposing the product bundle version.
            api::DataSourceVersion::Unknown
        }
    }

    #[derive(Clone, Debug, Eq)]
    pub(crate) struct TufRepository {
        blobs_directory: Box<dyn api::Path>,
    }

    impl PartialEq for TufRepository {
        fn eq(&self, other: &Self) -> bool {
            self.blobs_directory.eq(&other.blobs_directory)
        }
    }

    impl TufRepository {
        pub fn new(blobs_directory: Box<dyn api::Path>) -> Self {
            Self { blobs_directory }
        }
    }

    impl DataSourceInfo for TufRepository {
        fn kind(&self) -> api::DataSourceKind {
            api::DataSourceKind::TufRepository
        }

        fn path(&self) -> Option<Box<dyn api::Path>> {
            Some(self.blobs_directory.clone())
        }

        fn version(&self) -> api::DataSourceVersion {
            // TODO: Add support for exposing the TUF repository version.
            api::DataSourceVersion::Unknown
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProductBundle(Rc<ProductBundleData>);

impl ProductBundle {
    pub fn new(directory: Box<dyn api::Path>) -> Result<Self, ProductBundleError> {
        let mut product_bundle_data_source = ds::DataSource::new(
            vec![],
            Box::new(data_source::ProductBundle::new(directory.clone())),
        );
        let utf8_directory = Utf8PathBuf::from_path_buf(directory.as_ref().as_ref().to_path_buf())
            .map_err(|directory| ProductBundleError::InvalidDirectory { directory })?;
        let product_bundle = sdk::ProductBundle::try_load_from(&utf8_directory)
            .map_err(|error| ProductBundleError::DeserializationFailure { error })?;
        let product_bundle = match product_bundle {
            sdk::ProductBundle::V1(_) => {
                return Err(ProductBundleError::InvalidVerison { version: "V1".to_string() });
            }
            sdk::ProductBundle::V2(product_bundle) => product_bundle,
        };
        let update_package_hash: Box<dyn api::Hash> = Box::new(Hash::from(
            product_bundle
                .update_package_hash
                .ok_or_else(|| ProductBundleError::MissingUpdatePackage)?,
        ));
        let repositories = product_bundle
            .repositories
            .iter()
            .map(|repository| {
                let name = repository.name.clone();
                let blobs_directory: Box<dyn api::Path> = Box::new(
                    directory.as_ref().as_ref().to_path_buf().join(&repository.blobs_path),
                );
                let repository_data_source = ds::DataSource::new(
                    vec![],
                    Box::new(data_source::TufRepository::new(blobs_directory.clone())),
                );
                product_bundle_data_source.add_child(repository_data_source.clone());
                Repository::new(name, blobs_directory, repository_data_source)
            })
            .collect::<Vec<_>>();
        Ok(Self(Rc::new(ProductBundleData {
            directory,
            data_source: Box::new(product_bundle_data_source),
            update_package_hash,
            repositories: repositories.clone(),
            blobs: OnceCell::new(),
        })))
    }

    pub fn directory(&self) -> &Box<dyn api::Path> {
        &self.0.directory
    }

    pub fn update_package_hash(&self) -> &Box<dyn api::Hash> {
        &self.0.update_package_hash
    }

    pub fn repositories(&self) -> &Vec<Repository> {
        &self.0.repositories
    }

    pub fn blob_set(&self) -> Result<Rc<dyn BlobSet>, BlobDirectoryError> {
        self.0.blobs.get_or_try_init(|| self.init_blobs()).map(Rc::clone)
    }

    fn init_blobs(&self) -> Result<Rc<dyn BlobSet>, BlobDirectoryError> {
        self.0
            .repositories
            .clone()
            .into_iter()
            .map(|repository| repository.blobs())
            .collect::<Result<Vec<_>, _>>()
            .map(|repositories| {
                let blobs: Rc<dyn BlobSet> =
                    Rc::new(CompositeBlobSet::new(repositories.into_iter()));
                blobs
            })
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
struct ProductBundleData {
    directory: Box<dyn api::Path>,
    update_package_hash: Box<dyn api::Hash>,
    data_source: Box<dyn api::DataSource>,
    repositories: Vec<Repository>,
    #[derivative(Debug = "ignore")]
    blobs: OnceCell<Rc<dyn BlobSet>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Repository(Rc<RepositoryData>);

impl Repository {
    pub fn name(&self) -> &String {
        &self.0.name
    }

    pub fn blobs_directory(&self) -> &Box<dyn api::Path> {
        &self.0.blobs_directory
    }

    pub fn blobs(&self) -> Result<Rc<dyn BlobSet>, BlobDirectoryError> {
        self.0.blobs.get_or_try_init(|| self.init_blobs()).map(Rc::clone)
    }

    fn init_blobs(&self) -> Result<Rc<dyn BlobSet>, BlobDirectoryError> {
        BlobDirectory::new(Some(self.0.data_source.clone()), self.0.blobs_directory.clone()).map(
            |blobs| {
                let blobs: Rc<dyn BlobSet> = Rc::new(blobs);
                blobs
            },
        )
    }

    fn new(name: String, blobs_directory: Box<dyn api::Path>, data_source: ds::DataSource) -> Self {
        Self(Rc::new(RepositoryData { name, blobs_directory, data_source, blobs: OnceCell::new() }))
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
struct RepositoryData {
    name: String,
    blobs_directory: Box<dyn api::Path>,
    data_source: ds::DataSource,
    #[derivative(Debug = "ignore")]
    blobs: OnceCell<Rc<dyn BlobSet>>,
}

#[cfg(test)]
pub mod test {
    use assembly_partitions_config::PartitionsConfig;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use sdk_metadata::ProductBundle;
    use sdk_metadata::ProductBundleV2;
    use sdk_metadata::Repository;
    use std::path::Path;

    pub(crate) fn utf8_path_buf<P: AsRef<Path>>(path: P) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(path.as_ref().to_path_buf()).unwrap()
    }

    pub(crate) fn utf8_path(path: &Path) -> &Utf8Path {
        Utf8Path::from_path(path).unwrap()
    }

    pub(crate) const V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME: &str = "test.fuchsia.com";
    pub(crate) const V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH: &str = "test_metadata";
    pub(crate) const V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH: &str = "test_blobs";
    pub(crate) const V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_NAME: &str = "b.test.fuchsia.com";
    pub(crate) const V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH: &str = "b_test_metadata";
    pub(crate) const V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH: &str = "b_test_blobs";
    pub(crate) const V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_NAME: &str = "recovery.test.fuchsia.com";
    pub(crate) const V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH: &str =
        "recovery_test_metadata";
    pub(crate) const V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH: &str = "recovery_test_blobs";

    pub(crate) fn v2_sdk_a_product_bundle<P: AsRef<Path>>(
        product_bundle_path: P,
        update_package_hash: Option<fuchsia_hash::Hash>,
    ) -> ProductBundle {
        let metadata_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH),
        );
        let blobs_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH),
        );
        ProductBundle::V2(ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig::default(),
            sdk_version: String::default(),
            system_a: Some(vec![]),
            system_b: None,
            system_r: None,
            repositories: vec![Repository {
                name: V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
                metadata_path,
                blobs_path,
                delivery_blob_type: None,
                root_private_key_path: None,
                targets_private_key_path: None,
                snapshot_private_key_path: None,
                timestamp_private_key_path: None,
            }],
            update_package_hash,
            virtual_devices_path: None,
        })
    }

    pub(crate) fn v2_sdk_abr_product_bundle<P: AsRef<Path>>(
        product_bundle_path: P,
        update_package_hash: Option<fuchsia_hash::Hash>,
    ) -> ProductBundle {
        let a_metadata_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH),
        );
        let a_blobs_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH),
        );
        let b_metadata_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH),
        );
        let b_blobs_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH),
        );
        let r_metadata_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH),
        );
        let r_blobs_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH),
        );
        ProductBundle::V2(ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig::default(),
            sdk_version: String::default(),
            system_a: Some(vec![]),
            system_b: Some(vec![]),
            system_r: Some(vec![]),
            repositories: vec![
                Repository {
                    name: V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
                    metadata_path: a_metadata_path,
                    blobs_path: a_blobs_path,
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
                Repository {
                    name: V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
                    metadata_path: b_metadata_path,
                    blobs_path: b_blobs_path,
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
                Repository {
                    name: V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
                    metadata_path: r_metadata_path,
                    blobs_path: r_blobs_path,
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
            ],
            update_package_hash,
            virtual_devices_path: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::test;
    use super::ProductBundle;
    use super::ProductBundleError;
    use super::Repository;
    // use camino::Utf8PathBuf;
    use super::super::hash::Hash;
    use dyn_clone::DynClone;
    use sdk_metadata::ProductBundle as SdkProductBundle;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn v2_sdk_a_product_bundle<P: AsRef<Path>>(product_bundle_path: P) -> SdkProductBundle {
        test::v2_sdk_a_product_bundle(
            product_bundle_path,
            Some(fuchsia_hash::Hash::from([0; fuchsia_hash::HASH_SIZE])), // update_package_hash
        )
    }

    fn v2_sdk_abr_product_bundle<P: AsRef<Path>>(product_bundle_path: P) -> SdkProductBundle {
        test::v2_sdk_abr_product_bundle(
            product_bundle_path,
            Some(fuchsia_hash::Hash::from([0; fuchsia_hash::HASH_SIZE])), // update_package_hash
        )
    }

    fn v2_sdk_update_package_hash() -> Box<dyn api::Hash> {
        let hash: Hash = fuchsia_hash::Hash::from([0; fuchsia_hash::HASH_SIZE]).into();
        Box::new(hash)
    }

    fn path<P: AsRef<Path> + DynClone + 'static>(p: P) -> Box<dyn api::Path> {
        Box::new(p)
    }

    #[fuchsia::test]
    fn test_builder_simple_failures() {
        match ProductBundle::new(path("/definitely/does/not/exist"))
            .expect_err("product bundle from bad path")
        {
            ProductBundleError::DeserializationFailure { .. } => {}
            _ => {
                panic!("expected product bundle error when specifying path that does not exist");
            }
        }
    }

    #[fuchsia::test]
    fn test_missing_json_file() {
        let temp_dir = TempDir::new().expect("create temporary directory");
        let temp_dir_path = path(temp_dir.path().to_path_buf());
        match ProductBundle::new(temp_dir_path).expect_err("product bundle with no manifest") {
            ProductBundleError::DeserializationFailure { .. } => {}
            _ => {
                panic!("expected product bundle error when failing to load JSON");
            }
        }
    }

    #[fuchsia::test]
    fn test_v1_json_file() {
        let temp_dir = TempDir::new().expect("create temporary directory");
        let temp_dir_path = path(temp_dir.path().to_path_buf());
        let product_bundle_file = fs::File::create(temp_dir.path().join("product_bundle.json"))
            .expect("create product bundle manifest");
        serde_json::to_writer(
            &product_bundle_file,
            // Copied from sdk_metadata::product_bundle::tests::test_parse_v1.
            &serde_json::json!({
                "schema_id": "http://fuchsia.com/schemas/sdk/product_bundle-6320eef1.json",
                "data": {
                    "name": "generic-x64",
                    "type": "product_bundle",
                    "device_refs": ["generic-x64"],
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
        ).expect("write product bundle manifest");
        match ProductBundle::new(temp_dir_path).expect_err("product bundle with invalid version") {
            ProductBundleError::InvalidVerison { .. } => {}
            _ => {
                panic!("expected invalid version error when failing to generate JSON");
            }
        }
    }

    #[fuchsia::test]
    fn test_single_repository() {
        // Create directory for product bundle, complete with repository blob directory.
        let temp_dir = TempDir::new().expect("create temporary directory");
        let temp_dir_path = path(temp_dir.path().to_path_buf());
        let blobs_path_buf =
            temp_dir.path().join(test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        fs::create_dir_all(&blobs_path_buf).expect("create blobs directory");

        // Write product bundle manifest.
        v2_sdk_a_product_bundle(temp_dir.path())
            .write(test::utf8_path(temp_dir.path()))
            .expect("write product bundle manifest");

        // Instantiate product bundle under test.
        let product_bundle =
            ProductBundle::new(temp_dir_path.clone()).expect("instantiate product bundle");

        assert_eq!(product_bundle.directory(), &temp_dir_path);
        assert_eq!(product_bundle.update_package_hash(), &v2_sdk_update_package_hash());

        let repositories = product_bundle.repositories();
        assert_eq!(repositories.len(), 1);
        let repository = &repositories[0];
        assert_eq!(repository.name(), test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_NAME);
        assert_eq!(
            repository.blobs_directory(),
            &path(temp_dir.path().join(test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH))
        );
        assert!(product_bundle.blob_set().is_ok());
    }

    fn blob_path(blob: &[u8]) -> String {
        format!(
            "{}",
            fuchsia_merkle::MerkleTree::from_reader(blob)
                .expect("merkle tree from string bytes")
                .root()
        )
    }

    fn blob_hash(blob: &[u8]) -> Box<dyn api::Hash> {
        Box::new(Hash::from_contents(blob))
    }

    #[fuchsia::test]
    fn test_multiple_repositories() {
        // Create directory for product bundle, complete with repository blob directories.
        let temp_dir = TempDir::new().expect("create temporary directory");
        let temp_dir_path = path(temp_dir.path().to_path_buf());
        let blob_directories = [
            test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH,
            test::V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH,
            test::V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH,
        ]
        .into_iter()
        .map(|blob_directory| {
            let directory = temp_dir.path().join(blob_directory);
            fs::create_dir_all(&directory).expect("create blobs directory");
            directory
        })
        .collect::<Vec<_>>();

        // Add overlapping blobs.
        let a_blob = "a\n".as_bytes();
        let ab_blob = "ab\n".as_bytes();
        let abr_blob = "abr\n".as_bytes();
        let ar_blob = "ar\n".as_bytes();
        let b_blob = "b\n".as_bytes();
        let br_blob = "br\n".as_bytes();
        let r_blob = "r\n".as_bytes();
        fs::write(blob_directories[0].join(blob_path(a_blob)), a_blob).expect("write blob");
        fs::write(blob_directories[0].join(blob_path(ab_blob)), ab_blob).expect("write blob");
        fs::write(blob_directories[1].join(blob_path(ab_blob)), ab_blob).expect("write blob");
        fs::write(blob_directories[0].join(blob_path(abr_blob)), abr_blob).expect("write blob");
        fs::write(blob_directories[1].join(blob_path(abr_blob)), abr_blob).expect("write blob");
        fs::write(blob_directories[2].join(blob_path(abr_blob)), abr_blob).expect("write blob");
        fs::write(blob_directories[0].join(blob_path(ar_blob)), ar_blob).expect("write blob");
        fs::write(blob_directories[2].join(blob_path(ar_blob)), ar_blob).expect("write blob");
        fs::write(blob_directories[1].join(blob_path(b_blob)), b_blob).expect("write blob");
        fs::write(blob_directories[1].join(blob_path(br_blob)), br_blob).expect("write blob");
        fs::write(blob_directories[2].join(blob_path(br_blob)), br_blob).expect("write blob");
        fs::write(blob_directories[2].join(blob_path(r_blob)), r_blob).expect("write blob");

        // Write product bundle manifest.
        v2_sdk_abr_product_bundle(temp_dir.path())
            .write(test::utf8_path(temp_dir.path()))
            .expect("write product bundle manifest");

        // Instantiate product bundle under test.
        let product_bundle =
            ProductBundle::new(temp_dir_path.clone()).expect("instantiate product bundle");

        assert_eq!(product_bundle.directory(), &temp_dir_path);
        assert_eq!(product_bundle.update_package_hash(), &v2_sdk_update_package_hash());

        let repositories = product_bundle.repositories();
        assert_eq!(repositories.len(), 3);
        fn repository_blobs(repository: &Repository) -> Vec<Box<dyn api::Blob>> {
            repository.blobs().expect("repository blobs").iter().collect()
        }
        assert_eq!(repository_blobs(&repositories[0]).len(), 4);
        assert_eq!(repository_blobs(&repositories[1]).len(), 4);
        assert_eq!(repository_blobs(&repositories[2]).len(), 4);

        let blobs = product_bundle.blob_set().expect("product bundle blobs");

        let expected_a_blobs_path: Box<dyn api::Path> = Box::new(blob_directories[0].clone());
        let expected_b_blobs_path: Box<dyn api::Path> = Box::new(blob_directories[1].clone());
        let expected_r_blobs_path: Box<dyn api::Path> = Box::new(blob_directories[2].clone());

        // Check `a_blob` data sources.
        let actual_a_blob = blobs.blob(blob_hash(a_blob)).expect("get a blob");
        let a_blob_data_sources = actual_a_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(a_blob_data_sources.len(), 1);
        let actual_a_blobs_path = a_blob_data_sources[0].path().expect("a blob directory path");
        assert_eq!(actual_a_blobs_path.as_ref(), expected_a_blobs_path.as_ref());

        // Check `b_blob` data sources.
        let actual_b_blob = blobs.blob(blob_hash(b_blob)).expect("get b blob");
        let b_blob_data_sources = actual_b_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(b_blob_data_sources.len(), 1);
        let actual_b_blobs_path = b_blob_data_sources[0].path().expect("b blob directory path");
        assert_eq!(actual_b_blobs_path.as_ref(), expected_b_blobs_path.as_ref());

        // Check `r_blob` data sources.
        let actual_r_blob = blobs.blob(blob_hash(r_blob)).expect("get r blob");
        let r_blob_data_sources = actual_r_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(r_blob_data_sources.len(), 1);
        let actual_r_blobs_path = r_blob_data_sources[0].path().expect("r blob directory path");
        assert_eq!(actual_r_blobs_path.as_ref(), expected_r_blobs_path.as_ref());

        // Check `ab_blob` data sources.
        let actual_ab_blob = blobs.blob(blob_hash(ab_blob)).expect("get ab blob");
        let ab_blob_data_sources = actual_ab_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(ab_blob_data_sources.len(), 2);
        let actual_ab_blobs_paths = [
            ab_blob_data_sources[0].path().expect("ab blob directory path"),
            ab_blob_data_sources[1].path().expect("ab blob directory path"),
        ];
        assert_eq!(actual_ab_blobs_paths[0].as_ref(), expected_a_blobs_path.as_ref());
        assert_eq!(actual_ab_blobs_paths[1].as_ref(), expected_b_blobs_path.as_ref());

        // Check `ar_blob` data sources.
        let actual_ar_blob = blobs.blob(blob_hash(ar_blob)).expect("get ar blob");
        let ar_blob_data_sources = actual_ar_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(ar_blob_data_sources.len(), 2);
        let actual_ar_blobs_paths = [
            ar_blob_data_sources[0].path().expect("ar blob directory path"),
            ar_blob_data_sources[1].path().expect("ar blob directory path"),
        ];
        assert_eq!(actual_ar_blobs_paths[0].as_ref(), expected_a_blobs_path.as_ref());
        assert_eq!(actual_ar_blobs_paths[1].as_ref(), expected_r_blobs_path.as_ref());

        // Check `br_blob` data sources.
        let actual_br_blob = blobs.blob(blob_hash(br_blob)).expect("get br blob");
        let br_blob_data_sources = actual_br_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(br_blob_data_sources.len(), 2);
        let actual_br_blobs_paths = [
            br_blob_data_sources[0].path().expect("br blob directory path"),
            br_blob_data_sources[1].path().expect("br blob directory path"),
        ];
        assert_eq!(actual_br_blobs_paths[0].as_ref(), expected_b_blobs_path.as_ref());
        assert_eq!(actual_br_blobs_paths[1].as_ref(), expected_r_blobs_path.as_ref());

        // Check `abr_blob` data sources.
        let actual_abr_blob = blobs.blob(blob_hash(abr_blob)).expect("get abr blob");
        let abr_blob_data_sources = actual_abr_blob.data_sources().collect::<Vec<_>>();
        assert_eq!(abr_blob_data_sources.len(), 3);
        let actual_abr_blobs_paths = [
            abr_blob_data_sources[0].path().expect("abr blob directory path"),
            abr_blob_data_sources[1].path().expect("abr blob directory path"),
            abr_blob_data_sources[2].path().expect("abr blob directory path"),
        ];
        assert_eq!(actual_abr_blobs_paths[0].as_ref(), expected_a_blobs_path.as_ref());
        assert_eq!(actual_abr_blobs_paths[1].as_ref(), expected_b_blobs_path.as_ref());
        assert_eq!(actual_abr_blobs_paths[2].as_ref(), expected_r_blobs_path.as_ref());
    }
}
