// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::mem::size_of;
use zerocopy::{AsBytes, FromBytes};

// TODO(fxbug.dev/123360): Implement compression for real (the current stub functions just serialize
// addresses into arrays of eight bytes each).

/// Given the size of an uncompressed stack trace (expressed as the number of stack frames), returns
/// an upper bound of its compressed size.
pub const fn max_compressed_size(num_frames: usize) -> usize {
    num_frames * size_of::<u64>()
}

/// Compresses a stack trace into a preallocated buffer.
///
/// The destination buffer's size must be suitable for the input size (see `max_compressed_size` in
/// this crate).
///
/// This function returns the number of bytes actually written into the destination buffer (i.e. the
/// compressed size).
pub fn compress_into(src: &[u64], dest: &mut [u8]) -> usize {
    assert!(dest.len() >= max_compressed_size(src.len()), "dest buffer is not big enough");

    let num_bytes = src.len() * size_of::<u64>();
    dest[..num_bytes].copy_from_slice(src.as_bytes());

    num_bytes
}

/// Compresses a stack trace into a dynamically-allocated vector of bytes.
pub fn compress(src: &[u64]) -> Vec<u8> {
    let mut buf = vec![0; max_compressed_size(src.len())];
    let compressed_size = compress_into(src, &mut buf);
    buf.truncate(compressed_size);
    buf
}

/// Uncompresses a stack trace.
pub fn uncompress(src: &[u8]) -> Vec<u64> {
    src.chunks_exact(size_of::<u64>()).map(|slice| u64::read_from(slice).unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // Arbitrary constants, used instead of real code addresses in the tests below:
    const A: u64 = 0x0000008ab3fe9821;
    const B: u64 = 0x0000812c6a4a682e;
    const C: u64 = 0x0048b5a02d459e5a;
    const D: u64 = 0xccccf14839a59c5a;

    #[test_case(&[] ; "empty")]
    #[test_case(&[A] ; "A")]
    #[test_case(&[B] ; "B")]
    #[test_case(&[C] ; "C")]
    #[test_case(&[D] ; "D")]
    #[test_case(&[A, B] ; "AB")]
    #[test_case(&[A, A] ; "AA")]
    #[test_case(&[A, A, B] ; "AAB")]
    #[test_case(&[A, B, C, D] ; "ABCD")]
    #[test_case(&[A, A, B, B, C, C, D, D] ; "AABBCCDD")]
    #[test_case(&[D, D, C, C, B, B, A, A] ; "DDCCBBAA")]
    fn test_compress_and_uncompress(stack_trace: &[u64]) {
        let compressed_data = compress(stack_trace);
        assert_eq!(stack_trace, &uncompress(&compressed_data));
    }
}
