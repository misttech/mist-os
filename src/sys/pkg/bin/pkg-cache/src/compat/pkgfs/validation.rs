// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use log::{error, info};
use std::collections::HashSet;
use std::sync::Arc;
use vfs::directory::entry::{EntryInfo, OpenRequest};
use vfs::directory::immutable::connection::ImmutableConnection;
use vfs::directory::traversal_position::TraversalPosition;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path as VfsPath;
use vfs::{immutable_attributes, ObjectRequestRef, ProtocolsExt as _, ToObjectRequest};

/// The pkgfs /ctl/validation directory, except it contains only the "missing" file (e.g. does not
/// have the "present" file).
pub(crate) struct Validation {
    blobfs: blobfs::Client,
    base_blobs: HashSet<fuchsia_hash::Hash>,
}

impl Validation {
    pub(crate) fn new(
        blobfs: blobfs::Client,
        base_blobs: HashSet<fuchsia_hash::Hash>,
    ) -> Arc<Self> {
        Arc::new(Self { blobfs, base_blobs })
    }

    // The contents of the "missing" file. The hex-encoded hashes of all the base blobs missing
    // from blobfs, separated and terminated by the newline character, '\n'.
    async fn make_missing_contents(&self) -> Vec<u8> {
        info!("checking if any of the {} base package blobs are missing", self.base_blobs.len());

        let mut missing = self
            .blobfs
            .filter_to_missing_blobs(&self.base_blobs)
            .await
            .into_iter()
            .collect::<Vec<_>>();
        missing.sort();

        if missing.is_empty() {
            info!(total = self.base_blobs.len(); "all base package blobs were found");
        } else {
            error!(total = missing.len(); "base package blobs are missing");
        }

        #[allow(clippy::format_collect)]
        missing.into_iter().map(|hash| format!("{hash}\n")).collect::<String>().into_bytes()
    }
}

impl vfs::directory::entry::DirectoryEntry for Validation {
    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_dir(self)
    }
}

impl vfs::directory::entry::GetEntryInfo for Validation {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }
}

impl vfs::node::Node for Validation {
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(immutable_attributes!(
            requested_attributes,
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE,
                content_size: 1,
                storage_size: 1,
                id: 1,
            }
        ))
    }
}

impl vfs::directory::entry_container::Directory for Validation {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: VfsPath,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        let flags =
            flags.difference(fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE);

        let object_request = flags.to_object_request(server_end);

        if path.is_empty() {
            object_request.handle(|object_request| {
                if flags.intersects(
                    fio::OpenFlags::RIGHT_WRITABLE
                        | fio::OpenFlags::RIGHT_EXECUTABLE
                        | fio::OpenFlags::CREATE
                        | fio::OpenFlags::CREATE_IF_ABSENT
                        | fio::OpenFlags::TRUNCATE
                        | fio::OpenFlags::APPEND,
                ) {
                    return Err(zx::Status::NOT_SUPPORTED);
                }

                object_request.spawn_connection(scope, self, flags, ImmutableConnection::create)
            });
            return;
        }

        if path.as_ref() == "missing" {
            scope.clone().spawn(async move {
                let missing_contents = self.make_missing_contents().await;
                object_request.handle(|object_request| {
                    vfs::file::serve(
                        vfs::file::vmo::read_only(missing_contents),
                        scope,
                        &flags,
                        object_request,
                    )
                });
            });
            return;
        }

        object_request.shutdown(zx::Status::NOT_FOUND);
    }

    fn open3(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: VfsPath,
        flags: fio::Flags,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if path.is_empty() {
            if flags.creation_mode() != vfs::CreationMode::Never {
                return Err(zx::Status::NOT_SUPPORTED);
            }

            if let Some(rights) = flags.rights() {
                if rights.intersects(fio::Operations::WRITE_BYTES)
                    | rights.intersects(fio::Operations::EXECUTE)
                {
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

        if path.as_ref() == "missing" {
            let object_request = object_request.take();
            scope.clone().spawn(async move {
                let missing_contents = self.make_missing_contents().await;
                object_request.handle(|object_request| {
                    vfs::file::serve(
                        vfs::file::vmo::read_only(missing_contents),
                        scope,
                        &flags,
                        object_request,
                    )
                });
            });
            return Ok(());
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
            &[(EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File), "missing".into())],
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
    use blobfs_ramdisk::BlobfsRamdisk;
    use futures::prelude::*;
    use std::convert::TryInto as _;
    use vfs::directory::entry::GetEntryInfo;
    use vfs::directory::entry_container::Directory;
    use vfs::node::Node;
    use vfs::ObjectRequest;

    struct TestEnv {
        _blobfs: BlobfsRamdisk,
    }

    impl TestEnv {
        async fn new() -> (Self, Arc<Validation>) {
            Self::with_base_blobs_and_blobfs_contents(HashSet::new(), std::iter::empty()).await
        }

        async fn with_base_blobs_and_blobfs_contents(
            base_blobs: HashSet<fuchsia_hash::Hash>,
            blobfs_contents: impl IntoIterator<Item = (fuchsia_hash::Hash, Vec<u8>)>,
        ) -> (Self, Arc<Validation>) {
            let blobfs = BlobfsRamdisk::start().await.unwrap();
            for (hash, contents) in blobfs_contents.into_iter() {
                blobfs.add_blob_from(hash, contents.as_slice()).await.unwrap()
            }
            let validation = Validation::new(blobfs.client(), base_blobs);
            (Self { _blobfs: blobfs }, validation)
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_disallowed_flags() {
        let (_env, validation) = TestEnv::new().await;

        for forbidden_flag in [
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::CREATE,
            fio::OpenFlags::CREATE_IF_ABSENT,
            fio::OpenFlags::TRUNCATE,
            fio::OpenFlags::APPEND,
        ] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            validation.clone().open(
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
        let (_env, validation) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        validation.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE,
            VfsPath::dot(),
            server_end.into_channel().into(),
        );

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![fuchsia_fs::directory::DirEntry {
                name: "missing".to_string(),
                kind: fuchsia_fs::directory::DirentKind::File
            }]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_missing() {
        let (_env, validation) = TestEnv::with_base_blobs_and_blobfs_contents(
            HashSet::from([[0; 32].into()]),
            std::iter::empty(),
        )
        .await;

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
        validation.clone().open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE,
            VfsPath::validate_and_split("missing").unwrap(),
            server_end.into_channel().into(),
        );

        assert_eq!(
            fuchsia_fs::file::read(&proxy).await.unwrap(),
            b"0000000000000000000000000000000000000000000000000000000000000000\n".to_vec()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_entry_info() {
        let (_env, validation) = TestEnv::new().await;

        assert_eq!(
            GetEntryInfo::entry_info(validation.as_ref()),
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        );
    }

    /// Implementation of vfs::directory::dirents_sink::Sink.
    /// Sink::append begins to fail (returns Sealed) after `max_entries` entries have been appended.
    #[derive(Clone)]
    struct FakeSink {
        max_entries: usize,
        entries: Vec<(String, EntryInfo)>,
        sealed: bool,
    }

    impl FakeSink {
        fn new(max_entries: usize) -> Self {
            FakeSink { max_entries, entries: Vec::with_capacity(max_entries), sealed: false }
        }

        fn from_sealed(sealed: Box<dyn vfs::directory::dirents_sink::Sealed>) -> Box<FakeSink> {
            sealed.into()
        }
    }

    impl From<Box<dyn vfs::directory::dirents_sink::Sealed>> for Box<FakeSink> {
        fn from(sealed: Box<dyn vfs::directory::dirents_sink::Sealed>) -> Self {
            sealed.open().downcast::<FakeSink>().unwrap()
        }
    }

    impl vfs::directory::dirents_sink::Sink for FakeSink {
        fn append(
            mut self: Box<Self>,
            entry: &EntryInfo,
            name: &str,
        ) -> vfs::directory::dirents_sink::AppendResult {
            assert!(!self.sealed);
            if self.entries.len() == self.max_entries {
                vfs::directory::dirents_sink::AppendResult::Sealed(self.seal())
            } else {
                self.entries.push((name.to_owned(), entry.clone()));
                vfs::directory::dirents_sink::AppendResult::Ok(self)
            }
        }

        fn seal(mut self: Box<Self>) -> Box<dyn vfs::directory::dirents_sink::Sealed> {
            self.sealed = true;
            self
        }
    }

    impl vfs::directory::dirents_sink::Sealed for FakeSink {
        fn open(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_read_dirents() {
        let (_env, validation) = TestEnv::new().await;

        let (pos, sealed) = validation
            .read_dirents(&TraversalPosition::Start, Box::new(FakeSink::new(3)))
            .await
            .expect("read_dirents failed");
        assert_eq!(
            FakeSink::from_sealed(sealed).entries,
            vec![
                (".".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)),
                ("missing".to_string(), EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)),
            ]
        );
        assert_eq!(pos, TraversalPosition::End);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_register_watcher_not_supported() {
        let (_env, validation) = TestEnv::new().await;

        let (_client, server) = fidl::endpoints::create_endpoints();

        assert_eq!(
            Directory::register_watcher(
                validation,
                ExecutionScope::new(),
                fio::WatchMask::empty(),
                server.try_into().unwrap(),
            ),
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_get_attributes() {
        let (_env, validation) = TestEnv::new().await;

        assert_eq!(
            validation.get_attributes(fio::NodeAttributesQuery::all()).await.unwrap(),
            immutable_attributes!(
                fio::NodeAttributesQuery::all(),
                Immutable {
                    protocols: fio::NodeProtocolKinds::DIRECTORY,
                    abilities: fio::Operations::GET_ATTRIBUTES
                        | fio::Operations::ENUMERATE
                        | fio::Operations::TRAVERSE,
                    content_size: 1,
                    storage_size: 1,
                    id: 1,
                }
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn make_missing_contents_empty() {
        let (_env, validation) = TestEnv::new().await;

        assert_eq!(validation.make_missing_contents().await, Vec::<u8>::new());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn make_missing_contents_missing_blob() {
        let (_env, validation) = TestEnv::with_base_blobs_and_blobfs_contents(
            HashSet::from([[0; 32].into()]),
            std::iter::empty(),
        )
        .await;

        assert_eq!(
            validation.make_missing_contents().await,
            b"0000000000000000000000000000000000000000000000000000000000000000\n".to_vec()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn make_missing_contents_two_missing_blob() {
        let (_env, validation) = TestEnv::with_base_blobs_and_blobfs_contents(
            HashSet::from([[0; 32].into(), [1; 32].into()]),
            std::iter::empty(),
        )
        .await;

        assert_eq!(
            validation.make_missing_contents().await,
            b"0000000000000000000000000000000000000000000000000000000000000000\n\
              0101010101010101010101010101010101010101010101010101010101010101\n"
                .to_vec(),
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn make_missing_contents_irrelevant_blobfs_blob() {
        let blob = vec![0u8, 1u8];
        let hash = fuchsia_merkle::from_slice(&blob).root();
        let (_env, validation) =
            TestEnv::with_base_blobs_and_blobfs_contents(HashSet::new(), [(hash, blob)]).await;

        assert_eq!(validation.make_missing_contents().await, Vec::<u8>::new());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn make_missing_contents_present_blob() {
        let blob = vec![0u8, 1u8];
        let hash = fuchsia_merkle::from_slice(&blob).root();
        let (_env, validation) =
            TestEnv::with_base_blobs_and_blobfs_contents(HashSet::from([hash]), [(hash, blob)])
                .await;

        assert_eq!(validation.make_missing_contents().await, Vec::<u8>::new());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn make_missing_contents_present_blob_missing_blob() {
        let blob = vec![0u8, 1u8];
        let hash = fuchsia_merkle::from_slice(&blob).root();
        let mut missing_hash = <[u8; 32]>::from(hash);
        missing_hash[0] = !missing_hash[0];
        let missing_hash = missing_hash.into();

        let (_env, validation) = TestEnv::with_base_blobs_and_blobfs_contents(
            HashSet::from([hash, missing_hash]),
            [(hash, blob)],
        )
        .await;

        assert_eq!(
            validation.make_missing_contents().await,
            format!("{missing_hash}\n").into_bytes()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_self() {
        let (_env, validation) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        let flags = fio::Flags::PERM_READ;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
            .handle(|req| validation.open3(ExecutionScope::new(), VfsPath::dot(), flags, req));

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![fuchsia_fs::directory::DirEntry {
                name: "missing".to_string(),
                kind: fuchsia_fs::directory::DirentKind::File
            }]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_rejects_invalid_flags() {
        let (_env, validation) = TestEnv::new().await;

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
                .handle(|req| validation.clone().open3(scope, VfsPath::dot(), flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_rejects_file_flags() {
        let (_env, validation) = TestEnv::new().await;

        // Requesting to open with `PROTOCOL_FILE` should return a `NOT_FILE` error.
        {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let flags = fio::Flags::PROTOCOL_FILE;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| validation.clone().open3(scope, VfsPath::dot(), flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FILE, .. })
            );
        }

        // Opening with file flags is also invalid.
        for file_flags in [fio::Flags::FILE_APPEND, fio::Flags::FILE_TRUNCATE] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let scope = ExecutionScope::new();
            let flags = fio::Flags::PERM_READ | file_flags;
            ObjectRequest::new(flags, &fio::Options::default(), server_end.into())
                .handle(|req| validation.clone().open3(scope, VfsPath::dot(), flags, req));

            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::INVALID_ARGS, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open3_missing() {
        let (_env, validation) = TestEnv::with_base_blobs_and_blobfs_contents(
            HashSet::from([[0; 32].into()]),
            std::iter::empty(),
        )
        .await;

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>();
        let flags = fio::Flags::PERM_READ;
        ObjectRequest::new(flags, &fio::Options::default(), server_end.into()).handle(|req| {
            validation.clone().open3(
                ExecutionScope::new(),
                VfsPath::validate_and_split("missing").unwrap(),
                flags,
                req,
            )
        });

        assert_eq!(
            fuchsia_fs::file::read(&proxy).await.unwrap(),
            b"0000000000000000000000000000000000000000000000000000000000000000\n".to_vec()
        );
    }
}
