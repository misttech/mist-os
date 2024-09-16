// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io;

use crate::experimental::ring_buffer::Simple8bRleRingBuffer;

/// ZigzagSimple8bRleRingBuffer is a ring buffer that allows logging an i64 into a
/// Simple8bRleRingBuffer. It does this by zigzag encoding an i64 before logging the
/// result into the latter.
#[derive(Clone, Debug)]
pub struct ZigzagSimple8bRleRingBuffer {
    buffer: Simple8bRleRingBuffer,
}

impl ZigzagSimple8bRleRingBuffer {
    /// Create a new ZigzagSimple8bRleRingBuffer that holds at least |min_samples|
    /// The buffer would continually grow and only evict data if it wouldn't
    /// cause the number of samples to fall below |min_samples|.
    pub const fn with_min_samples(min_samples: usize) -> Self {
        Self { buffer: Simple8bRleRingBuffer::with_min_samples(min_samples) }
    }

    /// Serialize the ZigzagSimple8bRleRingBuffer data into a bytes buffer.
    pub fn serialize(&self, buffer: &mut impl io::Write) -> io::Result<()> {
        self.buffer.serialize(buffer)
    }

    /// Push a new value onto the ZigzagSimple8bRleRingBuffer. This might evict
    /// one or more oldest values in the process.
    pub fn push(&mut self, value: i64) {
        self.buffer.push(zigzag_encode(value));
    }
}

/// Return `-2x-1` if `x` is negative. Otherwise return `2x`
const fn zigzag_encode(x: i64) -> u64 {
    // The below bitwise operation assumes that the number has two-complement representation.
    //
    // Note that `>>` is an arithmetic shift and not a logical shift
    // because `i64` is a signed integer type.
    // When `x` is positive, `x >> 63` is always 0, hence `0 ^ (x << 1) = 2x`
    // When `x` is negative, `x >> 63` is always -1, which is all 1 bits,
    // hence `-1 ^ (x << 1) = -2x-1`
    //
    // Example for -1 = 0b111...111:
    // ```
    // 0b111...111 >> 63 = 0b111...111 (which is -1)
    // 0b111...111 << 1 = 0b111...110 (which is -2)
    // 0b111...111 ^ 0b111...110 = 0b000...001 (which is 1)
    // ```
    ((x >> i64::BITS - 1) ^ (x << 1)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_SAMPLES: usize = 120;

    #[test]
    fn test_zigzag_simple8b_rle_ring_buffer() {
        let mut ring_buffer = ZigzagSimple8bRleRingBuffer::with_min_samples(MIN_SAMPLES);
        ring_buffer.push(1);
        ring_buffer.push(i64::MIN);
        ring_buffer.push(i64::MAX);
        ring_buffer.push(-1);

        let mut buffer = vec![];
        ring_buffer.serialize(&mut buffer).expect("serialize should succeed");
        let expected_bytes = &[
            4, 0, // length
            0, 0,    // selector head index
            1,    // last block's # of values
            0xef, // first block: RLE selector, second block: 64-bit selector
            0xfe, // third block: 64-bit selector, fourth block: RLE selector
            2, 0, 0, 0, 0, 0, // first block: value (1 is encoded as 2)
            1, 0, // first block: length
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // second block
            0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // third block
            1, 0, 0, 0, 0, 0, // fourth block: value (-1 is encoded as 1)
            1, 0, // fourth block: length
        ];
        assert_eq!(&buffer[..], expected_bytes);
    }
}
