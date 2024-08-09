// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use block_server::async_interface::{Interface, SessionManager};
use block_server::{BlockServer, PartitionInfo};
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_fs_startup::{StartOptions, StartupMarker, StartupRequest, StartupRequestStream};
use fidl_fuchsia_fxfs::{
    MountOptions, VolumeRequest, VolumeRequestStream, VolumesMarker, VolumesRequest,
    VolumesRequestStream,
};
use fidl_fuchsia_hardware_block::BlockMarker;
use futures::future::try_join_all;
use futures::stream::TryStreamExt;
use remote_block_device::RemoteBlockClient;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use storage_device::block_device::BlockDevice;
use storage_device::{Device, DeviceHolder};
use tracing::{error, warn};
use uuid::Uuid;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use zerocopy::{FromBytes, FromZeroes, IntoBytes, NoCell};
use {fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio, fuchsia_zircon as zx};

// See //src/storage/fvm/format.h for a detailed description of the FVM format.

static MAGIC: u64 = 0x54524150204d5646;

#[repr(C)]
#[derive(Clone, Copy, FromBytes, FromZeroes, IntoBytes, NoCell)]
struct Header {
    magic: u64,
    major_version: u64,
    pslice_count: u64,
    slice_size: u64,
    fvm_partition_size: u64,
    vpartition_table_size: u64,
    allocation_table_size: u64,
    generation: u64,
    hash: [u8; 32],
    oldest_minor_version: u64,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, FromZeroes, IntoBytes, NoCell)]
struct PartitionEntry {
    type_guid: [u8; 16],
    guid: [u8; 16],
    slices: u32,
    flags: u32,
    name: [u8; 24],
}

impl PartitionEntry {
    fn is_allocated(&self) -> bool {
        self.slices > 0
    }

    fn name(&self) -> Cow<'_, str> {
        // Find the first NULL character.
        let end = self.name.iter().position(|c| *c == 0).unwrap_or(24);
        // TODO(https://fxbug.dev/357467643): Make sure names are unique and not empty.
        match std::str::from_utf8(&self.name[..end]) {
            Ok(name) => Cow::Borrowed(name),
            Err(_) => Cow::Owned(format!("{}", Uuid::from_slice(&self.guid).unwrap())),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, FromZeroes, IntoBytes, NoCell)]
struct SliceEntry(u64);

struct Fvm {
    // Which metadata slot the current metadata is using.
    _slot: u8,
    metadata: Metadata,
    device: DeviceHolder,
    mappings: HashMap<usize, Vec<Mapping>>,
}

#[derive(Debug)]
struct Mapping {
    logical_slice: u64,
    physical_slice: u64,
    slice_count: u64,
}

impl Mapping {
    fn end_slice(&self) -> u64 {
        self.logical_slice + self.slice_count
    }
}

struct Metadata {
    block_size: u32,

    // The `hash` field of the header is not necessarily up to date and must be recomputed before
    // writing.
    header: Header,

    partitions: Vec<PartitionEntry>,
    allocations: Vec<SliceEntry>,
}

impl Metadata {
    async fn read(header_block: &[u8], device: &dyn Device, offset: u64) -> Result<Self, Error> {
        let header =
            Header::ref_from_prefix(header_block).ok_or(anyhow!("Block size too small"))?;
        if header.magic != MAGIC {
            return Err(anyhow!("Magic mismatch"));
        }
        // Read the vpartition and allocation table.
        // TODO(https://fxbug.dev/357467643): Check sizes
        let bs = device.block_size() as u64;
        let allocation_size = header
            .pslice_count
            .checked_mul(8)
            .and_then(|n| n.checked_next_multiple_of(bs))
            .ok_or(anyhow!("Bad pslice_count"))? as usize;
        let part_table_size = header.vpartition_table_size as usize;
        let mut buffer = device.allocate_buffer(part_table_size + allocation_size).await;
        device.read(offset + bs, buffer.as_mut()).await?;

        // Check the hash.
        let mut hasher = Sha256::new();
        let mut header_copy = *header;
        header_copy.hash = [0; 32];
        hasher.update(header_copy.as_bytes());
        hasher.update(&header_block[std::mem::size_of::<Header>()..]);
        hasher.update(buffer.as_slice());

        if hasher.finalize().as_slice() != header.hash {
            return Err(anyhow!("Hash mismatch"));
        }

        // The first partition is unused.
        let partitions: Vec<_> = buffer.as_slice()
            [std::mem::size_of::<PartitionEntry>()..part_table_size]
            .chunks_exact(std::mem::size_of::<PartitionEntry>())
            .map(|e| PartitionEntry::read_from(e).unwrap())
            .collect();
        let allocations: Vec<_> = if allocation_size < std::mem::size_of::<SliceEntry>() {
            Vec::new()
        } else {
            // The first slice entry is unused.
            buffer.as_slice()[part_table_size + std::mem::size_of::<SliceEntry>()
                ..part_table_size + allocation_size]
                .chunks_exact(std::mem::size_of::<SliceEntry>())
                .map(|e| SliceEntry::read_from(e).unwrap())
                .collect()
        };

        Ok(Self { block_size: bs as u32, header: header_copy, partitions, allocations })
    }
}

impl Fvm {
    /// Opens the FVM device.
    pub async fn open(device: DeviceHolder) -> Result<Self, Error> {
        let bs = device.block_size();

        let mut metadata = Vec::new();
        {
            let mut header_block = device.allocate_buffer(bs as usize).await;
            device.read(0, header_block.as_mut()).await?;

            metadata.push(Metadata::read(header_block.as_slice(), device.as_ref(), 0).await);

            let header = Header::ref_from_prefix(header_block.as_slice())
                .ok_or(anyhow!("Block size too small"))?;
            // TODO(https://fxbug.dev/357467643): Check offset is sensible.
            let secondary_offset =
                bs as u64 + header.vpartition_table_size + header.allocation_table_size;
            device.read(secondary_offset, header_block.as_mut()).await?;

            metadata.push(
                Metadata::read(header_block.as_slice(), device.as_ref(), secondary_offset).await,
            );
        }

        let (slot, metadata) = Self::pick_metadata(metadata).ok_or_else(|| {
            warn!("No valid metadata");
            anyhow!("No valid metadata")
        })?;

        // Build the mappings.
        let mut mappings = HashMap::new();
        for (physical_slice, allocation) in metadata.allocations.iter().enumerate() {
            let mut partition_index = (allocation.0 & 0xffff) as usize;
            let slice = allocation.0 >> 16;
            if partition_index == 0 {
                // Entry is free.
                continue;
            }
            // Partition offsets are 1-based.
            partition_index -= 1;
            if partition_index >= metadata.partitions.len() {
                warn!("Slice entry partition out of range: 0x{:x?}", allocation.0);
                continue;
            }
            let partition = &metadata.partitions[partition_index];
            if !partition.is_allocated() {
                warn!("Slice entry points to free partition: 0x{:x?}", allocation.0);
                continue;
            }
            let mappings = mappings.entry(partition_index).or_insert_with(|| Vec::new());
            let mut bad_mapping = false;
            match mappings.binary_search_by(|m: &Mapping| m.logical_slice.cmp(&slice)) {
                Ok(_) => bad_mapping = true,
                Err(index) => {
                    let insert = if index > 0 {
                        // See if this can be merged with the previous entry.
                        let prev_mapping = &mut mappings[index - 1];
                        let end = prev_mapping.end_slice();
                        if end == slice {
                            prev_mapping.slice_count += 1;
                            false
                        } else if end < slice {
                            true
                        } else {
                            bad_mapping = true;
                            false
                        }
                    } else {
                        true
                    };
                    if insert {
                        mappings.insert(
                            index,
                            Mapping {
                                logical_slice: slice,
                                physical_slice: physical_slice as u64,
                                slice_count: 1,
                            },
                        );
                    }
                }
            };
            if bad_mapping {
                warn!("Duplicate slice entry: 0x{:x?}", allocation.0);
            }
        }

        Ok(Self { _slot: slot as u8, metadata, device, mappings })
    }

    fn pick_metadata(
        metadata: impl IntoIterator<Item = Result<Metadata, Error>>,
    ) -> Option<(usize, Metadata)> {
        metadata
            .into_iter()
            .enumerate()
            .filter_map(|(index, metadata)| match metadata {
                Ok(metadata) => Some((index, metadata)),
                Err(error) => {
                    warn!(?error, "Bad metadata {index}");
                    None
                }
            })
            .max_by_key(|(_index, metadata)| metadata.header.generation)
    }

    async fn read(
        &self,
        partition: usize,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        mut vmo_offset: u64,
    ) -> Result<(), Error> {
        let Some(mappings) = self.mappings.get(&partition) else {
            return Err(zx::Status::INTERNAL.into());
        };

        // TODO(https://fxbug.dev/357467643): Eliminate copying to improve performance.

        const BUFFER_SIZE: usize = 1048576;
        let mut buffer = self.device.allocate_buffer(BUFFER_SIZE).await;
        let mut offset = device_block_offset
            .checked_mul(self.metadata.block_size as u64)
            .ok_or(zx::Status::OUT_OF_RANGE)?;
        let mut total_len = (block_count as u64)
            .checked_mul(self.metadata.block_size as u64)
            .ok_or(zx::Status::OUT_OF_RANGE)?;
        let slice_size = self.metadata.header.slice_size;
        let data_start = (self.metadata.block_size as u64
            + self.metadata.header.vpartition_table_size
            + self.metadata.header.allocation_table_size)
            * 2;

        while total_len > 0 {
            let mut buffer_left = buffer.as_mut();
            let mut done = 0;
            let mut reads = Vec::new();
            while total_len > 0 {
                let slice = offset / slice_size;
                let index = match mappings.binary_search_by(|m| m.logical_slice.cmp(&slice)) {
                    Ok(index) => index,
                    Err(index) if index > 0 => index - 1,
                    _ => return Err(zx::Status::OUT_OF_RANGE.into()),
                };
                let mapping = &mappings[index];
                let end_slice = mapping.end_slice();
                if slice >= end_slice {
                    return Err(zx::Status::OUT_OF_RANGE.into());
                }
                let len = std::cmp::min(
                    (end_slice - slice) * slice_size,
                    std::cmp::min(total_len, buffer_left.len() as u64),
                ) as usize;
                let (buf, remaining) = buffer_left.split_at_mut(len);
                let physical_offset = data_start
                    + (mapping.physical_slice + (slice - mapping.logical_slice)) * slice_size;
                reads.push(self.device.read(physical_offset, buf));
                offset += len as u64;
                total_len -= len as u64;
                done += len;
                if remaining.is_empty() {
                    break;
                }
                buffer_left = remaining;
            }
            try_join_all(reads).await?;
            vmo.write(&buffer.as_slice()[..done], vmo_offset)?;
            vmo_offset += done as u64;
        }
        Ok(())
    }

    async fn write(
        &self,
        _partition: usize,
        _device_block_offset: u64,
        _block_count: u32,
        _vmo: &Arc<zx::Vmo>,
        _vmo_offset: u64,
    ) -> Result<(), Error> {
        todo!();
    }
}

/// Serves a multi-filesystem component that uses the FVM format.
struct Component {
    export_dir: Arc<vfs::directory::immutable::Simple>,
    scope: ExecutionScope,
    fvm: Mutex<Option<Arc<Fvm>>>,
    mounted: Mutex<HashMap<usize, Arc<BlockServer<SessionManager<PartitionInterface>>>>>,
}

impl Component {
    pub fn new() -> Self {
        Self {
            export_dir: vfs::directory::immutable::simple(),
            scope: ExecutionScope::new(),
            fvm: Mutex::default(),
            mounted: Mutex::default(),
        }
    }

    /// Serves an outgoing directory on `outgoing_dir`.
    pub async fn serve(self: &Arc<Self>, outgoing_dir: zx::Channel) -> Result<(), Error> {
        let svc_dir = vfs::directory::immutable::simple();
        self.export_dir.add_entry("svc", svc_dir.clone()).expect("Unable to create svc dir");

        let weak = Arc::downgrade(self);
        svc_dir.add_entry(
            StartupMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_startup_requests(requests).await;
                    }
                }
            }),
        )?;
        let weak = Arc::downgrade(self);
        svc_dir.add_entry(
            VolumesMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_volumes_requests(requests).await;
                    }
                }
            }),
        )?;
        self.export_dir.clone().open(
            self.scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::RIGHT_EXECUTABLE,
            Path::dot(),
            outgoing_dir.into(),
        );
        Ok(())
    }

    async fn handle_startup_requests(
        self: &Arc<Self>,
        mut stream: StartupRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                StartupRequest::Start { responder, device, options } => responder.send(
                    self.handle_start(device, options).await.map_err(|e| map_to_raw_status(e)),
                )?,
                StartupRequest::Format { responder, .. } => {
                    // Formatting FVM should be covered by C++ libraries.
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
                StartupRequest::Check { responder, .. } => {
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
            }
        }
        Ok(())
    }

    async fn handle_start(
        self: &Arc<Self>,
        device: ClientEnd<BlockMarker>,
        _options: StartOptions,
    ) -> Result<(), Error> {
        let client = RemoteBlockClient::new(device.into_proxy()?).await?;
        let device_holder = DeviceHolder::new(BlockDevice::new(Box::new(client), false).await?);
        let fvm = Arc::new(Fvm::open(device_holder).await?);

        let volumes_directory = vfs::directory::immutable::simple();
        for (index, partition) in fvm.metadata.partitions.iter().enumerate() {
            if partition.is_allocated() {
                let weak = Arc::downgrade(self);
                volumes_directory.add_entry(
                    partition.name(),
                    vfs::service::host(move |requests| {
                        let weak = weak.clone();
                        async move {
                            if let Some(me) = weak.upgrade() {
                                let _ = me.handle_volume_requests(requests, index).await;
                            }
                        }
                    }),
                )?;
            }
        }

        self.export_dir.add_entry_may_overwrite(
            "volumes",
            volumes_directory,
            /* overwrite: */ true,
        )?;

        *self.fvm.lock().unwrap() = Some(fvm);
        Ok(())
    }

    async fn handle_volumes_requests(&self, mut stream: VolumesRequestStream) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                VolumesRequest::Create { responder, .. } => {
                    // TODO(https://fxbug.dev/357467643): Implement this.
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
                VolumesRequest::Remove { responder, .. } => {
                    // TODO(https://fxbug.dev/357467643): Implement this.
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
            }
        }
        Ok(())
    }

    async fn handle_volume_requests(
        self: Arc<Self>,
        mut requests: VolumeRequestStream,
        partition: usize,
    ) -> Result<(), Error> {
        while let Some(request) = requests.try_next().await? {
            match request {
                VolumeRequest::Check { responder, .. } => {
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
                VolumeRequest::Mount { responder, outgoing_directory, options } => responder.send(
                    self.handle_mount(partition, outgoing_directory, options).map_err(|error| {
                        error!(?error, partition, "Failed to mount volume");
                        map_to_raw_status(error)
                    }),
                )?,
                VolumeRequest::SetLimit { responder, .. } => {
                    // TODO(https://fxbug.dev/357467643): Implement this.
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
                VolumeRequest::GetLimit { responder } => {
                    // TODO(https://fxbug.dev/357467643): Implement this.
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
            }
        }
        Ok(())
    }

    fn handle_mount(
        self: &Arc<Self>,
        partition_index: usize,
        server_end: ServerEnd<fio::DirectoryMarker>,
        _options: MountOptions,
    ) -> Result<(), Error> {
        let outgoing_dir = vfs::directory::immutable::simple();
        let svc_dir = vfs::directory::immutable::simple();
        outgoing_dir.add_entry("svc", svc_dir.clone())?;
        let weak = Arc::downgrade(self);
        svc_dir.add_entry(
            fvolume::VolumeMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_volume(partition_index, requests).await;
                    }
                }
            }),
        )?;
        let fvm = self.fvm.lock().unwrap();
        let fvm = fvm.as_ref().unwrap().clone();
        let partition = &fvm.metadata.partitions[partition_index];
        self.mounted.lock().unwrap().insert(
            partition_index,
            Arc::new(BlockServer::new(
                PartitionInfo {
                    block_count: u64::MAX,
                    block_size: fvm.metadata.block_size,
                    type_guid: partition.type_guid,
                    instance_guid: partition.guid,
                    name: partition.name().to_string(),
                },
                Arc::new(PartitionInterface { partition_index, fvm }),
            )),
        );
        outgoing_dir.open(
            self.scope.clone(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY
                | fio::OpenFlags::RIGHT_EXECUTABLE,
            Path::dot(),
            server_end.into_channel().into(),
        );
        Ok(())
    }

    async fn handle_volume(
        self: Arc<Self>,
        partition: usize,
        requests: fvolume::VolumeRequestStream,
    ) -> Result<(), Error> {
        let partition = self.mounted.lock().unwrap().get(&partition).unwrap().clone();
        partition.handle_requests(requests).await
    }
}

struct PartitionInterface {
    partition_index: usize,
    fvm: Arc<Fvm>,
}

impl Interface for PartitionInterface {
    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), zx::Status> {
        self.fvm
            .read(self.partition_index, device_block_offset, block_count, vmo, vmo_offset)
            .await
            .map_err(|e| map_to_status(e))
    }

    async fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), zx::Status> {
        self.fvm
            .write(self.partition_index, device_block_offset, block_count, vmo, vmo_offset)
            .await
            .map_err(|e| map_to_status(e))
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        todo!();
    }

    async fn trim(&self, _device_block_offset: u64, _block_count: u32) -> Result<(), zx::Status> {
        todo!();
    }
}

fn map_to_raw_status(e: Error) -> zx::sys::zx_status_t {
    map_to_status(e).into_raw()
}

fn map_to_status(error: anyhow::Error) -> zx::Status {
    if let Some(status) = error.root_cause().downcast_ref::<zx::Status>() {
        status.clone()
    } else {
        // Print the internal error if we re-map it because we will lose any context after this.
        warn!(?error, "Internal error");
        zx::Status::INTERNAL
    }
}

#[cfg(test)]
mod tests {
    use super::Component;
    use fake_block_server::FakeServer;
    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_fs_startup::{
        CompressionAlgorithm, EvictionPolicyOverride, StartOptions, StartupMarker,
    };
    use fidl_fuchsia_hardware_block::BlockMarker;
    use fuchsia_component::client::{
        connect_to_named_protocol_at_dir_root, connect_to_protocol_at_dir_svc,
    };
    use remote_block_device::{BlockClient, MutableBufferSlice, RemoteBlockClient};
    use std::sync::Arc;
    use {
        fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_hardware_block_volume as fvolume,
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
    };

    #[fuchsia::test]
    async fn test_golden() {
        let (block_client, block_server) =
            fidl::endpoints::create_request_stream::<BlockMarker>().unwrap();
        fasync::Task::spawn(async move {
            let contents = std::fs::read("/pkg/data/golden-fvm.blk").unwrap();
            let fake_server = FakeServer::new((contents.len() / 8192) as u64, 8192, &contents);
            let _ = fake_server.serve(block_server.cast_stream()).await;
        })
        .detach();
        let component = Arc::new(Component::new());
        let (outgoing_dir, server_end) =
            fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        component.serve(server_end.into_channel()).await.unwrap();
        let startup_proxy = connect_to_protocol_at_dir_svc::<StartupMarker>(&outgoing_dir).unwrap();

        startup_proxy
            .start(
                block_client.into_channel().into(),
                StartOptions {
                    read_only: false,
                    verbose: false,
                    fsck_after_every_transaction: false,
                    write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                    write_compression_level: 0,
                    cache_eviction_policy_override: EvictionPolicyOverride::None,
                    startup_profiling_seconds: 0,
                },
            )
            .await
            .expect("start failed (FIDL")
            .expect("start failed");

        // Mount the blobfs partition.
        let volume_proxy = connect_to_named_protocol_at_dir_root::<ffxfs::VolumeMarker>(
            &outgoing_dir,
            "volumes/blobfs",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");
        volume_proxy
            .mount(dir_server_end, ffxfs::MountOptions { crypt: None, as_blob: false })
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        // Look for blobfs's magic:
        let volume_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let client = RemoteBlockClient::new(volume_proxy).await.unwrap();
        let mut buf = vec![0; 8192];
        client.read_at(MutableBufferSlice::Memory(&mut buf), 0).await.unwrap();

        const BLOBFS_MAGIC: &[u8] = &[
            0x21, 0x4d, 0x69, 0x9e, 0x47, 0x53, 0x21, 0xac, 0x14, 0xd3, 0xd3, 0xd4, 0xd4, 0x00,
            0x50, 0x98,
        ];

        assert_eq!(&buf[..16], BLOBFS_MAGIC);

        // And check the backup super-block:
        let mut buf = vec![0; 8192];
        client.read_at(MutableBufferSlice::Memory(&mut buf), 8192).await.unwrap();
        assert_eq!(&buf[..16], BLOBFS_MAGIC);
    }
}
