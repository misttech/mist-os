// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::meta_as_dir::MetaAsDir;
use crate::meta_subdir::MetaSubdir;
use crate::non_meta_subdir::NonMetaSubdir;
use crate::{usize_to_u64_safe, Error, NonMetaStorageError};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use fuchsia_pkg::MetaContents;
use log::error;
use std::collections::HashMap;
use std::sync::Arc;
use vfs::common::send_on_open_with_error;
use vfs::directory::entry::{EntryInfo, OpenRequest};
use vfs::directory::immutable::connection::ImmutableConnection;
use vfs::directory::traversal_position::TraversalPosition;
use vfs::execution_scope::ExecutionScope;
use vfs::file::vmo::VmoFile;
use vfs::path::Path as VfsPath;
use vfs::{
    immutable_attributes, CreationMode, ObjectRequestRef, ProtocolsExt as _, ToObjectRequest,
};

/// The root directory of Fuchsia package.
#[derive(Debug)]
pub struct RootDir<S> {
    pub(crate) non_meta_storage: S,
    pub(crate) hash: fuchsia_hash::Hash,
    // The keys are object relative path expressions.
    pub(crate) meta_files: HashMap<String, MetaFileLocation>,
    // The keys are object relative path expressions.
    pub(crate) non_meta_files: HashMap<String, fuchsia_hash::Hash>,
    pub(crate) meta_far_vmo: zx::Vmo,
    dropper: Option<Box<dyn crate::OnRootDirDrop>>,
}

impl<S: crate::NonMetaStorage> RootDir<S> {
    /// Loads the package metadata given by `hash` from `non_meta_storage`, returning an object
    /// representing the package, backed by `non_meta_storage`.
    pub async fn new(non_meta_storage: S, hash: fuchsia_hash::Hash) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self::new_raw(non_meta_storage, hash, None).await?))
    }

    /// Loads the package metadata given by `hash` from `non_meta_storage`, returning an object
    /// representing the package, backed by `non_meta_storage`.
    /// Takes `dropper`, which will be dropped when the returned `RootDir` is dropped.
    pub async fn new_with_dropper(
        non_meta_storage: S,
        hash: fuchsia_hash::Hash,
        dropper: Box<dyn crate::OnRootDirDrop>,
    ) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self::new_raw(non_meta_storage, hash, Some(dropper)).await?))
    }

    /// Loads the package metadata given by `hash` from `non_meta_storage`, returning an object
    /// representing the package, backed by `non_meta_storage`.
    /// Takes `dropper`, which will be dropped when the returned `RootDir` is dropped.
    /// Like `new_with_dropper` except the returned `RootDir` is not in an `Arc`.
    pub async fn new_raw(
        non_meta_storage: S,
        hash: fuchsia_hash::Hash,
        dropper: Option<Box<dyn crate::OnRootDirDrop>>,
    ) -> Result<Self, Error> {
        let meta_far_vmo = non_meta_storage.get_blob_vmo(&hash).await.map_err(|e| {
            if e.is_not_found_error() {
                Error::MissingMetaFar
            } else {
                Error::OpenMetaFar(e)
            }
        })?;
        let (meta_files, non_meta_files) = load_package_metadata(&meta_far_vmo)?;

        Ok(RootDir { non_meta_storage, hash, meta_files, non_meta_files, meta_far_vmo, dropper })
    }

    /// Sets the dropper. If the dropper was already set, returns `dropper` in the error.
    pub fn set_dropper(
        &mut self,
        dropper: Box<dyn crate::OnRootDirDrop>,
    ) -> Result<(), Box<dyn crate::OnRootDirDrop>> {
        match self.dropper {
            Some(_) => Err(dropper),
            None => {
                self.dropper = Some(dropper);
                Ok(())
            }
        }
    }

    /// Returns the contents, if present, of the file at object relative path expression `path`.
    /// https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, ReadFileError> {
        if let Some(hash) = self.non_meta_files.get(path) {
            self.non_meta_storage.read_blob(hash).await.map_err(ReadFileError::ReadBlob)
        } else if let Some(location) = self.meta_files.get(path) {
            self.meta_far_vmo
                .read_to_vec(location.offset, location.length)
                .map_err(ReadFileError::ReadMetaFile)
        } else {
            Err(ReadFileError::NoFileAtPath { path: path.to_string() })
        }
    }

    /// Returns `true` iff there is a file at `path`, an object relative path expression.
    /// https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
    pub fn has_file(&self, path: &str) -> bool {
        self.non_meta_files.contains_key(path) || self.meta_files.contains_key(path)
    }

    /// Returns the hash of the package.
    pub fn hash(&self) -> &fuchsia_hash::Hash {
        &self.hash
    }

    /// Returns an iterator of the hashes of files stored externally to the package meta.far.
    /// May return duplicates.
    pub fn external_file_hashes(&self) -> impl ExactSizeIterator<Item = &fuchsia_hash::Hash> {
        self.non_meta_files.values()
    }

    /// Returns the path of the package as indicated by the "meta/package" file.
    pub async fn path(&self) -> Result<fuchsia_pkg::PackagePath, PathError> {
        Ok(fuchsia_pkg::MetaPackage::deserialize(&self.read_file("meta/package").await?[..])?
            .into_path())
    }

    /// Returns the subpackages of the package.
    pub async fn subpackages(&self) -> Result<fuchsia_pkg::MetaSubpackages, SubpackagesError> {
        let contents = match self.read_file(fuchsia_pkg::MetaSubpackages::PATH).await {
            Ok(contents) => contents,
            Err(ReadFileError::NoFileAtPath { .. }) => {
                return Ok(fuchsia_pkg::MetaSubpackages::default())
            }
            Err(e) => Err(e)?,
        };

        Ok(fuchsia_pkg::MetaSubpackages::deserialize(&*contents)?)
    }

    /// Creates a file that contains the package's hash.
    fn create_meta_as_file(&self) -> Result<Arc<VmoFile>, zx::Status> {
        let file_contents = self.hash.to_string();
        let vmo = zx::Vmo::create(usize_to_u64_safe(file_contents.len()))?;
        let () = vmo.write(file_contents.as_bytes(), 0)?;
        Ok(VmoFile::new_with_inode(
            vmo, /*readable*/ true, /*writable*/ false, /*executable*/ false,
            /*inode*/ 1,
        ))
    }

    /// Creates and returns a meta file if one exists at `path`.
    pub(crate) fn get_meta_file(&self, path: &str) -> Result<Option<Arc<VmoFile>>, zx::Status> {
        // The FAR spec requires 4 KiB alignment of content chunks [1], so offset will
        // always be page-aligned, because pages are required [2] to be a power of 2 and at
        // least 4 KiB.
        // [1] https://fuchsia.dev/fuchsia-src/concepts/source_code/archive_format#content_chunk
        // [2] https://fuchsia.dev/fuchsia-src/reference/syscalls/system_get_page_size
        // TODO(https://fxbug.dev/42162525) Need to manually zero the end of the VMO if
        // zx_system_get_page_size() > 4K.
        assert_eq!(zx::system_get_page_size(), 4096);

        let location = match self.meta_files.get(path) {
            Some(location) => location,
            None => return Ok(None),
        };
        let vmo = self
            .meta_far_vmo
            .create_child(
                zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE | zx::VmoChildOptions::NO_WRITE,
                location.offset,
                location.length,
            )
            .map_err(|e| {
                error!("Error creating child vmo for meta file {:?}", e);
                zx::Status::INTERNAL
            })?;

        Ok(Some(VmoFile::new_with_inode(
            vmo, /*readable*/ true, /*writable*/ false, /*executable*/ false,
            /*inode*/ 1,
        )))
    }

    /// Creates and returns a `MetaSubdir` if one exists at `path`. `path` must end in '/'.
    pub(crate) fn get_meta_subdir(self: &Arc<Self>, path: String) -> Option<Arc<MetaSubdir<S>>> {
        debug_assert!(path.ends_with("/"));
        for k in self.meta_files.keys() {
            if k.starts_with(&path) {
                return Some(MetaSubdir::new(self.clone(), path));
            }
        }
        None
    }

    /// Creates and returns a `NonMetaSubdir` if one exists at `path`. `path` must end in '/'.
    pub(crate) fn get_non_meta_subdir(
        self: &Arc<Self>,
        path: String,
    ) -> Option<Arc<NonMetaSubdir<S>>> {
        debug_assert!(path.ends_with("/"));
        for k in self.non_meta_files.keys() {
            if k.starts_with(&path) {
                return Some(NonMetaSubdir::new(self.clone(), path));
            }
        }
        None
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ReadFileError {
    #[error("reading blob")]
    ReadBlob(#[source] NonMetaStorageError),

    #[error("reading meta file")]
    ReadMetaFile(#[source] zx::Status),

    #[error("no file exists at path: {path:?}")]
    NoFileAtPath { path: String },
}

#[derive(thiserror::Error, Debug)]
pub enum SubpackagesError {
    #[error("reading manifest")]
    Read(#[from] ReadFileError),

    #[error("parsing manifest")]
    Parse(#[from] fuchsia_pkg::MetaSubpackagesError),
}

#[derive(thiserror::Error, Debug)]
pub enum PathError {
    #[error("reading meta/package")]
    Read(#[from] ReadFileError),

    #[error("parsing meta/package")]
    Parse(#[from] fuchsia_pkg::MetaPackageError),
}

impl<S: crate::NonMetaStorage> vfs::directory::entry::DirectoryEntry for RootDir<S> {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_dir(self)
    }
}

impl<S: crate::NonMetaStorage> vfs::directory::entry::GetEntryInfo for RootDir<S> {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

impl<S: crate::NonMetaStorage> vfs::node::Node for RootDir<S> {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: crate::DIRECTORY_ABILITIES,
                id: 1,
            }
        ))
    }
}

impl<S: crate::NonMetaStorage> vfs::directory::entry_container::Directory for RootDir<S> {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: VfsPath,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let flags = flags & !fio::OpenFlags::POSIX_WRITABLE;
        let describe = flags.contains(fio::OpenFlags::DESCRIBE);

        if flags.intersects(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT) {
            let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_SUPPORTED);
            return;
        }

        if path.is_empty() {
            flags.to_object_request(server_end).handle(|object_request| {
                if flags.intersects(
                    fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::TRUNCATE
                        | fio::OpenFlags::APPEND,
                ) {
                    return Err(zx::Status::NOT_SUPPORTED);
                }

                object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
            });
            return;
        }

        // vfs::path::Path::as_str() is an object relative path expression [1], except that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line removes the possible
        // trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let canonical_path = path.as_ref().strip_suffix('/').unwrap_or_else(|| path.as_ref());

        if canonical_path == "meta" {
            // This branch is done here instead of in MetaAsDir so that Clone'ing MetaAsDir yields
            // MetaAsDir. See the MetaAsDir::open impl for more.

            // To remain POSIX compliant, we must default to opening meta as a file unless the
            // DIRECTORY flag (which maps to O_DIRECTORY) is specified. Otherwise, it would be
            // impossible to open as a directory, as there is no POSIX equivalent for NOT_DIRECTORY.
            let open_meta_as_file =
                !flags.intersects(fio::OpenFlags::DIRECTORY | fio::OpenFlags::NODE_REFERENCE);
            if open_meta_as_file {
                flags.to_object_request(server_end).handle(|object_request| {
                    let file = self.create_meta_as_file().map_err(|e| {
                        error!("Error creating the meta file: {:?}", e);
                        zx::Status::INTERNAL
                    })?;
                    vfs::file::serve(file, scope, &flags, object_request)
                });
            } else {
                let () = MetaAsDir::new(self).open(scope, flags, VfsPath::dot(), server_end);
            }
            return;
        }

        if canonical_path.starts_with("meta/") {
            match self.get_meta_file(canonical_path) {
                Ok(Some(meta_file)) => {
                    flags.to_object_request(server_end).handle(|object_request| {
                        vfs::file::serve(meta_file, scope, &flags, object_request)
                    });
                    return;
                }
                Ok(None) => {}
                Err(status) => {
                    let () = send_on_open_with_error(describe, server_end, status);
                    return;
                }
            }

            if let Some(subdir) = self.get_meta_subdir(canonical_path.to_string() + "/") {
                let () = subdir.open(scope, flags, VfsPath::dot(), server_end);
                return;
            }

            let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_FOUND);
            return;
        }

        if let Some(blob) = self.non_meta_files.get(canonical_path) {
            let () =
                self.non_meta_storage.open(blob, flags, scope, server_end).unwrap_or_else(|e| {
                    error!("Error forwarding content blob open to blobfs: {:#}", anyhow::anyhow!(e))
                });
            return;
        }

        if let Some(subdir) = self.get_non_meta_subdir(canonical_path.to_string() + "/") {
            let () = subdir.open(scope, flags, VfsPath::dot(), server_end);
            return;
        }

        let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_FOUND);
    }

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: VfsPath,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if flags.creation_mode() != CreationMode::Never {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        if path.is_empty() {
            if let Some(rights) = flags.rights() {
                if rights.intersects(fio::Operations::WRITE_BYTES) {
                    return Err(zx::Status::NOT_SUPPORTED);
                }
            }

            // `ImmutableConnection::create` checks that only directory flags are specified.
            return object_request.spawn_connection(
                scope,
                self,
                flags,
                ImmutableConnection::create,
            );
        }

        // vfs::path::Path::as_str() is an object relative path expression [1], except that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line removes the possible
        // trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let canonical_path = path.as_ref().strip_suffix('/').unwrap_or_else(|| path.as_ref());

        if canonical_path == "meta" {
            // This branch is done here instead of in MetaAsDir so that Clone'ing MetaAsDir yields
            // MetaAsDir. See the MetaAsDir::open impl for more.

            // TODO(https://fxbug.dev/328485661): consider retrieving the merkle root by retrieving
            // the attribute instead of opening as a file to read the merkle root content.

            // To remain POSIX compliant, we must default to opening meta as a file unless the
            // directory protocol (which maps to O_DIRECTORY) is specified. Otherwise, it would be
            // impossible to open as a directory, since there is no equivalent flag in POSIX that
            // maps to the file protocol (the lack of O_DIRECTORY is used to specify this).
            let open_meta_as_file =
                !flags.intersects(fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::PROTOCOL_NODE);
            return if open_meta_as_file {
                let file = self.create_meta_as_file().map_err(|e| {
                    error!("Error creating the meta file: {:?}", e);
                    zx::Status::INTERNAL
                })?;
                vfs::file::serve(file, scope, &flags, object_request)
            } else if flags.is_node() || flags.is_dir_allowed() {
                MetaAsDir::new(self).open3(scope, VfsPath::dot(), flags, object_request)
            } else {
                // Reject opening as a symlink.
                Err(zx::Status::WRONG_TYPE)
            };
        }

        if canonical_path.starts_with("meta/") {
            if let Some(file) = self.get_meta_file(canonical_path)? {
                return vfs::file::serve(file, scope, &flags, object_request);
            }

            if let Some(subdir) = self.get_meta_subdir(canonical_path.to_string() + "/") {
                return subdir.open3(scope, VfsPath::dot(), flags, object_request);
            }
            return Err(zx::Status::NOT_FOUND);
        }

        if let Some(blob) = self.non_meta_files.get(canonical_path) {
            return self.non_meta_storage.open3(blob, flags, scope, object_request);
        }

        if let Some(subdir) = self.get_non_meta_subdir(canonical_path.to_string() + "/") {
            return subdir.open3(scope, VfsPath::dot(), flags, object_request);
        }

        Err(zx::Status::NOT_FOUND)
    }

    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<(dyn vfs::directory::dirents_sink::Sink + 'static)>,
    ) -> Result<
        (TraversalPosition, Box<(dyn vfs::directory::dirents_sink::Sealed + 'static)>),
        zx::Status,
    > {
        vfs::directory::read_dirents::read_dirents(
            // Add "meta/placeholder" file so the "meta" dir is included in the results
            &crate::get_dir_children(
                self.non_meta_files.keys().map(|s| s.as_str()).chain(["meta/placeholder"]),
                "",
            ),
            pos,
            sink,
        )
        .await
    }

    fn register_watcher(
        self: Arc<Self>,
        _: ExecutionScope,
        _: fio::WatchMask,
        _: vfs::directory::entry_container::DirectoryWatcher,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    // `register_watcher` is unsupported so no need to do anything here.
    fn unregister_watcher(self: Arc<Self>, _: usize) {}
}

#[allow(clippy::type_complexity)]
fn load_package_metadata(
    meta_far_vmo: &zx::Vmo,
) -> Result<(HashMap<String, MetaFileLocation>, HashMap<String, fuchsia_hash::Hash>), Error> {
    let stream =
        zx::Stream::create(zx::StreamOptions::MODE_READ, meta_far_vmo, 0).map_err(|e| {
            Error::OpenMetaFar(NonMetaStorageError::ReadBlob(
                fuchsia_fs::file::ReadError::ReadError(e),
            ))
        })?;

    let mut reader = fuchsia_archive::Reader::new(stream).map_err(Error::ArchiveReader)?;
    let reader_list = reader.list();
    let mut meta_files = HashMap::with_capacity(reader_list.len());
    for entry in reader_list {
        let path = std::str::from_utf8(entry.path())
            .map_err(|source| Error::NonUtf8MetaEntry { source, path: entry.path().to_owned() })?
            .to_owned();
        if path.starts_with("meta/") {
            for (i, _) in path.match_indices('/').skip(1) {
                if meta_files.contains_key(&path[..i]) {
                    return Err(Error::FileDirectoryCollision { path: path[..i].to_string() });
                }
            }
            meta_files
                .insert(path, MetaFileLocation { offset: entry.offset(), length: entry.length() });
        }
    }

    let meta_contents_bytes =
        reader.read_file(b"meta/contents").map_err(Error::ReadMetaContents)?;

    let non_meta_files = MetaContents::deserialize(&meta_contents_bytes[..])
        .map_err(Error::DeserializeMetaContents)?
        .into_contents();

    Ok((meta_files, non_meta_files))
}

/// Location of a meta file's contents within a meta.far
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MetaFileLocation {
    offset: u64,
    length: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_proxy, Proxy as _};
    use fuchsia_fs::directory::{DirEntry, DirentKind};
    use fuchsia_pkg_testing::blobfs::Fake as FakeBlobfs;
    use fuchsia_pkg_testing::PackageBuilder;
    use futures::{StreamExt as _, TryStreamExt as _};
    use pretty_assertions::assert_eq;
    use std::convert::TryInto as _;
    use std::io::Cursor;
    use vfs::directory::entry::GetEntryInfo;
    use vfs::directory::entry_container::Directory;
    use vfs::node::Node;
    use vfs::ObjectRequest;

    struct TestEnv {
        _blobfs_fake: FakeBlobfs,
    }

    impl TestEnv {
        async fn with_subpackages_content(
            subpackages_content: Option<&[u8]>,
        ) -> (Self, Arc<RootDir<blobfs::Client>>) {
            let mut pkg = PackageBuilder::new("base-package-0")
                .add_resource_at("resource", "blob-contents".as_bytes())
                .add_resource_at("dir/file", "bloblob".as_bytes())
                .add_resource_at("meta/file", "meta-contents0".as_bytes())
                .add_resource_at("meta/dir/file", "meta-contents1".as_bytes());
            if let Some(subpackages_content) = subpackages_content {
                pkg = pkg.add_resource_at(fuchsia_pkg::MetaSubpackages::PATH, subpackages_content);
            }
            let pkg = pkg.build().await.unwrap();
            let (metafar_blob, content_blobs) = pkg.contents();
            let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
            blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
            for (hash, bytes) in content_blobs {
                blobfs_fake.add_blob(hash, bytes);
            }

            let root_dir = RootDir::new(blobfs_client, metafar_blob.merkle).await.unwrap();
            (Self { _blobfs_fake: blobfs_fake }, root_dir)
        }

        async fn new() -> (Self, Arc<RootDir<blobfs::Client>>) {
            Self::with_subpackages_content(None).await
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn new_missing_meta_far_error() {
        let (_blobfs_fake, blobfs_client) = FakeBlobfs::new();
        assert_matches!(
            RootDir::new(blobfs_client, [0; 32].into()).await,
            Err(Error::MissingMetaFar)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn new_rejects_invalid_utf8() {
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        let mut meta_far = vec![];
        let () = fuchsia_archive::write(
            &mut meta_far,
            std::collections::BTreeMap::from_iter([(
                b"\xff",
                (0, Box::new("".as_bytes()) as Box<dyn std::io::Read>),
            )]),
        )
        .unwrap();
        let hash = fuchsia_merkle::from_slice(&meta_far).root();
        let () = blobfs_fake.add_blob(hash, meta_far);

        assert_matches!(
            RootDir::new(blobfs_client, hash).await,
            Err(Error::NonUtf8MetaEntry{path, ..})
                if path == vec![255]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn new_initializes_maps() {
        let (_env, root_dir) = TestEnv::new().await;

        let meta_files = HashMap::from([
            (String::from("meta/contents"), MetaFileLocation { offset: 4096, length: 148 }),
            (String::from("meta/package"), MetaFileLocation { offset: 20480, length: 39 }),
            (String::from("meta/file"), MetaFileLocation { offset: 12288, length: 14 }),
            (String::from("meta/dir/file"), MetaFileLocation { offset: 8192, length: 14 }),
            (
                String::from("meta/fuchsia.abi/abi-revision"),
                MetaFileLocation { offset: 16384, length: 8 },
            ),
        ]);
        assert_eq!(root_dir.meta_files, meta_files);

        let non_meta_files: HashMap<String, fuchsia_hash::Hash> = [
            (
                String::from("resource"),
                "bd905f783ceae4c5ba8319703d7505ab363733c2db04c52c8405603a02922b15"
                    .parse::<fuchsia_hash::Hash>()
                    .unwrap(),
            ),
            (
                String::from("dir/file"),
                "5f615dd575994fcbcc174974311d59de258d93cd523d5cb51f0e139b53c33201"
                    .parse::<fuchsia_hash::Hash>()
                    .unwrap(),
            ),
        ]
        .iter()
        .cloned()
        .collect();
        assert_eq!(root_dir.non_meta_files, non_meta_files);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn rejects_meta_file_collisions() {
        let pkg = PackageBuilder::new("base-package-0")
            .add_resource_at("meta/dir/file", "meta-contents0".as_bytes())
            .build()
            .await
            .unwrap();

        // Manually modify the meta.far to contain a "meta/dir" entry.
        let (metafar_blob, _) = pkg.contents();
        let mut metafar =
            fuchsia_archive::Reader::new(Cursor::new(&metafar_blob.contents)).unwrap();
        let mut entries = std::collections::BTreeMap::new();
        let farentries =
            metafar.list().map(|entry| (entry.path().to_vec(), entry.length())).collect::<Vec<_>>();
        for (path, length) in farentries {
            let contents = metafar.read_file(&path).unwrap();
            entries
                .insert(path, (length, Box::new(Cursor::new(contents)) as Box<dyn std::io::Read>));
        }
        let extra_contents = b"meta-contents1";
        entries.insert(
            b"meta/dir".to_vec(),
            (
                extra_contents.len() as u64,
                Box::new(Cursor::new(extra_contents)) as Box<dyn std::io::Read>,
            ),
        );

        let mut metafar: Vec<u8> = vec![];
        let () = fuchsia_archive::write(&mut metafar, entries).unwrap();
        let merkle = fuchsia_merkle::from_slice(&metafar).root();

        // Verify it fails to load with the expected error.
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(merkle, &metafar);

        match RootDir::new(blobfs_client, merkle).await {
            Ok(_) => panic!("this should not be reached!"),
            Err(Error::FileDirectoryCollision { path }) => {
                assert_eq!(path, "meta/dir".to_string());
            }
            Err(e) => panic!("Expected collision error, receieved {e:?}"),
        };
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn read_file() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(root_dir.read_file("resource").await.unwrap().as_slice(), b"blob-contents");
        assert_eq!(root_dir.read_file("meta/file").await.unwrap().as_slice(), b"meta-contents0");
        assert_matches!(
            root_dir.read_file("missing").await.unwrap_err(),
            ReadFileError::NoFileAtPath{path} if path == "missing"
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn has_file() {
        let (_env, root_dir) = TestEnv::new().await;

        assert!(root_dir.has_file("resource"));
        assert!(root_dir.has_file("meta/file"));
        assert_eq!(root_dir.has_file("missing"), false);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn external_file_hashes() {
        let (_env, root_dir) = TestEnv::new().await;

        let mut actual = root_dir.external_file_hashes().copied().collect::<Vec<_>>();
        actual.sort();
        assert_eq!(
            actual,
            vec![
                "5f615dd575994fcbcc174974311d59de258d93cd523d5cb51f0e139b53c33201".parse().unwrap(),
                "bd905f783ceae4c5ba8319703d7505ab363733c2db04c52c8405603a02922b15".parse().unwrap()
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn path() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            root_dir.path().await.unwrap(),
            "base-package-0/0".parse::<fuchsia_pkg::PackagePath>().unwrap()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn subpackages_present() {
        let subpackages = fuchsia_pkg::MetaSubpackages::from_iter([(
            fuchsia_url::RelativePackageUrl::parse("subpackage-name").unwrap(),
            "0000000000000000000000000000000000000000000000000000000000000000".parse().unwrap(),
        )]);
        let mut subpackages_bytes = vec![];
        let () = subpackages.serialize(&mut subpackages_bytes).unwrap();
        let (_env, root_dir) = TestEnv::with_subpackages_content(Some(&*subpackages_bytes)).await;

        assert_eq!(root_dir.subpackages().await.unwrap(), subpackages);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn subpackages_absent() {
        let (_env, root_dir) = TestEnv::with_subpackages_content(None).await;

        assert_eq!(root_dir.subpackages().await.unwrap(), fuchsia_pkg::MetaSubpackages::default());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn subpackages_error() {
        let (_env, root_dir) = TestEnv::with_subpackages_content(Some(b"invalid-json")).await;

        assert_matches!(root_dir.subpackages().await, Err(SubpackagesError::Parse(_)));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_get_attributes() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            Node::get_attributes(root_dir.as_ref(), fio::NodeAttributesQuery::all()).await.unwrap(),
            immutable_attributes!(
                fio::NodeAttributesQuery::all(),
                Immutable {
                    protocols: fio::NodeProtocolKinds::DIRECTORY,
                    abilities: crate::DIRECTORY_ABILITIES,
                    id: 1,
                }
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_entry_info() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            GetEntryInfo::entry_info(root_dir.as_ref()),
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_read_dirents() {
        let (_env, root_dir) = TestEnv::new().await;

        let (pos, sealed) = Directory::read_dirents(
            root_dir.as_ref(),
            &TraversalPosition::Start,
            Box::new(crate::tests::FakeSink::new(4)),
        )
        .await
        .expect("read_dirents failed");

        assert_eq!(
            crate::tests::FakeSink::from_sealed(sealed).entries,
            vec![
                (".".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("dir".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("meta".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("resource".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File))
            ]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_register_watcher_not_supported() {
        let (_env, root_dir) = TestEnv::new().await;

        let (_client, server) = fidl::endpoints::create_endpoints();

        assert_eq!(
            Directory::register_watcher(
                root_dir,
                ExecutionScope::new(),
                fio::WatchMask::empty(),
                server.try_into().unwrap(),
            ),
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_invalid_flags() {
        let (_env, root_dir) = TestEnv::new().await;

        for forbidden_flag in [
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::CREATE,
            fio::OpenFlags::CREATE_IF_ABSENT,
            fio::OpenFlags::TRUNCATE,
            fio::OpenFlags::APPEND,
        ] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::DESCRIBE | forbidden_flag,
                VfsPath::dot(),
                server_end.into_channel().into(),
            );

            assert_matches!(
                proxy.take_event_stream().next().await,
                Some(Ok(fio::DirectoryEvent::OnOpen_{ s, info: None}))
                    if s == zx::Status::NOT_SUPPORTED.into_raw()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_self() {
        let (_env, root_dir) = TestEnv::new().await;
        let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();

        root_dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE,
            VfsPath::dot(),
            server_end.into_channel().into(),
        );

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "meta".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "resource".to_string(), kind: DirentKind::File }
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_non_meta_file() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["resource", "resource/"] {
            let (proxy, server_end) = create_proxy();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end,
            );

            assert_eq!(
                fuchsia_fs::file::read(&fio::FileProxy::from_channel(
                    proxy.into_channel().unwrap()
                ))
                .await
                .unwrap(),
                b"blob-contents".to_vec()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_meta_as_file() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::FileMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(
                fuchsia_fs::file::read(&proxy).await.unwrap(),
                root_dir.hash.to_string().as_bytes()
            );

            // Cloning meta_as_file yields meta_as_file
            let (cloned_proxy, server_end) = create_proxy::<fio::FileMarker>();
            let () = proxy.clone(server_end.into_channel().into()).unwrap();
            assert_eq!(
                fuchsia_fs::file::read(&cloned_proxy).await.unwrap(),
                root_dir.hash.to_string().as_bytes()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_meta_as_dir() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![
                    DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                    DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "file".to_string(), kind: DirentKind::File },
                    DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "package".to_string(), kind: DirentKind::File },
                ]
            );

            // Cloning meta_as_dir yields meta_as_dir
            let (cloned_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let () = proxy.clone(server_end.into_channel().into()).unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&cloned_proxy).await.unwrap(),
                vec![
                    DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                    DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "file".to_string(), kind: DirentKind::File },
                    DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "package".to_string(), kind: DirentKind::File },
                ]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_meta_as_node_reference() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::NodeMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NODE_REFERENCE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            // Check that open as a node reference passed by calling `get_attr()` on the proxy.
            // The returned attributes should indicate the meta is a directory.
            let (status, attr) = proxy.get_attr().await.expect("get_attr failed");
            assert_eq!(zx::Status::from_raw(status), zx::Status::OK);
            assert_eq!(attr.mode & fio::MODE_TYPE_MASK, fio::MODE_TYPE_DIRECTORY);
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_meta_file() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta/file", "meta/file/"] {
            let (proxy, server_end) = create_proxy::<fio::FileMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"meta-contents0".to_vec());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_meta_subdir() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta/dir", "meta/dir/"] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_non_meta_subdir() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["dir", "dir/"] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();

            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_self() {
        let (_env, root_dir) = TestEnv::new().await;
        let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
        let scope = ExecutionScope::new();
        let flags = fio::Flags::PERM_READ;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|req| root_dir.open3(scope, VfsPath::dot(), flags, req));

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "meta".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "resource".to_string(), kind: DirentKind::File }
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_non_meta_file() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["resource", "resource/"] {
            let (proxy, server_end) = create_proxy::<fio::NodeMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));

            assert_eq!(
                fuchsia_fs::file::read(&fio::FileProxy::from_channel(
                    proxy.into_channel().unwrap()
                ))
                .await
                .unwrap(),
                b"blob-contents".to_vec()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_meta_as_file() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::FileMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PROTOCOL_FILE | fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));
            assert_eq!(
                fuchsia_fs::file::read(&proxy).await.unwrap(),
                root_dir.hash.to_string().as_bytes()
            );

            // Cloning meta_as_file yields meta_as_file
            let (cloned_proxy, server_end) = create_proxy::<fio::FileMarker>();
            let () = proxy.clone(server_end.into_channel().into()).unwrap();
            assert_eq!(
                fuchsia_fs::file::read(&cloned_proxy).await.unwrap(),
                root_dir.hash.to_string().as_bytes()
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_meta_as_dir() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PROTOCOL_DIRECTORY | fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));
            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![
                    DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                    DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "file".to_string(), kind: DirentKind::File },
                    DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "package".to_string(), kind: DirentKind::File },
                ]
            );

            // Cloning meta_as_dir yields meta_as_dir
            let (cloned_proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let () = proxy.clone(server_end.into_channel().into()).unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&cloned_proxy).await.unwrap(),
                vec![
                    DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                    DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "file".to_string(), kind: DirentKind::File },
                    DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                    DirEntry { name: "package".to_string(), kind: DirentKind::File },
                ]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_meta_as_node_reference() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::NodeMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PROTOCOL_NODE
                | fio::Flags::PERM_GET_ATTRIBUTES
                | fio::Flags::FLAG_SEND_REPRESENTATION;
            let options = fio::Options {
                attributes: Some(fio::NodeAttributesQuery::PROTOCOLS),
                ..Default::default()
            };
            ObjectRequest::new(flags, &options, server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));

            let event = proxy
                .take_event_stream()
                .try_next()
                .await
                .expect("take_event_stream failed")
                .expect("expected an OnRepresentation event");
            let representation = match event {
                fio::NodeEvent::OnRepresentation { payload } => payload,
                fio::NodeEvent::OnOpen_ { .. } => panic!("unexpected OnOpen representation"),
                fio::NodeEvent::_UnknownEvent { ordinal, .. } => panic!("unknown event {ordinal}"),
            };
            assert_matches!(representation,
                fio::Representation::Connector(fio::ConnectorInfo {
                    attributes: Some(node_attributes),
                    ..
                })
                if node_attributes == immutable_attributes!(
                    fio::NodeAttributesQuery::PROTOCOLS,
                    Immutable { protocols: fio::NodeProtocolKinds::DIRECTORY, abilities: crate::DIRECTORY_ABILITIES }
                )
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_meta_as_symlink_wrong_type() {
        let (_env, root_dir) = TestEnv::new().await;

        // Opening as symlink should return an error
        for path in ["meta", "meta/"] {
            let (proxy, server_end) = create_proxy::<fio::SymlinkMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PROTOCOL_SYMLINK;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::WRONG_TYPE, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_meta_file() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta/file", "meta/file/"] {
            let (proxy, server_end) = create_proxy::<fio::FileMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));

            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"meta-contents0".to_vec());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_meta_subdir() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["meta/dir", "meta/dir/"] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_non_meta_subdir() {
        let (_env, root_dir) = TestEnv::new().await;

        for path in ["dir", "dir/"] {
            let (proxy, server_end) = create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, path, flags, req));

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_rejects_invalid_flags() {
        let (_env, root_dir) = TestEnv::new().await;

        for invalid_flags in
            [fio::Flags::FLAG_MUST_CREATE, fio::Flags::FLAG_MAYBE_CREATE, fio::Flags::PERM_WRITE]
        {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let flags = fio::Flags::FLAG_SEND_REPRESENTATION | invalid_flags;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, VfsPath::dot(), flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_rejects_file_flags() {
        let (_env, root_dir) = TestEnv::new().await;

        // Requesting to open with `PROTOCOL_FILE` should return a `NOT_FILE` error.
        {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let flags = fio::Flags::PROTOCOL_FILE;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, VfsPath::dot(), flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FILE, .. })
            );
        }

        // Opening with file flags is also invalid.
        for file_flags in [fio::Flags::FILE_APPEND, fio::Flags::FILE_TRUNCATE] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            ObjectRequest::new(file_flags, &fio::Options::default(), server_end.into())
                .handle(|req| root_dir.clone().open3(scope, VfsPath::dot(), file_flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::INVALID_ARGS, .. })
            );
        }
    }
}
