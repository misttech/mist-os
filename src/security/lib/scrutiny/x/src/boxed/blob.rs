// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::data_source as ds;
use super::hash::Hash;
use dyn_clone::clone_trait_object;
use dyn_clone::DynClone;
use fuchsia_hash::ParseHashError as FuchsiaParseHashError;
use fuchsia_merkle::Hash as FuchsiaMerkleHash;
use fuchsia_merkle::MerkleTree as FuchsiaMerkleTree;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::iter;
use std::path;
use std::rc::Rc;
use std::str::FromStr as _;
use thiserror::Error;

/// Detailed error for `BlobSet::blob()` failure.
#[derive(Debug, Error)]
pub enum BlobOpenError {
    #[error("blob not found: {hash}, in directory: {directory}")]
    BlobNotFound { hash: Box<dyn api::Hash>, directory: Box<dyn api::Path> },
    #[error("multiple errors opening blob: {errors:?}")]
    Multiple { errors: Vec<BlobOpenError> },
}

/// Internal abstraction for a set of blobs.
pub(crate) trait BlobSet {
    /// Iterate over blobs in this set.
    fn iter(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>>;

    /// Access a particular blob in this set.
    fn blob(&self, hash: Box<dyn api::Hash>) -> Result<Box<dyn api::Blob>, BlobOpenError>;

    /// Iterate over this blob set's data sources.
    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>>;
}

impl BlobSet for Box<dyn BlobSet> {
    fn iter(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>> {
        self.as_ref().iter()
    }

    fn blob(&self, hash: Box<dyn api::Hash>) -> Result<Box<dyn api::Blob>, BlobOpenError> {
        self.as_ref().blob(hash)
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        self.as_ref().data_sources()
    }
}

mod data_source {
    use super::super::api;
    use super::super::data_source;
    use std::hash;

    /// `api::DataSource` for `super::BlobDirectory` blobs.
    #[derive(Clone, Debug)]
    pub(crate) struct BlobDirectory {
        directory: Box<dyn api::Path>,
    }

    impl BlobDirectory {
        pub fn new(directory: Box<dyn api::Path>) -> Self {
            Self { directory }
        }
    }

    impl PartialEq for BlobDirectory {
        fn eq(&self, other: &Self) -> bool {
            self.directory.as_ref() == other.directory.as_ref()
        }
    }

    impl Eq for BlobDirectory {}

    impl hash::Hash for BlobDirectory {
        fn hash<H: hash::Hasher>(&self, state: &mut H) {
            self.directory.as_ref().hash(state)
        }
    }

    impl data_source::DataSourceInfo for BlobDirectory {
        fn kind(&self) -> api::DataSourceKind {
            api::DataSourceKind::BlobDirectory
        }

        fn path(&self) -> Option<Box<dyn api::Path>> {
            Some(self.directory.clone())
        }

        fn version(&self) -> api::DataSourceVersion {
            // TODO: Add support for directory-as-blob-archive versioning.
            api::DataSourceVersion::Unknown
        }
    }
}

#[derive(Clone)]
pub(crate) struct CompositeBlobSet {
    delegates: Vec<Rc<dyn BlobSet>>,
}

impl CompositeBlobSet {
    pub fn new(delegates: impl Iterator<Item = Rc<dyn BlobSet>>) -> Self {
        Self { delegates: delegates.collect() }
    }
}

impl BlobSet for CompositeBlobSet {
    fn iter(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>> {
        Box::new(CompositeBlobSetIterator::new(Box::new(self.delegates.clone().into_iter())))
    }

    fn blob(&self, hash: Box<dyn api::Hash>) -> Result<Box<dyn api::Blob>, BlobOpenError> {
        let mut errors = vec![];
        let mut delegates_iter = self.delegates.clone().into_iter();
        while let Some(delegate) = delegates_iter.next() {
            match delegate.blob(hash.clone()) {
                Ok(blob) => {
                    return Ok(Box::new(CompositeBlob::new_with_blob_sets(
                        blob,
                        delegates_iter.clone(),
                    )));
                }
                Err(error) => {
                    errors.push(error);
                }
            }
        }

        Err(BlobOpenError::Multiple { errors })
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        let mut data_sources = vec![];

        for delegate in self.delegates.iter() {
            data_sources.extend(delegate.data_sources())
        }

        Box::new(data_sources.into_iter())
    }
}

/// Reifies dynamically clonable iterator as a trait for internal use with blob sets.
trait DynCloneIterator: DynClone + Iterator {}

impl<I, DCI: DynClone + Iterator<Item = I>> DynCloneIterator for DCI {}

clone_trait_object!(<I> DynCloneIterator<Item = I>);

/// Iterator implementation for blob sets that are composed of multiple blob set delegates.
struct CompositeBlobSetIterator {
    /// Iterator over current delegate's blobs.
    blob_iterator: Box<dyn Iterator<Item = Box<dyn api::Blob>>>,
    /// Iterator over subsequent blob sets that have not yet been visited.
    blob_set_iterator: Box<dyn DynCloneIterator<Item = Rc<dyn BlobSet>>>,
    /// Set of blobs that have already been observed during iteration.
    visited: HashSet<Box<dyn api::Hash>>,
}

impl CompositeBlobSetIterator {
    /// Constructs a new iterator that will visit all blobs in any blob set in `blob_set_iterator`.
    fn new(blob_set_iterator: Box<dyn DynCloneIterator<Item = Rc<dyn BlobSet>>>) -> Self {
        Self { blob_iterator: Box::new(iter::empty()), blob_set_iterator, visited: HashSet::new() }
    }

    /// Returns the next blob in `self.blob_iterator`, or else the first blob in
    /// `self.blob_set_iterator.next()`.
    ///
    /// This is the next blob for consideration of `self` as an iterator, but performs no
    /// deduplication.
    fn next_blob(&mut self) -> Option<Box<dyn api::Blob>> {
        // Check in-flight blob iterator.
        if let result @ Some(_) = self.blob_iterator.next() {
            return result;
        }

        // Keep checking unconsumed blob sets for a blob.
        while let Some(blob_set) = self.blob_set_iterator.next() {
            self.blob_iterator = blob_set.iter();
            if let result @ Some(_) = self.blob_iterator.next() {
                return result;
            }
        }

        return None;
    }
}

impl Iterator for CompositeBlobSetIterator {
    type Item = Box<dyn api::Blob>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut first_blob: Option<Box<dyn api::Blob>> = None;
        while let Some(blob) = self.next_blob() {
            if self.visited.insert(blob.hash()) {
                first_blob = Some(blob);
                break;
            }
        }

        match first_blob {
            // When a blob is found for the first time, locate it in all subsequent blob sets while
            // constructing its `CompositeBlob`.
            Some(first_blob) => Some(Box::new(CompositeBlob::new_with_blob_sets(
                first_blob,
                self.blob_set_iterator.clone(),
            ))),
            None => None,
        }
    }
}

/// A blob that may be backed by multiple data sources.
struct CompositeBlob {
    blob: Box<dyn api::Blob>,
    data_sources: Vec<Box<dyn api::DataSource>>,
}

impl CompositeBlob {
    /// Constructs a new [`CompositeBlob`] that refers to `first_blob`, and instances of the same
    /// blob (by `hash()`) that are found in `other_blob_sets`.
    pub fn new_with_blob_sets(
        first_blob: Box<dyn api::Blob>,
        other_blob_sets: impl Iterator<Item = Rc<dyn BlobSet>>,
    ) -> Self {
        let mut data_sources: Vec<_> = first_blob.data_sources().collect();
        let hash = first_blob.hash();
        for blob_set in other_blob_sets {
            if let Ok(blob) = blob_set.blob(hash.clone()) {
                for data_source in blob.data_sources() {
                    if !data_sources.contains(&data_source) {
                        data_sources.push(data_source);
                    }
                }
            }
        }
        Self { blob: first_blob, data_sources }
    }
}

impl api::Blob for CompositeBlob {
    fn hash(&self) -> Box<dyn api::Hash> {
        self.blob.hash()
    }

    fn reader_seeker(&self) -> Result<Box<dyn api::ReaderSeeker>, api::BlobError> {
        self.blob.reader_seeker()
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        Box::new(self.data_sources.clone().into_iter())
    }
}

/// Detailed error for parsing a hash digest (hex) string as a path.
#[derive(Debug, Error)]
pub enum ParseHashPathError {
    #[error("blob fuchsia merkle root string path contains non-unicode characters: {path_string}")]
    NonUnicodeCharacters { path_string: String },
    #[error("path does not contain fuchsia merkle root string: {path_string}: {fuchsia_parse_hash_error}")]
    NonFuchsiaMerkleRoot { path_string: String, fuchsia_parse_hash_error: FuchsiaParseHashError },
}

fn parse_path_as_hash<P: AsRef<path::Path>>(
    path: P,
) -> Result<Box<dyn api::Hash>, ParseHashPathError> {
    let path_ref = path.as_ref();
    let hash_str = path_ref.to_str().ok_or_else(|| ParseHashPathError::NonUnicodeCharacters {
        path_string: path_ref.to_string_lossy().to_string(),
    })?;
    let hash: Hash = FuchsiaMerkleHash::from_str(hash_str)
        .map_err(|fuchsia_parse_hash_error| ParseHashPathError::NonFuchsiaMerkleRoot {
            path_string: path_ref.to_string_lossy().to_string(),
            fuchsia_parse_hash_error,
        })?
        .into();
    Ok(Box::new(hash))
}

/// Detailed error for `BlobDirectory::new()` failure.
#[derive(Debug, Error)]
pub enum BlobDirectoryError {
    #[error("failed to list files in blob directory: {0}")]
    ListError(io::Error),
    #[error("failed to stat directory entry: {0}")]
    DirEntryError(io::Error),
    #[error("failed to losslessly convert file name to string: {0}")]
    PathStringError(String),
    #[error("failed to process blob path: {0}")]
    PathError(#[from] ParseHashPathError),
    #[error("failed to read blob from blob directory: {0}")]
    ReadBlobError(io::Error),
    #[error("hash mismatch: hash from path: {hash_from_path}; computed hash: {computed_hash}")]
    HashMismatch { hash_from_path: Box<dyn api::Hash>, computed_hash: Box<dyn api::Hash> },
}

/// [`Blob`] implementation for a blobs backed by a [`BlobDirectory`].
#[derive(Clone)]
pub(crate) struct FileBlob {
    hash: Box<dyn api::Hash>,
    blob_set: BlobDirectory,
}

impl FileBlob {
    fn new(hash: Box<dyn api::Hash>, blob_set: BlobDirectory) -> Self {
        Self { hash, blob_set }
    }
}

impl api::Blob for FileBlob {
    fn hash(&self) -> Box<dyn api::Hash> {
        self.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Box<dyn api::ReaderSeeker>, api::BlobError> {
        let hash = format!("{}", self.hash());
        let path = self.blob_set.directory().as_ref().as_ref().join(&hash);
        Ok(Box::new(fs::File::open(&path).map_err(|error| api::BlobError::IoError {
            hash: self.hash(),
            directory: self.blob_set.directory().clone(),
            io_error_string: format!("{}", error),
        })?))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        self.blob_set.data_sources()
    }
}

/// [`Iterator`] implementation for for blobs backed by a [`BlobDirectory`].
pub(crate) struct BlobDirectoryIterator {
    next_blob_id_idx: usize,
    blob_set: BlobDirectory,
}

impl BlobDirectoryIterator {
    /// Constructs a [`BlobDirectoryIterator`] that will iterate over all blobs in `blob_set`.
    pub fn new(blob_set: BlobDirectory) -> Self {
        Self { next_blob_id_idx: 0, blob_set }
    }
}

impl Iterator for BlobDirectoryIterator {
    type Item = Box<dyn api::Blob>;

    fn next(&mut self) -> Option<Self::Item> {
        let blob_ids = self.blob_set.blob_ids();
        if self.next_blob_id_idx >= blob_ids.len() {
            return None;
        }

        let blob_id_idx = self.next_blob_id_idx;
        self.next_blob_id_idx += 1;

        let hash = blob_ids[blob_id_idx].clone();
        let blob_set = self.blob_set.clone();
        Some(Box::new(FileBlob::new(hash, blob_set)))
    }
}

/// [`BlobSet`] implementation backed by a directory of blobs named after their Fuchsia merkle root
/// hashes. This object wraps a reference-counted pointer to its state, which makes it cheap to
/// clone. Note that objects of this type are constructed via a builder that that is responsible
/// for pre-computing the identity of blobs that can be loaded from the underlying directory.
#[derive(Clone)]
pub(crate) struct BlobDirectory(Rc<BlobDirectoryData>);

impl BlobDirectory {
    /// Constructs a new [`BlobDirectory`] backed by `directory`.
    pub fn new(
        mut parent_data_source: Option<ds::DataSource>,
        directory: Box<dyn api::Path>,
    ) -> Result<Box<dyn BlobSet>, BlobDirectoryError> {
        let paths =
            fs::read_dir(directory.as_ref().as_ref()).map_err(BlobDirectoryError::ListError)?;
        let dir_entries: Vec<_> =
            paths.collect::<Result<Vec<_>, _>>().map_err(BlobDirectoryError::DirEntryError)?;

        let mut blob_ids = dir_entries
            .into_par_iter()
            .map(|dir_entry| {
                let file_name = dir_entry.file_name();
                let file_name = file_name.to_str().ok_or_else(|| {
                    BlobDirectoryError::PathStringError(String::from(file_name.to_string_lossy()))
                })?;
                let hash_from_path =
                    parse_path_as_hash(file_name).map_err(BlobDirectoryError::PathError)?;

                let mut blob_file =
                    fs::File::open(dir_entry.path()).map_err(BlobDirectoryError::ReadBlobError)?;
                let fuchsia_hash = FuchsiaMerkleTree::from_reader(&mut blob_file)
                    .map_err(BlobDirectoryError::ReadBlobError)?
                    .root();
                let computed_hash: Box<dyn api::Hash> = Box::new(Hash::from(fuchsia_hash));
                if hash_from_path.as_ref() != computed_hash.as_ref() {
                    Err(BlobDirectoryError::HashMismatch { hash_from_path, computed_hash })
                } else {
                    Ok(computed_hash)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        blob_ids.sort();

        let data_source = ds::DataSource::new(
            vec![],
            Box::new(data_source::BlobDirectory::new(directory.clone())),
        );
        if let Some(parent_data_source) = parent_data_source.as_mut() {
            parent_data_source.add_child(data_source.clone());
        }

        Ok(Box::new(Self(Rc::new(BlobDirectoryData { directory, blob_ids, data_source }))))
    }

    /// Gets the path to this blobs directory.
    fn directory(&self) -> &Box<dyn api::Path> {
        &self.0.directory
    }

    /// Gets the hashes in this blobs directory.
    fn blob_ids(&self) -> &Vec<Box<dyn api::Hash>> {
        &self.0.blob_ids
    }
}

impl BlobSet for BlobDirectory {
    fn iter(&self) -> Box<dyn Iterator<Item = Box<dyn api::Blob>>> {
        Box::new(BlobDirectoryIterator::new(self.clone()))
    }

    fn blob(&self, hash: Box<dyn api::Hash>) -> Result<Box<dyn api::Blob>, BlobOpenError> {
        if self.blob_ids().contains(&hash) {
            Ok(Box::new(FileBlob::new(hash, self.clone())))
        } else {
            Err(BlobOpenError::BlobNotFound { directory: self.0.directory.clone(), hash })
        }
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        let data_source: Box<dyn api::DataSource> = Box::new(self.0.data_source.clone());
        Box::new([data_source].into_iter())
    }
}

/// Internal state of a [`BlobDirectory`].
struct BlobDirectoryData {
    /// Path to the underlying directory on the local filesystem.
    directory: Box<dyn api::Path>,

    /// Set of blob identities (content hashes) found in the underlying directory.
    blob_ids: Vec<Box<dyn api::Hash>>,

    /// Data source associated with blob directory.
    data_source: ds::DataSource,
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::hash::Hash;
    use super::BlobDirectory;
    use super::BlobOpenError;
    use fuchsia_hash::HASH_SIZE as FUCHSIA_HASH_SIZE;
    use fuchsia_merkle::Hash as FuchsiaMerkleHash;
    use fuchsia_merkle::MerkleTree as FuchsiaMerkleTree;
    use maplit::hashmap;
    use std::fs;
    use std::io::Write as _;
    use tempfile::tempdir;

    macro_rules! assert_ref_eq {
        ($left_val:expr, $right_val: expr) => {
            assert_eq!($left_val.as_ref(), $right_val.as_ref())
        };
    }

    macro_rules! fuchsia_hash {
        ($bytes:expr) => {
            FuchsiaMerkleTree::from_reader($bytes).unwrap().root()
        };
    }

    macro_rules! assert_blob_is {
        ($blob_set:expr, $blob:expr, $bytes:expr) => {
            let blob_fuchsia_hash = fuchsia_hash!($bytes);
            let blob_hash: Box<dyn api::Hash> = Box::new(Hash::from(blob_fuchsia_hash));
            assert_ref_eq!(blob_hash, $blob.hash());
            let expected_data_sources: Vec<_> = $blob_set.data_sources().collect();
            let actual_data_sources: Vec<_> = $blob.data_sources().collect();
            assert_eq!(expected_data_sources, actual_data_sources);
            let mut blob_reader_seeker = $blob.reader_seeker().expect("blob reader-seeker");
            let mut blob_contents = vec![];
            blob_reader_seeker.read_to_end(&mut blob_contents).expect("read blob to end");
            assert_ref_eq!($bytes, blob_contents.as_slice());
        };
    }

    macro_rules! assert_blob_set_contains {
        ($blob_set:expr, $bytes:expr) => {
            let blob_fuchsia_hash = fuchsia_hash!($bytes);
            let blob_hash: Box<dyn api::Hash> = Box::new(Hash::from(blob_fuchsia_hash));
            let found_blob = $blob_set.blob(blob_hash).expect("blob found");
            assert_blob_is!($blob_set, found_blob, $bytes);
        };
    }

    macro_rules! mk_temp_dir {
        ($file_hash_map:expr) => {{
            let temp_dir = tempdir().expect("create temporary directory");
            let dir_path = temp_dir.path();
            for (name, contents) in $file_hash_map.into_iter() {
                let path = dir_path.join(format!("{}", &name));
                let mut file = fs::File::create(&path).expect("create blob file");
                file.write_all(&contents).expect("write blob to file");
            }
            let temp_dir_path: Box<dyn api::Path> = Box::new(temp_dir.path().to_path_buf());
            (temp_dir, temp_dir_path)
        }};
    }

    #[fuchsia::test]
    fn empty_blobs_dir() {
        let temp_dir = tempdir().unwrap();
        let temp_dir_path = Box::new(temp_dir.path().to_path_buf());
        BlobDirectory::new(None, temp_dir_path).expect("blob set from empty directory");
    }

    #[fuchsia::test]
    fn single_blob_dir() {
        let blob_data = "Hello, World!";

        // Target directory contains one well-formed blob entry.
        let (_temp_dir, temp_dir_path): (_, Box<dyn api::Path>) = mk_temp_dir!(hashmap! {
            fuchsia_hash!(blob_data.as_bytes()) => blob_data.as_bytes(),
        });
        let blob_set =
            BlobDirectory::new(None, temp_dir_path.clone()).expect("single-blob directory");

        let hash_not_in_set: Box<dyn api::Hash> =
            Box::new(Hash::from(FuchsiaMerkleHash::from([0u8; FUCHSIA_HASH_SIZE])));

        // Check error contents on "failed to find blob" case.
        let missing_blob =
            blob_set.blob(hash_not_in_set.clone()).err().expect("error from blob-not-found");
        match missing_blob {
            BlobOpenError::BlobNotFound { hash, directory } => {
                assert_ref_eq!(temp_dir_path, directory);
                assert_ref_eq!(hash_not_in_set, hash);
            }
            BlobOpenError::Multiple { .. } => {
                panic!("unexpected multiple errors from blob_set.blob() with single-blob blob set");
            }
        }

        // Check `BlobSet` and `Blob` APIs for single blob in blob set.
        assert_blob_set_contains!(blob_set, blob_data.as_bytes());

        // Check that `BlobSet::iter` yields the expected single well-formed `Blob`.
        let blobs: Vec<_> = blob_set.iter().collect();
        assert_eq!(1, blobs.len());
        let single_blob = &blobs[0];
        assert_blob_is!(blob_set, single_blob, blob_data.as_bytes());
    }

    #[fuchsia::test]
    fn multi_blob_dir() {
        let blob_data = vec!["Hello, World!", "Hello, Universe!"];

        // Target directory contains two well-formed blob entries.
        let temp_dir_map = hashmap! {
            fuchsia_hash!(blob_data[0].as_bytes()) => blob_data[0].as_bytes(),
            fuchsia_hash!(blob_data[1].as_bytes()) => blob_data[1].as_bytes(),
        };
        let (_temp_dir, temp_dir_path): (_, Box<dyn api::Path>) = mk_temp_dir!(&temp_dir_map);
        let blob_set =
            BlobDirectory::new(None, temp_dir_path.clone()).expect("multi-blob directory");

        let hash_not_in_set: Box<dyn api::Hash> =
            Box::new(Hash::from(FuchsiaMerkleHash::from([0u8; FUCHSIA_HASH_SIZE])));

        // Check error contents on "failed to find blob" case.
        let missing_blob =
            blob_set.blob(hash_not_in_set.clone()).err().expect("error from blob-not-found");
        match missing_blob {
            BlobOpenError::BlobNotFound { hash, directory } => {
                assert_ref_eq!(temp_dir_path, directory);
                assert_ref_eq!(hash_not_in_set, hash);
            }
            BlobOpenError::Multiple { .. } => {
                panic!(
                    "unexpected multiple errors from blob_set.blob() with multiple-blob blob set"
                );
            }
        }

        // Check `BlobSet` and `Blob` APIs for two blobs in blob set.
        assert_blob_set_contains!(blob_set, blob_data[0].as_bytes());
        assert_blob_set_contains!(blob_set, blob_data[1].as_bytes());

        // Check that `BlobSet::iter` yields the expected two well-formed `Blob`.
        let blobs: Vec<_> = blob_set.iter().collect();
        assert_eq!(2, blobs.len());
        for blob in blobs {
            let hash_bytes: [u8; FUCHSIA_HASH_SIZE] =
                blob.hash().as_bytes().try_into().expect("well-sized Fuchsia hash");
            let fuchsia_hash = FuchsiaMerkleHash::from(hash_bytes);
            let blob_contents =
                *temp_dir_map.get(&fuchsia_hash).expect("known blob in temporary directory map");
            assert_blob_is!(blob_set, blob, blob_contents);
        }
    }
}
