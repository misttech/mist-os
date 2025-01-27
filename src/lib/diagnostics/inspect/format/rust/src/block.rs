// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for writing VMO blocks in a type-safe way.

use crate::bitfields::{HeaderFields, PayloadFields};
use crate::block_index::BlockIndex;
use crate::block_type::BlockType;
use crate::container::{ReadBytes, WriteBytes};
use crate::error::Error;
use crate::{constants, utils};
use byteorder::{ByteOrder, LittleEndian};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use std::cmp::min;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{fence, Ordering};

pub use diagnostics_hierarchy::ArrayFormat;

/// Disposition of a Link value.
#[derive(Clone, Debug, PartialEq, Eq, FromPrimitive)]
#[repr(u8)]
pub enum LinkNodeDisposition {
    Child = 0,
    Inline = 1,
}

/// Format in which the property will be read.
#[derive(Debug, PartialEq, Eq, FromPrimitive)]
#[repr(u8)]
pub enum PropertyFormat {
    String = 0,
    Bytes = 1,
    StringReference = 2,
}

/// Points to an index in the VMO and reads it according to the bytes in it.
#[derive(Debug, Clone)]
pub struct Block<T> {
    pub(crate) index: BlockIndex,
    pub(crate) container: T,
}

impl<T: Deref<Target = Q>, Q> Block<T> {
    /// Creates a new block.
    #[inline]
    pub fn new(container: T, index: BlockIndex) -> Self {
        Block { container, index }
    }
}

pub trait BlockAccessorExt: ReadBytes + Sized {
    #[inline]
    fn block_at(&self, index: BlockIndex) -> Block<&Self> {
        Block::new(self, index)
    }
}

pub trait BlockAccessorMutExt: WriteBytes + Sized {
    #[inline]
    fn block_at_mut(&mut self, index: BlockIndex) -> Block<&mut Self> {
        Block::new(self, index)
    }
}

impl<T> BlockAccessorExt for T where T: ReadBytes {}
impl<T> BlockAccessorMutExt for T where T: WriteBytes {}

impl<'a, T: Deref<Target = Q>, Q: ReadBytes + 'a> Block<T> {
    /// Returns index of the block in the vmo.
    pub fn index(&self) -> BlockIndex {
        self.index
    }

    /// Returns the order of the block.
    pub fn order(&self) -> u8 {
        HeaderFields::order(self)
    }

    /// Returns the magic number in a HEADER block.
    pub fn header_magic(&self) -> Result<u32, Error> {
        self.check_type(BlockType::Header)?;
        Ok(HeaderFields::header_magic(self))
    }

    /// Returns the version of a HEADER block.
    pub fn header_version(&self) -> Result<u32, Error> {
        self.check_type(BlockType::Header)?;
        Ok(HeaderFields::header_version(self))
    }

    /// Returns the generation count of a HEADER block.
    pub fn header_generation_count(&self) -> Result<u64, Error> {
        self.check_type(BlockType::Header)?;
        Ok(PayloadFields::header_generation_count(self))
    }

    /// Returns the size of the part of the VMO that is currently allocated. The size
    /// is saved in a field in the HEADER block.
    pub fn header_vmo_size(&self) -> Result<Option<u32>, Error> {
        self.check_type(BlockType::Header)?;
        if self.order() != constants::HEADER_ORDER {
            return Ok(None);
        }
        let offset = (self.index + 1).offset();
        let value = self.container.get_value(offset).ok_or(Error::InvalidOffset(offset))?;
        Ok(Some(*value))
    }

    /// True if the header is locked, false otherwise.
    pub fn header_is_locked(&self) -> Result<bool, Error> {
        self.check_type(BlockType::Header)?;
        Ok(PayloadFields::header_generation_count(self) & 1 == 1)
    }

    /// Gets the double value of a DOUBLE_VALUE block.
    pub fn double_value(&self) -> Result<f64, Error> {
        self.check_type(BlockType::DoubleValue)?;
        Ok(f64::from_bits(PayloadFields::numeric_value(self)))
    }

    /// Gets the value of an INT_VALUE block.
    pub fn int_value(&self) -> Result<i64, Error> {
        self.check_type(BlockType::IntValue)?;
        Ok(i64::from_le_bytes(PayloadFields::numeric_value(self).to_le_bytes()))
    }

    /// Gets the unsigned value of a UINT_VALUE block.
    pub fn uint_value(&self) -> Result<u64, Error> {
        self.check_type(BlockType::UintValue)?;
        Ok(PayloadFields::numeric_value(self))
    }

    /// Gets the bool values of a BOOL_VALUE block.
    pub fn bool_value(&self) -> Result<bool, Error> {
        self.check_type(BlockType::BoolValue)?;
        Ok(PayloadFields::numeric_value(self) != 0)
    }

    /// Gets the index of the EXTENT of the PROPERTY block.
    pub fn property_extent_index(&self) -> Result<BlockIndex, Error> {
        self.check_type(BlockType::BufferValue)?;
        Ok(PayloadFields::property_extent_index(self).into())
    }

    /// Gets the total length of a PROPERTY or STRING_REFERERENCE block.
    pub fn total_length(&self) -> Result<usize, Error> {
        self.check_multi_type(&[BlockType::BufferValue, BlockType::StringReference])?;
        Ok(PayloadFields::property_total_length(self) as usize)
    }

    /// Gets the flags of a PROPERTY block.
    pub fn property_format(&self) -> Result<PropertyFormat, Error> {
        self.check_type(BlockType::BufferValue)?;
        let raw_format = PayloadFields::property_flags(self);

        PropertyFormat::from_u8(raw_format)
            .ok_or_else(|| Error::invalid_flags("property", raw_format, self.index()))
    }

    /// Returns the next EXTENT in an EXTENT chain.
    pub fn next_extent(&self) -> Result<BlockIndex, Error> {
        self.check_multi_type(&[BlockType::Extent, BlockType::StringReference])?;
        Ok(HeaderFields::extent_next_index(self).into())
    }

    /// Returns the payload bytes value of an EXTENT block.
    pub fn extent_contents(&'a self) -> Result<&'a [u8], Error> {
        self.check_type(BlockType::Extent)?;
        let length = utils::payload_size_for_order(self.order());
        let offset = self.payload_offset();
        self.container.get_slice_at(offset, length).ok_or(Error::InvalidOffset(offset))
    }

    /// Gets the NAME block index of a *_VALUE block.
    pub fn name_index(&self) -> Result<BlockIndex, Error> {
        self.check_any_value()?;
        Ok(HeaderFields::value_name_index(self).into())
    }

    /// Gets the format of an ARRAY_VALUE block.
    pub fn array_format(&self) -> Result<ArrayFormat, Error> {
        self.check_type(BlockType::ArrayValue)?;
        let raw_flags = PayloadFields::array_flags(self);
        ArrayFormat::from_u8(raw_flags)
            .ok_or_else(|| Error::invalid_flags("array", raw_flags, self.index()))
    }

    /// Gets the number of slots in an ARRAY_VALUE block.
    pub fn array_slots(&self) -> Result<usize, Error> {
        self.check_type(BlockType::ArrayValue)?;
        Ok(PayloadFields::array_slots_count(self) as usize)
    }

    /// Gets the type of each slot in an ARRAY_VALUE block.
    pub fn array_entry_type(&self) -> Result<BlockType, Error> {
        self.check_type(BlockType::ArrayValue)?;
        let array_type_raw = PayloadFields::array_entry_type(self);
        let array_type = BlockType::from_u8(array_type_raw)
            .ok_or_else(|| Error::InvalidBlockTypeNumber(self.index(), array_type_raw))?;
        if !array_type.is_valid_for_array() {
            return Err(Error::InvalidArrayType(self.index()));
        }
        Ok(array_type)
    }

    pub fn array_get_string_index_slot(&self, slot_index: usize) -> Result<BlockIndex, Error> {
        self.check_array_entry_type(BlockType::StringReference)?;
        self.check_array_index(slot_index)?;
        let entry_type_size: usize = self
            .array_entry_type()?
            .array_element_size()
            .ok_or_else(|| Error::InvalidArrayType(self.index()))?;
        let offset = (self.index + 1).offset() + slot_index * entry_type_size;
        let index = self.container.get_value(offset).ok_or(Error::InvalidOffset(offset))?;
        Ok(BlockIndex::new(*index))
    }

    /// Gets the value of an int ARRAY_VALUE slot.
    pub fn array_get_int_slot(&self, slot_index: usize) -> Result<i64, Error> {
        self.check_array_entry_type(BlockType::IntValue)?;
        self.check_array_index(slot_index)?;
        let offset = (self.index + 1).offset() + slot_index * 8;
        let value = self.container.get_value(offset).ok_or(Error::InvalidOffset(offset))?;
        Ok(*value)
    }

    /// Gets the value of a double ARRAY_VALUE slot.
    pub fn array_get_double_slot(&self, slot_index: usize) -> Result<f64, Error> {
        self.check_array_entry_type(BlockType::DoubleValue)?;
        self.check_array_index(slot_index)?;
        let offset = (self.index + 1).offset() + slot_index * 8;
        let value = self.container.get_value(offset).ok_or(Error::InvalidOffset(offset))?;
        Ok(*value)
    }

    /// Gets the value of a uint ARRAY_VALUE slot.
    pub fn array_get_uint_slot(&self, slot_index: usize) -> Result<u64, Error> {
        self.check_array_entry_type(BlockType::UintValue)?;
        self.check_array_index(slot_index)?;
        let offset = (self.index + 1).offset() + slot_index * 8;
        let value = self.container.get_value(offset).ok_or(Error::InvalidOffset(offset))?;
        Ok(*value)
    }

    /// Gets the index of the content of this LINK_VALUE block.
    pub fn link_content_index(&self) -> Result<BlockIndex, Error> {
        self.check_type(BlockType::LinkValue)?;
        Ok(PayloadFields::content_index(self).into())
    }

    /// Gets the node disposition of a LINK_VALUE block.
    pub fn link_node_disposition(&self) -> Result<LinkNodeDisposition, Error> {
        self.check_type(BlockType::LinkValue)?;
        let flag = PayloadFields::disposition_flags(self);
        LinkNodeDisposition::from_u8(flag)
            .ok_or_else(|| Error::invalid_flags("disposition type", flag, self.index()))
    }

    /// Ensures the type of the array is the expected one.
    fn check_array_entry_type(&self, expected: BlockType) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let actual = self.array_entry_type()?;
            if actual == expected {
                return Ok(());
            } else {
                return Err(Error::UnexpectedBlockType(expected, actual));
            }
        }
        Ok(())
    }

    /// Ensure that the index is within the array bounds.
    fn check_array_index(&self, slot_index: usize) -> Result<(), Error> {
        if slot_index >= self.array_slots()? {
            return Err(Error::ArrayIndexOutOfBounds(slot_index));
        }
        Ok(())
    }

    /// Get the parent block index of a *_VALUE block.
    pub fn parent_index(&self) -> Result<BlockIndex, Error> {
        self.check_any_value()?;
        Ok(HeaderFields::value_parent_index(self).into())
    }

    /// Get the child count of a NODE_VALUE block.
    pub fn child_count(&self) -> Result<u64, Error> {
        self.check_multi_type(&[BlockType::NodeValue, BlockType::Tombstone])?;
        Ok(PayloadFields::numeric_value(self))
    }

    /// Get next free block
    pub fn free_next_index(&self) -> Result<BlockIndex, Error> {
        self.check_type(BlockType::Free)?;
        Ok(HeaderFields::free_next_index(self).into())
    }

    /// Get the length of the name of a NAME block
    pub fn name_length(&self) -> Result<usize, Error> {
        self.check_type(BlockType::Name)?;
        Ok(HeaderFields::name_length(self).into())
    }

    /// Returns the contents of a NAME block.
    pub fn name_contents(&'a self) -> Result<&'a str, Error> {
        self.check_type(BlockType::Name)?;
        let length = self.name_length()?;
        let offset = self.payload_offset();
        let bytes =
            self.container.get_slice_at(offset, length).ok_or(Error::InvalidOffset(offset))?;
        std::str::from_utf8(bytes).map_err(|_| Error::NameNotUtf8)
    }

    /// Returns the current reference count of a string reference.
    pub fn string_reference_count(&self) -> Result<u32, Error> {
        self.check_type(BlockType::StringReference)?;
        Ok(HeaderFields::string_reference_count(self))
    }

    /// Read the inline portion of a STRING_REFERENCE
    pub fn inline_string_reference(&'a self) -> Result<&'a [u8], Error> {
        self.check_type(BlockType::StringReference)?;
        let max_len_inlined = utils::payload_size_for_order(self.order())
            - constants::STRING_REFERENCE_TOTAL_LENGTH_BYTES;
        let length = self.total_length()?;
        let offset = self.payload_offset() + constants::STRING_REFERENCE_TOTAL_LENGTH_BYTES;
        let bytes = self
            .container
            .get_slice_at(offset, min(length, max_len_inlined))
            .ok_or(Error::InvalidOffset(offset))?;
        Ok(bytes)
    }

    /// Returns the type of a block. Panics on an invalid value.
    pub fn block_type(&self) -> BlockType {
        let block_type = HeaderFields::block_type(self);
        // Safety: BlockType is repr(u8). We never write anything but BlockTypes here.
        BlockType::from_u8(block_type).unwrap()
    }

    /// Returns the type of a block or an error if invalid.
    pub fn block_type_or(&self) -> Result<BlockType, Error> {
        let raw_type = HeaderFields::block_type(self);
        BlockType::from_u8(raw_type)
            .ok_or_else(|| Error::InvalidBlockTypeNumber(self.index(), raw_type))
    }

    /// Check that the block type is |block_type|
    fn check_type(&self, block_type: BlockType) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let self_type = HeaderFields::block_type(self);
            return self.check_type_eq(self_type, block_type);
        }
        Ok(())
    }

    fn check_type_eq(&self, actual_num: u8, expected: BlockType) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let actual = BlockType::from_u8(actual_num)
                .ok_or(Error::InvalidBlockTypeNumber(self.index, actual_num))?;
            if actual != expected {
                return Err(Error::UnexpectedBlockType(expected, actual));
            }
        }
        Ok(())
    }

    /// Get the offset of the payload in the container.
    pub(crate) fn payload_offset(&self) -> usize {
        self.index.offset() + constants::HEADER_SIZE_BYTES
    }

    /// Get the offset of the header in the container.
    pub(crate) fn header_offset(&self) -> usize {
        self.index.offset()
    }

    /// Check if the HEADER block is locked (when generation count is odd).
    /// NOTE: this should only be used for testing.
    pub fn check_locked(&self, value: bool) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let generation_count = PayloadFields::header_generation_count(self);
            if (generation_count & 1 == 1) != value {
                return Err(Error::ExpectedLockState(value));
            }
        }
        Ok(())
    }

    /// Check if the block is one of the given types.
    fn check_multi_type(&self, options: &[BlockType]) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let mut wanted = "".to_string();
            for b in options {
                match self.check_type(*b) {
                    Ok(_) => return Ok(()),
                    Err(e) => wanted.push_str(format!("{}|", e).as_str()),
                }
            }

            Err(Error::UnexpectedBlockTypeRepr(wanted, self.block_type()))
        } else {
            Ok(())
        }
    }

    /// Check if the block is of *_VALUE.
    fn check_any_value(&self) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let block_type = self.block_type();
            if block_type.is_any_value() {
                return Ok(());
            }
            return Err(Error::UnexpectedBlockTypeRepr("*_VALUE".to_string(), block_type));
        }
        Ok(())
    }

    fn check_array_format(
        &self,
        entry_type: BlockType,
        format_type: &ArrayFormat,
    ) -> Result<(), Error> {
        if !entry_type.is_valid_for_array() {
            return Err(Error::InvalidArrayType(self.index));
        }

        match (entry_type, format_type) {
            (BlockType::StringReference, ArrayFormat::Default) => Ok(()),
            (BlockType::StringReference, _) => Err(Error::InvalidArrayType(self.index)),
            _ => Ok(()),
        }
    }

    fn array_entry_type_size(&self) -> Result<usize, Error> {
        self.array_entry_type().map(|block_type| {
            block_type.array_element_size().ok_or(Error::InvalidArrayType(self.index))
        })?
    }
}

impl<T: Deref<Target = Q> + DerefMut<Target = Q>, Q: WriteBytes + ReadBytes> Block<T> {
    /// Initializes an empty free block.
    pub fn new_free(
        container: T,
        index: BlockIndex,
        order: u8,
        next_free: BlockIndex,
    ) -> Result<Block<T>, Error> {
        if order >= constants::NUM_ORDERS {
            return Err(Error::InvalidBlockOrder(order));
        }
        let mut block = Block::new(container, index);
        HeaderFields::set_value(&mut block, 0);
        HeaderFields::set_order(&mut block, order);
        HeaderFields::set_block_type(&mut block, BlockType::Free as u8);
        HeaderFields::set_free_next_index(&mut block, *next_free);
        Ok(block)
    }

    /// Set the order of the block.
    pub fn set_order(&mut self, order: u8) -> Result<(), Error> {
        if order >= constants::NUM_ORDERS {
            return Err(Error::InvalidBlockOrder(order));
        }
        HeaderFields::set_order(self, order);
        Ok(())
    }

    /// Initializes a HEADER block.
    pub fn become_header(&mut self, size: usize) -> Result<(), Error> {
        self.check_type(BlockType::Reserved)?;
        self.index = BlockIndex::HEADER;
        HeaderFields::set_order(self, constants::HEADER_ORDER);
        HeaderFields::set_block_type(self, BlockType::Header as u8);
        HeaderFields::set_header_magic(self, constants::HEADER_MAGIC_NUMBER);
        HeaderFields::set_header_version(self, constants::HEADER_VERSION_NUMBER);
        PayloadFields::set_value(self, 0);
        // Safety: a valid `size` is smaller than a u32
        self.set_header_vmo_size(size.try_into().unwrap())?;
        Ok(())
    }

    /// Allows to set the magic value of the header.
    /// NOTE: this should only be used for testing.
    pub fn set_header_magic(&mut self, value: u32) -> Result<(), Error> {
        self.check_type(BlockType::Header)?;
        HeaderFields::set_header_magic(self, value);
        Ok(())
    }

    /// Set the size of the part of the VMO that is currently allocated. The size is saved in
    /// a field in the HEADER block.
    pub fn set_header_vmo_size(&mut self, size: u32) -> Result<(), Error> {
        self.check_type(BlockType::Header)?;
        if self.order() != constants::HEADER_ORDER {
            return Ok(());
        }
        match self.container.get_value_mut((self.index + 1).offset()) {
            Some(value) => *value = size,
            None => return Err(Error::SizeNotWritten(size)),
        }
        Ok(())
    }

    /// Freeze the HEADER, indicating a VMO is frozen.
    pub fn freeze_header(&mut self) -> Result<u64, Error> {
        self.check_type(BlockType::Header)?;
        let value = PayloadFields::header_generation_count(self);
        PayloadFields::set_header_generation_count(self, constants::VMO_FROZEN);
        Ok(value)
    }

    /// Thaw the HEADER, indicating a VMO is Live again.
    pub fn thaw_header(&mut self, gen: u64) -> Result<(), Error> {
        self.check_type(BlockType::Header)?;
        PayloadFields::set_header_generation_count(self, gen);
        Ok(())
    }

    /// Lock a HEADER block
    pub fn lock_header(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::Header)?;
        self.check_locked(false)?;
        self.increment_generation_count();
        fence(Ordering::Acquire);
        Ok(())
    }

    /// Unlock a HEADER block
    pub fn unlock_header(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::Header)?;
        self.check_locked(true)?;
        fence(Ordering::Release);
        self.increment_generation_count();
        Ok(())
    }

    /// Initializes a TOMBSTONE block.
    pub fn become_tombstone(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::NodeValue)?;
        HeaderFields::set_block_type(self, BlockType::Tombstone as u8);
        HeaderFields::set_tombstone_empty(self, 0);
        Ok(())
    }

    /// Converts a FREE block to a RESERVED block
    pub fn become_reserved(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::Free)?;
        HeaderFields::set_block_type(self, BlockType::Reserved as u8);
        HeaderFields::set_reserved_empty(self, 0);
        Ok(())
    }

    /// Converts a block to a FREE block
    pub fn become_free(&mut self, next: BlockIndex) {
        HeaderFields::set_free_reserved(self, 0);
        HeaderFields::set_block_type(self, BlockType::Free as u8);
        HeaderFields::set_free_next_index(self, *next);
        HeaderFields::set_free_empty(self, 0);
    }

    /// Converts a block to an *_ARRAY_VALUE block
    pub fn become_array_value(
        &mut self,
        slots: usize,
        format: ArrayFormat,
        entry_type: BlockType,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        self.check_array_format(entry_type, &format)?;
        let order = self.order();
        let max_capacity = utils::array_capacity(order, entry_type)
            .ok_or_else(|| Error::InvalidArrayType(self.index()))?;

        if slots > max_capacity {
            return Err(Error::array_capacity_exceeded(slots, order, max_capacity));
        }
        self.write_value_header(BlockType::ArrayValue, name_index, parent_index)?;
        PayloadFields::set_value(self, 0);
        PayloadFields::set_array_entry_type(self, entry_type as u8);
        PayloadFields::set_array_flags(self, format as u8);
        PayloadFields::set_array_slots_count(self, slots as u8);
        self.array_clear(0)?;
        Ok(())
    }

    /// Sets all values of the array to zero starting on `start_slot_index` (inclusive).
    pub fn array_clear(&mut self, start_slot_index: usize) -> Result<(), Error> {
        let array_slots = self.array_slots()? - start_slot_index;
        let type_size = self.array_entry_type_size()?;
        let offset = (self.index + 1).offset() + start_slot_index * type_size;
        let slice = self
            .container
            .get_slice_mut_at(offset, array_slots * type_size)
            .ok_or(Error::InvalidOffset(offset))?;
        slice.fill(0);
        Ok(())
    }

    /// Sets the value of a string ARRAY_VALUE block.
    pub fn array_set_string_slot(
        &mut self,
        slot_index: usize,
        string_index: BlockIndex,
    ) -> Result<(), Error> {
        if string_index != BlockIndex::EMPTY {
            self.check_type_at(string_index, BlockType::StringReference)?;
        }
        self.check_array_entry_type(BlockType::StringReference)?;
        self.check_array_index(slot_index)?;
        // 0 is used as special value; the reader won't dereference it

        let type_size = self.array_entry_type_size()?;
        self.container.set_value((self.index + 1).offset() + slot_index * type_size, *string_index);
        Ok(())
    }

    fn check_type_at(
        &self,
        index_to_check: BlockIndex,
        block_type: BlockType,
    ) -> Result<(), Error> {
        if cfg!(any(debug_assertions, test)) {
            let block = Block::new(&*self.container, index_to_check);
            return block.check_type(block_type);
        }

        Ok(())
    }

    /// Sets the value of an int ARRAY_VALUE block.
    pub fn array_set_int_slot(&mut self, slot_index: usize, value: i64) -> Result<(), Error> {
        self.check_array_entry_type(BlockType::IntValue)?;
        self.check_array_index(slot_index)?;
        let type_size = self.array_entry_type_size()?;
        self.container.set_value((self.index + 1).offset() + slot_index * type_size, value);
        Ok(())
    }

    /// Sets the value of a double ARRAY_VALUE block.
    pub fn array_set_double_slot(&mut self, slot_index: usize, value: f64) -> Result<(), Error> {
        self.check_array_entry_type(BlockType::DoubleValue)?;
        self.check_array_index(slot_index)?;
        let type_size = self.array_entry_type_size()?;
        self.container.set_value((self.index + 1).offset() + slot_index * type_size, value);
        Ok(())
    }

    /// Sets the value of a uint ARRAY_VALUE block.
    pub fn array_set_uint_slot(&mut self, slot_index: usize, value: u64) -> Result<(), Error> {
        self.check_array_entry_type(BlockType::UintValue)?;
        self.check_array_index(slot_index)?;
        let type_size = self.array_entry_type_size()?;
        self.container.set_value((self.index + 1).offset() + slot_index * type_size, value);
        Ok(())
    }

    /// Converts a block to an EXTENT block.
    pub fn become_extent(&mut self, next_extent_index: BlockIndex) -> Result<(), Error> {
        self.check_type(BlockType::Reserved)?;
        HeaderFields::set_block_type(self, BlockType::Extent as u8);
        HeaderFields::set_extent_next_index(self, *next_extent_index);
        Ok(())
    }

    /// Sets the index of the next EXTENT in the chain.
    pub fn set_extent_next_index(&mut self, next_extent_index: BlockIndex) -> Result<(), Error> {
        self.check_multi_type(&[BlockType::Extent, BlockType::StringReference])?;
        HeaderFields::set_extent_next_index(self, *next_extent_index);
        Ok(())
    }

    /// Set the payload of an EXTENT block. The number of bytes written will be returned.
    pub fn extent_set_contents(&mut self, value: &[u8]) -> Result<usize, Error> {
        self.check_type(BlockType::Extent)?;
        let order = self.order();
        let max_bytes = utils::payload_size_for_order(order);
        let mut bytes = value;
        if bytes.len() > max_bytes {
            bytes = &bytes[..min(bytes.len(), max_bytes)];
        }
        self.write_payload_from_bytes(bytes);
        Ok(bytes.len())
    }

    /// Converts a RESERVED block into a DOUBLE_VALUE block.
    pub fn become_double_value(
        &mut self,
        value: f64,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::DoubleValue, name_index, parent_index)?;
        self.set_double_value(value)
    }

    /// Sets the value of a DOUBLE_VALUE block.
    pub fn set_double_value(&mut self, value: f64) -> Result<(), Error> {
        self.check_type(BlockType::DoubleValue)?;
        PayloadFields::set_numeric_value(self, value.to_bits());
        Ok(())
    }

    /// Converts a RESERVED block into a INT_VALUE block.
    pub fn become_int_value(
        &mut self,
        value: i64,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::IntValue, name_index, parent_index)?;
        self.set_int_value(value)
    }

    /// Sets the value of an INT_VALUE block.
    pub fn set_int_value(&mut self, value: i64) -> Result<(), Error> {
        self.check_type(BlockType::IntValue)?;
        PayloadFields::set_numeric_value(self, LittleEndian::read_u64(&value.to_le_bytes()));
        Ok(())
    }

    /// Converts a block into a UINT_VALUE block.
    pub fn become_uint_value(
        &mut self,
        value: u64,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::UintValue, name_index, parent_index)?;
        self.set_uint_value(value)
    }

    /// Sets the value of a UINT_VALUE block.
    pub fn set_uint_value(&mut self, value: u64) -> Result<(), Error> {
        self.check_type(BlockType::UintValue)?;
        PayloadFields::set_numeric_value(self, value);
        Ok(())
    }

    /// Converts a block into a BOOL_VALUE block.
    pub fn become_bool_value(
        &mut self,
        value: bool,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::BoolValue, name_index, parent_index)?;
        self.set_bool_value(value)
    }

    /// Sets the value of a BOOL_VALUE block.
    pub fn set_bool_value(&mut self, value: bool) -> Result<(), Error> {
        self.check_type(BlockType::BoolValue)?;
        PayloadFields::set_numeric_value(self, value as u64);
        Ok(())
    }

    /// Initializes a NODE_VALUE block.
    pub fn become_node(
        &mut self,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::NodeValue, name_index, parent_index)?;
        PayloadFields::set_value(self, 0);
        Ok(())
    }

    /// Converts a *_VALUE block into a BUFFER_VALUE block.
    pub fn become_property(
        &mut self,
        name_index: BlockIndex,
        parent_index: BlockIndex,
        format: PropertyFormat,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::BufferValue, name_index, parent_index)?;
        PayloadFields::set_value(self, 0);
        PayloadFields::set_property_flags(self, format as u8);
        Ok(())
    }

    /// Initializes a STRING_REFERENCE block. Everything is set except for
    /// the payload string and total length.
    pub fn become_string_reference(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::Reserved)?;
        HeaderFields::set_block_type(self, BlockType::StringReference as u8);
        HeaderFields::set_extent_next_index(self, *BlockIndex::EMPTY);
        HeaderFields::set_string_reference_count(self, 0);
        Ok(())
    }

    /// Sets the total length of a BUFFER_VALUE or STRING_REFERENCE block.
    pub fn set_total_length(&mut self, length: u32) -> Result<(), Error> {
        self.check_multi_type(&[BlockType::BufferValue, BlockType::StringReference])?;
        PayloadFields::set_property_total_length(self, length);
        Ok(())
    }

    /// Sets the index of the EXTENT of a BUFFER_VALUE block.
    pub fn set_property_extent_index(&mut self, index: BlockIndex) -> Result<(), Error> {
        self.check_type(BlockType::BufferValue)?;
        PayloadFields::set_property_extent_index(self, *index);
        Ok(())
    }

    /// Set the child count of a NODE_VALUE block.
    pub fn set_child_count(&mut self, count: u64) -> Result<(), Error> {
        self.check_multi_type(&[BlockType::NodeValue, BlockType::Tombstone])?;
        PayloadFields::set_value(self, count);
        Ok(())
    }

    /// Increment the reference count by 1.
    pub fn increment_string_reference_count(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::StringReference)?;
        let cur = HeaderFields::string_reference_count(self);
        let new_count = cur.checked_add(1).ok_or(Error::InvalidReferenceCount)?;
        HeaderFields::set_string_reference_count(self, new_count);
        Ok(())
    }

    /// Decrement the reference count by 1.
    pub fn decrement_string_reference_count(&mut self) -> Result<(), Error> {
        self.check_type(BlockType::StringReference)?;
        let cur = HeaderFields::string_reference_count(self);
        let new_count = cur.checked_sub(1).ok_or(Error::InvalidReferenceCount)?;
        HeaderFields::set_string_reference_count(self, new_count);
        Ok(())
    }

    /// Write the portion of the string that fits into the STRING_REFERENCE block,
    /// as well as write the total length of value to the block.
    /// Returns the number of bytes written.
    pub fn write_string_reference_inline(&mut self, value: &[u8]) -> Result<usize, Error> {
        self.check_type(BlockType::StringReference)?;
        let payload_offset = self.payload_offset();
        self.set_total_length(value.len() as u32)?;
        let max_len = utils::payload_size_for_order(self.order())
            - constants::STRING_REFERENCE_TOTAL_LENGTH_BYTES;
        // we do not care about splitting multibyte UTF-8 characters, because the rest
        // of the split will go in an extent and be joined together at read time.
        let bytes = min(value.len(), max_len);
        let to_inline = &value[..bytes];
        self.container.copy_from_slice_at(
            payload_offset + constants::STRING_REFERENCE_TOTAL_LENGTH_BYTES,
            to_inline,
        );
        Ok(bytes)
    }

    /// Creates a NAME block.
    pub fn become_name(&mut self, name: &str) -> Result<(), Error> {
        self.check_type(BlockType::Reserved)?;
        let mut bytes = name.as_bytes();
        let max_len = utils::payload_size_for_order(self.order());
        if bytes.len() > max_len {
            bytes = &bytes[..min(bytes.len(), max_len)];
            // Make sure we didn't split a multibyte UTF-8 character; if so, delete the fragment.
            while bytes[bytes.len() - 1] & 0x80 != 0 {
                bytes = &bytes[..bytes.len() - 1];
            }
        }
        HeaderFields::set_block_type(self, BlockType::Name as u8);
        // Safety: name length must fit in 12 bytes.
        HeaderFields::set_name_length(self, u16::from_usize(bytes.len()).unwrap());
        self.write_payload_from_bytes(bytes);
        Ok(())
    }

    /// Set the next free block.
    pub fn set_free_next_index(&mut self, next_free: BlockIndex) -> Result<(), Error> {
        self.check_type(BlockType::Free)?;
        HeaderFields::set_free_next_index(self, *next_free);
        Ok(())
    }

    /// Creates a LINK block.
    pub fn become_link(
        &mut self,
        name_index: BlockIndex,
        parent_index: BlockIndex,
        content_index: BlockIndex,
        disposition_flags: LinkNodeDisposition,
    ) -> Result<(), Error> {
        self.write_value_header(BlockType::LinkValue, name_index, parent_index)?;
        PayloadFields::set_value(self, 0);
        PayloadFields::set_content_index(self, *content_index);
        PayloadFields::set_disposition_flags(self, disposition_flags as u8);
        Ok(())
    }

    pub fn set_parent(&mut self, new_parent_index: BlockIndex) -> Result<(), Error> {
        self.check_any_value()?;
        HeaderFields::set_value_parent_index(self, *new_parent_index);
        Ok(())
    }

    /// Initializes a *_VALUE block header.
    fn write_value_header(
        &mut self,
        block_type: BlockType,
        name_index: BlockIndex,
        parent_index: BlockIndex,
    ) -> Result<(), Error> {
        if !block_type.is_any_value() {
            return Err(Error::UnexpectedBlockTypeRepr("*_VALUE".to_string(), block_type));
        }
        self.check_type(BlockType::Reserved)?;
        HeaderFields::set_block_type(self, block_type as u8);
        HeaderFields::set_value_name_index(self, *name_index);
        HeaderFields::set_value_parent_index(self, *parent_index);
        Ok(())
    }

    /// Write |bytes| to the payload section of the block in the container.
    fn write_payload_from_bytes(&mut self, bytes: &[u8]) {
        let offset = self.payload_offset();
        self.container.copy_from_slice_at(offset, bytes);
    }

    /// Increment generation counter in a HEADER block for locking/unlocking
    fn increment_generation_count(&mut self) {
        let value = PayloadFields::header_generation_count(self);
        // TODO(miguelfrde): perform this in place using overflowing add.
        let new_value = value.wrapping_add(1);
        PayloadFields::set_header_generation_count(self, new_value);
    }
}

pub mod testing {
    use super::*;

    pub fn override_header<T: WriteBytes + ReadBytes>(block: &mut Block<&mut T>, value: u64) {
        block.container.set_value(block.header_offset(), value);
    }

    pub fn override_payload<T: WriteBytes + ReadBytes>(block: &mut Block<&mut T>, value: u64) {
        block.container.set_value(block.payload_offset(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Container, CopyBytes};
    use std::collections::BTreeSet;

    macro_rules! assert_8_bytes {
        ($container:ident, $offset:expr, $expected:expr) => {
            let slice = $container.get_slice_at($offset, 8).unwrap();
            assert_eq!(slice, &$expected);
        };
    }

    fn create_with_type(
        container: &mut Container,
        index: BlockIndex,
        block_type: BlockType,
    ) -> Block<&mut Container> {
        let mut block = Block::new(container, index);
        HeaderFields::set_block_type(&mut block, block_type as u8);
        HeaderFields::set_order(&mut block, 2);
        block
    }

    fn test_error_types<T>(
        f: fn(Block<&mut Container>) -> Result<T, Error>,
        error_types: &BTreeSet<BlockType>,
    ) {
        if cfg!(any(debug_assertions, test)) {
            let (mut container, _storage) =
                Container::read_and_write(constants::MIN_ORDER_SIZE * 3).unwrap();
            for block_type in BlockType::all() {
                let block = create_with_type(&mut container, BlockIndex::EMPTY, block_type);
                let result = f(block);
                if error_types.contains(&block_type) {
                    assert!(result.is_err());
                } else {
                    assert!(result.is_ok());
                }
            }
        }
    }

    fn test_ok_types<T: std::fmt::Debug, F: FnMut(Block<&mut Container>) -> Result<T, Error>>(
        mut f: F,
        ok_types: &BTreeSet<BlockType>,
    ) {
        if cfg!(any(debug_assertions, test)) {
            let (mut container, _storage) =
                Container::read_and_write(constants::MIN_ORDER_SIZE * 3).unwrap();
            for block_type in BlockType::all().iter() {
                let block = create_with_type(&mut container, BlockIndex::EMPTY, *block_type);
                let result = f(block);
                if ok_types.contains(block_type) {
                    assert!(
                        result.is_ok(),
                        "BlockType {:?} should be ok but is not: {:?}",
                        block_type,
                        result
                    );
                } else {
                    assert!(result.is_err(), "BlockType {:?} should be err but is not", block_type);
                }
            }
        }
    }

    #[fuchsia::test]
    fn test_new_free() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        assert!(Block::new_free(&mut container, 3.into(), constants::NUM_ORDERS, 1.into()).is_err());

        let res = Block::new_free(&mut container, BlockIndex::EMPTY, 3, 1.into());
        assert!(res.is_ok());
        let block = res.unwrap();
        assert_eq!(*block.index(), 0);
        assert_eq!(block.order(), 3);
        assert_eq!(*block.free_next_index().unwrap(), 1);
        assert_eq!(block.block_type(), BlockType::Free);
        assert_8_bytes!(container, 0, [0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[fuchsia::test]
    fn test_block_type_or() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        container.set_value::<u8>(1, 0x02);
        let block = Block::new(&container, BlockIndex::EMPTY);
        assert_eq!(block.block_type_or().unwrap(), BlockType::Header);

        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        container.set_value::<u8>(1, 0x0f);
        let block = Block::new(&container, BlockIndex::EMPTY);
        assert!(block.block_type_or().is_err());
    }

    #[fuchsia::test]
    fn test_set_order() {
        test_error_types(move |mut b| b.set_order(1), &BTreeSet::new());
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = Block::new_free(&mut container, BlockIndex::EMPTY, 1, 1.into()).unwrap();
        assert!(block.set_order(3).is_ok());
        assert_eq!(block.order(), 3);
    }

    #[fuchsia::test]
    fn test_become_reserved() {
        test_ok_types(
            move |mut b| b.become_reserved(),
            &BTreeSet::from_iter(vec![BlockType::Free]),
        );
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = Block::new_free(&mut container, BlockIndex::EMPTY, 1, 2.into()).unwrap();
        assert!(block.become_reserved().is_ok());
        assert_eq!(block.block_type(), BlockType::Reserved);
        assert_8_bytes!(container, 0, [0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[fuchsia::test]
    fn test_become_string_reference() {
        test_ok_types(
            move |mut b| b.become_string_reference(),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_string_reference().is_ok());
        assert_eq!(block.block_type(), BlockType::StringReference);
        assert_eq!(*block.next_extent().unwrap(), 0);
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 0);
        assert_eq!(block.inline_string_reference().unwrap(), Vec::<u8>::new());
        assert_8_bytes!(container, 0, [0x01, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[fuchsia::test]
    fn test_inline_string_reference() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        block.set_order(0).unwrap();
        assert!(block.become_string_reference().is_ok());

        assert_eq!(block.write_string_reference_inline("ab".as_bytes()).unwrap(), 2);
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 2);
        assert_eq!(block.order(), 0);
        assert_eq!(*block.next_extent().unwrap(), 0);
        assert_eq!(block.inline_string_reference().unwrap(), "ab".as_bytes());

        assert_eq!(block.write_string_reference_inline("abcd".as_bytes()).unwrap(), 4);
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 4);
        assert_eq!(block.order(), 0);
        assert_eq!(*block.next_extent().unwrap(), 0);
        assert_eq!(block.inline_string_reference().unwrap(), "abcd".as_bytes());

        assert_eq!(
            block.write_string_reference_inline("abcdefghijklmnopqrstuvwxyz".as_bytes()).unwrap(),
            4 // with order == 0, only 4 bytes will be inlined
        );
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 26);
        assert_eq!(block.order(), 0);
        assert_eq!(*block.next_extent().unwrap(), 0);
        assert_eq!(block.inline_string_reference().unwrap(), "abcd".as_bytes());

        assert_eq!(block.write_string_reference_inline("abcdef".as_bytes()).unwrap(), 4);
        assert_eq!(block.string_reference_count().unwrap(), 0);
        assert_eq!(block.total_length().unwrap(), 6);
        assert_eq!(block.order(), 0);
        assert_eq!(*block.next_extent().unwrap(), 0);
        assert_eq!(block.inline_string_reference().unwrap(), "abcd".as_bytes());
    }

    #[fuchsia::test]
    fn test_string_reference_count() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        block.set_order(0).unwrap();
        assert!(block.become_string_reference().is_ok());
        assert_eq!(block.string_reference_count().unwrap(), 0);

        assert!(block.increment_string_reference_count().is_ok());
        assert_eq!(block.string_reference_count().unwrap(), 1);

        assert!(block.decrement_string_reference_count().is_ok());
        assert_eq!(block.string_reference_count().unwrap(), 0);

        assert!(block.decrement_string_reference_count().is_err());
        assert_eq!(block.string_reference_count().unwrap(), 0);
    }

    #[fuchsia::test]
    fn test_become_header() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_header(constants::MIN_ORDER_SIZE * 2).is_ok());
        let block = block;
        assert_eq!(block.block_type(), BlockType::Header);
        assert_eq!(*block.index(), 0);
        assert_eq!(block.order(), constants::HEADER_ORDER);
        assert_eq!(block.header_magic().unwrap(), constants::HEADER_MAGIC_NUMBER);
        assert_eq!(block.header_version().unwrap(), constants::HEADER_VERSION_NUMBER);
        assert_eq!(
            block.header_vmo_size().unwrap().unwrap() as usize,
            constants::MIN_ORDER_SIZE * 2
        );
        assert_8_bytes!(container, 0, [0x01, 0x02, 0x02, 0x00, 0x49, 0x4e, 0x53, 0x50]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        test_ok_types(
            |mut b| b.become_header(constants::MIN_ORDER_SIZE * 2),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        test_ok_types(move |b| b.header_magic(), &BTreeSet::from_iter(vec![BlockType::Header]));
        test_ok_types(move |b| b.header_version(), &BTreeSet::from_iter(vec![BlockType::Header]));
    }

    #[fuchsia::test]
    fn test_freeze_thaw_header() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_header(constants::MIN_ORDER_SIZE * 2).is_ok());
        let block = block;
        assert_eq!(block.block_type(), BlockType::Header);
        assert_eq!(*block.index(), 0);
        assert_eq!(block.order(), constants::HEADER_ORDER);
        assert_eq!(block.header_magic().unwrap(), constants::HEADER_MAGIC_NUMBER);
        assert_eq!(block.header_version().unwrap(), constants::HEADER_VERSION_NUMBER);
        assert_8_bytes!(container, 0, [0x01, 0x02, 0x02, 0x00, 0x49, 0x4e, 0x53, 0x50]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let old = container.block_at_mut(BlockIndex::HEADER).freeze_header().unwrap();
        assert_8_bytes!(container, 8, [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_8_bytes!(container, 16, [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert!(container.block_at_mut(BlockIndex::HEADER).thaw_header(old).is_ok());
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x020, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[fuchsia::test]
    fn test_lock_unlock_header() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_header(&mut container, constants::MIN_ORDER_SIZE * 2);
        // Can't unlock unlocked header.
        assert!(block.unlock_header().is_err());
        assert!(block.lock_header().is_ok());
        assert!(block.header_is_locked().unwrap());
        assert_eq!(block.header_generation_count().unwrap(), 1);
        let header_bytes: [u8; 8] = [0x01, 0x02, 0x02, 0x00, 0x49, 0x4e, 0x53, 0x50];
        assert_8_bytes!(container, 0, header_bytes[..]);
        assert_8_bytes!(container, 8, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // Can't lock locked header.
        let mut block = container.block_at_mut(BlockIndex::HEADER);
        assert!(block.lock_header().is_err());
        assert!(block.unlock_header().is_ok());
        assert!(!block.header_is_locked().unwrap());
        assert_eq!(block.header_generation_count().unwrap(), 2);
        assert_8_bytes!(container, 0, header_bytes[..]);
        assert_8_bytes!(container, 8, [0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        // Test overflow: set payload bytes to max u64 value. Ensure we cannot lock
        // and after unlocking, the value is zero.
        container.set_value(8, u64::MAX);
        let mut block = container.block_at_mut(BlockIndex::HEADER);
        assert!(block.lock_header().is_err());
        assert!(block.unlock_header().is_ok());
        assert_eq!(block.header_generation_count().unwrap(), 0);
        assert_8_bytes!(container, 0, header_bytes[..]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        test_ok_types(
            move |mut b| {
                b.header_generation_count()?;
                b.lock_header()?;
                b.unlock_header()
            },
            &BTreeSet::from_iter(vec![BlockType::Header]),
        );
    }

    #[fuchsia::test]
    fn test_header_vmo_size() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_header(&mut container, constants::MIN_ORDER_SIZE * 2);
        assert!(block
            .set_header_vmo_size(constants::DEFAULT_VMO_SIZE_BYTES.try_into().unwrap())
            .is_ok());
        assert_8_bytes!(container, 0, [0x01, 0x02, 0x02, 0x00, 0x49, 0x4e, 0x53, 0x50]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 16, [0x00, 0x00, 0x4, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 24, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(
            container.block_at_mut(BlockIndex::HEADER).header_vmo_size().unwrap().unwrap() as usize,
            constants::DEFAULT_VMO_SIZE_BYTES
        );
    }

    #[fuchsia::test]
    fn test_header_vmo_size_only_for_header_block() {
        let valid = BTreeSet::from_iter(vec![BlockType::Header]);
        test_ok_types(move |b| b.header_vmo_size(), &valid);
        test_ok_types(move |mut b| b.set_header_vmo_size(1), &valid);
    }

    #[fuchsia::test]
    fn test_header_vmo_size_wrong_order() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_header(&mut container, constants::MIN_ORDER_SIZE * 2);
        assert!(block.set_header_vmo_size(constants::DEFAULT_VMO_SIZE_BYTES as u32).is_ok());
        assert!(block.set_order(0).is_ok());
        assert!(block.header_vmo_size().unwrap().is_none());
        assert!(block.set_header_vmo_size(1).is_ok());
        assert!(block.set_order(1).is_ok());
        assert_eq!(
            block.header_vmo_size().unwrap().unwrap() as usize,
            constants::DEFAULT_VMO_SIZE_BYTES
        );
    }

    #[fuchsia::test]
    fn test_become_tombstone() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_node(2.into(), 3.into()).is_ok());
        assert!(block.set_child_count(4).is_ok());
        assert!(block.become_tombstone().is_ok());
        assert_eq!(block.block_type(), BlockType::Tombstone);
        assert_eq!(block.child_count().unwrap(), 4);
        assert_8_bytes!(container, 0, [0x01, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        test_ok_types(
            move |mut b| b.become_tombstone(),
            &BTreeSet::from_iter(vec![BlockType::NodeValue]),
        );
    }

    #[fuchsia::test]
    fn test_child_count() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_node(2.into(), 3.into()).is_ok());
        assert_8_bytes!(container, 0, [0x01, 0x03, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_child_count(4).is_ok());
        assert_eq!(block.child_count().unwrap(), 4);
        assert_8_bytes!(container, 0, [0x01, 0x03, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let types = BTreeSet::from_iter(vec![BlockType::Tombstone, BlockType::NodeValue]);
        test_ok_types(move |b| b.child_count(), &types);
        test_ok_types(move |mut b| b.set_child_count(3), &types);
    }

    #[fuchsia::test]
    fn test_free() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = Block::new_free(&mut container, BlockIndex::EMPTY, 1, 1.into()).unwrap();
        assert!(block.set_free_next_index(3.into()).is_ok());
        assert_eq!(*block.free_next_index().unwrap(), 3);
        assert_8_bytes!(container, 0, [0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        test_error_types(
            move |mut b| {
                b.become_free(1.into());
                Ok(())
            },
            &BTreeSet::new(),
        );
    }

    #[fuchsia::test]
    fn test_extent() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_extent(3.into()).is_ok());
        assert_eq!(block.block_type(), BlockType::Extent);
        assert_eq!(*block.next_extent().unwrap(), 3);
        assert_8_bytes!(container, 0, [0x01, 0x08, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert_eq!(block.extent_set_contents("test-rust-inspect".as_bytes()).unwrap(), 17);
        assert_eq!(
            String::from_utf8(block.extent_contents().unwrap().to_vec()).unwrap(),
            "test-rust-inspect\0\0\0\0\0\0\0"
        );
        let slice = container.get_slice_at(8, 17).unwrap();
        assert_eq!(slice, "test-rust-inspect".as_bytes());
        let slice = container.get_slice_at(25, 7).unwrap();
        assert_eq!(slice, &[0, 0, 0, 0, 0, 0, 0]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_extent_next_index(4.into()).is_ok());
        assert_eq!(*block.next_extent().unwrap(), 4);

        test_ok_types(
            move |mut b| b.become_extent(1.into()),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        test_ok_types(
            move |b| b.next_extent(),
            &BTreeSet::from_iter(vec![BlockType::Extent, BlockType::StringReference]),
        );
        test_ok_types(
            move |mut b| b.set_extent_next_index(4.into()),
            &BTreeSet::from_iter(vec![BlockType::Extent, BlockType::StringReference]),
        );
        test_ok_types(
            move |mut b| b.extent_set_contents("test".as_bytes()),
            &BTreeSet::from_iter(vec![BlockType::Extent]),
        );
    }

    #[fuchsia::test]
    fn test_any_value() {
        let any_value = &BTreeSet::from_iter(vec![
            BlockType::DoubleValue,
            BlockType::IntValue,
            BlockType::UintValue,
            BlockType::NodeValue,
            BlockType::BufferValue,
            BlockType::ArrayValue,
            BlockType::LinkValue,
            BlockType::BoolValue,
        ]);
        test_ok_types(move |b| b.name_index(), any_value);
        test_ok_types(move |b| b.parent_index(), any_value);
    }

    #[fuchsia::test]
    fn test_double_value() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_double_value(1.0, 2.into(), 3.into()).is_ok());
        assert_eq!(block.block_type(), BlockType::DoubleValue);
        assert_eq!(*block.name_index().unwrap(), 2);
        assert_eq!(*block.parent_index().unwrap(), 3);
        assert_eq!(block.double_value().unwrap(), 1.0);
        assert_8_bytes!(container, 0, [0x01, 0x06, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_double_value(5.0).is_ok());
        assert_eq!(block.double_value().unwrap(), 5.0);
        assert_8_bytes!(container, 0, [0x01, 0x06, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0x40]);

        let types = BTreeSet::from_iter(vec![BlockType::DoubleValue]);
        test_ok_types(move |b| b.double_value(), &types);
        test_ok_types(move |mut b| b.set_double_value(3.0), &types);
        test_ok_types(
            move |mut b| b.become_double_value(1.0, 1.into(), 2.into()),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
    }

    #[fuchsia::test]
    fn test_int_value() {
        test_ok_types(
            move |mut b| b.become_int_value(1, 1.into(), 2.into()),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_int_value(1, 2.into(), 3.into()).is_ok());
        assert_eq!(block.block_type(), BlockType::IntValue);
        assert_eq!(*block.name_index().unwrap(), 2);
        assert_eq!(*block.parent_index().unwrap(), 3);
        assert_eq!(block.int_value().unwrap(), 1);
        assert_8_bytes!(container, 0, [0x1, 0x04, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_int_value(-5).is_ok());
        assert_eq!(block.int_value().unwrap(), -5);
        assert_8_bytes!(container, 0, [0x1, 0x04, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

        let types = BTreeSet::from_iter(vec![BlockType::IntValue]);
        test_ok_types(move |b| b.int_value(), &types);
        test_ok_types(move |mut b| b.set_int_value(3), &types);
    }

    #[fuchsia::test]
    fn test_uint_value() {
        test_ok_types(
            move |mut b| b.become_uint_value(1, 1.into(), 2.into()),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_uint_value(1, 2.into(), 3.into()).is_ok());
        assert_eq!(block.block_type(), BlockType::UintValue);
        assert_eq!(*block.name_index().unwrap(), 2);
        assert_eq!(*block.parent_index().unwrap(), 3);
        assert_eq!(block.uint_value().unwrap(), 1);
        assert_8_bytes!(container, 0, [0x01, 0x05, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_uint_value(5).is_ok());
        assert_eq!(block.uint_value().unwrap(), 5);
        assert_8_bytes!(container, 0, [0x01, 0x05, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        test_ok_types(move |b| b.uint_value(), &BTreeSet::from_iter(vec![BlockType::UintValue]));
        test_ok_types(
            move |mut b| b.set_uint_value(3),
            &BTreeSet::from_iter(vec![BlockType::UintValue]),
        );
    }

    #[fuchsia::test]
    fn test_bool_value() {
        test_ok_types(
            move |mut b| b.become_bool_value(true, 1.into(), 2.into()),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_bool_value(false, 2.into(), 3.into()).is_ok());
        assert_eq!(block.block_type(), BlockType::BoolValue);
        assert_eq!(*block.name_index().unwrap(), 2);
        assert_eq!(*block.parent_index().unwrap(), 3);
        assert!(!block.bool_value().unwrap());
        assert_8_bytes!(container, 0, [0x01, 0x0D, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_bool_value(true).is_ok());
        assert!(block.bool_value().unwrap());
        assert_8_bytes!(container, 0, [0x01, 0x0D, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        test_ok_types(move |b| b.bool_value(), &BTreeSet::from_iter(vec![BlockType::BoolValue]));
        test_ok_types(
            move |mut b| b.set_bool_value(true),
            &BTreeSet::from_iter(vec![BlockType::BoolValue]),
        );
    }

    #[fuchsia::test]
    fn test_become_node() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_node(2.into(), 3.into()).is_ok());
        assert_eq!(block.block_type(), BlockType::NodeValue);
        assert_eq!(*block.name_index().unwrap(), 2);
        assert_eq!(*block.parent_index().unwrap(), 3);
        assert_8_bytes!(container, 0, [0x01, 0x03, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        test_ok_types(
            move |mut b| b.become_node(1.into(), 2.into()),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
    }

    #[fuchsia::test]
    fn test_property() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_property(2.into(), 3.into(), PropertyFormat::Bytes).is_ok());
        assert_eq!(block.block_type(), BlockType::BufferValue);
        assert_eq!(*block.name_index().unwrap(), 2);
        assert_eq!(*block.parent_index().unwrap(), 3);
        assert_eq!(block.property_format().unwrap(), PropertyFormat::Bytes);
        assert_8_bytes!(container, 0, [0x01, 0x07, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10]);

        let (mut bad_container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut bad_format_bytes = [0u8; constants::MIN_ORDER_SIZE];
        bad_format_bytes[15] = 0x30;
        bad_container.copy_from_slice(&bad_format_bytes);
        let bad_block = Block::new(&bad_container, BlockIndex::EMPTY);
        assert!(bad_block.property_format().is_err()); // Make sure we get Error not panic

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_property_extent_index(4.into()).is_ok());
        assert_eq!(*block.property_extent_index().unwrap(), 4);
        assert_8_bytes!(container, 0, [0x01, 0x07, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x10]);

        let mut block = container.block_at_mut(BlockIndex::EMPTY);
        assert!(block.set_total_length(10).is_ok());
        assert_eq!(block.total_length().unwrap(), 10);
        assert_8_bytes!(container, 0, [0x01, 0x07, 0x03, 0x00, 0x00, 0x02, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x0a, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x10]);

        let ok_for_buffer_and_string_reference =
            BTreeSet::from_iter(vec![BlockType::BufferValue, BlockType::StringReference]);
        let ok_for_buffer = BTreeSet::from_iter(vec![BlockType::BufferValue]);
        test_ok_types(move |mut b| b.set_property_extent_index(4.into()), &ok_for_buffer);
        test_ok_types(move |mut b| b.set_total_length(4), &ok_for_buffer_and_string_reference);
        test_ok_types(move |b| b.property_extent_index(), &ok_for_buffer);
        test_ok_types(move |b| b.property_format(), &ok_for_buffer);
        test_ok_types(
            move |mut b| b.become_property(2.into(), 3.into(), PropertyFormat::Bytes),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
    }

    #[fuchsia::test]
    fn test_name() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_name("test-rust-inspect").is_ok());
        assert_eq!(block.block_type(), BlockType::Name);
        assert_eq!(block.name_length().unwrap(), 17);
        assert_eq!(block.name_contents().unwrap(), "test-rust-inspect");
        assert_8_bytes!(container, 0, [0x01, 0x09, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let slice = container.get_slice_at(8, 17).unwrap();
        assert_eq!(slice, "test-rust-inspect".as_bytes());
        let slice = container.get_slice_at(25, 7).unwrap();
        assert_eq!(slice, [0, 0, 0, 0, 0, 0, 0]);

        *container.get_value_mut::<u8>(24).unwrap() = 0xff;
        let bad_block = Block::new(&container, BlockIndex::EMPTY);
        assert_eq!(bad_block.name_length().unwrap(), 17); // Sanity check we copied correctly
        assert!(bad_block.name_contents().is_err()); // Make sure we get Error not panic

        let types = BTreeSet::from_iter(vec![BlockType::Name]);
        test_ok_types(move |b| b.name_length(), &types);
        test_ok_types(move |b| b.name_contents().map(|s| s.to_string()), &types);
        test_ok_types(
            move |mut b| b.become_name("test"),
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
        // Test to make sure UTF8 strings are truncated safely if they're too long for the block,
        // even if the last character is multibyte and would be chopped in the middle.
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_name("abcdefghijklmnopqrstuvwxyz").is_ok());
        assert_eq!(block.name_contents().unwrap(), "abcdefghijklmnopqrstuvwx");
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_name("😀abcdefghijklmnopqrstuvwxyz").is_ok());
        assert_eq!(block.name_contents().unwrap(), "😀abcdefghijklmnopqrst");
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 2).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block.become_name("abcdefghijklmnopqrstu😀").is_ok());
        assert_eq!(block.name_contents().unwrap(), "abcdefghijklmnopqrstu");
        let byte = container.get_value::<u8>(31).unwrap();
        assert_eq!(*byte, 0);
    }

    #[fuchsia::test]
    fn test_invalid_type_for_array() {
        let (mut container, _storage) = Container::read_and_write(2048).unwrap();
        container.get_slice_mut_at(24, 2048 - 24).unwrap().fill(14);
        let mut block = get_reserved_of_order(&mut container, 4);

        test_ok_types(
            |typed_block| {
                block.become_free(BlockIndex::EMPTY);
                block.become_reserved().unwrap();
                block.become_array_value(
                    4,
                    ArrayFormat::Default,
                    typed_block.block_type(),
                    BlockIndex::EMPTY,
                    BlockIndex::EMPTY,
                )
            },
            &BTreeSet::from_iter(vec![
                BlockType::StringReference,
                BlockType::IntValue,
                BlockType::UintValue,
                BlockType::DoubleValue,
            ]),
        );

        test_ok_types(
            |typed_block| {
                block.become_free(BlockIndex::EMPTY);
                block.become_reserved().unwrap();
                block.become_array_value(
                    4,
                    ArrayFormat::LinearHistogram,
                    typed_block.block_type(),
                    BlockIndex::EMPTY,
                    BlockIndex::EMPTY,
                )?;

                block.become_free(BlockIndex::EMPTY);
                block.become_reserved().unwrap();
                block.become_array_value(
                    4,
                    ArrayFormat::ExponentialHistogram,
                    typed_block.block_type(),
                    BlockIndex::EMPTY,
                    BlockIndex::EMPTY,
                )
            },
            &BTreeSet::from_iter(vec![
                BlockType::IntValue,
                BlockType::UintValue,
                BlockType::DoubleValue,
            ]),
        );
    }

    // In this test, we actually don't care about generating string reference
    // blocks at all. That is tested elsewhere. All we care about is that the indexes
    // put in a certain slot come out of the slot correctly. Indexes are u32, so
    // for simplicity this test just uses meaningless numbers instead of creating actual
    // indexable string reference values.
    #[fuchsia::test]
    fn test_string_arrays() {
        let (mut container, _storage) = Container::read_and_write(2048).unwrap();
        container.get_slice_mut_at(48, 2048 - 48).unwrap().fill(14);

        let mut block = get_reserved(&mut container);
        let parent_index = BlockIndex::new(0);
        let name_index = BlockIndex::new(1);
        assert!(block
            .become_array_value(
                4,
                ArrayFormat::Default,
                BlockType::StringReference,
                name_index,
                parent_index
            )
            .is_ok());

        for i in 0..4 {
            assert!(block.array_set_string_slot(i, ((i + 4) as u32).into()).is_ok())
        }

        for i in 0..4 {
            let read_index = block.array_get_string_index_slot(i).unwrap();
            assert_eq!(*read_index, (i + 4) as u32);
        }

        assert_8_bytes!(
            container,
            0,
            [
                0x01, 0x0b, 0x00, /* parent */
                0x00, 0x00, 0x01, /* name_index */
                0x00, 0x00
            ]
        );
        assert_8_bytes!(container, 8, [0x0E, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        for i in 0..4 {
            let slice = container.get_slice_at(16 + (i * 4), 4).unwrap();
            assert_eq!(slice, [(i as u8 + 4), 0x00, 0x00, 0x00]);
        }
    }

    #[fuchsia::test]
    fn become_array() {
        // primarily these tests are making sure that arrays clear their payload space

        let (mut container, _storage) = Container::read_and_write(128).unwrap();
        container.get_slice_mut_at(16, 128 - 16).unwrap().fill(1);

        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 7, BlockIndex::EMPTY).unwrap();
        block.become_reserved().unwrap();
        block
            .become_array_value(
                14,
                ArrayFormat::Default,
                BlockType::IntValue,
                BlockIndex::EMPTY,
                BlockIndex::EMPTY,
            )
            .unwrap();
        let slice = container.get_slice_at(16, 128 - 16).unwrap();
        slice
            .iter()
            .enumerate()
            .for_each(|(index, i)| assert_eq!(*i, 0, "failed: byte = {} at index {}", *i, index));

        container.get_slice_mut_at(16, 128 - 16).unwrap().fill(1);
        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 7, BlockIndex::EMPTY).unwrap();
        block.become_reserved().unwrap();
        block
            .become_array_value(
                14,
                ArrayFormat::LinearHistogram,
                BlockType::IntValue,
                BlockIndex::EMPTY,
                BlockIndex::EMPTY,
            )
            .unwrap();
        let slice = container.get_slice_at(16, 128 - 16).unwrap();
        slice
            .iter()
            .enumerate()
            .for_each(|(index, i)| assert_eq!(*i, 0, "failed: byte = {} at index {}", *i, index));

        container.get_slice_mut_at(16, 128 - 16).unwrap().fill(1);
        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 7, BlockIndex::EMPTY).unwrap();
        block.become_reserved().unwrap();
        block
            .become_array_value(
                14,
                ArrayFormat::ExponentialHistogram,
                BlockType::IntValue,
                BlockIndex::EMPTY,
                BlockIndex::EMPTY,
            )
            .unwrap();
        let slice = container.get_slice_at(16, 128 - 16).unwrap();
        slice
            .iter()
            .enumerate()
            .for_each(|(index, i)| assert_eq!(*i, 0, "failed: byte = {} at index {}", *i, index));

        container.get_slice_mut_at(16, 128 - 16).unwrap().fill(1);
        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 7, BlockIndex::EMPTY).unwrap();
        block.become_reserved().unwrap();
        block
            .become_array_value(
                28,
                ArrayFormat::Default,
                BlockType::StringReference,
                BlockIndex::EMPTY,
                BlockIndex::EMPTY,
            )
            .unwrap();
        let slice = container.get_slice_at(16, 128 - 16).unwrap();
        slice
            .iter()
            .enumerate()
            .for_each(|(index, i)| assert_eq!(*i, 0, "failed: byte = {} at index {}", *i, index));
    }

    #[fuchsia::test]
    fn uint_array_value() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 4).unwrap();
        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 2, BlockIndex::EMPTY).unwrap();
        assert!(block.become_reserved().is_ok());
        assert!(block
            .become_array_value(
                4,
                ArrayFormat::LinearHistogram,
                BlockType::UintValue,
                3.into(),
                2.into(),
            )
            .is_ok());

        assert_eq!(block.block_type(), BlockType::ArrayValue);
        assert_eq!(*block.parent_index().unwrap(), 2);
        assert_eq!(*block.name_index().unwrap(), 3);
        assert_eq!(block.array_format().unwrap(), ArrayFormat::LinearHistogram);
        assert_eq!(block.array_slots().unwrap(), 4);
        assert_eq!(block.array_entry_type().unwrap(), BlockType::UintValue);

        for i in 0..4 {
            assert!(block.array_set_uint_slot(i, (i as u64 + 1) * 5).is_ok());
        }
        assert!(block.array_set_uint_slot(4, 3).is_err());
        assert!(block.array_set_uint_slot(7, 5).is_err());

        assert_8_bytes!(container, 0, [0x02, 0x0b, 0x02, 0x00, 0x00, 0x03, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x15, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        for i in 0..4 {
            assert_8_bytes!(
                container,
                8 * (i + 2),
                [(i as u8 + 1) * 5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
            );
        }

        let (mut bad_container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut bad_bytes = [0u8; constants::MIN_ORDER_SIZE];
        container.copy_bytes(&mut bad_bytes[..]);
        bad_bytes[8] = 0x12; // LinearHistogram; Header
        bad_container.copy_from_slice(&bad_bytes);
        let bad_block = Block::new(&bad_container, BlockIndex::EMPTY);
        assert_eq!(bad_block.array_format().unwrap(), ArrayFormat::LinearHistogram);
        // Make sure we get Error not panic or BlockType::Header
        assert!(bad_block.array_entry_type().is_err());

        bad_bytes[8] = 0xef; // Not in enum; Not in enum
        bad_container.copy_from_slice(&bad_bytes);
        let bad_block = Block::new(&bad_container, BlockIndex::EMPTY);
        assert!(bad_block.array_format().is_err());
        assert!(bad_block.array_entry_type().is_err());

        let block = container.block_at(BlockIndex::EMPTY);
        for i in 0..4 {
            assert_eq!(block.array_get_uint_slot(i).unwrap(), (i as u64 + 1) * 5);
        }
        assert!(block.array_get_uint_slot(4).is_err());

        let types = BTreeSet::from_iter(vec![BlockType::ArrayValue]);
        test_ok_types(move |b| b.array_format(), &types);
        test_ok_types(move |b| b.array_slots(), &types);
        test_ok_types(
            move |mut b| {
                b.become_array_value(
                    2,
                    ArrayFormat::Default,
                    BlockType::UintValue,
                    1.into(),
                    2.into(),
                )?;
                b.array_set_uint_slot(0, 3)?;
                b.array_get_uint_slot(0)
            },
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
    }

    #[fuchsia::test]
    fn array_slots_bigger_than_block_order() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MAX_ORDER_SIZE).unwrap();
        // A block of size 7 (max) can hold 254 values: 2048B - 8B (header) - 8B (array metadata)
        // gives 2032, which means 254 values of 8 bytes each maximum.
        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 7, BlockIndex::EMPTY).unwrap();
        block.become_reserved().unwrap();
        assert!(block
            .become_array_value(257, ArrayFormat::Default, BlockType::IntValue, 1.into(), 2.into())
            .is_err());
        assert!(block
            .become_array_value(254, ArrayFormat::Default, BlockType::IntValue, 1.into(), 2.into())
            .is_ok(),);

        // A block of size 2 can hold 6 values: 64B - 8B (header) - 8B (array metadata)
        // gives 48, which means 6 values of 8 bytes each maximum.
        let mut block =
            Block::new_free(&mut container, BlockIndex::EMPTY, 2, BlockIndex::EMPTY).unwrap();
        block.become_reserved().unwrap();
        assert!(block
            .become_array_value(8, ArrayFormat::Default, BlockType::IntValue, 1.into(), 2.into())
            .is_err());
        assert!(block
            .become_array_value(6, ArrayFormat::Default, BlockType::IntValue, 1.into(), 2.into())
            .is_ok());
    }

    #[fuchsia::test]
    fn array_clear() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE * 4).unwrap();

        // Write some sample data in the container after the slot fields.
        let sample = [0xff, 0xff, 0xff];
        container.copy_from_slice_at(48, &sample);

        let mut block = Block::new_free(&mut container, BlockIndex::EMPTY, 2, BlockIndex::EMPTY)
            .expect("new free");
        assert!(block.become_reserved().is_ok());
        assert!(block
            .become_array_value(
                4,
                ArrayFormat::LinearHistogram,
                BlockType::UintValue,
                3.into(),
                2.into()
            )
            .is_ok());

        for i in 0..4 {
            block.array_set_uint_slot(i, (i + 1) as u64).expect("set uint");
        }

        block.array_clear(1).expect("clear array");

        assert_eq!(1, block.array_get_uint_slot(0).expect("get uint 0"));
        assert_8_bytes!(container, 16, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        for i in 1..4 {
            assert_eq!(
                0,
                container.block_at(BlockIndex::EMPTY).array_get_uint_slot(i).expect("get uint")
            );
            assert_8_bytes!(
                container,
                16 + (i * 8),
                [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
            );
        }

        // Sample data shouldn't have been overwritten
        let slice = container.get_slice_at(48, 3).unwrap();
        assert_eq!(slice, &sample[..]);
    }

    #[fuchsia::test]
    fn become_link() {
        let (mut container, _storage) =
            Container::read_and_write(constants::MIN_ORDER_SIZE).unwrap();
        let mut block = get_reserved(&mut container);
        assert!(block
            .become_link(
                BlockIndex::new(1),
                BlockIndex::new(2),
                BlockIndex::new(3),
                LinkNodeDisposition::Inline
            )
            .is_ok());
        assert_eq!(*block.name_index().unwrap(), 1);
        assert_eq!(*block.parent_index().unwrap(), 2);
        assert_eq!(*block.link_content_index().unwrap(), 3);
        assert_eq!(block.block_type(), BlockType::LinkValue);
        assert_eq!(block.link_node_disposition().unwrap(), LinkNodeDisposition::Inline);
        assert_8_bytes!(container, 0, [0x01, 0x0c, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00]);
        assert_8_bytes!(container, 8, [0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10]);

        let types = BTreeSet::from_iter(vec![BlockType::LinkValue]);
        test_ok_types(move |b| b.link_content_index(), &types);
        test_ok_types(move |b| b.link_node_disposition(), &types);
        test_ok_types(
            move |mut b| {
                b.become_link(
                    BlockIndex::new(1),
                    BlockIndex::new(2),
                    BlockIndex::new(3),
                    LinkNodeDisposition::Inline,
                )
            },
            &BTreeSet::from_iter(vec![BlockType::Reserved]),
        );
    }

    fn get_header(container: &mut Container, size: usize) -> Block<&mut Container> {
        let mut block = get_reserved(container);
        assert!(block.become_header(size).is_ok());
        block
    }

    fn get_reserved(container: &mut Container) -> Block<&mut Container> {
        let mut block =
            Block::new_free(container, BlockIndex::EMPTY, 1, BlockIndex::new(0)).unwrap();
        assert!(block.become_reserved().is_ok());
        block
    }

    fn get_reserved_of_order(container: &mut Container, order: u8) -> Block<&mut Container> {
        let mut block =
            Block::new_free(container, BlockIndex::EMPTY, order, BlockIndex::new(0)).unwrap();
        assert!(block.become_reserved().is_ok());
        block
    }
}
