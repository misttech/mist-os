// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::api::Blob as _;
use super::blob::BlobOpenError;
use super::blob::BlobSet;
use super::blob::VerifiedMemoryBlob;
use super::data_source as ds;
use derivative::Derivative;
use once_cell::sync::OnceCell;
use scrutiny_utils::bootfs::BootfsReader;
use scrutiny_utils::zbi::ZbiReader;
use scrutiny_utils::zbi::ZbiType;
use std::cell::RefCell;
use std::collections::HashMap;
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
    #[error("expected to find exactly 1 bootfs section in zbi, but found {0}")]
    BootfsSections(usize),
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
        let bootfs_sections = zbi_sections
            .into_iter()
            .filter(|section| section.section_type == ZbiType::StorageBootfs)
            .collect::<Vec<_>>();
        let bootfs_section = match bootfs_sections.as_slice() {
            [section] => section,
            sections => {
                return Err(Error::BootfsSections(sections.len()));
            }
        };

        let data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Zbi,
            Some(path),
            // TODO: Add support for exposing the zbi version.
            api::DataSourceVersion::Unknown,
        ));

        if let Some(parent_data_source) = parent_data_source.as_mut() {
            parent_data_source.add_child(data_source.clone());
        }
        let bootfs_reader = Rc::new(RefCell::new(BootfsReader::new(bootfs_section.buffer.clone())));

        Ok(Self(Rc::new(ZbiData {
            data_source: Box::new(data_source),
            bootfs_reader,
            bootfs: OnceCell::new(),
        })))
    }

    fn init_bootfs(&self) -> Result<Bootfs, api::ZbiError> {
        let mut bootfs_reader = self.0.bootfs_reader.borrow_mut();
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
            let blob = VerifiedMemoryBlob::new([self.0.data_source.clone()].into_iter(), bytes)
                .map_err(|error| api::ZbiError::Hash { bootfs_path: path.clone(), error })?;
            bootfs_files.push((path, blob));
        }

        Ok(Bootfs::new(self.0.data_source.clone(), bootfs_files))
    }
}

impl api::Zbi for Zbi {
    fn bootfs(
        &self,
    ) -> Result<Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>>, api::ZbiError>
    {
        self.0.bootfs.get_or_try_init(|| self.init_bootfs()).map(Bootfs::iter_by_path)
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
struct ZbiData {
    data_source: Box<dyn api::DataSource>,
    #[derivative(Debug = "ignore")]
    bootfs_reader: Rc<RefCell<BootfsReader>>,
    #[derivative(Debug = "ignore")]
    bootfs: OnceCell<Bootfs>,
}

#[derive(Clone)]
struct Bootfs {
    data_source: Box<dyn api::DataSource>,
    blobs_by_path: HashMap<Box<dyn api::Path>, VerifiedMemoryBlob>,
    blobs_by_hash: HashMap<Box<dyn api::Hash>, VerifiedMemoryBlob>,
}

impl Bootfs {
    fn new<BlobsByPath: Clone + IntoIterator<Item = (Box<dyn api::Path>, VerifiedMemoryBlob)>>(
        data_source: Box<dyn api::DataSource>,
        blobs_by_path: BlobsByPath,
    ) -> Self {
        let blobs_by_hash = blobs_by_path
            .clone()
            .into_iter()
            .map(|(_path, blob)| (blob.hash(), blob))
            .collect::<HashMap<_, _>>();
        let blobs_by_path = blobs_by_path.into_iter().collect::<HashMap<_, _>>();
        Self { data_source, blobs_by_path, blobs_by_hash }
    }

    fn iter_by_path(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>> {
        Box::new(self.blobs_by_path.clone().into_iter().map(|(path, verified_memory_blob)| {
            let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
            (path, blob)
        }))
    }
}

impl BlobSet for Bootfs {
    fn iter(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>> {
        Box::new(self.blobs_by_hash.clone().into_iter().map(|(_path, verified_memory_blob)| {
            let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
            blob
        }))
    }

    fn blob(&self, hash: Box<dyn api::Hash>) -> Result<Box<dyn api::Blob>, BlobOpenError> {
        self.blobs_by_hash
            .get(&hash)
            .ok_or_else(|| BlobOpenError::BlobNotFound { hash, directory: None })
            .map(|verified_memory_blob_ref| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob_ref.clone());
                blob
            })
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        Box::new([self.data_source.clone()].into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::blob::BlobSet as _;
    use super::super::blob::VerifiedMemoryBlob;
    use super::super::data_source as ds;
    use super::Bootfs;
    use std::collections::HashMap;
    use std::io::Read as _;

    #[fuchsia::test]
    fn bootfs_iter_by_paths() {
        let data_source: Box<dyn api::DataSource> =
            Box::new(ds::DataSource::new(ds::DataSourceInfo::new(
                api::DataSourceKind::Unknown,
                None,
                api::DataSourceVersion::Unknown,
            )));
        let path_1: Box<dyn api::Path> = Box::new("path_1");
        let path_2: Box<dyn api::Path> = Box::new("path_2");
        let blob_1 =
            VerifiedMemoryBlob::new([data_source.clone()].into_iter(), "blob_1".as_bytes().into())
                .expect("blob");
        let blob_2 =
            VerifiedMemoryBlob::new([data_source.clone()].into_iter(), "blob_2".as_bytes().into())
                .expect("blob");
        let blobs = [(path_1.clone(), blob_1.clone()), (path_2.clone(), blob_2.clone())];
        let bootfs = Bootfs::new(data_source, blobs.clone().into_iter());
        let mut expected = blobs
            .into_iter()
            .map(|(path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (path, blob)
            })
            .collect::<HashMap<_, _>>();
        let actual = bootfs.iter_by_path().collect::<Vec<(_, _)>>();
        for (path, blob) in actual {
            let expected_blob = expected.get(&path).expect("actual blob in expectation set");
            assert_eq!(expected_blob.hash().as_ref(), blob.hash().as_ref());
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected.remove(&path);
        }
        assert_eq!(0, expected.len());
    }

    #[fuchsia::test]
    fn bootfs_blob_set_api() {
        let data_source: Box<dyn api::DataSource> =
            Box::new(ds::DataSource::new(ds::DataSourceInfo::new(
                api::DataSourceKind::Unknown,
                None,
                api::DataSourceVersion::Unknown,
            )));
        let path_1: Box<dyn api::Path> = Box::new("path_1");
        let path_2: Box<dyn api::Path> = Box::new("path_2");
        let blob_1 =
            VerifiedMemoryBlob::new([data_source.clone()].into_iter(), "blob_1".as_bytes().into())
                .expect("blob");
        let blob_2 =
            VerifiedMemoryBlob::new([data_source.clone()].into_iter(), "blob_2".as_bytes().into())
                .expect("blob");
        let blobs = [(path_1.clone(), blob_1.clone()), (path_2.clone(), blob_2.clone())];
        let bootfs = Bootfs::new(data_source, blobs.clone().into_iter());
        let mut expected = blobs
            .into_iter()
            .map(|(_path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (blob.hash(), blob)
            })
            .collect::<HashMap<_, _>>();
        let actual = bootfs.iter().collect::<Vec<_>>();
        for blob in actual {
            let hash = blob.hash();
            let expected_blob = expected.get(&hash).expect("actual blob in expectation set");
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected.remove(&hash);
        }
        assert_eq!(0, expected.len());
    }

    #[fuchsia::test]
    fn bootfs_different_iterators() {
        let data_source: Box<dyn api::DataSource> =
            Box::new(ds::DataSource::new(ds::DataSourceInfo::new(
                api::DataSourceKind::Unknown,
                None,
                api::DataSourceVersion::Unknown,
            )));
        let blob_1_path_1: Box<dyn api::Path> = Box::new("blob_1_path_1");
        let blob_1_path_2: Box<dyn api::Path> = Box::new("blob_1_path_2");
        let blob_2_path: Box<dyn api::Path> = Box::new("blob_2_path");
        let blob_1 =
            VerifiedMemoryBlob::new([data_source.clone()].into_iter(), "blob_1".as_bytes().into())
                .expect("blob");
        let blob_2 =
            VerifiedMemoryBlob::new([data_source.clone()].into_iter(), "blob_2".as_bytes().into())
                .expect("blob");
        let blobs = [
            (blob_1_path_1.clone(), blob_1.clone()),
            (blob_1_path_2.clone(), blob_1.clone()),
            (blob_2_path.clone(), blob_2.clone()),
        ];
        let bootfs = Bootfs::new(data_source, blobs.clone().into_iter());

        // Iterate-by-path should contain all 3 entries.
        let mut expected_by_path = blobs
            .clone()
            .into_iter()
            .map(|(path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (path, blob)
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(3, expected_by_path.len());
        let actual_by_path = bootfs.iter_by_path().collect::<Vec<(_, _)>>();
        for (path, blob) in actual_by_path {
            let expected_blob =
                expected_by_path.get(&path).expect("actual blob in expectation set");
            assert_eq!(expected_blob.hash().as_ref(), blob.hash().as_ref());
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected_by_path.remove(&path);
        }
        assert_eq!(0, expected_by_path.len());

        // Iterate-by-hash should contain all 2 entries; two identical blobs at different paths get
        // deduplicated.
        let mut expected_by_hash = blobs
            .into_iter()
            .map(|(_path, verified_memory_blob)| {
                let blob: Box<dyn api::Blob> = Box::new(verified_memory_blob);
                (blob.hash(), blob)
            })
            // Will add `blob_1` twice, but dedup by `blob.hash()`.
            .collect::<HashMap<_, _>>();
        assert_eq!(2, expected_by_hash.len());
        let actual = bootfs.iter().collect::<Vec<_>>();
        for blob in actual {
            let hash = blob.hash();
            let expected_blob =
                expected_by_hash.get(&hash).expect("actual blob in expectation set");
            let mut expected_bytes = vec![];
            expected_blob
                .reader_seeker()
                .expect("expected blob reader/seeker")
                .read_to_end(&mut expected_bytes)
                .expect("read expected blob");
            let mut actual_bytes = vec![];
            blob.reader_seeker()
                .expect("actual blob reader/seeker")
                .read_to_end(&mut actual_bytes)
                .expect("read actual blob");
            assert_eq!(expected_bytes, actual_bytes);
            expected_by_hash.remove(&hash);
        }
        assert_eq!(0, expected_by_hash.len());
    }
}
