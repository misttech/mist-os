// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::partition::PartitionBackend;
use crate::partitions_directory::PartitionsDirectory;
use anyhow::{anyhow, Context as _, Error};
use block_client::{
    BlockClient as _, BufferSlice, MutableBufferSlice, RemoteBlockClient, VmoId, WriteOptions,
};
use block_server::async_interface::SessionManager;
use block_server::BlockServer;
use fs_management::filesystem::BlockConnector;

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A single partition in a GPT device.
pub struct GptPartition {
    block_client: RemoteBlockClient,
    block_range: Range<u64>,
    index: u32,
}

impl GptPartition {
    pub fn new(block_client: RemoteBlockClient, index: u32, block_range: Range<u64>) -> Arc<Self> {
        debug_assert!(block_range.end >= block_range.start);
        Arc::new(Self { block_client, block_range, index })
    }

    pub async fn terminate(&self) {
        if let Err(error) = self.block_client.close().await {
            tracing::warn!(?error, "Failed to close block client");
        }
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn block_size(&self) -> u32 {
        self.block_client.block_size()
    }

    pub fn block_count(&self) -> u64 {
        self.block_range.end - self.block_range.start
    }

    pub async fn attach_vmo(&self, vmo: &zx::Vmo) -> Result<VmoId, zx::Status> {
        self.block_client.attach_vmo(vmo).await
    }

    pub async fn detach_vmo(&self, vmoid: VmoId) -> Result<(), zx::Status> {
        self.block_client.detach_vmo(vmoid).await
    }

    pub async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo_id: &VmoId,
        vmo_offset: u64, // *bytes* not blocks
    ) -> Result<(), zx::Status> {
        let dev_offset = self
            .absolute_offset(device_block_offset, block_count)
            .map(|offset| offset * self.block_size() as u64)?;
        let buffer = MutableBufferSlice::new_with_vmo_id(
            vmo_id,
            vmo_offset,
            (block_count * self.block_size()) as u64,
        );
        self.block_client.read_at(buffer, dev_offset).await
    }

    pub async fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo_id: &VmoId,
        vmo_offset: u64, // *bytes* not blocks
        opts: WriteOptions,
    ) -> Result<(), zx::Status> {
        let dev_offset = self
            .absolute_offset(device_block_offset, block_count)
            .map(|offset| offset * self.block_size() as u64)?;
        let buffer = BufferSlice::new_with_vmo_id(
            vmo_id,
            vmo_offset,
            (block_count * self.block_size()) as u64,
        );
        self.block_client.write_at_with_opts(buffer, dev_offset, opts).await
    }

    pub async fn flush(&self) -> Result<(), zx::Status> {
        self.block_client.flush().await
    }

    pub async fn trim(
        &self,
        mut device_block_offset: u64,
        block_count: u32,
    ) -> Result<(), zx::Status> {
        device_block_offset = self.absolute_offset(device_block_offset, block_count)?;
        self.block_client.trim(device_block_offset..device_block_offset + block_count as u64).await
    }

    // Converts a relative range specified by [offset, offset+len) into an absolute offset in the
    // GPT device, performing bounds checking within the partition.  Returns ZX_ERR_OUT_OF_RANGE for
    // an invalid offset/len.
    fn absolute_offset(&self, mut offset: u64, len: u32) -> Result<u64, zx::Status> {
        offset = offset.checked_add(self.block_range.start).ok_or(zx::Status::OUT_OF_RANGE)?;
        let end = offset.checked_add(len as u64).ok_or(zx::Status::OUT_OF_RANGE)?;
        if end > self.block_range.end {
            Err(zx::Status::OUT_OF_RANGE)
        } else {
            Ok(offset)
        }
    }
}

fn convert_partition_info(
    info: &gpt::PartitionInfo,
    block_size: u32,
) -> block_server::PartitionInfo {
    block_server::PartitionInfo {
        block_count: info.num_blocks,
        block_size,
        type_guid: info.type_guid.to_bytes(),
        instance_guid: info.instance_guid.to_bytes(),
        name: info.label.clone(),
    }
}

struct Inner {
    gpt: gpt::GptManager,
    partitions: BTreeMap<u32, Arc<BlockServer<SessionManager<PartitionBackend>>>>,
    // Exposes all partitions for discovery by other components.  Should be kept in sync with
    // `partitions`.
    partitions_dir: PartitionsDirectory,
}

/// Runs a GPT device.
pub struct GptManager {
    block_size: u32,
    block_count: u64,
    inner: Mutex<Inner>,
    shutdown: AtomicBool,
}

impl std::fmt::Debug for GptManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("GptManager")
            .field("block_size", &self.block_size)
            .field("block_count", &self.block_count)
            .field("metadata", &self.inner.lock().unwrap().gpt)
            .finish()
    }
}

impl GptManager {
    pub async fn new(
        block_connector: Arc<dyn BlockConnector>,
        partitions_dir: Arc<vfs::directory::immutable::Simple>,
    ) -> Result<Arc<Self>, Error> {
        tracing::info!("Binding to GPT");
        let block = block_connector.connect_block()?.into_proxy()?;
        let client = RemoteBlockClient::new(block).await?;
        let block_size = client.block_size();
        let block_count = client.block_count();
        let gpt = gpt::GptManager::open(client).await.context("Failed to load GPT")?;

        let mut inner = Inner {
            gpt,
            partitions: BTreeMap::new(),
            partitions_dir: PartitionsDirectory::new(partitions_dir),
        };
        tracing::info!("Bind to GPT OK, binding partitions");
        for (index, info) in inner.gpt.partitions().iter() {
            tracing::info!("GPT part {index}: {info:?}");
            let block = block_connector.connect_block()?.into_proxy()?;
            let block_client = RemoteBlockClient::new(block).await?;
            let block_server_info = convert_partition_info(&info, block_size);
            let partition = PartitionBackend::new(GptPartition::new(
                block_client,
                *index,
                info.start_block
                    ..info
                        .start_block
                        .checked_add(info.num_blocks)
                        .ok_or(anyhow!("Overflow in partition range"))?,
            ));
            let block_server = Arc::new(BlockServer::new(block_server_info, partition));
            inner
                .partitions_dir
                .add_entry(&format!("part-{}", index), Arc::downgrade(&block_server));
            inner.partitions.insert(*index, block_server);
        }
        tracing::info!("Starting all partitions OK!");
        Ok(Arc::new(Self {
            block_size,
            block_count,
            inner: Mutex::new(inner),
            shutdown: AtomicBool::new(false),
        }))
    }

    pub async fn shutdown(self: Arc<Self>) {
        tracing::info!("Shutting down gpt");
        let mut inner = self.inner.lock().unwrap();
        inner.partitions_dir.clear();
        inner.partitions.clear();
        self.shutdown.store(true, Ordering::Relaxed);
        tracing::info!("Shutting down gpt OK");
    }
}

impl Drop for GptManager {
    fn drop(&mut self) {
        assert!(self.shutdown.load(Ordering::Relaxed), "Did you forget to shutdown?");
    }
}

#[cfg(test)]
mod tests {
    use super::GptManager;
    use anyhow::Error;
    use block_client::{BlockClient as _, BufferSlice, MutableBufferSlice, RemoteBlockClient};
    use block_server::WriteOptions;
    use event_listener::{Event, EventListener};
    use fake_block_server::{FakeServer, FakeServerOptions};
    use fidl::endpoints::{create_proxy, create_request_stream, ClientEnd, Proxy as _};
    use fs_management::filesystem::BlockConnector;
    use gpt_testing::{format_gpt, PartitionDescriptor};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use vfs::directory::entry_container::Directory as _;
    use vfs::directory::immutable::simple;
    use vfs::execution_scope::ExecutionScope;
    use vfs::ObjectRequest;
    use {
        fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio,
        fuchsia_async as fasync, zx,
    };

    struct MockBlockConnector {
        server: Arc<FakeServer>,
        tasks: Mutex<Vec<fasync::Task<()>>>,
        shutdown: Mutex<Option<EventListener>>,
    }

    impl MockBlockConnector {
        async fn run_until_shutdown(&self) {
            if let Some(shutdown) = self.shutdown.lock().unwrap().take() {
                shutdown.await;
            }
            self.tasks.lock().unwrap().clear();
        }
    }

    impl BlockConnector for MockBlockConnector {
        fn connect_volume(&self) -> Result<ClientEnd<fvolume::VolumeMarker>, Error> {
            let (client, server_end) = fidl::endpoints::create_endpoints::<fvolume::VolumeMarker>();
            let server = self.server.clone();
            let task = fasync::Task::spawn(async move {
                server.serve(server_end.into_stream().unwrap()).await.unwrap();
            });
            self.tasks.lock().unwrap().push(task);
            Ok(client)
        }
    }

    fn setup(
        vmo: zx::Vmo,
        block_size: u32,
    ) -> (Arc<MockBlockConnector>, Event, Arc<vfs::directory::immutable::Simple>) {
        let shutdown_event = Event::new();
        let server = Arc::new(FakeServer::from_vmo(block_size, vmo));
        (
            Arc::new(MockBlockConnector {
                server,
                tasks: Mutex::new(vec![]),
                shutdown: Mutex::new(Some(shutdown_event.listen())),
            }),
            shutdown_event,
            vfs::directory::immutable::simple(),
        )
    }

    #[fuchsia::test]
    async fn load_unformatted_gpt() {
        let vmo = zx::Vmo::create(4096).unwrap();
        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                GptManager::new(block_device_clone, partitions_dir)
                    .await
                    .expect_err("load should fail");
                shutdown.notify(usize::MAX);
            },
        );
    }

    #[fuchsia::test]
    async fn load_formatted_empty_gpt() {
        let vmo = zx::Vmo::create(4096).unwrap();
        format_gpt(&vmo, 512, vec![]);
        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                let runner = GptManager::new(block_device_clone, partitions_dir)
                    .await
                    .expect("load should succeed");
                runner.shutdown().await;
                shutdown.notify(usize::MAX);
            },
        );
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_one_partition() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part";

        let vmo = zx::Vmo::create(4096).unwrap();
        format_gpt(
            &vmo,
            512,
            vec![PartitionDescriptor {
                label: PART_NAME.to_string(),
                type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
            }],
        );
        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                let partitions_dir_clone = partitions_dir.clone();
                let runner = GptManager::new(block_device_clone, partitions_dir_clone)
                    .await
                    .expect("load should succeed");
                partitions_dir.get_entry("part-0").expect("No entry found");
                partitions_dir.get_entry("part-1").map(|_| ()).expect_err("Extra entry found");
                runner.shutdown().await;
                shutdown.notify(usize::MAX);
            },
        );
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_two_partitions() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(4096).unwrap();
        format_gpt(
            &vmo,
            512,
            vec![
                PartitionDescriptor {
                    label: PART_1_NAME.to_string(),
                    type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                    instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                },
                PartitionDescriptor {
                    label: PART_2_NAME.to_string(),
                    type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                    instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_2_GUID),
                    start_block: 4,
                    num_blocks: 1,
                },
            ],
        );
        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                let partitions_dir_clone = partitions_dir.clone();
                let runner = GptManager::new(block_device_clone, partitions_dir_clone)
                    .await
                    .expect("load should succeed");
                partitions_dir.get_entry("part-0").expect("No entry found");
                partitions_dir.get_entry("part-1").expect("No entry found");
                partitions_dir.get_entry("part-2").map(|_| ()).expect_err("Extra entry found");
                runner.shutdown().await;
                shutdown.notify(usize::MAX);
            },
        );
    }

    #[fuchsia::test]
    async fn partition_io() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_GUID: [u8; 16] = [2u8; 16];
        const PART_NAME: &str = "part";

        let vmo = zx::Vmo::create(4096).unwrap();
        let vmo_child = vmo.create_child(zx::VmoChildOptions::SLICE, 0, 4096).unwrap();
        format_gpt(
            &vmo,
            512,
            vec![PartitionDescriptor {
                label: PART_NAME.to_string(),
                type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_GUID),
                start_block: 4,
                num_blocks: 1,
            }],
        );
        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                let partitions_dir_clone = partitions_dir.clone();
                let runner = GptManager::new(block_device_clone, partitions_dir_clone)
                    .await
                    .expect("load should succeed");

                let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>()
                    .expect("Failed to create connection endpoints");
                let flags = fio::Flags::PERM_CONNECT;
                let options = fio::Options::default();
                let scope = vfs::execution_scope::ExecutionScope::new();
                partitions_dir
                    .clone()
                    .open3(
                        scope.clone(),
                        vfs::path::Path::dot(),
                        flags.clone(),
                        &mut ObjectRequest::new3(flags, &options, server_end.into_channel().into()),
                    )
                    .unwrap();
                let dir = fio::DirectoryProxy::new(proxy.into_channel().unwrap());
                let block = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                    fvolume::VolumeMarker,
                >(&dir, "part-0/block")
                .expect("Failed to open block service");
                let client =
                    RemoteBlockClient::new(block).await.expect("Failed to create block client");

                assert_eq!(client.block_count(), 1);
                assert_eq!(client.block_size(), 512);

                let buf = vec![0xabu8; 512];
                client.write_at(BufferSlice::Memory(&buf[..]), 0).await.expect("write_at failed");
                client
                    .write_at(BufferSlice::Memory(&buf[..]), 512)
                    .await
                    .expect_err("write_at should fail when writing past partition end");
                let mut buf2 = vec![0u8; 512];
                client
                    .read_at(MutableBufferSlice::Memory(&mut buf2[..]), 0)
                    .await
                    .expect("read_at failed");
                assert_eq!(buf, buf2);
                client
                    .read_at(MutableBufferSlice::Memory(&mut buf2[..]), 512)
                    .await
                    .expect_err("read_at should fail when reading past partition end");
                runner.shutdown().await;
                shutdown.notify(usize::MAX);
            },
        );

        // Ensure writes persisted to the partition.
        let mut buf = vec![0u8; 512];
        vmo_child.read(&mut buf[..], 2048).unwrap();
        assert_eq!(&buf[..], &[0xabu8; 512]);
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_invalid_primary_header() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(4096).unwrap();
        format_gpt(
            &vmo,
            512,
            vec![
                PartitionDescriptor {
                    label: PART_1_NAME.to_string(),
                    type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                    instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                },
                PartitionDescriptor {
                    label: PART_2_NAME.to_string(),
                    type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                    instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_2_GUID),
                    start_block: 4,
                    num_blocks: 1,
                },
            ],
        );
        // Clobber the primary header.  The backup should allow the GPT to be used.
        vmo.write(&[0xffu8; 512], 512).unwrap();

        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                let runner = GptManager::new(block_device_clone, partitions_dir.clone())
                    .await
                    .expect("load should succeed");
                partitions_dir.get_entry("part-0").expect("No entry found");
                partitions_dir.get_entry("part-1").expect("No entry found");
                runner.shutdown().await;
                shutdown.notify(usize::MAX);
            },
        );
    }

    #[fuchsia::test]
    async fn load_formatted_gpt_with_invalid_primary_partition_table() {
        const PART_TYPE_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_1_GUID: [u8; 16] = [2u8; 16];
        const PART_INSTANCE_2_GUID: [u8; 16] = [3u8; 16];
        const PART_1_NAME: &str = "part1";
        const PART_2_NAME: &str = "part2";

        let vmo = zx::Vmo::create(4096).unwrap();
        format_gpt(
            &vmo,
            512,
            vec![
                PartitionDescriptor {
                    label: PART_1_NAME.to_string(),
                    type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                    instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_1_GUID),
                    start_block: 4,
                    num_blocks: 1,
                },
                PartitionDescriptor {
                    label: PART_2_NAME.to_string(),
                    type_guid: uuid::Uuid::from_bytes(PART_TYPE_GUID),
                    instance_guid: uuid::Uuid::from_bytes(PART_INSTANCE_2_GUID),
                    start_block: 4,
                    num_blocks: 1,
                },
            ],
        );
        // Clobber the primary partition table.  The backup should allow the GPT to be used.
        vmo.write(&[0xffu8; 512], 1024).unwrap();

        let (block_device, shutdown, partitions_dir) = setup(vmo, 512);
        let block_device_clone = block_device.clone() as Arc<dyn BlockConnector>;

        futures::join!(
            async {
                block_device.run_until_shutdown().await;
            },
            async {
                let runner = GptManager::new(block_device_clone, partitions_dir.clone())
                    .await
                    .expect("load should succeed");
                partitions_dir.get_entry("part-0").expect("No entry found");
                partitions_dir.get_entry("part-1").expect("No entry found");
                runner.shutdown().await;
                shutdown.notify(usize::MAX);
            },
        );
    }

    #[fuchsia::test]
    async fn test_force_access_passed_through() {
        const BLOCK_SIZE: u32 = 512;
        const BLOCK_COUNT: u64 = 1024;
        let vmo = zx::Vmo::create(BLOCK_COUNT * BLOCK_SIZE as u64).unwrap();

        format_gpt(
            &vmo,
            BLOCK_SIZE,
            vec![PartitionDescriptor {
                label: "foo".to_string(),
                type_guid: uuid::Uuid::from_bytes([1; 16]),
                instance_guid: uuid::Uuid::from_bytes([2; 16]),
                start_block: 4,
                num_blocks: 1,
            }],
        );

        let expect_force_access = Arc::new(AtomicBool::new(false));

        struct Observer(Arc<AtomicBool>);

        impl fake_block_server::Observer for Observer {
            fn write(
                &self,
                _device_block_offset: u64,
                _block_count: u32,
                _vmo: &Arc<zx::Vmo>,
                _vmo_offset: u64,
                opts: WriteOptions,
            ) {
                assert_eq!(
                    opts.contains(WriteOptions::FORCE_ACCESS),
                    self.0.load(Ordering::Relaxed)
                );
            }
        }

        let server = Arc::new(
            FakeServerOptions {
                block_count: Some(BLOCK_COUNT),
                block_size: BLOCK_SIZE,
                vmo: Some(vmo),
                observer: Some(Box::new(Observer(expect_force_access.clone()))),
                ..Default::default()
            }
            .into(),
        );

        struct Server(Arc<FakeServer>);

        impl BlockConnector for Server {
            fn connect_volume(&self) -> Result<ClientEnd<fvolume::VolumeMarker>, Error> {
                let (client, stream) = create_request_stream().unwrap();
                let server = self.0.clone();
                fasync::Task::spawn(async move {
                    let _ = server.serve(stream).await;
                })
                .detach();
                Ok(client)
            }
        }

        let partitions_dir = simple();
        let manager =
            GptManager::new(Arc::new(Server(server)), partitions_dir.clone()).await.unwrap();

        let scope = ExecutionScope::new();
        let (client, server) = create_proxy::<fvolume::VolumeMarker>().unwrap();
        partitions_dir.open(
            scope.clone(),
            fio::OpenFlags::empty(),
            "part-0/block".try_into().unwrap(),
            server.into_channel().into(),
        );

        let client = RemoteBlockClient::new(client).await.unwrap();

        let buffer = vec![0; BLOCK_SIZE as usize];
        client.write_at(BufferSlice::Memory(&buffer), 0).await.unwrap();

        expect_force_access.store(true, Ordering::Relaxed);

        client
            .write_at_with_opts(BufferSlice::Memory(&buffer), 0, WriteOptions::FORCE_ACCESS)
            .await
            .unwrap();

        manager.shutdown().await;
    }
}
