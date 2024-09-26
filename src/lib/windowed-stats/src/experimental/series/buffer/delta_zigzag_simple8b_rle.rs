// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use byteorder::{LittleEndian, WriteBytesExt};
use std::io;

use crate::experimental::series::buffer::encoding;
use crate::experimental::series::buffer::zigzag_simple8b_rle::ZigzagSimple8bRleRingBuffer;

#[derive(Debug)]
pub enum Encoding {}

impl<A> encoding::Encoding<A> for Encoding {
    type Compression = encoding::compression::DeltaSimple8bRle;

    const PAYLOAD: encoding::payload::Simple8bRle = encoding::payload::Simple8bRle::Signed;
}

pub struct DeltaZigzagSimple8bRleRingBuffer {
    base: Option<i64>,
    last: Option<i64>,
    buffer: ZigzagSimple8bRleRingBuffer,
}

impl DeltaZigzagSimple8bRleRingBuffer {
    /// Create a new DeltaZigzagSimple8bRleRingBuffer that holds at least |min_samples|
    /// (including the base value).
    /// The buffer would continually grow and only evict data if it wouldn't
    /// cause the number of samples to fall below |min_samples|.
    pub const fn with_min_samples(min_samples: usize) -> Self {
        Self {
            base: None,
            last: None,
            buffer: ZigzagSimple8bRleRingBuffer::with_min_samples(min_samples.saturating_sub(1)),
        }
    }

    /// Push |value| onto the ring buffer. Oldest values might be evicted by this call.
    /// Evicted values are applied to the base value.
    pub fn push(&mut self, value: i64) {
        let (base, last) = match (self.base.as_mut(), self.last.as_mut()) {
            (Some(base), Some(last)) => (base, last),
            _ => {
                self.base = Some(value);
                self.last = Some(value);
                return;
            }
        };
        let diff = value - *last;
        let evicted_blocks = self.buffer.push(diff);
        for evicted_block in evicted_blocks {
            *base = base.saturating_add(evicted_block.saturating_sum_with_zigzag_decode());
        }
        self.last = Some(value);
    }

    /// Serialize the DeltaSimple8bRleRingBuffer data into a bytes buffer.
    pub fn serialize(&self, buffer: &mut impl io::Write) -> io::Result<()> {
        let metadata = self.buffer.metadata();
        // Add one count for the base value
        let num_blocks = metadata
            .num_blocks
            .checked_add(1)
            .ok_or(io::Error::new(io::ErrorKind::InvalidData, "Metadata num_blocks overflow"))?;
        buffer.write_u16::<LittleEndian>(num_blocks)?;
        buffer.write_u16::<LittleEndian>(metadata.selectors_head_index)?;
        buffer.write_u8(metadata.last_block_num_values)?;

        if let Some(base) = self.base {
            buffer.write_i64::<LittleEndian>(base)?;
        }
        self.buffer.serialize_data(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_SAMPLES: usize = 120;

    #[test]
    fn test_ring_buffer_has_base_value_only() {
        let mut ring_buffer = DeltaZigzagSimple8bRleRingBuffer::with_min_samples(MIN_SAMPLES);
        ring_buffer.push(-42);
        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            1, 0, // length
            0, 0, // selector head index
            0, // last block's # of values
            214, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // base value (-42)
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_ring_buffer_has_base_and_encoded_diff_value() {
        let mut ring_buffer = DeltaZigzagSimple8bRleRingBuffer::with_min_samples(MIN_SAMPLES);
        ring_buffer.push(42);
        ring_buffer.push(0);
        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            2, 0, // length
            0, 0, // selector head index
            1, // last block's # of values
            42, 0, 0, 0, 0, 0, 0, 0,    // base value
            0x0f, // RLE selector
            83, 0, 0, 0, 0, 0, // first block: value (-42 is encoded as 83)
            1, 0, // first block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }

    #[test]
    fn test_evicted_values_are_added_to_base_value() {
        let mut ring_buffer = DeltaZigzagSimple8bRleRingBuffer::with_min_samples(10);
        let mut counter = 516i64;
        ring_buffer.push(counter);
        for _ in 0..8 {
            counter -= 128; // -128 will be encoded as 255, which takes 8 bits
            ring_buffer.push(counter);
            ring_buffer.push(counter);
        }

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            3, 0, // length
            0, 0, // selector head index
            8, // last block's # of values
            0x04, 0x2, 0, 0, 0, 0, 0, 0,    // base value (0x204 = 516)
            0x77, // first block: 8-bit selector, second block: 8-bit selector
            255, 0, 255, 0, 255, 0, 255, 0, // first block (alternating encoded -128 and 0)
            255, 0, 255, 0, 255, 0, 255, 0, // second block (alternating encoded -128 and 0)
        ];
        assert_eq!(&buffer[..], expected_bytes);

        counter += 1;
        ring_buffer.push(counter);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            3, 0, // length
            1, 0, // selector head index
            1, // last block's # of values
            4, 0, 0, 0, 0, 0, 0, 0,    // base value (512 - 128*4 = 4)
            0x77, // first block: 8-bit selector
            // Note that because selector head index is 1, the first block selector is at
            // bits 4-7. Bits 0-3 above are ignored.
            0x0f, // second block: RLE selector
            255, 0, 255, 0, 255, 0, 255, 0, // first block
            2, 0, 0, 0, 0, 0, // second block: value (1 is encoded as 2)
            1, 0, // second block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }
}
