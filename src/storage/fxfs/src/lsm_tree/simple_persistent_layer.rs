// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// SimplePersistentLayer object format
//
// The layer is made up of 1 or more "blocks" whose size are some multiple of the block size used
// by the underlying handle.
//
// The persistent layer has 3 types of blocks:
//  - LayerInfo block
//  - Data block
//  - Seek block
//
// They are arranged with one LayerInfo block at the start, zero or more data blocks then zero or
// more seek blocks. There should be an order of magnitude fewer seek blocks than data blocks. Items
// within the layerfile are sorted with respect to their deserialized keys.
//
// |Layerinfo|Data|Data|Data|Data|Data|Data|Data|Data|Data|Data|Data|Data|Data|Seek|Seek|
//
// LayerInfo block just contains a single serialized LayerInfo struct.
//
// Data blocks contain a little endian encoded u16 item count at the start, then a series of
// serialized items, and a list of little endian u16 offsets within the block for where
// serialized items start, excluding the first item (since it is at a known offset). The list of
// offsets ends at the end of the block and since the items are of variable length, there may be
// space between the two sections if the next item and its offset cannot fit into the block.
//
// |item_count|item|item|item|item|item|item|dead space|offset|offset|offset|offset|offset|
//
// Seek blocks are at the end of the layer and contain a little endian u64 for every data block
// except for the first one. The entries should be monotonically increasing, as they represent some
// mapping for how the keys for the first item in each block would be predominantly sorted, and
// there may be duplicate entries. There should be exactly as many seek blocks as are required to
// house one entry fewer than the number of data blocks.

use {
    crate::{
        drop_event::DropEvent,
        errors::FxfsError,
        filesystem::MAX_BLOCK_SIZE,
        log::*,
        lsm_tree::types::{
            BoxedLayerIterator, Item, ItemRef, Key, Layer, LayerIterator, LayerWriter, Value,
        },
        object_handle::{ReadObjectHandle, WriteBytes},
        round::{how_many, round_down, round_up},
        serialized_types::{
            Version, Versioned, VersionedLatest, INTERBLOCK_SEEK_VERSION, LATEST_VERSION,
            PER_BLOCK_SEEK_VERSION,
        },
    },
    anyhow::{anyhow, bail, ensure, Context, Error},
    async_trait::async_trait,
    byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt},
    fprint::TypeFingerprint,
    serde::{Deserialize, Serialize},
    static_assertions::const_assert,
    std::{
        cmp::Ordering,
        io::Read,
        marker::PhantomData,
        ops::{Bound, Drop},
        sync::{Arc, Mutex},
        vec::Vec,
    },
    storage_device::buffer::Buffer,
};

// The first block of each layer contains metadata for the rest of the layer.
#[derive(Debug, Serialize, Deserialize, TypeFingerprint, Versioned)]
pub struct LayerInfo {
    /// The version of the key and value structs serialized in this layer.
    key_value_version: Version,
    /// The block size used within this layer file. This is typically set at compaction time to the
    /// same block size as the underlying object handle.
    ///
    /// (Each block starts with a 2 byte item count so there is a 64k item limit per block,
    /// regardless of block size).
    block_size: u64,
}

/// Implements a very primitive persistent layer where items are packed into blocks and searching
/// for items is done via a simple binary search.
pub struct SimplePersistentLayer {
    object_handle: Arc<dyn ReadObjectHandle>,
    layer_info: LayerInfo,
    size: u64,
    seek_table: Option<Vec<u64>>,
    close_event: Mutex<Option<Arc<DropEvent>>>,
}

struct BufferCursor<'a> {
    buffer: Buffer<'a>,
    pos: usize,
    len: usize,
}

impl std::io::Read for BufferCursor<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let to_read = std::cmp::min(buf.len(), self.len.saturating_sub(self.pos));
        if to_read > 0 {
            buf[..to_read].copy_from_slice(&self.buffer.as_slice()[self.pos..self.pos + to_read]);
            self.pos += to_read;
        }
        Ok(to_read)
    }
}

const BLOCK_HEADER_SIZE: usize = 2;
const BLOCK_SEEK_ENTRY_SIZE: usize = 2;

struct Iterator<'iter, K: Key, V: Value> {
    // Allocated out of |layer|.
    buffer: BufferCursor<'iter>,

    layer: &'iter SimplePersistentLayer,

    // The position of the _next_ block to be read.
    pos: u64,

    // The item index in the current block.
    item_index: u16,

    // The number of items in the current block.
    item_count: u16,

    // The current item.
    item: Option<Item<K, V>>,
}

impl<K: Key, V: Value> Iterator<'_, K, V> {
    fn new<'iter>(layer: &'iter SimplePersistentLayer, pos: u64) -> Iterator<'iter, K, V> {
        Iterator {
            layer,
            buffer: BufferCursor {
                buffer: layer.object_handle.allocate_buffer(layer.layer_info.block_size as usize),
                pos: 0,
                len: 0,
            },
            pos,
            item_index: 0,
            item_count: 0,
            item: None,
        }
    }

    // Find the offset in the block for a particular object index. Returns error if the index is
    // out of range or the resulting offset contains an obviously invalid value.
    fn offset_for_index<'a>(&'a mut self, index: usize) -> Result<u16, Error> {
        ensure!(index < usize::from(self.item_count), FxfsError::OutOfRange);
        // First entry isn't actually recorded, it is at the start of the block.
        if index == 0 {
            return Ok(BLOCK_HEADER_SIZE.try_into().unwrap());
        }

        let old_buffer_pos = self.buffer.pos;
        self.buffer.pos = self.layer.layer_info.block_size as usize
            - (BLOCK_SEEK_ENTRY_SIZE * (usize::from(self.item_count) - index));
        let res = self.buffer.read_u16::<LittleEndian>();
        self.buffer.pos = old_buffer_pos;
        let offset = res.context("Failed to read offset")?;
        if u64::from(offset) >= self.layer.layer_info.block_size
            || usize::try_from(offset).unwrap() <= BLOCK_HEADER_SIZE
        {
            return Err(anyhow!(FxfsError::Inconsistent))
                .context(format!("Offset {} is out of valid range.", offset));
        }
        Ok(offset)
    }
}

#[async_trait]
impl<'iter, K: Key, V: Value> LayerIterator<K, V> for Iterator<'iter, K, V> {
    async fn advance(&mut self) -> Result<(), Error> {
        if self.item_index >= self.item_count {
            if self.pos >= self.layer.size {
                self.item = None;
                return Ok(());
            }
            let len = self
                .layer
                .object_handle
                .read(self.pos, self.buffer.buffer.as_mut())
                .await
                .context("Reading during advance")?;
            self.buffer.pos = 0;
            self.buffer.len = len;
            debug!(
                pos = self.pos,
                object_size = self.layer.size,
                oid = self.layer.object_handle.object_id()
            );
            self.item_count = self.buffer.read_u16::<LittleEndian>()?;
            if self.item_count == 0 {
                bail!(
                    "Read block with zero item count (object: {}, offset: {})",
                    self.layer.object_handle.object_id(),
                    self.pos
                );
            }
            self.pos += self.layer.layer_info.block_size;
            self.item_index = 0;
        }
        self.item = Some(Item {
            key: K::deserialize_from_version(
                self.buffer.by_ref(),
                self.layer.layer_info.key_value_version,
            )
            .context("Corrupt layer (key)")?,
            value: V::deserialize_from_version(
                self.buffer.by_ref(),
                self.layer.layer_info.key_value_version,
            )
            .context("Corrupt layer (value)")?,
            sequence: self.buffer.read_u64::<LittleEndian>().context("Corrupt layer (seq)")?,
        });
        self.item_index += 1;
        Ok(())
    }

    fn get(&self) -> Option<ItemRef<'_, K, V>> {
        return self.item.as_ref().map(<&Item<K, V>>::into);
    }
}

// Takes the total size of the layer and block size then returns the count that are data blocks
// and the count that are blocks containing seek data. There are some impossible values due to
// boundary conditions, which will result in an Inconsistent error.
fn block_split(size: u64, block_size: u64) -> Result<(u64, u64), Error> {
    // Don't count the layerinfo block.
    let blocks = how_many(size, block_size) - 1;
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
    Ok((data_block_count, seek_block_count))
}

// Parse the seek table, if present at the end of the layer and return the offset where the
// seek table begins. If not present, return None for the seek table and the end of layer
// offset.
async fn parse_seek_table(
    object_handle: &(impl ReadObjectHandle + 'static),
    layer_info: &LayerInfo,
    mut buffer: Buffer<'_>,
) -> Result<(Option<Vec<u64>>, u64), Error> {
    let size = object_handle.get_size();
    ensure!(size < u64::MAX - layer_info.block_size, FxfsError::TooBig);
    if layer_info.key_value_version < INTERBLOCK_SEEK_VERSION {
        return Ok((None, size));
    }

    let (data_block_count, seek_block_count) = block_split(size, layer_info.block_size)?;
    let seek_table_offset =
        round_up(size, layer_info.block_size).unwrap() - (layer_info.block_size * seek_block_count);
    let mut seek_table = Vec::with_capacity(data_block_count as usize);
    // No entry for the first data block, assume a lower bound 0.
    seek_table.push(0);
    let mut position = seek_table_offset;
    let mut prev = 0;
    let mut buffer_position = 0;
    let mut bytes_read = 0;
    while seek_table.len() < data_block_count as usize {
        if buffer_position >= bytes_read {
            if position >= size {
                return Err(anyhow!(FxfsError::Inconsistent)
                    .context("Not enough space for expected seek table"));
            }
            // `read` demands that everything be block aligned, so it is expected to read at least
            // a whole block, which must be divisible by 8.
            bytes_read = object_handle
                .read(position, buffer.as_mut())
                .await
                .context("Reading seek table blocks")?;
            position += bytes_read as u64;
            buffer_position = 0;
        }
        let next = LittleEndian::read_u64(&buffer.as_slice()[buffer_position..buffer_position + 8]);
        buffer_position += 8;
        // Should be in strict ascending order, otherwise something's broken, or we've gone off
        // the end and we're reading zeroes.
        if prev > next {
            return Err(anyhow!(FxfsError::Inconsistent))
                .context(format!("Seek table entry out of order, {:?} > {:?}", prev, next));
        }
        prev = next;
        seek_table.push(next);
    }
    Ok((Some(seek_table), seek_table_offset))
}

impl SimplePersistentLayer {
    /// Opens an existing layer that is accessible via |object_handle| (which provides a read
    /// interface to the object).  The layer should have been written prior using
    /// SimplePersistentLayerWriter.
    pub async fn open(object_handle: impl ReadObjectHandle + 'static) -> Result<Arc<Self>, Error> {
        let physical_block_size = object_handle.block_size();

        // The first block contains layer file information instead of key/value data.
        let mut buffer = object_handle.allocate_buffer(physical_block_size as usize);
        let (layer_info, _version) = {
            object_handle.read(0, buffer.as_mut()).await?;
            let mut cursor = std::io::Cursor::new(buffer.as_slice());
            LayerInfo::deserialize_with_version(&mut cursor)
                .context("Failed to deserialize LayerInfo")?
        };

        // We expect the layer block size to be a multiple of the physical block size.
        if layer_info.block_size % physical_block_size != 0 {
            return Err(anyhow!(FxfsError::Inconsistent)).context(format!(
                "{} not a multiple of physical block size {}",
                layer_info.block_size, physical_block_size
            ));
        }
        ensure!(layer_info.block_size <= MAX_BLOCK_SIZE, FxfsError::NotSupported);

        let (seek_table, data_size) = parse_seek_table(&object_handle, &layer_info, buffer).await?;

        Ok(Arc::new(SimplePersistentLayer {
            object_handle: Arc::new(object_handle),
            layer_info,
            size: data_size,
            seek_table,
            close_event: Mutex::new(Some(Arc::new(DropEvent::new()))),
        }))
    }
}

#[async_trait]
impl<K: Key, V: Value> Layer<K, V> for SimplePersistentLayer {
    fn handle(&self) -> Option<&dyn ReadObjectHandle> {
        Some(self.object_handle.as_ref())
    }

    async fn seek<'a>(&'a self, bound: Bound<&K>) -> Result<BoxedLayerIterator<'a, K, V>, Error> {
        let first_block_offset = self.layer_info.block_size;
        let (key, excluded) = match bound {
            Bound::Unbounded => {
                let mut iterator = Iterator::new(self, first_block_offset);
                iterator.advance().await.context("Unbounded seek advance")?;
                return Ok(Box::new(iterator));
            }
            Bound::Included(k) => (k, false),
            Bound::Excluded(k) => (k, true),
        };

        let (mut left_offset, mut right_offset) = match &self.seek_table {
            Some(seek_table) => {
                // We are searching for a range here, as multiple items can have the same value in
                // this approximate search. Since the values used are the smallest in the associated
                // block it means that if the value equals the target you should also search the
                // one before it. The goal is for table[left] < target < table[right].
                let target = key.get_leading_u64();
                // Because the first entry in the table is always 0, right_index will never be 0.
                let right_index = seek_table.as_slice().partition_point(|&x| x <= target) as u64;
                // Since partition_point will find the index of the first place where the predicate
                // is false, we subtract 1 to get the index where it was last true.
                let left_index = seek_table.as_slice()[..right_index as usize]
                    .partition_point(|&x| x < target)
                    .saturating_sub(1) as u64;

                // Skip the first block. We store version info there for now.
                (
                    (left_index + 1) * self.layer_info.block_size,
                    (right_index + 1) * self.layer_info.block_size,
                )
            }
            None => (
                self.layer_info.block_size,
                round_up(self.size, self.layer_info.block_size).unwrap(),
            ),
        };
        let mut left = Iterator::new(self, left_offset);
        left.advance().await.context("Initial seek advance")?;
        match left.get() {
            None => return Ok(Box::new(left)),
            Some(item) => match item.key.cmp_upper_bound(key) {
                Ordering::Greater => return Ok(Box::new(left)),
                Ordering::Equal => {
                    if excluded {
                        left.advance().await?;
                    }
                    return Ok(Box::new(left));
                }
                Ordering::Less => {}
            },
        }
        let mut right = Iterator::new(self, right_offset);
        while right_offset - left_offset > self.layer_info.block_size as u64 {
            // Pick a block midway.
            let mid_offset = round_down(
                left_offset + (right_offset - left_offset) / 2,
                self.layer_info.block_size,
            );
            let mut iterator = Iterator::new(self, mid_offset);
            iterator.advance().await?;
            let item: ItemRef<'_, K, V> = iterator.get().unwrap();
            match item.key.cmp_upper_bound(key) {
                Ordering::Greater => {
                    right_offset = mid_offset;
                    right = iterator;
                }
                Ordering::Equal => {
                    if excluded {
                        iterator.advance().await?;
                    }
                    return Ok(Box::new(iterator));
                }
                Ordering::Less => {
                    left_offset = mid_offset;
                    left = iterator;
                }
            }
        }

        if self.layer_info.key_value_version >= PER_BLOCK_SEEK_VERSION {
            // We have a seek table, use it to binary search.
            let mut left_index = 0;
            let mut right_index = left.item_count;
            // If the size is zero then we don't touch the iterator.
            while left_index < (right_index - 1) {
                let mid_index = left_index + ((right_index - left_index) / 2);
                left.buffer.pos = left
                    .offset_for_index(mid_index.into())
                    .context("Read index offset for binary search")?
                    as usize;
                left.item_index = u16::try_from(mid_index).unwrap();
                // Only deserialize the key while searching.
                let current_key = K::deserialize_from_version(
                    left.buffer.by_ref(),
                    left.layer.layer_info.key_value_version,
                )
                .context("Corrupt layer (key)")?;
                match current_key.cmp_upper_bound(key) {
                    Ordering::Greater => {
                        right_index = mid_index;
                    }
                    Ordering::Equal => {
                        // Already deserialized the key, get the rest of the item.
                        let current_value = V::deserialize_from_version(
                            left.buffer.by_ref(),
                            left.layer.layer_info.key_value_version,
                        )
                        .context("Corrupt layer (value)")?;
                        let current_sequence = left
                            .buffer
                            .read_u64::<LittleEndian>()
                            .context("Corrupt layer (seq)")?;
                        left.item_index += 1;
                        if excluded {
                            left.advance().await?;
                        } else {
                            left.item = Some(Item {
                                key: current_key,
                                value: current_value,
                                sequence: current_sequence,
                            });
                        }
                        return Ok(Box::new(left));
                    }
                    Ordering::Less => {
                        left_index = mid_index;
                    }
                }
            }
            // When we don't find an exact match, we need to return with the first entry *after* the
            // the target key. Which might be the first one in the next block, currently already
            // pointed to by the "right" buffer, but usually it's just the result of the right index
            // within the "left" buffer.
            if right_index < left.item_count {
                right = left;
                right.buffer.pos = right
                    .offset_for_index(right_index.into())
                    .context("Read index for offset of right pointer")?
                    as usize;
                right.item_index = u16::try_from(right_index).unwrap();
                right.advance().await?;
            } else if right.item.is_none() {
                // This is cheap if we're actually off the end, but otherwise it catches when we
                // need to look inside the next block.
                right.advance().await.context("Seek to start of next block")?;
            }
            return Ok(Box::new(right));
        } else {
            // At this point, we know that left_key < key and right_key >= key, so we have to
            // iterate through left_key to find the key we want.
            loop {
                left.advance().await?;
                match left.get() {
                    None => return Ok(Box::new(left)),
                    Some(item) => match item.key.cmp_upper_bound(key) {
                        Ordering::Greater => return Ok(Box::new(left)),
                        Ordering::Equal => {
                            if excluded {
                                left.advance().await?;
                            }
                            return Ok(Box::new(left));
                        }
                        Ordering::Less => {}
                    },
                }
            }
        }
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
        return self.layer_info.key_value_version;
    }
}

// This ensures that item_count can't be overflowed below.
const_assert!(MAX_BLOCK_SIZE <= u16::MAX as u64 + 1);

// -- Writer support --

pub struct SimplePersistentLayerWriter<W: WriteBytes, K: Key, V: Value> {
    writer: W,
    block_size: u64,
    buf: Vec<u8>,
    item_count: u16,
    block_offsets: Vec<u16>,
    block_keys: Vec<u64>,
    _key: PhantomData<K>,
    _value: PhantomData<V>,
}

impl<W: WriteBytes, K: Key, V: Value> SimplePersistentLayerWriter<W, K, V> {
    /// Creates a new writer that will serialize items to the object accessible via |object_handle|
    /// (which provdes a write interface to the object).
    pub async fn new(mut writer: W, block_size: u64) -> Result<Self, Error> {
        ensure!(block_size <= MAX_BLOCK_SIZE, FxfsError::NotSupported);
        let layer_info = LayerInfo { block_size, key_value_version: LATEST_VERSION };
        let mut buf: Vec<u8> = vec![0u8; block_size as usize];
        let len;
        {
            let mut cursor = std::io::Cursor::new(&mut buf);
            layer_info.serialize_with_version(&mut cursor)?;
            len = cursor.position();
        }
        writer.write_bytes(&buf[..len as usize]).await?;
        writer.skip(block_size - len).await?;
        Ok(SimplePersistentLayerWriter {
            writer,
            block_size,
            buf: vec![0; BLOCK_HEADER_SIZE],
            item_count: 0,
            block_offsets: Vec::new(),
            block_keys: Vec::new(),
            _key: PhantomData,
            _value: PhantomData,
        })
    }

    async fn write_some(&mut self, len: usize) -> Result<(), Error> {
        if self.item_count == 0 {
            return Ok(());
        }
        LittleEndian::write_u16(&mut self.buf[0..BLOCK_HEADER_SIZE], self.item_count);
        self.writer.write_bytes(&self.buf[..len]).await?;
        // Leave a gap and write the seek table to the end.
        self.writer
            .skip(
                self.block_size
                    - len as u64
                    - (self.block_offsets.len() * BLOCK_SEEK_ENTRY_SIZE) as u64,
            )
            .await?;
        // Write the seek table overwriting the buffer. It must be smaller, entries are 2 bytes each
        // and items are always at least 10,
        let mut pointer = 0;
        for offset in &self.block_offsets {
            LittleEndian::write_u16(
                &mut self.buf[pointer..pointer + BLOCK_SEEK_ENTRY_SIZE],
                *offset,
            );
            pointer += BLOCK_SEEK_ENTRY_SIZE;
        }
        self.writer.write_bytes(&self.buf[..pointer]).await?;
        debug!(item_count = self.item_count, byte_count = len, "wrote items");
        // Leave space to prepend the next header.
        self.buf.drain(..len - BLOCK_HEADER_SIZE);
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
        self.writer.skip(self.block_size - (len as u64 % self.block_size)).await?;
        Ok(())
    }
}

#[async_trait]
impl<W: WriteBytes + Send, K: Key, V: Value> LayerWriter<K, V>
    for SimplePersistentLayerWriter<W, K, V>
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
            self.block_offsets.push(u16::try_from(len).unwrap());
            added_offset = true;
        }

        // If writing the item took us over a block, flush the bytes in the buffer prior to this
        // item.
        if self.buf.len() + (self.block_offsets.len() * BLOCK_SEEK_ENTRY_SIZE)
            > self.block_size as usize - 1
        {
            if added_offset {
                // Drop the recently added offset from the list. The latest item will be the first
                // on the next block and have a known offset there.
                self.block_offsets.pop();
            }
            self.write_some(len).await?;

            // Note that this will not insert an entry for the first data block.
            self.block_keys.push(item.key.get_leading_u64());
        }

        self.item_count += 1;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Error> {
        self.write_some(self.buf.len()).await?;
        self.write_seek_table().await?;
        self.writer.complete().await
    }
}

impl<W: WriteBytes, K: Key, V: Value> Drop for SimplePersistentLayerWriter<W, K, V> {
    fn drop(&mut self) {
        if self.item_count > 0 {
            warn!("Dropping unwritten items; did you forget to flush?");
        }
    }
}

#[cfg(test)]
mod tests {

    use {
        super::{block_split, SimplePersistentLayer, SimplePersistentLayerWriter},
        crate::{
            filesystem::MAX_BLOCK_SIZE,
            lsm_tree::{
                types::{
                    DefaultOrdUpperBound, Item, ItemRef, Layer, LayerWriter, NextKey, SortByU64,
                },
                LayerIterator,
            },
            object_handle::Writer,
            round::round_up,
            serialized_types::{
                versioned_type, Version, Versioned, VersionedLatest, LATEST_VERSION,
            },
            testing::fake_object::{FakeObject, FakeObjectHandle},
        },
        fprint::TypeFingerprint,
        std::{
            fmt::Debug,
            ops::{Bound, Range},
            sync::Arc,
        },
    };

    impl DefaultOrdUpperBound for i32 {}
    impl SortByU64 for i32 {
        fn get_leading_u64(&self) -> u64 {
            if self >= &0 {
                return u64::try_from(*self).unwrap() + u64::try_from(i32::MAX).unwrap() + 1;
            }
            u64::try_from(self + i32::MAX + 1).unwrap()
        }
    }

    impl Debug for SimplePersistentLayerWriter<Writer<'_>, i32, i32> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
            f.debug_struct("SimplerPersistentLayerWriter")
                .field("block_size", &self.block_size)
                .field("item_count", &self.item_count)
                .finish()
        }
    }

    #[fuchsia::test]
    async fn test_iterate_after_write() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 10000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(
                Writer::new(&handle),
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
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
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(
                Writer::new(&handle),
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
        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
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
        const ITEM_COUNT: i32 = 10000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(
                Writer::new(&handle),
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
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
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(
                Writer::new(&handle),
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            writer.flush().await.expect("flush failed");
        }

        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
        let iterator = (layer.as_ref() as &dyn Layer<i32, i32>)
            .seek(Bound::Unbounded)
            .await
            .expect("seek failed");
        assert!(iterator.get().is_none())
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
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(
                Writer::new(&handle),
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

        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
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
        SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(Writer::new(&handle), BLOCK_SIZE)
            .await
            .expect_err("Creating writer with overlarge block size.");
    }

    #[fuchsia::test]
    async fn test_seek_bound_excluded() {
        const BLOCK_SIZE: u64 = 512;
        const ITEM_COUNT: i32 = 10000;

        let handle = FakeObjectHandle::new(Arc::new(FakeObject::new()));
        {
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, i32, i32>::new(
                Writer::new(&handle),
                BLOCK_SIZE,
            )
            .await
            .expect("writer new");
            for i in 0..ITEM_COUNT {
                writer.write(Item::new(i, i).as_item_ref()).await.expect("write failed");
            }
            writer.flush().await.expect("flush failed");
        }
        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");

        for i in 0..ITEM_COUNT {
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
    impl NextKey for TestKey {
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
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, TestKey, u64>::new(
                Writer::new(&handle),
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

        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
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
            let mut writer = SimplePersistentLayerWriter::<Writer<'_>, TestKey, u64>::new(
                Writer::new(&handle),
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

        let layer = SimplePersistentLayer::open(handle).await.expect("new failed");
        for target in to_find {
            let iterator: Box<dyn LayerIterator<TestKey, u64>> =
                layer.seek(Bound::Included(&target)).await.expect("failed to seek");
            let ItemRef { key, .. } = iterator.get().expect("missing item");
            assert_eq!(&target, key);
        }
    }

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

    // Verifies that the size of the generated seek blocks will be correctly calculated based on the
    // size of the layer. Even though there are no interesting branches, testing the math from two
    // ends was really helpful in development.
    #[fuchsia::test]
    async fn test_seek_block_count() {
        const BLOCK_SIZE: u64 = 512;
        const PER_BLOCK: u64 = BLOCK_SIZE / 8;
        // Don't include zero, there must always be a layerinfo block.
        for block_count in 1..(PER_BLOCK * (PER_BLOCK + 1)) {
            let size = block_count * BLOCK_SIZE;
            if let Ok((data_blocks, seek_blocks)) = block_split(size, BLOCK_SIZE) {
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
                let (data_blocks, _seek_blocks) =
                    block_split(BLOCK_SIZE * (block_count - 1), BLOCK_SIZE)
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
}
