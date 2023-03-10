// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::api;
use crate::data_source::BlobDirectory;
use crate::data_source::BlobSource;
use crate::hash::Hash;
use crate::product_bundle::ProductBundleRepositoryBlob;
use fuchsia_hash::ParseHashError as FuchsiaParseHashError;
use fuchsia_merkle::Hash as FuchsiaMerkleHash;
use fuchsia_merkle::MerkleTree;
use rayon::prelude::*;
use scrutiny_utils::io::ReadSeek;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt::Debug;
use std::fs;
use std::io;
use std::iter;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::str::FromStr;
use std::vec;
use thiserror::Error;

pub trait BlobSet {
    /// Concrete type for the content-addressed hash used to identify blobs.
    type Hash: api::Hash;

    /// Concrete type for individual blobs.
    type Blob: api::Blob<DataSource = Self::DataSource>;

    /// Concrete type for data sources of blobs.
    type DataSource: api::DataSource;

    /// Concrete type for errors returned by `BlobSet::blob`.
    type Error: api::Error;

    /// Iterate over blobs in this set.
    fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>>;

    /// Access a particular blob in this set.
    fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error>;

    /// Iterate over this blob set's data sources.
    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>>;
}

pub struct CompositeBlobSet<B: api::Blob + 'static, E: api::Error> {
    delegates:
        Vec<Rc<dyn BlobSet<Hash = B::Hash, Blob = B, DataSource = B::DataSource, Error = E>>>,
}

impl<B: api::Blob + 'static, E: api::Error> CompositeBlobSet<B, E> {
    pub fn new<
        I: Into<Rc<dyn BlobSet<Hash = B::Hash, Blob = B, DataSource = B::DataSource, Error = E>>>,
    >(
        delegates: impl Iterator<Item = I>,
    ) -> Self {
        Self { delegates: delegates.map(|delegate| delegate.into()).collect() }
    }
}

impl<B: api::Blob + 'static, E: api::Error> BlobSet for CompositeBlobSet<B, E> {
    type Hash = B::Hash;
    type Blob = CompositeBlob<B>;
    type DataSource = B::DataSource;
    type Error = anyhow::Error;

    fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
        Box::new(CompositeBlobSetIterator::new(self.delegates.clone().into_iter()))
    }

    fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error> {
        let mut errors = vec![];
        let mut delegates_iter = self.delegates.clone().into_iter();
        while let Some(delegate) = delegates_iter.next() {
            match delegate.blob(hash.clone()) {
                Ok(blob) => {
                    return Ok(CompositeBlob::new_with_blob_sets(blob, delegates_iter.clone()))
                }
                Err(error) => {
                    errors.push(error);
                }
            }
        }

        Err(errors.into_iter().fold(
            anyhow::anyhow!("blob with id {} not found in multiple data sources", hash),
            |anyhow_error, error| anyhow_error.context(format!("{:?}", error)),
        ))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        let mut data_sources = vec![];

        for delegate in self.delegates.iter() {
            data_sources.extend(delegate.data_sources())
        }

        Box::new(data_sources.into_iter())
    }
}

pub struct CompositeBlobSetIterator<
    B: api::Blob + 'static,
    E: api::Error,
    I: Iterator<
            Item = Rc<dyn BlobSet<Hash = B::Hash, Blob = B, DataSource = B::DataSource, Error = E>>,
        > + Clone,
> {
    blob_iterator: Box<dyn Iterator<Item = B>>,
    blob_set_iterator: I,
    visited: HashSet<B::Hash>,
}

impl<
        B: api::Blob + 'static,
        E: api::Error,
        I: Iterator<
                Item = Rc<
                    dyn BlobSet<Hash = B::Hash, Blob = B, DataSource = B::DataSource, Error = E>,
                >,
            > + Clone,
    > CompositeBlobSetIterator<B, E, I>
{
    pub fn new(blob_set_iterator: I) -> Self {
        Self { blob_iterator: Box::new(iter::empty()), blob_set_iterator, visited: HashSet::new() }
    }

    /// Returns the next blob in `self.blob_iterator`, or else the first blob in
    /// `self.blob_set_iterator.next()`.
    ///
    /// This is the next blob for consideration of `self` as an iterator, but performs no
    /// deduplication.
    fn next_blob(&mut self) -> Option<B> {
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

impl<
        B: api::Blob + 'static,
        E: api::Error,
        I: Iterator<
                Item = Rc<
                    dyn BlobSet<Hash = B::Hash, Blob = B, DataSource = B::DataSource, Error = E>,
                >,
            > + Clone,
    > Iterator for CompositeBlobSetIterator<B, E, I>
{
    type Item = CompositeBlob<B>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut first_blob: Option<B> = None;
        while let Some(blob) = self.next_blob() {
            if self.visited.insert(blob.hash()) {
                first_blob = Some(blob);
                break;
            }
        }

        match first_blob {
            // When a blob is found for the first time, locate it in all subsequent blob sets while
            // constructing its `CompositeBlob`.
            Some(first_blob) => {
                Some(CompositeBlob::new_with_blob_sets(first_blob, self.blob_set_iterator.clone()))
            }
            None => None,
        }
    }
}

/// A `api::Blob` implementation that is backed by multiple data sources for the same blob. All
/// operations delegate to the first blob in the underlying composite, except for `data_sources`,
/// which concatenates the series of sources returned by each underlying `api::Blob` implementation.
///
/// This type is used, for example, when the system is aware of multiple repositories that may serve
/// some of the same blobs.
pub struct CompositeBlob<B: api::Blob + 'static> {
    blobs: Vec<B>,
}

impl<B: api::Blob + 'static> CompositeBlob<B> {
    /// Constructs a new [`CompositeBlob`] that refers to `first_blob`, and instances of the same
    /// blob (by `hash()`) that are found in `other_blob_sets`.
    pub fn new_with_blob_sets<
        E: api::Error,
        BS: Borrow<dyn BlobSet<Hash = B::Hash, Blob = B, DataSource = B::DataSource, Error = E>>,
        I: Iterator<Item = BS>,
    >(
        first_blob: B,
        other_blob_sets: I,
    ) -> Self {
        let hash = first_blob.hash();
        let blobs = iter::once(first_blob)
            .chain(other_blob_sets.filter_map(|set| set.borrow().blob(hash.clone()).ok()))
            .collect();
        Self { blobs }
    }
}

impl<B: api::Blob + 'static> api::Blob for CompositeBlob<B> {
    type Hash = B::Hash;
    type ReaderSeeker = B::ReaderSeeker;
    type DataSource = B::DataSource;
    type Error = B::Error;

    fn hash(&self) -> Self::Hash {
        self.blobs[0].hash()
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        self.blobs[0].reader_seeker()
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        let mut data_sources = vec![];
        for blob in self.blobs.iter() {
            data_sources.extend(blob.data_sources())
        }
        Box::new(data_sources.into_iter())
    }
}

#[derive(Debug, Error)]
pub enum ParseHashPathError {
    #[error("blob fuchsia merkle root string path contains non-unicode characters: {path_string}")]
    NonUnicodeCharacters { path_string: String },
    #[error("path does not contain fuchsia merkle root string: {path_string}: {fuchsia_parse_hash_error}")]
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

// TODO(fxbug.dev/112505): `scrutiny_x` should contain its own blobfs archive reader pattern based
// on an API owned by the storage team. The implementation below uses a bespoke blobfs archive
// parser that is not owned by the storage team.
#[cfg(test)]
mod blobfs {
    use super::parse_path_as_hash;
    use super::BlobSet;
    use super::ParseHashPathError;
    use crate::api;
    use crate::data_source::blobfs::BlobFsArchive;
    use crate::hash::Hash;
    use scrutiny_utils::blobfs::BlobFsReader;
    use scrutiny_utils::blobfs::BlobFsReaderBuilder;
    use scrutiny_utils::io::ReadSeek;
    use scrutiny_utils::io::TryClonableBufReaderFile;
    use std::cell::RefCell;
    use std::fs::File;
    use std::io;
    use std::io::BufReader;
    use std::path::Path;
    use std::path::PathBuf;
    use std::rc::Rc;
    use thiserror::Error;

    /// [`BlobSet`] implementation backed by a blobfs archive.
    #[derive(Clone)]
    pub struct BlobFsBlobSet(Rc<BlobFsBlobSetData>);

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
    pub enum BlobFsError {
        #[error("blob not found: {hash}, in blobfs archive: {archive_path}")]
        BlobNotFound { archive_path: PathBuf, hash: Hash },
        #[error("error reading from blobfs archive: {archive_path}: {error}")]
        BlobFsError { archive_path: PathBuf, error: anyhow::Error },
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

    pub struct BlobFsBlob {
        hash: Hash,
        blob_set: BlobFsBlobSet,
    }

    impl BlobFsBlob {
        /// Constructs a [`BlobFsBlob`] for accessing the blob with hash `hash` from `blob_set`.
        pub fn new(hash: Hash, blob_set: BlobFsBlobSet) -> Self {
            Self { hash, blob_set }
        }
    }

    impl api::Blob for BlobFsBlob {
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
        #[error("failed to open blobfs archive: {0}")]
        OpenError(#[from] io::Error),
        #[error("error preparing blobfs archive reader: {0}")]
        BlobFsError(#[from] anyhow::Error),
        #[error("failed to process blobfs path: {0}")]
        PathError(#[from] ParseHashPathError),
    }

    /// Builder pattern for constructing [`BlobFsBlobSet`].
    pub(crate) struct BlobFsBlobSetBuilder {
        archive_path: PathBuf,
    }

    impl BlobFsBlobSetBuilder {
        /// Instantiates empty builder.
        pub fn new<P: AsRef<Path>>(archive_path: P) -> Self {
            Self { archive_path: archive_path.as_ref().to_path_buf() }
        }

        /// Builds a [`BlobFsBlobSet`] based on data accumulated in builder.
        #[tracing::instrument(level = "trace", skip_all)]
        pub fn build(self) -> Result<BlobFsBlobSet, BlobFsBlobSetBuilderError> {
            let reader_seeker =
                TryClonableBufReaderFile::from(BufReader::new(File::open(&self.archive_path)?));
            let blobfs_reader = BlobFsReaderBuilder::new().archive(reader_seeker)?.build()?;
            let blob_ids = blobfs_reader
                .blob_paths()
                .map(|blob_path| {
                    parse_path_as_hash(blob_path).map_err(BlobFsBlobSetBuilderError::from)
                })
                .collect::<Result<Vec<Hash>, BlobFsBlobSetBuilderError>>()?;
            Ok(BlobFsBlobSet(Rc::new(BlobFsBlobSetData {
                archive_path: self.archive_path,
                blob_ids,
                reader: Rc::new(RefCell::new(blobfs_reader)),
            })))
        }
    }
}

/// [`BlobSet`] implementation backed by a directory of blobs named after their Fuchsia merkle root
/// hashes. This object wraps a reference-counted pointer to its state, which makes it cheap to
/// clone. Note that objects of this type are constructed via a builder that that is responsible
/// for pre-computing the identity of blobs that can be loaded from the underlying directory.
#[derive(Clone)]
pub struct BlobDirectoryBlobSet(Rc<BlobDirectoryBlobSetData>);

impl BlobDirectoryBlobSet {
    /// Creates a builder for a [`BlobDirectoryBlobSet`].
    pub fn builder<P: AsRef<Path>>(directory: P) -> BlobDirectoryBlobSetBuilder {
        BlobDirectoryBlobSetBuilder::new(directory)
    }

    /// Gets the path to this blobs directory.
    pub fn directory(&self) -> &PathBuf {
        self.0.directory()
    }

    /// Gets the hashes in this blobs directory.
    pub fn blob_ids(&self) -> &Vec<Hash> {
        self.0.blob_ids()
    }

    /// Constructs a new blob directory blob set from backing data.
    fn new(directory: PathBuf, blob_ids: Vec<Hash>) -> Self {
        Self(Rc::new(BlobDirectoryBlobSetData::new(directory, blob_ids)))
    }
}

/// Internal state of a [`BlobDirectoryBlobSet`].
pub struct BlobDirectoryBlobSetData {
    /// Path to the underlying directory on the local filesystem.
    directory: PathBuf,

    /// Set of blob identities (content hashes) found in the underlying directory.
    blob_ids: Vec<Hash>,
}

impl BlobDirectoryBlobSetData {
    fn new(directory: PathBuf, blob_ids: Vec<Hash>) -> Self {
        Self { directory, blob_ids }
    }

    fn directory(&self) -> &PathBuf {
        &self.directory
    }

    fn blob_ids(&self) -> &Vec<Hash> {
        &self.blob_ids
    }
}

/// Errors that can be encountered accessing blobs via a [`BlobDirectoryBlobSet`].
#[derive(Debug, Error)]
pub enum BlobDirectoryError {
    #[error("blob not found: {hash}, in blob directory: {directory}")]
    BlobNotFound { directory: PathBuf, hash: Hash },
    #[error("error reading from blob directory: {directory}: {error}")]
    IoError { directory: PathBuf, error: io::Error },
}

/// [`Iterator`] implementation for for blobs backed by a [`BlobDirectoryBlobSet`].
pub struct BlobDirectoryIterator {
    next_blob_id_idx: usize,
    blob_set: BlobDirectoryBlobSet,
}

impl BlobDirectoryIterator {
    /// Constructs a [`BlobDirectoryIterator`] that will iterate over all blobs in `blob_set`.
    pub fn new(blob_set: BlobDirectoryBlobSet) -> Self {
        Self { next_blob_id_idx: 0, blob_set }
    }
}

impl Iterator for BlobDirectoryIterator {
    type Item = FileBlob;

    fn next(&mut self) -> Option<Self::Item> {
        let blob_ids = self.blob_set.blob_ids();
        if self.next_blob_id_idx >= blob_ids.len() {
            return None;
        }

        let blob_id_idx = self.next_blob_id_idx;
        self.next_blob_id_idx += 1;

        let hash = blob_ids[blob_id_idx].clone();
        let blob_set = self.blob_set.clone();
        Some(FileBlob::new(hash, blob_set))
    }
}

impl BlobSet for BlobDirectoryBlobSet {
    type Hash = Hash;
    type Blob = FileBlob;
    type DataSource = BlobDirectory;
    type Error = BlobDirectoryError;

    fn iter(&self) -> Box<dyn Iterator<Item = Self::Blob>> {
        Box::new(BlobDirectoryIterator::new(self.clone()))
    }

    fn blob(&self, hash: Self::Hash) -> Result<Self::Blob, Self::Error> {
        if self.blob_ids().contains(&hash) {
            Ok(FileBlob::new(hash, self.clone()))
        } else {
            Err(BlobDirectoryError::BlobNotFound { directory: self.directory().clone(), hash })
        }
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([BlobDirectory::new(self.directory().clone())].into_iter())
    }
}

/// [`Blob`] implementation for a blobs backed by a [`BlobDirectoryBlobSet`].
pub struct FileBlob {
    hash: Hash,
    blob_set: BlobDirectoryBlobSet,
}

impl FileBlob {
    fn new(hash: Hash, blob_set: BlobDirectoryBlobSet) -> Self {
        Self { hash, blob_set }
    }
}

impl api::Blob for FileBlob {
    type Hash = Hash;
    type ReaderSeeker = Box<dyn ReadSeek>;
    type DataSource = BlobDirectory;
    type Error = BlobDirectoryError;

    fn hash(&self) -> Self::Hash {
        self.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        let hash = format!("{}", self.hash());
        let path = self.blob_set.directory().join(&hash);
        Ok(Box::new(fs::File::open(&path).map_err(|error| BlobDirectoryError::IoError {
            directory: self.blob_set.directory().clone(),
            error,
        })?))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        self.blob_set.data_sources()
    }
}

/// Unified error type for production `crate::api::Blob` implementations.
#[derive(Debug, Error)]
pub enum BlobError {
    #[cfg(test)]
    #[error("{0}")]
    BlobFsError(#[from] blobfs::BlobFsError),

    #[error("{0}")]
    BlobDirectoryError(#[from] BlobDirectoryError),

    #[error("{0:?}")]
    CompositeBlobError(#[from] anyhow::Error),
}

/// Unified `crate::api::Blob` implementation for production blob types.
pub enum Blob<CB: api::Blob<Hash = Hash, ReaderSeeker = Box<dyn ReadSeek>> + 'static>
where
    CB::Error: Into<BlobError>,
    CB::DataSource: Into<BlobSource>,
{
    #[cfg(test)]
    BlobFsBlob(blobfs::BlobFsBlob),

    FileBlob(FileBlob),
    ProductBundleRepositoryBlob(ProductBundleRepositoryBlob),
    CompositeBlob(CompositeBlob<CB>),
}

#[cfg(test)]
impl<CB: api::Blob<Hash = Hash, ReaderSeeker = Box<dyn ReadSeek>> + 'static>
    From<blobfs::BlobFsBlob> for Blob<CB>
where
    CB::Error: Into<BlobError>,
    CB::DataSource: Into<BlobSource>,
{
    fn from(blob_fs_blob: blobfs::BlobFsBlob) -> Self {
        Self::BlobFsBlob(blob_fs_blob)
    }
}

impl<CB: api::Blob<Hash = Hash, ReaderSeeker = Box<dyn ReadSeek>> + 'static> From<FileBlob>
    for Blob<CB>
where
    CB::Error: Into<BlobError>,
    CB::DataSource: Into<BlobSource>,
{
    fn from(file_blob: FileBlob) -> Self {
        Self::FileBlob(file_blob)
    }
}

impl<CB: api::Blob<Hash = Hash, ReaderSeeker = Box<dyn ReadSeek>> + 'static>
    From<ProductBundleRepositoryBlob> for Blob<CB>
where
    CB::Error: Into<BlobError>,
    CB::DataSource: Into<BlobSource>,
{
    fn from(product_bundle_repository_blob: ProductBundleRepositoryBlob) -> Self {
        Self::ProductBundleRepositoryBlob(product_bundle_repository_blob)
    }
}

impl<CB: api::Blob<Hash = Hash, ReaderSeeker = Box<dyn ReadSeek>> + 'static> From<CompositeBlob<CB>>
    for Blob<CB>
where
    CB::Error: Into<BlobError>,
    CB::DataSource: Into<BlobSource>,
{
    fn from(composite_blob: CompositeBlob<CB>) -> Self {
        Self::CompositeBlob(composite_blob)
    }
}

impl<CB: api::Blob<Hash = Hash, ReaderSeeker = Box<dyn ReadSeek>> + 'static> api::Blob for Blob<CB>
where
    CB::Error: Into<BlobError>,
    CB::DataSource: Into<BlobSource>,
{
    type Hash = Hash;
    type ReaderSeeker = Box<dyn ReadSeek>;
    type DataSource = BlobSource;
    type Error = BlobError;

    fn hash(&self) -> Self::Hash {
        match self {
            #[cfg(test)]
            Self::BlobFsBlob(blob_fs_blob) => blob_fs_blob.hash(),

            Self::FileBlob(file_blob) => file_blob.hash(),
            Self::ProductBundleRepositoryBlob(product_bundle_repository_blob) => {
                product_bundle_repository_blob.hash()
            }
            Self::CompositeBlob(composite_blob) => composite_blob.hash(),
        }
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        match self {
            #[cfg(test)]
            Self::BlobFsBlob(blob_fs_blob) => blob_fs_blob.reader_seeker().map_err(BlobError::from),
            Self::FileBlob(file_blob) => file_blob.reader_seeker().map_err(BlobError::from),
            Self::ProductBundleRepositoryBlob(product_bundle_repository_blob) => {
                product_bundle_repository_blob.reader_seeker().map_err(BlobError::from)
            }
            Self::CompositeBlob(composite_blob) => {
                composite_blob.reader_seeker().map_err(|cb_error| cb_error.into())
            }
        }
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        match self {
            #[cfg(test)]
            Self::BlobFsBlob(blob_fs_blob) => {
                Box::new(blob_fs_blob.data_sources().map(|data_source| data_source.into()))
            }

            Self::FileBlob(file_blob) => {
                Box::new(file_blob.data_sources().map(|data_source| data_source.into()))
            }
            Self::ProductBundleRepositoryBlob(product_bundle_repository_blob) => Box::new(
                product_bundle_repository_blob.data_sources().map(|data_source| data_source.into()),
            ),
            Self::CompositeBlob(composite_blob) => {
                Box::new(composite_blob.data_sources().map(|data_source| data_source.into()))
            }
        }
    }
}

/// Errors that may be emitted by `BlobDirectoryBlobSetBuilder::build`.
#[derive(Debug, Error)]
pub enum BlobDirectoryBlobSetBuilderError {
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
    HashMismatch { hash_from_path: Hash, computed_hash: Hash },
}

/// Builder pattern for constructing [`BlobDirectoryBlobSet`].
pub struct BlobDirectoryBlobSetBuilder {
    directory: PathBuf,
}

impl BlobDirectoryBlobSetBuilder {
    /// Instantiates empty builder.
    pub fn new<P: AsRef<Path>>(directory: P) -> Self {
        Self { directory: directory.as_ref().to_path_buf() }
    }

    /// Builds a [`BlobDirectoryBlobSet`] based on data accumulated in builder. This builder
    /// enumerates entries in the underlying directory and ensures that:
    ///
    /// - All entries are files with content lowercase hash hex strings;
    /// - File names match content hash of file contents.
    ///
    /// After these checks are performed, the set of verified hashes is passed to a
    /// [`BlobDirectoryBlobSet`] constructor.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn build(self) -> Result<BlobDirectoryBlobSet, BlobDirectoryBlobSetBuilderError> {
        let paths =
            fs::read_dir(&self.directory).map_err(BlobDirectoryBlobSetBuilderError::ListError)?;
        let dir_entries: Vec<_> = paths
            .collect::<Result<Vec<_>, _>>()
            .map_err(BlobDirectoryBlobSetBuilderError::DirEntryError)?;

        let mut blob_ids = dir_entries
            .into_par_iter()
            .map(|dir_entry| {
                let file_name = dir_entry.file_name();
                let file_name = file_name.to_str().ok_or_else(|| {
                    BlobDirectoryBlobSetBuilderError::PathStringError(String::from(
                        file_name.to_string_lossy(),
                    ))
                })?;
                let hash_from_path = parse_path_as_hash(file_name)
                    .map_err(BlobDirectoryBlobSetBuilderError::PathError)?;

                let mut blob_file = fs::File::open(dir_entry.path())
                    .map_err(BlobDirectoryBlobSetBuilderError::ReadBlobError)?;
                let fuchsia_hash = MerkleTree::from_reader(&mut blob_file)
                    .map_err(BlobDirectoryBlobSetBuilderError::ReadBlobError)?
                    .root();
                let computed_hash = Hash::from(fuchsia_hash);
                if hash_from_path != computed_hash {
                    return Err(BlobDirectoryBlobSetBuilderError::HashMismatch {
                        hash_from_path,
                        computed_hash,
                    });
                }
                Ok(computed_hash)
            })
            .collect::<Result<Vec<_>, _>>()?;
        blob_ids.sort();

        Ok(BlobDirectoryBlobSet::new(self.directory, blob_ids))
    }
}

#[cfg(test)]
mod tests {
    use super::blobfs::BlobFsBlobSetBuilder;
    use super::BlobDirectoryBlobSetBuilder;
    use super::BlobDirectoryBlobSetBuilderError;
    use super::BlobDirectoryError;
    use super::BlobSet;
    use super::ParseHashPathError;
    use crate::api::Blob as _;
    use crate::hash::Hash;
    use fuchsia_hash::HASH_SIZE as FUCHSIA_HASH_SIZE;
    use fuchsia_merkle::Hash as FuchsiaMerkleHash;
    use fuchsia_merkle::MerkleTree;
    use maplit::hashmap;
    use std::fs;
    use std::io::Read;
    use std::io::Write;
    use tempfile::tempdir;

    macro_rules! fuchsia_hash {
        ($bytes:expr) => {
            MerkleTree::from_reader($bytes).unwrap().root()
        };
    }

    macro_rules! assert_blob_is {
        ($blob_set:expr, $blob:expr, $bytes:expr) => {
            let blob_fuchsia_hash = fuchsia_hash!($bytes);
            let blob_hash = Hash::from(blob_fuchsia_hash);
            assert_eq!(blob_hash, $blob.hash());
            let expected_data_sources: Vec<_> = $blob_set.data_sources().collect();
            let actual_data_sources: Vec<_> = $blob.data_sources().collect();
            assert_eq!(expected_data_sources, actual_data_sources);
            let mut blob_reader_seeker = $blob.reader_seeker().unwrap();
            let mut blob_contents = vec![];
            blob_reader_seeker.read_to_end(&mut blob_contents).unwrap();
            assert_eq!($bytes, blob_contents.as_slice());
        };
    }

    macro_rules! assert_blob_set_contains {
        ($blob_set:expr, $bytes:expr) => {
            let blob_fuchsia_hash = fuchsia_hash!($bytes);
            let blob_hash = Hash::from(blob_fuchsia_hash);
            let found_blob = $blob_set.blob(blob_hash.clone()).unwrap();
            assert_blob_is!($blob_set, found_blob, $bytes);
        };
    }

    macro_rules! mk_temp_dir {
        ($file_hash_map:expr) => {{
            let temp_dir = tempdir().unwrap();
            let dir_path = temp_dir.path();
            for (name, contents) in $file_hash_map.into_iter() {
                let path = dir_path.join(format!("{}", &name));
                let mut file = fs::File::create(&path).unwrap();
                file.write_all(&contents).unwrap();
            }
            temp_dir
        }};
    }

    #[fuchsia::test]
    fn empty_blobs_dir() {
        let temp_dir = tempdir().unwrap();
        BlobDirectoryBlobSetBuilder::new(temp_dir.path()).build().unwrap();
    }

    #[fuchsia::test]
    fn single_blob_dir() {
        let blob_data = "Hello, World!";

        // Target directory contains one well-formed blob entry.
        let temp_dir = mk_temp_dir!(hashmap! {
            fuchsia_hash!(blob_data.as_bytes()) => blob_data.as_bytes(),
        });
        let blob_set = BlobDirectoryBlobSetBuilder::new(temp_dir.path()).build().unwrap();

        let hash_not_in_set = Hash::from(FuchsiaMerkleHash::from([0u8; FUCHSIA_HASH_SIZE]));

        // Check error contents on "failed to find blob" case.
        let missing_blob = blob_set.blob(hash_not_in_set.clone()).err().unwrap();
        match missing_blob {
            BlobDirectoryError::BlobNotFound { directory, hash } => {
                assert_eq!(temp_dir.path().to_path_buf(), directory);
                assert_eq!(hash_not_in_set, hash);
            }
            _ => {
                assert!(false, "Expected BlobNotFound error, but got {:?}", missing_blob);
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
        let temp_dir = mk_temp_dir!(&temp_dir_map);
        let blob_set = BlobDirectoryBlobSetBuilder::new(temp_dir.path()).build().unwrap();

        let hash_not_in_set = Hash::from(FuchsiaMerkleHash::from([0u8; FUCHSIA_HASH_SIZE]));

        // Check error contents on "failed to find blob" case.
        let missing_blob = blob_set.blob(hash_not_in_set.clone()).err().unwrap();
        match missing_blob {
            BlobDirectoryError::BlobNotFound { directory, hash } => {
                assert_eq!(temp_dir.path().to_path_buf(), directory);
                assert_eq!(hash_not_in_set, hash);
            }
            _ => {
                assert!(false, "Expected BlobNotFound error, but got {:?}", missing_blob);
            }
        }

        // Check `BlobSet` and `Blob` APIs for two blobs in blob set.
        assert_blob_set_contains!(blob_set, blob_data[0].as_bytes());
        assert_blob_set_contains!(blob_set, blob_data[1].as_bytes());

        // Check that `BlobSet::iter` yields the expected two well-formed `Blob`.
        let blobs: Vec<_> = blob_set.iter().collect();
        assert_eq!(2, blobs.len());
        for blob in blobs {
            let blob_contents = *temp_dir_map.get(&blob.hash().into()).unwrap();
            assert_blob_is!(blob_set, blob, blob_contents);
        }
    }

    #[fuchsia::test]
    fn blob_dir_directory_does_not_exist() {
        match BlobDirectoryBlobSetBuilder::new("/this/directory/definitely/does/not/exist")
            .build()
            .err()
            .unwrap()
        {
            BlobDirectoryBlobSetBuilderError::ListError(_) => {}
            err => {
                assert!(false, "Expected ListError error, but got {:?}", err);
            }
        };
    }

    #[fuchsia::test]
    fn blob_dir_invalid_path_subdir() {
        let temp_dir = tempdir().unwrap();
        let dir_path = temp_dir.path();

        let subdir_path = dir_path.join("subdir");
        fs::create_dir(subdir_path).unwrap();

        match BlobDirectoryBlobSetBuilder::new(dir_path).build().err().unwrap() {
            BlobDirectoryBlobSetBuilderError::PathError(
                ParseHashPathError::NonFuchsiaMerkleRoot { path_string, .. },
            ) => {
                assert_eq!("subdir", &path_string);
            }
            err => {
                assert!(false, "Expected ReadBlobError error, but got {:?}", err);
            }
        };
    }

    #[fuchsia::test]
    fn blob_dir_valid_path_invalid_subdir() {
        let temp_dir = tempdir().unwrap();
        let dir_path = temp_dir.path();

        let valid_merkle_root = format!("{}", fuchsia_hash!("Hello, World!".as_bytes()));
        let subdir_path = dir_path.join(&valid_merkle_root);
        fs::create_dir(subdir_path).unwrap();

        match BlobDirectoryBlobSetBuilder::new(dir_path).build().err().unwrap() {
            BlobDirectoryBlobSetBuilderError::ReadBlobError(_) => {}
            err => {
                assert!(false, "Expected ReadBlobError error, but got {:?}", err);
            }
        };
    }

    #[fuchsia::test]
    fn blob_dir_wrong_data() {
        let world_str = "Hello, World!";
        let universe_str = "Hello, Universe!";
        let world_bytes = world_str.as_bytes();
        let universe_bytes = universe_str.as_bytes();
        let world_hash = fuchsia_hash!(world_bytes);
        let universe_hash = fuchsia_hash!(universe_bytes);

        // Target directory contains a malformed blob entry: hello-world file-name-as-hash
        // contains hello-universe bytes.
        let temp_dir_map = hashmap! {
            world_hash => universe_bytes,
        };
        let temp_dir = mk_temp_dir!(&temp_dir_map);

        match BlobDirectoryBlobSetBuilder::new(temp_dir.path()).build().err().unwrap() {
            BlobDirectoryBlobSetBuilderError::HashMismatch { hash_from_path, computed_hash } => {
                let (world_hash, universe_hash): (Hash, Hash) =
                    (world_hash.into(), universe_hash.into());
                assert_eq!(world_hash, hash_from_path);
                assert_eq!(universe_hash, computed_hash);
            }
            err => {
                assert!(false, "Expected HashMismatch error, but got {:?}", err);
            }
        };
    }

    // TODO(fxbug.dev/116719): Below is a vacuous test that exercise `super::*::BlobFs*` code. When
    // this code is updated to use a stable storage API, these tests should be updated to be
    // meaningful.
    #[fuchsia::test]
    fn exercise_blobfs_blob_set_api() {
        let _ = BlobFsBlobSetBuilder::new("/some/path").build();
    }
}

#[cfg(test)]
pub mod test {
    /// Assert that two `crate::api::Blob` objects, possibly of different underlying types, refer
    /// to the same blob.
    macro_rules! assert_blobs_eq {
        ($b1:expr, $t1:ty, $b2:expr, $t2:ty) => {
            assert_eq!($b1.hash(), $b2.hash());

            // Convince type system to invoke `<dyn crate::api::DataSource as PartialEq>::eq()`.
            let ds1: Vec<_> = $b1
                .data_sources()
                .map(|data_source| {
                    let data_source: Box<
                        dyn crate::api::DataSource<
                            SourcePath = <<$t1 as crate::api::Blob>::DataSource as crate::api::DataSource>::SourcePath,
                        >,
                    > = Box::new(data_source);
                    data_source
                })
                .collect();
            let ds2: Vec<_> = $b2
                .data_sources()
                .map(|data_source| {
                    let data_source: Box<
                        dyn crate::api::DataSource<
                            SourcePath = <<$t2 as crate::api::Blob>::DataSource as crate::api::DataSource>::SourcePath,
                        >,
                    > = Box::new(data_source);
                    data_source
                })
                .collect();
            assert_eq!(ds1, ds2);

            let mut rs1 = $b1.reader_seeker().unwrap();
            let mut data1 = vec![];
            rs1.read_to_end(&mut data1).unwrap();

            let mut rs2 = $b2.reader_seeker().unwrap();
            let mut data2 = vec![];
            rs2.read_to_end(&mut data2).unwrap();

            assert_eq!(data1, data2);
        };
    }

    pub(crate) use assert_blobs_eq;
}

#[cfg(test)]
pub mod fake {
    use crate::api;
    use crate::blob;
    use crate::data_source::fake::DataSource;
    use crate::hash::test::HashGenerator;
    use std::collections::HashMap;
    use std::io::Cursor;
    use thiserror::Error;

    #[derive(Clone, Debug, Eq, Error, PartialEq)]
    pub(crate) enum BlobError {
        #[error("fake blob error")]
        BlobError,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct Blob<H: api::Hash> {
        hash: H,
        blob: Result<Cursor<Vec<u8>>, BlobError>,
        data_sources: Vec<DataSource>,
    }

    impl<H: Default + api::Hash> Default for Blob<H> {
        fn default() -> Self {
            Self {
                hash: H::default(),
                blob: Ok(Cursor::new(vec![])),
                data_sources: vec![DataSource::default()],
            }
        }
    }

    #[derive(Clone, Debug, Eq, Error, PartialEq)]
    pub(crate) enum BlobSetError {
        #[error("fake blob set error: blob not found")]
        BlobNotFound,
        #[error("fake blob set error: {0}")]
        BlobError(#[from] BlobError),
    }

    impl<H: api::Hash> Blob<H> {
        pub fn new(hash: H, blob: Vec<u8>) -> Self {
            Self { hash, blob: Ok(Cursor::new(blob)), data_sources: vec![DataSource::default()] }
        }

        pub fn new_error(hash: H) -> Self {
            Self {
                hash,
                blob: Err(BlobError::BlobError),
                data_sources: vec![DataSource::default()],
            }
        }

        pub fn new_with_data_sources(
            hash: H,
            blob: Vec<u8>,
            data_sources: Vec<DataSource>,
        ) -> Self {
            Self { hash, blob: Ok(Cursor::new(blob)), data_sources }
        }

        pub fn new_error_with_data_sources(hash: H, data_sources: Vec<DataSource>) -> Self {
            Self { hash, blob: Err(BlobError::BlobError), data_sources: data_sources }
        }
    }

    impl<H: api::Hash> api::Blob for Blob<H> {
        type Hash = H;
        type ReaderSeeker = Cursor<Vec<u8>>;
        type DataSource = DataSource;
        type Error = BlobError;

        fn hash(&self) -> Self::Hash {
            self.hash.clone()
        }

        fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
            self.blob.clone()
        }

        fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
            Box::new(self.data_sources.clone().into_iter())
        }
    }

    pub(crate) struct BlobSet<H: api::Hash> {
        blobs: HashMap<H, Result<Blob<H>, BlobError>>,
    }

    impl<H: api::Hash> BlobSet<H> {
        pub fn from_hash_map(blobs: HashMap<H, Result<Blob<H>, BlobError>>) -> Self {
            Self { blobs }
        }

        pub fn into_hash_map(self) -> HashMap<H, Result<Blob<H>, BlobError>> {
            self.blobs
        }
    }

    impl<H: api::Hash + 'static> blob::BlobSet for BlobSet<H> {
        type Blob = Blob<H>;
        type DataSource = DataSource;
        type Hash = H;
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

    pub(crate) struct BlobSetBuilder<H: api::Hash + HashGenerator> {
        next_id: H,
        blobs: HashMap<H, Result<Blob<H>, BlobError>>,
    }

    impl<H: api::Hash + HashGenerator> BlobSetBuilder<H> {
        pub fn new() -> Self {
            Self { next_id: H::default(), blobs: HashMap::new() }
        }

        pub fn new_with_start_id(start_id: H) -> Self {
            Self { next_id: start_id, blobs: HashMap::new() }
        }

        pub fn blob(mut self, blob: Vec<u8>) -> (Self, H) {
            if self.blobs.contains_key(&self.next_id) {
                panic!("Duplicate ID inserting contents into fake blob set: {}", self.next_id)
            }
            let id = self.next_id.clone();
            self.next_id = self.next_id.next();
            self.blobs.insert(id.clone(), Ok(Blob::new(id.clone(), blob)));
            (self, id)
        }

        pub fn error(mut self, err: BlobError) -> (Self, H) {
            if self.blobs.contains_key(&self.next_id) {
                panic!("Duplicate ID inserting error into fake blob set: {}", self.next_id)
            }
            let id = self.next_id.clone();
            self.next_id = self.next_id.next();
            self.blobs.insert(id.clone(), Err(err));
            (self, id)
        }

        pub fn hash_blob(mut self, hash: H, blob: Vec<u8>) -> Self {
            if self.blobs.contains_key(&hash) {
                panic!("Duplicate ID inserting contents with predefined hash: {}", hash)
            }
            self.blobs.insert(hash.clone(), Ok(Blob::new(hash, blob)));
            self
        }

        pub fn hash_error(mut self, hash: H, err: BlobError) -> Self {
            if self.blobs.contains_key(&hash) {
                panic!("Duplicate ID inserting error with predefined hash: {}", hash)
            }
            self.blobs.insert(hash, Err(err));
            self
        }

        #[tracing::instrument(level = "trace", skip_all)]
        pub fn build(self) -> (BlobSet<H>, H) {
            (BlobSet::from_hash_map(self.blobs), self.next_id)
        }
    }

    mod test {
        use super::Blob;
        use super::BlobError;
        use super::BlobSet;
        use super::BlobSetBuilder;
        use super::BlobSetError;
        use crate::api::Blob as _;
        use crate::blob::BlobSet as _;
        use crate::data_source::fake::DataSource;
        use crate::hash::fake::Hash;
        use maplit::hashmap;

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
            let mut blobs = vec![Blob::new(0, vec![]), Blob::new(7, vec![])];
            for blob in blob_set.iter() {
                // Remove blob at position matching location of blob in `blobs`.
                // This will panic on `.unwrap()` if `blob` appears nowhere in `blobs`.
                blobs.remove(blobs.iter().position(|b| b == &blob).unwrap());
            }
            assert_eq!(blobs.len(), 0);
        }

        #[fuchsia::test]
        fn test_blob_set_builder_simple() {
            let (blob_set, next_id) = BlobSetBuilder::<Hash>::new().build();
            assert_eq!(next_id, 0);
            assert_eq!(blob_set.into_hash_map(), hashmap! {});

            let (_, assigned_id) = BlobSetBuilder::<Hash>::new().blob(vec![]);
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
