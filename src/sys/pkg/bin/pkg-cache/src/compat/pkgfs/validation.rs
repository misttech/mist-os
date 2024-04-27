// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::{
        collections::{BTreeMap, HashSet},
        sync::Arc,
    },
    tracing::{error, info},
    vfs::{
        directory::{
            entry::{EntryInfo, OpenRequest},
            immutable::connection::ImmutableConnection,
            traversal_position::TraversalPosition,
        },
        execution_scope::ExecutionScope,
        immutable_attributes,
        path::Path as VfsPath,
        ToObjectRequest,
    },
};

#[cfg(feature = "supports_open2")]
use vfs::{ObjectRequestRef, ProtocolsExt as _};

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

        let mut missing = match self.blobfs.filter_to_missing_blobs(&self.base_blobs).await {
            Ok(missing) => missing,
            Err(e) => {
                error!(
                    "failed to determine blobfs contents, behaving as if empty: {:#}",
                    anyhow::anyhow!(e)
                );
                self.base_blobs.clone()
            }
        }
        .into_iter()
        .collect::<Vec<_>>();
        missing.sort();

        if missing.is_empty() {
            info!(total = self.base_blobs.len(), "all base package blobs were found");
        } else {
            error!(total = missing.len(), "base package blobs are missing");
        }

        missing.into_iter().map(|hash| format!("{hash}\n")).collect::<String>().into_bytes()
    }
}

impl vfs::directory::entry::DirectoryEntry for Validation {
    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
    }

    fn open_entry(self: Arc<Self>, request: OpenRequest<'_>) -> Result<(), zx::Status> {
        request.open_dir(self)
    }
}

#[async_trait]
impl vfs::node::Node for Validation {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_DIRECTORY,
            id: 1,
            content_size: 1,
            storage_size: 1,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

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
                link_count: 1,
                id: 1,
            }
        ))
    }
}

#[async_trait]
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
            let () = scope.clone().spawn(async move {
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

    #[cfg(feature = "supports_open2")]
    fn open2(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: VfsPath,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if path.is_empty() {
            if protocols.creation_mode() != vfs::CreationMode::Never {
                return Err(zx::Status::NOT_SUPPORTED);
            }

            if let Some(rights) = protocols.rights() {
                if rights.intersects(fio::Operations::WRITE_BYTES)
                    | rights.intersects(fio::Operations::EXECUTE)
                {
                    return Err(zx::Status::NOT_SUPPORTED);
                }
            }

            // Note that `ImmutableConnection::create` will check that protocols contain
            // directory-only protocols.
            return object_request.spawn_connection(
                scope,
                self,
                protocols,
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
                        &protocols,
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
        super::read_dirents(
            &BTreeMap::from([("missing".to_string(), super::DirentType::File)]),
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
    use {
        super::*,
        assert_matches::assert_matches,
        blobfs_ramdisk::BlobfsRamdisk,
        futures::prelude::*,
        std::convert::TryInto as _,
        vfs::{
            directory::{entry::DirectoryEntry, entry_container::Directory},
            node::Node,
        },
    };

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
    async fn directory_entry_open_unsets_posix_flags() {
        let (_env, validation) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

        validation.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::POSIX_WRITABLE
                | fio::OpenFlags::POSIX_EXECUTABLE,
            VfsPath::dot(),
            server_end.into_channel().into(),
        );

        let (status, flags) = proxy.get_flags().await.unwrap();
        let () = zx::Status::ok(status).unwrap();
        assert_eq!(flags, fio::OpenFlags::RIGHT_READABLE);
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
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
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
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

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

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
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
            DirectoryEntry::entry_info(validation.as_ref()),
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Directory)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_read_dirents() {
        let (_env, validation) = TestEnv::new().await;

        let (pos, sealed) = validation
            .read_dirents(
                &TraversalPosition::Start,
                Box::new(crate::compat::pkgfs::testing::FakeSink::new(3)),
            )
            .await
            .expect("read_dirents failed");
        assert_eq!(
            crate::compat::pkgfs::testing::FakeSink::from_sealed(sealed).entries,
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
    async fn directory_get_attrs() {
        let (_env, validation) = TestEnv::new().await;

        assert_eq!(
            validation.get_attrs().await.unwrap(),
            fio::NodeAttributes {
                mode: fio::MODE_TYPE_DIRECTORY,
                id: 1,
                content_size: 1,
                storage_size: 1,
                link_count: 1,
                creation_time: 0,
                modification_time: 0,
            }
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
                    link_count: 1,
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

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_self() {
        let (_env, validation) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();

        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            rights: Some(fio::Operations::READ_BYTES),
            ..Default::default()
        });
        protocols
            .to_object_request(server_end)
            .handle(|req| validation.open2(ExecutionScope::new(), VfsPath::dot(), protocols, req));

        assert_eq!(
            fuchsia_fs::directory::readdir(&proxy).await.unwrap(),
            vec![fuchsia_fs::directory::DirEntry {
                name: "missing".to_string(),
                kind: fuchsia_fs::directory::DirentKind::File
            }]
        );
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_open_modes() {
        let (_env, validation) = TestEnv::new().await;

        for forbidden_open_mode in [vfs::CreationMode::Always, vfs::CreationMode::AllowExisting] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                mode: Some(forbidden_open_mode.into()),
                rights: Some(fio::Operations::READ_BYTES),
                ..Default::default()
            });
            protocols
                .to_object_request(server_end)
                .handle(|req| validation.clone().open2(scope, VfsPath::dot(), protocols, req));
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_rights() {
        let (_env, validation) = TestEnv::new().await;

        for forbidden_rights in [fio::Operations::WRITE_BYTES, fio::Operations::EXECUTE] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(forbidden_rights),
                ..Default::default()
            });
            protocols
                .to_object_request(server_end)
                .handle(|req| validation.clone().open2(scope, VfsPath::dot(), protocols, req));
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_file_protocols() {
        let (_env, validation) = TestEnv::new().await;

        for file_protocols in [
            fio::FileProtocolFlags::default(),
            fio::FileProtocolFlags::APPEND,
            fio::FileProtocolFlags::TRUNCATE,
        ] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                protocols: Some(fio::NodeProtocols {
                    file: Some(file_protocols),
                    ..Default::default()
                }),
                ..Default::default()
            });
            protocols
                .to_object_request(server_end)
                .handle(|req| validation.clone().open2(scope, VfsPath::dot(), protocols, req));
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_FILE, .. })
            );
        }
    }

    #[cfg(feature = "supports_open2")]
    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_missing() {
        let (_env, validation) = TestEnv::with_base_blobs_and_blobfs_contents(
            HashSet::from([[0; 32].into()]),
            std::iter::empty(),
        )
        .await;

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            rights: Some(fio::Operations::READ_BYTES),
            ..Default::default()
        });
        protocols.to_object_request(server_end).handle(|req| {
            validation.clone().open2(
                ExecutionScope::new(),
                VfsPath::validate_and_split("missing").unwrap(),
                protocols,
                req,
            )
        });

        assert_eq!(
            fuchsia_fs::file::read(&proxy).await.unwrap(),
            b"0000000000000000000000000000000000000000000000000000000000000000\n".to_vec()
        );
    }
}
