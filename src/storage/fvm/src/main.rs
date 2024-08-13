// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, ensure, Context, Error};
use block_client::RemoteBlockClient;
use block_server::async_interface::{Interface, SessionManager};
use block_server::{BlockServer, PartitionInfo};
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_fs_startup::{
    CreateOptions, MountOptions, StartOptions, StartupMarker, StartupRequest, StartupRequestStream,
    VolumeRequest, VolumeRequestStream, VolumesMarker, VolumesRequest, VolumesRequestStream,
};
use fidl_fuchsia_hardware_block::BlockMarker;
use futures::future::try_join_all;
use futures::stream::TryStreamExt;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Formatter;
use std::future::Future;
use std::sync::{Arc, Mutex};
use storage_device::block_device::BlockDevice;
use storage_device::buffer::MutableBufferRef;
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
const BLOCK_SIZE: u64 = 8192;

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

impl Header {
    fn allocation_size(&self) -> Result<usize, Error> {
        self.pslice_count
            .checked_mul(std::mem::size_of::<SliceEntry>() as u64)
            .and_then(|n| n.checked_next_multiple_of(BLOCK_SIZE))
            .ok_or(anyhow!("Bad pslice_count"))
            .map(|n| n as usize)
    }

    /// Returns the offset of the second copy of the metadata.
    fn offset_for_slot(&self, slot: u8) -> u64 {
        match slot {
            0 => 0,
            1 => BLOCK_SIZE + self.vpartition_table_size + self.allocation_table_size,
            _ => unreachable!(),
        }
    }

    /// Returns the offset where the data starts.
    fn data_start(&self) -> u64 {
        (BLOCK_SIZE + self.vpartition_table_size + self.allocation_table_size) * 2
    }
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

impl std::fmt::Debug for PartitionEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("PartitionEntry")
            .field("type_guid", &Uuid::from_slice(&self.type_guid).unwrap())
            .field("guid", &Uuid::from_slice(&self.guid).unwrap())
            .field("slices", &self.slices)
            .field("flags", &self.flags)
            .field("name", &self.name())
            .finish()
    }
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

impl SliceEntry {
    fn partition_index(&self) -> u16 {
        self.0 as u16
    }

    fn logical_slice(&self) -> u64 {
        self.0 >> 16
    }

    fn set(&mut self, partition_index: u16, logical_slice: u64) {
        self.0 = partition_index as u64 | logical_slice << 16;
    }
}

struct Fvm {
    // Which metadata slot the current metadata is using.
    device: DeviceHolder,

    // We use an async lock to make it easier to mediate safe access to the metadata.  When we
    // mutate the metadata, we need to hold a lock whilst writing the metadata (which is done using
    // async code) so that no other mutations can race.  When performing regular I/O, we can iterate
    // through mappings and perform async I/O without having to drop the lock.
    inner: async_lock::RwLock<Inner>,
}

struct Inner {
    slot: u8,
    metadata: Metadata,
    mappings: HashMap<u16, Vec<Mapping>>,
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

#[derive(Clone)]
struct Metadata {
    // The `hash` field of the header is not necessarily up to date and must be recomputed before
    // writing.
    header: Header,

    partitions: BTreeMap<u16, PartitionEntry>,
    allocations: Vec<SliceEntry>,
}

impl Metadata {
    async fn read(header_block: &[u8], device: &dyn Device, offset: u64) -> Result<Self, Error> {
        let header =
            Header::ref_from_prefix(header_block).ok_or(anyhow!("Block size too small"))?;
        if header.magic != MAGIC {
            bail!("Magic mismatch");
        }
        ensure!(
            header.slice_size > 0 && header.slice_size % BLOCK_SIZE == 0,
            format!("Slice size ({}) not a non-zero multiple of {BLOCK_SIZE}", header.slice_size)
        );

        // Read the vpartition and allocation table.
        // TODO(https://fxbug.dev/357467643): Check sizes
        let allocation_size = header.allocation_size()?;

        let part_table_size = header.vpartition_table_size as usize;
        let mut buffer = device.allocate_buffer(part_table_size + allocation_size).await;
        device.read(offset + BLOCK_SIZE, buffer.as_mut()).await?;

        // Check the hash.
        let mut hasher = Sha256::new();
        let mut header_copy = *header;
        header_copy.hash = [0; 32];
        hasher.update(header_copy.as_bytes());
        hasher.update(&header_block[std::mem::size_of::<Header>()..]);
        hasher.update(buffer.as_slice());

        if hasher.finalize().as_slice() != header.hash {
            bail!("Hash mismatch");
        }

        let partitions: BTreeMap<_, _> = buffer.as_slice()[..part_table_size]
            .chunks_exact(std::mem::size_of::<PartitionEntry>())
            .enumerate()
            .skip(1) // The first partition is unused.
            .filter_map(|(index, e)| {
                let partition = PartitionEntry::read_from(e).unwrap();
                partition.is_allocated().then(|| (index as u16, partition))
            })
            .collect();
        let allocations: Vec<_> = if allocation_size < std::mem::size_of::<SliceEntry>() {
            Vec::new()
        } else {
            buffer.as_slice()[part_table_size..part_table_size + allocation_size]
                .chunks_exact(std::mem::size_of::<SliceEntry>())
                .skip(1) // The first slice entry is unused.
                .map(|e| SliceEntry::read_from(e).unwrap())
                .collect()
        };

        Ok(Self { header: header_copy, partitions, allocations })
    }

    async fn write(&self, device: &dyn Device, offset: u64) -> Result<(), Error> {
        let mut buffer = device
            .allocate_buffer(
                (BLOCK_SIZE + self.header.vpartition_table_size) as usize
                    + self.header.allocation_size()?,
            )
            .await;
        buffer.as_mut_slice().fill(0);
        let header = Header::mut_from_prefix(buffer.as_mut_slice()).unwrap();
        *header = self.header;
        header.generation += 1;
        header.hash.fill(0);

        // Write out the partitions:
        for (&index, partition) in &self.partitions {
            let entry = PartitionEntry::mut_from_prefix(
                &mut buffer.as_mut_slice()[BLOCK_SIZE as usize
                    + std::mem::size_of::<PartitionEntry>() * index as usize..],
            )
            .unwrap();
            *entry = *partition;
        }

        // Write out the allocation table:
        let mut out = buffer.as_mut_slice()
            [(BLOCK_SIZE + self.header.vpartition_table_size) as usize..]
            .chunks_exact_mut(std::mem::size_of::<SliceEntry>());

        // The first slice entry is unused.
        out.next();

        for slice_entry in &self.allocations {
            *SliceEntry::mut_from_prefix(out.next().unwrap()).unwrap() = *slice_entry;
        }

        // Compute the hash.
        let mut hasher = Sha256::new();
        hasher.update(buffer.as_slice());
        let header = Header::mut_from_prefix(buffer.as_mut_slice()).unwrap();
        header.hash.copy_from_slice(hasher.finalize().as_slice());

        device.write(offset, buffer.as_ref()).await
    }
}

impl Fvm {
    /// Opens the FVM device.
    pub async fn open(device: DeviceHolder) -> Result<Self, Error> {
        let mut metadata = Vec::new();
        {
            let mut header_block = device.allocate_buffer(BLOCK_SIZE as usize).await;
            device.read(0, header_block.as_mut()).await?;

            metadata.push(Metadata::read(header_block.as_slice(), device.as_ref(), 0).await);

            let header = Header::ref_from_prefix(header_block.as_slice())
                .ok_or(anyhow!("Block size too small"))?;
            // TODO(https://fxbug.dev/357467643): Check offset is sensible.
            let secondary_offset = header.offset_for_slot(1);
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
            let partition_index = allocation.partition_index();
            let slice = allocation.logical_slice();
            if partition_index == 0 {
                // Entry is free.
                continue;
            }
            if !metadata.partitions.contains_key(&partition_index) {
                warn!("Slice entry points to free partition: 0x{:x?}", allocation.0);
                continue;
            };
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

        Ok(Self {
            device,
            inner: async_lock::RwLock::new(Inner { slot: slot as u8, metadata, mappings }),
        })
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
        partition: u16,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), Error> {
        struct Read;
        impl IoTrait for Read {
            fn get_op<'a>(
                device: &'a dyn Device,
                offset: u64,
                buf: MutableBufferRef<'a>,
            ) -> impl Future<Output = Result<(), Error>> + 'a {
                device.read(offset, buf)
            }

            fn post(buf: &[u8], vmo: &zx::Vmo, vmo_offset: u64) -> Result<(), zx::Status> {
                vmo.write(buf, vmo_offset)
            }
        }
        self.do_io::<Read>(partition, device_block_offset, block_count, vmo, vmo_offset)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "Read failed");
                error
            })
    }

    async fn write(
        &self,
        partition: u16,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), Error> {
        struct Write;
        impl IoTrait for Write {
            fn pre(buf: &mut [u8], vmo: &zx::Vmo, vmo_offset: u64) -> Result<(), zx::Status> {
                vmo.read(buf, vmo_offset)
            }
            fn get_op<'a>(
                device: &'a dyn Device,
                offset: u64,
                buf: MutableBufferRef<'a>,
            ) -> impl Future<Output = Result<(), Error>> + 'a {
                device.write(offset, buf.into_ref())
            }
        }
        self.do_io::<Write>(partition, device_block_offset, block_count, vmo, vmo_offset)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "Write failed");
                error
            })
    }

    async fn do_io<Io: IoTrait>(
        &self,
        partition: u16,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        mut vmo_offset: u64,
    ) -> Result<(), Error> {
        let inner = self.inner.read().await;
        let Some(mappings) = inner.mappings.get(&partition) else {
            bail!(zx::Status::INTERNAL);
        };

        // TODO(https://fxbug.dev/357467643): Eliminate copying to improve performance.

        const BUFFER_SIZE: usize = 1048576;
        let mut buffer = self.device.allocate_buffer(BUFFER_SIZE).await;
        let mut offset = device_block_offset
            .checked_mul(BLOCK_SIZE)
            .ok_or(zx::Status::OUT_OF_RANGE)
            .with_context(|| format!("Bad offset ({device_block_offset})"))?;
        let mut total_len = (block_count as u64)
            .checked_mul(BLOCK_SIZE)
            .ok_or(zx::Status::OUT_OF_RANGE)
            .with_context(|| format!("Bad count ({block_count})"))?;

        let metadata = &inner.metadata;
        let slice_size = metadata.header.slice_size;
        let data_start = metadata.header.data_start();

        while total_len > 0 {
            let amount = std::cmp::min(buffer.len() as u64, total_len) as usize;
            Io::pre(&mut buffer.as_mut_slice()[..amount], &vmo, vmo_offset)?;
            let mut buffer_left = buffer.as_mut();
            let mut ops = Vec::new();
            while total_len > 0 {
                let slice = offset / slice_size;
                let index = match mappings.binary_search_by(|m| m.logical_slice.cmp(&slice)) {
                    Ok(index) => index,
                    Err(index) if index > 0 => index - 1,
                    _ => {
                        return Err(zx::Status::OUT_OF_RANGE).with_context(|| {
                            format!("No mapping #1 ({device_block_offset}, {block_count})")
                        });
                    }
                };
                let mapping = &mappings[index];
                let end_slice = mapping.end_slice();
                if slice >= end_slice {
                    return Err(zx::Status::OUT_OF_RANGE).with_context(|| {
                        format!("No mapping #2 ({device_block_offset}, {block_count})")
                    });
                }
                let len = std::cmp::min(
                    (end_slice - slice) * slice_size,
                    std::cmp::min(total_len, buffer_left.len() as u64),
                ) as usize;
                let (buf, remaining) = buffer_left.split_at_mut(len);
                let physical_offset = data_start
                    + (mapping.physical_slice + (slice - mapping.logical_slice)) * slice_size;
                ops.push(Io::get_op(self.device.as_ref(), physical_offset, buf));
                offset += len as u64;
                total_len -= len as u64;
                if remaining.is_empty() {
                    break;
                }
                buffer_left = remaining;
            }
            try_join_all(ops).await?;
            Io::post(&buffer.as_slice()[..amount], &vmo, vmo_offset)?;
            vmo_offset += amount as u64;
        }
        Ok(())
    }

    async fn create_partition(
        &self,
        inner: async_lock::RwLockUpgradableReadGuard<'_, Inner>,
        type_guid: [u8; 16],
        guid: [u8; 16],
        slices: u32,
        name_str: &str,
    ) -> Result<u16, Error> {
        // TODO(https://fxbug.dev/357467643): Handle growing pslice_count.

        ensure!(slices > 0, zx::Status::INVALID_ARGS);
        let name_len = name_str.as_bytes().len();
        ensure!(name_len <= 24, zx::Status::INVALID_ARGS);

        // Find a free partition
        let mut proposed = 1;
        for (&index, _) in &inner.metadata.partitions {
            if proposed != index {
                break;
            }
            let Some(next) = index.checked_add(1) else {
                bail!(zx::Status::NO_SPACE);
            };
            proposed = next;
        }

        const MAX_PARTITIONS: u64 = 1024;
        let max_partitions = std::cmp::min(
            inner.metadata.header.vpartition_table_size
                / std::mem::size_of::<PartitionEntry>() as u64,
            MAX_PARTITIONS,
        );
        ensure!((proposed as u64) < max_partitions, zx::Status::NO_SPACE);

        let mut new_metadata = inner.metadata.clone();

        // Allocate slices:
        let mut mappings: Vec<Mapping> = Vec::new();
        let mut logical_slice = 0;

        // Limit the maximum slice to the device size.
        let max_slice = (self.device.block_count() * self.device.block_size() as u64
            - new_metadata.header.data_start())
            / new_metadata.header.slice_size;

        for (physical_slice, allocation) in new_metadata.allocations.iter_mut().enumerate() {
            if physical_slice as u64 == max_slice {
                break;
            }

            if allocation.partition_index() == 0 {
                allocation.set(proposed, logical_slice);
                let add_new_mapping = match mappings.last_mut() {
                    Some(mapping)
                        if mapping.physical_slice + mapping.slice_count
                            == physical_slice as u64 =>
                    {
                        mapping.slice_count += 1;
                        false
                    }
                    _ => true,
                };
                if add_new_mapping {
                    mappings.push(Mapping {
                        logical_slice,
                        physical_slice: physical_slice as u64,
                        slice_count: 1,
                    });
                }
                logical_slice += 1;
                if logical_slice == slices as u64 {
                    break;
                }
            }
        }
        ensure!(logical_slice == slices as u64, zx::Status::NO_SPACE);

        let mut name = [0; 24];
        name[..name_len].copy_from_slice(name_str.as_bytes());
        new_metadata
            .partitions
            .insert(proposed, PartitionEntry { type_guid, guid, slices, flags: 0, name });
        new_metadata.header.generation = new_metadata
            .header
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow!(zx::Status::BAD_STATE))?;

        let new_slot = 1 - inner.slot;
        new_metadata
            .write(self.device.as_ref(), new_metadata.header.offset_for_slot(new_slot))
            .await?;

        let mut inner = async_lock::RwLockUpgradableReadGuard::upgrade(inner).await;

        inner.slot = new_slot;
        inner.metadata = new_metadata;
        inner.mappings.insert(proposed, mappings);

        Ok(proposed)
    }
}

// Trait to abstract over the difference between reads and writes.
trait IoTrait {
    // Called prior to performing the operation (used for writes).
    fn pre(_buf: &mut [u8], _vmo: &zx::Vmo, _vmo_offset: u64) -> Result<(), zx::Status> {
        Ok(())
    }

    // Called to get the future that performs the read or write.
    fn get_op<'a>(
        device: &'a dyn Device,
        offset: u64,
        buf: MutableBufferRef<'a>,
    ) -> impl Future<Output = Result<(), Error>> + 'a;

    // Called after performing the operation (used for reads).
    fn post(_buf: &[u8], _vmo: &zx::Vmo, _vmo_offset: u64) -> Result<(), zx::Status> {
        Ok(())
    }
}

/// Serves a multi-filesystem component that uses the FVM format.
struct Component {
    export_dir: Arc<vfs::directory::immutable::Simple>,
    scope: ExecutionScope,
    fvm: Mutex<Option<Arc<Fvm>>>,
    mounted: Mutex<HashMap<u16, Arc<BlockServer<SessionManager<PartitionInterface>>>>>,
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
                StartupRequest::Start { responder, device, options } => responder
                    .send(self.handle_start(device, options).await.map_err(map_to_raw_status))?,
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
        let mut fvm = Fvm::open(device_holder).await?;

        let volumes_directory = vfs::directory::immutable::simple();

        for (&index, partition) in &fvm.inner.get_mut().metadata.partitions {
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

        self.export_dir.add_entry_may_overwrite(
            "volumes",
            volumes_directory,
            /* overwrite: */ true,
        )?;

        *self.fvm.lock().unwrap() = Some(Arc::new(fvm));
        Ok(())
    }

    async fn handle_volumes_requests(
        self: &Arc<Self>,
        mut stream: VolumesRequestStream,
    ) -> Result<(), Error> {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                VolumesRequest::Create {
                    responder,
                    name,
                    outgoing_directory,
                    create_options,
                    mount_options,
                } => {
                    responder.send(
                        self.handle_create_volume(
                            &name,
                            outgoing_directory,
                            create_options,
                            mount_options,
                        )
                        .await
                        .map_err(map_to_raw_status),
                    )?;
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
        partition: u16,
    ) -> Result<(), Error> {
        while let Some(request) = requests.try_next().await? {
            match request {
                VolumeRequest::Check { responder, .. } => {
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
                VolumeRequest::Mount { responder, outgoing_directory, options } => responder.send(
                    self.handle_mount(partition, outgoing_directory, options).await.map_err(
                        |error| {
                            error!(?error, partition, "Failed to mount volume");
                            map_to_raw_status(error)
                        },
                    ),
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

    async fn handle_mount(
        self: &Arc<Self>,
        partition_index: u16,
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
        let fvm = self.fvm.lock().unwrap().as_ref().unwrap().clone();
        let partition_info = {
            let inner = fvm.inner.read().await;
            let partition =
                &inner.metadata.partitions.get(&partition_index).ok_or(zx::Status::INTERNAL)?;
            PartitionInfo {
                block_count: u64::MAX,
                block_size: BLOCK_SIZE as u32,
                type_guid: partition.type_guid,
                instance_guid: partition.guid,
                name: partition.name().to_string(),
            }
        };
        self.mounted.lock().unwrap().insert(
            partition_index,
            Arc::new(BlockServer::new(
                partition_info,
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
        partition: u16,
        requests: fvolume::VolumeRequestStream,
    ) -> Result<(), Error> {
        let partition = self.mounted.lock().unwrap().get(&partition).unwrap().clone();
        partition.handle_requests(requests).await
    }

    async fn handle_create_volume(
        self: &Arc<Self>,
        name: &str,
        outgoing_directory: ServerEnd<fio::DirectoryMarker>,
        create_options: CreateOptions,
        mount_options: MountOptions,
    ) -> Result<(), Error> {
        let fvm = self.fvm.lock().unwrap().as_ref().unwrap().clone();
        let inner = fvm.inner.upgradable_read().await;
        let Some(type_guid) = create_options.type_guid else {
            bail!(zx::Status::INVALID_ARGS);
        };
        let guid = create_options.guid.unwrap_or_else(|| Uuid::new_v4().to_bytes_le());
        let slices = match create_options.initial_size {
            Some(x) => {
                ensure!(x % inner.metadata.header.slice_size == 0, zx::Status::INVALID_ARGS);
                (x / inner.metadata.header.slice_size)
                    .try_into()
                    .map_err(|_| zx::Status::INVALID_ARGS)?
            }
            None => 1,
        };
        let partition_index = fvm.create_partition(inner, type_guid, guid, slices, name).await?;
        self.handle_mount(partition_index, outgoing_directory, mount_options).await.map_err(
            |error| {
                tracing::warn!(?error, "Created partition {name}, but failed to mount");
                error
            },
        )
    }
}

struct PartitionInterface {
    partition_index: u16,
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

#[fuchsia::main]
fn main() -> Result<(), Error> {
    todo!();
}

#[cfg(test)]
mod tests {
    use super::{map_to_status, Component};
    use block_client::{BlockClient, BufferSlice, MutableBufferSlice, RemoteBlockClient};
    use fake_block_server::FakeServer;
    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_fs_startup::{
        CompressionAlgorithm, CreateOptions, EvictionPolicyOverride, MountOptions, StartOptions,
        StartupMarker, VolumeMarker, VolumesMarker,
    };
    use fidl_fuchsia_hardware_block::BlockMarker;
    use fuchsia_component::client::{
        connect_to_named_protocol_at_dir_root, connect_to_protocol_at_dir_svc,
    };
    use std::sync::Arc;
    use {
        fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio,
        fuchsia_async as fasync, fuchsia_zircon as zx,
    };

    struct Fixture {
        component: Arc<Component>,
        outgoing_dir: fio::DirectoryProxy,
        fake_server: Arc<FakeServer>,
    }

    impl Fixture {
        async fn new(extra_space: u64) -> Self {
            let contents = std::fs::read("/pkg/data/golden-fvm.blk").unwrap();
            let fake_server = Arc::new(FakeServer::new(
                (contents.len() as u64 + extra_space) / 8192,
                8192,
                &contents,
            ));
            Self::from_fake_server(fake_server).await
        }

        async fn from_fake_server(fake_server: Arc<FakeServer>) -> Self {
            let (outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
            let fixture =
                Fixture { component: Arc::new(Component::new()), outgoing_dir, fake_server };
            let fake_server = fixture.fake_server.clone();
            let (block_client, block_server) =
                fidl::endpoints::create_request_stream::<BlockMarker>().unwrap();
            fasync::Task::spawn(async move {
                let _ = fake_server.serve(block_server.cast_stream()).await;
            })
            .detach();
            fixture.component.serve(server_end.into_channel()).await.unwrap();
            let startup_proxy =
                connect_to_protocol_at_dir_svc::<StartupMarker>(&fixture.outgoing_dir).unwrap();

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

            fixture
        }
    }

    #[fuchsia::test]
    async fn test_golden() {
        let fixture = Fixture::new(0).await;

        // Mount the blobfs partition.
        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/blobfs",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        // Look for blobfs's magic:
        let block_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let client = RemoteBlockClient::new(block_proxy).await.unwrap();
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

        // And check the journal magic, which is in a different slice:
        let mut buf = vec![0; 8192];
        client.read_at(MutableBufferSlice::Memory(&mut buf), 0x30000 * 8192).await.unwrap();
        assert_eq!(&buf[..8], &[0x6c, 0x6e, 0x72, 0x6a, 0x62, 0x6f, 0x6c, 0x62]);

        // Reading from a slice that's not allocated should fail.
        assert_eq!(
            map_to_status(
                client
                    .read_at(MutableBufferSlice::Memory(&mut buf), 32768)
                    .await
                    .expect_err("Read from slice #2 should fail")
            ),
            zx::Status::OUT_OF_RANGE
        );

        // Mount the minfs partition.
        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/data",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let block_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let client = RemoteBlockClient::new(block_proxy).await.unwrap();

        // Check some writes.
        for offset in [0, 10 * 8192, 20 * 8192] {
            let buf = vec![0xaf; 16384];
            client.write_at(BufferSlice::Memory(&buf), offset).await.unwrap();
            let mut read_buf = vec![0; 16384];
            client.read_at(MutableBufferSlice::Memory(&mut read_buf), offset).await.unwrap();
            assert_eq!(&buf, &read_buf);
        }
    }

    #[fuchsia::test]
    async fn test_create_volume() {
        let buf = vec![0xaf; 16384];

        let fake_server = {
            let fixture = Fixture::new(32768).await;

            let volumes_proxy =
                connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

            let (dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                    .expect("Create proxy to succeed");
            volumes_proxy
                .create(
                    "foo",
                    dir_server_end,
                    CreateOptions {
                        type_guid: Some([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
                        ..CreateOptions::default()
                    },
                    MountOptions::default(),
                )
                .await
                .expect("create failed (FIDL)")
                .expect("create failed");

            // Check we can read and write from the new partition.
            let block_proxy =
                connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
            let client = RemoteBlockClient::new(block_proxy).await.unwrap();

            // Check some writes.
            for offset in [0, 16384] {
                client.write_at(BufferSlice::Memory(&buf), offset).await.unwrap();
                let mut read_buf = vec![0; 16384];
                client.read_at(MutableBufferSlice::Memory(&mut read_buf), offset).await.unwrap();
                assert_eq!(&buf, &read_buf);
            }
            fixture.fake_server
        };

        // Reopen, and check the same reads.
        let fixture = Fixture::from_fake_server(fake_server).await;

        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/foo",
        )
        .unwrap();
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
            .expect("Create proxy to succeed");
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let block_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let client = RemoteBlockClient::new(block_proxy).await.unwrap();

        for offset in [0, 16384] {
            let mut read_buf = vec![0; 16384];
            client.read_at(MutableBufferSlice::Memory(&mut read_buf), offset).await.unwrap();
            assert_eq!(&buf, &read_buf);
        }
    }

    #[fuchsia::test]
    async fn test_create_volume_no_space() {
        // On the first pass, we should run out of space due to lack of space for the partition
        // data, and in the second case, we should run out of space due to lack of space in the
        // partition table.
        for extra_space in [32768, 32768 * 1024] {
            // Keep creating partitions until we run out of space.
            let mut partition_count = 0;

            let fake_server = {
                let fixture = Fixture::new(extra_space).await;

                let volumes_proxy =
                    connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

                loop {
                    let (_dir_proxy, dir_server_end) =
                        fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                            .expect("Create proxy to succeed");
                    match volumes_proxy
                        .create(
                            &format!("foo {partition_count}"),
                            dir_server_end,
                            CreateOptions {
                                type_guid: Some([
                                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
                                ]),
                                ..CreateOptions::default()
                            },
                            MountOptions::default(),
                        )
                        .await
                        .expect("create failed (FIDL)")
                    {
                        Ok(()) => {}
                        Err(zx::sys::ZX_ERR_NO_SPACE) => break,
                        Err(error) => panic!("create failed: {error:?}"),
                    }
                    partition_count += 1;
                }
                fixture.fake_server
            };

            tracing::info!("Created {partition_count} partitions");

            // Reopen and check we can mount all the partitions we created.
            let fixture = Fixture::from_fake_server(fake_server).await;

            for i in 0..partition_count {
                let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
                    &fixture.outgoing_dir,
                    &format!("volumes/foo {i}"),
                )
                .unwrap();

                let (_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>()
                        .expect("Create proxy to succeed");
                volume_proxy
                    .mount(dir_server_end, MountOptions::default())
                    .await
                    .expect("mount failed (FIDL)")
                    .expect("mount failed");
            }
        }
    }
}
