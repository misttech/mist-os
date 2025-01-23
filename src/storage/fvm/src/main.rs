// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod device;
mod mapping;
mod zxcrypt;

use anyhow::{anyhow, bail, ensure, Context, Error};
use block_client::{
    BlockClient, BufferSlice, MutableBufferSlice, RemoteBlockClient, VmoId, WriteOptions,
};
use block_server::async_interface::{Interface, SessionManager};
use block_server::{BlockServer, PartitionInfo};
use device::Device;
use fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_fs_startup::{
    CompressionAlgorithm, CreateOptions, EvictionPolicyOverride, FormatOptions, MountOptions,
    StartOptions, StartupMarker, StartupRequest, StartupRequestStream, VolumeRequest,
    VolumeRequestStream, VolumesMarker, VolumesRequest, VolumesRequestStream,
};
use fidl_fuchsia_hardware_block::BlockMarker;
use fs_management::filesystem::{BlockConnector, Filesystem};
use fs_management::{ComponentType, FSConfig, Options};
use fuchsia_runtime::HandleType;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, TryStreamExt as _};
use log::{debug, error, info, warn};
use mapping::{Mapping, MappingExt as _};
use regex::Regex;
use sha2::{Digest, Sha256};
use static_assertions::const_assert;
use std::alloc;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Formatter;
use std::future::Future;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::execution_scope::ExecutionScope;
use vfs::path::Path;
use vfs::ObjectRequest;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
use {
    fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio, fuchsia_async as fasync,
};

// See //src/storage/fvm/format.h for a detailed description of the FVM format.

static MAGIC: u64 = 0x54524150204d5646;

// Whilst this is the block size that FVM uses to round up certain structures, FVM still supports
// I/O at a smaller size than this (it will pass along the block size from the underlying device).
const BLOCK_SIZE: u64 = 8192;

// This is the maximum slice count which means the maximum slice offset is one less than this.
const MAX_SLICE_COUNT: u64 = u32::MAX as u64;

#[repr(C)]
#[derive(Clone, Copy, KnownLayout, FromBytes, IntoBytes, Immutable)]
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
            .ok_or_else(|| anyhow!("Bad pslice_count"))
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
#[derive(Clone, Copy, KnownLayout, FromBytes, IntoBytes, Immutable)]
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
#[derive(Clone, Copy, KnownLayout, FromBytes, IntoBytes, Immutable)]
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
    device: Arc<Device>,

    // The slize size in blocks.
    slice_blocks: u64,

    // We use an async lock to make it easier to mediate safe access to the metadata.  When we
    // mutate the metadata, we need to hold a lock whilst writing the metadata (which is done using
    // async code) so that no other mutations can race.  When performing regular I/O, we can iterate
    // through mappings and perform async I/O without having to drop the lock.
    inner: async_lock::RwLock<Inner>,
}

struct Inner {
    slot: u8,
    metadata: Metadata,
    partition_state: HashMap<u16, PartitionState>,
    assigned_slice_count: u64,
}

#[derive(Default)]
struct PartitionState {
    mappings: Vec<Mapping>,
    slice_limit: u64,
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
    async fn read(
        header_block: &[u8],
        device: &RemoteBlockClient,
        offset: u64,
    ) -> Result<Self, Error> {
        let (header, _) =
            Header::ref_from_prefix(header_block).map_err(|_| anyhow!("Block size too small"))?;
        ensure!(header.magic == MAGIC, "Magic mismatch");
        ensure!(
            header.slice_size > 0 && header.slice_size % BLOCK_SIZE == 0,
            format!("Slice size ({}) not a non-zero multiple of {BLOCK_SIZE}", header.slice_size)
        );

        // Read the vpartition and allocation table.
        // TODO(https://fxbug.dev/357467643): Check sizes
        let allocation_size = header.allocation_size()?;
        let part_table_size = header.vpartition_table_size as usize;

        let mut buffer = vec![0; part_table_size + allocation_size];
        device.read_at(MutableBufferSlice::Memory(&mut buffer), offset + BLOCK_SIZE).await?;

        // Check the hash.
        let mut hasher = Sha256::new();
        let mut header_copy = *header;
        header_copy.hash = [0; 32];
        hasher.update(header_copy.as_bytes());
        hasher.update(&header_block[std::mem::size_of::<Header>()..]);
        hasher.update(&buffer);

        if hasher.finalize().as_slice() != header.hash {
            return Err(zx::Status::IO_DATA_INTEGRITY).context("Hash mismatch");
        }

        let partitions: BTreeMap<_, _> = buffer[..part_table_size]
            .chunks_exact(std::mem::size_of::<PartitionEntry>())
            .enumerate()
            .skip(1) // The first partition is unused.
            .filter_map(|(index, e)| {
                let partition = PartitionEntry::read_from_bytes(e).unwrap();
                partition.is_allocated().then(|| (index as u16, partition))
            })
            .collect();
        let allocations: Vec<_> = if allocation_size < std::mem::size_of::<SliceEntry>() {
            Vec::new()
        } else {
            buffer[part_table_size..part_table_size + allocation_size]
                .chunks_exact(std::mem::size_of::<SliceEntry>())
                .skip(1) // The first slice entry is unused.
                .map(|e| SliceEntry::read_from_bytes(e).unwrap())
                .collect()
        };

        Ok(Self { header: header_copy, partitions, allocations })
    }

    async fn write(&self, device: &RemoteBlockClient, offset: u64) -> Result<(), Error> {
        // We align the memory to that required by `Header`, but the memory needs to be aligned for
        // `PartitionEntry` and `SliceEntry` too.
        const_assert!(std::mem::align_of::<Header>() >= std::mem::align_of::<PartitionEntry>());
        const_assert!(std::mem::align_of::<Header>() >= std::mem::align_of::<SliceEntry>());

        let mut buffer = AlignedMem::<Header>::new(
            (BLOCK_SIZE + self.header.vpartition_table_size) as usize
                + self.header.allocation_size()?,
        );
        let (header, _) = Header::mut_from_prefix(&mut buffer).unwrap();
        *header = self.header;
        header.hash.fill(0);

        // Write out the partitions:
        for (&index, partition) in &self.partitions {
            let (entry, _) = PartitionEntry::mut_from_prefix(
                &mut buffer[BLOCK_SIZE as usize
                    + std::mem::size_of::<PartitionEntry>() * index as usize..],
            )
            .unwrap();
            *entry = *partition;
        }

        // Write out the allocation table.  The first slice entry is unused.
        <[SliceEntry]>::mut_from_bytes(
            &mut buffer[(BLOCK_SIZE + self.header.vpartition_table_size) as usize..],
        )
        .unwrap()[1..1 + self.allocations.len()]
            .copy_from_slice(&self.allocations);

        // Compute the hash.
        let mut hasher = Sha256::new();
        hasher.update(&*buffer);
        let (header, _) = Header::mut_from_prefix(&mut buffer).unwrap();
        header.hash.copy_from_slice(hasher.finalize().as_slice());

        device.write_at(BufferSlice::Memory(&buffer), offset).await?;

        // Always flush after writing metadata.
        device.flush().await?;

        Ok(())
    }

    /// Allocates slices.  NOTE: This will leave the metadata in an inconsistent state if this
    /// fails.
    fn allocate_slices(
        &mut self,
        partition_index: u16,
        mut logical_slice: u64,
        mut count: u64,
        max_slice: u64,
    ) -> Result<Vec<Mapping>, zx::Status> {
        let partition = self.partitions.get_mut(&partition_index).unwrap();
        partition.slices = partition
            .slices
            .checked_add(count.try_into().map_err(|_| zx::Status::NO_SPACE)?)
            .ok_or(zx::Status::NO_SPACE)?;
        let mut mappings = Vec::<Mapping>::new();
        for (physical_slice, allocation) in self.allocations.iter_mut().enumerate() {
            if physical_slice as u64 == max_slice {
                break;
            }

            if allocation.partition_index() == 0 {
                allocation.set(partition_index, logical_slice);
                let add_new_mapping = match mappings.last_mut() {
                    Some(mapping) if mapping.to + mapping.len() == physical_slice as u64 => {
                        mapping.from.end += 1;
                        false
                    }
                    _ => true,
                };
                if add_new_mapping {
                    mappings.push(Mapping {
                        from: logical_slice..logical_slice + 1,
                        to: physical_slice as u64,
                    });
                }
                logical_slice += 1;
                count -= 1;
                if count == 0 {
                    return Ok(mappings);
                }
            }
        }
        Err(zx::Status::NO_SPACE)
    }
}

impl Fvm {
    /// Opens the FVM device.
    pub async fn open(client: RemoteBlockClient) -> Result<Self, Error> {
        ensure!(BLOCK_SIZE as u32 % client.block_size() == 0, zx::Status::NOT_SUPPORTED);

        let mut metadata = Vec::new();
        {
            let mut header_block = AlignedMem::<Header>::new(BLOCK_SIZE as usize);
            client.read_at(MutableBufferSlice::Memory(&mut header_block), 0).await?;

            metadata.push(Metadata::read(&header_block, &client, 0).await);

            let (header, _) = Header::ref_from_prefix(&header_block)
                .map_err(|_| anyhow!("Block size too small"))?;
            // TODO(https://fxbug.dev/357467643): Check offset is sensible.
            let secondary_offset = header.offset_for_slot(1);
            client.read_at(MutableBufferSlice::Memory(&mut header_block), secondary_offset).await?;

            metadata.push(Metadata::read(&header_block, &client, secondary_offset).await);
        }

        let (slot, metadata) = Self::pick_metadata(metadata).ok_or_else(|| {
            warn!("No valid metadata");
            anyhow!("No valid metadata")
        })?;

        // Build the mappings.
        let mut partition_state = HashMap::<u16, PartitionState>::new();
        let mut assigned_slice_count = 0;
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
            assigned_slice_count += 1;
            let mappings = &mut partition_state.entry(partition_index).or_default().mappings;
            let mut bad_mapping = false;
            match mappings.binary_search_by(|m: &Mapping| m.from.start.cmp(&slice)) {
                Ok(_) => bad_mapping = true,
                Err(index) => {
                    let insert = if index > 0 {
                        // See if this can be merged with the previous entry.
                        let prev_mapping = &mut mappings[index - 1];
                        let end = prev_mapping.from.end;
                        if end == slice
                            && prev_mapping.to + prev_mapping.len() == physical_slice as u64
                        {
                            prev_mapping.from.end += 1;
                            false
                        } else if end <= slice {
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
                            Mapping { from: slice..slice + 1, to: physical_slice as u64 },
                        );
                    }
                }
            };
            if bad_mapping {
                warn!("Duplicate slice entry: 0x{:x?}", allocation.0);
            }
        }

        info!(
            "Mounted fvm, slice size {} partitions: {:?}",
            metadata.header.slice_size,
            metadata.partitions.iter().map(|(_, e)| e.name()).collect::<Vec<_>>()
        );

        let slice_blocks = metadata.header.slice_size / client.block_size() as u64;
        Ok(Self {
            device: Arc::new(Device::new(client).await?),
            slice_blocks,
            inner: async_lock::RwLock::new(Inner {
                slot: slot as u8,
                metadata,
                partition_state,
                assigned_slice_count,
            }),
        })
    }

    fn block_size(&self) -> u32 {
        self.device.block_size()
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
                    warn!(error:?; "Bad metadata {index}");
                    None
                }
            })
            .max_by_key(|(_index, metadata)| metadata.header.generation)
    }

    async fn do_io(
        &self,
        mut io: impl IoTrait,
        partition_index: u16,
        device_block_offset: u64,
        block_count: u32,
    ) -> Result<(), Error> {
        let inner = self.inner.read().await;
        let Some(PartitionState { mappings, .. }) = inner.partition_state.get(&partition_index)
        else {
            bail!(zx::Status::INTERNAL);
        };

        let block_size = self.block_size() as u64;
        let mut offset = device_block_offset
            .checked_mul(block_size)
            .ok_or(zx::Status::OUT_OF_RANGE)
            .with_context(|| format!("Bad offset ({device_block_offset})"))?;
        let mut total_len = (block_count as u64)
            .checked_mul(block_size)
            .ok_or(zx::Status::OUT_OF_RANGE)
            .with_context(|| format!("Bad count ({block_count})"))?;

        let metadata = &inner.metadata;
        let slice_size = metadata.header.slice_size;
        let data_start = metadata.header.data_start();

        while total_len > 0 {
            let slice = offset / slice_size;
            let index = match mappings.binary_search_by(|m| m.from.start.cmp(&slice)) {
                Ok(index) => index,
                Err(index) if index > 0 => index - 1,
                _ => {
                    return Err(zx::Status::OUT_OF_RANGE).with_context(|| {
                        format!("No mapping #1 ({device_block_offset}, {block_count})")
                    });
                }
            };
            let mapping = &mappings[index];
            if slice >= mapping.from.end {
                return Err(zx::Status::OUT_OF_RANGE).with_context(|| {
                    format!("No mapping #2 ({device_block_offset}, {block_count})")
                });
            }
            let end = mapping.from.end * slice_size;
            let len = std::cmp::min(end - offset, total_len);
            let physical_offset =
                data_start + mapping.to * slice_size + (offset - mapping.from.start * slice_size);

            io.add_op(physical_offset, len).await?;
            offset += len;
            total_len -= len;
        }
        io.flush().await?;
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
        let max_slice = self.max_slice(&new_metadata);

        let mut name = [0; 24];
        name[..name_len].copy_from_slice(name_str.as_bytes());
        new_metadata
            .partitions
            .insert(proposed, PartitionEntry { type_guid, guid, slices: 0, flags: 0, name });

        let slices = slices as u64;
        let mappings = new_metadata.allocate_slices(proposed, 0, slices, max_slice)?;

        let mut inner = self
            .write_new_metadata(inner, new_metadata)
            .await
            .context("Failed to write metadata")?;

        inner.assigned_slice_count = inner.assigned_slice_count.checked_add(slices).unwrap();
        inner.partition_state.insert(proposed, PartitionState { mappings, slice_limit: 0 });

        Ok(proposed)
    }

    fn max_slice(&self, metadata: &Metadata) -> u64 {
        (self.device.block_count() * self.device.block_size() as u64 - metadata.header.data_start())
            / metadata.header.slice_size
    }

    async fn write_new_metadata<'a>(
        &self,
        inner: async_lock::RwLockUpgradableReadGuard<'a, Inner>,
        mut new_metadata: Metadata,
    ) -> Result<async_lock::RwLockWriteGuard<'a, Inner>, Error> {
        new_metadata.header.generation = new_metadata
            .header
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow!(zx::Status::BAD_STATE))?;

        let new_slot = 1 - inner.slot;
        new_metadata.write(&self.device, new_metadata.header.offset_for_slot(new_slot)).await?;

        let mut inner = async_lock::RwLockUpgradableReadGuard::upgrade(inner).await;

        inner.slot = new_slot;
        inner.metadata = new_metadata;

        Ok(inner)
    }

    /// Ensures the first `slices` slices are allocated.
    async fn ensure_allocated(&self, partition_index: u16, slices: u64) -> Result<(), zx::Status> {
        let inner = self.inner.upgradable_read().await;

        let partition_state = &inner.partition_state[&partition_index];
        let mappings = &partition_state.mappings;

        let mut new_metadata = inner.metadata.clone();
        let max_slice = self.max_slice(&new_metadata);
        let mut new_mappings = Vec::new();
        let mut next_slice = 0;
        let mut allocated = 0;
        for mapping in mappings {
            if mapping.from.start >= slices {
                break;
            }
            let count = mapping.from.start - next_slice;
            if count > 0 {
                new_mappings.push(new_metadata.allocate_slices(
                    partition_index,
                    next_slice,
                    count,
                    max_slice,
                )?);
                allocated += count;
            }
            next_slice = mapping.from.end;
        }
        if next_slice < slices {
            let count = slices - next_slice;
            new_mappings.push(new_metadata.allocate_slices(
                partition_index,
                next_slice,
                count,
                max_slice,
            )?);
            allocated += count;
        }

        if partition_state.slice_limit > 0
            && (inner.metadata.partitions[&partition_index].slices as u64)
                .checked_add(allocated)
                .map_or(true, |s| s > partition_state.slice_limit)
        {
            return Err(zx::Status::NO_SPACE);
        }

        let mut inner =
            self.write_new_metadata(inner, new_metadata).await.map_err(map_to_status)?;

        inner.assigned_slice_count = inner.assigned_slice_count.checked_add(allocated).unwrap();

        for m in new_mappings {
            inner
                .partition_state
                .get_mut(&partition_index)
                .unwrap()
                .mappings
                .insert_contiguous_mappings(m);
        }

        Ok(())
    }

    #[cfg(test)]
    async fn find_partition_with_name(&self, name: &str) -> Option<u16> {
        self.inner
            .read()
            .await
            .metadata
            .partitions
            .iter()
            .find_map(|(&index, p)| (p.name() == name).then_some(index))
    }
}

// Trait to abstract over the difference between reads and writes.
trait IoTrait {
    // Called to get the future that performs the read or write.
    fn add_op(&mut self, offset: u64, len: u64) -> impl Future<Output = Result<(), zx::Status>>;

    fn flush(&mut self) -> impl Future<Output = Result<(), zx::Status>>;
}

struct Read<'a> {
    device: &'a Device,
    vmo_id: &'a VmoId,
    ops: FuturesUnordered<BoxFuture<'a, Result<(), zx::Status>>>,
    vmo_offset: u64,
}

impl IoTrait for Read<'_> {
    async fn add_op(&mut self, offset: u64, len: u64) -> Result<(), zx::Status> {
        self.ops.push(Box::pin(self.device.read_at(
            MutableBufferSlice::new_with_vmo_id(self.vmo_id, self.vmo_offset, len),
            offset,
        )));
        self.vmo_offset += len;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), zx::Status> {
        while self.ops.try_next().await?.is_some() {}
        Ok(())
    }
}

// Reads to memory.
struct ReadToMem<'a> {
    device: &'a Device,
    buffer: &'a mut [u8],
}

impl<'a> ReadToMem<'a> {
    fn new(device: &'a Device, buffer: &'a mut [u8]) -> Self {
        Self { device, buffer }
    }
}

impl IoTrait for ReadToMem<'_> {
    async fn add_op(&mut self, offset: u64, len: u64) -> Result<(), zx::Status> {
        // We don't care about performance, so we issue the operation immediately.
        let (head, tail) = std::mem::take(&mut self.buffer).split_at_mut(len as usize);
        self.buffer = tail;
        self.device.read_at(MutableBufferSlice::Memory(head), offset).await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), zx::Status> {
        Ok(())
    }
}

struct Write<'a> {
    device: &'a Device,
    vmo_id: &'a VmoId,
    ops: FuturesUnordered<BoxFuture<'a, Result<(), zx::Status>>>,
    vmo_offset: u64,
    options: WriteOptions,
}

impl IoTrait for Write<'_> {
    async fn add_op(&mut self, offset: u64, len: u64) -> Result<(), zx::Status> {
        self.ops.push(Box::pin(self.device.write_at_with_opts(
            BufferSlice::new_with_vmo_id(self.vmo_id, self.vmo_offset, len),
            offset,
            self.options,
        )));
        self.vmo_offset += len;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), zx::Status> {
        while self.ops.try_next().await?.is_some() {}
        Ok(())
    }
}

// Writes from memory.
struct WriteFromMem<'a> {
    device: &'a Device,
    buffer: &'a [u8],
}

impl<'a> WriteFromMem<'a> {
    fn new(device: &'a Device, buffer: &'a [u8]) -> Self {
        Self { device, buffer }
    }
}

impl IoTrait for WriteFromMem<'_> {
    async fn add_op(&mut self, offset: u64, len: u64) -> Result<(), zx::Status> {
        // We don't care about performance, so we issue the operation immediately.
        let (head, tail) = self.buffer.split_at(len as usize);
        self.buffer = tail;
        self.device.write_at(BufferSlice::Memory(head), offset).await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), zx::Status> {
        Ok(())
    }
}

struct MountedVolumeInner {
    pub scope: ExecutionScope,
    pub block_server: BlockServer<SessionManager<PartitionInterface>>,
    pub shutdown: AtomicBool,
}

#[derive(Clone)]
struct MountedVolume(Arc<MountedVolumeInner>);

impl Deref for MountedVolume {
    type Target = Arc<MountedVolumeInner>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl MountedVolume {
    pub fn new(block_server: BlockServer<SessionManager<PartitionInterface>>) -> Self {
        Self(Arc::new(MountedVolumeInner {
            scope: ExecutionScope::new(),
            block_server,
            shutdown: AtomicBool::new(false),
        }))
    }
}

impl BlockConnector for MountedVolume {
    fn connect_volume(&self) -> Result<ClientEnd<fvolume::VolumeMarker>, Error> {
        let (client, stream) = fidl::endpoints::create_request_stream();
        let this = self.clone();
        self.scope.spawn(async move {
            let _ = this.block_server.handle_requests(stream).await;
        });
        Ok(client)
    }
}

/// Serves a multi-filesystem component that uses the FVM format.
struct Component {
    export_dir: Arc<vfs::directory::immutable::Simple>,
    scope: ExecutionScope,
    fvm: Mutex<Option<Arc<Fvm>>>,
    mounted: Mutex<HashMap<u16, MountedVolume>>,
    volumes_directory: Arc<vfs::directory::immutable::Simple>,
}

impl Component {
    pub fn new() -> Self {
        Self {
            export_dir: vfs::directory::immutable::simple(),
            scope: ExecutionScope::new(),
            fvm: Mutex::default(),
            mounted: Mutex::default(),
            volumes_directory: vfs::directory::immutable::simple(),
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
        let flags = fio::Flags::PROTOCOL_DIRECTORY
            | fio::PERM_READABLE
            | fio::PERM_WRITABLE
            | fio::PERM_EXECUTABLE;
        ObjectRequest::new(flags, &fio::Options::default(), outgoing_dir).handle(|request| {
            self.export_dir.clone().open3(self.scope.clone(), Path::dot(), flags, request)
        });
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
        let mut fvm = Fvm::open(RemoteBlockClient::new(device.into_proxy()).await?).await?;

        for (&index, partition) in &fvm.inner.get_mut().metadata.partitions {
            self.add_volume_to_volumes_directory(index, &partition.name()).unwrap();
        }

        self.export_dir.add_entry_may_overwrite(
            "volumes",
            self.volumes_directory.clone(),
            /* overwrite: */ true,
        )?;

        *self.fvm.lock().unwrap() = Some(Arc::new(fvm));
        Ok(())
    }

    fn add_volume_to_volumes_directory(
        self: &Arc<Self>,
        index: u16,
        name: &str,
    ) -> Result<(), Error> {
        let weak = Arc::downgrade(self);
        self.volumes_directory.add_entry(
            name,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_volume_requests(requests, index).await;
                    }
                }
            }),
        )?;
        Ok(())
    }

    // NOTE: Only safe after `handle_start` has been called.
    fn fvm(&self) -> Arc<Fvm> {
        self.fvm.lock().unwrap().as_ref().unwrap().clone()
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
                    log::info!(name:?; "Create volume");
                    responder.send(
                        self.handle_create_volume(
                            &name,
                            outgoing_directory,
                            create_options,
                            mount_options,
                        )
                        .await
                        .map_err(|error| {
                            log::warn!(error:?; "Create volume failed");
                            map_to_raw_status(error)
                        }),
                    )?;
                }
                VolumesRequest::Remove { responder, name } => {
                    log::info!(name:?; "Remove volume");
                    responder.send(self.handle_remove_volume(&name).await.map_err(|error| {
                        log::warn!(error:?; "Remove volume failed");
                        map_to_raw_status(error)
                    }))?;
                }
            }
        }
        Ok(())
    }

    async fn handle_volume_requests(
        self: Arc<Self>,
        mut requests: VolumeRequestStream,
        partition_index: u16,
    ) -> Result<(), Error> {
        while let Some(request) = requests.try_next().await? {
            match request {
                VolumeRequest::Check { responder, .. } => {
                    responder.send(Err(zx::Status::NOT_SUPPORTED.into_raw()))?;
                }
                VolumeRequest::Mount { responder, outgoing_directory, options } => responder.send(
                    self.handle_mount(partition_index, outgoing_directory, options, false)
                        .await
                        .map_err(|error| {
                            error!(error:?, partition_index; "Failed to mount volume");
                            map_to_raw_status(error)
                        }),
                )?,
                VolumeRequest::SetLimit { responder, bytes } => {
                    let fvm = self.fvm();
                    let mut inner = fvm.inner.write().await;
                    // slice_size cannot be zero since we check it when we first mount.
                    inner.partition_state.get_mut(&partition_index).unwrap().slice_limit =
                        bytes / inner.metadata.header.slice_size;
                    responder.send(Ok(()))?;
                }
                VolumeRequest::GetLimit { responder } => {
                    let fvm = self.fvm();
                    let inner = fvm.inner.read().await;
                    responder.send(Ok(inner.partition_state[&partition_index].slice_limit
                        * inner.metadata.header.slice_size))?;
                }
            }
        }
        Ok(())
    }

    async fn handle_mount(
        self: &Arc<Self>,
        partition_index: u16,
        server_end: ServerEnd<fio::DirectoryMarker>,
        options: MountOptions,
        format: bool,
    ) -> Result<(), Error> {
        let fvm = self.fvm();
        let partition_info = {
            let inner = fvm.inner.read().await;
            let partition =
                &inner.metadata.partitions.get(&partition_index).ok_or(zx::Status::INTERNAL)?;
            PartitionInfo {
                block_range: None, // Supplied via `get_volume_info`.
                type_guid: partition.type_guid,
                instance_guid: partition.guid,
                name: Some(partition.name().to_string()),
                flags: 0,
            }
        };

        let key = if let Some(crypt) = options.crypt {
            let crypt_proxy = crypt.into_proxy();
            Some(if format {
                zxcrypt::Key::format(&fvm, partition_index, &crypt_proxy).await.unwrap()
            } else {
                zxcrypt::Key::unseal(&fvm, partition_index, &crypt_proxy).await?
            })
        } else {
            None
        };

        let block_server = BlockServer::new(
            fvm.block_size(),
            Arc::new(PartitionInterface { partition_index, partition_info, key, fvm: fvm.clone() }),
        );

        let server_end = server_end.into_channel();

        if let Some(uri) = options.uri {
            // For now we only support URIs of the form: "#meta/<component_name>.cm".
            let re = Regex::new(r"^#meta/(.*)\.cm$").unwrap();
            let Some(caps) = re.captures(&uri) else { bail!(zx::Status::INVALID_ARGS) };

            struct ComponentName(String);
            impl FSConfig for ComponentName {
                fn options(&self) -> Options<'_> {
                    Options {
                        component_name: &self.0,
                        reuse_component_after_serving: false,
                        format_options: FormatOptions::default(),
                        start_options: StartOptions {
                            read_only: false,
                            verbose: false,
                            fsck_after_every_transaction: false,
                            write_compression_level: -1,
                            write_compression_algorithm: CompressionAlgorithm::ZstdChunked,
                            cache_eviction_policy_override: EvictionPolicyOverride::None,
                            startup_profiling_seconds: 0,
                        },
                        component_type: ComponentType::DynamicChild {
                            // Use a separate collection for blobfs because it needs additional
                            // capabilities routed to it (e.g. VmexResource) and we don't want
                            // to route those capabilities to other filesystems.
                            collection_name: if self.0 == "blobfs" {
                                "blobfs-collection".to_string()
                            } else {
                                fs_management::FS_COLLECTION_NAME.to_string()
                            },
                        },
                    }
                }
            }

            let volume = MountedVolume::new(block_server);
            let mut fs = Filesystem::new(volume.clone(), ComponentName(caps[1].to_string()));

            if format {
                fs.format().await?;
            }

            // TODO(https://fxbug.dev/357467643): Support properly shutting down the filesystem.
            // For now, just leak the mounted filesystem.
            let exposed_dir = fs.serve().await?.take_exposed_dir();

            self.mounted.lock().unwrap().insert(partition_index, volume);

            let _ = exposed_dir.open3(
                ".",
                fio::PERM_READABLE
                    | fio::Flags::PERM_INHERIT_EXECUTE
                    | fio::Flags::PERM_INHERIT_WRITE,
                &fio::Options::default(),
                server_end,
            );
        } else {
            // Expose the volume as a block device.
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

            let volume = MountedVolume::new(block_server);
            let volume_clone = volume.clone();
            let volume_scope = volume.scope.clone();
            self.mounted.lock().unwrap().insert(partition_index, volume);

            // Unmount when the last connection is closed (i.e. when `scope` terminates).
            let this = self.clone();
            fasync::Task::spawn(async move {
                volume_clone.scope.wait().await;
                if !volume_clone.shutdown.swap(true, Ordering::Relaxed) {
                    this.mounted.lock().unwrap().remove(&partition_index);
                }
            })
            .detach();

            let flags = fio::Flags::PROTOCOL_DIRECTORY
                | fio::PERM_READABLE
                | fio::PERM_WRITABLE
                | fio::PERM_EXECUTABLE;
            ObjectRequest::new(flags, &fio::Options::default(), server_end)
                .handle(|request| outgoing_dir.open3(volume_scope, Path::dot(), flags, request));
        }

        Ok(())
    }

    async fn handle_volume(
        self: Arc<Self>,
        partition_index: u16,
        requests: fvolume::VolumeRequestStream,
    ) -> Result<(), Error> {
        let partition = self.mounted.lock().unwrap()[&partition_index].clone();
        partition.block_server.handle_requests(requests).await
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
            return Err(anyhow!(zx::Status::INVALID_ARGS).context("Missing type GUID"));
        };
        let guid = create_options.guid.unwrap_or_else(|| Uuid::new_v4().to_bytes_le());
        let slices = match create_options.initial_size {
            Some(x) => {
                if x % inner.metadata.header.slice_size > 0 {
                    return Err(anyhow!(zx::Status::INVALID_ARGS).context("Invalid volume size"));
                }
                (x / inner.metadata.header.slice_size)
                    .try_into()
                    .map_err(|_| anyhow!(zx::Status::INVALID_ARGS).context("Invalid volume size"))?
            }
            None => 1,
        };
        let partition_index = fvm
            .create_partition(inner, type_guid, guid, slices, name)
            .await
            .context("Failed to create partition")?;

        async move {
            self.add_volume_to_volumes_directory(partition_index, name)?;

            self.handle_mount(partition_index, outgoing_directory, mount_options, true).await
        }
        .await
        .map_err(|error| {
            warn!(error:?; "Created partition {name}, but failed to mount");
            error
        })
    }

    async fn handle_remove_volume(self: &Arc<Self>, name: &str) -> Result<(), Error> {
        let fvm = self.fvm.lock().unwrap().as_ref().unwrap().clone();
        let inner = fvm.inner.upgradable_read().await;
        let Some((&partition_index, _)) =
            inner.metadata.partitions.iter().find(|(_, p)| p.name() == name)
        else {
            bail!(zx::Status::NOT_FOUND);
        };
        let volume = self.mounted.lock().unwrap().get(&partition_index).cloned();
        if let Some(volume) = volume {
            volume.scope.shutdown();
            volume.scope.wait().await;
            if !volume.shutdown.swap(true, Ordering::Relaxed) {
                self.mounted.lock().unwrap().remove(&partition_index);
            }
        }
        let mut new_metadata = inner.metadata.clone();
        let mut removed_slices = 0u64;
        for slice_entry in &mut new_metadata.allocations {
            if slice_entry.partition_index() == partition_index {
                slice_entry.0 = 0;
                removed_slices += 1;
            }
        }
        if new_metadata.partitions.remove(&partition_index).unwrap().slices as u64 != removed_slices
        {
            warn!("Mismatch between removed slices and assigned slices");
        }
        let mut inner = fvm.write_new_metadata(inner, new_metadata).await?;
        inner.assigned_slice_count -= removed_slices;
        inner.partition_state.remove(&partition_index);

        if let Err(error) = self.volumes_directory.remove_entry(name, false) {
            warn!(error:?; "Removed volume from FVM, but failed to remove entry from directory");
        }

        Ok(())
    }
}

struct PartitionInterface {
    partition_index: u16,
    partition_info: PartitionInfo,
    key: Option<zxcrypt::Key>,
    fvm: Arc<Fvm>,
}

impl Interface for PartitionInterface {
    async fn on_attach_vmo(&self, vmo: &zx::Vmo) -> Result<(), zx::Status> {
        self.fvm.device.attach_vmo(vmo).await
    }

    fn on_detach_vmo(&self, vmo: &zx::Vmo) {
        self.fvm.device.detach_vmo(vmo);
    }

    async fn get_info(&self) -> Result<Cow<'_, PartitionInfo>, zx::Status> {
        Ok(Cow::Borrowed(&self.partition_info))
    }

    async fn read(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
    ) -> Result<(), zx::Status> {
        debug!(
            "read {}: @{device_block_offset}, count={block_count}, vmo_offset={vmo_offset}",
            self.partition_index
        );
        let device = &self.fvm.device;
        if let Some(key) = &self.key {
            let device_block_offset = device_block_offset
                .checked_add(self.fvm.slice_blocks)
                .ok_or(zx::Status::OUT_OF_RANGE)?;
            self.fvm
                .do_io(
                    zxcrypt::EncryptedRead::new(
                        &device,
                        &key,
                        device_block_offset,
                        vmo,
                        vmo_offset,
                    )
                    .await,
                    self.partition_index,
                    device_block_offset,
                    block_count,
                )
                .await
        } else {
            self.fvm
                .do_io(
                    Read {
                        device: &device,
                        vmo_id: &device.get_vmo_id(&vmo),
                        ops: FuturesUnordered::new(),
                        vmo_offset,
                    },
                    self.partition_index,
                    device_block_offset,
                    block_count,
                )
                .await
        }
        .map_err(|error| {
            warn!(error:?; "Read failed");
            map_to_status(error)
        })
    }

    async fn write(
        &self,
        device_block_offset: u64,
        block_count: u32,
        vmo: &Arc<zx::Vmo>,
        vmo_offset: u64,
        options: WriteOptions,
    ) -> Result<(), zx::Status> {
        debug!(
            "write {}: @{device_block_offset}, count={block_count}, vmo_offset={vmo_offset}",
            self.partition_index
        );
        let device = &self.fvm.device;
        if let Some(key) = &self.key {
            let device_block_offset = device_block_offset
                .checked_add(self.fvm.slice_blocks)
                .ok_or(zx::Status::OUT_OF_RANGE)?;
            self.fvm
                .do_io(
                    zxcrypt::EncryptedWrite::new(
                        &device,
                        &key,
                        device_block_offset,
                        vmo,
                        vmo_offset,
                        options,
                    )
                    .await,
                    self.partition_index,
                    device_block_offset,
                    block_count,
                )
                .await
        } else {
            let vmo_id = device.get_vmo_id(&vmo);
            self.fvm
                .do_io(
                    Write {
                        device,
                        vmo_id: &vmo_id,
                        ops: FuturesUnordered::new(),
                        options,
                        vmo_offset,
                    },
                    self.partition_index,
                    device_block_offset,
                    block_count,
                )
                .await
        }
        .map_err(|error| {
            warn!(error:?; "Write failed");
            map_to_status(error)
        })
    }

    async fn flush(&self) -> Result<(), zx::Status> {
        self.fvm.device.flush().await
    }

    async fn trim(&self, _device_block_offset: u64, _block_count: u32) -> Result<(), zx::Status> {
        todo!();
    }

    async fn get_volume_info(
        &self,
    ) -> Result<(fvolume::VolumeManagerInfo, fvolume::VolumeInfo), zx::Status> {
        let inner = self.fvm.inner.read().await;
        let slice_count = self.fvm.max_slice(&inner.metadata);
        let reserved = if self.key.is_some() { 1 } else { 0 };
        Ok((
            fvolume::VolumeManagerInfo {
                slice_size: inner.metadata.header.slice_size,
                slice_count: slice_count.saturating_sub(reserved),
                assigned_slice_count: inner.assigned_slice_count.saturating_sub(reserved),
                maximum_slice_count: slice_count,
                max_virtual_slice: MAX_SLICE_COUNT.saturating_sub(reserved),
            },
            fvolume::VolumeInfo {
                partition_slice_count: (inner
                    .metadata
                    .partitions
                    .get(&self.partition_index)
                    .unwrap()
                    .slices as u64)
                    .saturating_sub(reserved),
                slice_limit: inner.partition_state[&self.partition_index]
                    .slice_limit
                    .saturating_sub(reserved),
            },
        ))
    }

    async fn query_slices(
        &self,
        start_slices: &[u64],
    ) -> Result<Vec<fvolume::VsliceRange>, zx::Status> {
        let inner = self.fvm.inner.read().await;
        let mappings = &inner.partition_state[&self.partition_index].mappings;
        let mut results = Vec::new();
        for &(mut slice) in start_slices {
            if self.key.is_some() {
                slice = slice.checked_add(1).ok_or(zx::Status::OUT_OF_RANGE)?;
            }
            if slice >= MAX_SLICE_COUNT {
                return Err(zx::Status::OUT_OF_RANGE);
            }
            let index = match mappings.binary_search_by(|m: &Mapping| m.from.start.cmp(&slice)) {
                Ok(index) => index,
                Err(index) if index > 0 => index - 1,
                _ => {
                    results.push(fvolume::VsliceRange {
                        allocated: false,
                        count: MAX_SLICE_COUNT - slice,
                    });
                    continue;
                }
            };
            let mapping = &mappings[index];
            let mut end_slice = mapping.from.end;
            if slice >= end_slice {
                if index + 1 < mappings.len() {
                    results.push(fvolume::VsliceRange {
                        allocated: false,
                        count: mappings[index + 1].from.start - slice,
                    });
                } else {
                    results.push(fvolume::VsliceRange {
                        allocated: false,
                        count: MAX_SLICE_COUNT - slice,
                    });
                }
            } else {
                // Coalesce mappings.
                for mapping in &mappings[index + 1..] {
                    if mapping.from.start != end_slice {
                        break;
                    }
                    end_slice = mapping.from.end;
                }
                results.push(fvolume::VsliceRange { allocated: true, count: end_slice - slice });
            }
        }
        Ok(results)
    }

    async fn extend(&self, start_slice: u64, slice_count: u64) -> Result<(), zx::Status> {
        if slice_count == 0 {
            return Ok(());
        }
        let start_slice = if self.key.is_some() {
            start_slice.checked_add(1).ok_or(zx::Status::OUT_OF_RANGE)?
        } else {
            start_slice
        };
        let inner = self.fvm.inner.upgradable_read().await;

        let partition_state = &inner.partition_state[&self.partition_index];
        let mappings = &partition_state.mappings;

        let check_already_allocated =
            || match mappings.binary_search_by(|m: &Mapping| m.from.start.cmp(&start_slice)) {
                Ok(_) => Err(zx::Status::INVALID_ARGS),
                Err(index) => {
                    if (index > 0 && mappings[index - 1].from.end > start_slice)
                        || (index < mappings.len()
                            && mappings[index].from.start - start_slice < slice_count)
                    {
                        Err(zx::Status::INVALID_ARGS)
                    } else {
                        Ok(index)
                    }
                }
            };

        if let Err(e) = check_already_allocated() {
            warn!("Attempt to allocate vslice {start_slice} that is already allocated.");
            return Err(e);
        }

        if partition_state.slice_limit > 0
            && (inner.metadata.partitions[&self.partition_index].slices as u64)
                .checked_add(slice_count)
                .map_or(true, |s| s > partition_state.slice_limit)
        {
            return Err(zx::Status::NO_SPACE);
        }

        let mut new_metadata = inner.metadata.clone();
        let max_slice = self.fvm.max_slice(&new_metadata);
        let new_mappings = new_metadata.allocate_slices(
            self.partition_index,
            start_slice,
            slice_count,
            max_slice,
        )?;

        let mut inner =
            self.fvm.write_new_metadata(inner, new_metadata).await.map_err(map_to_status)?;

        inner.assigned_slice_count = inner.assigned_slice_count.checked_add(slice_count).unwrap();

        inner
            .partition_state
            .get_mut(&self.partition_index)
            .unwrap()
            .mappings
            .insert_contiguous_mappings(new_mappings);

        Ok(())
    }

    async fn shrink(&self, start_slice: u64, slice_count: u64) -> Result<(), zx::Status> {
        let start_slice = if self.key.is_some() {
            start_slice.checked_add(1).ok_or(zx::Status::OUT_OF_RANGE)?
        } else {
            start_slice
        };
        let inner = self.fvm.inner.upgradable_read().await;
        let mappings = &inner.partition_state[&self.partition_index].mappings;

        // When we're updating the mappings, we might need to update the first and last mapping
        // in the range and then delete the mappings between.
        let delete_start;
        let start_index =
            match mappings.binary_search_by(|m: &Mapping| m.from.start.cmp(&start_slice)) {
                Ok(index) => {
                    delete_start = index;
                    index
                }
                Err(index) if index > 0 => {
                    delete_start = index;
                    index - 1
                }
                _ => return Err(zx::Status::INVALID_ARGS),
            };

        let mut new_metadata = inner.metadata.clone();

        let mut index = start_index;
        let mut slice = start_slice;
        let end_slice = start_slice.checked_add(slice_count).ok_or(zx::Status::INVALID_ARGS)?;
        loop {
            let mapping = &mappings[index];
            if mapping.from.start > slice || mapping.from.end <= slice {
                return Err(zx::Status::INVALID_ARGS);
            }
            let offset = slice - mapping.from.start;
            let start_physical_slice = (mapping.to + offset) as usize;
            let end = std::cmp::min(mapping.from.end, end_slice);
            new_metadata.allocations
                [start_physical_slice..start_physical_slice + (end - slice) as usize]
                .fill(SliceEntry(0));
            slice = end;
            if slice == end_slice {
                break;
            }
            index += 1;
            if index == mappings.len() {
                return Err(zx::Status::INVALID_ARGS);
            }
        }

        let slices = &mut new_metadata.partitions.get_mut(&self.partition_index).unwrap().slices;
        *slices = slices.saturating_sub(slice_count as u32);

        let mut inner =
            self.fvm.write_new_metadata(inner, new_metadata).await.map_err(map_to_status)?;

        inner.assigned_slice_count = inner.assigned_slice_count.checked_sub(slice_count).unwrap();

        let mappings = &mut inner.partition_state.get_mut(&self.partition_index).unwrap().mappings;
        let delete_end = if mappings[index].from.end == slice { index + 1 } else { index };
        if delete_end > delete_start {
            mappings.drain(delete_start..delete_end);
        }

        // Now adjust the first and last mappings if necessary.
        if start_index != delete_start {
            let mapping = &mut mappings[start_index];
            let end = mapping.from.end;
            mapping.from.end = start_slice;

            if end > slice {
                // We need to insert a new mapping to cover the remainder.
                let new_mapping =
                    Mapping { from: slice..end, to: mapping.to + (slice - mapping.from.start) };
                mappings.insert(start_index + 1, new_mapping);

                // This path is for when we're deleting a chunk out of a single mapping.  We don't
                // want to enter the code path below because that's for the case where there is
                // more than one mapping involved.
                return Ok(());
            }
        }

        if delete_end == index {
            let mapping = &mut mappings[start_index + 1];
            let delta = slice - mapping.from.start;
            mapping.from.start = slice;
            mapping.to += delta;
        }

        Ok(())
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
        warn!(error:?; "Internal error");
        zx::Status::INTERNAL
    }
}

#[fuchsia::main(logging_tags = ["fvm"])]
async fn main() -> Result<(), Error> {
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    let component = Arc::new(Component::new());
    component
        .serve(
            fuchsia_runtime::take_startup_handle(HandleType::DirectoryRequest.into())
                .ok_or_else(|| anyhow!("Missing startup handle"))
                .unwrap()
                .into(),
        )
        .await
        .unwrap();
    component.scope.wait().await;
    Ok(())
}

/// Holds memory that has the same alignment as T, but with an arbitrary size.
struct AlignedMem<T>(&'static mut [u8], PhantomData<T>);

impl<T> AlignedMem<T> {
    fn new(size: usize) -> Self {
        Self(
            // SAFETY: Safe by inspection.
            unsafe {
                std::slice::from_raw_parts_mut(
                    alloc::alloc_zeroed(
                        alloc::Layout::from_size_align(size, std::mem::align_of::<T>()).unwrap(),
                    ),
                    size,
                )
            },
            PhantomData,
        )
    }
}

impl<T> Drop for AlignedMem<T> {
    fn drop(&mut self) {
        // SAFETY: Pairs with the allocation in `new`.
        unsafe {
            alloc::dealloc(
                self.0.as_mut_ptr(),
                alloc::Layout::from_size_align(self.0.len(), std::mem::align_of::<T>()).unwrap(),
            );
        }
    }
}

impl<T> Deref for AlignedMem<T> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.0
    }
}

impl<T> DerefMut for AlignedMem<T> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{Component, MAX_SLICE_COUNT};
    use assert_matches::assert_matches;
    use block_client::{
        BlockClient, BufferSlice, MutableBufferSlice, RemoteBlockClient, WriteOptions,
    };
    use fake_block_server::{FakeServer, FakeServerOptions};
    use fidl::endpoints::RequestStream;
    use fidl_fuchsia_fs_startup::{
        CheckOptions, CompressionAlgorithm, CreateOptions, EvictionPolicyOverride, MountOptions,
        StartOptions, StartupMarker, VolumeMarker, VolumesMarker,
    };
    use fidl_fuchsia_hardware_block::BlockMarker;
    use fuchsia_component::client::{
        connect_to_named_protocol_at_dir_root, connect_to_protocol_at_dir_svc,
    };
    use fuchsia_fs::directory::{open_directory, readdir};
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use {
        fidl_fuchsia_hardware_block_volume as fvolume, fidl_fuchsia_io as fio,
        fuchsia_async as fasync,
    };

    struct Fixture {
        component: Arc<Component>,
        outgoing_dir: fio::DirectoryProxy,
        fake_server: Arc<FakeServer>,
    }

    const BLOCK_SIZE: u32 = 512;
    const SLICE_SIZE: u64 = 32768;

    impl Fixture {
        async fn new(extra_space: u64) -> Self {
            let contents = std::fs::read("/pkg/data/golden-fvm.blk").unwrap();
            let fake_server = Arc::new(FakeServer::new(
                (contents.len() as u64 + extra_space) / BLOCK_SIZE as u64,
                BLOCK_SIZE,
                &contents,
            ));
            Self::from_fake_server(fake_server).await
        }

        async fn from_fake_server(fake_server: Arc<FakeServer>) -> Self {
            let (outgoing_dir, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let fixture =
                Fixture { component: Arc::new(Component::new()), outgoing_dir, fake_server };
            let fake_server = fixture.fake_server.clone();
            let (block_client, block_server) =
                fidl::endpoints::create_request_stream::<BlockMarker>();
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

        async fn mount_volume(&self, volume_name: &str) -> fvolume::VolumeProxy {
            let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
                &self.outgoing_dir,
                &format!("volumes/{volume_name}"),
            )
            .unwrap();

            let (dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            volume_proxy
                .mount(dir_server_end, MountOptions::default())
                .await
                .expect("mount failed (FIDL)")
                .expect("mount failed");

            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap()
        }

        fn take_fake_server(self) -> Arc<FakeServer> {
            self.fake_server
        }
    }

    /// Returns the number of assigned slices for the individual volume and the whole FVM volume.
    async fn get_counts(proxy: &fvolume::VolumeProxy) -> (u64, u64) {
        let (status, manager_info, volume_info) = proxy.get_volume_info().await.unwrap();
        assert_eq!(status, zx::sys::ZX_OK);
        (manager_info.unwrap().assigned_slice_count, volume_info.unwrap().partition_slice_count)
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

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        // Look for blobfs's magic:
        let block_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let client = RemoteBlockClient::new(block_proxy).await.unwrap();

        // Make sure FVM passes through the right block size.
        assert_eq!(client.block_size(), BLOCK_SIZE);

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
            client
                .read_at(MutableBufferSlice::Memory(&mut buf), SLICE_SIZE)
                .await
                .expect_err("Read from slice #2 should fail"),
            zx::Status::OUT_OF_RANGE
        );

        // Mount the minfs partition.
        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/data",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let block_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let client = RemoteBlockClient::new(block_proxy).await.unwrap();

        // Check some writes.
        let offsets = [0, 16384, 10 * 8192, 20 * 8192];
        for (index, &offset) in offsets.iter().enumerate() {
            let buf = vec![index as u8; 16384];
            client.write_at(BufferSlice::Memory(&buf), offset).await.unwrap();
        }

        // Read back in reverse.
        for (index, &offset) in offsets.iter().enumerate().rev() {
            let mut read_buf = vec![index as u8; 16384];
            client.read_at(MutableBufferSlice::Memory(&mut read_buf), offset).await.unwrap();
            assert_eq!(&read_buf, &[index as u8; 16384]);
        }
    }

    #[fuchsia::test]
    async fn test_create_volume() {
        let buf = vec![0xaf; 16384];

        let fake_server = {
            let fixture = Fixture::new(SLICE_SIZE).await;

            let volumes_proxy =
                connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

            let (dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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

            // Make sure the volume appears in the volumes directory.
            let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
                &fixture.outgoing_dir,
                "volumes/foo",
            )
            .unwrap();

            // Check we connected by calling the Check method (even though it's unsupported).
            assert_eq!(
                volume_proxy.check(CheckOptions::default()).await.unwrap(),
                Err(zx::sys::ZX_ERR_NOT_SUPPORTED)
            );

            fixture.fake_server
        };

        // Reopen, and check the same reads.
        let fixture = Fixture::from_fake_server(fake_server).await;

        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/foo",
        )
        .unwrap();
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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
        for extra_space in [SLICE_SIZE, SLICE_SIZE * 1024] {
            // Keep creating partitions until we run out of space.
            let mut partition_count = 0;

            let fake_server = {
                let fixture = Fixture::new(extra_space).await;

                let volumes_proxy =
                    connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

                loop {
                    let (_dir_proxy, dir_server_end) =
                        fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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

            log::info!("Created {partition_count} partitions");

            // Reopen and check we can mount all the partitions we created.
            let fixture = Fixture::from_fake_server(fake_server).await;

            for i in 0..partition_count {
                let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
                    &fixture.outgoing_dir,
                    &format!("volumes/foo {i}"),
                )
                .unwrap();

                let (_dir_proxy, dir_server_end) =
                    fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
                volume_proxy
                    .mount(dir_server_end, MountOptions::default())
                    .await
                    .expect("mount failed (FIDL)")
                    .expect("mount failed");
            }
        }
    }

    #[fuchsia::test]
    async fn test_remove_volume() {
        let fixture = Fixture::new(SLICE_SIZE).await;

        let volumes_proxy =
            connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

        let (_dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volumes_proxy
            .create(
                "foo",
                dir_server_end,
                CreateOptions { type_guid: Some([1; 16]), ..CreateOptions::default() },
                MountOptions::default(),
            )
            .await
            .expect("create failed (FIDL)")
            .expect("create failed");

        // Creating another volume should fail because there won't be enough space.
        let (_dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        assert_eq!(
            volumes_proxy
                .create(
                    "bar",
                    dir_server_end,
                    CreateOptions { type_guid: Some([1; 16]), ..CreateOptions::default() },
                    MountOptions::default(),
                )
                .await
                .expect("create failed (FIDL)"),
            Err(zx::sys::ZX_ERR_NO_SPACE)
        );

        volumes_proxy.remove("foo").await.expect("remove failed (FIDL)").expect("remove failed");

        // Creating should now succeed.
        let (_dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volumes_proxy
            .create(
                "bar",
                dir_server_end,
                CreateOptions { type_guid: Some([1; 16]), ..CreateOptions::default() },
                MountOptions::default(),
            )
            .await
            .expect("create failed (FIDL)")
            .expect("create failed");

        let expected = HashSet::from(["bar".to_string(), "blobfs".to_string(), "data".to_string()]);
        let volumes_dir =
            open_directory(&fixture.outgoing_dir, "volumes", fio::Flags::empty()).await.unwrap();
        assert_eq!(
            &readdir(&volumes_dir)
                .await
                .unwrap()
                .into_iter()
                .map(|d| d.name)
                .collect::<HashSet<_>>(),
            &expected,
        );

        // Check again after a remount.
        let fixture = Fixture::from_fake_server(fixture.take_fake_server()).await;
        let volumes_dir =
            open_directory(&fixture.outgoing_dir, "volumes", fio::Flags::empty()).await.unwrap();
        assert_eq!(
            &readdir(&volumes_dir)
                .await
                .unwrap()
                .into_iter()
                .map(|d| d.name)
                .collect::<HashSet<_>>(),
            &expected,
        );
    }

    #[fuchsia::test]
    async fn test_create_mount_remove() {
        let fixture = Fixture::new(SLICE_SIZE).await;

        let volumes_proxy =
            connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

        {
            let (_dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            volumes_proxy
                .create(
                    "foo",
                    dir_server_end,
                    CreateOptions { type_guid: Some([1; 16]), ..CreateOptions::default() },
                    MountOptions::default(),
                )
                .await
                .expect("create failed (FIDL)")
                .expect("create failed");

            volumes_proxy
                .remove("foo")
                .await
                .expect("remove failed (FIDL)")
                .expect("remove failed");
        }

        let expected = HashSet::from(["blobfs".to_string(), "data".to_string()]);
        let volumes_dir =
            open_directory(&fixture.outgoing_dir, "volumes", fio::Flags::empty()).await.unwrap();
        assert_eq!(
            &readdir(&volumes_dir)
                .await
                .unwrap()
                .into_iter()
                .map(|d| d.name)
                .collect::<HashSet<_>>(),
            &expected,
        );

        // Check again after a remount.
        let fixture = Fixture::from_fake_server(fixture.take_fake_server()).await;
        let volumes_dir =
            open_directory(&fixture.outgoing_dir, "volumes", fio::Flags::empty()).await.unwrap();
        assert_eq!(
            &readdir(&volumes_dir)
                .await
                .unwrap()
                .into_iter()
                .map(|d| d.name)
                .collect::<HashSet<_>>(),
            &expected,
        );
    }

    #[fuchsia::test]
    async fn test_get_volume_info() {
        let fixture = Fixture::new(0).await;

        // Mount the minfs partition.
        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/data",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let volume_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let (status, manager_info, volume_info) = volume_proxy.get_volume_info().await.unwrap();

        assert_eq!(status, zx::sys::ZX_OK);
        assert_matches!(
            manager_info.as_deref(),
            Some(fvolume::VolumeManagerInfo {
                slice_size: SLICE_SIZE,
                max_virtual_slice: MAX_SLICE_COUNT,
                assigned_slice_count,
                ..
            }) if *assigned_slice_count > 0
        );
        assert_matches!(
            volume_info.as_deref(),
            Some(fvolume::VolumeInfo { partition_slice_count: 257, .. })
        );
        // Make sure assigned_slice_count matches block_count from GetInfo.
        let info = volume_proxy.get_info().await.unwrap().expect("get_info failed");
        assert_eq!(
            info.block_count,
            volume_info.unwrap().partition_slice_count * SLICE_SIZE / BLOCK_SIZE as u64
        );
    }

    #[fuchsia::test]
    async fn test_query_slices() {
        let fixture = Fixture::new(0).await;

        // Mount the blobfs partition.
        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/blobfs",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let volume_proxy =
            Arc::new(connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap());

        let check = |start, allocated, count| {
            let volume_proxy = volume_proxy.clone();
            async move {
                let (status, ranges, range_count) =
                    volume_proxy.query_slices(&[start]).await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(range_count, 1);
                assert_eq!(ranges[0], fvolume::VsliceRange { allocated, count });
                range_count
            }
        };

        let mut slice = 0;
        loop {
            let (status, ranges, _range_count) = volume_proxy.query_slices(&[slice]).await.unwrap();
            if status != 0 {
                break;
            }
            println!("{slice}: {:?}", ranges[0]);
            slice += ranges[0].count;
        }

        check(0, true, 1).await;
        check(1, false, 0x4000 - 1).await;
        check(2, false, 0x4000 - 2).await;
        check(0x4000, true, 1).await;
        check(0x8000, true, 20).await;
        check(0x8001, true, 19).await;
        check(0x8000 + 20, false, 0x4000 - 20).await;
        check(0xc000, true, 17).await;
        check(0xc000 + 17, false, 0x4000 - 17).await;
        check(0x10000, true, 1).await;
        check(0x10001, false, MAX_SLICE_COUNT - 0x10001).await;
    }

    #[fuchsia::test]
    async fn test_extend() {
        let fixture = Fixture::new(SLICE_SIZE * 3).await;

        // Mount the blobfs partition.
        let volume_proxy = fixture.mount_volume("blobfs").await;

        assert_eq!(volume_proxy.extend(1, 1).await.expect("extend failed (FIDL)"), zx::sys::ZX_OK);

        let client = RemoteBlockClient::new(&volume_proxy).await.unwrap();

        // A write and read spanning the first two slices should now succeed.
        let buf = vec![0xef; 16384];
        let offset = SLICE_SIZE - 8192;
        client.write_at(BufferSlice::Memory(&buf), offset).await.unwrap();
        let mut read_buf = vec![0; 16384];
        client.read_at(MutableBufferSlice::Memory(&mut read_buf), offset).await.unwrap();
        assert_eq!(&buf, &read_buf);

        // Check that query_slices shows the allocation.
        let (status, ranges, range_count) = volume_proxy.query_slices(&[0]).await.unwrap();
        assert_eq!(status, zx::sys::ZX_OK);
        assert_eq!(range_count, 1);
        assert_eq!(ranges[0], fvolume::VsliceRange { allocated: true, count: 2 });

        // Try again, and it should fail.
        assert_eq!(
            volume_proxy.extend(1, 1).await.expect("extend failed (FIDL)"),
            zx::sys::ZX_ERR_INVALID_ARGS
        );

        // Same, but with overlapping ranges.
        assert_eq!(
            volume_proxy.extend(0x4000 - 2, 4).await.expect("extend failed (FIDL)"),
            zx::sys::ZX_ERR_INVALID_ARGS
        );
        assert_eq!(
            volume_proxy.extend(0x8005, 20).await.expect("extend failed (FIDL)"),
            zx::sys::ZX_ERR_INVALID_ARGS
        );

        // Extend the minfs partition, so that the extension of the blobfs volume is not contiguous.
        let data_volume_proxy = fixture.mount_volume("data").await;

        assert_eq!(
            data_volume_proxy.extend(0xa0000, 1).await.expect("extend failed (FIDL)"),
            zx::sys::ZX_OK
        );

        // Extend the blobfs partition again.
        assert_eq!(volume_proxy.extend(2, 1).await.expect("extend failed (FIDL)"), zx::sys::ZX_OK);

        // Write to the blobfs partition over the two slices we have extended by.
        let data = vec![0x73; SLICE_SIZE as usize * 2];
        client.write_at(BufferSlice::Memory(&data), SLICE_SIZE).await.unwrap();

        // Now read it back.
        let mut buf = vec![0; SLICE_SIZE as usize * 2];
        client.read_at(MutableBufferSlice::Memory(&mut buf), SLICE_SIZE).await.unwrap();

        assert_eq!(&buf, &data);

        // Check we get the same when we remount.
        let fixture = Fixture::from_fake_server(fixture.take_fake_server()).await;
        let client = RemoteBlockClient::new(&fixture.mount_volume("blobfs").await).await.unwrap();

        buf.fill(0);
        client.read_at(MutableBufferSlice::Memory(&mut buf), SLICE_SIZE).await.unwrap();

        assert_eq!(&buf, &data);
    }

    #[fuchsia::test]
    async fn test_shrink() {
        let final_checks;

        let initial_counts;

        let fake_server = {
            let fixture = Fixture::new(23 * SLICE_SIZE).await;

            let volumes_proxy =
                connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

            // Mount the blobfs partition.
            let blobfs_volume = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
                &fixture.outgoing_dir,
                "volumes/blobfs",
            )
            .unwrap();

            let (dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            blobfs_volume
                .mount(dir_server_end, MountOptions::default())
                .await
                .expect("mount failed (FIDL)")
                .expect("mount failed");

            let blobfs_volume_proxy =
                connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();

            // Record the initial assigned slice count via the blobfs_volume.
            let blobfs_initial_counts = get_counts(&blobfs_volume_proxy).await;

            // Create a new volume.

            let (dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            volumes_proxy
                .create(
                    "foo",
                    dir_server_end,
                    CreateOptions {
                        initial_size: Some(SLICE_SIZE * 5),
                        type_guid: Some([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
                        ..CreateOptions::default()
                    },
                    MountOptions::default(),
                )
                .await
                .expect("create failed (FIDL)")
                .expect("create failed");

            let volume_proxy =
                connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();

            let get_counts = || get_counts(&volume_proxy);
            initial_counts = get_counts().await;
            assert_eq!(initial_counts, (blobfs_initial_counts.0 + 5, 5));

            // Extend the blobfs volume so we get some fragmentation
            assert_eq!(
                blobfs_volume_proxy.extend(1, 1).await.expect("extend failed (FIDL)"),
                zx::sys::ZX_OK
            );

            assert_eq!(get_counts().await, (initial_counts.0 + 1, 5));

            // Extend the volume we created by another 5 slices.
            assert_eq!(
                volume_proxy.extend(5, 5).await.expect("extend failed (FIDL)"),
                zx::sys::ZX_OK
            );

            assert_eq!(get_counts().await, (initial_counts.0 + 6, 10));

            // And again...
            assert_eq!(
                blobfs_volume_proxy.extend(2, 1).await.expect("extend failed (FIDL)"),
                zx::sys::ZX_OK
            );
            assert_eq!(
                volume_proxy.extend(10, 5).await.expect("extend failed (FIDL)"),
                zx::sys::ZX_OK
            );

            // And again...
            assert_eq!(
                blobfs_volume_proxy.extend(3, 1).await.expect("extend failed (FIDL)"),
                zx::sys::ZX_OK
            );
            assert_eq!(
                volume_proxy.extend(15, 5).await.expect("extend failed (FIDL)"),
                zx::sys::ZX_OK
            );

            assert_eq!(get_counts().await, (initial_counts.0 + 18, 20));

            // Write to every slice.
            let client = RemoteBlockClient::new(&volume_proxy).await.unwrap();
            for i in 0..20 {
                let buf = vec![i; SLICE_SIZE as usize];
                client.write_at(BufferSlice::Memory(&buf), i as u64 * SLICE_SIZE).await.unwrap();
            }

            // Shrink, and check with QuerySlices.
            assert_eq!(
                volume_proxy.shrink(4, 7).await.expect("shrink failed (FIDL)"),
                zx::sys::ZX_OK
            );

            assert_eq!(get_counts().await, (initial_counts.0 + 11, 13));

            let (status, ranges, range_count) =
                volume_proxy.query_slices(&[0, 4, 11]).await.unwrap();
            assert_eq!(status, zx::sys::ZX_OK);
            assert_eq!(range_count, 3);
            assert_eq!(
                &ranges[..3],
                &[
                    fvolume::VsliceRange { allocated: true, count: 4 },
                    fvolume::VsliceRange { allocated: false, count: 7 },
                    fvolume::VsliceRange { allocated: true, count: 9 }
                ]
            );

            // Delete the last range we added, which should occupy a whole mapping.
            assert_eq!(
                volume_proxy.shrink(15, 5).await.expect("shrink failed (FIDL)"),
                zx::sys::ZX_OK
            );

            assert_eq!(get_counts().await, (initial_counts.0 + 6, 8));

            let (status, ranges, range_count) =
                volume_proxy.query_slices(&[0, 4, 11, 15]).await.unwrap();
            assert_eq!(status, zx::sys::ZX_OK);
            assert_eq!(range_count, 4);
            assert_eq!(
                &ranges[..4],
                &[
                    fvolume::VsliceRange { allocated: true, count: 4 },
                    fvolume::VsliceRange { allocated: false, count: 7 },
                    fvolume::VsliceRange { allocated: true, count: 4 },
                    fvolume::VsliceRange { allocated: false, count: MAX_SLICE_COUNT - 15 }
                ]
            );

            // Delete a chunk within a single mapping.
            assert_eq!(
                volume_proxy.shrink(1, 2).await.expect("shrink failed (FIDL)"),
                zx::sys::ZX_OK
            );

            assert_eq!(get_counts().await, (initial_counts.0 + 4, 6));

            // Some checks that we also want to perform after reopening.
            final_checks = |volume_proxy: fvolume::VolumeProxy| async move {
                let (status, ranges, range_count) =
                    volume_proxy.query_slices(&[0, 1, 3, 4, 11, 15]).await.unwrap();
                assert_eq!(status, zx::sys::ZX_OK);
                assert_eq!(range_count, 6);
                assert_eq!(
                    &ranges[..6],
                    &[
                        fvolume::VsliceRange { allocated: true, count: 1 },
                        fvolume::VsliceRange { allocated: false, count: 2 },
                        fvolume::VsliceRange { allocated: true, count: 1 },
                        fvolume::VsliceRange { allocated: false, count: 7 },
                        fvolume::VsliceRange { allocated: true, count: 4 },
                        fvolume::VsliceRange { allocated: false, count: MAX_SLICE_COUNT - 15 }
                    ]
                );

                // Read back and check all slices.
                let client = RemoteBlockClient::new(&volume_proxy).await.unwrap();
                for i in [0, 3, 11, 12, 13, 14] {
                    let mut buf = vec![0; SLICE_SIZE as usize];
                    client
                        .read_at(MutableBufferSlice::Memory(&mut buf), i as u64 * SLICE_SIZE)
                        .await
                        .unwrap();
                    assert_eq!(&buf, &vec![i; SLICE_SIZE as usize]);
                }
            };

            final_checks(volume_proxy).await;

            fixture.fake_server
        };

        // Reopen, and check we get the same.
        let fixture = Fixture::from_fake_server(fake_server).await;

        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/foo",
        )
        .unwrap();
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let volume_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();

        assert_eq!(get_counts(&volume_proxy).await, (initial_counts.0 + 4, 6));

        final_checks(volume_proxy).await;
    }

    #[fuchsia::test]
    async fn test_limits() {
        let fixture = Fixture::new(10 * SLICE_SIZE).await;

        // Mount the blobfs partition.
        let blobfs_volume = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/blobfs",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        blobfs_volume
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let volume_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();

        let (status, manager_info, volume_info) = volume_proxy.get_volume_info().await.unwrap();
        assert_eq!(status, zx::sys::ZX_OK);

        // Set the limit for Blobfs to be just one more slice.
        let limit =
            (volume_info.unwrap().partition_slice_count + 1) * manager_info.unwrap().slice_size;
        blobfs_volume
            .set_limit(limit)
            .await
            .expect("set_limit FIDL failed")
            .expect("set_limit failed");

        assert_eq!(
            blobfs_volume
                .get_limit()
                .await
                .expect("get_limit FIDL failed")
                .expect("get_limmit_failed"),
            limit
        );

        // Try and extend by two slices and it should fail with out-of-space.
        assert_eq!(
            volume_proxy.extend(1, 2).await.expect("extend failed (FIDL)"),
            zx::sys::ZX_ERR_NO_SPACE
        );

        // But one should be fine.
        assert_eq!(volume_proxy.extend(1, 1).await.expect("extend failed (FIDL)"), zx::sys::ZX_OK);

        // And then another one should fail.
        assert_eq!(
            volume_proxy.extend(2, 1).await.expect("extend failed (FIDL)"),
            zx::sys::ZX_ERR_NO_SPACE
        );
    }

    #[fuchsia::test]
    async fn test_force_access_passed_through() {
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
            ) -> fake_block_server::WriteAction {
                assert_eq!(
                    opts.contains(WriteOptions::FORCE_ACCESS),
                    self.0.load(Ordering::Relaxed)
                );
                fake_block_server::WriteAction::Write
            }
        }

        let contents = std::fs::read("/pkg/data/golden-fvm.blk").unwrap();
        let fake_server = Arc::new(
            FakeServerOptions {
                block_count: Some(contents.len() as u64 / BLOCK_SIZE as u64),
                block_size: BLOCK_SIZE,
                initial_content: Some(&contents),
                observer: Some(Box::new(Observer(expect_force_access.clone()))),
                ..Default::default()
            }
            .into(),
        );

        let fixture = Fixture::from_fake_server(fake_server).await;

        let volume_proxy = connect_to_named_protocol_at_dir_root::<VolumeMarker>(
            &fixture.outgoing_dir,
            "volumes/data",
        )
        .unwrap();

        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        volume_proxy
            .mount(dir_server_end, MountOptions::default())
            .await
            .expect("mount failed (FIDL)")
            .expect("mount failed");

        let client = RemoteBlockClient::new(
            &connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap(),
        )
        .await
        .unwrap();

        let buffer = vec![0; BLOCK_SIZE as usize];
        client.write_at(BufferSlice::Memory(&buffer), 0).await.unwrap();

        expect_force_access.store(true, Ordering::Relaxed);

        client
            .write_at_with_opts(BufferSlice::Memory(&buffer), 0, WriteOptions::FORCE_ACCESS)
            .await
            .unwrap();
    }

    #[fuchsia::test]
    async fn test_flush_after_metadata_write() {
        let flush_called = Arc::new(AtomicBool::new(false));

        struct Observer(Arc<AtomicBool>);

        impl fake_block_server::Observer for Observer {
            fn flush(&self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }

        let contents = std::fs::read("/pkg/data/golden-fvm.blk").unwrap();
        let fake_server = Arc::new(
            FakeServerOptions {
                block_count: Some((contents.len() as u64 + SLICE_SIZE) / BLOCK_SIZE as u64),
                block_size: BLOCK_SIZE,
                initial_content: Some(&contents),
                observer: Some(Box::new(Observer(flush_called.clone()))),
                ..Default::default()
            }
            .into(),
        );

        let fixture = Fixture::from_fake_server(fake_server).await;

        let volumes_proxy =
            connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

        assert!(!flush_called.load(Ordering::Relaxed));

        let (_dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
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

        assert!(flush_called.load(Ordering::Relaxed));
    }

    #[fuchsia::test]
    async fn ensure_allocated() {
        let fixture = Fixture::new(10 * SLICE_SIZE).await;
        let volumes_proxy =
            connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();
        let (dir_proxy, dir_server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();

        volumes_proxy
            .create(
                "foo",
                dir_server_end,
                CreateOptions { type_guid: Some([1; 16]), ..CreateOptions::default() },
                MountOptions::default(),
            )
            .await
            .expect("create failed (FIDL)")
            .expect("create failed");

        let volume_proxy =
            connect_to_protocol_at_dir_svc::<fvolume::VolumeMarker>(&dir_proxy).unwrap();
        let (original_manager_slice_count, _) = get_counts(&volume_proxy).await;

        assert_eq!(volume_proxy.extend(3, 1).await.expect("extend failed (FIDL)"), zx::sys::ZX_OK);

        let fvm = fixture.component.fvm();
        let partition_index = fvm.find_partition_with_name("foo").await.unwrap();
        fvm.ensure_allocated(partition_index, 5).await.unwrap();

        let (status, ranges, range_count) = volume_proxy.query_slices(&[0, 5]).await.unwrap();
        assert_eq!(status, zx::sys::ZX_OK);
        assert_eq!(range_count, 2);
        assert_eq!(
            &ranges[..2],
            &[
                fvolume::VsliceRange { allocated: true, count: 5 },
                fvolume::VsliceRange { allocated: false, count: MAX_SLICE_COUNT - 5 }
            ]
        );

        let (manager_slice_count, partition_slice_count) = get_counts(&volume_proxy).await;
        // When the volume was created, it uses 1 slice.
        assert_eq!(manager_slice_count, original_manager_slice_count + 4);
        assert_eq!(partition_slice_count, 5);

        // Write to the assigned slices and then read back.
        let client = RemoteBlockClient::new(&volume_proxy).await.unwrap();
        let mut data = vec![0; SLICE_SIZE as usize * 5];
        for (i, chunk) in data.chunks_exact_mut(BLOCK_SIZE as usize).enumerate() {
            chunk.fill(i as u8);
        }

        client.write_at(BufferSlice::Memory(&data), 0).await.unwrap();

        let mut read = vec![0; SLICE_SIZE as usize * 5];
        client.read_at(MutableBufferSlice::Memory(&mut read), 0).await.unwrap();

        assert_eq!(&read, &data);
    }

    #[fuchsia::test(threads = 2)]
    async fn test_unmount_volume_race() {
        let fixture = Fixture::new(SLICE_SIZE).await;

        let volumes_proxy =
            connect_to_protocol_at_dir_svc::<VolumesMarker>(&fixture.outgoing_dir).unwrap();

        for _ in 0..100 {
            let (dir_proxy, dir_server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
            let volumes_proxy = volumes_proxy.clone();
            volumes_proxy
                .create(
                    "foo",
                    dir_server_end,
                    CreateOptions { type_guid: Some([1; 16]), ..CreateOptions::default() },
                    MountOptions::default(),
                )
                .await
                .expect("create failed (FIDL)")
                .expect("create failed");

            // Drop the volume in one thread, remove it explicitly in the other.  Neither branch
            // should ever fail, regardless of who wins the race.
            futures::join! {
                async move {
                    std::mem::drop(dir_proxy);
                },
                async move {
                    volumes_proxy
                        .remove("foo").await.expect("remove failed (FIDL)").expect("remove failed");
                },
            };

            let expected = HashSet::from(["blobfs".to_string(), "data".to_string()]);
            let volumes_dir = open_directory(&fixture.outgoing_dir, "volumes", fio::Flags::empty())
                .await
                .unwrap();
            assert_eq!(
                &readdir(&volumes_dir)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|d| d.name)
                    .collect::<HashSet<_>>(),
                &expected,
            );
        }
    }
}
