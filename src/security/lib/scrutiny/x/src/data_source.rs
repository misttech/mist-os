// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::DataSource as DataSourceApi;
use crate::api::DataSourceKind;
use crate::api::DataSourceVersion;
use std::fmt::Debug;
use std::iter;
use std::path::PathBuf;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BlobFsArchive {
    path: PathBuf,
}

impl BlobFsArchive {
    /// Constructs a [`BlobFsArchive`] that is backed by the file located at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl DataSourceApi for BlobFsArchive {
    type SourcePath = PathBuf;

    fn kind(&self) -> DataSourceKind {
        DataSourceKind::BlobfsArchive
    }

    fn parent(&self) -> Option<Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>> {
        None
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>>> {
        Box::new(iter::empty())
    }

    fn path(&self) -> Option<Self::SourcePath> {
        Some(self.path.clone())
    }

    fn version(&self) -> DataSourceVersion {
        // TODO: Add support for exposing the blobfs format version.
        DataSourceVersion::Unknown
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BlobDirectory {
    directory: PathBuf,
}

impl BlobDirectory {
    /// Construct a [`BlobFsArchive`] that is backed by the file located at `path`.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
}

impl DataSourceApi for BlobDirectory {
    type SourcePath = PathBuf;

    fn kind(&self) -> DataSourceKind {
        DataSourceKind::BlobDirectory
    }

    fn parent(&self) -> Option<Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>> {
        None
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>>> {
        Box::new(iter::empty())
    }

    fn path(&self) -> Option<Self::SourcePath> {
        Some(self.directory.clone())
    }

    fn version(&self) -> DataSourceVersion {
        // TODO: Add support for directory-as-blob-archive versioning.
        DataSourceVersion::Unknown
    }
}

/// Unified `crate::api::DataSource` implementation over production blob types.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum BlobSource {
    BlobFsArchive(BlobFsArchive),
    BlobDirectory(BlobDirectory),
}

impl From<BlobFsArchive> for BlobSource {
    fn from(blob_fs_archive: BlobFsArchive) -> Self {
        Self::BlobFsArchive(blob_fs_archive)
    }
}

impl From<BlobDirectory> for BlobSource {
    fn from(blob_directory: BlobDirectory) -> Self {
        Self::BlobDirectory(blob_directory)
    }
}

impl DataSourceApi for BlobSource {
    type SourcePath = PathBuf;

    fn kind(&self) -> DataSourceKind {
        match self {
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.kind(),
            Self::BlobDirectory(blob_directory) => blob_directory.kind(),
        }
    }

    fn parent(&self) -> Option<Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>> {
        match self {
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.parent(),
            Self::BlobDirectory(blob_directory) => blob_directory.parent(),
        }
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>>> {
        match self {
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.children(),
            Self::BlobDirectory(blob_directory) => blob_directory.children(),
        }
    }

    fn path(&self) -> Option<Self::SourcePath> {
        match self {
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.path(),
            Self::BlobDirectory(blob_directory) => blob_directory.path(),
        }
    }

    fn version(&self) -> DataSourceVersion {
        match self {
            Self::BlobFsArchive(blob_fs_archive) => blob_fs_archive.version(),
            Self::BlobDirectory(blob_directory) => blob_directory.version(),
        }
    }
}

// TODO(fxbug.dev/111251): Add additional data source types for production System API.

#[cfg(test)]
pub(crate) mod fake {
    use crate::api::DataSource as DataSourceApi;
    use crate::api::DataSourceKind;
    use crate::api::DataSourceVersion;
    use std::iter;

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct DataSource;

    impl DataSourceApi for DataSource {
        type SourcePath = &'static str;

        fn kind(&self) -> DataSourceKind {
            DataSourceKind::Unknown
        }

        fn parent(&self) -> Option<Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>> {
            None
        }

        fn children(
            &self,
        ) -> Box<dyn Iterator<Item = Box<dyn DataSourceApi<SourcePath = Self::SourcePath>>>>
        {
            Box::new(iter::empty())
        }

        fn path(&self) -> Option<Self::SourcePath> {
            None
        }

        fn version(&self) -> DataSourceVersion {
            DataSourceVersion::Unknown
        }
    }
}
