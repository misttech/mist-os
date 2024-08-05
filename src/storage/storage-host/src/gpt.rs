// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::partition::PartitionBackend;
use crate::partitions_directory::PartitionsDirectory;
use anyhow::{anyhow, Context as _, Error};
use block_server::async_interface::SessionManager;
use block_server::BlockServer;
use fs_management::filesystem::BlockConnector;
use fuchsia_zircon as zx;
use remote_block_device::{
    BlockClient as _, BufferSlice, MutableBufferSlice, RemoteBlockClient, VmoId,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zerocopy::FromBytes as _;

pub mod format;

/// A single partition in a GPT device.
pub struct GptPartition {
    runner: Arc<GptManager>,
    block_client: RemoteBlockClient,
    info: PartitionInfo,
    index: u32,
}

impl GptPartition {
    pub fn new(
        runner: Arc<GptManager>,
        block_client: RemoteBlockClient,
        index: u32,
        info: PartitionInfo,
    ) -> Arc<Self> {
        Arc::new(Self { runner, block_client, info, index })
    }

    pub async fn terminate(&self) {
        if let Err(e) = self.block_client.close().await {
            tracing::warn!(?e, "Failed to close block client");
        }
    }

    pub fn info(&self) -> &PartitionInfo {
        &self.info
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn block_size(&self) -> u32 {
        self.runner.block_size
    }

    pub fn block_count(&self) -> u64 {
        self.info.num_blocks
    }

    pub async fn attach_vmo(&self, vmo: &zx::Vmo) -> Result<VmoId, zx::Status> {
        // TODO(https://fxbug.dev/355660116): Properly translate errors
        self.block_client.attach_vmo(vmo).await.map_err(|_| zx::Status::BAD_STATE)
    }

    pub async fn detach_vmo(&self, vmoid: VmoId) -> Result<(), zx::Status> {
        // TODO(https://fxbug.dev/355660116): Properly translate errors
        self.block_client.detach_vmo(vmoid).await.map_err(|_| zx::Status::BAD_STATE)
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
            .map(|offset| offset * self.block_size() as u64)
            .map_err(|_| zx::Status::OUT_OF_RANGE)?;
        let buffer = MutableBufferSlice::new_with_vmo_id(
            vmo_id,
            vmo_offset,
            (block_count * self.block_size()) as u64,
        );
        // TODO(https://fxbug.dev/355660116): Properly translate errors
        self.block_client.read_at(buffer, dev_offset).await.map_err(|e| {
            tracing::warn!(?e, "Failed to read");
            zx::Status::BAD_STATE
        })
    }

    pub async fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo_id: &VmoId,
        vmo_offset: u64, // *bytes* not blocks
    ) -> Result<(), zx::Status> {
        let dev_offset = self
            .absolute_offset(device_block_offset, block_count)
            .map(|offset| offset * self.block_size() as u64)
            .map_err(|_| zx::Status::OUT_OF_RANGE)?;
        let buffer = BufferSlice::new_with_vmo_id(
            vmo_id,
            vmo_offset,
            (block_count * self.block_size()) as u64,
        );
        // TODO(https://fxbug.dev/355660116): Properly translate errors
        self.block_client.write_at(buffer, dev_offset).await.map_err(|e| {
            tracing::warn!(?e, "Failed to write");
            zx::Status::BAD_STATE
        })
    }

    pub async fn flush(&self) -> Result<(), zx::Status> {
        // TODO(https://fxbug.dev/355660116): Properly translate errors
        self.block_client.flush().await.map_err(|_| zx::Status::BAD_STATE)
    }

    pub async fn trim(
        &self,
        mut device_block_offset: u64,
        block_count: u32,
    ) -> Result<(), zx::Status> {
        device_block_offset = self
            .absolute_offset(device_block_offset, block_count)
            .map_err(|_| zx::Status::OUT_OF_RANGE)?;
        // TODO(https://fxbug.dev/355660116): Properly translate errors
        self.block_client
            .trim(device_block_offset..device_block_offset + block_count as u64)
            .await
            .map_err(|_| zx::Status::BAD_STATE)
    }

    fn start_offset(&self) -> u64 {
        self.info.start_block
    }

    fn end_offset(&self) -> u64 {
        self.info.start_block + self.info.num_blocks
    }

    // Converts a relative range specified by [offset, offset+len) into an absolute offset in the
    // GPT device, performing bounds checking within the partition.
    fn absolute_offset(&self, mut offset: u64, len: u32) -> Result<u64, Error> {
        offset = offset.checked_add(self.start_offset()).ok_or(anyhow!("Overflow, {offset}"))?;
        let end = offset.checked_add(len as u64).ok_or(anyhow!("Overflow, {offset} + {len}"))?;
        if end > self.end_offset() {
            return Err(anyhow!("{offset} is out of range"));
        }
        Ok(offset)
    }
}

#[derive(Debug)]
pub struct PartitionInfo {
    pub label: String,
    pub type_guid: uuid::Uuid,
    pub instance_guid: uuid::Uuid,
    pub start_block: u64,
    pub num_blocks: u64,
}

impl PartitionInfo {
    fn as_block_server_info(&self, block_size: u32) -> block_server::PartitionInfo {
        block_server::PartitionInfo {
            block_count: self.num_blocks,
            block_size,
            type_guid: self.type_guid.as_bytes().to_owned(),
            instance_guid: self.instance_guid.as_bytes().to_owned(),
            name: self.label.clone(),
        }
    }

    fn from_entry(entry: &format::PartitionTableEntry) -> Result<Self, Error> {
        Ok(Self {
            label: String::from_utf16(&entry.name)?.trim_end_matches('\0').to_owned(),
            type_guid: uuid::Uuid::from_bytes_le(entry.type_guid),
            instance_guid: uuid::Uuid::from_bytes_le(entry.instance_guid),
            start_block: entry.first_lba,
            num_blocks: entry.last_lba.checked_sub(entry.first_lba).unwrap(),
        })
    }
}

/// Runs a GPT device.
pub struct GptManager {
    block_connector: Arc<dyn BlockConnector>,
    block_size: u32,
    block_count: u64,
    // Exposes all partitions for discovery by other components.
    partitions_dir: PartitionsDirectory,
    partitions: Mutex<BTreeMap<u32, Arc<BlockServer<SessionManager<PartitionBackend>>>>>,
    shutdown: AtomicBool,
}

impl std::fmt::Debug for GptManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("GptManager")
            .field("block_size", &self.block_size)
            .field("block_count", &self.block_count)
            .field("partitions_dir", &self.partitions_dir)
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
        let partition_table =
            Self::load_partitions(client).await.context("Failed to load partition table")?;

        let this = Arc::new(Self {
            block_connector,
            block_size,
            block_count,
            partitions: Default::default(),
            partitions_dir: PartitionsDirectory::new(partitions_dir),
            shutdown: AtomicBool::new(false),
        });
        tracing::info!("Bind to GPT OK, binding partitions");
        // TODO(https://fxbug.dev/339491886): Think about handling errors during startup.  (Avoid
        // leaving a half-initialized service.)
        {
            for (index, info) in partition_table.into_iter() {
                tracing::info!("GPT part {index}: {info:?}");
                let block_client =
                    this.create_session().await.context("Failed to establish new block session")?;
                let block_server_info = info.as_block_server_info(block_size);
                let partition = PartitionBackend::new(GptPartition::new(
                    this.clone(),
                    block_client,
                    index,
                    info,
                ));
                let block_server = Arc::new(BlockServer::new(block_server_info, partition));
                this.partitions_dir
                    .add_entry(&format!("part-{}", index), Arc::downgrade(&block_server));
                this.partitions.lock().unwrap().insert(index, block_server);
            }
        }
        tracing::info!("Starting all partitions OK!");
        Ok(this)
    }

    pub async fn shutdown(self: Arc<Self>) {
        tracing::info!("Shutting down gpt");
        self.partitions_dir.clear();
        self.partitions.lock().unwrap().clear();
        self.shutdown.store(true, Ordering::Relaxed);
        tracing::info!("Shutting down gpt OK");
    }

    async fn create_session(&self) -> Result<RemoteBlockClient, Error> {
        let block = self.block_connector.connect_block()?.into_proxy()?;
        RemoteBlockClient::new(block).await
    }

    async fn load_partitions(
        client: RemoteBlockClient,
    ) -> Result<BTreeMap<u32, PartitionInfo>, Error> {
        let bs = client.block_size() as usize;
        let mut header_block = vec![0u8; bs];
        // First, read the primary and secondary headers.  Note the primary header is at block 1,
        // not block 0 (which is a protective MBR header).
        client
            .read_at(MutableBufferSlice::Memory(&mut header_block[..]), bs as u64)
            .await
            .context("Read primary header")?;
        let header = format::Header::ref_from_prefix(&header_block[..])
            .ok_or(anyhow!("Failed to parse primary header"))?;

        // TODO(https://fxbug.dev/339491886): Should we best-effort look at secondary header?
        header.ensure_integrity().context("GPT primary header invalid!")?;

        // TODO(https://fxbug.dev/339491886): Do we need to read in chunks?
        let partition_table_size = ((header.num_parts * header.part_size) as usize)
            .checked_next_multiple_of(bs)
            .ok_or(anyhow!("Overflow when rounding up partition table size "))?;
        let mut partition_table_blocks = vec![0u8; partition_table_size];
        client
            .read_at(MutableBufferSlice::Memory(&mut partition_table_blocks[..]), 2 * bs as u64)
            .await
            .context("Failed to read partition table")?;

        let mut partition_table = BTreeMap::new();
        for i in 0..header.num_parts as usize {
            let entry_raw = &partition_table_blocks
                [i * header.part_size as usize..(i + 1) * header.part_size as usize];
            let entry = format::PartitionTableEntry::ref_from_prefix(entry_raw)
                .ok_or(anyhow!("Failed to parse partition {i}"))?;
            if entry.is_empty() {
                continue;
            }
            entry.ensure_integrity().context("GPT partition table entry invalid!")?;

            partition_table.insert(i as u32, PartitionInfo::from_entry(entry)?);
        }
        Ok(partition_table)
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
    use crate::gpt::format::testing::{format_gpt, PartitionDescriptor};
    use anyhow::Error;
    use event_listener::{Event, EventListener};
    use fake_block_server::FakeServer;
    use fidl::endpoints::{ClientEnd, Proxy as _};
    use fs_management::filesystem::BlockConnector;
    use remote_block_device::{
        BlockClient as _, BufferSlice, MutableBufferSlice, RemoteBlockClient,
    };
    use std::sync::{Arc, Mutex};
    use vfs::directory::entry_container::Directory as _;
    use vfs::ObjectRequest;
    use {
        fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio,
        fuchsia_async as fasync, fuchsia_zircon as zx,
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
}
