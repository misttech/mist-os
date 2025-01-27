// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A snapshot represents all the loaded blocks of the VMO in a way that we can reconstruct the
//! implicit tree.

use crate::reader::error::ReaderError;
use crate::reader::readable_tree::SnapshotSource;
use crate::reader::LinkValue;
use crate::Inspector;
use diagnostics_hierarchy::{ArrayContent, Property};
use inspect_format::{
    constants, utils, Block, BlockAccessorExt, BlockContainer, BlockIndex, BlockType, Container,
    CopyBytes, Error as FormatError, PropertyFormat, ReadBytes,
};
use itertools::Itertools;
use std::cmp;

pub use crate::reader::tree_reader::SnapshotTree;

/// Enables to scan all the blocks in a given buffer.
#[derive(Debug)]
pub struct Snapshot {
    /// The buffer read from an Inspect VMO.
    buffer: BackingBuffer,
}

/// A scanned block.
pub type ScannedBlock<'a> = Block<&'a BackingBuffer>;

const SNAPSHOT_TRIES: u64 = 1024;

impl Snapshot {
    /// Returns an iterator that returns all the Blocks in the buffer.
    pub fn scan(&self) -> BlockIterator<'_> {
        BlockIterator::from(&self.buffer)
    }

    /// Gets the block at the given |index|.
    pub fn get_block(&self, index: BlockIndex) -> Result<ScannedBlock<'_>, ReaderError> {
        if index.offset() < self.buffer.len() {
            Ok(self.buffer.block_at(index))
        } else {
            Err(ReaderError::GetBlock(index))
        }
    }

    /// Try to take a consistent snapshot of the given VMO once.
    ///
    /// Returns a Snapshot on success or an Error if a consistent snapshot could not be taken.
    pub fn try_once_from_vmo(source: &SnapshotSource) -> Result<Snapshot, ReaderError> {
        Snapshot::try_once_with_callback(source, &mut || {})
    }

    fn try_once_with_callback<F>(
        source: &SnapshotSource,
        read_callback: &mut F,
    ) -> Result<Snapshot, ReaderError>
    where
        F: FnMut(),
    {
        // Read the generation count one time
        let mut header_bytes: [u8; 32] = [0; 32];
        source.copy_bytes(&mut header_bytes);
        let header_block = header_bytes.block_at(BlockIndex::HEADER);
        let generation = header_block.header_generation_count();

        let Ok(gen) = generation else {
            return Err(ReaderError::InconsistentSnapshot);
        };
        if gen == constants::VMO_FROZEN {
            if let Ok(buffer) = BackingBuffer::try_from(source) {
                return Ok(Snapshot { buffer });
            }
        }

        // Read the buffer
        let vmo_size = if let Some(vmo_size) = header_block.header_vmo_size()? {
            cmp::min(vmo_size as usize, constants::MAX_VMO_SIZE)
        } else {
            cmp::min(source.len(), constants::MAX_VMO_SIZE)
        };
        let mut buffer = vec![0u8; vmo_size];
        source.copy_bytes(&mut buffer);
        if cfg!(test) {
            read_callback();
        }

        // Read the generation count one more time to ensure the previous buffer read is
        // consistent. It's safe to unwrap this time, we already checked we can read 32 bytes from
        // the slice.
        source.copy_bytes(&mut header_bytes);
        match header_generation_count(&header_bytes) {
            None => Err(ReaderError::InconsistentSnapshot),
            Some(new_generation) if new_generation != gen => Err(ReaderError::InconsistentSnapshot),
            Some(_) => Ok(Snapshot { buffer: BackingBuffer::from(buffer) }),
        }
    }

    fn try_from_with_callback<F>(
        source: &SnapshotSource,
        mut read_callback: F,
    ) -> Result<Snapshot, ReaderError>
    where
        F: FnMut(),
    {
        let mut i = 0;
        loop {
            match Snapshot::try_once_with_callback(source, &mut read_callback) {
                Ok(snapshot) => return Ok(snapshot),
                Err(e) => {
                    if i >= SNAPSHOT_TRIES {
                        return Err(e);
                    }
                }
            };
            i += 1;
        }
    }

    pub(crate) fn get_name(&self, index: BlockIndex) -> Option<String> {
        let block = self.get_block(index).ok()?;

        match block.block_type() {
            BlockType::Name => self.load_name(block),
            BlockType::StringReference => self.load_string_reference(block).ok(),
            _ => None,
        }
    }

    pub(crate) fn load_name(&self, block: ScannedBlock<'_>) -> Option<String> {
        block.name_contents().ok().map(|s| s.to_string())
    }

    pub(crate) fn load_string_reference(
        &self,
        block: ScannedBlock<'_>,
    ) -> Result<String, ReaderError> {
        let mut data = block.inline_string_reference()?.to_vec();
        let total_length = block.total_length()?;
        if data.len() == total_length {
            return Ok(String::from_utf8_lossy(&data).to_string());
        }

        let extent_index = block.next_extent()?;
        let still_to_read_length = total_length - data.len();
        data.append(&mut self.read_extents(still_to_read_length, extent_index)?);

        Ok(String::from_utf8_lossy(&data).to_string())
    }

    pub(crate) fn parse_numeric_property(
        &self,
        block: &ScannedBlock<'_>,
    ) -> Result<Property, ReaderError> {
        let name_index = block.name_index()?;
        let name = self.get_name(name_index).ok_or(ReaderError::ParseName(name_index))?;
        match block.block_type() {
            BlockType::IntValue => {
                let value = block.int_value()?;
                Ok(Property::Int(name, value))
            }
            BlockType::UintValue => {
                let value = block.uint_value()?;
                Ok(Property::Uint(name, value))
            }
            BlockType::DoubleValue => {
                let value = block.double_value()?;
                Ok(Property::Double(name, value))
            }
            _ => Err(ReaderError::VmoFormat(FormatError::UnexpectedBlockTypeRepr(
                "Number".to_string(),
                block.block_type(),
            ))),
        }
    }

    pub(crate) fn parse_bool_property(
        &self,
        block: &ScannedBlock<'_>,
    ) -> Result<Property, ReaderError> {
        let name_index = block.name_index()?;
        let name = self.get_name(name_index).ok_or(ReaderError::ParseName(name_index))?;
        if matches!(block.block_type(), BlockType::BoolValue) {
            let value = block.bool_value()?;
            Ok(Property::Bool(name, value))
        } else {
            Err(ReaderError::VmoFormat(FormatError::UnexpectedBlockType(
                BlockType::BoolValue,
                block.block_type(),
            )))
        }
    }

    pub(crate) fn parse_array_property(
        &self,
        block: &ScannedBlock<'_>,
    ) -> Result<Property, ReaderError> {
        let name_index = block.name_index()?;
        let name = self.get_name(name_index).ok_or(ReaderError::ParseName(name_index))?;
        let array_slots = block.array_slots()?;
        // Safety: So long as the array is valid, array_capacity will return a valid value.
        if utils::array_capacity(block.order(), block.array_entry_type()?).unwrap() < array_slots {
            return Err(ReaderError::AttemptedToReadTooManyArraySlots(block.index()));
        }
        let value_indexes = 0..array_slots;
        let parsed_property = match block.array_entry_type()? {
            BlockType::IntValue => {
                let values = value_indexes
                    // Safety: in release mode, this can only error for index-out-of-bounds.
                    // We check above that indexes are in-bounds.
                    .map(|i| block.array_get_int_slot(i).unwrap())
                    .collect::<Vec<i64>>();
                Property::IntArray(
                    name,
                    // Safety: if the block is an array, it must have an array format.
                    // We have already verified it is an array.
                    ArrayContent::new(values, block.array_format().unwrap())?,
                )
            }
            BlockType::UintValue => {
                let values = value_indexes
                    // Safety: in release mode, this can only error for index-out-of-bounds.
                    // We check above that indexes are in-bounds.
                    .map(|i| block.array_get_uint_slot(i).unwrap())
                    .collect::<Vec<u64>>();
                Property::UintArray(
                    name,
                    // Safety: if the block is an array, it must have an array format.
                    // We have already verified it is an array.
                    ArrayContent::new(values, block.array_format().unwrap())?,
                )
            }
            BlockType::DoubleValue => {
                let values = value_indexes
                    // Safety: in release mode, this can only error for index-out-of-bounds.
                    // We check above that indexes are in-bounds.
                    .map(|i| block.array_get_double_slot(i).unwrap())
                    .collect::<Vec<f64>>();
                Property::DoubleArray(
                    name,
                    // Safety: if the block is an array, it must have an array format.
                    // We have already verified it is an array.
                    ArrayContent::new(values, block.array_format().unwrap())?,
                )
            }
            BlockType::StringReference => {
                let values = value_indexes
                    .map(|i| block.array_get_string_index_slot(i))
                    .map_ok(|string_idx| {
                        // default initialize unset values -- 0 index is never a string, it is always
                        // the header block
                        if string_idx == BlockIndex::EMPTY {
                            return Ok(String::new());
                        }

                        let ref_block = self.get_block(string_idx)?;
                        self.load_string_reference(ref_block)
                    })
                    .collect::<Result<Result<Vec<String>, _>, _>>()??;
                Property::StringList(name, values)
            }
            t => return Err(ReaderError::UnexpectedArrayEntryFormat(t)),
        };
        Ok(parsed_property)
    }

    pub(crate) fn parse_property(&self, block: &ScannedBlock<'_>) -> Result<Property, ReaderError> {
        let name_index = block.name_index()?;
        let name = self.get_name(name_index).ok_or(ReaderError::ParseName(name_index))?;
        let data_index = block.property_extent_index()?;
        match block.property_format()? {
            PropertyFormat::String => {
                let total_length = block.total_length()?;
                let buffer = self.read_extents(total_length, data_index)?;
                Ok(Property::String(name, String::from_utf8_lossy(&buffer).to_string()))
            }
            PropertyFormat::Bytes => {
                let total_length = block.total_length()?;
                let buffer = self.read_extents(total_length, data_index)?;
                Ok(Property::Bytes(name, buffer))
            }
            PropertyFormat::StringReference => {
                let data_head = self.get_block(data_index)?;
                Ok(Property::String(name, self.load_string_reference(data_head)?))
            }
        }
    }

    pub(crate) fn parse_link(&self, block: &ScannedBlock<'_>) -> Result<LinkValue, ReaderError> {
        let name_index = block.name_index()?;
        let name = self.get_name(name_index).ok_or(ReaderError::ParseName(name_index))?;
        let link_content_index = block.link_content_index()?;
        let content =
            self.get_name(link_content_index).ok_or(ReaderError::ParseName(link_content_index))?;
        let disposition = block.link_node_disposition()?;
        Ok(LinkValue { name, content, disposition })
    }

    // Incrementally add the contents of each extent in the extent linked list
    // until we reach the last extent or the maximum expected length.
    pub(crate) fn read_extents(
        &self,
        total_length: usize,
        first_extent: BlockIndex,
    ) -> Result<Vec<u8>, ReaderError> {
        let mut buffer = vec![0u8; total_length];
        let mut offset = 0;
        let mut extent_index = first_extent;
        while extent_index != BlockIndex::EMPTY && offset < total_length {
            let extent = self.get_block(extent_index)?;
            let content = extent.extent_contents()?;
            let extent_length = cmp::min(total_length - offset, content.len());
            buffer[offset..offset + extent_length].copy_from_slice(&content[..extent_length]);
            offset += extent_length;
            extent_index = extent.next_extent()?;
        }

        Ok(buffer)
    }

    // Used for snapshot tests.
    #[cfg(test)]
    pub fn build(bytes: &[u8]) -> Self {
        Snapshot { buffer: BackingBuffer::from(bytes.to_vec()) }
    }
}

/// Reads the given 16 bytes as an Inspect Block Header and returns the
/// generation count if the header is valid: correct magic number, version number
/// and nobody is writing to it.
fn header_generation_count<T: ReadBytes>(bytes: &T) -> Option<u64> {
    if bytes.len() < 16 {
        return None;
    }
    let block = Block::new(bytes, BlockIndex::HEADER);
    if block.block_type_or().unwrap_or(BlockType::Reserved) == BlockType::Header
        && block.header_magic().unwrap() == constants::HEADER_MAGIC_NUMBER
        && block.header_version().unwrap() <= constants::HEADER_VERSION_NUMBER
        && !block.header_is_locked().unwrap()
    {
        return block.header_generation_count().ok();
    }
    None
}

/// Construct a snapshot from a byte vector.
impl TryFrom<Vec<u8>> for Snapshot {
    type Error = ReaderError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if header_generation_count(&bytes).is_some() {
            Ok(Snapshot { buffer: BackingBuffer::from(bytes) })
        } else {
            Err(ReaderError::MissingHeaderOrLocked)
        }
    }
}

impl TryFrom<&Inspector> for Snapshot {
    type Error = ReaderError;

    fn try_from(inspector: &Inspector) -> Result<Self, Self::Error> {
        let handle = inspector.get_storage_handle();
        let storage = handle.as_ref().ok_or(ReaderError::NoOpInspector)?;
        Snapshot::try_from_with_callback(storage, || {})
    }
}

#[cfg(target_os = "fuchsia")]
impl TryFrom<&zx::Vmo> for Snapshot {
    type Error = ReaderError;

    fn try_from(vmo: &zx::Vmo) -> Result<Self, Self::Error> {
        Snapshot::try_from_with_callback(vmo, || {})
    }
}

#[cfg(not(target_os = "fuchsia"))]
impl TryFrom<&Vec<u8>> for Snapshot {
    type Error = ReaderError;

    fn try_from(buffer: &Vec<u8>) -> Result<Self, Self::Error> {
        Snapshot::try_from_with_callback(buffer, || {})
    }
}

/// Iterates over a byte array containing Inspect API blocks and returns the
/// blocks in order.
pub struct BlockIterator<'a> {
    /// Current offset at which the iterator is reading.
    offset: usize,

    /// The bytes being read.
    container: &'a BackingBuffer,
}

impl<'a> From<&'a BackingBuffer> for BlockIterator<'a> {
    fn from(container: &'a BackingBuffer) -> Self {
        BlockIterator { offset: 0, container }
    }
}

impl<'a> Iterator for BlockIterator<'a> {
    type Item = ScannedBlock<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.container.len() {
            return None;
        }
        let index = BlockIndex::from_offset(self.offset);
        let block = Block::new(self.container, index);
        if self.container.len() - self.offset < utils::order_to_size(block.order()) {
            return None;
        }
        self.offset += utils::order_to_size(block.order());
        Some(block)
    }
}

#[derive(Debug)]
pub enum BackingBuffer {
    Bytes(Vec<u8>),
    Container(Container),
}

#[cfg(target_os = "fuchsia")]
impl TryFrom<&zx::Vmo> for BackingBuffer {
    type Error = ReaderError;
    fn try_from(source: &zx::Vmo) -> Result<Self, Self::Error> {
        let container = Container::read_only(source)?;
        Ok(BackingBuffer::Container(container))
    }
}

#[cfg(not(target_os = "fuchsia"))]
impl TryFrom<&Vec<u8>> for BackingBuffer {
    type Error = ReaderError;
    fn try_from(source: &Vec<u8>) -> Result<Self, Self::Error> {
        let container = Container::read_only(source);
        Ok(BackingBuffer::Container(container))
    }
}

impl From<Vec<u8>> for BackingBuffer {
    fn from(v: Vec<u8>) -> Self {
        BackingBuffer::Bytes(v)
    }
}

impl ReadBytes for BackingBuffer {
    fn get_slice_at(&self, offset: usize, size: usize) -> Option<&[u8]> {
        match &self {
            BackingBuffer::Container(m) => m.get_slice_at(offset, size),
            BackingBuffer::Bytes(b) => b.get_slice_at(offset, size),
        }
    }
}

impl BlockContainer for BackingBuffer {
    type Data = Self;
    type ShareableData = ();

    fn len(&self) -> usize {
        match &self {
            BackingBuffer::Container(m) => m.len(),
            BackingBuffer::Bytes(v) => v.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;
    use assert_matches::assert_matches;
    use inspect_format::{BlockAccessorMutExt, WriteBytes};

    #[cfg(target_os = "fuchsia")]
    macro_rules! get_snapshot {
        ($container:ident, $storage:ident, $callback:expr) => {
            Snapshot::try_from_with_callback(&$storage, $callback)
        };
    }

    #[cfg(not(target_os = "fuchsia"))]
    macro_rules! get_snapshot {
        ($container:ident, $storage:ident, $callback:expr) => {{
            let _storage = $storage;
            let slice = $container.get_slice($container.len()).unwrap().to_vec();
            Snapshot::try_from_with_callback(&slice, $callback)
        }};
    }

    #[fuchsia::test]
    fn scan() -> Result<(), Error> {
        let size = 4096;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        {
            let mut header = Block::new_free(
                &mut container,
                BlockIndex::HEADER,
                constants::HEADER_ORDER,
                BlockIndex::EMPTY,
            )?;
            header.become_reserved()?;
            header.become_header(size)?;
        }
        {
            let mut b = Block::new_free(&mut container, 2.into(), 2, BlockIndex::EMPTY)?;
            b.become_reserved()?;
            b.become_extent(6.into())?;
        }
        {
            let mut b = Block::new_free(&mut container, 6.into(), 0, BlockIndex::EMPTY)?;
            b.become_reserved()?;
            b.become_int_value(1, 3.into(), 4.into())?;
        }

        let snapshot = get_snapshot!(container, storage, || {})?;

        // Scan blocks
        let blocks = snapshot.scan().collect::<Vec<ScannedBlock<'_>>>();

        assert_eq!(blocks[0].block_type(), BlockType::Header);
        assert_eq!(*blocks[0].index(), 0);
        assert_eq!(blocks[0].order(), constants::HEADER_ORDER);
        assert_eq!(blocks[0].header_magic().unwrap(), constants::HEADER_MAGIC_NUMBER);
        assert_eq!(blocks[0].header_version().unwrap(), constants::HEADER_VERSION_NUMBER);

        assert_eq!(blocks[1].block_type(), BlockType::Extent);
        assert_eq!(*blocks[1].index(), 2);
        assert_eq!(blocks[1].order(), 2);
        assert_eq!(*blocks[1].next_extent().unwrap(), 6);

        assert_eq!(blocks[2].block_type(), BlockType::IntValue);
        assert_eq!(*blocks[2].index(), 6);
        assert_eq!(blocks[2].order(), 0);
        assert_eq!(*blocks[2].name_index().unwrap(), 3);
        assert_eq!(*blocks[2].parent_index().unwrap(), 4);
        assert_eq!(blocks[2].int_value().unwrap(), 1);

        assert!(blocks[3..].iter().all(|b| b.block_type() == BlockType::Free));

        // Verify get_block
        assert_eq!(snapshot.get_block(0.into()).unwrap().block_type(), BlockType::Header);
        assert_eq!(snapshot.get_block(2.into()).unwrap().block_type(), BlockType::Extent);
        assert_eq!(snapshot.get_block(6.into()).unwrap().block_type(), BlockType::IntValue);
        assert_eq!(snapshot.get_block(7.into()).unwrap().block_type(), BlockType::Free);
        let bad_index = BlockIndex::from(4096);
        assert_matches!(
            snapshot.get_block(bad_index),
            Err(ReaderError::GetBlock(index)) if index == bad_index
        );

        Ok(())
    }

    #[fuchsia::test]
    fn scan_bad_header() -> Result<(), Error> {
        let (mut container, storage) = Container::read_and_write(4096).unwrap();

        // create a header block with an invalid version number
        container.copy_from_slice(&[
            0x00, /* order/reserved */
            0x02, /* type */
            0xff, /* invalid version number */
            b'I', b'N', b'S', b'P',
        ]);
        assert!(get_snapshot!(container, storage, || {}).is_err());
        Ok(())
    }

    #[fuchsia::test]
    fn invalid_type() -> Result<(), Error> {
        let (mut container, storage) = Container::read_and_write(4096).unwrap();
        container.copy_from_slice(&[0x00, 0xff, 0x01]);
        assert!(get_snapshot!(container, storage, || {}).is_err());
        Ok(())
    }

    #[fuchsia::test]
    fn invalid_order() -> Result<(), Error> {
        let (mut container, storage) = Container::read_and_write(4096).unwrap();
        container.copy_from_slice(&[0xff, 0xff]);
        assert!(get_snapshot!(container, storage, || {}).is_err());
        Ok(())
    }

    #[fuchsia::test]
    fn invalid_pending_write() -> Result<(), Error> {
        let size = 4096;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        let mut header = Block::new_free(
            &mut container,
            BlockIndex::HEADER,
            constants::HEADER_ORDER,
            BlockIndex::EMPTY,
        )?;
        header.become_reserved()?;
        header.become_header(size)?;
        header.lock_header()?;
        assert!(get_snapshot!(container, storage, || {}).is_err());
        Ok(())
    }

    #[fuchsia::test]
    fn invalid_magic_number() -> Result<(), Error> {
        let size = 4096;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        let mut header = Block::new_free(
            &mut container,
            BlockIndex::HEADER,
            constants::HEADER_ORDER,
            BlockIndex::EMPTY,
        )?;
        header.become_reserved()?;
        header.become_header(size)?;
        header.set_header_magic(3)?;
        assert!(get_snapshot!(container, storage, || {}).is_err());
        Ok(())
    }

    #[fuchsia::test]
    fn invalid_generation_count() -> Result<(), Error> {
        let size = 4096;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        let mut header = Block::new_free(
            &mut container,
            BlockIndex::HEADER,
            constants::HEADER_ORDER,
            BlockIndex::EMPTY,
        )?;
        header.become_reserved()?;
        header.become_header(size)?;
        let result = get_snapshot!(container, storage, || {
            let mut header = container.block_at_mut(BlockIndex::HEADER);
            header.lock_header().unwrap();
            header.unlock_header().unwrap();
        });
        #[cfg(target_os = "fuchsia")]
        assert!(result.is_err());
        // When in the host, we don't have underlying shared memory, so this can't fail as we
        // had already cloned the underlying vector.
        #[cfg(not(target_os = "fuchsia"))]
        assert!(result.is_ok());
        Ok(())
    }

    #[fuchsia::test]
    fn snapshot_from_few_bytes() {
        let values = (0u8..16).collect::<Vec<u8>>();
        assert!(Snapshot::try_from(values.clone()).is_err());
        assert!(Snapshot::try_from(values).is_err());
        assert!(Snapshot::try_from(vec![]).is_err());
        assert!(Snapshot::try_from(vec![0u8, 1, 2, 3, 4]).is_err());
    }

    #[fuchsia::test]
    fn snapshot_frozen_vmo() -> Result<(), Error> {
        let size = 4096;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        {
            let mut header = Block::new_free(
                &mut container,
                BlockIndex::HEADER,
                constants::HEADER_ORDER,
                BlockIndex::EMPTY,
            )?;
            header.become_reserved()?;
            header.become_header(size)?;
        }
        container.copy_from_slice_at(8, &[0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);

        let snapshot = get_snapshot!(container, storage, || {})?;

        assert!(matches!(snapshot.buffer, BackingBuffer::Container(_)));

        container.copy_from_slice_at(8, &[2u8; 8]);
        let snapshot = get_snapshot!(container, storage, || {})?;
        assert!(matches!(snapshot.buffer, BackingBuffer::Bytes(_)));

        Ok(())
    }

    #[fuchsia::test]
    fn snapshot_vmo_with_unused_space() -> Result<(), Error> {
        let size = 4 * constants::PAGE_SIZE_BYTES;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        let mut header = Block::new_free(
            &mut container,
            BlockIndex::HEADER,
            constants::HEADER_ORDER,
            BlockIndex::EMPTY,
        )?;
        header.become_reserved()?;
        header.become_header(constants::PAGE_SIZE_BYTES)?;

        let snapshot = get_snapshot!(container, storage, || {})?;
        assert_eq!(snapshot.buffer.len(), constants::PAGE_SIZE_BYTES);

        Ok(())
    }

    #[fuchsia::test]
    fn snapshot_vmo_with_very_large_vmo() -> Result<(), Error> {
        let size = 2 * constants::MAX_VMO_SIZE;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        let mut header = Block::new_free(
            &mut container,
            BlockIndex::HEADER,
            constants::HEADER_ORDER,
            BlockIndex::EMPTY,
        )?;
        header.become_reserved()?;
        header.become_header(size)?;

        let snapshot = get_snapshot!(container, storage, || {})?;
        assert_eq!(snapshot.buffer.len(), constants::MAX_VMO_SIZE);

        Ok(())
    }

    #[fuchsia::test]
    fn snapshot_vmo_with_header_without_size_info() -> Result<(), Error> {
        let size = 2 * constants::PAGE_SIZE_BYTES;
        let (mut container, storage) = Container::read_and_write(size).unwrap();
        let mut header = Block::new_free(&mut container, BlockIndex::HEADER, 0, BlockIndex::EMPTY)?;
        header.become_reserved()?;
        header.become_header(constants::PAGE_SIZE_BYTES)?;
        header.set_order(0)?;

        let snapshot = get_snapshot!(container, storage, || {})?;
        assert_eq!(snapshot.buffer.len(), size);

        Ok(())
    }
}
