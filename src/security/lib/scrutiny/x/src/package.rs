// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxbug.dev/111242): Exercise production package implementations to eliminate dead code
// warnings.
#![allow(dead_code)]

use super::api;
use super::blob::BlobOpenError;
use super::blob::BlobSet;
use super::data_source as ds;
use super::hash::Hash;
use super::DataSource;
use fuchsia_archive::Error as FarError;
use fuchsia_archive::Utf8Reader as FarReader;
use fuchsia_merkle::MerkleTree as FuchsiaMerkleTree;
use fuchsia_pkg::MetaContents as FuchsiaMetaContents;
use fuchsia_pkg::MetaContentsError as FuchsiaMetaContentsError;
use fuchsia_pkg::MetaPackage as FuchsiaMetaPackage;
use fuchsia_pkg::MetaPackageError as FuchsiaMetaPackageError;
use std::cell::RefCell;
use std::fmt::Debug;
use std::io;
use std::rc::Rc;
use thiserror::Error;

/// Simple wrapper around `fuchsia_pkg::MetaPackage` that implements `MetaPackage` trait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetaPackage(FuchsiaMetaPackage);

impl api::MetaPackage for MetaPackage {
    fn name(&self) -> &fuchsia_url::PackageName {
        self.0.name()
    }

    fn variant(&self) -> &fuchsia_url::PackageVariant {
        self.0.variant()
    }
}

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
    fn contents(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Hash>)>> {
        Box::new(
            self.0
                .contents()
                .clone()
                .into_iter()
                .map(|(path, hash)| {
                    let path: Box<dyn api::Path> = Box::new(path);
                    let hash: Hash = hash.into();
                    let hash: Box<dyn api::Hash> = Box::new(hash);
                    (path, hash)
                })
                .into_iter(),
        )
    }
}

/// Errors that can occur initializing a [`Package`] via `Package::new`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("error parsing meta/contents in meta.far: {0}")]
    MetaContentsError(#[from] FuchsiaMetaContentsError),
    #[error("error parsing meta/package in meta.far: {0}")]
    MetaPackageError(#[from] FuchsiaMetaPackageError),
    #[error("error reading meta.far for package: {0}")]
    Far(#[from] FarError),
    #[error("error performing i/o operations on far reader for package: {0}")]
    Io(#[from] io::Error),
    #[error("failed to open package meta.far: {0}")]
    MissingMeta(#[from] api::BlobError),
    #[error("failed to locate package content blob: {0}")]
    MissingContent(#[from] BlobOpenError),
}

#[derive(Clone)]
pub(crate) struct Package(Rc<PackageData>);

impl Package {
    /// Constructs a package based on:
    /// - `parent_data_source`: The most specific data source where the package is being loaded
    ///   from. For example, if a package is listed in the system update package, and is loaded
    ///   from the bootfs section of the system zbi, then the ancestor chain of `parent_data_source`
    ///   should look something like `bootfs -> zbi -> update package -> product bundle -> ...`.
    /// - `meta_far`: A [`api::Blob`] object that refers to the package `meta.far` file.
    /// - `content_blob_set`: A [`BlobSet`] object that contains, at least, all the blobs listed in
    ///   the package's `"meta/contents"` file.
    pub fn new(
        mut parent_data_source: Option<ds::DataSource>,
        // TODO: Consider adding URL to data sources to retain package URLs. For now, this parameter
        // is retained because all package-instantiating contexts should have a notion of the URL
        // from which the package is referenced.
        _url: api::PackageResolverUrl,
        meta_far: Box<dyn api::Blob>,
        content_blob_set: Box<dyn BlobSet>,
    ) -> Result<Self, Error> {
        // Prepare data source.
        let meta_far_hash = meta_far.hash();
        let hash = meta_far_hash.clone();

        // TODO: Consider adding URL to data sources to retain package URLs.
        let mut package_data_source = ds::DataSource::new(ds::DataSourceInfo::new(
            api::DataSourceKind::Package,
            Some(Box::new(format!("{}", meta_far_hash))),
            // TODO: Add support for package versioning.
            api::DataSourceVersion::Unknown,
        ));
        if let Some(parent_data_source) = parent_data_source.as_mut() {
            parent_data_source.add_child(package_data_source.clone());
        }

        // Use `FarReader` to construct `MetaPackage` and `MetaContents`.
        let mut far_reader = FarReader::new(meta_far.reader_seeker()?)?;
        let meta_package = FuchsiaMetaPackage::deserialize(
            far_reader.read_file(FuchsiaMetaPackage::PATH)?.as_slice(),
        )
        .map_err(Error::from)?;
        let meta_contents = FuchsiaMetaContents::deserialize(
            far_reader.read_file(FuchsiaMetaContents::PATH)?.as_slice(),
        )
        .map_err(Error::from)?;

        // Use `FarReader` to gather all meta blobs.
        let far_paths = far_reader.list().map(|entry| entry.path().to_owned()).collect::<Vec<_>>();
        let meta_blobs = far_paths
            .into_iter()
            .map(|path_string| {
                let contents = far_reader.read_file(&path_string)?;
                let hash: Hash = FuchsiaMerkleTree::from_reader(contents.as_slice())
                    .map_err(Error::from)?
                    .root()
                    .into();
                Ok(BlobData {
                    data_sources: vec![Box::new(package_data_source.clone())],
                    path: Box::new(path_string),
                    hash: Box::new(hash),
                })
            })
            .collect::<Result<Vec<BlobData>, Error>>()?;

        // Use `MetaContents` to gather all content blobs.
        let content_blobs = meta_contents
            .contents()
            .clone()
            .into_iter()
            .map(|(path_string, hash)| {
                let hash: Box<dyn api::Hash> = Box::new(Hash::from(hash.clone()));
                let blob = content_blob_set.blob(hash.clone())?;
                // Prepare data sources in such a way that:
                // 1. Each data source associated with one or more content blobs appears exactly
                //    once as a child of `package_data_source`;
                // 2. Each data source associated with `blob` is correctly stored in the returned
                //    `BlobData`.
                let data_sources = blob
                    .data_sources()
                    .map(|content_data_source| {
                        // Construct a new data source info is a candidate to add as a child of
                        // `package_data_source` in the case that such a child does not already
                        // exist.
                        let content_data_source_info =
                            ds::DataSourceInfo::new_detached(&content_data_source);
                        // Either:
                        match package_data_source.children().find(|child| {
                            // Note: Compare only `DataSourceInfo` (not data source tree
                            // structures).
                            ds::DataSourceInfo::new_detached(child) == content_data_source_info
                        }) {
                            // This data source info was already added as a child of
                            // `package_data_source`. Store the existing child in `data_sources`.
                            Some(existing_child) => existing_child,
                            // This is the first time constructing this data source info as a child
                            // of `package_data_source`. Add it as a child first, then store it in
                            // `data_sources`.
                            None => {
                                let content_data_source =
                                    ds::DataSource::new(content_data_source_info);
                                package_data_source.add_child(content_data_source.clone());
                                Box::new(content_data_source)
                            }
                        }
                    })
                    .collect::<Vec<Box<dyn api::DataSource>>>();
                Ok(BlobData { data_sources, path: Box::new(path_string), hash })
            })
            .collect::<Result<Vec<BlobData>, Error>>()?;

        Ok(Self(Rc::new(PackageData {
            data_source: Box::new(package_data_source),
            hash,
            meta_package: MetaPackage(meta_package),
            meta_contents: MetaContents(meta_contents),
            meta_blobs,
            content_blobs,
            far_reader: Rc::new(RefCell::new(far_reader)),
            content_blob_set,
        })))
    }
}

impl api::Package for Package {
    fn hash(&self) -> Box<dyn api::Hash> {
        self.0.hash.clone()
    }

    fn meta_package(&self) -> Box<dyn api::MetaPackage> {
        Box::new(self.0.meta_package.clone())
    }

    fn meta_contents(&self) -> Box<dyn api::MetaContents> {
        Box::new(self.0.meta_contents.clone())
    }

    fn content_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>> {
        let content_blob_set = self.0.content_blob_set.clone();
        Box::new(self.0.content_blobs.clone().into_iter().map(move |data| {
            let path = data.path.clone();
            let content_blob: Box<dyn api::Blob> =
                Box::new(ContentBlob { data, content_blob_set: content_blob_set.clone() });
            (path, content_blob)
        }))
    }

    /// Constructs iterator over files in the package's "meta.far" file. This includes, but is not
    /// limited to "meta/package" and "meta/contents" which have their own structured
    /// APIs.
    fn meta_blobs(&self) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Blob>)>> {
        let far_reader = self.0.far_reader.clone();
        Box::new(self.0.meta_blobs.clone().into_iter().map(move |data| {
            let path = data.path.clone();
            let meta_blob: Box<dyn api::Blob> =
                Box::new(MetaBlob { data, far_reader: far_reader.clone() });
            (path, meta_blob)
        }))
    }

    fn components(
        &self,
    ) -> Box<dyn Iterator<Item = (Box<dyn api::Path>, Box<dyn api::Component>)>> {
        todo!("TODO(fxbug.dev/111245): Implement component-finding over meta and content blobs");
    }
}

/// Internal state of a [`Package`] object.
struct PackageData {
    /// The data source from which the package was loaded.
    data_source: Box<dyn api::DataSource>,

    /// The `Hash` of the package's meta.far file.
    hash: Box<dyn api::Hash>,

    /// The parsed meta/package file loaded from this object's `far_reader`.
    meta_package: MetaPackage,

    /// The parsed meta/contents file loaded from this object's `far_reader`.
    meta_contents: MetaContents,

    /// A vector of meta blobs backed by this object's `far_reader`.
    meta_blobs: Vec<BlobData>,

    /// A vector of content blobs backed by this object's `blobs_source`.
    content_blobs: Vec<BlobData>,

    // TODO: Add subpackages support via `fuchsia_pkg::MetaSubpackages`.
    /// A shared pointer to a reader abstraction over this package's meta.far file.
    far_reader: Rc<RefCell<FarReader<Box<dyn api::ReaderSeeker>>>>,

    /// The `BlobSet` from which this package's blobs can be loaded.
    content_blob_set: Box<dyn BlobSet>,
}

#[derive(Clone, Debug)]
struct BlobData {
    data_sources: Vec<Box<dyn api::DataSource>>,
    path: Box<dyn api::Path>,
    hash: Box<dyn api::Hash>,
}

struct MetaBlob {
    data: BlobData,
    far_reader: Rc<RefCell<FarReader<Box<dyn api::ReaderSeeker>>>>,
}

impl api::Blob for MetaBlob {
    fn hash(&self) -> Box<dyn api::Hash> {
        self.data.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Box<dyn api::ReaderSeeker>, api::BlobError> {
        let mut far_reader = self.far_reader.borrow_mut();
        let path = self.data.path.as_ref().as_ref();
        let contents = far_reader
            .read_file(path.to_str().ok_or_else(|| api::BlobError::Path {
                path: path.to_string_lossy().to_string(),
                data_sources: self.data.data_sources.clone(),
            })?)
            .map_err(|error| api::BlobError::Far {
                meta_file_path: self.data.path.clone(),
                meta_file_hash: self.data.hash.clone(),
                data_sources: self.data.data_sources.clone(),
                error,
            })?;
        Ok(Box::new(io::Cursor::new(contents)))
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        Box::new(self.data.data_sources.clone().into_iter())
    }
}

struct ContentBlob {
    data: BlobData,
    content_blob_set: Box<dyn BlobSet>,
}

impl api::Blob for ContentBlob {
    fn hash(&self) -> Box<dyn api::Hash> {
        self.data.hash.clone()
    }

    fn reader_seeker(&self) -> Result<Box<dyn api::ReaderSeeker>, api::BlobError> {
        self.content_blob_set
            .blob(self.data.hash.clone())
            .map_err(|error| api::BlobError::Open { error })?
            .reader_seeker()
    }

    fn data_sources(&self) -> Box<dyn Iterator<Item = Box<dyn api::DataSource>>> {
        Box::new(self.data.data_sources.clone().into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::super::api;
    use super::super::api::Package as _;
    use super::super::blob::test::VerifiedMemoryBlobSet;
    use super::super::blob::BlobOpenError;
    use super::super::blob::BlobSet as _;
    use super::super::blob::UnverifiedMemoryBlob;
    use super::super::data_source as ds;
    use super::super::hash::Hash;
    use super::Error;
    use super::MetaContents;
    use super::MetaPackage;
    use super::Package;
    use fuchsia_merkle::MerkleTree as FuchsiaMerkleTree;
    use fuchsia_pkg::MetaContents as FuchsiaMetaContents;
    use fuchsia_pkg::MetaPackage as FuchsiaMetaPackage;
    use fuchsia_pkg::PackageName;
    use maplit::btreemap;
    use maplit::hashmap;
    use std::collections::HashMap;
    use std::io;
    use std::iter;
    use std::str::FromStr as _;

    #[fuchsia::test]
    fn simple_package() {
        // Package contains an extra meta.far entry other than meta/package and meta/contents.
        let meta_blob_content_str = "Metadata";
        let meta_blob_path_str = "meta/data";

        // Package contains one content blob designated in meta/contents.
        let content_blob_content_str = "Hello, World!";
        let content_blob_path_str = "data";
        let content_blob_fuchsia_hash =
            FuchsiaMerkleTree::from_reader(content_blob_content_str.as_bytes()).unwrap().root();

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
            FuchsiaMetaPackage::PATH.to_string() => (meta_package_bytes.len() as u64, Box::new(meta_package_bytes.as_slice()) as Box<dyn io::Read>),
            FuchsiaMetaContents::PATH.to_string() => (meta_contents_bytes.len() as u64, Box::new(meta_contents_bytes.as_slice()) as Box<dyn io::Read>),
            meta_blob_path_str.to_string() => (meta_blob_content_str.as_bytes().len() as u64, Box::new(meta_blob_content_str.as_bytes()) as Box<dyn io::Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();
        let far_fuchsia_hash = FuchsiaMerkleTree::from_reader(far_bytes.as_slice()).unwrap().root();
        let far_fuchsia_hash_string = format!("{}", far_fuchsia_hash);

        // Construct blob set with content blob and package meta.far.
        let blob_set_data_source_info = ds::DataSourceInfo::new(
            api::DataSourceKind::BlobDirectory,
            Some(Box::new("/fake/blob/directory")),
            api::DataSourceVersion::Unknown,
        );
        let blob_set_data_source = ds::DataSource::new(blob_set_data_source_info.clone());
        let blob_set: VerifiedMemoryBlobSet = VerifiedMemoryBlobSet::new(
            [Box::new(blob_set_data_source.clone()) as Box<dyn api::DataSource>].into_iter(),
            [far_bytes.clone().as_slice(), content_blob_content_str.as_bytes()].into_iter(),
        );

        let meta_far_hash: Box<dyn api::Hash> = Box::new(Hash::from(far_fuchsia_hash));
        let meta_far_blob = blob_set.blob(meta_far_hash.clone()).unwrap();

        // Construct package.
        let package = Package::new(
            Some(blob_set_data_source),
            api::PackageResolverUrl::Url,
            meta_far_blob,
            Box::new(blob_set),
        )
        .unwrap();

        // Check high-level package data.
        assert_eq!(meta_far_hash.as_ref(), package.hash().as_ref());
        let actual_meta_package: Box<dyn api::MetaPackage> =
            Box::new(MetaPackage::from(meta_package));
        assert_eq!(actual_meta_package.as_ref(), package.meta_package().as_ref());
        let actual_meta_contents: Box<dyn api::MetaContents> =
            Box::new(MetaContents::from(meta_contents));
        assert_eq!(actual_meta_contents.as_ref(), package.meta_contents().as_ref());

        // Describe full set of meta blobs used in assertions.
        let mut meta_blobs_contents: HashMap<Box<dyn api::Path>, &[u8]> = hashmap! {
            Box::new(FuchsiaMetaPackage::PATH) as Box<dyn api::Path> =>
                meta_package_bytes.as_slice(),
            Box::new(FuchsiaMetaContents::PATH) => meta_contents_bytes.as_slice(),
            Box::new(meta_blob_path_str) => meta_blob_content_str.as_bytes(),
        };
        // Remove elements from map and check equality of underlying data.
        for (path, blob) in package.meta_blobs() {
            let expected_bytes = meta_blobs_contents.remove(&path).unwrap();
            let mut actual_bytes = vec![];
            blob.reader_seeker().unwrap().read_to_end(&mut actual_bytes).unwrap();
            assert_eq!(expected_bytes, actual_bytes.as_slice());

            let expected_hash: Box<dyn api::Hash> = Box::new(Hash::from(
                FuchsiaMerkleTree::from_reader(expected_bytes).unwrap().root(),
            ));
            let actual_hash = blob.hash();
            assert_eq!(expected_hash.as_ref(), actual_hash.as_ref());

            // One data source.
            let mut actual_data_sources = blob.data_sources();
            let actual_data_source = actual_data_sources.next().unwrap();
            assert_eq!(None, actual_data_sources.next());

            // Data source info refers to package.
            assert_eq!(api::DataSourceKind::Package, actual_data_source.kind());
            assert_eq!(
                Some(Box::new(far_fuchsia_hash_string.clone()) as Box<dyn api::Path>),
                actual_data_source.path()
            );
            assert_eq!(api::DataSourceVersion::Unknown, actual_data_source.version());

            // Parent refers to blob set.
            let actual_data_source_parent = actual_data_source.parent().unwrap();
            assert_eq!(blob_set_data_source_info.kind, actual_data_source_parent.kind());
            assert_eq!(blob_set_data_source_info.path, actual_data_source_parent.path());
            assert_eq!(blob_set_data_source_info.version, actual_data_source_parent.version());
        }
        // All elements should have been removed from map.
        assert_eq!(0, meta_blobs_contents.len());

        // Describe full set of content blobs used for assertions.
        let mut content_blobs_contents: HashMap<Box<dyn api::Path>, &[u8]> = hashmap! {
            Box::new(content_blob_path_str) as Box<dyn api::Path> => content_blob_content_str.as_bytes(),
        };
        // Remove elements from map and check equality of underlying data.
        for (path, blob) in package.content_blobs() {
            let expected_bytes = content_blobs_contents.remove(&path).unwrap();
            let mut actual_bytes = vec![];
            blob.reader_seeker().unwrap().read_to_end(&mut actual_bytes).unwrap();
            assert_eq!(expected_bytes, actual_bytes.as_slice());

            let expected_hash: Box<dyn api::Hash> = Box::new(Hash::from(
                FuchsiaMerkleTree::from_reader(expected_bytes).unwrap().root(),
            ));
            let actual_hash = blob.hash();
            assert_eq!(expected_hash.as_ref(), actual_hash.as_ref());

            // One data source.
            let mut actual_data_sources = blob.data_sources();
            let actual_data_source = actual_data_sources.next().unwrap();
            assert_eq!(None, actual_data_sources.next());

            // Local data source info refers to blob set.
            assert_eq!(blob_set_data_source_info.kind, actual_data_source.kind());
            assert_eq!(blob_set_data_source_info.path, actual_data_source.path());
            assert_eq!(blob_set_data_source_info.version, actual_data_source.version());

            // Parent data source info refers to package.
            let actual_data_source_parent = actual_data_source.parent().unwrap();
            assert_eq!(api::DataSourceKind::Package, actual_data_source_parent.kind());
            assert_eq!(
                Some(Box::new(far_fuchsia_hash_string.clone()) as Box<dyn api::Path>),
                actual_data_source_parent.path()
            );
            assert_eq!(api::DataSourceVersion::Unknown, actual_data_source_parent.version());

            // Grandparent refers to blob set again (because that is where the package came from).
            let actual_data_source_grandparent = actual_data_source_parent.parent().unwrap();
            assert_eq!(blob_set_data_source_info.kind, actual_data_source_grandparent.kind());
            assert_eq!(blob_set_data_source_info.path, actual_data_source_grandparent.path());
            assert_eq!(blob_set_data_source_info.version, actual_data_source_grandparent.version());
        }
        // All elements should have been removed from map.
        assert_eq!(0, content_blobs_contents.len());
    }

    #[fuchsia::test]
    fn bad_far() {
        let bad_far_contents = vec![];
        let bad_far_blob = UnverifiedMemoryBlob::new(
            [].into_iter(),
            Box::new(Hash::from_contents(bad_far_contents.as_slice())),
            bad_far_contents.clone(),
        );
        match Package::new(
            None,
            api::PackageResolverUrl::Url,
            Box::new(bad_far_blob),
            Box::new(VerifiedMemoryBlobSet::new(iter::empty(), iter::empty::<&[u8]>())),
        ) {
            Err(Error::Far(_)) => {}
            Ok(_) => {
                assert!(false, "Expected Error, but package initialization succeeded");
            }
            Err(err) => {
                assert!(false, "Expected Error::Far, but got: {:?}", err);
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
            // Note: No meta/package entry.
            FuchsiaMetaContents::PATH.to_string() => (meta_contents_bytes.len() as u64, Box::new(meta_contents_bytes.as_slice()) as Box<dyn io::Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();
        let far_blob = UnverifiedMemoryBlob::new(
            [].into_iter(),
            Box::new(Hash::from_contents(far_bytes.as_slice())),
            far_bytes.clone(),
        );

        // Use empty blob set.
        let blob_set = VerifiedMemoryBlobSet::new(iter::empty(), iter::empty::<&[u8]>());

        // Attempt to construct package; expect FarError due to missing meta/package.
        match Package::new(
            None,
            api::PackageResolverUrl::Url,
            Box::new(far_blob),
            Box::new(blob_set),
        ) {
            Err(Error::Far(_)) => {}
            Ok(_) => {
                assert!(false, "Expected Error, but package initialization succeeded");
            }
            Err(err) => {
                assert!(false, "Expected Error::Far, but got: {:?}", err);
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
            // Note: No meta/contents entry.
            FuchsiaMetaPackage::PATH.to_string() => (meta_package_bytes.len() as u64, Box::new(meta_package_bytes.as_slice()) as Box<dyn io::Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();
        let far_blob = UnverifiedMemoryBlob::new(
            [].into_iter(),
            Box::new(Hash::from_contents(far_bytes.as_slice())),
            far_bytes.clone(),
        );

        // Use empty blob set.
        let blob_set = VerifiedMemoryBlobSet::new(iter::empty(), iter::empty::<&[u8]>());

        // Attempt to construct package; expect FarError due to missing meta/package.
        match Package::new(
            None,
            api::PackageResolverUrl::Url,
            Box::new(far_blob),
            Box::new(blob_set),
        ) {
            Err(Error::Far(_)) => {}
            Ok(_) => {
                assert!(false, "Expected Error, but package initialization succeeded");
            }
            Err(err) => {
                assert!(false, "Expected Error::Far, but got: {:?}", err);
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
            FuchsiaMerkleTree::from_reader(content_blob_content_str.as_bytes()).unwrap().root();
        let content_blob_hash: Box<dyn api::Hash> =
            Box::new(Hash::from(content_blob_fuchsia_hash.clone()));

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
            FuchsiaMetaPackage::PATH.to_string() => (meta_package_bytes.len() as u64, Box::new(meta_package_bytes.as_slice()) as Box<dyn io::Read>),
            FuchsiaMetaContents::PATH.to_string() => (meta_contents_bytes.len() as u64, Box::new(meta_contents_bytes.as_slice()) as Box<dyn io::Read>),
            meta_blob_path_str.to_string() => (meta_blob_content_str.as_bytes().len() as u64, Box::new(meta_blob_content_str.as_bytes()) as Box<dyn io::Read>),
        };
        let mut far_bytes = vec![];
        fuchsia_archive::write(&mut far_bytes, far_map).unwrap();

        // Use empty blob set: Lookup of content blob will fail.
        let blob_set = VerifiedMemoryBlobSet::new(iter::empty(), iter::empty::<&[u8]>());

        let far_blob = UnverifiedMemoryBlob::new(
            [].into_iter(),
            Box::new(Hash::from_contents(far_bytes.as_slice())),
            far_bytes.clone(),
        );

        // Attempt to construct package. This will construct content blobs that store their metadata
        // such as their data source. Locating the content blob that is missing from `blob_set`
        // should fail.
        match Package::new(
            None,
            api::PackageResolverUrl::Url,
            Box::new(far_blob),
            Box::new(blob_set),
        ) {
            Err(Error::MissingContent(BlobOpenError::BlobNotFound { hash, directory })) => {
                assert_eq!(content_blob_hash.as_ref(), hash.as_ref());
                assert_eq!(None, directory);
            }
            Ok(_) => {
                assert!(false, "Expected BlobNotFound error, but got valid reader/seeker");
            }
            Err(err) => {
                assert!(false, "Expected BlobNotFound error, but got: {:?}", err);
            }
        }
    }
}
