// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api::Blob as BlobApi;
use crate::api::DataSource as DataSourceApi;
use crate::api::Hash as HashApi;
use crate::data_source::BlobFsArchive;
use crate::hash::Hash;
use fuchsia_hash::ParseHashError as FuchsiaParseHashError;
use fuchsia_merkle::Hash as FuchsiaMerkleHash;
use scrutiny_utils::blobfs::BlobFsReader;
use scrutiny_utils::blobfs::BlobFsReaderBuilder;
use scrutiny_utils::io::ReadSeek;
use scrutiny_utils::io::TryClonableBufReaderFile;
use std::cell::RefCell;
use std::error;
use std::fs::File;
use std::io;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::str::FromStr;
use thiserror::Error;

pub(crate) trait BlobSet {
    /// Concrete type for the content-addressed hash used to identify blobs.
    type Hash: HashApi;

    /// Concrete type for individual blobs.
    type Blob: BlobApi<DataSource = Self::DataSource>;

    /// Concrete type for data sources of blobs.
    type DataSource: DataSourceApi;

    /// Concrete type for errors returned by `BlobSet::blob`.
    type Error: error::Error;

    /// Iterate over blobs in this set.
    fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>>;

    /// Access a particular blob in this set.
    fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error>;

    /// Iterate over this blob set's data sources.
    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>>;
}

// TODO(fxbug.dev/112505): `scrutiny_x` should contain its own blobfs archive reader pattern based
// on an API owned by the storage team. The implementation below uses a bespoke blobfs archive
// parser that is not owned by the storage team.

/// [`BlobSet`] implementation backed by a blobfs archive.
#[derive(Clone)]
pub(crate) struct BlobFsBlobSet(Rc<BlobFsBlobSetData>);

impl BlobFsBlobSet {
    /// Gets the hashes in this blobfs archive.
    pub fn blob_ids(&self) -> &Vec<Hash> {
        &self.0.blob_ids
    }

    /// Gets the path to this blobfs archive.
    pub fn archive_path(&self) -> &PathBuf {
        &self.0.archive_path
    }

    /// Gets the shared blobfs archive reader for this blobfs archive.
    pub fn reader(&self) -> &Rc<RefCell<BlobFsReader<TryClonableBufReaderFile>>> {
        &self.0.reader
    }

    /// # Panics
    ///
    /// Concurrent invocations of `BlobFsBlobSet::reader_seeker` is not supported.
    pub fn reader_seeker(&self, hash: &Hash) -> Result<Box<dyn ReadSeek>, BlobFsError> {
        let path = PathBuf::from(format!("{}", hash));
        let mut blobfs_reader = self.reader().borrow_mut();
        blobfs_reader.open(&path).map_err(|error| BlobFsError::BlobFsError {
            archive_path: self.archive_path().clone(),
            error,
        })
    }
}

/// Data stored in [`BlobFsBlobSet`] behind a shared reference.
#[derive(Clone)]
struct BlobFsBlobSetData {
    archive_path: PathBuf,
    blob_ids: Vec<Hash>,
    reader: Rc<RefCell<BlobFsReader<TryClonableBufReaderFile>>>,
}

#[derive(Debug, Error)]
pub(crate) enum BlobFsError {
    #[error("Blob not found: {hash}, in blobfs archive: {archive_path}")]
    BlobNotFound { archive_path: PathBuf, hash: Hash },
    #[error("Error reading from blobfs archive: {archive_path}: {error}")]
    BlobFsError { archive_path: PathBuf, error: anyhow::Error },
}

#[derive(Debug, Error)]
pub(crate) enum ParseHashPathError {
    #[error("Blob fuchsia merkle root string path contains non-unicode characters: {path_string}")]
    NonUnicodeCharacters { path_string: String },
    #[error("Path does not contain fuchsia merkle root string: {path_string}: {fuchsia_parse_hash_error}")]
    NonFuchsiaMerkleRoot { path_string: String, fuchsia_parse_hash_error: FuchsiaParseHashError },
}

fn parse_path_as_hash<P: AsRef<Path>>(path: P) -> Result<Hash, ParseHashPathError> {
    let path_ref = path.as_ref();
    let hash_str = path_ref.to_str().ok_or_else(|| ParseHashPathError::NonUnicodeCharacters {
        path_string: path_ref.to_string_lossy().to_string(),
    })?;
    let hash = FuchsiaMerkleHash::from_str(hash_str)
        .map_err(|fuchsia_parse_hash_error| ParseHashPathError::NonFuchsiaMerkleRoot {
            path_string: path_ref.to_string_lossy().to_string(),
            fuchsia_parse_hash_error,
        })?
        .into();
    Ok(hash)
}

pub(crate) struct BlobFsIterator {
    next_blob_id_idx: usize,
    blob_set: BlobFsBlobSet,
}

/// Iterator type returned by `BlobFsBlobSet::iter`.
impl BlobFsIterator {
    /// Constructs a [`BlobFsIterator`] that will iterate over all blobs in `blob_set`.
    pub fn new(blob_set: BlobFsBlobSet) -> Self {
        Self { next_blob_id_idx: 0, blob_set }
    }
}

impl Iterator for BlobFsIterator {
    type Item = BlobFsBlob;

    fn next(&mut self) -> Option<Self::Item> {
        let blob_ids = self.blob_set.blob_ids();
        if self.next_blob_id_idx >= blob_ids.len() {
            return None;
        }

        let blob_id_idx = self.next_blob_id_idx;
        self.next_blob_id_idx += 1;

        let hash = blob_ids[blob_id_idx].clone();
        let blob_set = self.blob_set.clone();
        Some(BlobFsBlob::new(hash, blob_set))
    }
}

impl BlobSet for BlobFsBlobSet {
    type Hash = Hash;
    type Blob = BlobFsBlob;
    type DataSource = BlobFsArchive;
    type Error = BlobFsError;

    fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
        // Note: `self.clone()` is a cheap increment of a reference count.
        Box::new(BlobFsIterator::new(self.clone()))
    }

    fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error> {
        if self.blob_ids().contains(&hash) {
            // Note: `self.clone()` is a cheap increment of a reference count.
            Ok(BlobFsBlob::new(hash, self.clone()))
        } else {
            Err(BlobFsError::BlobNotFound { archive_path: self.archive_path().clone(), hash })
        }
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([BlobFsArchive::new(self.archive_path().clone())].into_iter())
    }
}

pub(crate) struct BlobFsBlob {
    hash: Hash,
    blob_set: BlobFsBlobSet,
}

impl BlobFsBlob {
    /// Constructs a [`BlobFsBlob`] for accessing the blob with hash `hash` from `blob_set`.
    pub fn new(hash: Hash, blob_set: BlobFsBlobSet) -> Self {
        Self { hash, blob_set }
    }
}

impl BlobApi for BlobFsBlob {
    type Hash = Hash;
    type ReaderSeeker = Box<dyn ReadSeek>;
    type DataSource = BlobFsArchive;
    type Error = BlobFsError;

    fn hash(&self) -> Self::Hash {
        self.hash.clone()
    }

    /// # Panics
    ///
    /// Delegates to `BlobFsBlobSet::reader_seeker`.
    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        self.blob_set.reader_seeker(&self.hash)
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        self.blob_set.data_sources()
    }
}

/// Errors that may be emitted by `BlobFsBlobSetBuilder::build`.
#[derive(Debug, Error)]
pub(crate) enum BlobFsBlobSetBuilderError {
    #[error("Attempt to build blobfs set without specifying an archive path")]
    MissingArchivePath,
    #[error("Failed to open blobfs archive: {0}")]
    OpenError(#[from] io::Error),
    #[error("Error preparing blobfs archive reader: {0}")]
    BlobFsError(#[from] anyhow::Error),
    #[error("Failed to process blobfs path: {0}")]
    PathError(#[from] ParseHashPathError),
}

/// Builder pattern for constructing [`BlobFsBlobSet`].
pub(crate) struct BlobFsBlobSetBuilder {
    archive_path: Option<PathBuf>,
}

impl BlobFsBlobSetBuilder {
    /// Instantiates empty builder.
    pub fn new() -> Self {
        Self { archive_path: None }
    }

    /// Associates builder with blobfs archive specified by `archive_path`.
    pub fn archive<P: AsRef<Path>>(mut self, archive_path: P) -> Self {
        self.archive_path = Some(archive_path.as_ref().to_path_buf());
        self
    }

    /// Builds a [`BlobFsBlobSet`] based on data accumulated in builder.
    pub fn build(self) -> Result<BlobFsBlobSet, BlobFsBlobSetBuilderError> {
        if self.archive_path.is_none() {
            return Err(BlobFsBlobSetBuilderError::MissingArchivePath);
        }

        let archive_path = self.archive_path.unwrap();
        let reader_seeker =
            TryClonableBufReaderFile::from(BufReader::new(File::open(&archive_path)?));
        let blobfs_reader = BlobFsReaderBuilder::new().archive(reader_seeker)?.build()?;
        let blob_ids = blobfs_reader
            .blob_paths()
            .map(|blob_path| parse_path_as_hash(blob_path).map_err(BlobFsBlobSetBuilderError::from))
            .collect::<Result<Vec<Hash>, BlobFsBlobSetBuilderError>>()?;
        Ok(BlobFsBlobSet(Rc::new(BlobFsBlobSetData {
            archive_path,
            blob_ids,
            reader: Rc::new(RefCell::new(blobfs_reader)),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::BlobFsBlobSetBuilder;

    // TODO(fxbug.dev/116719): Below is a vacuous test that exercise `super::BlobFs*` code. When
    // this code is updated to use a stable storage API, these tests should be updated to be
    // meaningful.
    #[fuchsia::test]
    fn exercise_blobfs_blob_set_api() {
        let _ = BlobFsBlobSetBuilder::new().archive("/some/path").build();
    }
}

#[cfg(test)]
pub mod fake {
    use super::BlobSet as BlobSetApi;
    use crate::api::Blob as BlobApi;
    use crate::data_source::fake::DataSource;
    use crate::hash::fake::Hash;
    use std::collections::HashMap;
    use std::hash;
    use std::io::Cursor;
    use thiserror::Error;

    #[derive(Clone, Debug, Eq, Error, PartialEq)]
    pub(crate) enum BlobError {
        #[error("fake blob error")]
        BlobError,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct Blob {
        hash: Hash,
        blob: Result<Cursor<Vec<u8>>, BlobError>,
        data_sources: Vec<DataSource>,
    }

    impl Default for Blob {
        fn default() -> Self {
            Self {
                hash: Hash::default(),
                blob: Ok(Cursor::new(vec![])),
                data_sources: vec![DataSource::default()],
            }
        }
    }

    impl hash::Hash for Blob {
        fn hash<H>(&self, state: &mut H)
        where
            H: hash::Hasher,
        {
            self.hash.hash(state)
        }
    }

    #[derive(Clone, Debug, Eq, Error, PartialEq)]
    pub(crate) enum BlobSetError {
        #[error("fake blob set error: blob not found")]
        BlobNotFound,
        #[error("fake blob set error: {0}")]
        BlobError(#[from] BlobError),
    }

    impl Blob {
        pub fn new(hash: Hash, blob: Vec<u8>) -> Self {
            Self { hash, blob: Ok(Cursor::new(blob)), data_sources: vec![DataSource::default()] }
        }

        pub fn new_error(hash: Hash) -> Self {
            Self {
                hash,
                blob: Err(BlobError::BlobError),
                data_sources: vec![DataSource::default()],
            }
        }

        pub fn new_with_data_sources(
            hash: Hash,
            blob: Vec<u8>,
            data_sources: Vec<DataSource>,
        ) -> Self {
            Self { hash, blob: Ok(Cursor::new(blob)), data_sources }
        }

        pub fn new_error_with_data_sources(hash: Hash, data_sources: Vec<DataSource>) -> Self {
            Self { hash, blob: Err(BlobError::BlobError), data_sources: data_sources }
        }
    }

    impl BlobApi for Blob {
        type Hash = Hash;
        type ReaderSeeker = Cursor<Vec<u8>>;
        type DataSource = DataSource;
        type Error = BlobError;

        fn hash(&self) -> Self::Hash {
            self.hash
        }

        fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
            self.blob.clone()
        }

        fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
            Box::new(self.data_sources.clone().into_iter())
        }
    }

    pub(crate) struct BlobSet {
        blobs: HashMap<Hash, Result<Blob, BlobError>>,
    }

    impl BlobSet {
        pub fn from_hash_map(blobs: HashMap<Hash, Result<Blob, BlobError>>) -> Self {
            Self { blobs }
        }

        pub fn into_hash_map(self) -> HashMap<Hash, Result<Blob, BlobError>> {
            self.blobs
        }
    }

    impl BlobSetApi for BlobSet {
        type Blob = Blob;
        type DataSource = DataSource;
        type Hash = Hash;
        type Error = BlobSetError;

        fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
            Box::new(
                self.blobs
                    .values()
                    .filter_map(|blob_or_err| blob_or_err.as_ref().ok())
                    .map(|blob| blob.clone())
                    .collect::<Vec<Self::Blob>>()
                    .into_iter(),
            )
        }

        fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error> {
            match self.blobs.get(&hash) {
                Some(blob_or_err) => blob_or_err.clone().map_err(BlobSetError::from),
                None => Err(BlobSetError::BlobNotFound),
            }
        }

        fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
            Box::new(vec![DataSource].into_iter())
        }
    }

    pub(crate) struct BlobSetBuilder {
        next_id: Hash,
        blobs: HashMap<Hash, Result<Blob, BlobError>>,
    }

    impl BlobSetBuilder {
        pub fn new() -> Self {
            Self { next_id: 0, blobs: HashMap::new() }
        }

        pub fn new_with_start_id(start_id: Hash) -> Self {
            Self { next_id: start_id, blobs: HashMap::new() }
        }

        pub fn blob(mut self, blob: Vec<u8>) -> (Self, Hash) {
            if self.blobs.contains_key(&self.next_id) {
                panic!("Duplicate ID inserting contents into fake blob set: {}", self.next_id)
            }
            let id = self.next_id;
            self.next_id += 1;
            self.blobs.insert(id, Ok(Blob::new(id, blob)));
            (self, id)
        }

        pub fn error(mut self, err: BlobError) -> (Self, Hash) {
            if self.blobs.contains_key(&self.next_id) {
                panic!("Duplicate ID inserting error into fake blob set: {}", self.next_id)
            }
            let id = self.next_id;
            self.next_id += 1;
            self.blobs.insert(id, Err(err));
            (self, id)
        }

        pub fn hash_blob(mut self, hash: Hash, blob: Vec<u8>) -> Self {
            if self.blobs.contains_key(&hash) {
                panic!("Duplicate ID inserting contents with predefined hash: {}", hash)
            }
            self.blobs.insert(hash, Ok(Blob::new(hash, blob)));
            self
        }

        pub fn hash_error(mut self, hash: Hash, err: BlobError) -> Self {
            if self.blobs.contains_key(&hash) {
                panic!("Duplicate ID inserting error with predefined hash: {}", hash)
            }
            self.blobs.insert(hash, Err(err));
            self
        }

        pub fn build(self) -> (BlobSet, Hash) {
            (BlobSet::from_hash_map(self.blobs), self.next_id)
        }
    }

    mod test {
        use super::Blob;
        use super::BlobError;
        use super::BlobSet;
        use super::BlobSetBuilder;
        use super::BlobSetError;
        use crate::api::Blob as BlobApi;
        use crate::blob::BlobSet as BlobSetApi;
        use crate::data_source::fake::DataSource;
        use maplit::hashmap;
        use maplit::hashset;

        #[fuchsia::test]
        fn test_blob_new() {
            let (a, b, c) = (Blob::new(0, vec![]), Blob::new(0, vec![]), Blob::new(1, vec![]));
            assert_eq!(a.hash(), b.hash());
            assert!(a.hash() != c.hash());
            let v = vec![a, b, c];
            for x in v.into_iter() {
                assert!(x.reader_seeker().is_ok());
                let ds = x.data_sources().collect::<Vec<DataSource>>();
                assert_eq!(ds.len(), 1);
                assert_eq!(ds[0], DataSource);
            }
        }

        #[fuchsia::test]
        fn test_blob_new_error() {
            let (a, b, c) = (Blob::new_error(0), Blob::new_error(0), Blob::new_error(1));
            assert_eq!(a.hash(), b.hash());
            assert!(a.hash() != c.hash());
            let v = vec![a, b, c];
            for x in v.into_iter() {
                assert_eq!(x.reader_seeker(), Err(BlobError::BlobError));
                let ds = x.data_sources().collect::<Vec<DataSource>>();
                assert_eq!(ds.len(), 1);
                assert_eq!(ds[0], DataSource);
            }
        }

        #[fuchsia::test]
        fn test_blob_new_with_data_sources() {
            let ds_a = vec![];
            let ds_b = vec![DataSource];
            let ds_c = vec![DataSource, DataSource];
            let (a, b, c) = (
                Blob::new_with_data_sources(0, vec![], ds_a.clone()),
                Blob::new_with_data_sources(0, vec![], ds_b.clone()),
                Blob::new_with_data_sources(1, vec![], ds_c.clone()),
            );
            assert_eq!(a.hash(), b.hash());
            assert!(a.hash() != c.hash());

            let ds = a.data_sources().collect::<Vec<DataSource>>();
            assert_eq!(ds, ds_a);

            let ds = b.data_sources().collect::<Vec<DataSource>>();
            assert_eq!(ds, ds_b);

            let ds = c.data_sources().collect::<Vec<DataSource>>();
            assert_eq!(ds, ds_c);

            let v = vec![a, b, c];
            for x in v.iter() {
                assert!(x.reader_seeker().is_ok());
            }
        }

        #[fuchsia::test]
        fn test_blob_new_with_error_with_data_sources() {
            let ds_a = vec![];
            let ds_b = vec![DataSource];
            let ds_c = vec![DataSource, DataSource];
            let (a, b, c) = (
                Blob::new_error_with_data_sources(0, ds_a.clone()),
                Blob::new_error_with_data_sources(0, ds_b.clone()),
                Blob::new_error_with_data_sources(1, ds_c.clone()),
            );
            assert_eq!(a.hash(), b.hash());
            assert!(a.hash() != c.hash());

            let ds = a.data_sources().collect::<Vec<DataSource>>();
            assert_eq!(ds, ds_a);

            let ds = b.data_sources().collect::<Vec<DataSource>>();
            assert_eq!(ds, ds_b);

            let ds = c.data_sources().collect::<Vec<DataSource>>();
            assert_eq!(ds, ds_c);

            let v = vec![a, b, c];
            for x in v.iter() {
                assert_eq!(x.reader_seeker(), Err(BlobError::BlobError));
            }
        }

        #[fuchsia::test]
        fn test_blob_set_from_hash_map() {
            let blob_set = BlobSet::from_hash_map(hashmap! {
                0 => Ok(Blob::new(0, vec![])),
                1 => Err(BlobError::BlobError.into()),

                // `from_hash_map()` can be used to simulate a bad state where key != Blob.hash.
                2 => Ok(Blob::new(7, vec![])),
            });

            assert_eq!(blob_set.blob(0), Ok(Blob::new(0, vec![])));
            assert_eq!(blob_set.blob(1), Err(BlobError::BlobError.into()));
            assert_eq!(blob_set.blob(2), Ok(Blob::new(7, vec![])));
            assert_eq!(blob_set.blob(3), Err(BlobSetError::BlobNotFound));

            // Assert each member of `blobs` appears exactly once in iteration.
            let mut blobs = hashset! {
                Blob::new(0, vec![]),
                Blob::new(7, vec![]),
            };
            for blob in blob_set.iter() {
                // HashSet::remove() == true iff value was found in the set.
                assert!(blobs.remove(&blob));
            }
            assert_eq!(blobs.len(), 0);
        }

        #[fuchsia::test]
        fn test_blob_set_builder_simple() {
            let (blob_set, next_id) = BlobSetBuilder::new().build();
            assert_eq!(next_id, 0);
            assert_eq!(blob_set.into_hash_map(), hashmap! {});

            let (_, assigned_id) = BlobSetBuilder::new().blob(vec![]);
            assert_eq!(assigned_id, 0);
        }

        #[fuchsia::test]
        fn test_blob_set_builder_complex() {
            let mut hash = 7;
            let mut builder = BlobSetBuilder::new_with_start_id(hash);
            let mut data = hashmap! {};
            let blobs = vec![vec![], vec![0], vec![1]];
            let errs = vec![BlobError::BlobError];

            for blob in blobs.into_iter() {
                (builder, hash) = builder.blob(blob.clone());
                data.insert(hash, Ok(Blob::new(hash, blob)));
            }

            for err in errs.into_iter() {
                (builder, hash) = builder.error(err.clone());
                data.insert(hash, Err(err));
            }

            data.insert(1000, Ok(Blob::new(1000, vec![9])));
            builder = builder.hash_blob(1000, vec![9]);

            data.insert(2000, Err(BlobError::BlobError));
            builder = builder.hash_error(2000, BlobError::BlobError);

            let blob_set = builder.build().0;

            assert_eq!(blob_set.blob(0), Err(BlobSetError::BlobNotFound));
            assert_eq!(blob_set.blob(1), Err(BlobSetError::BlobNotFound));
            assert_eq!(blob_set.blob(3000), Err(BlobSetError::BlobNotFound));
            assert_eq!(data, blob_set.into_hash_map());
        }
    }
}
