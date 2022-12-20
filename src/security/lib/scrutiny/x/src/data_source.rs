// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::DataSource as DataSourceApi;
use crate::api::DataSourceKind;
use crate::api::DataSourceVersion;
use std::iter;
use std::path::PathBuf;

#[derive(Debug, Eq, PartialEq)]
pub struct BlobFsArchive {
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
pub struct BlobDirectory {
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

// TODO(fxbug.dev/111251): Add additional data source types for production System API.

#[cfg(test)]
pub mod fake {
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
