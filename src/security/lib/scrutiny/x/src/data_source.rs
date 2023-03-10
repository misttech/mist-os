// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::product_bundle::DataSource as ProductBundleSource;
use crate::product_bundle::ProductBundleRepositoryBlobs;
use std::fmt::Debug;
use std::iter;
use std::path::PathBuf;

/// Unified `crate::api::DataSource` implementation over production types.
#[derive(Debug, Eq, PartialEq)]
pub enum DataSource {
    BlobSource(BlobSource),
    ProductBundleSource(ProductBundleSource),
}

impl From<BlobSource> for DataSource {
    fn from(blob_source: BlobSource) -> Self {
        Self::BlobSource(blob_source)
    }
}

impl From<ProductBundleSource> for DataSource {
    fn from(product_bundle_source: ProductBundleSource) -> Self {
        Self::ProductBundleSource(product_bundle_source)
    }
}

impl api::DataSource for DataSource {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        match self {
            Self::BlobSource(blob_source) => blob_source.kind(),
            Self::ProductBundleSource(product_bundle_source) => product_bundle_source.kind(),
        }
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        match self {
            Self::BlobSource(blob_source) => blob_source.parent(),
            Self::ProductBundleSource(product_bundle_source) => product_bundle_source.parent(),
        }
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        match self {
            Self::BlobSource(blob_source) => blob_source.children(),
            Self::ProductBundleSource(product_bundle_source) => product_bundle_source.children(),
        }
    }

    fn path(&self) -> Option<Self::SourcePath> {
        match self {
            Self::BlobSource(blob_source) => blob_source.path(),
            Self::ProductBundleSource(product_bundle_source) => product_bundle_source.path(),
        }
    }

    fn version(&self) -> api::DataSourceVersion {
        match self {
            Self::BlobSource(blob_source) => blob_source.version(),
            Self::ProductBundleSource(product_bundle_source) => product_bundle_source.version(),
        }
    }
}

/// Unified `crate::api::DataSource` implementation over production blob types.
#[derive(Debug, Eq, Hash, PartialEq)]
pub enum BlobSource {
    #[cfg(test)]
    BlobFsArchive(blobfs::BlobFsArchive),

    BlobDirectory(BlobDirectory),
    ProductBundleRepositoryBlobs(ProductBundleRepositoryBlobs),
}

#[cfg(test)]
impl From<blobfs::BlobFsArchive> for BlobSource {
    fn from(blob_fs_archive: blobfs::BlobFsArchive) -> Self {
        Self::BlobFsArchive(blob_fs_archive)
    }
}

impl From<BlobDirectory> for BlobSource {
    fn from(blob_directory: BlobDirectory) -> Self {
        Self::BlobDirectory(blob_directory)
    }
}

impl From<ProductBundleRepositoryBlobs> for BlobSource {
    fn from(product_bundle_repository_blobs: ProductBundleRepositoryBlobs) -> Self {
        Self::ProductBundleRepositoryBlobs(product_bundle_repository_blobs)
    }
}

impl api::DataSource for BlobSource {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        match self {
            #[cfg(test)]
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.kind(),

            Self::BlobDirectory(blob_directory) => blob_directory.kind(),
            Self::ProductBundleRepositoryBlobs(product_bundle_repository_blobs) => {
                product_bundle_repository_blobs.kind()
            }
        }
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        match self {
            #[cfg(test)]
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.parent(),

            Self::BlobDirectory(blob_directory) => blob_directory.parent(),
            Self::ProductBundleRepositoryBlobs(product_bundle_repository_blobs) => {
                product_bundle_repository_blobs.parent()
            }
        }
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        match self {
            #[cfg(test)]
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.children(),

            Self::BlobDirectory(blob_directory) => blob_directory.children(),
            Self::ProductBundleRepositoryBlobs(product_bundle_repository_blobs) => {
                product_bundle_repository_blobs.children()
            }
        }
    }

    fn path(&self) -> Option<Self::SourcePath> {
        match self {
            #[cfg(test)]
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.path(),

            Self::BlobDirectory(blob_directory) => blob_directory.path(),
            Self::ProductBundleRepositoryBlobs(product_bundle_repository_blobs) => {
                product_bundle_repository_blobs.path()
            }
        }
    }

    fn version(&self) -> api::DataSourceVersion {
        match self {
            #[cfg(test)]
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.version(),

            Self::BlobDirectory(blob_directory) => blob_directory.version(),
            Self::ProductBundleRepositoryBlobs(product_bundle_repository_blobs) => {
                product_bundle_repository_blobs.version()
            }
        }
    }
}

#[cfg(test)]
pub mod blobfs {
    use crate::api;
    use std::iter;
    use std::path::PathBuf;

    #[derive(Debug, Eq, Hash, PartialEq)]
    pub struct BlobFsArchive {
        path: PathBuf,
    }

    impl BlobFsArchive {
        /// Constructs a [`BlobFsArchive`] that is backed by the file located at `path`.
        pub fn new(path: PathBuf) -> Self {
            Self { path }
        }
    }

    impl api::DataSource for BlobFsArchive {
        type SourcePath = PathBuf;

        fn kind(&self) -> api::DataSourceKind {
            api::DataSourceKind::BlobfsArchive
        }

        fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
            None
        }

        fn children(
            &self,
        ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>>
        {
            Box::new(iter::empty())
        }

        fn path(&self) -> Option<Self::SourcePath> {
            Some(self.path.clone())
        }

        fn version(&self) -> api::DataSourceVersion {
            // TODO: Add support for exposing the blobfs format version.
            api::DataSourceVersion::Unknown
        }
    }
}

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct BlobDirectory {
    directory: PathBuf,
}

impl BlobDirectory {
    /// Construct a [`BlobFsArchive`] that is backed by the file located at `path`.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
}

impl api::DataSource for BlobDirectory {
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        api::DataSourceKind::BlobDirectory
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        None
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        Box::new(iter::empty())
    }

    fn path(&self) -> Option<Self::SourcePath> {
        Some(self.directory.clone())
    }

    fn version(&self) -> api::DataSourceVersion {
        // TODO: Add support for directory-as-blob-archive versioning.
        api::DataSourceVersion::Unknown
    }
}

// TODO(fxbug.dev/111251): Add additional data source types for production System API.

#[cfg(test)]
pub(crate) mod fake {
    use crate::api;
    use std::iter;

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct DataSource;

    impl api::DataSource for DataSource {
        type SourcePath = &'static str;

        fn kind(&self) -> api::DataSourceKind {
            api::DataSourceKind::Unknown
        }

        fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
            None
        }

        fn children(
            &self,
        ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>>
        {
            Box::new(iter::empty())
        }

        fn path(&self) -> Option<Self::SourcePath> {
            None
        }

        fn version(&self) -> api::DataSourceVersion {
            api::DataSourceVersion::Unknown
        }
    }
}
