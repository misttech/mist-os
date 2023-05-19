// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of chunked-compression library in Rust.

use {
    crc::Hasher32 as _,
    rayon::prelude::*,
    zerocopy::{
        byteorder::{LE, U16, U32, U64},
        AsBytes, FromBytes, Unaligned,
    },
};

#[derive(AsBytes, FromBytes, Unaligned, Clone, Copy, Debug)]
#[repr(C)]
struct ChunkedArchiveHeader {
    magic: [u8; 8],
    version: U16<LE>,
    reserved_0: U16<LE>,
    num_frames: U32<LE>,
    checksum: U32<LE>,
    reserved_1: U32<LE>,
    reserved_2: U64<LE>,
}

#[derive(AsBytes, FromBytes, Unaligned, Clone, Copy, Debug)]
#[repr(C)]
struct SeekTableEntry {
    decompressed_offset: U64<LE>,
    decompressed_size: U64<LE>,
    compressed_offset: U64<LE>,
    compressed_size: U64<LE>,
}

impl ChunkedArchiveHeader {
    const CHUNKED_ARCHIVE_MAGIC: [u8; 8] = [0x46, 0x9b, 0x78, 0xef, 0x0f, 0xd0, 0xb2, 0x03];
    const CHUNKED_ARCHIVE_VERSION: u16 = 2;
    const CHUNKED_ARCHIVE_MAX_FRAMES: usize = 1023;
    const CHUNKED_ARCHIVE_CHECKSUM_OFFSET: usize = 16;

    fn new(seek_table: &[SeekTableEntry]) -> Self {
        let header = Self {
            magic: Self::CHUNKED_ARCHIVE_MAGIC,
            version: Self::CHUNKED_ARCHIVE_VERSION.into(),
            reserved_0: 0.into(),
            num_frames: TryInto::<u32>::try_into(seek_table.len()).unwrap().into(),
            checksum: 0.into(),
            reserved_1: 0.into(),
            reserved_2: 0.into(),
        };
        Self { checksum: header.checksum(seek_table).into(), ..header }
    }

    fn checksum(&self, entries: &[SeekTableEntry]) -> u32 {
        let mut first_crc = crc::crc32::Digest::new(crc::crc32::IEEE);
        first_crc.write(&self.as_bytes()[..Self::CHUNKED_ARCHIVE_CHECKSUM_OFFSET]);
        let mut crc = crc::crc32::Digest::new_with_initial(crc::crc32::IEEE, first_crc.sum32());
        crc.write(
            &self.as_bytes()
                [Self::CHUNKED_ARCHIVE_CHECKSUM_OFFSET + self.checksum.as_bytes().len()..],
        );
        crc.write(entries.as_bytes());
        crc.sum32()
    }

    /// Calculate the total header length of an archive *including* all seek table entries.
    fn header_length(num_entries: usize) -> usize {
        std::mem::size_of::<ChunkedArchiveHeader>()
            + (std::mem::size_of::<SeekTableEntry>() * num_entries)
    }
}

pub(crate) struct ChunkedArchive {
    chunk_size: usize,
    chunks: Vec<Vec<u8>>,
}

impl ChunkedArchive {
    const MAX_CHUNKS: usize = ChunkedArchiveHeader::CHUNKED_ARCHIVE_MAX_FRAMES;
    const TARGET_CHUNK_SIZE: usize = 32 * 1024;
    const COMPRESSION_LEVEL: i32 = 14;

    /// Create a ChunkedArchive for `data` compressing each chunk in parallel. This function uses
    /// the `rayon` crate for parallelism. By default compression happens in the global thread pool,
    /// but this function can also be executed within a locally scoped pool.
    pub fn new(data: &[u8], chunk_alignment: usize) -> Self {
        let chunk_size = ChunkedArchive::chunk_size_for(data.len(), chunk_alignment);
        let mut chunks: Vec<Vec<u8>> = vec![];
        data.par_chunks(chunk_size)
            .map(|chunk| {
                let mut compressor = zstd::bulk::Compressor::new(Self::COMPRESSION_LEVEL).unwrap();
                compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
                compressor.compress(chunk).unwrap()
            })
            .collect_into_vec(&mut chunks);
        ChunkedArchive { chunk_size, chunks }
    }

    /// Serialize the chunked archive.
    pub fn serialize(self) -> Vec<u8> {
        let seek_table = self.make_seek_table();
        let header = ChunkedArchiveHeader::new(&seek_table);
        let mut serialized: Vec<u8> = Vec::new();
        let compressed_size: usize = self.chunks.iter().map(|chunk| chunk.len()).sum();
        serialized.reserve(ChunkedArchiveHeader::header_length(seek_table.len()) + compressed_size);
        serialized.extend_from_slice(header.as_bytes());
        serialized.extend_from_slice(seek_table.as_slice().as_bytes());
        serialized
            .extend_from_slice(self.chunks.into_iter().flatten().collect::<Vec<u8>>().as_slice());
        serialized
    }

    /// Calculate how large chunks must be for a given uncompressed buffer.
    fn chunk_size_for(uncompressed_length: usize, chunk_alignment: usize) -> usize {
        if uncompressed_length <= (Self::MAX_CHUNKS * Self::TARGET_CHUNK_SIZE) {
            return Self::TARGET_CHUNK_SIZE;
        }
        let chunk_size = uncompressed_length / ChunkedArchive::MAX_CHUNKS;
        return round_up(chunk_size, chunk_alignment);
    }

    fn make_seek_table(&self) -> Vec<SeekTableEntry> {
        let header_length = ChunkedArchiveHeader::header_length(self.chunks.len());
        let mut seek_table = vec![];
        seek_table.reserve(self.chunks.len());
        let mut compressed_size: usize = 0;
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            seek_table.push(SeekTableEntry {
                decompressed_offset: ((chunk_index * self.chunk_size) as u64).into(),
                decompressed_size: (self.chunk_size as u64).into(),
                compressed_offset: ((header_length + compressed_size) as u64).into(),
                compressed_size: (chunk.len() as u64).into(),
            });
            compressed_size += chunk.len();
        }
        seek_table
    }
}

/// TODO(https://github.com/rust-lang/rust/issues/88581): Replace with
/// `{integer}::checked_next_multiple_of()` when `int_roundings` is available.
fn round_up(value: usize, multiple: usize) -> usize {
    let remainder = value % multiple;
    if remainder > 0 {
        value.checked_add(multiple - remainder).unwrap()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_simple() {
        const BLOCK_SIZE: usize = 8192;
        let data: Vec<u8> = vec![0; 32 * 1024 * 16];
        let archive = ChunkedArchive::new(&data, BLOCK_SIZE);
        // This data is highly compressible, so the result should be smaller than the original.
        let compressed = archive.serialize();
        assert!(compressed.len() <= data.len());
    }
}
