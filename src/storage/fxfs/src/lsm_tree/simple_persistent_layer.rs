// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
        round::{round_down, round_up},
        serialized_types::{
            Version, Versioned, VersionedLatest, LATEST_VERSION, PER_BLOCK_SEEK_VERSION,
        },
    },
    anyhow::{bail, ensure, Context, Error},
    async_trait::async_trait,
    byteorder::{ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt},
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
    type_hash::TypeHash,
};

// The first block of each layer contains metadata for the rest of the layer.
#[derive(Debug, Serialize, Deserialize, TypeHash, Versioned)]
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
        let offset = res?;
        ensure!(
            u64::from(offset) < self.layer.layer_info.block_size
                && usize::try_from(offset).unwrap() > BLOCK_HEADER_SIZE,
            FxfsError::Inconsistent
        );
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
            let len = self.layer.object_handle.read(self.pos, self.buffer.buffer.as_mut()).await?;
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

impl SimplePersistentLayer {
    /// Opens an existing layer that is accessible via |object_handle| (which provides a read
    /// interface to the object).  The layer should have been written prior using
    /// SimplePersistentLayerWriter.
    pub async fn open(object_handle: impl ReadObjectHandle + 'static) -> Result<Arc<Self>, Error> {
        let size = object_handle.get_size();
        let physical_block_size = object_handle.block_size();

        // The first block contains layer file information instead of key/value data.
        let (layer_info, _version) = {
            let mut buffer = object_handle.allocate_buffer(physical_block_size as usize);
            object_handle.read(0, buffer.as_mut()).await?;
            let mut cursor = std::io::Cursor::new(buffer.as_slice());
            LayerInfo::deserialize_with_version(&mut cursor)
                .context("Failed to deserialize LayerInfo")?
        };

        // We expect the layer block size to be a multiple of the physical block size.
        ensure!(layer_info.block_size % physical_block_size == 0, FxfsError::Inconsistent);
        ensure!(layer_info.block_size <= MAX_BLOCK_SIZE, FxfsError::NotSupported);

        // Catch obviously bad sizes.
        ensure!(size < u64::MAX - layer_info.block_size, FxfsError::Inconsistent);

        Ok(Arc::new(SimplePersistentLayer {
            object_handle: Arc::new(object_handle),
            layer_info,
            size,
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
                iterator.advance().await?;
                return Ok(Box::new(iterator));
            }
            Bound::Included(k) => (k, false),
            Bound::Excluded(k) => (k, true),
        };
        // Skip the first block. We Store version info there for now.
        let mut left_offset = self.layer_info.block_size;
        let mut right_offset = round_up(self.size, self.layer_info.block_size).unwrap();
        let mut left = Iterator::new(self, left_offset);
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
                left.buffer.pos = left.offset_for_index(mid_index.into())? as usize;
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
                right.buffer.pos = right.offset_for_index(right_index.into())? as usize;
                right.item_index = u16::try_from(right_index).unwrap();
                right.advance().await?;
            };
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
        }

        self.item_count += 1;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Error> {
        self.write_some(self.buf.len()).await?;
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
        super::{SimplePersistentLayer, SimplePersistentLayerWriter},
        crate::{
            filesystem::MAX_BLOCK_SIZE,
            lsm_tree::types::{DefaultOrdUpperBound, Item, ItemRef, Layer, LayerWriter},
            object_handle::Writer,
            round::round_up,
            testing::fake_object::{FakeObject, FakeObjectHandle},
        },
        std::{fmt::Debug, ops::Bound, sync::Arc},
    };

    impl DefaultOrdUpperBound for i32 {}

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
        // Items will be 16 bytes, so fill up a few pages.
        const ITEM_COUNT: i32 = ((BLOCK_SIZE as i32) / 16) * 3;

        let handle =
            FakeObjectHandle::new_with_block_size(Arc::new(FakeObject::new()), BLOCK_SIZE as usize);
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
}
