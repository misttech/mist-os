// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::mem::take;

use crate::{Chunk, Decode, DecodeError, Decoder, DecoderExt as _, Owned, CHUNK_SIZE};

/// A basic decoder.
///
/// Basic decoders do not support resources.
pub struct BasicDecoder<'buf> {
    chunks: &'buf mut [Chunk],
}

impl<'buf> BasicDecoder<'buf> {
    /// Returns a new decoder for the given chunks and handles.
    pub fn new(chunks: &'buf mut [Chunk]) -> Self {
        Self { chunks }
    }

    /// Finishes decoding.
    ///
    /// Returns `Err` if not all chunks or handles were used.
    pub fn finish(self) -> Result<(), DecodeError> {
        if !self.chunks.is_empty() {
            return Err(DecodeError::ExtraBytes { num_extra: self.chunks.len() * CHUNK_SIZE });
        }

        Ok(())
    }

    /// Decodes the entirety of the decoder's data as a `T`.
    ///
    /// Returns `Err` if decoding failed or not all chunks or handles were used.
    pub fn decode<T: Decode<Self>>(mut self) -> Result<Owned<'buf, T>, DecodeError> {
        let owned = self.decode_next::<T>()?;
        self.finish()?;
        Ok(owned)
    }
}

impl<'buf> Decoder<'buf> for BasicDecoder<'buf> {
    fn take_chunks(&mut self, count: usize) -> Result<&'buf mut [Chunk], DecodeError> {
        if count > self.chunks.len() {
            return Err(DecodeError::InsufficientData);
        }

        let chunks = take(&mut self.chunks);
        let (prefix, suffix) = unsafe { chunks.split_at_mut_unchecked(count) };
        self.chunks = suffix;
        Ok(prefix)
    }

    fn __internal_take_handles(&mut self, _: usize) -> Result<(), DecodeError> {
        Err(DecodeError::InsufficientHandles)
    }

    fn __internal_handles_remaining(&mut self) -> usize {
        0
    }
}
