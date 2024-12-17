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
use vfs::path::Path as VfsPath;
use vfs::{immutable_attributes, ObjectRequestRef, ProtocolsExt, ToObjectRequest};

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
        path: VfsPath,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let flags = flags & !(fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE);
        let describe = flags.contains(fio::OpenFlags::DESCRIBE);

        if flags.intersects(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT) {
            let () = send_on_open_with_error(describe, server_end, zx::Status::NOT_SUPPORTED);
            return;
        }

        if path.is_empty() {
            flags.to_object_request(server_end).handle(|object_request| {
                if flags.intersects(
                    fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::RIGHT_EXECUTABLE
                        | fio::OpenFlags::TRUNCATE
                        | fio::OpenFlags::APPEND,
                ) {
                    return Err(zx::Status::NOT_SUPPORTED);
                }

                // Only MetaAsDir can be obtained from Open calls to MetaAsDir. To obtain the "meta"
                // file, the Open call must be made on RootDir. This is consistent with pkgfs
                // behavior and is needed so that Clone'ing MetaAsDir results in MetaAsDir, because
                // VFS handles Clone by calling Open with a path of ".", a mode of 0, and mostly
                // unmodified flags and that combination of arguments would normally result in the
                // file being used.
                object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
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
        if flags.creation_mode() != vfs::CreationMode::Never {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        if path.is_empty() {
            if let Some(rights) = flags.rights() {
                if rights.intersects(fio::Operations::WRITE_BYTES)
                    | rights.intersects(fio::Operations::EXECUTE)
                {
                    return Err(zx::Status::NOT_SUPPORTED);
                }
            }

            // Only MetaAsDir can be obtained from Open calls to MetaAsDir. To obtain the "meta"
            // file, the Open call must be made on RootDir. This is consistent with pkgfs behavior
            // and is needed so that Clone'ing MetaAsDir results in MetaAsDir, because VFS handles
            // Clone by calling Open with a path of ".", a mode of 0, and mostly unmodified flags
            // and that combination of arguments would normally result in the file being used.
            //
            // `ImmutableConnection::create` will check flags contain only directory-allowed flags.
            return object_request.spawn_connection(
                scope,
                self,
                flags,
                ImmutableConnection::create,
            );
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
    use futures::prelude::*;
    use std::convert::TryInto as _;
    use vfs::directory::entry::EntryInfo;
    use vfs::directory::entry_container::Directory;
    use vfs::node::Node;
    use vfs::ObjectRequest;

    struct TestEnv {
        _blobfs_fake: FakeBlobfs,
    }

    impl TestEnv {
        async fn new() -> (Self, Arc<MetaAsDir<blobfs::Client>>) {
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
            (Self { _blobfs_fake: blobfs_fake }, meta_as_dir)
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_unsets_posix_flags() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        let () = crate::verify_open_adjusts_flags(
            meta_as_dir,
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            fio::OpenFlags::RIGHT_READABLE,
        )
        .await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_disallowed_flags() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        for forbidden_flag in [
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::CREATE,
            fio::OpenFlags::CREATE_IF_ABSENT,
            fio::OpenFlags::TRUNCATE,
            fio::OpenFlags::APPEND,
        ] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            meta_as_dir.clone().open(
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
        let (_env, meta_as_dir) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        meta_as_dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE,
            VfsPath::dot(),
            server_end.into_channel().into(),
        );

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![
                DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "package".to_string(), kind: DirentKind::File }
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_file() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        for path in ["dir/file", "dir/file/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
            meta_as_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::RIGHT_READABLE,
                VfsPath::validate_and_split(path).unwrap(),
                server_end.into_channel().into(),
            );

            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"contents".to_vec());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_directory() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        for path in ["dir", "dir/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            meta_as_dir.clone().open(
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
    async fn directory_read_dirents() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        let (pos, sealed) = Directory::read_dirents(
            meta_as_dir.as_ref(),
            &TraversalPosition::Start,
            Box::new(crate::tests::FakeSink::new(5)),
        )
        .await
        .expect("read_dirents failed");
        assert_eq!(
            crate::tests::FakeSink::from_sealed(sealed).entries,
            vec![
                (".".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("contents".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)),
                ("dir".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                (
                    "fuchsia.abi".to_string(),
                    EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
                ),
                ("package".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)),
            ]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_register_watcher_not_supported() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        let (_client, server) = fidl::endpoints::create_endpoints();

        assert_eq!(
            Directory::register_watcher(
                meta_as_dir,
                ExecutionScope::new(),
                fio::WatchMask::empty(),
                server.try_into().unwrap(),
            ),
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_get_attributes() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        assert_eq!(
            Node::get_attributes(meta_as_dir.as_ref(), fio::NodeAttributesQuery::all())
                .await
                .unwrap(),
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
    async fn directory_entry_open3_self() {
        let (_env, meta_as_dir) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        let scope = ExecutionScope::new();
        let flags = fio::Flags::PERM_READ;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|req| meta_as_dir.open3(scope, VfsPath::dot(), flags, req));

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![
                DirEntry { name: "contents".to_string(), kind: DirentKind::File },
                DirEntry { name: "dir".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "fuchsia.abi".to_string(), kind: DirentKind::Directory },
                DirEntry { name: "package".to_string(), kind: DirentKind::File }
            ]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_file() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        for path in ["dir/file", "dir/file/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| meta_as_dir.clone().open3(scope, path, flags, req));

            assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), b"contents".to_vec());
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_directory() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        for path in ["dir", "dir/"] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let path = VfsPath::validate_and_split(path).unwrap();
            let flags = fio::Flags::PERM_READ;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| meta_as_dir.clone().open3(scope, path, flags, req));

            assert_eq!(
                fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
                vec![DirEntry { name: "file".to_string(), kind: DirentKind::File }]
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_rejects_invalid_flags() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        for invalid_flags in [
            fio::Flags::FLAG_MUST_CREATE,
            fio::Flags::FLAG_MAYBE_CREATE,
            fio::Flags::PERM_WRITE,
            fio::Flags::PERM_EXECUTE,
        ] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let flags = fio::Flags::PERM_READ | invalid_flags;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| meta_as_dir.clone().open3(scope, VfsPath::dot(), flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_rejects_file_flags() {
        let (_env, meta_as_dir) = TestEnv::new().await;

        // Requesting to open with `PROTOCOL_FILE` should return a `NOT_FILE` error.
        {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let flags = fio::Flags::PROTOCOL_FILE;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| meta_as_dir.clone().open3(scope, VfsPath::dot(), flags, req));

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
                .handle(|req| meta_as_dir.clone().open3(scope, VfsPath::dot(), file_flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::INVALID_ARGS, .. })
            );
        }
    }
}
