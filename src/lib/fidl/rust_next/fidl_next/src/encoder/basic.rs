// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Chunk, Encoder, CHUNK_SIZE};

/// A basic FIDL encoder.
///
/// Basic FIDL encoders do not support resources.
#[derive(Default)]
pub struct BasicEncoder {
    chunks: Vec<Chunk>,
}

impl BasicEncoder {
    /// Returns a new base encoder.
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Finishes encoding, returning the encoded chunks and handles.
    pub fn finish(self) -> Vec<Chunk> {
        self.chunks
    }
}

impl Encoder for BasicEncoder {
    fn bytes_written(&self) -> usize {
        self.chunks.len() * CHUNK_SIZE
    }

    fn reserve(&mut self, len: usize) {
        let count = len.div_ceil(CHUNK_SIZE);
        self.chunks.reserve(count);
        let ptr = unsafe { self.chunks.as_mut_ptr().add(self.chunks.len()) };
        unsafe {
            ptr.write_bytes(0, count);
        }
        unsafe {
            self.chunks.set_len(self.chunks.len() + count);
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let count = bytes.len().div_ceil(CHUNK_SIZE);
        self.chunks.reserve(count);
        let ptr = unsafe { self.chunks.as_mut_ptr().add(self.chunks.len()).cast::<u8>() };

        // Copy all the bytes
        unsafe {
            ptr.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        }

        // Zero out any trailing bytes
        let trailing = count * CHUNK_SIZE - bytes.len();
        unsafe {
            ptr.add(bytes.len()).write_bytes(0, trailing);
        }

        // Set the new length
        unsafe {
            self.chunks.set_len(self.chunks.len() + count);
        }
    }

    fn rewrite(&mut self, pos: usize, bytes: &[u8]) {
        assert!(pos + bytes.len() <= self.bytes_written());

        let ptr = unsafe { self.chunks.as_mut_ptr().cast::<u8>().add(pos) };
        unsafe {
            ptr.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        }
    }

    fn __internal_handle_count(&self) -> usize {
        0
    }
}
