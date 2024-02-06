// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{root_dir::RootDir, u64_to_usize_safe, usize_to_u64_safe},
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::sync::Arc,
    vfs::{
        attributes, directory::entry::EntryInfo, execution_scope::ExecutionScope,
        file::FidlIoConnection, path::Path as VfsPath, ObjectRequestRef, ProtocolsExt as _,
        ToObjectRequest,
    },
};

pub(crate) struct MetaAsFile<S: crate::NonMetaStorage> {
    root_dir: Arc<RootDir<S>>,
}

impl<S: crate::NonMetaStorage> MetaAsFile<S> {
    pub(crate) fn new(root_dir: Arc<RootDir<S>>) -> Arc<Self> {
        Arc::new(MetaAsFile { root_dir })
    }

    fn file_size(&self) -> u64 {
        crate::usize_to_u64_safe(self.root_dir.hash.to_string().as_bytes().len())
    }
}

impl<S: crate::NonMetaStorage> vfs::directory::entry::DirectoryEntry for MetaAsFile<S> {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: VfsPath,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            if !path.is_empty() {
                return Err(zx::Status::NOT_DIR);
            }

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

            object_request.spawn_connection(scope, self, flags, FidlIoConnection::create)
        });
    }

    fn open2(
        self: Arc<Self>,
        scope: ExecutionScope,
        path: VfsPath,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if !path.is_empty() {
            return Err(zx::Status::NOT_DIR);
        }

        match protocols.open_mode() {
            fio::OpenMode::OpenExisting => {}
            fio::OpenMode::AlwaysCreate | fio::OpenMode::MaybeCreate => {
                return Err(zx::Status::NOT_SUPPORTED);
            }
        }

        if let Some(rights) = protocols.rights() {
            if rights.intersects(fio::Operations::WRITE_BYTES)
                | rights.intersects(fio::Operations::EXECUTE)
            {
                return Err(zx::Status::NOT_SUPPORTED);
            }
        }

        if protocols.to_file_options()?.is_append || object_request.truncate {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        object_request.spawn_connection(scope, self, protocols, FidlIoConnection::create)
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
    }
}

#[async_trait]
impl<S: crate::NonMetaStorage> vfs::node::Node for MetaAsFile<S> {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, zx::Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_FILE
                | vfs::common::rights_to_posix_mode_bits(
                    true,  // read
                    true,  // write
                    false, // execute
                ),
            id: 1,
            content_size: self.file_size(),
            storage_size: self.file_size(),
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    // TODO(b/293947862): include new io2 attributes, e.g. change_time and access_time
    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::FILE,
                abilities: fio::Operations::GET_ATTRIBUTES,
                content_size: self.file_size(),
                storage_size: self.file_size(),
                link_count: 1,
                id: 1,
            }
        ))
    }
}

impl<S: crate::NonMetaStorage> vfs::file::File for MetaAsFile<S> {
    async fn open_file(&self, _options: &vfs::file::FileOptions) -> Result<(), zx::Status> {
        Ok(())
    }

    async fn truncate(&self, _length: u64) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn get_backing_memory(&self, _flags: fio::VmoFlags) -> Result<zx::Vmo, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn get_size(&self) -> Result<u64, zx::Status> {
        Ok(self.file_size())
    }

    async fn set_attrs(
        &self,
        _flags: fio::NodeAttributeFlags,
        _attrs: fio::NodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn update_attributes(
        &self,
        _attributes: fio::MutableNodeAttributes,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn sync(&self, _mode: vfs::file::SyncMode) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

impl<S: crate::NonMetaStorage> vfs::file::FileIo for MetaAsFile<S> {
    async fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<u64, zx::Status> {
        let contents = self.root_dir.hash.to_string();
        let offset = std::cmp::min(u64_to_usize_safe(offset), contents.len());
        let count = std::cmp::min(buffer.len(), contents.len() - offset);
        let () = buffer[..count].copy_from_slice(&contents.as_bytes()[offset..offset + count]);
        Ok(usize_to_u64_safe(count))
    }

    async fn write_at(&self, _offset: u64, _content: &[u8]) -> Result<u64, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    async fn append(&self, _content: &[u8]) -> Result<(u64, u64), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assert_matches::assert_matches,
        fuchsia_pkg_testing::{blobfs::Fake as FakeBlobfs, PackageBuilder},
        futures::{stream::StreamExt as _, TryStreamExt as _},
        std::convert::TryInto as _,
        vfs::{
            directory::entry::DirectoryEntry,
            file::{File, FileIo},
            node::Node,
            ProtocolsExt,
        },
    };

    struct TestEnv {
        _blobfs_fake: FakeBlobfs,
    }

    impl TestEnv {
        async fn new() -> (Self, Arc<MetaAsFile<blobfs::Client>>) {
            let pkg = PackageBuilder::new("pkg").build().await.unwrap();
            let (metafar_blob, _) = pkg.contents();
            let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
            blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
            let root_dir = RootDir::new(blobfs_client, metafar_blob.merkle).await.unwrap();
            let meta_as_file = MetaAsFile::new(root_dir);
            (Self { _blobfs_fake: blobfs_fake }, meta_as_file)
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_size() {
        let (_env, meta_as_file) = TestEnv::new().await;
        assert_eq!(meta_as_file.file_size(), 64);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_non_empty_path() {
        let (_env, meta_as_file) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();

        DirectoryEntry::open(
            meta_as_file,
            ExecutionScope::new(),
            fio::OpenFlags::DESCRIBE,
            VfsPath::validate_and_split("non-empty").unwrap(),
            server_end.into_channel().into(),
        );

        assert_matches!(
            proxy.take_event_stream().next().await,
            Some(Ok(fio::FileEvent::OnOpen_{ s, info: None}))
                if s == zx::Status::NOT_DIR.into_raw()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_disallowed_flags() {
        let (_env, meta_as_file) = TestEnv::new().await;

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
            DirectoryEntry::open(
                meta_as_file.clone(),
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
    async fn directory_entry_open_succeeds() {
        let (_env, meta_as_file) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let hash = meta_as_file.root_dir.hash.to_string();

        DirectoryEntry::open(
            meta_as_file,
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            VfsPath::dot(),
            server_end.into_channel().into(),
        );

        assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), hash.as_bytes());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_entry_info() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            DirectoryEntry::entry_info(meta_as_file.as_ref()),
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::File)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_open() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            File::open_file(
                meta_as_file.as_ref(),
                &fio::OpenFlags::empty().to_file_options().unwrap()
            )
            .await,
            Ok(())
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_read_at_caps_offset() {
        let (_env, meta_as_file) = TestEnv::new().await;
        let mut buffer = vec![0u8];
        assert_eq!(
            FileIo::read_at(
                meta_as_file.as_ref(),
                (meta_as_file.root_dir.hash.to_string().as_bytes().len() + 1).try_into().unwrap(),
                buffer.as_mut()
            )
            .await,
            Ok(0)
        );
        assert_eq!(buffer.as_slice(), &[0]);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_read_at_caps_count() {
        let (_env, meta_as_file) = TestEnv::new().await;
        let mut buffer = vec![0u8; 2];
        assert_eq!(
            FileIo::read_at(
                meta_as_file.as_ref(),
                (meta_as_file.root_dir.hash.to_string().as_bytes().len() - 1).try_into().unwrap(),
                buffer.as_mut()
            )
            .await,
            Ok(1)
        );
        assert_eq!(
            buffer.as_slice(),
            &[*meta_as_file.root_dir.hash.to_string().as_bytes().last().unwrap(), 0]
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_read_at() {
        let (_env, meta_as_file) = TestEnv::new().await;
        let content_len = meta_as_file.root_dir.hash.to_string().as_bytes().len();
        let mut buffer = vec![0u8; content_len];

        assert_eq!(FileIo::read_at(meta_as_file.as_ref(), 0, buffer.as_mut()).await, Ok(64));
        assert_eq!(buffer.as_slice(), meta_as_file.root_dir.hash.to_string().as_bytes());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_write_at() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            FileIo::write_at(meta_as_file.as_ref(), 0, &[]).await,
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_append() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            FileIo::append(meta_as_file.as_ref(), &[]).await,
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_truncate() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(File::truncate(meta_as_file.as_ref(), 0).await, Err(zx::Status::NOT_SUPPORTED));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_backing_memory() {
        let (_env, meta_as_file) = TestEnv::new().await;

        for sharing_mode in
            [fio::VmoFlags::empty(), fio::VmoFlags::SHARED_BUFFER, fio::VmoFlags::PRIVATE_CLONE]
        {
            for flag in [fio::VmoFlags::empty(), fio::VmoFlags::READ] {
                assert_eq!(
                    File::get_backing_memory(meta_as_file.as_ref(), sharing_mode | flag)
                        .await
                        .err()
                        .unwrap(),
                    zx::Status::NOT_SUPPORTED
                );
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_size() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(File::get_size(meta_as_file.as_ref()).await, Ok(64));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_attrs() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            Node::get_attrs(meta_as_file.as_ref()).await,
            Ok(fio::NodeAttributes {
                mode: fio::MODE_TYPE_FILE | 0o600,
                id: 1,
                content_size: 64,
                storage_size: 64,
                link_count: 1,
                creation_time: 0,
                modification_time: 0,
            })
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_set_attrs() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            File::set_attrs(
                meta_as_file.as_ref(),
                fio::NodeAttributeFlags::empty(),
                fio::NodeAttributes {
                    mode: 0,
                    id: 0,
                    content_size: 0,
                    storage_size: 0,
                    link_count: 0,
                    creation_time: 0,
                    modification_time: 0,
                },
            )
            .await,
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_sync() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            File::sync(meta_as_file.as_ref(), Default::default()).await,
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_attributes() {
        let (_env, meta_as_file) = TestEnv::new().await;

        assert_eq!(
            Node::get_attributes(meta_as_file.as_ref(), fio::NodeAttributesQuery::all())
                .await
                .unwrap(),
            attributes!(
                fio::NodeAttributesQuery::all(),
                Mutable {
                    creation_time: 0,
                    modification_time: 0,
                    mode: 0,
                    uid: 0,
                    gid: 0,
                    rdev: 0
                },
                Immutable {
                    protocols: fio::NodeProtocolKinds::FILE,
                    abilities: fio::Operations::GET_ATTRIBUTES,
                    content_size: 64,
                    storage_size: 64,
                    link_count: 1,
                    id: 1,
                }
            )
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_non_empty_path() {
        let (_env, meta_as_file) = TestEnv::new().await;

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let scope = ExecutionScope::new();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            rights: Some(fio::Operations::READ_BYTES),
            protocols: Some(fio::NodeProtocols {
                file: Some(fio::FileProtocolFlags::default()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let path = VfsPath::validate_and_split("non-empty").unwrap();
        protocols
            .to_object_request(server_end)
            .handle(|req| DirectoryEntry::open2(meta_as_file, scope, path, protocols, req));

        assert_matches!(
            proxy.take_event_stream().try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_DIR, .. })
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_succeeds() {
        let (_env, meta_as_file) = TestEnv::new().await;
        let hash = meta_as_file.root_dir.hash.to_string();

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let scope = ExecutionScope::new();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            rights: Some(fio::Operations::READ_BYTES),
            ..Default::default()
        });
        protocols.to_object_request(server_end).handle(|req| {
            DirectoryEntry::open2(meta_as_file, scope, VfsPath::dot(), protocols, req)
        });
        assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), hash.as_bytes());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_open_modes() {
        let (_env, meta_as_file) = TestEnv::new().await;

        for forbidden_open_mode in [fio::OpenMode::AlwaysCreate, fio::OpenMode::MaybeCreate] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                mode: Some(forbidden_open_mode),
                rights: Some(fio::Operations::READ_BYTES),
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                DirectoryEntry::open2(meta_as_file.clone(), scope, VfsPath::dot(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_rights() {
        let (_env, meta_as_file) = TestEnv::new().await;

        for forbidden_rights in [fio::Operations::WRITE_BYTES, fio::Operations::EXECUTE] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(forbidden_rights),
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                DirectoryEntry::open2(meta_as_file.clone(), scope, VfsPath::dot(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_file_protocols() {
        let (_env, meta_as_file) = TestEnv::new().await;
        for forbidden_file_protocols in
            [fio::FileProtocolFlags::APPEND, fio::FileProtocolFlags::TRUNCATE]
        {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                rights: Some(fio::Operations::READ_BYTES),
                protocols: Some(fio::NodeProtocols {
                    file: Some(forbidden_file_protocols),
                    ..Default::default()
                }),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                DirectoryEntry::open2(meta_as_file.clone(), scope, VfsPath::dot(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_SUPPORTED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_non_file() {
        let (_env, meta_as_file) = TestEnv::new().await;

        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let scope = ExecutionScope::new();
        let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
            rights: Some(fio::Operations::READ_BYTES),
            protocols: Some(fio::NodeProtocols {
                directory: Some(fio::DirectoryProtocolOptions::default()),
                ..Default::default()
            }),
            ..Default::default()
        });
        protocols.to_object_request(server_end).handle(|req| {
            DirectoryEntry::open2(meta_as_file.clone(), scope, VfsPath::dot(), protocols, req)
        });
        assert_matches!(
            proxy.take_event_stream().try_next().await,
            Err(fidl::Error::ClientChannelClosed { status: zx::Status::NOT_DIR, .. })
        );
    }
}
