// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::api::DataSource as _;
use super::blob::VerifiedMemoryBlob;
use super::bootfs::Bootfs;
use super::data_source as ds;
use derivative::Derivative;
use once_cell::sync::OnceCell;
use scrutiny_utils::bootfs::BootfsReader;
use scrutiny_utils::zbi::ZbiReader;
use scrutiny_utils::zbi::ZbiSection;
use scrutiny_utils::zbi::ZbiType;
use std::rc::Rc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to open zbi blob: {0}")]
    Open(#[from] api::BlobError),
    #[error("failed to read zbi blob: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed parse zbi image: {0}")]
    ParseZbi(#[from] anyhow::Error),
}

#[derive(Clone, Debug)]
pub(crate) struct Zbi(Rc<ZbiData>);

impl Zbi {
    pub fn new(
        mut parent_data_source: Option<ds::DataSource>,
        path: Box<dyn api::Path>,
        blob: Box<dyn api::Blob>,
    ) -> Result<Self, Error> {
        let mut buffer = vec![];
        blob.reader_seeker()?.read_to_end(&mut buffer)?;
        let buffer = buffer;
        let mut zbi_reader = ZbiReader::new(buffer);
        let zbi_sections = zbi_reader.parse()?;

        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Zbi,
            Some(path),
            // TODO: Add support for exposing the zbi version.
            api::DataSourceVersion::Unknown,
        ));

        if let Some(parent_data_source) = parent_data_source.as_mut() {
            parent_data_source.add_child(data_source.clone());
        }

        Ok(Self(Rc::new(ZbiData { data_source, sections: zbi_sections, bootfs: OnceCell::new() })))
    }

    /// Returns borrow of [`super::bootfs::Bootfs`] *implementation* to client that has access to a
    /// [`Zbi`] *implementation*.
    pub fn bootfs(&self) -> Result<&Bootfs, api::ZbiError> {
        self.0.bootfs.get_or_try_init(|| self.init_bootfs())
    }

    fn init_bootfs(&self) -> Result<Bootfs, api::ZbiError> {
        let bootfs_sections = self
            .0
            .sections
            .iter()
            .filter(|section| section.section_type == ZbiType::StorageBootfs)
            .collect::<Vec<_>>();
        let bootfs_section = match bootfs_sections.as_slice() {
            [section] => section,
            sections => {
                return Err(api::ZbiError::BootfsSections { num_sections: sections.len() });
            }
        };
        let mut bootfs_reader = BootfsReader::new(bootfs_section.buffer.clone());
        let mut bootfs_files = vec![];
        for (path_string, bytes) in bootfs_reader
            .parse()
            .map_err(|error| api::ZbiError::ParseBootfs {
                path: self.0.data_source.path().expect("zbi path"),
                error,
            })?
            .into_iter()
        {
            let path: Box<dyn api::Path> = Box::new(path_string);
            let blob = VerifiedMemoryBlob::new(
                [Box::new(self.0.data_source.clone()) as Box<dyn api::DataSource>].into_iter(),
                bytes,
            )
            .map_err(|error| api::ZbiError::Hash { bootfs_path: path.clone(), error })?;
            bootfs_files.push((path, blob));
        }

        Ok(Bootfs::new(self.0.data_source.clone(), bootfs_files))
    }
}

impl api::Zbi for Zbi {
    fn bootfs(&self) -> Result<Box<dyn api::Bootfs>, api::ZbiError> {
        Ok(Box::new(self.0.bootfs.get_or_try_init(|| self.init_bootfs())?.clone()))
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
struct ZbiData {
    data_source: ds::DataSource,
    #[derivative(Debug = "ignore")]
    sections: Vec<ZbiSection>,
    #[derivative(Debug = "ignore")]
    bootfs: OnceCell<Bootfs>,
}
