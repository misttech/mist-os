// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/111242): Exercise production package implementations to eliminate dead code
// warnings.
#![allow(dead_code)]

use crate::api;
use crate::blob::BlobDirectoryError;
use crate::blob::BlobSet;
use crate::blob::CompositeBlobSet;
use crate::data_source::BlobSource;
use crate::hash::Hash;
use crate::product_bundle::ProductBundleRepositoryBlob;
use fuchsia_archive::Error as FarError;
use fuchsia_archive::Utf8Reader as FarReader;
use fuchsia_merkle::Hash as FuchsiaMerkleHash;
use fuchsia_merkle::MerkleTree as FuchsiaMerkleTree;
use fuchsia_pkg::MetaContents as FuchsiaMetaContents;
use fuchsia_pkg::MetaContentsError as FuchsiaMetaContentsError;
use fuchsia_pkg::MetaPackage as FuchsiaMetaPackage;
use fuchsia_pkg::MetaPackageError as FuchsiaMetaPackageError;
use scrutiny_utils::io::ReadSeek;
use std::cell::RefCell;
use std::fmt::Debug;
use std::io;
use std::iter;
use std::path::PathBuf;
use std::rc::Rc;
use thiserror::Error;

// TODO(fxbug.dev/111243): Use production component API when it is ready.
use crate::todo::Component;

/// Simple wrapper around `fuchsia_pkg::MetaPackage` that implements `MetaPackage` trait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetaPackage(FuchsiaMetaPackage);

impl api::MetaPackage for MetaPackage {}

impl From<FuchsiaMetaPackage> for MetaPackage {
    fn from(meta_package: FuchsiaMetaPackage) -> Self {
        Self(meta_package)
    }
}

/// Simple wrapper around `fuchsia_pkg::MetaContents` that implements `MetaContents` trait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetaContents(FuchsiaMetaContents);

impl From<FuchsiaMetaContents> for MetaContents {
    fn from(meta_contents: FuchsiaMetaContents) -> Self {
        Self(meta_contents)
    }
}

impl api::MetaContents for MetaContents {
    type Hash = Hash;
    type EntryPath = PathBuf;

    fn contents(&self) -> Box<dyn Iterator<Item = (Self::EntryPath, Self::Hash)>> {
        Box::new(
            self.0
                .contents()
                .clone()
                .into_iter()
                .map(|(path, hash)| (PathBuf::from(path), hash.into()))
                .into_iter(),
        )
    }
}

/// Errors that can occur initializing a [`Package`] via `Package::new`.
#[derive(Debug, Error)]
pub enum PackageInitializationError {
    #[error("error parsing meta/contents in meta.far: {0}")]
    MetaContentsError(#[from] FuchsiaMetaContentsError),
    #[error("error parsing meta/package in meta.far: {0}")]
    MetaPackageError(#[from] FuchsiaMetaPackageError),
    #[error("error reading meta.far for package: {0}")]
    FarError(#[from] FarError),
    #[error("error performing i/o operations on far reader for package: {0}")]
    IoError(#[from] io::Error),
}

/// Model of a Fuchsia package, including meta entries (from meta.far) and content entries
/// accessible via an associated `BlobSet`. This object wraps a reference-counted pointer
/// to its state, which makes it cheap to clone.
pub struct Package<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
>(Rc<PackageData<PackageDataSource, PackageBlobs, FarReaderSeeker>>);

/// Unboxed production `crate::package::Package` implementation used by production
/// `crate::api::Scrutiny` implementation.
pub type ScrutinyPackage = Package<
    BlobSource,
    CompositeBlobSet<ProductBundleRepositoryBlob, BlobDirectoryError>,
    Box<dyn ReadSeek>,
>;

impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > Clone for Package<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > Package<PackageDataSource, PackageBlobs, FarReaderSeeker>
where
    <PackageBlobs as BlobSet>::Hash: From<FuchsiaMerkleHash>,
{
    pub(crate) fn new(
        data_source: PackageDataSource,
        mut far_reader_seeker: FarReaderSeeker,
        blobs_source: PackageBlobs,
    ) -> Result<Self, PackageInitializationError> {
        // Compute package hash.
        far_reader_seeker.seek(io::SeekFrom::Start(0)).map_err(PackageInitializationError::from)?;
        let mut far_bytes = vec![];
        far_reader_seeker.read_to_end(&mut far_bytes).map_err(PackageInitializationError::from)?;
        let hash = FuchsiaMerkleTree::from_reader(far_bytes.as_slice())
            .map_err(PackageInitializationError::from)?
            .root();
        far_reader_seeker.seek(io::SeekFrom::Start(0)).map_err(PackageInitializationError::from)?;

        // Use `FarReader` to construct `MetaPackage` and `MetaContents`.
        let mut far_reader =
            FarReader::new(far_reader_seeker).map_err(PackageInitializationError::from)?;
        let meta_package = FuchsiaMetaPackage::deserialize(
            far_reader
                .read_file(FuchsiaMetaPackage::PATH)
                .map_err(PackageInitializationError::from)?
                .as_slice(),
        )
        .map_err(PackageInitializationError::from)?;
        let meta_contents = FuchsiaMetaContents::deserialize(
            far_reader
                .read_file(FuchsiaMetaContents::PATH)
                .map_err(PackageInitializationError::from)?
                .as_slice(),
        )
        .map_err(PackageInitializationError::from)?;

        // Use `FarReader` to gather all meta blobs.
        let far_paths = far_reader.list().map(|entry| entry.path().to_owned()).collect::<Vec<_>>();
        let meta_blobs = far_paths
            .into_iter()
            .map(|path_string| {
                let contents =
                    far_reader.read_file(&path_string).map_err(PackageInitializationError::from)?;
                let hash = FuchsiaMerkleTree::from_reader(contents.as_slice())
                    .map_err(PackageInitializationError::from)?
                    .root();
                Ok(MetaBlobData { path: PathBuf::from(path_string), hash: hash.into() })
            })
            .collect::<Result<Vec<MetaBlobData<PackageBlobs>>, PackageInitializationError>>()?;

        // Use `MetaContents` to gather all content blobs.
        let content_blobs = meta_contents
            .contents()
            .into_iter()
            .map(|(path_string, hash)| ContentBlobData {
                path: PathBuf::from(path_string),
                hash: hash.clone().into(),
            })
            .collect();

        Ok(Self(Rc::new(PackageData {
            data_source,
            hash: hash.into(),
            meta_package: MetaPackage(meta_package),
            meta_contents: MetaContents(meta_contents),
            meta_blobs,
            content_blobs,
            far_reader: Rc::new(RefCell::new(far_reader)),
            blobs_source,
        })))
    }
}

impl<
        PackageDataSource: 'static + Clone + api::DataSource,
        PackageBlobs: 'static + BlobSet,
        FarReaderSeeker: 'static + io::Read + io::Seek,
    > api::Package for Package<PackageDataSource, PackageBlobs, FarReaderSeeker>
where
    <PackageBlobs as BlobSet>::Error: 'static,
    <<PackageBlobs as BlobSet>::Blob as api::Blob>::Error: 'static,
{
    type Hash = Hash;
    type MetaPackage = MetaPackage;
    type MetaContents = MetaContents;
    type Blob = Blob<PackageDataSource, PackageBlobs, FarReaderSeeker>;
    type PackagePath = PathBuf;
    type Component = Component;

    fn hash(&self) -> Self::Hash {
        self.0.hash.clone()
    }

    fn meta_package(&self) -> Self::MetaPackage {
        self.0.meta_package.clone()
    }

    fn meta_contents(&self) -> Self::MetaContents {
        self.0.meta_contents.clone()
    }

    fn content_blobs(&self) -> Box<dyn Iterator<Item = (Self::PackagePath, Self::Blob)>> {
        Box::new(ContentBlobIterator {
            package: self.clone(),
            blob_iterator: Box::new(self.0.content_blobs.clone().into_iter()),
        })
    }

    fn meta_blobs(&self) -> Box<dyn Iterator<Item = (Self::PackagePath, Self::Blob)>> {
        Box::new(MetaBlobIterator {
            package: self.clone(),
            blob_iterator: Box::new(self.0.meta_blobs.clone().into_iter()),
        })
    }

    fn components(&self) -> Box<dyn Iterator<Item = (Self::PackagePath, Self::Component)>> {
        // TODO(fxbug.dev/111243): Implement component iteration when production component API is
        // available.
        Box::new(iter::empty())
    }
}

// Some fakes rely on package implementations with `Default`. In order to write tests that use
// these fakes, but for other purposes in the test, use the production `Package` implementation,
// a panicking test-only `Default` implementation is provided.
#[cfg(test)]
impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > Default for Package<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    fn default() -> Self {
        panic!("Production `Package` has no default");
    }
}

/// Internal state of a [`Package`] object.
struct PackageData<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
> {
    /// The data source from which the package was loaded.
    data_source: PackageDataSource,

    /// The `Hash` of the package's meta.far file.
    hash: Hash,

    /// The parsed meta/package file loaded from this object's `far_reader`.
    meta_package: MetaPackage,

    /// The parsed meta/contents file loaded from this object's `far_reader`.
    meta_contents: MetaContents,

    /// A vector of meta blobs backed by this object's `far_reader`.
    meta_blobs: Vec<MetaBlobData<PackageBlobs>>,

    /// A vector of content blobs backed by this object's `blobs_source`.
    content_blobs: Vec<ContentBlobData<PackageBlobs>>,

    // TODO: Add subpackages support via `fuchsia_pkg::MetaSubpackages`.
    /// A shared pointer to a reader abstraction over this package's meta.far file.
    far_reader: Rc<RefCell<FarReader<FarReaderSeeker>>>,

    /// The `BlobSet` from which this package's blobs can be loaded.
    blobs_source: PackageBlobs,
}

/// Metadata about a meta blob that can be combined with a `FarReader` to access the meta blob.
struct MetaBlobData<PackageBlobs: BlobSet> {
    path: PathBuf,
    hash: PackageBlobs::Hash,
}

/// Boilerplate implementation of `Clone` that is needed because `MetaBlobData` has a type
/// parameter that may not, itself, be clonable.
impl<PackageBlobs: BlobSet> Clone for MetaBlobData<PackageBlobs> {
    fn clone(&self) -> Self {
        Self { path: self.path.clone(), hash: self.hash.clone() }
    }
}

/// Metadata about a content blob that can be combined with a `BlobSet` to access the content blob.
struct ContentBlobData<PackageBlobs: BlobSet> {
    path: PathBuf,
    hash: PackageBlobs::Hash,
}

/// Boilerplate implementation of `Clone` that is needed because `ContentBlobData` has a type
/// parameter that may not, itself, be clonable.
impl<PackageBlobs: BlobSet> Clone for ContentBlobData<PackageBlobs> {
    fn clone(&self) -> Self {
        Self { path: self.path.clone(), hash: self.hash.clone() }
    }
}

/// Errors that can occur attempting to use a `MetaBlob` as a `Blob`.
#[derive(Debug, Error)]
pub enum MetaBlobError {
    #[error("failed to load meta blob from far: {0}")]
    FarError(#[from] FarError),
    #[error("failed to read path as UTF8 string: {0}")]
    PathError(String),
}

/// `Blob` implementation that combines `MetaBlobData` and a `FarReader`.
pub struct MetaBlob<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
> {
    data: MetaBlobData<PackageBlobs>,
    package: Package<PackageDataSource, PackageBlobs, FarReaderSeeker>,
}

impl<
        PackageDataSource: 'static + Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > api::Blob for MetaBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    type Hash = PackageBlobs::Hash;
    type ReaderSeeker = io::Cursor<Vec<u8>>;
    type DataSource = PackageDataSource;
    type Error = MetaBlobError;

    fn hash(&self) -> Self::Hash {
        self.data.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        let mut far_reader = self.package.0.far_reader.borrow_mut();
        let contents = far_reader
            .read_file(&self.data.path.to_str().ok_or_else(|| {
                MetaBlobError::PathError(self.data.path.to_string_lossy().to_string())
            })?)
            .map_err(MetaBlobError::from)?;
        Ok(io::Cursor::new(contents))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([self.package.0.data_source.clone()].into_iter())
    }
}

/// Errors that can occur attempting to use a `ContentBlob` as a `Blob`.
#[derive(Debug, Error)]
pub enum ContentBlobError<BlobSourceError: api::Error, BlobError: api::Error> {
    #[error("failed to lookup package content blob: {0:?}")]
    BlobSource(BlobSourceError),
    #[error("failed to open package content blob: {0:?}")]
    Blob(BlobError),
}

/// `Blob` implementation that combines `ContentBlobData` and a `BlobSet`.
pub struct ContentBlob<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
> {
    data: ContentBlobData<PackageBlobs>,
    package: Package<PackageDataSource, PackageBlobs, FarReaderSeeker>,
}

impl<
        PackageDataSource: 'static + Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > api::Blob for ContentBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    type Hash = PackageBlobs::Hash;
    type ReaderSeeker = <PackageBlobs::Blob as api::Blob>::ReaderSeeker;
    type DataSource = PackageDataSource;
    type Error = ContentBlobError<
        <PackageBlobs as BlobSet>::Error,
        <<PackageBlobs as BlobSet>::Blob as api::Blob>::Error,
    >;

    fn hash(&self) -> Self::Hash {
        self.data.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        self.package
            .0
            .blobs_source
            .blob(self.hash().clone())
            .map_err(ContentBlobError::BlobSource)?
            .reader_seeker()
            .map_err(ContentBlobError::Blob)
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        Box::new([self.package.0.data_source.clone()].into_iter())
    }
}

/// Iterator implementation for iterating over meta blobs in a package.
struct MetaBlobIterator<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
> {
    /// Package containing meta blobs.
    package: Package<PackageDataSource, PackageBlobs, FarReaderSeeker>,

    /// Iterator over metadata describing each meta blob.
    blob_iterator: Box<dyn Iterator<Item = MetaBlobData<PackageBlobs>>>,
}

impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > Iterator for MetaBlobIterator<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    type Item = (PathBuf, Blob<PackageDataSource, PackageBlobs, FarReaderSeeker>);

    fn next(&mut self) -> Option<Self::Item> {
        self.blob_iterator.next().map(|MetaBlobData { path, hash }| {
            (
                path.clone(),
                MetaBlob { data: MetaBlobData { path, hash }, package: self.package.clone() }
                    .into(),
            )
        })
    }
}

/// Iterator implementation for iterating over content blobs in a package.
struct ContentBlobIterator<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
> {
    /// Package containing content blobs.
    package: Package<PackageDataSource, PackageBlobs, FarReaderSeeker>,

    /// Iterator over metadata describing each content blob.
    blob_iterator: Box<dyn Iterator<Item = ContentBlobData<PackageBlobs>>>,
}

impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > Iterator for ContentBlobIterator<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    type Item = (PathBuf, Blob<PackageDataSource, PackageBlobs, FarReaderSeeker>);

    fn next(&mut self) -> Option<Self::Item> {
        self.blob_iterator.next().map(|ContentBlobData { path, hash }| {
            (
                path.clone(),
                ContentBlob { data: ContentBlobData { path, hash }, package: self.package.clone() }
                    .into(),
            )
        })
    }
}

/// Errors that can occur attempting to use a `Blob` as a `crate::api::Blob`.
#[derive(Debug, Error)]
pub enum BlobError<
    BlobSourceErrorForContentBlobError: api::Error,
    BlobErrorForContentBlobError: api::Error,
> {
    #[error("error accessing meta blob: {0}")]
    MetaBlobError(#[from] MetaBlobError),
    #[error("error accessing content blob: {0}")]
    ContentBlobError(
        #[from] ContentBlobError<BlobSourceErrorForContentBlobError, BlobErrorForContentBlobError>,
    ),
}

/// `std::io::Read + std::io::Seek` implementation over either a content blob or a meta blob
/// loaded from a [`Package`].
pub enum BlobReaderSeeker<PackageBlobs: BlobSet> {
    ///`std::io::Read + std::io::Seek` for a content blob loaded from a [`Package`].
    ContentReaderSeeker(<PackageBlobs::Blob as api::Blob>::ReaderSeeker),

    ///`std::io::Read + std::io::Seek` for a meta blob loaded from a [`Package`].
    MetaReaderSeeker(io::Cursor<Vec<u8>>),
}

impl<PackageBlobs: BlobSet> BlobReaderSeeker<PackageBlobs> {
    fn from_content(
        content_reader_seeker: <PackageBlobs::Blob as api::Blob>::ReaderSeeker,
    ) -> Self {
        Self::ContentReaderSeeker(content_reader_seeker)
    }

    fn from_meta(meta_reader_seeker: io::Cursor<Vec<u8>>) -> Self {
        Self::MetaReaderSeeker(meta_reader_seeker)
    }
}

impl<PackageBlobs: BlobSet> io::Read for BlobReaderSeeker<PackageBlobs> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::ContentReaderSeeker(content_reader_seeker) => content_reader_seeker.read(buf),
            Self::MetaReaderSeeker(meta_reader_seeker) => meta_reader_seeker.read(buf),
        }
    }
}

impl<PackageBlobs: BlobSet> io::Seek for BlobReaderSeeker<PackageBlobs> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        match self {
            Self::ContentReaderSeeker(content_reader_seeker) => content_reader_seeker.seek(pos),
            Self::MetaReaderSeeker(meta_reader_seeker) => meta_reader_seeker.seek(pos),
        }
    }
}

/// `crate::api::Blob` implementation for either a content blob or a meta blob loaded from a
/// [`Package`].
pub enum Blob<
    PackageDataSource: Clone + api::DataSource,
    PackageBlobs: BlobSet,
    FarReaderSeeker: io::Read + io::Seek,
> {
    /// Delegate blob implementation for a content blob loaded from a [`Package`].
    ContentBlob(ContentBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>),

    /// Delegate blob implementation for a meta blob loaded from a [`Package`].
    MetaBlob(MetaBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>),
}

impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > From<ContentBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>>
    for Blob<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    fn from(content_blob: ContentBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>) -> Self {
        Self::ContentBlob(content_blob)
    }
}

impl<
        PackageDataSource: Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > From<MetaBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>>
    for Blob<PackageDataSource, PackageBlobs, FarReaderSeeker>
{
    fn from(meta_blob: MetaBlob<PackageDataSource, PackageBlobs, FarReaderSeeker>) -> Self {
        Self::MetaBlob(meta_blob)
    }
}

impl<
        PackageDataSource: 'static + Clone + api::DataSource,
        PackageBlobs: BlobSet,
        FarReaderSeeker: io::Read + io::Seek,
    > api::Blob for Blob<PackageDataSource, PackageBlobs, FarReaderSeeker>
where
    <PackageBlobs as BlobSet>::Error: 'static,
    <<PackageBlobs as BlobSet>::Blob as api::Blob>::Error: 'static,
{
    type Hash = PackageBlobs::Hash;
    type ReaderSeeker = BlobReaderSeeker<PackageBlobs>;
    type DataSource = PackageDataSource;
    type Error = BlobError<
        <PackageBlobs as BlobSet>::Error,
        <<PackageBlobs as BlobSet>::Blob as api::Blob>::Error,
    >;

    fn hash(&self) -> Self::Hash {
        match self {
            Blob::MetaBlob(meta_blob) => meta_blob.hash(),
            Blob::ContentBlob(content_blob) => content_blob.hash(),
        }
    }

    fn reader_seeker(&self) -> Result<Self::ReaderSeeker, Self::Error> {
        Ok(match self {
            Blob::MetaBlob(meta_blob) => {
                BlobReaderSeeker::from_meta(meta_blob.reader_seeker().map_err(BlobError::from)?)
            }
            Blob::ContentBlob(content_blob) => BlobReaderSeeker::from_content(
                content_blob.reader_seeker().map_err(BlobError::from)?,
            ),
        })
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Self::DataSource>> {
        match self {
            Blob::MetaBlob(meta_blob) => meta_blob.data_sources(),
            Blob::ContentBlob(content_blob) => content_blob.data_sources(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Blob;
    use super::BlobError;
    use super::ContentBlobError;
    use super::MetaContents;
    use super::MetaPackage;
    use super::Package;
    use super::PackageInitializationError;
    use crate::api::Blob as _;
    use crate::api::Package as _;
    use crate::blob::fake::Blob as FakeBlob;
    use crate::blob::fake::BlobSet;
    use crate::blob::fake::BlobSetError;
    use crate::blob::test::assert_blobs_eq;
    use crate::data_source::fake::DataSource;
    use crate::hash::Hash;
    use fuchsia_merkle::MerkleTree as FuchsiMerkleTree;
    use fuchsia_pkg::MetaContents as FuchsiaMetaContents;
    use fuchsia_pkg::MetaPackage as FuchsiaMetaPackage;
    use fuchsia_pkg::PackageName;
    use maplit::btreemap;
    use maplit::hashmap;
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::io::Read;
    use std::path::PathBuf;
    use std::str::FromStr;

    #[fuchsia::test]
    fn simple_package() {
        // Package contains an extra meta.far entry other than meta/package and meta/contents.
        let meta_blob_content_str = "Metadata";
        let meta_blob_path_str = "meta/data";
        let meta_blob_fuchsia_hash =
            FuchsiMerkleTree::from_reader(meta_blob_content_str.as_bytes()).unwrap().root();

        // Package contains one content blob designated in meta/contents.
        let content_blob_content_str = "Hello, World!";
        let content_blob_path_str = "data";
        let content_blob_fuchsia_hash =
            FuchsiMerkleTree::from_reader(content_blob_content_str.as_bytes()).unwrap().root();

        // Define meta/package.
        let meta_package =
            FuchsiaMetaPackage::from_name(PackageName::from_str("frobinator").unwrap());
        let mut meta_package_bytes = vec![];
        meta_package.serialize(&mut meta_package_bytes).unwrap();
        let meta_package_fuchsia_hash =
            FuchsiMerkleTree::from_reader(meta_package_bytes.as_slice()).unwrap().root();

        // Define meta/contents with above-mentioned content blob.
        let meta_contents = FuchsiaMetaContents::from_map(hashmap! {
            content_blob_path_str.to_string() => content_blob_fuchsia_hash,
        })
        .unwrap();
        let mut meta_contents_bytes = vec![];
        meta_contents.serialize(&mut meta_contents_bytes).unwrap();
        let meta_contents_fuchsia_hash =
            FuchsiMerkleTree::from_reader(meta_contents_bytes.as_slice()).unwrap().root();

        // Include "extra metadata file", meta/package, and meta/contents in package meta.far.
        let far_map = btreemap! {
            FuchsiaMetaPackage::PATH.to_string() => (meta_package_bytes.len() as u64, Box::new(meta_package_bytes.as_slice()) as Box<dyn Read>),
            FuchsiaMetaContents::PATH.to_string() => (meta_contents_bytes.len() as u64, Box::new(meta_contents_bytes.as_slice()) as Box<dyn Read>),
            meta_blob_path_str.to_string() => (meta_blob_content_str.as_bytes().len() as u64, Box::new(meta_blob_content_str.as_bytes()) as Box<dyn Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();
        let far_fuchsia_hash = FuchsiMerkleTree::from_reader(far_bytes.as_slice()).unwrap().root();

        // Include content blob designated in meta/contents in blob set backing package.
        let blobs_map = hashmap! {
            content_blob_fuchsia_hash.clone().into() => Ok(FakeBlob::<Hash>::new(content_blob_fuchsia_hash.clone().into(), content_blob_content_str.as_bytes().to_owned())),
        };
        let blob_set = BlobSet::<Hash>::from_hash_map(blobs_map);

        // Describe full set of meta blobs used in assertions.
        let mut meta_blobs: HashMap<PathBuf, FakeBlob<Hash>> = hashmap! {
            FuchsiaMetaPackage::PATH.into() => FakeBlob::<Hash>::new(meta_package_fuchsia_hash.into(), meta_package_bytes),
            FuchsiaMetaContents::PATH.into() => FakeBlob::<Hash>::new(meta_contents_fuchsia_hash.into(), meta_contents_bytes),
            meta_blob_path_str.into() => FakeBlob::<Hash>::new(meta_blob_fuchsia_hash.into(), meta_blob_content_str.as_bytes().to_owned()),
        };

        // Describe full set of content blobs used for assertions.
        let mut content_blobs: HashMap<PathBuf, FakeBlob<Hash>> = hashmap! {
            content_blob_path_str.into() => FakeBlob::<Hash>::new(content_blob_fuchsia_hash.into(), content_blob_content_str.as_bytes().to_owned()),
        };

        // Construct package.
        let package = Package::new(DataSource, Cursor::new(far_bytes), blob_set).unwrap();

        // Check high-level package data.
        assert_eq!(Hash::from(far_fuchsia_hash), package.hash());
        assert_eq!(MetaPackage::from(meta_package), package.meta_package());
        assert_eq!(MetaContents::from(meta_contents), package.meta_contents());

        // Check that package contains exactly expected meta blobs.
        for (path, blob) in package.meta_blobs() {
            let b = meta_blobs.remove(&path).unwrap();
            assert_blobs_eq!(b, FakeBlob<Hash>, blob, Blob<DataSource, BlobSet<Hash>, Cursor<Vec<u8>>>);
        }
        assert_eq!(0, meta_blobs.len());

        // Check that package contains exactly expected content blobs.
        for (path, blob) in package.content_blobs() {
            let b = content_blobs.remove(&path).unwrap();
            assert_blobs_eq!(b, FakeBlob<Hash>, blob, Blob<DataSource, BlobSet<Hash>, Cursor<Vec<u8>>>);
        }
        assert_eq!(0, content_blobs.len());
    }

    #[fuchsia::test]
    fn bad_far() {
        match Package::new(
            DataSource,
            // meta.far is empty file.
            Cursor::new(vec![]),
            BlobSet::<Hash>::from_hash_map(hashmap! {}),
        ) {
            Err(PackageInitializationError::FarError(_)) => {}
            Ok(_) => {
                assert!(
                    false,
                    "Expected PackageInitializationError, but package initialization succeeded"
                );
            }
            Err(err) => {
                assert!(false, "Expected PackageInitializationError::FarError, but got: {:?}", err);
            }
        };
    }

    #[fuchsia::test]
    fn missing_meta_package() {
        // Define meta/contents with no blobs.
        let meta_contents = FuchsiaMetaContents::from_map(hashmap! {}).unwrap();
        let mut meta_contents_bytes = vec![];
        meta_contents.serialize(&mut meta_contents_bytes).unwrap();

        // Include meta/contents in package meta.far.
        let far_map = btreemap! {
            FuchsiaMetaContents::PATH.to_string() => (meta_contents_bytes.len() as u64, Box::new(meta_contents_bytes.as_slice()) as Box<dyn Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();

        // Use empty blob set.
        let blob_set = BlobSet::<Hash>::from_hash_map(hashmap! {});

        // Attempt to construct package; expect FarError due to missing meta/package.
        match Package::new(DataSource, Cursor::new(far_bytes), blob_set) {
            Err(PackageInitializationError::FarError(_)) => {}
            Ok(_) => {
                assert!(
                    false,
                    "Expected PackageInitializationError, but package initialization succeeded"
                );
            }
            Err(err) => {
                assert!(false, "Expected PackageInitializationError::FarError, but got: {:?}", err);
            }
        };
    }

    #[fuchsia::test]
    fn missing_meta_contents() {
        // Define meta/package.
        let meta_package =
            FuchsiaMetaPackage::from_name(PackageName::from_str("frobinator").unwrap());
        let mut meta_package_bytes = vec![];
        meta_package.serialize(&mut meta_package_bytes).unwrap();

        // Include meta/package in package meta.far.
        let far_map = btreemap! {
            FuchsiaMetaPackage::PATH.to_string() => (meta_package_bytes.len() as u64, Box::new(meta_package_bytes.as_slice()) as Box<dyn Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();

        // Use empty blob set.
        let blob_set = BlobSet::<Hash>::from_hash_map(hashmap! {});

        // Attempt to construct package; expect FarError due to missing meta/contents.
        match Package::new(DataSource, Cursor::new(far_bytes), blob_set) {
            Err(PackageInitializationError::FarError(_)) => {}
            Ok(_) => {
                assert!(
                    false,
                    "Expected PackageInitializationError, but package initialization succeeded"
                );
            }
            Err(err) => {
                assert!(false, "Expected PackageInitializationError::FarError, but got: {:?}", err);
            }
        };
    }

    #[fuchsia::test]
    fn missing_content_blob() {
        // Package contains an extra meta.far entry other than meta/package and meta/contents.
        let meta_blob_content_str = "Metadata";
        let meta_blob_path_str = "meta/data";

        // Package contains one content blob designated in meta/contents.
        let content_blob_content_str = "Hello, World!";
        let content_blob_path_str = "data";
        let content_blob_fuchsia_hash =
            FuchsiMerkleTree::from_reader(content_blob_content_str.as_bytes()).unwrap().root();

        // Define meta/package.
        let meta_package =
            FuchsiaMetaPackage::from_name(PackageName::from_str("frobinator").unwrap());
        let mut meta_package_bytes = vec![];
        meta_package.serialize(&mut meta_package_bytes).unwrap();

        // Define meta/contents with above-mentioned content blob.
        let meta_contents = FuchsiaMetaContents::from_map(hashmap! {
            content_blob_path_str.to_string() => content_blob_fuchsia_hash,
        })
        .unwrap();
        let mut meta_contents_bytes = vec![];
        meta_contents.serialize(&mut meta_contents_bytes).unwrap();

        // Include "extra metadata file", meta/package, and meta/contents in package meta.far.
        let far_map = btreemap! {
            FuchsiaMetaPackage::PATH.to_string() => (meta_package_bytes.len() as u64, Box::new(meta_package_bytes.as_slice()) as Box<dyn Read>),
            FuchsiaMetaContents::PATH.to_string() => (meta_contents_bytes.len() as u64, Box::new(meta_contents_bytes.as_slice()) as Box<dyn Read>),
            meta_blob_path_str.to_string() => (meta_blob_content_str.as_bytes().len() as u64, Box::new(meta_blob_content_str.as_bytes()) as Box<dyn Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();

        // Use empty blob set: Lookup of content blob will fail.
        let blob_set = BlobSet::<Hash>::from_hash_map(hashmap! {});

        // Construct package.
        let package = Package::new(DataSource, Cursor::new(far_bytes), blob_set).unwrap();

        // Attempt to open content blob. This should fail because it is missing from the `BlobSet`.
        let (path, content_blob) = package.content_blobs().next().unwrap();
        assert_eq!(PathBuf::from(content_blob_path_str), path);
        match content_blob.reader_seeker() {
            Err(BlobError::ContentBlobError(ContentBlobError::BlobSource(
                BlobSetError::BlobNotFound,
            ))) => {}
            Ok(_) => {
                assert!(false, "Expected BlobNotFound error, but got valid reader/seeker");
            }
            Err(err) => {
                assert!(false, "Expected BlobNotFound error, but got: {:?}", err);
            }
        }
    }
}

#[cfg(test)]
pub mod fake {
    use crate::api;
    use crate::blob::fake::Blob;
    use crate::component::fake::Component;
    use crate::hash::fake::Hash;
    use std::iter;

    #[derive(Default)]
    pub(crate) struct Package;

    impl api::Package for Package {
        type Hash = Hash;
        type MetaPackage = MetaPackage;
        type MetaContents = MetaContents;
        type Blob = Blob<Hash>;
        type PackagePath = &'static str;
        type Component = Component;

        fn hash(&self) -> Self::Hash {
            Hash::default()
        }

        fn meta_package(&self) -> Self::MetaPackage {
            MetaPackage::default()
        }

        fn meta_contents(&self) -> Self::MetaContents {
            MetaContents::default()
        }

        fn content_blobs(&self) -> Box<dyn Iterator<Item = (Self::PackagePath, Self::Blob)>> {
            Box::new(iter::empty())
        }

        fn meta_blobs(&self) -> Box<dyn Iterator<Item = (Self::PackagePath, Self::Blob)>> {
            Box::new(iter::empty())
        }

        fn components(&self) -> Box<dyn Iterator<Item = (Self::PackagePath, Self::Component)>> {
            Box::new(iter::empty())
        }
    }

    /// TODO(fxbug.dev/111249): Implement for production package API.
    #[derive(Default)]
    pub(crate) struct MetaPackage;

    impl api::MetaPackage for MetaPackage {}

    /// TODO(fxbug.dev/111249): Implement for production package API.
    #[derive(Default)]
    pub(crate) struct MetaContents;

    impl api::MetaContents for MetaContents {
        type Hash = Hash;
        type EntryPath = &'static str;

        fn contents(&self) -> Box<dyn Iterator<Item = (Self::EntryPath, Self::Hash)>> {
            Box::new(iter::empty())
        }
    }
}
