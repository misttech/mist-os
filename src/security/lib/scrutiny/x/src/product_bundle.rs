// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::blob;
use crate::blob::BlobDirectoryBlobSet;
use crate::blob::BlobDirectoryBlobSetBuilderError;
use crate::blob::BlobDirectoryError;
use crate::blob::CompositeBlobSet;
use crate::blob::FileBlob;
use crate::hash::Hash;
use camino::Utf8PathBuf;
use scrutiny_utils::io::ReadSeek;
use sdk_metadata::ProductBundle as SdkProductBundle;
use sdk_metadata::ProductBundleV2 as SdkProductBundleV2;
use sdk_metadata::Repository;
use std::iter;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use thiserror::Error;

/// Unified `crate::api::DataSource` implementation over product bundle types.
#[derive(Debug, Eq, PartialEq)]
pub enum DataSource {
    ProductBundle(ProductBundle),
    ProductBundleRepository(ProductBundleRepository),
}

impl From<ProductBundle> for DataSource {
    fn from(product_bundle: ProductBundle) -> Self {
        Self::ProductBundle(product_bundle)
    }
}

impl From<ProductBundleRepository> for DataSource {
    fn from(product_bundle_repository: ProductBundleRepository) -> Self {
        Self::ProductBundleRepository(product_bundle_repository)
    }
}

impl api::DataSource for DataSource {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        match self {
            Self::ProductBundle(product_bundle) => product_bundle.kind(),
            Self::ProductBundleRepository(product_bundle_repository) => {
                product_bundle_repository.kind()
            }
        }
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        match self {
            Self::ProductBundle(product_bundle) => product_bundle.parent(),
            Self::ProductBundleRepository(product_bundle_repository) => {
                product_bundle_repository.parent()
            }
        }
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        match self {
            Self::ProductBundle(product_bundle) => product_bundle.children(),
            Self::ProductBundleRepository(product_bundle_repository) => {
                product_bundle_repository.children()
            }
        }
    }

    fn path(&self) -> Option<Self::SourcePath> {
        match self {
            Self::ProductBundle(product_bundle) => product_bundle.path(),
            Self::ProductBundleRepository(product_bundle_repository) => {
                product_bundle_repository.path()
            }
        }
    }

    fn version(&self) -> api::DataSourceVersion {
        match self {
            Self::ProductBundle(product_bundle) => product_bundle.version(),
            Self::ProductBundleRepository(product_bundle_repository) => {
                product_bundle_repository.version()
            }
        }
    }
}

/// Errors that may be encountered in `ProductBundleBuilder::build`.
#[derive(Debug, Error)]
pub enum ProductBundleBuilderError {
    #[error("product bundle directory path is not a valid UTF8 string: {directory:?}")]
    InvalidDirectory { directory: PathBuf },
    #[error("failed to deserialize product bundle: {error}")]
    DeserializationFailure { error: anyhow::Error },
    #[error(
        "attempted to build product bundle from unsupported product bundle format version: {version}"
    )]
    InvalidVerison { version: String },
}

/// Builder pattern for constructing instances of [`ProductBundle`].
pub struct ProductBundleBuilder {
    directory: PathBuf,
    system_slot: SystemSlot,
}

impl ProductBundleBuilder {
    /// Constructs a new [`ProductBundleBuilder`] for building a [`ProductBundle`].
    pub fn new<P: AsRef<Path>>(directory: P, system_slot: SystemSlot) -> Self {
        Self { directory: directory.as_ref().to_path_buf(), system_slot }
    }

    /// Builds a [`ProductBundle`] based on data encoded in this builder.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn build(self) -> Result<ProductBundle, ProductBundleBuilderError> {
        let utf8_directory = Utf8PathBuf::from_path_buf(self.directory)
            .map_err(|directory| ProductBundleBuilderError::InvalidDirectory { directory })?;
        let product_bundle = SdkProductBundle::try_load_from(&utf8_directory)
            .map_err(|error| ProductBundleBuilderError::DeserializationFailure { error })?;
        let product_bundle = match product_bundle {
            SdkProductBundle::V1(_) => {
                return Err(ProductBundleBuilderError::InvalidVerison {
                    version: "V1".to_string(),
                });
            }
            SdkProductBundle::V2(product_bundle) => product_bundle,
        };
        Ok(ProductBundle::new(ProductBundleData {
            directory: utf8_directory.into(),
            product_bundle,
            system_slot: self.system_slot,
        }))
    }
}

/// A system slot under which images may be grouped in a product bundle. See
/// https://fuchsia.dev/fuchsia-src/glossary?hl=en#abr for details.
#[derive(Debug, Eq, Hash, PartialEq)]
pub enum SystemSlot {
    A,
    B,
    R,
}

/// A model of a particular system described by a product bundle.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProductBundle(Rc<ProductBundleData>);

impl ProductBundle {
    /// Constructs a builder for a new [`ProductBundle`].
    pub fn builder<P: AsRef<Path>>(directory: P, system_slot: SystemSlot) -> ProductBundleBuilder {
        ProductBundleBuilder::new(directory, system_slot)
    }

    /// Constructs a data source that refers to this product bundle's repository.
    pub fn repositories(&self) -> impl Iterator<Item = ProductBundleRepository> {
        let product_bundle = self.clone();
        product_bundle.0.product_bundle.repositories.clone().into_iter().map(move |repository| {
            ProductBundleRepository::new(product_bundle.clone(), repository.clone())
        })
    }

    pub fn blob_set(
        &self,
    ) -> Result<
        CompositeBlobSet<ProductBundleRepositoryBlob, BlobDirectoryError>,
        BlobDirectoryBlobSetBuilderError,
    > {
        let delegates = self
            .repositories()
            .map(|repository| repository.blobs().blob_set())
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|blob_set| {
                let blob_set: Rc<AbstractProductBundleRepositoryBlobSet> = Rc::new(blob_set);
                blob_set
            });
        Ok(CompositeBlobSet::new(delegates))
    }

    /// Constructs a path to this product bundle's repository's blobs directory. This path includes
    /// the path to the product bundle itself.
    pub fn repository_blobs_directories(&self) -> impl Iterator<Item = &Path> {
        self.0
            .product_bundle
            .repositories
            .iter()
            .map(|repository| repository.blobs_path.as_std_path())
    }

    /// Constructs a new product bundle from backing data.
    fn new(product_bundle_data: ProductBundleData) -> Self {
        Self(Rc::new(product_bundle_data))
    }
}

impl ProductBundle {
    /// Returns a reference to the directory that backs this product bundle.
    pub fn directory(&self) -> &PathBuf {
        &self.0.directory
    }
}

/// Data underlying a system in a product bundle.
#[derive(Debug, Eq, Hash, PartialEq)]
struct ProductBundleData {
    /// The path to the product bundle directory.
    directory: PathBuf,
    /// The structured data from this product bundle's JSON.
    product_bundle: SdkProductBundleV2,
    /// The system slot that this instance refers to.
    system_slot: SystemSlot,
}

impl api::DataSource for ProductBundle {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        api::DataSourceKind::ProductBundle
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        None
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        Box::new(self.repositories().map(|repository| {
            let repository: Box<dyn api::DataSource<SourcePath = Self::SourcePath>> =
                Box::new(repository);
            repository
        }))
    }

    fn path(&self) -> Option<Self::SourcePath> {
        Some(self.0.directory.clone())
    }

    fn version(&self) -> api::DataSourceVersion {
        // TODO: Add support for exposing the product bundle version.
        api::DataSourceVersion::Unknown
    }
}

/// A data source for a product bundle's TUF repository.
#[derive(Debug, Eq, Hash, PartialEq)]
pub struct ProductBundleRepository {
    product_bundle: ProductBundle,
    repository: Repository,
}

impl ProductBundleRepository {
    /// Constructs a data source that describes the repository in a product bundle.
    fn new(product_bundle: ProductBundle, repository: Repository) -> Self {
        Self { product_bundle, repository }
    }

    /// Constructs a data source that describes the blobs directory of this repository.
    pub fn blobs(&self) -> ProductBundleRepositoryBlobs {
        ProductBundleRepositoryBlobs::new(self.product_bundle.clone(), self.repository.clone())
    }
}

impl api::DataSource for ProductBundleRepository {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        api::DataSourceKind::TUFRepository
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        Some(Box::new(self.product_bundle.clone()))
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        Box::new([self.blobs()].into_iter().map(|blobs| {
            let blobs: Box<dyn api::DataSource<SourcePath = Self::SourcePath>> = Box::new(blobs);
            blobs
        }))
    }

    fn path(&self) -> Option<Self::SourcePath> {
        None
    }

    fn version(&self) -> api::DataSourceVersion {
        // TODO: Add support for exposing the TUF version.
        api::DataSourceVersion::Unknown
    }
}

/// A data source for a product bundle's TUF repository's blobs.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProductBundleRepositoryBlobs {
    product_bundle: ProductBundle,
    repository: Repository,
    directory: PathBuf,
}

impl ProductBundleRepositoryBlobs {
    /// Constructs a data source that refers to a product bundle's repository's blobs directory.
    fn new(product_bundle: ProductBundle, repository: Repository) -> Self {
        let directory = product_bundle.directory().join(&repository.blobs_path);
        Self { product_bundle, repository, directory }
    }

    /// Constructs a blob set backed by this blobs directory.
    pub fn blob_set(
        &self,
    ) -> Result<ProductBundleRepositoryBlobSet, BlobDirectoryBlobSetBuilderError> {
        let blob_set = BlobDirectoryBlobSet::builder(self.directory()).build()?;
        Ok(ProductBundleRepositoryBlobSet::new(self.clone(), blob_set))
    }

    /// Constructs the absolute path to the blobs directory.
    fn directory(&self) -> &PathBuf {
        &self.directory
    }
}

impl api::DataSource for ProductBundleRepositoryBlobs {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        api::DataSourceKind::ProductBundle
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        let repository: Box<dyn api::DataSource<SourcePath = Self::SourcePath>> = Box::new(
            ProductBundleRepository::new(self.product_bundle.clone(), self.repository.clone()),
        );
        Some(repository)
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        Box::new(iter::empty())
    }

    fn path(&self) -> Option<Self::SourcePath> {
        Some(self.directory().to_path_buf())
    }

    fn version(&self) -> api::DataSourceVersion {
        // TODO: Add support for exposing the blob identity version.
        api::DataSourceVersion::Unknown
    }
}

#[derive(Clone)]
pub struct ProductBundleRepositoryBlobSet {
    data_source: ProductBundleRepositoryBlobs,
    blob_set: BlobDirectoryBlobSet,
}

impl ProductBundleRepositoryBlobSet {
    fn new(data_source: ProductBundleRepositoryBlobs, blob_set: BlobDirectoryBlobSet) -> Self {
        Self { data_source, blob_set }
    }
}

impl blob::BlobSet for ProductBundleRepositoryBlobSet {
    type Hash = Hash;
    type Blob = ProductBundleRepositoryBlob;
    type DataSource = ProductBundleRepositoryBlobs;
    type Error = BlobDirectoryError;

    fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
        let data_source = self.data_source.clone();
        Box::new(
            self.blob_set.iter().map(move |file_blob| {
                ProductBundleRepositoryBlob::new(data_source.clone(), file_blob)
            }),
        )
    }

    fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error> {
        let data_source = self.data_source.clone();
        self.blob_set
            .blob(hash)
            .map(|file_blob| ProductBundleRepositoryBlob::new(data_source.clone(), file_blob))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([self.data_source.clone()].into_iter())
    }
}

type AbstractProductBundleRepositoryBlobSet = dyn blob::BlobSet<
    Hash = Hash,
    Blob = ProductBundleRepositoryBlob,
    DataSource = ProductBundleRepositoryBlobs,
    Error = BlobDirectoryError,
>;

pub struct ProductBundleRepositoryBlob {
    data_source: ProductBundleRepositoryBlobs,
    blob: FileBlob,
}

impl ProductBundleRepositoryBlob {
    fn new(data_source: ProductBundleRepositoryBlobs, blob: FileBlob) -> Self {
        Self { data_source, blob }
    }
}

impl api::Blob for ProductBundleRepositoryBlob {
    type Hash = Hash;
    type ReaderSeeker = Box<dyn ReadSeek>;
    type DataSource = ProductBundleRepositoryBlobs;
    type Error = BlobDirectoryError;

    fn hash(&self) -> Self::Hash {
        self.blob.hash()
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        self.blob.reader_seeker()
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([self.data_source.clone()].into_iter())
    }
}

#[cfg(test)]
pub mod test {
    use super::ProductBundle;
    use super::ProductBundleRepositoryBlobs;
    use assembly_partitions_config::PartitionsConfig;
    use camino::Utf8Path;
    use camino::Utf8PathBuf;
    use sdk_metadata::ProductBundle as SdkProductBundle;
    use sdk_metadata::ProductBundleV2 as SdkProductBundleV2;
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
    ) -> SdkProductBundle {
        let metadata_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_METADATA_PATH),
        );
        let blobs_path = utf8_path_buf(
            product_bundle_path.as_ref().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH),
        );
        SdkProductBundle::V2(SdkProductBundleV2 {
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
            }],
            update_package_hash: None,
            virtual_devices_path: None,
        })
    }

    pub(crate) fn v2_sdk_abr_product_bundle<P: AsRef<Path>>(
        product_bundle_path: P,
    ) -> SdkProductBundle {
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
        SdkProductBundle::V2(SdkProductBundleV2 {
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
                },
                Repository {
                    name: V2_SDK_B_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
                    metadata_path: b_metadata_path,
                    blobs_path: b_blobs_path,
                },
                Repository {
                    name: V2_SDK_R_PRODUCT_BUNDLE_REPOSITORY_NAME.to_string(),
                    metadata_path: r_metadata_path,
                    blobs_path: r_blobs_path,
                },
            ],
            update_package_hash: None,
            virtual_devices_path: None,
        })
    }

    impl ProductBundleRepositoryBlobs {
        pub fn new_for_test(product_bundle: ProductBundle, repository: Repository) -> Self {
            let directory = product_bundle.directory().join(&repository.blobs_path);
            Self { product_bundle, repository, directory }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProductBundleBuilder;
    use super::ProductBundleBuilderError;
    use super::SystemSlot;
    use crate::api;
    use crate::api::DataSource as _;
    use crate::blob::BlobDirectoryBlobSetBuilderError;
    use crate::blob::BlobSet;
    use crate::product_bundle::test::utf8_path;
    use crate::product_bundle::test::v2_sdk_a_product_bundle;
    use crate::product_bundle::test::V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH;
    use std::fs;
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[fuchsia::test]
    fn test_builder_simple_failures() {
        match ProductBundleBuilder::new("/definitely/does/not/exist", SystemSlot::R)
            .build()
            .unwrap_err()
        {
            ProductBundleBuilderError::DeserializationFailure { .. } => {}
            _ => {
                panic!("expected product bundle builder error when specifying path that does not exist");
            }
        }
    }

    #[fuchsia::test]
    fn test_missing_json_file() {
        let temp_dir = TempDir::new().unwrap();
        match ProductBundleBuilder::new(temp_dir.path(), SystemSlot::A).build().unwrap_err() {
            ProductBundleBuilderError::DeserializationFailure { .. } => {}
            _ => {
                panic!("expected product bundle builder error when failing to generate JSON");
            }
        }
    }

    #[fuchsia::test]
    fn test_invalid_json_file() {
        let temp_dir = TempDir::new().unwrap();
        let mut product_bundle_file =
            File::create(temp_dir.path().join("product_bundle.json")).unwrap();
        write!(product_bundle_file, "}}{{").unwrap();
        match ProductBundleBuilder::new(temp_dir.path(), SystemSlot::A).build().unwrap_err() {
            ProductBundleBuilderError::DeserializationFailure { .. } => {}
            _ => {
                panic!("expected deserialization failure when failing to generate JSON");
            }
        }
    }

    #[fuchsia::test]
    fn test_v1_json_file() {
        let temp_dir = TempDir::new().unwrap();
        let product_bundle_file =
            File::create(temp_dir.path().join("product_bundle.json")).unwrap();
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
        ).unwrap();
        match ProductBundleBuilder::new(temp_dir.path(), SystemSlot::A).build().unwrap_err() {
            ProductBundleBuilderError::InvalidVerison { .. } => {}
            _ => {
                panic!("expected invalid version error when failing to generate JSON");
            }
        }
    }

    #[fuchsia::test]
    fn test_data_sources_and_blob_set() {
        // Create directory for product bundle, complete with repository blob directory.
        let temp_dir = TempDir::new().unwrap();
        let blobs_path_buf = temp_dir.path().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        fs::create_dir_all(&blobs_path_buf).unwrap();

        // Write product bundle manifest.
        v2_sdk_a_product_bundle(temp_dir.path()).write(utf8_path(temp_dir.path())).unwrap();

        // Instantiate product bundle under test.
        let product_bundle =
            ProductBundleBuilder::new(temp_dir.path(), SystemSlot::A).build().unwrap();

        //
        // Check product bundle.
        //

        assert_eq!(product_bundle.directory(), temp_dir.path());
        assert_eq!(
            product_bundle.repository_blobs_directories().collect::<Vec<_>>(),
            vec![blobs_path_buf]
        );
        assert_eq!(product_bundle.kind(), api::DataSourceKind::ProductBundle);
        assert_eq!(product_bundle.parent(), None);
        assert_eq!(product_bundle.path().unwrap(), temp_dir.path());

        //
        // Check repository of interest.
        //

        // Expect 1 product bundle child: the repository of interest.
        let product_bundle_children: Vec<_> = product_bundle.children().collect();
        assert_eq!(product_bundle_children.len(), 1);
        let repository_as_data_source = &product_bundle_children[0];

        // Expect `children()[0]` and `repositoryies()[0]` to be the same.
        let mut repositories = product_bundle.repositories().map(|repository| {
            let repository: Box<dyn api::DataSource<SourcePath = PathBuf>> = Box::new(repository);
            repository
        });
        let repository = repositories.next().unwrap();
        assert!(repositories.next().is_none());
        assert_eq!(&repository, repository_as_data_source);

        //
        // Check repository blobs.
        //

        // Expect 1 repository child: the blobs in the repository.
        let repository_children: Vec<_> = repository_as_data_source.children().collect();
        assert_eq!(1, repository_children.len());
        let blobs_as_data_source = &repository_children[0];
        let mut repositories = product_bundle.repositories();
        let repository = repositories.next().unwrap();
        assert!(repositories.next().is_none());

        // Expect `children[0]` and `blobs()` to be the same.
        let blobs: Box<dyn api::DataSource<SourcePath = PathBuf>> = Box::new(repository.blobs());
        assert_eq!(&blobs, blobs_as_data_source);

        // Expect blobs data source to refer to valid (empty) blobs directory that refers back
        // to the same data source.
        let blobs = repository.blobs();
        let blob_set = blobs.blob_set().unwrap();
        assert!(blob_set.iter().next().is_none());
        let blob_set_data_sources: Vec<_> = blob_set.data_sources().collect();
        assert_eq!(1, blob_set_data_sources.len());
        let data_source_from_blob_set = &blob_set_data_sources[0];
        assert_eq!(&blobs, data_source_from_blob_set);
    }

    #[fuchsia::test]
    fn test_data_sources_with_invalid_blobs_directory() {
        // Create malformed blobs dir: Contains a directory entry (forbidden) with a name that is
        // not a hash string (also forbidden).
        let temp_dir = TempDir::new().unwrap();
        let extra_dir_path_buf = temp_dir
            .path()
            .join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH)
            .join("neither_a_hash_no_a_file");
        fs::create_dir_all(&extra_dir_path_buf).unwrap();

        // Write product bundle manifest.
        v2_sdk_a_product_bundle(temp_dir.path()).write(utf8_path(temp_dir.path())).unwrap();

        // Instantiate product bundle under test.
        let product_bundle =
            ProductBundleBuilder::new(temp_dir.path(), SystemSlot::A).build().unwrap();

        let mut repositories = product_bundle.repositories();
        let repository = repositories.next().unwrap();
        assert!(repositories.next().is_none());

        // Attempt to construct blob set from repository of interest. This should fail because
        // the blobs directory contains malformed entries.
        match repository.blobs().blob_set() {
            Ok(_) => assert!(false, "Expected failure to construct blob set when bad entries exist in blob directory, but got blob set"),
            Err(BlobDirectoryBlobSetBuilderError::PathError(_)) => {},
            Err(err) => assert!(false, "Expected path error when bad entries exist in blob directory, but got {:?}", err),
        }
    }
}
