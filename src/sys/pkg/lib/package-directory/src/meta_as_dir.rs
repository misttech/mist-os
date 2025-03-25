// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::root_dir::RootDir;
use crate::usize_to_u64_safe;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use std::sync::Arc;
use vfs::common::send_on_open_with_error;
use vfs::directory::entry::EntryInfo;
use vfs::directory::immutable::connection::ImmutableConnection;
use vfs::directory::traversal_position::TraversalPosition;
use vfs::execution_scope::ExecutionScope;
use vfs::{immutable_attributes, ObjectRequestRef, ProtocolsExt as _, ToObjectRequest as _};

pub(crate) struct MetaAsDir<S: crate::NonMetaStorage> {
    root_dir: Arc<RootDir<S>>,
}

impl<S: crate::NonMetaStorage> MetaAsDir<S> {
    pub(crate) fn new(root_dir: Arc<RootDir<S>>) -> Arc<Self> {
        Arc::new(MetaAsDir { root_dir })
    }
}

impl<S: crate::NonMetaStorage> vfs::directory::entry::GetEntryInfo for MetaAsDir<S> {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

impl<S: crate::NonMetaStorage> vfs::node::Node for MetaAsDir<S> {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: crate::DIRECTORY_ABILITIES,
                content_size: usize_to_u64_safe(self.root_dir.meta_files.len()),
                storage_size: usize_to_u64_safe(self.root_dir.meta_files.len()),
                id: 1,
            }
        ))
    }
}

impl<S: crate::NonMetaStorage> vfs::directory::entry_container::Directory for MetaAsDir<S> {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: vfs::Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let flags = flags & !(fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE);
        let describe = flags.contains(fio::OpenFlags::DESCRIBE);
        // Disallow creating a writable or executable connection to this node or any children. We
        // also disallow file flags which do not apply. Note that the latter is not required for
        // Open3, as we require writable rights for the latter flags already.
        if flags.intersects(
            fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::RIGHT_EXECUTABLE
                | fio::OpenFlags::TRUNCATE,
        ) {
            let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_SUPPORTED);
            return;
        }
        // The VFS should disallow file creation since we cannot serve a mutable connection.
        assert!(!flags.intersects(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT));

        if path.is_empty() {
            flags.to_object_request(server_end).handle(|object_request| {
                // NOTE: Some older CTF tests still rely on being able to use the APPEND flag in
                // some cases, so we cannot check this flag above. Appending is still not possible.
                // As we plan to remove this method entirely, we can just leave this for now.
                if flags.intersects(fio::OpenFlags::APPEND) {
                    return Err(zx::Status::NOT_SUPPORTED);
                }

                // Only MetaAsDir can be obtained from Open calls to MetaAsDir. To obtain the "meta"
                // file, the Open call must be made on RootDir. This is consistent with pkgfs
                // behavior and is needed so that Clone'ing MetaAsDir results in MetaAsDir, because
                // VFS handles Clone by calling Open with a path of ".", a mode of 0, and mostly
                // unmodified flags and that combination of arguments would normally result in the
                // file being used.
                object_request
                    .take()
                    .create_connection_sync::<ImmutableConnection<_>, _>(scope, self, flags);
                Ok(())
            });
            return;
        }

        // <path as vfs::path::Path>::as_str() is an object relative path expression [1], except
        // that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line removes the possible
        // trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let file_path =
            format!("meta/{}", path.as_ref().strip_suffix('/').unwrap_or_else(|| path.as_ref()));

        match self.root_dir.get_meta_file(&file_path) {
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

        if let Some(subdir) = self.root_dir.get_meta_subdir(file_path + "/") {
            let () = subdir.open(scope, flags, vfs::Path::dot(), server_end);
            return;
        }

        let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_FOUND);
    }

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: vfs::Path,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        // Disallow creating a mutable or executable connection to this node or any children.
        if flags.intersects(crate::MUTABLE_FLAGS.union(fio::Flags::PERM_EXECUTE)) {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        // The VFS should disallow file creation or append/truncate as these require mutable rights.
        assert!(flags.creation_mode() == vfs::CreationMode::Never);
        assert!(!flags.intersects(fio::Flags::FILE_APPEND | fio::Flags::FILE_TRUNCATE));

        if path.is_empty() {
            // Only MetaAsDir can be obtained from Open calls to MetaAsDir. To obtain the "meta"
            // file, the Open call must be made on RootDir. This is consistent with pkgfs behavior
            // and is needed so that Clone'ing MetaAsDir results in MetaAsDir, because VFS handles
            // Clone by calling Open with a path of ".", a mode of 0, and mostly unmodified flags
            // and that combination of arguments would normally result in the file being used.
            //
            // `ImmutableConnection` will check flags contain only directory-allowed flags.
            object_request
                .take()
                .create_connection_sync::<ImmutableConnection<_>, _>(scope, self, flags);
            return Ok(());
        }

        // <path as vfs::path::Path>::as_str() is an object relative path expression [1], except
        // that it may:
        //   1. have a trailing "/"
        //   2. be exactly "."
        //   3. be longer than 4,095 bytes
        // The .is_empty() check above rules out "." and the following line removes the possible
        // trailing "/".
        // [1] https://fuchsia.dev/fuchsia-src/concepts/process/namespaces?hl=en#object_relative_path_expressions
        let file_path =
            format!("meta/{}", path.as_ref().strip_suffix('/').unwrap_or_else(|| path.as_ref()));

        if let Some(file) = self.root_dir.get_meta_file(&file_path)? {
            return vfs::file::serve(file, scope, &flags, object_request);
        }

        if let Some(subdir) = self.root_dir.get_meta_subdir(file_path + "/") {
            return subdir.open3(scope, vfs::Path::dot(), flags, object_request);
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
            &crate::get_dir_children(self.root_dir.meta_files.keys().map(|s| s.as_str()), "meta/"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fuchsia_fs::directory::{DirEntry, DirentKind};
    use fuchsia_pkg_testing::blobfs::Fake as FakeBlobfs;
    use fuchsia_pkg_testing::PackageBuilder;
    use futures::TryStreamExt as _;
    use vfs::directory::entry_container::Directory as _;

    struct TestEnv {
        _blobfs_fake: FakeBlobfs,
    }

    impl TestEnv {
        async fn new() -> (Self, fio::DirectoryProxy) {
            let pkg = PackageBuilder::new("pkg")
                .add_resource_at("meta/dir/file", &b"contents"[..])
                .build()
                .await
                .unwrap();
            let (metafar_blob, _) = pkg.contents();
            let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
            blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
            let root_dir = RootDir::new(blobfs_client, metafar_blob.merkle).await.unwrap();
            let meta_as_dir = MetaAsDir::new(root_dir);
            (Self { _blobfs_fake: blobfs_fake }, vfs::directory::serve_read_only(meta_as_dir))
        }
    }

    /// Ensure connections to a [`MetaAsDir`] cannot be created as mutable (i.e. with
    /// [`fio::PERM_WRITABLE`]) or executable ([`fio::PERM_EXECUTABLE`]). This ensures that the VFS
    /// will disallow any attempts to create a new file/directory, modify the attributes of any
    /// nodes, or open any files as writable/executable.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_cannot_be_served_as_mutable() {
        let pkg = PackageBuilder::new("pkg")
            .add_resource_at("meta/dir/file", &b"contents"[..])
            .build()
            .await
            .unwrap();
        let (metafar_blob, _) = pkg.contents();
        let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
        blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
        let meta_as_dir =
            MetaAsDir::new(RootDir::new(blobfs_client, metafar_blob.merkle).await.unwrap());
        for flags in [fio::PERM_WRITABLE, fio::PERM_EXECUTABLE] {
            let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let request = flags.to_object_request(server);
            request.handle(|request: &mut vfs::ObjectRequest| {
                meta_as_dir.clone().open3(ExecutionScope::new(), vfs::Path::dot(), flags, request)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_readdir() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        assert_eq!(
            fuchsia_fs::directory::readdir_inclusive(&meta_as_dir).await.unwrap(),
            vec![
                DirEntry { name: ".".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "package".to_string(), kind: DirentKind::File }
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_get_attributes() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        let (mutable_attributes, immutable_attributes) =
            meta_as_dir.get_attributes(fio::NodeAttributesQuery::all()).await.unwrap().unwrap();
        assert_eq!(
            fio::NodeAttributes2 { mutable_attributes, immutable_attributes },
            immutable_attributes!(
                fio::NodeAttributesQuery::all(),
                Immutable {
                    protocols: fio::NodeProtocolKinds::DIRECTORY,
                    abilities: crate::DIRECTORY_ABILITIES,
                    content_size: 4,
                    storage_size: 4,
                    id: 1,
                }
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_watch_not_supported() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        let (_client, server) = fidl::endpoints::create_endpoints();
        let status = zx::Status::from_raw(
            meta_as_dir.watch(fio::WatchMask::empty(), 0, server).await.unwrap(),
        );
        assert_eq!(status, zx::Status::NOT_SUPPORTED);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_open_file() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        for path in ["dir/file", "dir/file/"] {
            let proxy = fuchsia_fs::directory::open_file(&meta_as_dir, path, fio::PERM_READABLE)
                .await
                .unwrap();
            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"contents".to_vec());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_open_directory() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        for path in ["dir", "dir/"] {
            let proxy =
                fuchsia_fs::directory::open_directory(&meta_as_dir, path, fio::PERM_READABLE)
                    .await
                    .unwrap();
            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_deprecated_open_file() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
        meta_as_dir
            .deprecated_open(
                fio::OpenFlags::RIGHT_READABLE,
                Default::default(),
                "dir/file",
                server_end.into_channel().into(),
            )
            .unwrap();
        assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"contents".to_vec());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn meta_as_dir_deprecated_open_directory() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        for path in ["dir", "dir/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            meta_as_dir
                .deprecated_open(
                    fio::OpenFlags::RIGHT_READABLE,
                    Default::default(),
                    path,
                    server_end.into_channel().into(),
                )
                .unwrap();

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }
}
