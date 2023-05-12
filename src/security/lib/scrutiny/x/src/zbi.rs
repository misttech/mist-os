// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::Cursor;
use std::path::PathBuf;

use crate::api;
use crate::hash::Hash;
use fuchsia_merkle::MerkleTree;
use scrutiny_utils::bootfs::BootfsReader;
use scrutiny_utils::zbi::ZbiReader;
use scrutiny_utils::zbi::ZbiType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::convert;
use std::fs;
use std::io;
use std::iter;
use std::rc::Rc;
use thiserror::Error;

/// Errors that may occur interacting with a [`Zbi`] object.
#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to read zbi from {path:?}: {error}")]
    Filesystem { path: PathBuf, error: io::Error },
    #[error("failed to hash blobfs blob at bootfs path {bootfs_path}: {error}")]
    Hash { bootfs_path: String, error: io::Error },
    #[error("failed parse zbi image from path {path:?}: {error}")]
    ParseZbi { path: PathBuf, error: anyhow::Error },
    #[error("failed parse bootfs image in zbi at path {path:?}: {error}")]
    ParseBootfs { path: PathBuf, error: anyhow::Error },
    #[error("expected to find exactly 1 bootfs section in zbi, but found {num_bootfs_sections} in zbi at path {path:?}")]
    BootfsSections { path: PathBuf, num_bootfs_sections: usize },
}

/// A Zircon Boot Image (ZBI) located or loaded via a parent data source (`PDS`).
#[derive(Clone)]
pub struct Zbi<PDS: Clone + api::DataSource>(Rc<ZbiData<PDS>>);

#[derive(Clone, Debug)]
pub struct DataSource<PDS: Clone + api::DataSource> {
    parent: Option<PDS>,
    path: PathBuf,
}

struct ZbiData<PDS: Clone + api::DataSource> {
    data_source: DataSource<PDS>,
    bootfs_reader: Rc<RefCell<BootfsReader>>,
}

impl<PDS: Clone + api::DataSource> Zbi<PDS> {
    /// Constructs a [`Zbi`] by loading the image from `path`, and noting its `parent_data_source`.
    pub fn new(path: PathBuf, parent_data_source: Option<PDS>) -> Result<Self, Error> {
        let buffer =
            fs::read(&path).map_err(|error| Error::Filesystem { path: path.clone(), error })?;
        let mut zbi_reader = ZbiReader::new(buffer);
        let zbi_sections =
            zbi_reader.parse().map_err(|error| Error::ParseZbi { path: path.clone(), error })?;
        let bootfs_sections = zbi_sections
            .into_iter()
            .filter(|section| section.section_type == ZbiType::StorageBootfs)
            .collect::<Vec<_>>();
        let bootfs_section = match bootfs_sections.as_slice() {
            [section] => section,
            sections => {
                return Err(Error::BootfsSections {
                    path: path.clone(),
                    num_bootfs_sections: sections.len(),
                });
            }
        };

        let bootfs_reader = Rc::new(RefCell::new(BootfsReader::new(bootfs_section.buffer.clone())));
        let data_source = DataSource { parent: parent_data_source, path };

        Ok(Self(Rc::new(ZbiData { data_source, bootfs_reader })))
    }
}

impl<PDS: Clone + api::DataSource<SourcePath = PathBuf> + 'static> api::Zbi for Zbi<PDS> {
    type BootfsPath = PathBuf;
    type Blob = BootfsBlob<PDS>;
    type Error = Error;

    fn bootfs(
        &self,
    ) -> Result<Box<dyn Iterator<Item = (Self::BootfsPath, Self::Blob)>>, Self::Error> {
        let mut bootfs_reader = self.0.bootfs_reader.borrow_mut();

        let mut bootfs_files = HashMap::new();
        for (path_string, bytes) in bootfs_reader
            .parse()
            .map_err(|error| Error::ParseBootfs { path: self.0.data_source.path.clone(), error })?
            .into_iter()
        {
            let hash: Hash = MerkleTree::from_reader(bytes.as_slice())
                .map_err(|error| Error::Hash { bootfs_path: path_string.clone(), error })?
                .root()
                .into();
            bootfs_files.insert(path_string.into(), (hash, bytes));
        }

        let data_source = self.0.data_source.clone();

        Ok(Box::new(bootfs_files.into_iter().map(move |(path, (hash, bytes))| {
            (path, BootfsBlob { data_source: data_source.clone(), hash, bytes })
        })))
    }
}

impl<PDS: Clone + api::DataSource<SourcePath = PathBuf> + 'static> api::DataSource
    for DataSource<PDS>
{
    type SourcePath = PathBuf;

    fn kind(&self) -> api::DataSourceKind {
        api::DataSourceKind::ZbiBootfs
    }

    fn parent(&self) -> Option<Box<dyn api::DataSource<SourcePath = Self::SourcePath>>> {
        self.parent.clone().map(|parent_data_source| {
            let parent_data_source: Box<dyn api::DataSource<SourcePath = Self::SourcePath>> =
                Box::new(parent_data_source);
            parent_data_source
        })
    }

    fn children(
        &self,
    ) -> Box<dyn Iterator<Item = Box<dyn api::DataSource<SourcePath = Self::SourcePath>>>> {
        Box::new(iter::empty())
    }

    fn path(&self) -> Option<Self::SourcePath> {
        Some(self.path.clone())
    }

    fn version(&self) -> api::DataSourceVersion {
        // TODO: Add support for exposing the zbi+bootfs format version.
        api::DataSourceVersion::Unknown
    }
}

pub struct BootfsBlob<PDS: Clone + api::DataSource> {
    data_source: DataSource<PDS>,
    hash: Hash,
    bytes: Vec<u8>,
}

impl<PDS: Clone + api::DataSource<SourcePath = PathBuf> + 'static> api::Blob for BootfsBlob<PDS> {
    type Hash = Hash;

    type ReaderSeeker = Cursor<Vec<u8>>;

    type DataSource = DataSource<PDS>;

    type Error = convert::Infallible;

    fn hash(&self) -> Self::Hash {
        self.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        Ok(Cursor::new(self.bytes.clone()))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([self.data_source.clone()].into_iter())
    }
}
