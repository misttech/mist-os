// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::RingBuffer;
use byteorder::{LittleEndian, WriteBytesExt};
use std::{io, iter};

const SELECTORS_PER_BYTE: usize = 2;
const BITS_PER_BYTE: usize = 8;
const BITS_PER_SELECTOR: usize = BITS_PER_BYTE / SELECTORS_PER_BYTE;

/// This is a ring buffer that packs 4-bit values into a u8's; primarily intended
/// to hold a ring buffer of selectors.
#[derive(Clone, Debug)]
struct PackedU4RingBuffer {
    u8_buffer: Vec<u8>,
    // head_index, len, and capacity are based on the number of selectors, not the number
    // of bytes
    head_index: usize,
    len: usize,
    capacity: usize,
}

impl PackedU4RingBuffer {
    const fn new(capacity: usize) -> Self {
        Self { u8_buffer: vec![], head_index: 0, len: 0, capacity }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn push(&mut self, selector: u8) {
        let selector = selector & 0x0f;
        if self.len / SELECTORS_PER_BYTE == self.u8_buffer.len() && self.len < self.capacity {
            self.u8_buffer.push(selector);
            self.len += 1;
            return;
        }

        let index = (self.head_index + self.len) % self.capacity;
        let u8_index = index / SELECTORS_PER_BYTE;
        self.u8_buffer[u8_index] = if index % SELECTORS_PER_BYTE == 0 {
            (self.u8_buffer[u8_index] & 0xf0) | selector
        } else {
            (selector << BITS_PER_SELECTOR) | (self.u8_buffer[u8_index] & 0x0f)
        };
        if self.len < self.capacity {
            self.len += 1;
        } else {
            self.head_index += 1;
        }
    }

    fn pop(&mut self) -> Option<u8> {
        let val = self.back();
        if val.is_some() {
            self.len -= 1;
        }
        val
    }

    fn back(&self) -> Option<u8> {
        if self.len == 0 {
            None
        } else {
            let index = (self.head_index + self.len - 1) % self.capacity();
            let byte = self.u8_buffer[index / SELECTORS_PER_BYTE];
            let selector = if index % SELECTORS_PER_BYTE == 0 {
                byte & 0x0f
            } else {
                (byte & 0xf0) >> BITS_PER_SELECTOR
            };
            Some(selector)
        }
    }

    fn buffer(&self) -> &[u8] {
        &self.u8_buffer[..]
    }
}

const U64_SELECTOR: u8 = 14;
const RLE_SELECTOR: u8 = 15;
const RLE_DATA_NUM_BITS: u64 = 48;
const RLE_DATA_BITMASK: u64 = 0xffffffffffff;
const RLE_LEN_BITMASK: u64 = 0xffff000000000000;
const RLE_LEN_MAX: u64 = RLE_LEN_BITMASK >> RLE_DATA_NUM_BITS;

const SIMPLE8B_SELECTOR_BIT_COUNTS: [u32; 15] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 16, 21, 32, 64];

const _: () = {
    assert!(
        (1 << RLE_DATA_NUM_BITS) - 1 == RLE_DATA_BITMASK,
        "RLE_DATA_BITMASK is incongruent with RLE_DATA_NUM_BITS",
    );
    assert!(
        RLE_LEN_BITMASK ^ RLE_DATA_BITMASK == u64::MAX,
        "RLE_LEN_BITMASK is incongruent with RLE_DATA_BITMASK",
    );
};

#[derive(Clone, Copy, Debug)]
struct Simple8bRleBlock {
    pub selector: u8,
    pub data: u64,
}

/// Simple8bRleRingBuffer is a ringbuffer that uses a modified combination of simple8b
/// and run-length encoding (RLE) to encode its data. Simple8bRleRingBuffer is only
/// intended to accept unsigned integers.
///
/// The Simple8bRleRingBuffer holds a ring buffer of selectors and a ring buffer of u64 value
/// blocks. A selector describes what encoding is used, and a corresponding value block holds
/// the values encoded with that encoding.
///
/// The following table describes the meaning of each selector:
/// ```
/// Selector value:  0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 | 15 (RLE)
/// Integers coded: 64 32 21 16 12 10  9  8  7  6  5  4  3  2  1 | up to 2^16
/// Bits/integer:    1  2  3  4  5  6  7  8  9 10 12 16 21 32 64 | 48 bits
/// Wasted bits:     0  0  1  0  4  4  1  0  1  4  4  0  1  0  0 |   N/A
/// ```
///
/// Selector 15 uses RLE. The smallest bits are used to encode the actual value.
/// The largest bits are used to encode the count.
///
/// Selectors 0-14 use simple8b bit-packing scheme. For example, selector 0 means
/// that 1 bit is used to encode each value and that 64 integers are encoded into
/// the value block.
///
/// Note that although the simple8b selector describes how many integers are encoded
/// into the value block, the most recent block might be simple8b-encoded but does
/// not contain the sufficient number of integers. In that case, the number of integers
/// encoded is tracked by the `current_block_num_values` field.
#[derive(Clone, Debug)]
pub struct Simple8bRleRingBuffer {
    selectors: PackedU4RingBuffer,
    value_blocks: RingBuffer<u64>,
    current_block_num_values: u32,
}

impl Simple8bRleRingBuffer {
    /// Create a new Simple8bRleRingBuffer with the given |capacity|.
    /// |capacity| is rounded up to the nearest number that's divisible by 2,
    /// unless it's `usize::MAX`, in which case it's rounded down.
    /// Because selectors are only 4 bits, we need even capacity for selectors to
    /// take up a full byte.
    pub const fn with_nearest_capacity(mut capacity: usize) -> Self {
        if capacity == usize::MAX {
            capacity = capacity - 1;
        } else if capacity % 2 != 0 {
            capacity = capacity + 1;
        }

        Self {
            selectors: PackedU4RingBuffer::new(capacity),
            value_blocks: RingBuffer::new(capacity),
            current_block_num_values: 0,
        }
    }

    /// Serialize the Simple8bRleRingBuffer data into a bytes buffer.
    pub fn serialize(&self, buffer: &mut impl io::Write) -> io::Result<()> {
        buffer.write_u16::<LittleEndian>(self.selectors.len() as u16)?;
        buffer.write_u16::<LittleEndian>(self.selectors.head_index as u16)?;
        buffer.write_u8(self.current_block_num_values as u8)?;

        buffer.write_all(self.selectors.buffer())?;
        for value_block in &self.value_blocks.buffer {
            buffer.write_u64::<LittleEndian>(*value_block)?;
        }
        Ok(())
    }

    /// Push a new value onto the Simple8bRleRingBuffer.
    ///
    /// This method will first attempt to encode the value into the existing block.
    /// If that's not possible, new blocks would be created, and in the process, the
    /// oldest blocks might be evicted if the ring buffer runs out of space.
    ///
    /// Note that because a block can hold an arbitrary amount of data points, and
    /// multiple blocks might be evicted, an arbitrary number of data points might
    /// be evicted due to this call.
    pub fn push(&mut self, value: u64) {
        let current_block = match self.back() {
            Some(block) => block,
            _ => {
                self.push_value_onto_new_block(value);
                return;
            }
        };

        // Check whether we can encode the new value into the existing block
        if current_block.selector == RLE_SELECTOR {
            let current_value = current_block.data & RLE_DATA_BITMASK;
            if value == current_value {
                // If the new value is the same as the existing value of an RLE block,
                // and we have not exceeeded the length limit, then we can encode the
                // new value into it.
                if (self.current_block_num_values as u64) < RLE_LEN_MAX {
                    self.current_block_num_values += 1;
                    let block = rle_block_from_value(value, self.current_block_num_values);
                    self.replace_back(block);
                    return;
                }
            } else {
                // If the new value is different from the existing value of an RLE block,
                // check whether the new value would still fit if the existing block were
                // re-encoded as a simple-8b block. If yes, do the re-encode that includes
                // the new value.
                let bits_needed =
                    std::cmp::max(repr_bits_needed(value), repr_bits_needed(current_value));
                let new_selector = choose_simple8b_selector(bits_needed);
                let new_selector_bit_len = SIMPLE8B_SELECTOR_BIT_COUNTS[new_selector as usize];
                if new_selector_bit_len * (self.current_block_num_values + 1) <= u64::BITS {
                    let mut values = iter::repeat(current_value)
                        .take(self.current_block_num_values as usize)
                        .chain(iter::once(value));
                    let new_block = simple8b_block_from_values(new_selector, &mut values);
                    self.current_block_num_values += 1;
                    self.replace_back(new_block);
                    return;
                }
            }
        } else {
            let bits_needed = repr_bits_needed(value);
            let current_selector_bit_len =
                SIMPLE8B_SELECTOR_BIT_COUNTS[current_block.selector as usize];
            if bits_needed <= current_selector_bit_len {
                // If the existing block is simple-8b whose selector can accept the new value,
                // and if the new value fits into the existing block, then encode the new value
                // into the block.
                if current_selector_bit_len * (self.current_block_num_values + 1) <= u64::BITS {
                    let mut current_block = current_block;
                    simple8b_block_set_value(
                        &mut current_block,
                        value,
                        self.current_block_num_values as usize,
                    );
                    self.current_block_num_values += 1;
                    self.replace_back(current_block);
                    return;
                }
            } else {
                // If the new value's bit length is larger than what the existing block's
                // simple8b selector can accept, then check whether re-encoding the existing
                // block with a new selector would work. Additionally, check whether the
                // new value would fit into the re-encoded block. If yes, proceed with the
                // re-encoding with the new value included.
                let new_selector = choose_simple8b_selector(bits_needed);
                let new_selector_bit_len = SIMPLE8B_SELECTOR_BIT_COUNTS[new_selector as usize];
                if new_selector_bit_len * (self.current_block_num_values + 1) <= u64::BITS {
                    let mut values =
                        Simple8bIter::new(current_block, self.current_block_num_values as usize)
                            .chain(iter::once(value));
                    let new_block = simple8b_block_from_values(new_selector, &mut values);
                    self.current_block_num_values += 1;
                    self.replace_back(new_block);
                    return;
                }
            }

            // Trying to fit the new value into the existing block doesn't work, so we need to
            // place it in a new block. Before we do so, check if the existing block is incomplete.
            // Decoders won't have enough information to decode an incomplete block, so we need
            // to turn the existing block into a complete block first. In the process, any excess
            // values and the new value would go on new blocks.
            //
            // An existing block is considered incomplete when it's a simple8b-encoded block
            // that doesn't hold the exact number of values specified by its selector.
            if self.current_block_num_values
                < u64::BITS / SIMPLE8B_SELECTOR_BIT_COUNTS[current_block.selector as usize]
            {
                let mut try_rle_values =
                    Simple8bIter::new(current_block, self.current_block_num_values as usize);
                let mut try_simple8b_values = try_rle_values.clone();

                // Re-encode as many values from last block as possible, trying both RLE and
                // simple8b.
                let new_rle_block = try_rle_values.next_rle_block();
                let new_simple8b_block = try_simple8b_values.next_simple8b_block();
                let remaining_values = if try_rle_values.index() > try_simple8b_values.index() {
                    self.replace_back(new_rle_block);
                    try_rle_values
                } else {
                    self.replace_back(new_simple8b_block);
                    try_simple8b_values
                };

                // Put any excess value, along with new value into a new block
                // Note that it's possible not all excess values can be fully encoded in the
                // new block, in which case another block may be created in the recursion.
                let mut remaining_values = remaining_values.chain(iter::once(value));
                // Safe to unwrap because we just pushed one value in `remaining_values`
                self.push_value_onto_new_block(remaining_values.next().unwrap());
                while let Some(v) = remaining_values.next() {
                    self.push(v);
                }
                return;
            }
        }

        // Fall off case: we cannot fit the new value into the existing block, and the existing
        // block is already complete, so we just need to put the new value into a new block.
        self.push_value_onto_new_block(value);
    }

    fn back(&self) -> Option<Simple8bRleBlock> {
        self.selectors
            .back()
            .zip(self.value_blocks.back())
            .map(|(selector, data)| Simple8bRleBlock { selector, data: *data })
    }

    fn push_value_onto_new_block(&mut self, value: u64) {
        self.current_block_num_values = 1;
        let bits_needed = repr_bits_needed(value);
        let block = if bits_needed as u64 <= RLE_DATA_NUM_BITS {
            rle_block_from_value(value, self.current_block_num_values)
        } else {
            Simple8bRleBlock { selector: U64_SELECTOR, data: value }
        };
        self.push_block(block);
    }

    fn push_block(&mut self, block: Simple8bRleBlock) {
        self.selectors.push(block.selector);
        self.value_blocks.push(block.data);
    }

    fn replace_back(&mut self, block: Simple8bRleBlock) {
        if self.back().is_none() {
            return;
        }
        self.selectors.pop();
        self.value_blocks.pop();
        self.push_block(block);
    }
}

/// Choose a simple8b selector that can encode the most values with the needed required of bits
/// per value
fn choose_simple8b_selector(bits_needed: u32) -> u8 {
    for (selector, bit_len) in SIMPLE8B_SELECTOR_BIT_COUNTS.iter().enumerate() {
        if bits_needed <= *bit_len {
            return selector as u8;
        }
    }
    U64_SELECTOR
}

/// Choose a simple8b selector that encode values with the required number of bits per value.
/// At the same time, the selector should not be able to encode more than `num_items`.
/// If there are multiple selectors that fit these criteria, pick the selector that can fit
/// the most number of values.
fn choose_simple8b_selector_with_max_items(bits_needed: u32, num_items: u32) -> u8 {
    for (selector, bit_len) in SIMPLE8B_SELECTOR_BIT_COUNTS.iter().enumerate() {
        if bits_needed > *bit_len {
            continue;
        }
        if u64::BITS / bit_len <= num_items {
            return selector as u8;
        }
    }
    U64_SELECTOR
}

/// Return the number of bits needed to represent this value.
fn repr_bits_needed(value: u64) -> u32 {
    u64::BITS - value.leading_zeros()
}

fn rle_block_from_value(value: u64, len: u32) -> Simple8bRleBlock {
    Simple8bRleBlock {
        selector: RLE_SELECTOR,
        data: (value & RLE_DATA_BITMASK) | ((len as u64) << RLE_DATA_NUM_BITS),
    }
}

/// Encode a simple8b block given the selector and a set of values.
/// This function will either encode all the values in the set or as many values that can
/// fit in a u64 block.
///
/// As the result of this function, the |values| iterator will advance past the values that
/// have been encoded.
fn simple8b_block_from_values(
    selector: u8,
    values: &mut impl Iterator<Item = u64>,
) -> Simple8bRleBlock {
    let mut block = Simple8bRleBlock { selector, data: 0 };
    let num_values: usize = (u64::BITS / SIMPLE8B_SELECTOR_BIT_COUNTS[selector as usize]) as usize;
    for (i, value) in values.enumerate() {
        simple8b_block_set_value(&mut block, value, i);
        if i == num_values - 1 {
            break;
        }
    }
    block
}

fn simple8b_block_set_value(block: &mut Simple8bRleBlock, value: u64, index: usize) {
    let selector_bit_len = SIMPLE8B_SELECTOR_BIT_COUNTS[block.selector as usize];
    block.data |= value << (index as u64 * selector_bit_len as u64)
}

#[derive(Clone)]
struct Simple8bIter {
    selector: u8,
    value_block: u64,
    index: usize,
    num_items: usize,
}

impl Simple8bIter {
    fn new(block: Simple8bRleBlock, num_items: usize) -> Self {
        Self { selector: block.selector, value_block: block.data, index: 0, num_items }
    }

    fn index(&self) -> usize {
        self.index
    }

    fn peek(&mut self) -> Option<<Self as Iterator>::Item> {
        if self.index >= self.num_items {
            return None;
        }
        let selector_bit_len = SIMPLE8B_SELECTOR_BIT_COUNTS[self.selector as usize];
        let item = (self.value_block >> (self.index as u64 * selector_bit_len as u64))
            & ((1 << selector_bit_len) - 1);
        Some(item)
    }

    /// Encode with RLE as many values as possible from this iterator.
    /// Advance the iterator past the encoded set of values, and return the
    /// encoded block.
    fn next_rle_block(&mut self) -> Simple8bRleBlock {
        let first_value = self.peek().unwrap_or(0);
        let mut num_rle_values = 0;
        while let Some(value) = self.peek() {
            if value == first_value {
                num_rle_values += 1;
                let _ = self.next();
            } else {
                break;
            }
        }
        rle_block_from_value(first_value, num_rle_values)
    }

    /// Encode with simple8b as many values as possible from this iterator,
    /// picking a selector that would create a complete simple8b-encoded block.
    /// Advance the iterator past the encoded set of values, and return a
    /// `(selector, complete_simpl8b_encoded_block)` tuple.
    fn next_simple8b_block(&mut self) -> Simple8bRleBlock {
        let bit_len = SIMPLE8B_SELECTOR_BIT_COUNTS[self.selector as usize];
        let new_simple8b_selector =
            choose_simple8b_selector_with_max_items(bit_len, self.num_items as u32);
        simple8b_block_from_values(new_simple8b_selector, self)
    }
}

impl Iterator for Simple8bIter {
    type Item = u64;
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.peek();
        if item.is_some() {
            self.index += 1;
        }
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(1; "odd capacity")]
    #[test_case(2; "even capacity")]
    fn test_ring_buffer_rotates_out_old_values(capacity: usize) {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(capacity);
        ring_buffer.push(u64::MAX);
        ring_buffer.push(u64::MAX - 1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0xee, // 64-bit selector
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // first block
            0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // second block
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(1);
        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            2, 0, // length
            1, 0,    // selector head index
            1,    // last block's # of values
            0xef, // first block: 64-bit selector, second block: RLE selector
            // Note that because selector head index is 1, the first block selector is at
            // bits 4-7
            0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // first block
            1, 0, 0, 0, 0, 0, // second block: value
            1, 0, // second block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_new_block_rle() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);
        ring_buffer.push((1 << 48) - 1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0x0f, // RLE selector
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // first block: value
            1, 0, // first block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_new_block_simple8b() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);
        ring_buffer.push(1 << 48);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0x0e, // 64-bit selector
            0, 0, 0, 0, 0, 0, 1, 0, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_rle() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);
        for _i in 0..258 {
            ring_buffer.push(1);
        }

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            0x02, // last block's # of values -- this is incorrect, but it doesn't matter because
            //                             the RLE block itself has the correct value
            0x0f, // RLE selector
            1, 0, 0, 0, 0, 0, // first block: value
            0x02, 0x01, // first block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_simple8b() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);
        ring_buffer.push(0x0e);
        ring_buffer.push(1);
        ring_buffer.push(0);
        ring_buffer.push(0);
        ring_buffer.push(5);
        ring_buffer.push(6);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            6,    // last block's # of values
            0x03, // 4-bit selector
            0x1e, 0, 0x65, 0, 0, 0, 0, 0, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_reencode_rle_to_simple8b() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);
        for _i in 0..4 {
            ring_buffer.push(1);
        }

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            4,    // last block's # of values
            0x0f, // RLE selector
            1, 0, 0, 0, 0, 0, // first block: value
            4, 0, // first block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push((1 << 12) - 1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            5,    // last block's # of values
            0x0a, // 12-bit selector
            0x01, 0x10, 0x00, 0x01, 0x10, 0x00, 0xff, 0x0f, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_reencode_simple8b_to_simple8b() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);

        ring_buffer.push(0x0e);
        ring_buffer.push(1);
        ring_buffer.push(2);
        ring_buffer.push(3);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            4,    // last block's # of values
            0x03, // 4-bit selector
            0x1e, 0x32, 0, 0, 0, 0, 0, 0, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push((1 << 12) - 1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            5,    // last block's # of values
            0x0a, // 12-bit selector
            0x0e, 0x10, 0x00, 0x02, 0x30, 0x00, 0xff, 0x0f, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_new_block_because_rle_block_max_len() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);

        ring_buffer.push(1);
        ring_buffer.current_block_num_values = 0xfffe;
        ring_buffer.push(1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            0xff, // last block's # of values -- this is incorrect, but it doesn't matter because
            //                             the RLE block itself has the correct value
            0x0f, // RLE selector
            1, 0, 0, 0, 0, 0, // first block: value
            0xff, 0xff, // first block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0xff, // first block: RLE selector, second block: RLE selector
            1, 0, 0, 0, 0, 0, // first block: value
            0xff, 0xff, // first block: length
            1, 0, 0, 0, 0, 0, // second block: value
            1, 0, // second block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_new_block_because_not_fit_into_current_simple8b_block() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);

        ring_buffer.push((1 << 32) - 1);
        ring_buffer.push(2);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            2,    // last block's # of values
            0x0d, // 32-bit selector
            0xff, 0xff, 0xff, 0xff, 0x02, 0x00, 0x00, 0x00, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(3);
        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0xfd, // first block: 32-bit selector, second block: RLE
            0xff, 0xff, 0xff, 0xff, 0x02, 0x00, 0x00, 0x00, // first block
            3, 0, 0, 0, 0, 0, // second block: value
            1, 0, // second block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_new_block_because_no_space_to_reencode_rle_to_simple8b() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);
        for _i in 0..4 {
            ring_buffer.push(1);
        }

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            4,    // last block's # of values
            0x0f, // RLE selector
            1, 0, 0, 0, 0, 0, // first block: value
            4, 0, // first block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(1 << 12);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0xff, // first block: RLE selector, second block: RLE selector
            1, 0, 0, 0, 0, 0, // first block: value
            4, 0, // first block: length
            0x00, 0x10, 0, 0, 0, 0, // second block: value
            1, 0, // second block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_encode_new_block_because_no_space_to_reencode_simple8b_to_simple8b() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(2);

        ring_buffer.push(0x0e);
        ring_buffer.push(1);
        ring_buffer.push(2);
        ring_buffer.push(3);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            4,    // last block's # of values
            0x03, // 4-bit selector
            0x1e, 0x32, 0, 0, 0, 0, 0, 0, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(1 << 12);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        // First block was re-encoded with selector 11 (16-bit) to fit exactly four values.
        // Then new value is added to second block.
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0xfb, // first block: 4-bit selector, second block: RLE selector
            0x0e, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, // first block
            0x00, 0x10, 0, 0, 0, 0, // second block: value
            1, 0, // second block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    // Sometimes when a new value is pushed to the ring buffer, the existing partially
    // encoded simple-8b block has to be re-encoded into multiple ones
    //
    // This case tests the scenario where one partially encoded block and one new value
    // leads to 3 fully encoded blocks and 1 newly encoded block
    #[test]
    fn test_chain_reencode_four_block_scenario() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(4);

        // Push 63 one-bit values
        ring_buffer.push(0);
        for _i in 0..31 {
            ring_buffer.push(1);
            ring_buffer.push(0);
        }

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            63,   // last block's # of values
            0x00, // 1-bit selector
            0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0x2a, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);
        assert_eq!(ring_buffer.current_block_num_values, 63);

        ring_buffer.push((1 << 6) - 1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        // The 63 one-bit values are then split into:
        // - 32 two-bit values
        // - 21 three-bit values
        // - 10 six-bit values
        let expected_bytes = &[
            4, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0x21, // first block: 2-bit selector, second block: 3-bit selector
            0xf5, // third block: 6-bit selector, fourth block: RLE selector
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, // first block
            0x08, 0x82, 0x20, 0x08, 0x82, 0x20, 0x08, 0x02, // second block
            0x01, 0x10, 0x00, 0x01, 0x10, 0x00, 0x01, 0x00, // third block
            0x3f, 0, 0, 0, 0, 0, // fourth block: value
            1, 0, // fourth block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_chain_reencode_new_value_goes_with_excess_value() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(4);

        // Push 63 one-bit values
        ring_buffer.push(0);
        for _i in 0..31 {
            ring_buffer.push(1);
            ring_buffer.push(0);
        }

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            63,   // last block's # of values
            0x00, // 1-bit selector
            0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0x2a, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(3);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        // The 63 one-bit values + the new 2-bit value are then split into:
        // - 32 two-bit values
        // - 32 two-bit values
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            32,   // last block's # of values
            0x11, // first block: 2-bit selector, second block: 2-bit selector
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, // first block
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0xc4, // second block
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_chain_reencode_rle_wins() {
        let mut ring_buffer = Simple8bRleRingBuffer::with_nearest_capacity(4);

        // Push 63 one-bit values
        for _i in 0..62 {
            ring_buffer.push(1);
        }
        ring_buffer.push(0);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0,    // selector head index
            63,   // last block's # of values
            0x00, // 1-bit selector
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x3f, // first block
        ];
        assert_eq!(&buffer[..], expected_bytes);

        ring_buffer.push(15);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        // The 63 one-bit values + the new 4-bit value are then split into:
        // - 62 RLE values
        // - 2 four-bit values
        let expected_bytes = &[
            2, 0, // length
            0, 0,    // selector head index
            2,    // last block's # of values
            0x3f, // first block: RLE selector, second block: 4-bit selector
            1, 0, 0, 0, 0, 0, // first block: value
            62, 0, // first block: length
            0xf0, 0, 0, 0, 0, 0, 0, 0, // second block
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }
}
