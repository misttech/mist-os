// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// PersistentLayer object format
//
// The layer is made up of 1 or more "blocks" whose size are some multiple of the block size used
// by the underlying handle.
//
// The persistent layer has 4 types of blocks:
//  - Header block
//  - Data block
//  - BloomFilter block
//  - Seek block (+LayerInfo)
//
// The structure of the file is as follows:
//
// blk#     contents
// 0        [Header]
// 1        [Data]
// 2        [Data]
// ...      [Data]
// L        [BloomFilter]
// L + 1    [BloomFilter]
// ...      [BloomFilter]
// M        [Seek]
// M + 1    [Seek]
// ...      [Seek]
// N        [Seek/LayerInfo]
//
// Generally, there will be an order of magnitude more Data blocks than Seek/BloomFilter blocks.
//
// Header contains a Version-prefixed LayerHeader struct.  This version is used for everything in
// the layer file.
//
// Data blocks contain a little endian encoded u16 item count at the start, then a series of
// serialized items, and a list of little endian u16 offsets within the block for where
// serialized items start, excluding the first item (since it is at a known offset). The list of
// offsets ends at the end of the block and since the items are of variable length, there may be
// space between the two sections if the next item and its offset cannot fit into the block.
//
// |item_count|item|item|item|item|item|item|dead space|offset|offset|offset|offset|offset|
//
// BloomFilter blocks contain a bitmap which is used to probabilistically determine if a given key
// might exist in the layer file.   See `BloomFilter` for details on this structure.  Note that this
// can be absent from the file for small layer files.
//
// Seek/LayerInfo blocks contain both the seek table, and a single LayerInfo struct at the tail of
// the last block, with the LayerInfo's length written as a little-endian u64 at the very end.  The
// padding between the two structs is ignored but nominally is zeroed. They share blocks to avoid
// wasting padding bytes.  Note that the seek table can be absent from the file for small layer
// files (but there will always be one block for the LayerInfo).
//
// The seek table consists of a little-endian u64 for every data block except for the first one. The
// entries should be monotonically increasing, as they represent some mapping for how the keys for
// the first item in each block would be predominantly sorted, and there may be duplicate entries.
// There should be exactly as many seek blocks as are required to house one entry fewer than the
// number of data blocks.

use crate::drop_event::DropEvent;
use crate::errors::FxfsError;
use crate::filesystem::MAX_BLOCK_SIZE;
use crate::log::*;
use crate::lsm_tree::bloom_filter::{BloomFilterReader, BloomFilterStats, BloomFilterWriter};
use crate::lsm_tree::types::{
    BoxedLayerIterator, FuzzyHash, Item, ItemCount, ItemRef, Key, Layer, LayerIterator, LayerValue,
    LayerWriter,
};
use crate::object_handle::{ObjectHandle, ReadObjectHandle, WriteBytes};
use crate::object_store::caching_object_handle::{CachedChunk, CachingObjectHandle, CHUNK_SIZE};
use crate::round::{how_many, round_down, round_up};
use crate::serialized_types::{
    Version, Versioned, VersionedLatest, LATEST_VERSION, NEW_PERSISTENT_LAYER_VERSION,
};
use anyhow::{anyhow, bail, ensure, Context, Error};
use async_trait::async_trait;
use byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};
use fprint::TypeFingerprint;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use static_assertions::const_assert;
use std::cmp::Ordering;
use std::io::{Read, Write as _};
use std::marker::PhantomData;
use std::ops::Bound;
use std::sync::{Arc, Mutex};

const PERSISTENT_LAYER_MAGIC: &[u8; 8] = b"FxfsLayr";

/// LayerHeader is stored in the first block of the persistent layer.
pub type LayerHeader = LayerHeaderV39;

#[derive(Debug, Serialize, Deserialize, TypeFingerprint, Versioned)]
pub struct LayerHeaderV39 {
    /// 'FxfsLayr'
    magic: [u8; 8],
    /// The block size used within this layer file. This is typically set at compaction time to the
    /// same block size as the underlying object handle.
    ///
    /// (Each block starts with a 2 byte item count so there is a 64k item limit per block,
    /// regardless of block size).
    block_size: u64,
}

/// The last block of each layer contains metadata for the rest of the layer.
pub type LayerInfo = LayerInfoV39;

#[derive(Debug, Serialize, Deserialize, TypeFingerprint, Versioned)]
pub struct LayerInfoV39 {
    /// How many items are in the layer file.  Mainly used for sizing bloom filters during
    /// compaction.
    num_items: usize,
    /// The number of data blocks in the layer file.
    num_data_blocks: u64,
    /// The size of the bloom filter in the layer file.  Not necessarily block-aligned.
    bloom_filter_size_bytes: usize,
    /// The seed for the nonces used in the bloom filter.
    bloom_filter_seed: u64,
    /// How many nonces to use for bloom filter hashing.
    bloom_filter_num_nonces: usize,
}

/// Prior to V39, the old layer file format is used.  The LayerInfo was stored in the first block.
pub type OldLayerInfo = OldLayerInfoV32;

#[derive(Debug, Serialize, Deserialize, TypeFingerprint, Versioned)]
pub struct OldLayerInfoV32 {
    /// The version of the key and value structs serialized in this layer.
    key_value_version: Version,
    /// The block size used within this layer file. This is typically set at compaction time to the
    /// same block size as the underlying object handle.
    ///
    /// (Each block starts with a 2 byte item count so there is a 64k item limit per block,
    /// regardless of block size).
    block_size: u64,
}

/// A handle to a persistent layer.
pub struct PersistentLayer<K, V> {
    // We retain a reference to the underlying object handle so we can hand out references to it for
    // `Layer::handle` when clients need it.  Internal reads should go through
    // `caching_object_handle` so they are cached.  Note that `CachingObjectHandle` used to
    // implement `ReadObjectHandle`, but that was removed so that `CachingObjectHandle` could hand
    // out data references rather than requiring copying to a buffer, which speeds up LSM tree
    // operations.
    object_handle: Arc<dyn ReadObjectHandle>,
    caching_object_handle: CachingObjectHandle<Arc<dyn ReadObjectHandle>>,
    version: Version,
    block_size: u64,
    data_size: u64,
    seek_table: Vec<u64>,
    num_items: Option<usize>,
    bloom_filter: Option<BloomFilterReader<K>>,
    bloom_filter_stats: Option<BloomFilterStats>,
    close_event: Mutex<Option<Arc<DropEvent>>>,
    _value_type: PhantomData<V>,
}

#[derive(Debug)]
struct BufferCursor {
    chunk: Option<CachedChunk>,
    pos: usize,
}

impl std::io::Read for BufferCursor {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let chunk = if let Some(chunk) = &self.chunk {
            chunk
        } else {
            return Ok(0);
        };
        let to_read = std::cmp::min(buf.len(), chunk.len().saturating_sub(self.pos));
        if to_read > 0 {
            buf[..to_read].copy_from_slice(&chunk[self.pos..self.pos + to_read]);
            self.pos += to_read;
        }
        Ok(to_read)
    }
}

const MIN_BLOCK_SIZE: u64 = 512;

// For small layer files, don't bother with the bloom filter.  Arbitrarily chosen.
const MINIMUM_DATA_BLOCKS_FOR_BLOOM_FILTER: usize = 4;

// How many blocks we reserve for the header.  Data blocks start at this offset.
const NUM_HEADER_BLOCKS: u64 = 1;

/// The smallest possible (empty) layer file is always 2 blocks, one for the header and one for
/// LayerInfo.
const MINIMUM_LAYER_FILE_BLOCKS: u64 = 2;

// Put safety rails on the size of the bloom filter and seek table to avoid OOMing the system.
// It's more likely that tampering has occurred in these cases.
const MAX_BLOOM_FILTER_SIZE: usize = 64 * 1024 * 1024;
const MAX_SEEK_TABLE_SIZE: usize = 64 * 1024 * 1024;

// The following constants refer to sizes of metadata in the data blocks.
const PER_DATA_BLOCK_HEADER_SIZE: usize = 2;
const PER_DATA_BLOCK_SEEK_ENTRY_SIZE: usize = 2;

// A key-only iterator, used while seeking through the tree.
struct KeyOnlyIterator<'iter, K: Key, V: LayerValue> {
    // Allocated out of |layer|.
    buffer: BufferCursor,

    layer: &'iter PersistentLayer<K, V>,

    // The position of the _next_ block to be read.
    pos: u64,

    // The item index in the current block.
    item_index: u16,

    // The number of items in the current block.
    item_count: u16,

    // The current key.
    key: Option<K>,

    // Set by a wrapping iterator once the value has been deserialized, so the KeyOnlyIterator knows
    // whether it is pointing at the next key or not.
    value_deserialized: bool,
}

impl<K: Key, V: LayerValue> KeyOnlyIterator<'_, K, V> {
    fn new<'iter>(layer: &'iter PersistentLayer<K, V>, pos: u64) -> KeyOnlyIterator<'iter, K, V> {
        assert!(pos % layer.block_size == 0);
        KeyOnlyIterator {
            layer,
            buffer: BufferCursor { chunk: None, pos: pos as usize % CHUNK_SIZE },
            pos,
            item_index: 0,
            item_count: 0,
            key: None,
            value_deserialized: false,
        }
    }

    // Repositions the iterator to point to the `index`'th item in the current block.
    // Returns an error if the index is out of range or the resulting offset contains an obviously
    // invalid value.
    fn seek_to_block_item(&mut self, index: u16) -> Result<(), Error> {
        ensure!(index < self.item_count, FxfsError::OutOfRange);
        if index == self.item_index && self.value_deserialized {
            // Fast-path when we are seeking in a linear manner, as is the case when advancing a
            // wrapping iterator that also deserializes the values.
            return Ok(());
        }
        let offset_in_block = if index == 0 {
            // First entry isn't actually recorded, it is at the start of the block after the item
            // count.
            PER_DATA_BLOCK_HEADER_SIZE
        } else {
            let old_buffer_pos = self.buffer.pos;
            self.buffer.pos = round_up(self.buffer.pos, self.layer.block_size as usize).unwrap()
                - (PER_DATA_BLOCK_SEEK_ENTRY_SIZE * (usize::from(self.item_count - index)));
            let res = self.buffer.read_u16::<LittleEndian>();
            self.buffer.pos = old_buffer_pos;
            let offset_in_block = res.context("Failed to read offset")? as usize;
            if offset_in_block >= self.layer.block_size as usize
                || offset_in_block <= PER_DATA_BLOCK_HEADER_SIZE
            {
                return Err(anyhow!(FxfsError::Inconsistent))
                    .context(format!("Offset {} is out of valid range.", offset_in_block));
            }
            offset_in_block
        };
        self.item_index = index;
        self.buffer.pos =
            round_down(self.buffer.pos, self.layer.block_size as usize) + offset_in_block;
        Ok(())
    }

    async fn advance(&mut self) -> Result<(), Error> {
        if self.item_index >= self.item_count {
            if self.pos >= self.layer.data_offset() + self.layer.data_size {
                self.key = None;
                return Ok(());
            }
            if self.buffer.chunk.is_none() || self.pos as usize % CHUNK_SIZE == 0 {
                self.buffer.chunk = Some(
                    self.layer
                        .caching_object_handle
                        .read(self.pos as usize)
                        .await
                        .context("Reading during advance")?,
                );
            }
            self.buffer.pos = self.pos as usize % CHUNK_SIZE;
            self.item_count = self.buffer.read_u16::<LittleEndian>()?;
            if self.item_count == 0 {
                bail!(
                    "Read block with zero item count (object: {}, offset: {})",
                    self.layer.object_handle.object_id(),
                    self.pos
                );
            }
            debug!(
                pos = self.pos,
                buf = ?self.buffer,
                object_size = self.layer.data_offset() + self.layer.data_size,
                oid = self.layer.object_handle.object_id(),
            );
            self.pos += self.layer.block_size;
            self.item_index = 0;
            self.value_deserialized = true;
        }
        self.seek_to_block_item(self.item_index)?;
        self.key = Some(
            K::deserialize_from_version(self.buffer.by_ref(), self.layer.version)
                .context("Corrupt layer (key)")?,
        );
        self.item_index += 1;
        self.value_deserialized = false;
        Ok(())
    }

    fn get(&self) -> Option<&K> {
        self.key.as_ref()
    }
}

struct Iterator<'iter, K: Key, V: LayerValue> {
    inner: KeyOnlyIterator<'iter, K, V>,
    // The current item.
    item: Option<Item<K, V>>,
}

impl<'iter, K: Key, V: LayerValue> Iterator<'iter, K, V> {
    fn new(mut seek_iterator: KeyOnlyIterator<'iter, K, V>) -> Result<Self, Error> {
        let key = std::mem::take(&mut seek_iterator.key);
        let item = if let Some(key) = key {
            seek_iterator.value_deserialized = true;
            Some(Item {
                key,
                value: V::deserialize_from_version(
                    seek_iterator.buffer.by_ref(),
                    seek_iterator.layer.version,
                )
                .context("Corrupt layer (value)")?,
                sequence: seek_iterator
                    .buffer
                    .read_u64::<LittleEndian>()
                    .context("Corrupt layer (seq)")?,
            })
        } else {
            None
        };
        Ok(Self { inner: seek_iterator, item })
    }
}

#[async_trait]
impl<'iter, K: Key, V: LayerValue> LayerIterator<K, V> for Iterator<'iter, K, V> {
    async fn advance(&mut self) -> Result<(), Error> {
        self.inner.advance().await?;
        let key = std::mem::take(&mut self.inner.key);
        self.item = if let Some(key) = key {
            self.inner.value_deserialized = true;
            Some(Item {
                key,
                value: V::deserialize_from_version(
                    self.inner.buffer.by_ref(),
                    self.inner.layer.version,
                )
                .context("Corrupt layer (value)")?,
                sequence: self
                    .inner
                    .buffer
                    .read_u64::<LittleEndian>()
                    .context("Corrupt layer (seq)")?,
            })
        } else {
            None
        };
        Ok(())
    }

    fn get(&self) -> Option<ItemRef<'_, K, V>> {
        self.item.as_ref().map(<&Item<K, V>>::into)
    }
}

// Returns the size of the seek table in bytes.
fn seek_table_size(num_data_blocks: u64) -> usize {
    // The first data block doesn't have an entry.
    let seek_table_entries = num_data_blocks.saturating_sub(1) as usize;
    if seek_table_entries == 0 {
        return 0;
    }
    let entry_size = std::mem::size_of::<u64>();
    seek_table_entries * entry_size
}

async fn load_seek_table(
    object_handle: &(impl ReadObjectHandle + 'static),
    seek_table_offset: u64,
    num_data_blocks: u64,
) -> Result<Vec<u64>, Error> {
    let seek_table_size = seek_table_size(num_data_blocks);
    if seek_table_size == 0 {
        return Ok(vec![]);
    }
    if seek_table_size > MAX_SEEK_TABLE_SIZE {
        return Err(anyhow!(FxfsError::NotSupported)).context("Seek table too large");
    }
    let mut buffer = object_handle.allocate_buffer(seek_table_size).await;
    let bytes_read = object_handle
        .read(seek_table_offset, buffer.as_mut())
        .await
        .context("Reading seek table blocks")?;
    ensure!(bytes_read == seek_table_size, "Short read");

    let mut seek_table = Vec::with_capacity(num_data_blocks as usize);
    // No entry for the first data block, assume a lower bound 0.
    seek_table.push(0);
    let mut prev = 0;
    for chunk in buffer.as_slice().chunks_exact(std::mem::size_of::<u64>()) {
        let next = LittleEndian::read_u64(chunk);
        // Should be in strict ascending order, otherwise something's broken, or we've gone off
        // the end and we're reading zeroes.
        if prev > next {
            return Err(anyhow!(FxfsError::Inconsistent))
                .context(format!("Seek table entry out of order, {:?} > {:?}", prev, next));
        }
        prev = next;
        seek_table.push(next);
    }
    Ok(seek_table)
}

// Takes the total size of the data and seek segments of the layer file, then returns the count that
// are data blocks.  There are some impossible values due to boundary conditions, which will result
// in an Inconsistent error.
// NB: This can only be used for the old format.
fn block_split_old_format(data_and_seek_size: u64, block_size: u64) -> Result<u64, Error> {
    let blocks = how_many(data_and_seek_size, block_size);
    // Entries are u64, so 8 bytes.
    assert!(block_size % 8 == 0);
    let entries_per_block = block_size / 8;
    let seek_block_count = if blocks <= 1 {
        0
    } else {
        // The first data block doesn't get an entry. Divide by `entries_per block + 1` since one
        // seek block will be required for each `entries_per_block` data blocks.
        how_many(blocks - 1, entries_per_block + 1)
    };
    let data_block_count = blocks - seek_block_count;
    // When the number of blocks is such that all seek blocks are completely filled to support
    // the associated number of data blocks, adding one block more would be impossible. So check
    // that we don't have an extra seek block that would be empty.
    if seek_block_count != 0 && (seek_block_count - 1) * entries_per_block >= data_block_count - 1 {
        return Err(anyhow!(FxfsError::Inconsistent))
            .context(format!("Invalid blocks to split: {}", blocks));
    }
    Ok(data_block_count)
}

async fn load_bloom_filter<K: FuzzyHash>(
    handle: &(impl ReadObjectHandle + 'static),
    bloom_filter_offset: u64,
    layer_info: &LayerInfo,
) -> Result<Option<BloomFilterReader<K>>, Error> {
    if layer_info.bloom_filter_size_bytes == 0 {
        return Ok(None);
    }
    if layer_info.bloom_filter_size_bytes > MAX_BLOOM_FILTER_SIZE {
        return Err(anyhow!(FxfsError::NotSupported)).context("Bloom filter too large");
    }
    let mut buffer = handle.allocate_buffer(layer_info.bloom_filter_size_bytes).await;
    handle.read(bloom_filter_offset, buffer.as_mut()).await.context("Failed to read")?;
    Ok(Some(BloomFilterReader::read(
        buffer.as_slice(),
        layer_info.bloom_filter_seed,
        layer_info.bloom_filter_num_nonces,
    )?))
}

impl<K: Key, V: LayerValue> PersistentLayer<K, V> {
    pub async fn open(handle: impl ReadObjectHandle + 'static) -> Result<Arc<Self>, Error> {
        let bs = handle.block_size();
        let mut buffer = handle.allocate_buffer(bs as usize).await;
        handle.read(0, buffer.as_mut()).await.context("Failed to read first block")?;
        let mut cursor = std::io::Cursor::new(buffer.as_slice());
        let version = Version::deserialize_from(&mut cursor)?;
        if version < NEW_PERSISTENT_LAYER_VERSION {
            std::mem::drop(cursor);
            std::mem::drop(buffer);
            return Self::open_old_format(handle).await;
        }

        ensure!(version <= LATEST_VERSION, FxfsError::InvalidVersion);
        let header = LayerHeader::deserialize_from_version(&mut cursor, version)
            .context("Failed to deserialize header")?;
        if &header.magic != PERSISTENT_LAYER_MAGIC {
            return Err(anyhow!(FxfsError::Inconsistent).context("Invalid layer file magic"));
        }
        if version < NEW_PERSISTENT_LAYER_VERSION {
            return Err(anyhow!(FxfsError::Inconsistent).context("Unexpectedly old version"));
        }
        if header.block_size == 0 || !header.block_size.is_power_of_two() {
            return Err(anyhow!(FxfsError::Inconsistent))
                .context(format!("Invalid block size {}", header.block_size));
        }
        ensure!(header.block_size > 0, FxfsError::Inconsistent);
        ensure!(header.block_size <= MAX_BLOCK_SIZE, FxfsError::NotSupported);
        let physical_block_size = handle.block_size();
        if header.block_size % physical_block_size != 0 {
            return Err(anyhow!(FxfsError::Inconsistent)).context(format!(
                "{} not a multiple of physical block size {}",
                header.block_size, physical_block_size
            ));
        }
        std::mem::drop(cursor);

        let bs = header.block_size as usize;
        if handle.get_size() < MINIMUM_LAYER_FILE_BLOCKS * bs as u64 {
            return Err(anyhow!(FxfsError::Inconsistent).context("Layer file too short"));
        }

        let layer_info = {
            let last_block_offset = handle
                .get_size()
                .checked_sub(header.block_size)
                .ok_or(FxfsError::Inconsistent)
                .context("Layer file unexpectedly short")?;
            handle
                .read(last_block_offset, buffer.subslice_mut(0..header.block_size as usize))
                .await
                .context("Failed to read layer info")?;
            let layer_info_len =
                LittleEndian::read_u64(&buffer.as_slice()[bs - std::mem::size_of::<u64>()..]);
            let layer_info_offset = bs
                .checked_sub(std::mem::size_of::<u64>() + layer_info_len as usize)
                .ok_or(FxfsError::Inconsistent)
                .context("Invalid layer info length")?;
            let mut cursor = std::io::Cursor::new(&buffer.as_slice()[layer_info_offset..]);
            LayerInfo::deserialize_from_version(&mut cursor, version)
                .context("Failed to deserialize LayerInfo")?
        };
        std::mem::drop(buffer);
        if layer_info.num_items == 0 && layer_info.num_data_blocks > 0 {
            return Err(anyhow!(FxfsError::Inconsistent))
                .context("Invalid num_items/num_data_blocks");
        }
        let total_blocks = handle.get_size() / header.block_size;
        let bloom_filter_blocks =
            round_up(layer_info.bloom_filter_size_bytes as u64, header.block_size)
                .unwrap_or(layer_info.bloom_filter_size_bytes as u64)
                / header.block_size;
        if layer_info.num_data_blocks + bloom_filter_blocks
            > total_blocks - MINIMUM_LAYER_FILE_BLOCKS
        {
            return Err(anyhow!(FxfsError::Inconsistent)).context("Invalid number of blocks");
        }

        let bloom_filter_offset =
            header.block_size * (NUM_HEADER_BLOCKS + layer_info.num_data_blocks);
        let bloom_filter = load_bloom_filter(&handle, bloom_filter_offset, &layer_info)
            .await
            .context("Failed to load bloom filter")?;
        let bloom_filter_stats = bloom_filter.as_ref().map(|b| b.stats());

        let seek_offset = header.block_size
            * (NUM_HEADER_BLOCKS + layer_info.num_data_blocks + bloom_filter_blocks);
        let seek_table = load_seek_table(&handle, seek_offset, layer_info.num_data_blocks)
            .await
            .context("Failed to load seek table")?;

        let object_handle = Arc::new(handle) as Arc<dyn ReadObjectHandle>;
        let caching_object_handle = CachingObjectHandle::new(object_handle.clone());
        Ok(Arc::new(PersistentLayer {
            object_handle,
            caching_object_handle,
            version,
            block_size: header.block_size,
            data_size: layer_info.num_data_blocks * header.block_size,
            seek_table,
            num_items: Some(layer_info.num_items),
            bloom_filter,
            bloom_filter_stats,
            close_event: Mutex::new(Some(Arc::new(DropEvent::new()))),
            _value_type: PhantomData::default(),
        }))
    }

    async fn open_old_format(handle: impl ReadObjectHandle + 'static) -> Result<Arc<Self>, Error> {
        let physical_block_size = handle.block_size();
        let (layer_info, version) = {
            let mut buffer = handle.allocate_buffer(physical_block_size as usize).await;
            handle.read(0, buffer.as_mut()).await?;
            let mut cursor = std::io::Cursor::new(buffer.as_slice());
            OldLayerInfo::deserialize_with_version(&mut cursor)?
        };

        // We expect the layer block size to be a multiple of the physical block size.
        if layer_info.block_size % physical_block_size != 0 {
            return Err(anyhow!(FxfsError::Inconsistent)).context(format!(
                "{} not a multiple of physical block size {}",
                layer_info.block_size, physical_block_size
            ));
        }
        ensure!(
            layer_info.key_value_version <= NEW_PERSISTENT_LAYER_VERSION,
            FxfsError::InvalidVersion
        );
        ensure!(layer_info.block_size <= MAX_BLOCK_SIZE, FxfsError::NotSupported);

        let data_and_seek_size = handle.get_size() - layer_info.block_size;
        let data_block_count = block_split_old_format(data_and_seek_size, layer_info.block_size)?;
        let data_size = data_block_count * layer_info.block_size;
        let seek_table_offset = data_size + layer_info.block_size;
        let seek_table = load_seek_table(&handle, seek_table_offset, data_block_count)
            .await
            .context("Failed to load seek table")?;

        let handle = Arc::new(handle) as Arc<dyn ReadObjectHandle>;
        let caching_object_handle = CachingObjectHandle::new(handle.clone());
        Ok(Arc::new(PersistentLayer {
            object_handle: handle,
            caching_object_handle,
            data_size,
            seek_table,
            close_event: Mutex::new(Some(Arc::new(DropEvent::new()))),
            version,
            block_size: layer_info.block_size,
            num_items: None,
            bloom_filter: None,
            bloom_filter_stats: None,
            _value_type: PhantomData,
        }))
    }

    fn data_offset(&self) -> u64 {
        NUM_HEADER_BLOCKS * self.block_size
    }
}

#[async_trait]
impl<K: Key, V: LayerValue> Layer<K, V> for PersistentLayer<K, V> {
    fn handle(&self) -> Option<&dyn ReadObjectHandle> {
        Some(&self.object_handle)
    }

    fn purge_cached_data(&self) {
        self.caching_object_handle.purge();
    }

    async fn seek<'a>(&'a self, bound: Bound<&K>) -> Result<BoxedLayerIterator<'a, K, V>, Error> {
        let (key, excluded) = match bound {
            Bound::Unbounded => {
                let mut iterator = Iterator::new(KeyOnlyIterator::new(self, self.data_offset()))?;
                iterator.advance().await.context("Unbounded seek advance")?;
                return Ok(Box::new(iterator));
            }
            Bound::Included(k) => (k, false),
            Bound::Excluded(k) => (k, true),
        };
        let first_data_block_index = self.data_offset() / self.block_size;

        let (mut left_offset, mut right_offset) = {
            // We are searching for a range here, as multiple items can have the same value in
            // this approximate search. Since the values used are the smallest in the associated
            // block it means that if the value equals the target you should also search the
            // one before it. The goal is for table[left] < target < table[right].
            let target = key.get_leading_u64();
            // Because the first entry in the table is always 0, right_index will never be 0.
            let right_index = self.seek_table.as_slice().partition_point(|&x| x <= target) as u64;
            // Since partition_point will find the index of the first place where the predicate
            // is false, we subtract 1 to get the index where it was last true.
            let left_index = self.seek_table.as_slice()[..right_index as usize]
                .partition_point(|&x| x < target)
                .saturating_sub(1) as u64;

            (
                (left_index + first_data_block_index) * self.block_size,
                (right_index + first_data_block_index) * self.block_size,
            )
        };
        let mut left = KeyOnlyIterator::new(self, left_offset);
        left.advance().await.context("Initial seek advance")?;
        match left.get() {
            None => return Ok(Box::new(Iterator::new(left)?)),
            Some(left_key) => match left_key.cmp_upper_bound(key) {
                Ordering::Greater => return Ok(Box::new(Iterator::new(left)?)),
                Ordering::Equal => {
                    if excluded {
                        left.advance().await?;
                    }
                    return Ok(Box::new(Iterator::new(left)?));
                }
                Ordering::Less => {}
            },
        }
        let mut right = None;
        while right_offset - left_offset > self.block_size {
            // Pick a block midway.
            let mid_offset =
                round_down(left_offset + (right_offset - left_offset) / 2, self.block_size);
            let mut iterator = KeyOnlyIterator::new(self, mid_offset);
            iterator.advance().await?;
            let iter_key: &K = iterator.get().unwrap();
            match iter_key.cmp_upper_bound(key) {
                Ordering::Greater => {
                    right_offset = mid_offset;
                    right = Some(iterator);
                }
                Ordering::Equal => {
                    if excluded {
                        iterator.advance().await?;
                    }
                    return Ok(Box::new(Iterator::new(iterator)?));
                }
                Ordering::Less => {
                    left_offset = mid_offset;
                    left = iterator;
                }
            }
        }

        // Finish the binary search on the block pointed to by `left`.
        let mut left_index = 0;
        let mut right_index = left.item_count;
        // If the size is zero then we don't touch the iterator.
        while left_index < (right_index - 1) {
            let mid_index = left_index + ((right_index - left_index) / 2);
            left.seek_to_block_item(mid_index).context("Read index offset for binary search")?;
            left.advance().await?;
            match left.get().unwrap().cmp_upper_bound(key) {
                Ordering::Greater => {
                    right_index = mid_index;
                }
                Ordering::Equal => {
                    if excluded {
                        left.advance().await?;
                    }
                    return Ok(Box::new(Iterator::new(left)?));
                }
                Ordering::Less => {
                    left_index = mid_index;
                }
            }
        }
        // When we don't find an exact match, we need to return with the first entry *after* the the
        // target key which might be the first one in the next block, currently already pointed to
        // by the "right" buffer, but usually it's just the result of the right index within the
        // "left" buffer.
        if right_index < left.item_count {
            left.seek_to_block_item(right_index)
                .context("Read index for offset of right pointer")?;
        } else if let Some(right) = right {
            return Ok(Box::new(Iterator::new(right)?));
        } else {
            // We want the end of the layer.  `right_index == left.item_count`, so `left_index ==
            // left.item_count - 1`, and the left iterator must be positioned on `left_index` since
            // we cannot have gone through the `Ordering::Greater` path above because `right_index`
            // would not be equal to `left.item_count` in that case, so all we need to do is advance
            // the iterator.
        }
        left.advance().await?;
        return Ok(Box::new(Iterator::new(left)?));
    }

    fn estimated_len(&self) -> ItemCount {
        match self.num_items {
            Some(num_items) => ItemCount::Precise(num_items),
            None => {
                // Older layer files didn't have the exact number, so guess it.  Precision doesn't
                // matter too much here, as the impact of getting this wrong is mis-sizing the bloom
                // filter, which will be corrected during later compactions anyways.
                const ITEM_SIZE_ESTIMATE: usize = 32;
                ItemCount::Estimate(self.object_handle.get_size() as usize / ITEM_SIZE_ESTIMATE)
            }
        }
    }

    fn maybe_contains_key(&self, key: &K) -> bool {
        self.bloom_filter.as_ref().map_or(true, |f| f.maybe_contains(key))
    }

    fn lock(&self) -> Option<Arc<DropEvent>> {
        self.close_event.lock().unwrap().clone()
    }

    async fn close(&self) {
        let listener =
            self.close_event.lock().unwrap().take().expect("close already called").listen();
        listener.await;
    }

    fn get_version(&self) -> Version {
        return self.version;
    }

    fn record_inspect_data(self: Arc<Self>, node: &fuchsia_inspect::Node) {
        node.record_bool("persistent", true);
        node.record_uint("size", self.object_handle.get_size());
        if let Some(stats) = self.bloom_filter_stats.as_ref() {
            node.record_child("bloom_filter", move |node| {
                node.record_uint("size", stats.size as u64);
                node.record_uint("num_nonces", stats.num_nonces as u64);
                node.record_uint("fill_percentage", stats.fill_percentage as u64);
            });
        }
        if let Some(items) = self.num_items {
            node.record_uint("num_items", items as u64);
        }
    }
}

// This ensures that item_count can't be overflowed below.
const_assert!(MAX_BLOCK_SIZE <= u16::MAX as u64 + 1);

// -- Writer support --

pub struct PersistentLayerWriter<W: WriteBytes, K: Key, V: LayerValue> {
    writer: W,
    block_size: u64,
    buf: Vec<u8>,
    buf_item_count: u16,
    item_count: usize,
    block_offsets: Vec<u16>,
    block_keys: Vec<u64>,
    bloom_filter: BloomFilterWriter<K>,
    _value: PhantomData<V>,
}

impl<W: WriteBytes, K: Key, V: LayerValue> PersistentLayerWriter<W, K, V> {
    /// Creates a new writer that will serialize items to the object accessible via |object_handle|
    /// (which provides a write interface to the object).
    /// `estimated_num_items` is an estimate for the number of items that we expect to write into
    /// the layer.  This need not be exact; it is used for estimating the size of bloom filter to
    /// create.  In general this is likely to be an over-count, as during compaction items might be
    /// merged.  That results in bloom filters that are slightly bigger than necessary, which isn't
    /// a significant problem.
    pub async fn new(
        mut writer: W,
        estimated_num_items: usize,
        block_size: u64,
    ) -> Result<Self, Error> {
        ensure!(block_size <= MAX_BLOCK_SIZE, FxfsError::NotSupported);
        ensure!(block_size >= MIN_BLOCK_SIZE, FxfsError::NotSupported);

        // Write the header block.
        let header = LayerHeader { magic: PERSISTENT_LAYER_MAGIC.clone(), block_size };
        let mut buf = vec![0u8; block_size as usize];
        {
            let mut cursor = std::io::Cursor::new(&mut buf[..]);
            header.serialize_with_version(&mut cursor)?;
        }
        writer.write_bytes(&buf[..]).await?;

        let nonce: u64 = rand::thread_rng().gen();
        Ok(PersistentLayerWriter {
            writer,
            block_size,
            buf: Vec::new(),
            buf_item_count: 0,
            item_count: 0,
            block_offsets: Vec::new(),
            block_keys: Vec::new(),
            bloom_filter: BloomFilterWriter::new(nonce, estimated_num_items),
            _value: PhantomData,
        })
    }

    /// Writes 'buf[..len]' out as a block.
    ///
    /// Blocks are fixed size, consisting of a 16-bit item count, data, zero padding
    /// and seek table at the end.
    async fn write_block(&mut self, len: usize) -> Result<(), Error> {
        if self.buf_item_count == 0 {
            return Ok(());
        }
        let seek_table_size = self.block_offsets.len() * PER_DATA_BLOCK_SEEK_ENTRY_SIZE;
        assert!(PER_DATA_BLOCK_HEADER_SIZE + seek_table_size + len <= self.block_size as usize);
        let mut cursor = std::io::Cursor::new(vec![0u8; self.block_size as usize]);
        cursor.write_u16::<LittleEndian>(self.buf_item_count)?;
        cursor.write_all(self.buf.drain(..len).as_ref())?;
        cursor.set_position(self.block_size - seek_table_size as u64);
        // Write the seek table. Entries are 2 bytes each and items are always at least 10.
        for &offset in &self.block_offsets {
            cursor.write_u16::<LittleEndian>(offset)?;
        }
        self.writer.write_bytes(cursor.get_ref()).await?;
        debug!(item_count = self.buf_item_count, byte_count = len, "wrote items");
        self.buf_item_count = 0;
        self.block_offsets.clear();
        Ok(())
    }

    // Assumes the writer is positioned to a new block.
    // Returns the size, in bytes, of the seek table.
    // Note that the writer will be positioned to exactly the end of the seek table, not to the end
    // of a block.
    async fn write_seek_table(&mut self) -> Result<usize, Error> {
        if self.block_keys.len() == 0 {
            return Ok(0);
        }
        let size = self.block_keys.len() * std::mem::size_of::<u64>();
        self.buf.resize(size, 0);
        let mut len = 0;
        for key in &self.block_keys {
            LittleEndian::write_u64(&mut self.buf[len..len + std::mem::size_of::<u64>()], *key);
            len += std::mem::size_of::<u64>();
        }
        self.writer.write_bytes(&self.buf).await?;
        Ok(size)
    }

    // Assumes the writer is positioned to exactly the end of the seek table, which was
    // `seek_table_len` bytes.
    async fn write_info(
        &mut self,
        num_data_blocks: u64,
        bloom_filter_size_bytes: usize,
        seek_table_len: usize,
    ) -> Result<(), Error> {
        let block_size = self.writer.block_size() as usize;
        let layer_info = LayerInfo {
            num_items: self.item_count,
            num_data_blocks,
            bloom_filter_size_bytes,
            bloom_filter_seed: self.bloom_filter.seed(),
            bloom_filter_num_nonces: self.bloom_filter.num_nonces(),
        };
        let actual_len = {
            let mut cursor = std::io::Cursor::new(&mut self.buf);
            layer_info.serialize_into(&mut cursor)?;
            let layer_info_len = cursor.position();
            cursor.write_u64::<LittleEndian>(layer_info_len)?;
            cursor.position() as usize
        };

        // We want the LayerInfo to be at the end of the last block.  That might require creating a
        // new block if we don't have enough room.
        let avail_in_block = block_size - (seek_table_len % block_size);
        let to_skip = if avail_in_block < actual_len {
            block_size + avail_in_block - actual_len
        } else {
            avail_in_block - actual_len
        } as u64;
        self.writer.skip(to_skip).await?;
        self.writer.write_bytes(&self.buf[..actual_len]).await?;
        Ok(())
    }

    // Assumes the writer is positioned to a new block.
    // Returns the size of the bloom filter, in bytes.
    async fn write_bloom_filter(&mut self) -> Result<usize, Error> {
        if self.data_blocks() < MINIMUM_DATA_BLOCKS_FOR_BLOOM_FILTER {
            return Ok(0);
        }
        // TODO(https://fxbug.dev/323571978): Avoid bounce-buffering.
        let size = round_up(self.bloom_filter.serialized_size(), self.block_size as usize).unwrap();
        self.buf.resize(size, 0);
        let mut cursor = std::io::Cursor::new(&mut self.buf);
        self.bloom_filter.write(&mut cursor)?;
        self.writer.write_bytes(&self.buf).await?;
        Ok(self.bloom_filter.serialized_size())
    }

    fn data_blocks(&self) -> usize {
        if self.item_count == 0 {
            0
        } else {
            self.block_keys.len() + 1
        }
    }
}

impl<W: WriteBytes + Send, K: Key, V: LayerValue> LayerWriter<K, V>
    for PersistentLayerWriter<W, K, V>
{
    async fn write(&mut self, item: ItemRef<'_, K, V>) -> Result<(), Error> {
        // Note the length before we write this item.
        let len = self.buf.len();
        item.key.serialize_into(&mut self.buf)?;
        item.value.serialize_into(&mut self.buf)?;
        self.buf.write_u64::<LittleEndian>(item.sequence)?;
        let mut added_offset = false;
        // Never record the first item. The offset is always the same.
        if self.buf_item_count > 0 {
            self.block_offsets.push(u16::try_from(len + PER_DATA_BLOCK_HEADER_SIZE).unwrap());
            added_offset = true;
        }

        // If writing the item took us over a block, flush the bytes in the buffer prior to this
        // item.
        if PER_DATA_BLOCK_HEADER_SIZE
            + self.buf.len()
            + (self.block_offsets.len() * PER_DATA_BLOCK_SEEK_ENTRY_SIZE)
            > self.block_size as usize - 1
        {
            if added_offset {
                // Drop the recently added offset from the list. The latest item will be the first
                // on the next block and have a known offset there.
                self.block_offsets.pop();
            }
            self.write_block(len).await?;

            // Note that this will not insert an entry for the first data block.
            self.block_keys.push(item.key.get_leading_u64());
        }

        self.bloom_filter.insert(&item.key);
        self.buf_item_count += 1;
        self.item_count += 1;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Error> {
        self.write_block(self.buf.len()).await?;
        let data_blocks = self.data_blocks() as u64;
        let bloom_filter_len = self.write_bloom_filter().await?;
        let seek_table_len = self.write_seek_table().await?;
        self.write_info(data_blocks, bloom_filter_len, seek_table_len).await?;
        self.writer.complete().await
    }
}

impl<W: WriteBytes, K: Key, V: LayerValue> Drop for PersistentLayerWriter<W, K, V> {
    fn drop(&mut self) {
        if self.buf_item_count > 0 {
            warn!("Dropping unwritten items; did you forget to flush?");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        block_split_old_format, OldLayerInfo, PersistentLayer, PersistentLayerWriter,
        PER_DATA_BLOCK_HEADER_SIZE, PER_DATA_BLOCK_SEEK_ENTRY_SIZE,
    };
    use crate::filesystem::MAX_BLOCK_SIZE;
    use crate::lsm_tree::types::{
        DefaultOrdUpperBound, FuzzyHash, Item, ItemRef, Key, Layer, LayerKey, LayerValue,
        LayerWriter, MergeType, SortByU64,
    };
    use crate::lsm_tree::LayerIterator;
    use crate::object_handle::WriteBytes;
    use crate::round::round_up;
    use crate::serialized_types::{
        versioned_type, Version, Versioned, VersionedLatest, LATEST_VERSION,
        NEW_PERSISTENT_LAYER_VERSION,
    };
    use crate::testing::fake_object::{FakeObject, FakeObjectHandle};
    use crate::testing::writer::Writer;
    use anyhow::Error;
    use byteorder::{ByteOrder as _, LittleEndian, WriteBytesExt as _};
    use fprint::TypeFingerprint;
    use fxfs_macros::FuzzyHash;
    use std::fmt::Debug;
    use std::hash::Hash;
    use std::io::Write as _;
    use std::marker::PhantomData;
    use std::ops::{Bound, Range};
    use std::sync::Arc;

    /// Exists for testing backwards compatibility.
    struct OldPersistentLayerWriter<W: WriteBytes, K: Key, V: LayerValue> {
        writer: W,
        block_size: u64,
        buf: Vec<u8>,
        item_count: u16,
        block_offsets: Vec<u16>,
        block_keys: Vec<u64>,
        _key: PhantomData<K>,
        _value: PhantomData<V>,
    }

    impl<W: WriteBytes, K: Key, V: LayerValue> OldPersistentLayerWriter<W, K, V> {
        /// Creates a new writer that will serialize items to the object accessible via
        /// |object_handle| (which provides a write interface to the object).
        pub async fn new(version: Version, mut writer: W, block_size: u64) -> Result<Self, Error> {
            assert!(block_size <= MAX_BLOCK_SIZE);
            let layer_info = OldLayerInfo { block_size, key_value_version: version };
            let mut cursor = std::io::Cursor::new(vec![0u8; block_size as usize]);
            version.serialize_into(&mut cursor)?;
            layer_info.serialize_into(&mut cursor)?;
            writer.write_bytes(cursor.get_ref()).await?;
            Ok(OldPersistentLayerWriter {
                writer,
                block_size,
                buf: Vec::new(),
                item_count: 0,
                block_offsets: Vec::new(),
                block_keys: Vec::new(),
                _key: PhantomData,
                _value: PhantomData,
            })
        }

        /// Writes 'buf[..len]' out as a block.
        ///
        /// Blocks are fixed size, consisting of a 16-bit item count, data, zero padding
        /// and seek table at the end.
        async fn write_block(&mut self, len: usize) -> Result<(), Error> {
            if self.item_count == 0 {
                return Ok(());
            }
            let seek_table_size = self.block_offsets.len() * PER_DATA_BLOCK_SEEK_ENTRY_SIZE;
            assert!(seek_table_size + len + std::mem::size_of::<u16>() <= self.block_size as usize);
            let mut cursor = std::io::Cursor::new(vec![0u8; self.block_size as usize]);
            cursor.write_u16::<LittleEndian>(self.item_count)?;
            cursor.write_all(self.buf.drain(..len).as_ref())?;
            cursor.set_position(self.block_size - seek_table_size as u64);
            // Write the seek table. Entries are 2 bytes each and items are always at least 10.
            for &offset in &self.block_offsets {
                cursor.write_u16::<LittleEndian>(offset)?;
            }
            self.writer.write_bytes(cursor.get_ref()).await?;
            self.item_count = 0;
            self.block_offsets.clear();
            Ok(())
        }

        async fn write_seek_table(&mut self) -> Result<(), Error> {
            if self.block_keys.len() == 0 {
                return Ok(());
            }
            self.buf.resize(self.block_keys.len() * 8, 0);
            let mut len = 0;
            for key in &self.block_keys {
                LittleEndian::write_u64(&mut self.buf[len..len + 8], *key);
                len += 8;
            }
            self.writer.write_bytes(&self.buf).await?;
            Ok(())
        }
    }

    impl<W: WriteBytes + Send, K: Key, V: LayerValue> LayerWriter<K, V>
        for OldPersistentLayerWriter<W, K, V>
    {
        async fn write(&mut self, item: ItemRef<'_, K, V>) -> Result<(), Error> {
            // Note the length before we write this item.
            let len = self.buf.len();
            item.key.serialize_into(&mut self.buf)?;
            item.value.serialize_into(&mut self.buf)?;
            self.buf.write_u64::<LittleEndian>(item.sequence)?;
            let mut added_offset = false;
            // Never record the first item. The offset is always the same.
            if self.item_count > 0 {
                self.block_offsets.push(u16::try_from(len + PER_DATA_BLOCK_HEADER_SIZE).unwrap());
                added_offset = true;
            }

            // If writing the item took us over a block, flush the bytes in the buffer prior to this
            // item.
            if PER_DATA_BLOCK_HEADER_SIZE
                + self.buf.len()
                + (self.block_offsets.len() * PER_DATA_BLOCK_SEEK_ENTRY_SIZE)
                > self.block_size as usize - 1
            {
                if added_offset {
                    // Drop the recently added offset from the list. The latest item will be the
                    // first on the next block and have a known offset there.
                    self.block_offsets.pop();
                }
                self.write_block(len).await?;

                // Note that this will not insert an entry for the first data block.
                self.block_keys.push(item.key.get_leading_u64());
            }

            self.item_count += 1;
            Ok(())
        }

        async fn flush(&mut self) -> Result<(), Error> {
            self.write_block(self.buf.len()).await?;
            self.write_seek_table().await?;
            self.writer.complete().await
        }
    }

    impl<W: WriteBytes> Debug for PersistentLayerWriter<W, i32, i32> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
            f.debug_struct("rPersistentLayerWriter")
                .field("block_size", &self.block_size)
                .field("item_count", &self.buf_item_count)
                .finish()
        }
    }

    #[fuchsia::test]
    async fn test_iterate_after_write() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 10000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                ITEM_COUNT as usize * 4,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        let mut iterator = layer.seek(Bound::Unbounded).await.expect("seek failed");
        for i in 0..ITEM_COUNT {
            let ItemRef { key, value, .. } = iterator.get().expect("missing item");
            assert_eq!((key, value), (&i, &i));
            iterator.advance().await.expect("failed to advance");
        }
        assert!(iterator.get().is_none());
    }

    #[fuchsia::test]
    async fn test_seek_after_write() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 5000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                ITEM_COUNT as usize * 18,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                // Populate every other value as an item.
                writer.write(Item::new(i * 2, i * 2).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        // Search for all values to check the in-between values.
        for i in 0..ITEM_COUNT * 2 {
            // We've written every other value, we expect to get either the exact value searched
            // for, or the next one after it. So round up to the nearest multiple of 2.
            let expected = round_up(i, 2).unwrap();
            let mut iterator = layer.seek(Bound::Included(&i)).await.expect("failed to seek");
            // We've written values up to (N-1)*2=2*N-2, so when looking for 2*N-1 we'll go off the
            // end of the layer and get back no item.
            if i >= (ITEM_COUNT * 2) - 1 {
                assert!(iterator.get().is_none());
            } else {
                let ItemRef { key, value, .. } = iterator.get().expect("missing item");
                assert_eq!((key, value), (&expected, &expected));
            }

            // Check that we can advance to the next item.
            iterator.advance().await.expect("failed to advance");
            // The highest value is 2*N-2, searching for 2*N-3 will find the last value, and
            // advancing will go off the end of the layer and return no item. If there was
            // previously no item, then it will latch and always return no item.
            if i >= (ITEM_COUNT * 2) - 3 {
                assert!(iterator.get().is_none());
            } else {
                let ItemRef { key, value, .. } = iterator.get().expect("missing item");
                let next = expected + 2;
                assert_eq!((key, value), (&next, &next));
            }
        }
    }

    #[fuchsia::test]
    async fn test_seek_unbounded() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 1000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                ITEM_COUNT as usize * 18,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        let mut iterator = layer.seek(Bound::Unbounded).await.expect("failed to seek");
        let ItemRef { key, value, .. } = iterator.get().expect("missing item");
        assert_eq!((key, value), (&0, &0));

        // Check that we can advance to the next item.
        iterator.advance().await.expect("failed to advance");
        let ItemRef { key, value, .. } = iterator.get().expect("missing item");
        assert_eq!((key, value), (&1, &1));
    }

    #[fuchsia::test]
    async fn test_zero_items() {
        const BLOCK_SIZE: u64 = 512;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                0,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            writer.flush().await.expect("flush failed");
        }

        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        let iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
            .seek(Bound::Unbounded)
            .await
            .expect("seek failed");
        assert!(iterator.get().is_none())
    }

    #[fuchsia::test]
    async fn test_one_item() {
        const BLOCK_SIZE: u64 = 512;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                1,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            writer.write(Item::new(42, 42).as_item_ref()).await.expect("write failed");
            writer.flush().await.expect("flush failed");
        }

        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        {
            let mut iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
                .seek(Bound::Unbounded)
                .await
                .expect("seek failed");
            let ItemRef { key, value, .. } = iterator.get().expect("missing item");
            assert_eq!((key, value), (&42, &42));
            iterator.advance().await.expect("failed to advance");
            assert!(iterator.get().is_none())
        }
        {
            let mut iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
                .seek(Bound::Included(&30))
                .await
                .expect("seek failed");
            let ItemRef { key, value, .. } = iterator.get().expect("missing item");
            assert_eq!((key, value), (&42, &42));
            iterator.advance().await.expect("failed to advance");
            assert!(iterator.get().is_none())
        }
        {
            let mut iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
                .seek(Bound::Included(&42))
                .await
                .expect("seek failed");
            let ItemRef { key, value, .. } = iterator.get().expect("missing item");
            assert_eq!((key, value), (&42, &42));
            iterator.advance().await.expect("failed to advance");
            assert!(iterator.get().is_none())
        }
        {
            let iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
                .seek(Bound::Included(&43))
                .await
                .expect("seek failed");
            assert!(iterator.get().is_none())
        }
    }

    #[fuchsia::test]
    async fn test_large_block_size() {
        // At the upper end of the supported size.
        const BLOCK_SIZE: u64 = MAX_BLOCK_SIZE;
        // Items will be 18 bytes, so fill up a few pages.
        const ITEM_COUNT: i32 = ((BLOCK_SIZE as i32) / 18) * 3;

        let handle =
            FakeObjectHandle::new_with_block_size(Arc::new(FakeObject::new()), BLOCK_SIZE as usize);
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                ITEM_COUNT as usize * 18,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            // Use large values to force varint encoding to use consistent space.
            for i in 2000000000..(2000000000 + ITEM_COUNT) {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }

        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        let mut iterator = layer.seek(Bound::Unbounded).await.expect("seek failed");
        for i in 2000000000..(2000000000 + ITEM_COUNT) {
            let ItemRef { key, value, .. } = iterator.get().expect("missing item");
            assert_eq!((key, value), (&i, &i));
            iterator.advance().await.expect("failed to advance");
        }
        assert!(iterator.get().is_none());
    }

    #[fuchsia::test]
    async fn test_overlarge_block_size() {
        // At the upper end of the supported size.
        const BLOCK_SIZE: u64 = MAX_BLOCK_SIZE * 2;

        let handle =
            FakeObjectHandle::new_with_block_size(Arc::new(FakeObject::new()), BLOCK_SIZE as usize);
        PersistentLayerWriter::<_, i32, i32>::new(Writer::new(&handle).await, 0, BLOCK_SIZE)
            .await
            .expect_err("Creating writer with overlarge block size.");
    }

    #[fuchsia::test]
    async fn test_seek_bound_excluded() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 10000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = PersistentLayerWriter::<_, i32, i32>::new(
                Writer::new(&handle).await,
                ITEM_COUNT as usize * 18,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");

        for i in 9982..ITEM_COUNT {
            let mut iterator = layer.seek(Bound::Excluded(&i)).await.expect("failed to seek");
            let i_plus_one = i + 1;
            if i_plus_one < ITEM_COUNT {
                let ItemRef { key, value, .. } = iterator.get().expect("missing item");

                assert_eq!((key, value), (&i_plus_one, &i_plus_one));

                // Check that we can advance to the next item.
                iterator.advance().await.expect("failed to advance");
                let i_plus_two = i + 2;
                if i_plus_two < ITEM_COUNT {
                    let ItemRef { key, value, .. } = iterator.get().expect("missing item");
                    assert_eq!((key, value), (&i_plus_two, &i_plus_two));
                } else {
                    assert!(iterator.get().is_none());
                }
            } else {
                assert!(iterator.get().is_none());
            }
        }
    }

    #[derive(
        Clone,
        Eq,
        Hash,
        FuzzyHash,
        PartialEq,
        Debug,
        serde::Serialize,
        serde::Deserialize,
        TypeFingerprint,
        Versioned,
    )]
    struct TestKey(Range<u64>);
    versioned_type! { 1.. => TestKey }
    impl SortByU64 for TestKey {
        fn get_leading_u64(&self) -> u64 {
            self.0.start
        }
    }
    impl LayerKey for TestKey {
        fn merge_type(&self) -> crate::lsm_tree::types::MergeType {
            MergeType::OptimizedMerge
        }

        fn next_key(&self) -> Option<Self> {
            Some(TestKey(self.0.end..self.0.end + 1))
        }
    }
    impl Ord for TestKey {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            self.0.start.cmp(&other.0.start).then(self.0.end.cmp(&other.0.end))
        }
    }
    impl PartialOrd for TestKey {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl DefaultOrdUpperBound for TestKey {}

    // Create a large spread of data across several blocks to ensure that no part of the range is
    // lost by the partial search using the layer seek table.
    #[fuchsia::test]
    async fn test_block_seek_duplicate_keys() {
        // At the upper end of the supported size.
        const BLOCK_SIZE: u64 = 512;
        // Items will be 37 bytes each. Max length varint u64 is 9 bytes, 3 of those plus one
        // straight encoded sequence number for another 8 bytes. Then 2 more for each seek table
        // entry.
        const ITEMS_TO_FILL_BLOCK: u64 = BLOCK_SIZE / 37;

        let mut to_find = Vec::new();

        let handle =
            FakeObjectHandle::new_with_block_size(Arc::new(FakeObject::new()), BLOCK_SIZE as usize);
        {
            let mut writer = PersistentLayerWriter::<_, TestKey, u64>::new(
                Writer::new(&handle).await,
                3 * BLOCK_SIZE as usize,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");

            // Make all values take up maximum space for varint encoding.
            let mut current_value = u32::MAX as u64 + 1;

            // First fill the front with a duplicate leading u64 amount, then look at the start,
            // middle and end of the range.
            {
                let items = ITEMS_TO_FILL_BLOCK * 3;
                for i in 0..items {
                    writer
                        .write(
                            Item::new(TestKey(current_value..current_value + i), current_value)
                                .as_item_ref(),
                        )
                        .await
                        .expect("write failed");
                }
                to_find.push(TestKey(current_value..current_value));
                to_find.push(TestKey(current_value..(current_value + (items / 2))));
                to_find.push(TestKey(current_value..current_value + (items - 1)));
                current_value += 1;
            }

            // Add some filler of all different leading u64.
            {
                let items = ITEMS_TO_FILL_BLOCK * 3;
                for _ in 0..items {
                    writer
                        .write(
                            Item::new(TestKey(current_value..current_value), current_value)
                                .as_item_ref(),
                        )
                        .await
                        .expect("write failed");
                    current_value += 1;
                }
            }

            // Fill the middle with a duplicate leading u64 amount, then look at the start,
            // middle and end of the range.
            {
                let items = ITEMS_TO_FILL_BLOCK * 3;
                for i in 0..items {
                    writer
                        .write(
                            Item::new(TestKey(current_value..current_value + i), current_value)
                                .as_item_ref(),
                        )
                        .await
                        .expect("write failed");
                }
                to_find.push(TestKey(current_value..current_value));
                to_find.push(TestKey(current_value..(current_value + (items / 2))));
                to_find.push(TestKey(current_value..current_value + (items - 1)));
                current_value += 1;
            }

            // Add some filler of all different leading u64.
            {
                let items = ITEMS_TO_FILL_BLOCK * 3;
                for _ in 0..items {
                    writer
                        .write(
                            Item::new(TestKey(current_value..current_value), current_value)
                                .as_item_ref(),
                        )
                        .await
                        .expect("write failed");
                    current_value += 1;
                }
            }

            // Fill the end with a duplicate leading u64 amount, then look at the start,
            // middle and end of the range.
            {
                let items = ITEMS_TO_FILL_BLOCK * 3;
                for i in 0..items {
                    writer
                        .write(
                            Item::new(TestKey(current_value..current_value + i), current_value)
                                .as_item_ref(),
                        )
                        .await
                        .expect("write failed");
                }
                to_find.push(TestKey(current_value..current_value));
                to_find.push(TestKey(current_value..(current_value + (items / 2))));
                to_find.push(TestKey(current_value..current_value + (items - 1)));
            }

            writer.flush().await.expect("flush failed");
        }

        let layer = PersistentLayer::<TestKey, u64>::open(handle).await.expect("new failed");
        for target in to_find {
            let iterator: Box<dyn LayerIterator<TestKey, u64>> =
                layer.seek(Bound::Included(&target)).await.expect("failed to seek");
            let ItemRef { key, .. } = iterator.get().expect("missing item");
            assert_eq!(&target, key);
        }
    }

    #[fuchsia::test]
    async fn test_two_seek_blocks() {
        // At the upper end of the supported size.
        const BLOCK_SIZE: u64 = 512;
        // Items will be 37 bytes each. Max length varint u64 is 9 bytes, 3 of those plus one
        // straight encoded sequence number for another 8 bytes. Then 2 more for each seek table
        // entry.
        const ITEMS_TO_FILL_BLOCK: u64 = BLOCK_SIZE / 37;
        // Add enough items to create enough blocks that the seek table can't fit into a single
        // block. Entries are 8 bytes. Remember to add one because the first block entry is omitted,
        // then add one more to overflow into the next block.
        const ITEM_COUNT: u64 = ITEMS_TO_FILL_BLOCK * ((BLOCK_SIZE / 8) + 2);

        let mut to_find = Vec::new();

        let handle =
            FakeObjectHandle::new_with_block_size(Arc::new(FakeObject::new()), BLOCK_SIZE as usize);
        {
            let mut writer = PersistentLayerWriter::<_, TestKey, u64>::new(
                Writer::new(&handle).await,
                ITEM_COUNT as usize * 18,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");

            // Make all values take up maximum space for varint encoding.
            let initial_value = u32::MAX as u64 + 1;
            for i in 0..ITEM_COUNT {
                writer
                    .write(
                        Item::new(TestKey(initial_value + i..initial_value + i), initial_value)
                            .as_item_ref(),
                    )
                    .await
                    .expect("write failed");
            }
            // Look at the start middle and end.
            to_find.push(TestKey(initial_value..initial_value));
            let middle = initial_value + ITEM_COUNT / 2;
            to_find.push(TestKey(middle..middle));
            let end = initial_value + ITEM_COUNT - 1;
            to_find.push(TestKey(end..end));

            writer.flush().await.expect("flush failed");
        }

        let layer = PersistentLayer::<TestKey, u64>::open(handle).await.expect("new failed");
        for target in to_find {
            let iterator: Box<dyn LayerIterator<TestKey, u64>> =
                layer.seek(Bound::Included(&target)).await.expect("failed to seek");
            let ItemRef { key, .. } = iterator.get().expect("missing item");
            assert_eq!(&target, key);
        }
    }

    // Verifies behaviour around creating full seek blocks, to ensure that it is able to be opened
    // and parsed afterward.
    #[fuchsia::test]
    async fn test_full_seek_block() {
        const BLOCK_SIZE: u64 = 512;

        // Items will be 37 bytes each. Max length varint u64 is 9 bytes, 3 of those plus one
        // straight encoded sequence number for another 8 bytes. Then 2 more for each seek table
        // entry.
        const ITEMS_TO_FILL_BLOCK: u64 = BLOCK_SIZE / 37;

        // How many entries there are in a seek table block.
        const SEEK_TABLE_ENTRIES: u64 = BLOCK_SIZE / 8;

        // Number of entries to fill a seek block would need one more block of entries, but we're
        // starting low here on purpose to do a range and make sure we hit the size we are
        // interested in.
        const START_ENTRIES_COUNT: u64 = ITEMS_TO_FILL_BLOCK * SEEK_TABLE_ENTRIES;

        for entries in START_ENTRIES_COUNT..START_ENTRIES_COUNT + (ITEMS_TO_FILL_BLOCK * 2) {
            let handle = FakeObjectHandle::new_with_block_size(
                Arc::new(FakeObject::new()),
                BLOCK_SIZE as usize,
            );
            {
                let mut writer = PersistentLayerWriter::<_, TestKey, u64>::new(
                    Writer::new(&handle).await,
                    entries as usize,
                    BLOCK_SIZE,
                )
                .await
                .expect("writer new");

                // Make all values take up maximum space for varint encoding.
                let initial_value = u32::MAX as u64 + 1;
                for i in 0..entries {
                    writer
                        .write(
                            Item::new(TestKey(initial_value + i..initial_value + i), initial_value)
                                .as_item_ref(),
                        )
                        .await
                        .expect("write failed");
                }

                writer.flush().await.expect("flush failed");
            }
            PersistentLayer::<TestKey, u64>::open(handle).await.expect("new failed");
        }
    }

    // Verifies (for the old layer file format) that the size of the generated seek blocks will be
    // correctly calculated based on the size of the layer. Even though there are no interesting
    // branches, testing the math from two ends was really helpful in development.
    #[fuchsia::test]
    async fn test_seek_block_count() {
        fn validate_values(block_size: u64, data_blocks: u64, seek_blocks: u64) -> bool {
            if data_blocks <= 1 {
                seek_blocks == 0
            } else {
                // No entry for the first data block, so don't count it. Look how much space would be
                // generated in the seek blocks for those entries and then ensure that is the right
                // amount of space reserved for the seek blocks.
                round_up((data_blocks - 1) * 8, block_size).unwrap() / block_size == seek_blocks
            }
        }
        const BLOCK_SIZE: u64 = 512;
        const PER_BLOCK: u64 = BLOCK_SIZE / 8;
        // Don't include zero, there must always be a layerinfo block.
        for block_count in 1..(PER_BLOCK * (PER_BLOCK + 1)) {
            let size = block_count * BLOCK_SIZE;
            if let Ok(data_blocks) = block_split_old_format(size, BLOCK_SIZE) {
                let seek_blocks = block_count - data_blocks;
                assert!(
                    validate_values(BLOCK_SIZE, data_blocks, seek_blocks),
                    "For {} blocks got {} data and {} seek",
                    block_count,
                    data_blocks,
                    seek_blocks
                );
            } else {
                // Whenever the split fails, the previous count would have perfectly saturated all
                // seek blocks. The only available options with this count of blocks is an empty
                // seek block or a data block entry that doesn't fit into any seek block.
                let data_blocks =
                    block_split_old_format(BLOCK_SIZE * (block_count - 1), BLOCK_SIZE)
                        .expect("Previous count split");
                assert_eq!(
                    (data_blocks - 1) % PER_BLOCK,
                    0,
                    "Failed to split a valid value {}.",
                    block_count
                );
            }
        }
    }

    #[fuchsia::test]
    async fn test_zero_items_old_format() {
        const BLOCK_SIZE: u64 = 512;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = OldPersistentLayerWriter::<_, i32, i32>::new(
                Version { major: NEW_PERSISTENT_LAYER_VERSION.major - 1, minor: 0 },
                Writer::new(&handle).await,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            writer.flush().await.expect("flush failed");
        }

        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        let iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
            .seek(Bound::Unbounded)
            .await
            .expect("seek failed");
        assert!(iterator.get().is_none())
    }

    #[fuchsia::test]
    async fn test_some_items_old_format() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 10000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = OldPersistentLayerWriter::<_, i32, i32>::new(
                Version { major: NEW_PERSISTENT_LAYER_VERSION.major - 1, minor: 0 },
                Writer::new(&handle).await,
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = PersistentLayer::<i32, i32>::open(handle).await.expect("new failed");
        let mut iterator = layer.seek(Bound::Unbounded).await.expect("failed to seek");
        let ItemRef { key, value, .. } = iterator.get().expect("missing item");
        assert_eq!((key, value), (&0, &0));

        // Check that we can advance to the next item.
        iterator.advance().await.expect("failed to advance");
        let ItemRef { key, value, .. } = iterator.get().expect("missing item");
        assert_eq!((key, value), (&1, &1));
    }
}
