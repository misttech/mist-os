// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Implementation of chunked-compression library in Rust. Archives can be created by making a new
//! [`ChunkedArchive`] and serializing/writing it. An archive's header can be verified and seek
//! table decoded using [`decode_archive`].

use {
    crc::Hasher32,
    itertools::Itertools,
    rayon::prelude::*,
    static_assertions::assert_eq_size,
    std::ops::Range,
    thiserror::Error,
    zerocopy::{
        byteorder::{LE, U16, U32, U64},
        AsBytes, FromBytes, FromZeroes, LayoutVerified, Unaligned,
    },
};

assert_eq_size!(usize, u64);

#[derive(Debug, Error)]
pub enum ChunkedArchiveError {
    #[error("Invalid or unsupported archive version.")]
    InvalidVersion,

    #[error("Archive header has incorrect magic.")]
    BadMagic,

    #[error("Integrity checks failed (e.g. incorrect CRC, inconsistent header fields).")]
    IntegrityError,

    #[error("Value is out of range or cannot be represented in specified type.")]
    OutOfRange,

    #[error("Error when decompressing chunk: `{0:?}`.")]
    DecompressionError(std::io::Error),
}

/// Validated chunk information from an archive. Compressed ranges are relative to the start of
/// compressed data (i.e. they start after the header and seek table).
#[derive(Clone, Debug)]
pub struct ChunkInfo {
    pub decompressed_range: Range<usize>,
    pub compressed_range: Range<usize>,
}

impl ChunkInfo {
    fn from_entry(
        entry: &SeekTableEntry,
        header_length: usize,
    ) -> Result<Self, ChunkedArchiveError> {
        let decompressed_start = entry.decompressed_offset.get() as usize;
        let decompressed_size = entry.decompressed_size.get() as usize;
        let decompressed_range = decompressed_start
            ..decompressed_start
                .checked_add(decompressed_size)
                .ok_or(ChunkedArchiveError::OutOfRange)?;

        let compressed_offset = entry.compressed_offset.get() as usize;
        let compressed_start = compressed_offset
            .checked_sub(header_length)
            .ok_or(ChunkedArchiveError::IntegrityError)?;
        let compressed_size = entry.compressed_size.get() as usize;
        let compressed_range = compressed_start
            ..compressed_start
                .checked_add(compressed_size)
                .ok_or(ChunkedArchiveError::OutOfRange)?;

        Ok(Self { decompressed_range, compressed_range })
    }
}

/// Decode and verify a chunked archive's header, including seek table, returning verified chunk
/// information. Ranges in the resulting chunks are relative to start of `archive_data`.
pub fn decode_archive(
    data: &[u8],
    archive_length: usize,
) -> Result<Option<(Vec<ChunkInfo>, /*archive_data*/ &[u8])>, ChunkedArchiveError> {
    let Some((header, data)) = LayoutVerified::<_, ChunkedArchiveHeader>::new_unaligned_from_prefix(data) else {
        return Ok(None);
    };
    header.decode_seek_table(data, archive_length as u64)
}

/// Chunked archive header.
#[derive(AsBytes, FromZeroes, FromBytes, Unaligned, Clone, Copy, Debug)]
#[repr(C)]
struct ChunkedArchiveHeader {
    magic: [u8; 8],
    version: U16<LE>,
    reserved_0: U16<LE>,
    num_entries: U32<LE>,
    checksum: U32<LE>,
    reserved_1: U32<LE>,
    reserved_2: U64<LE>,
}

/// Chunked archive seek table entry.
#[derive(AsBytes, FromZeroes, FromBytes, Unaligned, Clone, Copy, Debug)]
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

    fn new(seek_table: &[SeekTableEntry]) -> Result<Self, ChunkedArchiveError> {
        let header: ChunkedArchiveHeader = Self {
            magic: Self::CHUNKED_ARCHIVE_MAGIC,
            version: Self::CHUNKED_ARCHIVE_VERSION.into(),
            reserved_0: 0.into(),
            num_entries: TryInto::<u32>::try_into(seek_table.len())
                .or(Err(ChunkedArchiveError::OutOfRange))?
                .into(),
            checksum: 0.into(), // `checksum` is calculated below.
            reserved_1: 0.into(),
            reserved_2: 0.into(),
        };
        Ok(Self { checksum: header.checksum(seek_table).into(), ..header })
    }

    /// Calculate the checksum of the header + all seek table entries.
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

    /// Decode the seek table associated with this archive. `data` must point to the start of
    /// the archive's seek table.
    fn decode_seek_table(
        self,
        data: &[u8],
        archive_length: u64,
    ) -> Result<Option<(Vec<ChunkInfo>, /*chunk_data*/ &[u8])>, ChunkedArchiveError> {
        // Deserialize seek table.
        let num_entries = self.num_entries.get() as usize;
        let Some((entries, chunk_data)) =
            LayoutVerified::<_, [SeekTableEntry]>::new_slice_unaligned_from_prefix(data, num_entries) else {
                return Ok(None);
            };
        let entries = entries.into_slice();

        // Validate archive header.
        if self.magic != Self::CHUNKED_ARCHIVE_MAGIC {
            return Err(ChunkedArchiveError::BadMagic);
        }
        if self.version.get() != Self::CHUNKED_ARCHIVE_VERSION {
            return Err(ChunkedArchiveError::InvalidVersion);
        }
        if self.checksum.get() != self.checksum(entries) {
            return Err(ChunkedArchiveError::IntegrityError);
        }
        if entries.len() > Self::CHUNKED_ARCHIVE_MAX_FRAMES {
            return Err(ChunkedArchiveError::IntegrityError);
        }

        // Validate seek table using invariants I0 through I5.

        // I0: The first seek table entry, if any, must have decompressed offset 0.
        if !entries.is_empty() && entries[0].decompressed_offset.get() != 0 {
            return Err(ChunkedArchiveError::IntegrityError);
        }

        // I1: The compressed offsets of all seek table entries must not overlap with the header.
        let header_length = Self::header_length(entries.len());
        if entries.iter().any(|entry| entry.compressed_offset.get() < header_length as u64) {
            return Err(ChunkedArchiveError::IntegrityError);
        }

        // I2: Each entry's decompressed offset must be equal to the end of the previous frame
        //     (i.e. to the previous frame's decompressed offset + length).
        for (prev, curr) in entries.iter().tuple_windows() {
            if (prev.decompressed_offset.get() + prev.decompressed_size.get())
                != curr.decompressed_offset.get()
            {
                return Err(ChunkedArchiveError::IntegrityError);
            }
        }

        // I3: Each entry's compressed offset must be greater than or equal to the end of the
        //     previous frame (i.e. to the previous frame's compressed offset + length).
        for (prev, curr) in entries.iter().tuple_windows() {
            if (prev.compressed_offset.get() + prev.compressed_size.get())
                > curr.compressed_offset.get()
            {
                return Err(ChunkedArchiveError::IntegrityError);
            }
        }

        // I4: Each entry must have a non-zero decompressed and compressed length.
        for entry in entries.iter() {
            if entry.decompressed_size.get() == 0 || entry.compressed_size.get() == 0 {
                return Err(ChunkedArchiveError::IntegrityError);
            }
        }

        // I5: Data referenced by each entry must fit within the specified file size.
        for entry in entries.iter() {
            let compressed_end = entry.compressed_offset.get() + entry.compressed_size.get();
            if compressed_end > archive_length {
                return Err(ChunkedArchiveError::IntegrityError);
            }
        }

        let seek_table = entries
            .into_iter()
            .map(|entry| ChunkInfo::from_entry(entry, header_length))
            .try_collect()?;
        Ok(Some((seek_table, chunk_data)))
    }
}

pub(crate) struct ChunkedArchive {
    chunks: Vec<(/*chunk_size*/ usize, /*chunk_data*/ Vec<u8>)>,
}

impl ChunkedArchive {
    const MAX_CHUNKS: usize = ChunkedArchiveHeader::CHUNKED_ARCHIVE_MAX_FRAMES;
    const TARGET_CHUNK_SIZE: usize = 32 * 1024;
    const COMPRESSION_LEVEL: i32 = 14;

    /// Create a ChunkedArchive for `data` compressing each chunk in parallel. This function uses
    /// the `rayon` crate for parallelism. By default compression happens in the global thread pool,
    /// but this function can also be executed within a locally scoped pool.
    pub fn new(data: &[u8], chunk_alignment: usize) -> Result<Self, ChunkedArchiveError> {
        let chunk_size = ChunkedArchive::chunk_size_for(data.len(), chunk_alignment);
        let mut chunks: Vec<Result<(usize, Vec<u8>), ChunkedArchiveError>> = vec![];
        data.par_chunks(chunk_size)
            .map(|chunk| {
                let mut compressor = zstd::bulk::Compressor::new(Self::COMPRESSION_LEVEL)
                    .map_err(ChunkedArchiveError::DecompressionError)?;
                compressor
                    .set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
                    .map_err(ChunkedArchiveError::DecompressionError)?;
                Ok((
                    chunk.len(),
                    compressor.compress(chunk).map_err(ChunkedArchiveError::DecompressionError)?,
                ))
            })
            .collect_into_vec(&mut chunks);
        let chunks: Vec<_> = chunks.into_iter().try_collect()?;
        Ok(ChunkedArchive { chunks })
    }

    /// Total size of the archive in bytes.
    pub fn serialized_size(&self) -> usize {
        let (seek_table, compressed_size) = self.make_seek_table();
        ChunkedArchiveHeader::header_length(seek_table.len()) + compressed_size
    }

    /// Write the archive to `writer`.
    pub fn write(self, mut writer: impl std::io::Write) -> Result<(), std::io::Error> {
        let (seek_table, _compressed_size) = self.make_seek_table();
        let header = ChunkedArchiveHeader::new(&seek_table).unwrap();
        writer.write_all(header.as_bytes())?;
        writer.write_all(seek_table.as_slice().as_bytes())?;
        for (_, chunk) in self.chunks {
            writer.write_all(&chunk)?;
        }
        Ok(())
    }

    /// Calculate how large chunks must be for a given uncompressed buffer.
    fn chunk_size_for(uncompressed_length: usize, chunk_alignment: usize) -> usize {
        if uncompressed_length <= (Self::MAX_CHUNKS * Self::TARGET_CHUNK_SIZE) {
            return Self::TARGET_CHUNK_SIZE;
        }
        let chunk_size = uncompressed_length / ChunkedArchive::MAX_CHUNKS;
        return round_up(chunk_size, chunk_alignment);
    }

    /// Create the seek table for this archive.
    fn make_seek_table(&self) -> (Vec<SeekTableEntry>, usize) {
        let header_length = ChunkedArchiveHeader::header_length(self.chunks.len());
        let mut seek_table = vec![];
        seek_table.reserve(self.chunks.len());
        let mut compressed_size: usize = 0;
        let mut decompressed_offset: usize = 0;
        for (chunk_size, chunk) in &self.chunks {
            seek_table.push(SeekTableEntry {
                decompressed_offset: (decompressed_offset as u64).into(),
                decompressed_size: (*chunk_size as u64).into(),
                compressed_offset: ((header_length + compressed_size) as u64).into(),
                compressed_size: (chunk.len() as u64).into(),
            });
            compressed_size += chunk.len();
            decompressed_offset += *chunk_size;
        }
        (seek_table, compressed_size)
    }
}

/// Streaming decompressor for chunked archives. Example:
/// ```
/// // Create a chunked archive:
/// let data: Vec<u8> = vec![3; 1024];
/// let compressed = ChunkedArchive::new(&data, /*block_size*/ 8192).serialize().unwrap();
/// // Verify the header + decode the seek table:
/// let (seek_table, archive_data) = decode_archive(&compressed, compressed.len())?.unwrap();
/// let mut decompressed: Vec<u8> = vec![];
/// let mut on_chunk = |data: &[u8]| { decompressed.extend_from_slice(data); };
/// let mut decompressor = ChunkedDecompressor(seek_table);
/// // `on_chunk` is invoked as each slice is made available. Archive can be provided as chunks.
/// decompressor.update(archive_data, &mut on_chunk);
/// assert_eq!(data.as_slice(), decompressed.as_slice());
/// ```
pub struct ChunkedDecompressor {
    seek_table: Vec<ChunkInfo>,
    buffer: Vec<u8>,
    curr_chunk: usize,
    total_compressed_size: usize,
}

impl ChunkedDecompressor {
    /// Create a new decompressor to decode an archive from a validated seek table.
    pub fn new(seek_table: Vec<ChunkInfo>) -> Self {
        // Caller must provide a valid seek table with at least one entry.
        assert!(!seek_table.is_empty());
        let last_chunk = seek_table.last().unwrap();
        let total_compressed_size = last_chunk.compressed_range.end;
        Self { seek_table, buffer: vec![], curr_chunk: 0, total_compressed_size }
    }

    pub fn seek_table(&self) -> &Vec<ChunkInfo> {
        &self.seek_table
    }

    pub fn update(
        &mut self,
        data: &[u8],
        chunk_callback: &mut impl FnMut(&[u8]) -> (),
    ) -> Result<(), ChunkedArchiveError> {
        // Caller must not provide too much data and must stop calling when all chunks are complete.
        if !(self.buffer.len() + data.len() <= self.total_compressed_size
            && self.curr_chunk < self.seek_table.len())
        {
            return Err(ChunkedArchiveError::OutOfRange);
        }

        self.buffer.extend_from_slice(data);

        // TODO(fxbug.dev/127530): Only buffer a single chunk rather than the whole archive.

        if self.buffer.len() == self.total_compressed_size {
            for chunk in &self.seek_table {
                let chunk_data = &self.buffer[chunk.compressed_range.clone()];
                let decompressed =
                    zstd::bulk::decompress(chunk_data, chunk.decompressed_range.len())
                        .map_err(|err| ChunkedArchiveError::DecompressionError(err))?;
                chunk_callback(&decompressed);
            }
        }
        Ok(())
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

    use {super::*, rand::Rng, std::matches};

    /// Create a compressed archive and ensure we can decode it as a valid archive that passes all
    /// required integrity checks.
    #[test]
    fn compress_simple() {
        const BLOCK_SIZE: usize = 8192;
        let data: Vec<u8> = vec![0; 32 * 1024 * 16];
        let archive = ChunkedArchive::new(&data, BLOCK_SIZE).unwrap();
        // This data is highly compressible, so the result should be smaller than the original.
        let mut compressed: Vec<u8> = vec![];
        archive.write(&mut compressed).unwrap();
        assert!(compressed.len() <= data.len());
        // We should be able to decode and verify the archive's integrity in-place.
        assert!(decode_archive(&compressed, compressed.len()).unwrap().is_some());
    }

    /// Generate a header + seek table for verifying invariants/integrity checks.
    fn generate_archive(
        num_entries: usize,
    ) -> (ChunkedArchiveHeader, Vec<SeekTableEntry>, /*archive_length*/ u64) {
        let mut seek_table = vec![];
        seek_table.reserve(num_entries);
        let header_length = ChunkedArchiveHeader::header_length(num_entries) as u64;
        const COMPRESSED_CHUNK_SIZE: u64 = 1024;
        const DECOMPRESSED_CHUNK_SIZE: u64 = 2048;
        for n in 0..(num_entries as u64) {
            seek_table.push(SeekTableEntry {
                compressed_offset: (header_length + (n * COMPRESSED_CHUNK_SIZE)).into(),
                compressed_size: COMPRESSED_CHUNK_SIZE.into(),
                decompressed_offset: (n * DECOMPRESSED_CHUNK_SIZE).into(),
                decompressed_size: DECOMPRESSED_CHUNK_SIZE.into(),
            });
        }
        let header = ChunkedArchiveHeader::new(&seek_table).unwrap();
        let archive_length: u64 = header_length + (num_entries as u64 * COMPRESSED_CHUNK_SIZE);
        (header, seek_table, archive_length)
    }

    #[test]
    fn should_validate_self() {
        let (header, seek_table, archive_length) = generate_archive(4);
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(header.decode_seek_table(serialized_table, archive_length).unwrap().is_some());
    }

    #[test]
    fn should_validate_empty() {
        let (header, _, archive_length) = generate_archive(0);
        assert!(header.decode_seek_table(&[], archive_length).unwrap().is_some());
    }

    #[test]
    fn should_detect_bad_magic() {
        let (header, seek_table, archive_length) = generate_archive(4);
        let mut corrupt_magic = ChunkedArchiveHeader::CHUNKED_ARCHIVE_MAGIC;
        corrupt_magic[0] = !corrupt_magic[0];
        let bad_magic = ChunkedArchiveHeader { magic: corrupt_magic, ..header };
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            bad_magic.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::BadMagic
        ));
    }
    #[test]
    fn should_detect_wrong_version() {
        let (header, seek_table, archive_length) = generate_archive(4);
        let wrong_version = ChunkedArchiveHeader {
            version: (ChunkedArchiveHeader::CHUNKED_ARCHIVE_VERSION + 1).into(),
            ..header
        };
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            wrong_version.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::InvalidVersion
        ));
    }

    #[test]
    fn should_detect_corrupt_checksum() {
        let (header, seek_table, archive_length) = generate_archive(4);
        let corrupt_checksum =
            ChunkedArchiveHeader { checksum: (!header.checksum.get()).into(), ..header };
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            corrupt_checksum.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn should_reject_too_many_entries() {
        let (too_many_entries, seek_table, archive_length) =
            generate_archive(ChunkedArchiveHeader::CHUNKED_ARCHIVE_MAX_FRAMES + 1);

        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            too_many_entries.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i0_first_entry_zero() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        assert_eq!(seek_table[0].decompressed_offset.get(), 0);
        seek_table[0].decompressed_offset = 1.into();

        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i1_no_header_overlap() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        let header_end = ChunkedArchiveHeader::header_length(seek_table.len()) as u64;
        assert!(seek_table[0].compressed_offset.get() >= header_end);
        seek_table[0].compressed_offset = (header_end - 1).into();
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i2_decompressed_monotonic() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        assert_eq!(
            seek_table[0].decompressed_offset.get() + seek_table[0].decompressed_size.get(),
            seek_table[1].decompressed_offset.get()
        );
        seek_table[1].decompressed_offset = (seek_table[1].decompressed_offset.get() - 1).into();
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i3_compressed_monotonic() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        assert!(
            (seek_table[0].compressed_offset.get() + seek_table[0].compressed_size.get())
                <= seek_table[1].compressed_offset.get()
        );
        seek_table[1].compressed_offset = (seek_table[1].compressed_offset.get() - 1).into();
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i4_nonzero_compressed_size() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        assert!(seek_table[0].compressed_size.get() > 0);
        seek_table[0].compressed_size = 0.into();
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i4_nonzero_decompressed_size() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        assert!(seek_table[0].decompressed_size.get() > 0);
        seek_table[0].decompressed_size = 0.into();
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn invariant_i5_within_archive() {
        let (header, mut seek_table, archive_length) = generate_archive(4);
        let last_entry = seek_table.last_mut().unwrap();
        assert!(
            (last_entry.compressed_offset.get() + last_entry.compressed_size.get())
                <= archive_length
        );
        last_entry.compressed_offset = (archive_length + 1).into();
        let serialized_table = seek_table.as_slice().as_bytes();
        assert!(matches!(
            header.decode_seek_table(serialized_table, archive_length).unwrap_err(),
            ChunkedArchiveError::IntegrityError
        ));
    }

    #[test]
    fn test_streaming_decompressor() {
        const BLOCK_SIZE: usize = 8192;
        const UNCOMPRESSED_LENGTH: usize = 3_000_000;
        let data: Vec<u8> = {
            let range = rand::distributions::Uniform::<u8>::new_inclusive(0, 255);
            rand::thread_rng().sample_iter(&range).take(UNCOMPRESSED_LENGTH).collect()
        };
        let mut compressed: Vec<u8> = vec![];
        ChunkedArchive::new(&data, BLOCK_SIZE)
            .expect("compress")
            .write(&mut compressed)
            .expect("write archive");
        let (seek_table, chunk_data) =
            decode_archive(&compressed, compressed.len()).unwrap().unwrap();

        // Make sure we have multiple chunks for this test.
        let num_chunks = seek_table.len();
        assert!(num_chunks > 1);

        let mut decompressor = ChunkedDecompressor::new(seek_table.clone());

        let mut decoded_chunks: usize = 0;
        let mut decompressed_offset: usize = 0;
        let mut chunk_callback = |decompressed_chunk: &[u8]| {
            assert!(
                decompressed_chunk
                    == &data[decompressed_offset..decompressed_offset + decompressed_chunk.len()]
            );
            decompressed_offset += decompressed_chunk.len();
            decoded_chunks += 1;
        };

        // Stream data into the decompressor in small chunks to exhaust more edge cases.
        chunk_data
            .chunks(4)
            .for_each(|data| decompressor.update(data, &mut chunk_callback).unwrap());
        assert_eq!(decoded_chunks, num_chunks);
    }
}
