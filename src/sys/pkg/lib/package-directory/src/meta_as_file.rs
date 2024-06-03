// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{root_dir::RootDir, u64_to_usize_safe, usize_to_u64_safe},
    async_trait::async_trait,
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::sync::Arc,
    vfs::{
        execution_scope::ExecutionScope,
        file::{FidlIoConnection, FileLike, FileOptions},
        immutable_attributes, ObjectRequestRef,
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

impl<S: crate::NonMetaStorage> FileLike for MetaAsFile<S> {
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        options: FileOptions,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), zx::Status> {
        if options.is_append {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        FidlIoConnection::spawn(scope, self, options, object_request)
    }
}

impl<S: crate::NonMetaStorage> vfs::node::IsDirectory for MetaAsFile<S> {
    fn is_directory(&self) -> bool {
        false
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

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, zx::Status> {
        Ok(immutable_attributes!(
            requested_attributes,
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
        futures::prelude::*,
        std::convert::TryInto as _,
        vfs::{
            directory::entry_container::Directory,
            file::{File, FileIo},
            node::Node,
            ToObjectRequest,
        },
    };

    struct TestEnv {
        _blobfs_fake: FakeBlobfs,
    }

    impl TestEnv {
        async fn new() -> (Self, Arc<RootDir<blobfs::Client>>) {
            let pkg = PackageBuilder::new("pkg").build().await.unwrap();
            let (metafar_blob, _) = pkg.contents();
            let (blobfs_fake, blobfs_client) = FakeBlobfs::new();
            blobfs_fake.add_blob(metafar_blob.merkle, metafar_blob.contents);
            let root_dir = RootDir::new(blobfs_client, metafar_blob.merkle).await.unwrap();
            (Self { _blobfs_fake: blobfs_fake }, root_dir)
        }
    }

    fn get_meta_as_file(root_dir: Arc<RootDir<blobfs::Client>>) -> Arc<MetaAsFile<blobfs::Client>> {
        MetaAsFile::new(root_dir)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_size() {
        let (_env, root_dir) = TestEnv::new().await;
        assert_eq!(get_meta_as_file(root_dir).file_size(), 64);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_rejects_disallowed_flags() {
        let (_env, root_dir) = TestEnv::new().await;

        for forbidden_flag in [
            fio::OpenFlags::RIGHT_WRITABLE,   // ACCESS_DENIED
            fio::OpenFlags::RIGHT_EXECUTABLE, // ACCESS_DENIED
            fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT, // NOT_SUPPORTED
            fio::OpenFlags::TRUNCATE,         // INVALID_ARGS without RIGHT_WRITABLE
            fio::OpenFlags::APPEND,           // NOT_SUPPORTED
        ] {
            let (proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            root_dir.clone().open(
                ExecutionScope::new(),
                fio::OpenFlags::NOT_DIRECTORY | fio::OpenFlags::DESCRIBE | forbidden_flag,
                "meta".try_into().unwrap(),
                server_end.into_channel().into(),
            );

            assert_matches!(
                proxy.take_event_stream().next().await,
                Some(Ok(fio::DirectoryEvent::OnOpen_{ s, info: None}))
                    if s == zx::Status::NOT_SUPPORTED.into_raw()
                        || s == zx::Status::ACCESS_DENIED.into_raw()
                        || s == zx::Status::INVALID_ARGS.into_raw(),
                "forbidden_flag: {forbidden_flag:?}",
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open_succeeds() {
        let (_env, root_dir) = TestEnv::new().await;
        let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
        let hash = root_dir.hash.to_string();

        root_dir.open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY,
            "meta".try_into().unwrap(),
            server_end.into_channel().into(),
        );

        assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), hash.as_bytes());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_open() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(get_meta_as_file(root_dir).open_file(&FileOptions::default()).await, Ok(()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_read_at_caps_offset() {
        let (_env, root_dir) = TestEnv::new().await;
        let meta_as_file = get_meta_as_file(root_dir);
        let mut buffer = vec![0u8];
        assert_eq!(
            meta_as_file
                .read_at(
                    (meta_as_file.root_dir.hash.to_string().as_bytes().len() + 1)
                        .try_into()
                        .unwrap(),
                    buffer.as_mut()
                )
                .await,
            Ok(0)
        );
        assert_eq!(buffer.as_slice(), &[0]);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_read_at_caps_count() {
        let (_env, root_dir) = TestEnv::new().await;
        let meta_as_file = get_meta_as_file(root_dir);
        let mut buffer = vec![0u8; 2];
        assert_eq!(
            meta_as_file
                .read_at(
                    (meta_as_file.root_dir.hash.to_string().as_bytes().len() - 1)
                        .try_into()
                        .unwrap(),
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
        let (_env, root_dir) = TestEnv::new().await;
        let meta_as_file = get_meta_as_file(root_dir);
        let content_len = meta_as_file.root_dir.hash.to_string().as_bytes().len();
        let mut buffer = vec![0u8; content_len];

        assert_eq!(meta_as_file.read_at(0, buffer.as_mut()).await, Ok(64));
        assert_eq!(buffer.as_slice(), meta_as_file.root_dir.hash.to_string().as_bytes());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_write_at() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            get_meta_as_file(root_dir).write_at(0, &[]).await,
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_append() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(get_meta_as_file(root_dir).append(&[]).await, Err(zx::Status::NOT_SUPPORTED));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_truncate() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(get_meta_as_file(root_dir).truncate(0).await, Err(zx::Status::NOT_SUPPORTED));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_backing_memory() {
        let (_env, root_dir) = TestEnv::new().await;
        let meta_as_file = get_meta_as_file(root_dir);

        for sharing_mode in
            [fio::VmoFlags::empty(), fio::VmoFlags::SHARED_BUFFER, fio::VmoFlags::PRIVATE_CLONE]
        {
            for flag in [fio::VmoFlags::empty(), fio::VmoFlags::READ] {
                assert_eq!(
                    meta_as_file.get_backing_memory(sharing_mode | flag).await.err().unwrap(),
                    zx::Status::NOT_SUPPORTED
                );
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_size() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(get_meta_as_file(root_dir).get_size().await, Ok(64));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_attrs() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            get_meta_as_file(root_dir).get_attrs().await,
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
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            get_meta_as_file(root_dir)
                .set_attrs(
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
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            get_meta_as_file(root_dir).sync(Default::default()).await,
            Err(zx::Status::NOT_SUPPORTED)
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn file_get_attributes() {
        let (_env, root_dir) = TestEnv::new().await;

        assert_eq!(
            get_meta_as_file(root_dir)
                .get_attributes(fio::NodeAttributesQuery::all())
                .await
                .unwrap(),
            immutable_attributes!(
                fio::NodeAttributesQuery::all(),
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
    async fn directory_entry_open2_succeeds() {
        let (_env, root_dir) = TestEnv::new().await;
        let hash = root_dir.hash.to_string();

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
        protocols
            .to_object_request(server_end)
            .handle(|req| root_dir.open2(scope, "meta".try_into().unwrap(), protocols, req));

        assert_eq!(fuchsia_fs::file::read(&proxy).await.unwrap(), hash.as_bytes());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_open_modes() {
        let (_env, root_dir) = TestEnv::new().await;

        for forbidden_open_mode in [vfs::CreationMode::Always, vfs::CreationMode::AllowExisting] {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::FileMarker>().unwrap();
            let scope = ExecutionScope::new();
            let protocols = fio::ConnectionProtocols::Node(fio::NodeOptions {
                flags: Some(fio::NodeFlags::GET_REPRESENTATION),
                mode: Some(forbidden_open_mode.into()),
                rights: Some(fio::Operations::READ_BYTES),
                protocols: Some(fio::NodeProtocols {
                    file: Some(fio::FileProtocolFlags::default()),
                    ..Default::default()
                }),
                ..Default::default()
            });
            protocols.to_object_request(server_end).handle(|req| {
                root_dir.clone().open2(scope, "meta".try_into().unwrap(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status, .. })
                    if status == zx::Status::NOT_SUPPORTED
                        || status == zx::Status::ACCESS_DENIED
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_rights() {
        let (_env, root_dir) = TestEnv::new().await;

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
                root_dir.clone().open2(scope, "meta".try_into().unwrap(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status: zx::Status::ACCESS_DENIED, .. })
            );
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn directory_entry_open2_rejects_forbidden_file_protocols() {
        let (_env, root_dir) = TestEnv::new().await;
        for forbidden_file_protocols in [
            fio::FileProtocolFlags::APPEND,   // NOT_SUPPORTED
            fio::FileProtocolFlags::TRUNCATE, // INVALID_ARGS without WRITE_BYTES rights
        ] {
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
                root_dir.clone().open2(scope, "meta".try_into().unwrap(), protocols, req)
            });
            assert_matches!(
                proxy.take_event_stream().try_next().await,
                Err(fidl::Error::ClientChannelClosed { status , .. })
                    if status == zx::Status::NOT_SUPPORTED || status == zx::Status::INVALID_ARGS
            );
        }
    }
}
