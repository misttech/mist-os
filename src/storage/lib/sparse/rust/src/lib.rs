// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_endian = "big")]
assert!(false, "This library assumes little-endian!");

pub mod builder;
mod format;

use {
    crate::format::{ChunkHeader, SparseHeader},
    anyhow::{bail, ensure, Context, Result},
    core::fmt,
    serde::de::DeserializeOwned,
    std::{
        fs::File,
        io::{Cursor, Read, Seek, SeekFrom, Write},
        path::Path,
    },
    tempfile::{NamedTempFile, TempPath},
};

// Size of blocks to write.  Note that the format supports varied block sizes; this is the preferred
// size by this library.
const BLK_SIZE: usize = 0x1000;

fn deserialize_from<'a, T: DeserializeOwned, R: Read>(source: &mut R) -> Result<T> {
    let mut buf = vec![0u8; std::mem::size_of::<T>()];
    source.read_exact(&mut buf[..]).context("Failed to read bytes")?;
    Ok(bincode::deserialize(&buf[..])?)
}

/// A union trait for `Read` and `Seek`.
pub trait Reader: Read + Seek {}

impl<T: Read + Seek> Reader for T {}

/// A union trait for `Write` and `Seek` that also allows truncation.
pub trait Writer: Write + Seek {
    /// Sets the length of the output stream.
    fn set_len(&mut self, size: u64) -> Result<()>;
}

impl Writer for File {
    fn set_len(&mut self, size: u64) -> Result<()> {
        Ok(File::set_len(self, size)?)
    }
}

impl Writer for Cursor<Vec<u8>> {
    fn set_len(&mut self, size: u64) -> Result<()> {
        Vec::resize(self.get_mut(), size as usize, 0u8);
        Ok(())
    }
}

// A wrapper around a Reader, which makes it seem like the underlying stream is only self.1 bytes
// long.  The underlying reader is still advanced upon reading.
struct LimitedReader<'a>(pub &'a mut dyn Reader, pub usize);

impl<'a> Read for LimitedReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let offset = self.0.stream_position()?;
        let avail = self.1.saturating_sub(offset as usize);
        let to_read = std::cmp::min(avail, buf.len());
        self.0.read(&mut buf[..to_read])
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Chunk {
    /// `Raw` represents a set of blocks to be written to disk as-is.
    /// `start` and `size` are in bytes, but must be block-aligned.
    Raw { start: u64, size: usize },
    /// Represents a Chunk that has the `value` repeated enough to fill `size` bytes.
    /// `size` is in bytes, but must be block-aligned.
    Fill { start: u64, size: usize, value: u32 },
    /// `DontCare` represents a set of blocks that need to be "offset" by the
    /// image recipeint. If an image needs to be broken up into two sparse images,
    /// and we flash n bytes for Sparse Image 1, Sparse Image 2
    /// needs to start with a DontCareChunk with (n/blocksize) blocks as its "size" property.
    /// `size` is in bytes, but must be block-aligned.
    DontCare { size: usize },
    /// `Crc32Chunk` is used as a checksum of a given set of Chunks for a SparseImage.
    /// This is not required and unused in most implementations of the Sparse Image
    /// format. The type is included for completeness. It has 4 bytes of CRC32 checksum
    /// as describable in a u32.
    #[allow(dead_code)]
    Crc32 { checksum: u32 },
}

impl Chunk {
    fn valid(&self, block_size: usize) -> bool {
        let size_bytes = match self {
            Self::Raw { size, .. } => *size,
            Self::Fill { size, .. } => *size,
            Self::DontCare { size } => *size,
            Self::Crc32 { .. } => 0,
        };
        size_bytes % block_size == 0
    }

    /// Return number of blocks the chunk expands to when written to the partition.
    fn output_blocks(&self, block_size: usize) -> u32 {
        let size_bytes = match self {
            Self::Raw { size, .. } => *size,
            Self::Fill { size, .. } => *size,
            Self::DontCare { size } => *size,
            Self::Crc32 { .. } => 0,
        };
        ((size_bytes + block_size - 1) / block_size) as u32
    }

    /// `chunk_type` returns the integer flag to represent the type of chunk
    /// to use in the ChunkHeader
    fn chunk_type(&self) -> u16 {
        match self {
            Self::Raw { .. } => format::CHUNK_TYPE_RAW,
            Self::Fill { .. } => format::CHUNK_TYPE_FILL,
            Self::DontCare { .. } => format::CHUNK_TYPE_DONT_CARE,
            Self::Crc32 { .. } => format::CHUNK_TYPE_CRC32,
        }
    }

    /// `chunk_data_len` returns the length of the chunk's header plus the
    /// length of the data when serialized
    fn chunk_data_len(&self) -> usize {
        let header_size = format::CHUNK_HEADER_SIZE;
        let data_size = match self {
            Self::Raw { size, .. } => *size,
            Self::Fill { .. } => std::mem::size_of::<u32>(),
            Self::DontCare { .. } => 0,
            Self::Crc32 { .. } => std::mem::size_of::<u32>(),
        };
        header_size + data_size
    }

    /// Writes the chunk to the given Writer. The source is a Reader of the
    /// original file the sparse chunk was created from.
    fn write<W: Write + Seek, R: Read + Seek>(&self, source: &mut R, dest: &mut W) -> Result<()> {
        ensure!(self.valid(BLK_SIZE), "Not writing invalid chunk");
        let header = ChunkHeader::new(
            self.chunk_type(),
            0x0,
            self.output_blocks(BLK_SIZE),
            self.chunk_data_len() as u32,
        );

        let header_bytes: Vec<u8> = bincode::serialize(&header)?;
        std::io::copy(&mut Cursor::new(header_bytes), dest)?;

        match self {
            Self::Raw { start, size } => {
                // Seek to start
                source.seek(SeekFrom::Start(*start))?;
                // Read in the data
                let mut reader = source.take(*size as u64);
                let n: usize = std::io::copy(&mut reader, dest).unwrap().try_into()?;
                if n < *size {
                    let zeroes = vec![0u8; *size - n];
                    std::io::copy(&mut Cursor::new(zeroes), dest)?;
                }
            }
            Self::Fill { value, .. } => {
                // Serliaze the value,
                bincode::serialize_into(dest, value)?;
            }
            Self::DontCare { .. } => {
                // DontCare has no data to write
            }
            Self::Crc32 { checksum } => {
                bincode::serialize_into(dest, checksum)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Raw { start, size } => {
                format!("RawChunk: start: {}, total bytes: {}", start, size)
            }
            Self::Fill { start, size, value } => {
                format!("FillChunk: start: {}, value: {}, n_blocks: {}", start, value, size)
            }
            Self::DontCare { size } => {
                format!("DontCareChunk: bytes: {}", size)
            }
            Self::Crc32 { checksum } => format!("Crc32Chunk: checksum: {:?}", checksum),
        };
        write!(f, "{}", message)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SparseFileWriter {
    chunks: Vec<Chunk>,
}

impl SparseFileWriter {
    fn new(chunks: Vec<Chunk>) -> SparseFileWriter {
        SparseFileWriter { chunks }
    }

    fn total_blocks(&self) -> u32 {
        self.chunks.iter().map(|c| c.output_blocks(BLK_SIZE)).sum()
    }

    #[tracing::instrument(skip(self, reader, writer))]
    fn write<W: Write + Seek, R: Read + Seek>(&self, reader: &mut R, writer: &mut W) -> Result<()> {
        let header = SparseHeader::new(
            BLK_SIZE.try_into().unwrap(),          // Size of the blocks
            self.total_blocks(),                   // Total blocks in this image
            self.chunks.len().try_into().unwrap(), // Total chunks in this image
        );

        let header_bytes: Vec<u8> = bincode::serialize(&header)?;
        std::io::copy(&mut Cursor::new(header_bytes), writer)?;

        for chunk in &self.chunks {
            chunk.write(reader, writer)?;
        }

        Ok(())
    }
}

impl fmt::Display for SparseFileWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, r"SparseFileWriter: {} Chunks:", self.chunks.len())
    }
}

/// `add_sparse_chunk` takes the input vec, v and given `Chunk`, chunk, and
/// attempts to add the chunk to the end of the vec. If the current last chunk
/// is the same kind of Chunk as the `chunk`, then it will merge the two chunks
/// into one chunk.
///
/// Example: A `FillChunk` with value 0 and size 1 is the last chunk
/// in `v`, and `chunk` is a FillChunk with value 0 and size 1, after this,
/// `v`'s last element will be a FillChunk with value 0 and size 2.
fn add_sparse_chunk(r: &mut Vec<Chunk>, chunk: Chunk) -> Result<()> {
    match r.last_mut() {
        // We've got something in the Vec... if they are both the same type,
        // merge them, otherwise, just push the new one
        Some(last) => match (&last, &chunk) {
            (Chunk::Raw { start, size }, Chunk::Raw { size: new_length, .. }) => {
                *last = Chunk::Raw { start: *start, size: size + new_length };
                return Ok(());
            }
            (
                Chunk::Fill { start, size, value },
                Chunk::Fill { size: new_size, value: new_value, .. },
            ) if value == new_value => {
                *last = Chunk::Fill { start: *start, size: size + new_size, value: *value };
                return Ok(());
            }
            (Chunk::DontCare { size }, Chunk::DontCare { size: new_size }) => {
                *last = Chunk::DontCare { size: size + new_size };
                return Ok(());
            }
            _ => {}
        },
        None => {}
    }

    // If the chunk types differ they cannot be merged.
    // If they are both Fill but have different values, they cannot be merged.
    // Crc32 cannot be merged.
    // If we dont have any chunks then we add it
    r.push(chunk);
    Ok(())
}

/// Reads a sparse image from `source` and expands it to its unsparsed representation in `dest`.
#[tracing::instrument(skip(source, dest))]
pub fn unsparse<W: Writer, R: Reader>(source: &mut R, dest: &mut W) -> Result<()> {
    let header: SparseHeader = deserialize_from(source).context("Failed to read header")?;
    ensure!(header.valid(), "Invalid header");
    let num_chunks = header.total_chunks as usize;

    for _ in 0..num_chunks {
        expand_chunk(source, dest, header.blk_sz).context("Failed to expand chunk")?;
    }
    // Truncate output to its current seek offset, in case the last chunk we wrote was DontNeed.
    let offset = dest.stream_position()?;
    dest.set_len(offset).context("Failed to truncate output")?;
    dest.flush()?;
    Ok(())
}

/// Reads a chunk from `source`, and expands it, writing the result to `dest`.
fn expand_chunk<R: Read + Seek, W: Write + Seek>(
    source: &mut R,
    dest: &mut W,
    block_size: u32,
) -> Result<()> {
    let header: ChunkHeader =
        deserialize_from(source).context("Failed to deserialize chunk header")?;
    ensure!(header.valid(), "Invalid chunk header {:x?}", header);
    let size = (header.chunk_sz * block_size) as usize;
    match header.chunk_type {
        format::CHUNK_TYPE_RAW => {
            let limit = source.stream_position()? as usize + size;
            std::io::copy(&mut LimitedReader(source, limit), dest)
                .context("Failed to copy contents")?;
        }
        format::CHUNK_TYPE_FILL => {
            let value: [u8; 4] =
                deserialize_from(source).context("Failed to deserialize fill value")?;
            assert!(size % 4 == 0);
            let repeated = value.repeat(size / 4);
            std::io::copy(&mut Cursor::new(repeated), dest).context("Failed to fill contents")?;
        }
        format::CHUNK_TYPE_DONT_CARE => {
            dest.seek(SeekFrom::Current(size as i64)).context("Failed to skip contents")?;
        }
        format::CHUNK_TYPE_CRC32 => {
            let _: u32 = deserialize_from(source).context("Failed to deserialize fill value")?;
        }
        _ => bail!("Invalid type {}", header.chunk_type),
    };
    Ok(())
}

/// `resparse` takes a SparseFile and a maximum size and will
/// break the single SparseFile into multiple SparseFiles whose
/// size will not exceed the maximum_download_size.
///
/// This will return an error if max_download_size is <= BLK_SIZE
#[tracing::instrument]
fn resparse(
    sparse_file: SparseFileWriter,
    max_download_size: u64,
) -> Result<Vec<SparseFileWriter>> {
    if max_download_size as usize <= BLK_SIZE {
        anyhow::bail!(
            "Given maximum download size ({}) is less than the block size ({})",
            max_download_size,
            BLK_SIZE
        );
    }
    let mut ret = Vec::<SparseFileWriter>::new();

    // File length already starts with a header for the SparseFile as
    // well as the size of a potential DontCare and Crc32 Chunk
    let sunk_file_length = format::SPARSE_HEADER_SIZE
        + (Chunk::DontCare { size: BLK_SIZE }.chunk_data_len()
            + Chunk::Crc32 { checksum: 2345 }.chunk_data_len());

    let mut chunk_pos = 0;
    while chunk_pos < sparse_file.chunks.len() {
        tracing::trace!("Starting a new file at chunk position: {}", chunk_pos);

        let mut file_len = 0;
        file_len += sunk_file_length;

        let mut chunks = Vec::<Chunk>::new();
        if chunk_pos > 0 {
            // If we already have some chunks... add a DontCare block to
            // move the pointer
            tracing::trace!("Adding a DontCare chunk offset: {}", chunk_pos);
            let dont_care = Chunk::DontCare { size: BLK_SIZE * chunk_pos };
            chunks.push(dont_care);
        }

        loop {
            match sparse_file.chunks.get(chunk_pos) {
                Some(chunk) => {
                    let curr_chunk_data_len = chunk.chunk_data_len();
                    if (file_len + curr_chunk_data_len) as u64 > max_download_size {
                        tracing::trace!("Current file size is: {} and adding another chunk of len: {} would put us over our max: {}", file_len, curr_chunk_data_len, max_download_size);

                        // Add a dont care chunk to do the last offset.
                        // While this is not strictly speaking needed, other tools
                        // (simg2simg) produce this chunk, and the Sparse image inspection tool
                        // simg_dump will produce a warning if a sparse file does not have the same
                        // number of output blocks as declared in the header.
                        let remainder_chunks = sparse_file.chunks.len() - chunk_pos;
                        let dont_care = Chunk::DontCare { size: BLK_SIZE * remainder_chunks };
                        chunks.push(dont_care);
                        break;
                    }
                    tracing::trace!("chunk: {} curr_chunk_data_len: {} current file size: {} max_download_size: {} diff: {}", chunk_pos, curr_chunk_data_len, file_len, max_download_size, (max_download_size as usize - file_len - curr_chunk_data_len) );
                    add_sparse_chunk(&mut chunks, chunk.clone())?;
                    file_len += curr_chunk_data_len;
                    chunk_pos = chunk_pos + 1;
                }
                None => {
                    tracing::trace!("Finished iterating chunks");
                    break;
                }
            }
        }
        let resparsed = SparseFileWriter::new(chunks);
        tracing::trace!("resparse: Adding new SparseFile: {}", resparsed);
        ret.push(resparsed);
    }

    Ok(ret)
}

/// Takes the given `file_to_upload` for the `named` partition and creates a
/// set of temporary files in the given `dir` in Sparse Image Format. With the
/// provided `max_download_size` constraining file size.
///
/// # Arguments
///
/// * `writer` - Used for writing log information.
/// * `name` - Name of the partition the image. Used for logs only.
/// * `file_to_upload` - Path to the file to translate to sparse image format.
/// * `dir` - Path to write the Sparse file(s).
/// * `max_download_size` - Maximum size that can be downloaded by the device.
#[tracing::instrument(skip(writer))]
pub fn build_sparse_files<W: Write>(
    writer: &mut W,
    name: &str,
    file_to_upload: &str,
    dir: &Path,
    max_download_size: u64,
) -> Result<Vec<TempPath>> {
    if max_download_size as usize <= BLK_SIZE {
        anyhow::bail!(
            "Given maximum download size ({}) is less than the block size ({})",
            max_download_size,
            BLK_SIZE
        );
    }
    writeln!(writer, "Building sparse files for: {}. File: {}", name, file_to_upload)?;
    let mut in_file = File::open(file_to_upload)?;

    let mut total_read: usize = 0;
    // Preallocate vector to avoid reallocations as it grows.
    let mut chunks =
        Vec::<Chunk>::with_capacity((in_file.metadata()?.len() as usize / BLK_SIZE) + 1);
    let mut buf = [0u8; BLK_SIZE];
    loop {
        let read = in_file.read(&mut buf)?;
        if read == 0 {
            break;
        }

        let is_fill = buf.chunks(4).collect::<Vec<&[u8]>>().windows(2).all(|w| w[0] == w[1]);
        if is_fill {
            // The Android Sparse Image Format specifies that a fill block
            // is a four-byte u32 repeated to fill BLK_SIZE. Here we use
            // bincode::deserialize to get the repeated four byte pattern from
            // the buffer so that it can be serialized later when we write
            // the sparse file with bincode::serialize.
            let value: u32 = bincode::deserialize(&buf[0..4])?;
            // Add a fill chunk
            let fill = Chunk::Fill { start: total_read as u64, size: buf.len(), value };
            tracing::trace!("Sparsing file: {}. Created: {}", file_to_upload, fill);
            chunks.push(fill);
        } else {
            // Add a raw chunk
            let raw = Chunk::Raw { start: total_read as u64, size: buf.len() };
            tracing::trace!("Sparsing file: {}. Created: {}", file_to_upload, raw);
            chunks.push(raw);
        }
        total_read += read;
    }

    tracing::trace!("Creating sparse file from: {} chunks", chunks.len());

    // At this point we are making a new sparse file fom an unoptomied set of
    // Chunks. This primarliy means that adjacent Fill chunks of same value are
    // not collapsed into a single Fill chunk (with a larger size). The advantage
    // to this two pass approach is that (with some future work), we can create
    // the "unoptomized" sparse file from a given image, and then "resparse" it
    // as many times as desired with different `max_download_size` parameters.
    // This would simplify the scenario where we want to flash the same image
    // to multiple physical devices which may have slight differences in their
    // hardware (and therefore different `max_download_size`es)
    let sparse_file = SparseFileWriter::new(chunks);
    tracing::trace!("Created sparse file: {}", sparse_file);

    let mut ret = Vec::<TempPath>::new();
    tracing::trace!("Resparsing sparse file");
    for re_sparsed_file in resparse(sparse_file, max_download_size)? {
        let (file, temp_path) = NamedTempFile::new_in(dir)?.into_parts();
        let mut file_create = File::from(file);

        tracing::trace!("Writing resparsed {} to disk", re_sparsed_file);
        re_sparsed_file.write(&mut in_file, &mut file_create)?;

        ret.push(temp_path);
    }

    writeln!(writer, "Finished building sparse files")?;

    Ok(ret)
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use {
        super::{
            add_sparse_chunk,
            builder::{DataSource, SparseImageBuilder},
            resparse, unsparse, Chunk, SparseFileWriter, BLK_SIZE,
        },
        rand::{rngs::SmallRng, RngCore, SeedableRng},
        std::io::{Cursor, Read as _, Seek as _, SeekFrom, Write as _},
        tempfile::{NamedTempFile, TempDir},
    };

    #[test]
    fn test_fill_into_bytes() {
        let mut source = Cursor::new(Vec::<u8>::new());
        let mut dest = Cursor::new(Vec::<u8>::new());

        let fill_chunk = Chunk::Fill { start: 0, size: 5 * BLK_SIZE, value: 365 };
        fill_chunk.write(&mut source, &mut dest).unwrap();
        assert_eq!(dest.into_inner(), [194, 202, 0, 0, 5, 0, 0, 0, 16, 0, 0, 0, 109, 1, 0, 0]);
    }

    #[test]
    fn test_raw_into_bytes() {
        const EXPECTED_RAW_BYTES: [u8; 22] =
            [193, 202, 0, 0, 1, 0, 0, 0, 12, 16, 0, 0, 49, 50, 51, 52, 53, 0, 0, 0, 0, 0];

        let mut source = Cursor::new(Vec::<u8>::from(&b"12345"[..]));
        let mut sparse = Cursor::new(Vec::<u8>::new());
        let chunk = Chunk::Raw { start: 0, size: BLK_SIZE };

        chunk.write(&mut source, &mut sparse).unwrap();
        let buf = sparse.into_inner();
        assert_eq!(buf.len(), 4108);
        assert_eq!(&buf[..EXPECTED_RAW_BYTES.len()], EXPECTED_RAW_BYTES);
        assert_eq!(&buf[EXPECTED_RAW_BYTES.len()..], &[0u8; 4108 - EXPECTED_RAW_BYTES.len()]);
    }

    #[test]
    fn test_dont_care_into_bytes() {
        let mut source = Cursor::new(Vec::<u8>::new());
        let mut dest = Cursor::new(Vec::<u8>::new());
        let chunk = Chunk::DontCare { size: 5 * BLK_SIZE };

        chunk.write(&mut source, &mut dest).unwrap();
        assert_eq!(dest.into_inner(), [195, 202, 0, 0, 5, 0, 0, 0, 12, 0, 0, 0]);
    }

    #[test]
    fn test_sparse_file_into_bytes() {
        let mut source = Cursor::new(Vec::<u8>::from(&b"123"[..]));
        let mut sparse = Cursor::new(Vec::<u8>::new());
        let mut chunks = Vec::<Chunk>::new();
        // Add a fill chunk
        let fill = Chunk::Fill { start: 0, size: 4096, value: 5 };
        chunks.push(fill);
        // Add a raw chunk
        let raw = Chunk::Raw { start: 0, size: 12288 };
        chunks.push(raw);
        // Add a dontcare chunk
        let dontcare = Chunk::DontCare { size: 4096 };
        chunks.push(dontcare);

        let sparsefile = SparseFileWriter::new(chunks);
        sparsefile.write(&mut source, &mut sparse).unwrap();

        sparse.seek(SeekFrom::Start(0)).unwrap();
        let mut unsparsed = Cursor::new(Vec::<u8>::new());
        unsparse(&mut sparse, &mut unsparsed).unwrap();
        let buf = unsparsed.into_inner();
        assert_eq!(buf.len(), 4096 + 12288 + 4096);
        {
            let chunks = buf[..4096].chunks(4);
            for chunk in chunks {
                assert_eq!(chunk, &[5u8, 0, 0, 0]);
            }
        }
        assert_eq!(&buf[4096..4099], b"123");
        assert_eq!(&buf[4099..16384], &[0u8; 12285]);
        assert_eq!(&buf[16384..], &[0u8; 4096]);
    }

    ////////////////////////////////////////////////////////////////////////////
    // Tests for resparse

    #[test]
    fn test_resparse_bails_on_too_small_size() {
        let sparse = SparseFileWriter::new(Vec::<Chunk>::new());
        assert!(resparse(sparse, 4095).is_err());
    }

    #[test]
    fn test_resparse_splits() {
        let max_download_size = 4096 * 2;
        let temp_bytes = Chunk::Raw { start: 0, size: 4096 };

        let mut chunks = Vec::<Chunk>::new();
        chunks.push(temp_bytes.clone());
        chunks.push(Chunk::Fill { start: 4096, size: 4, value: 2 });
        // We want 2 sparse files with the second sparse file having a
        // DontCare chunk and then this chunk
        chunks.push(temp_bytes.clone());

        let input_sparse_file = SparseFileWriter::new(chunks);
        let resparsed_files = resparse(input_sparse_file, max_download_size).unwrap();
        assert_eq!(2, resparsed_files.len());

        // Make assertions about the first resparsed fileDevice
        assert_eq!(3, resparsed_files[0].chunks.len());
        assert_eq!(Chunk::Raw { start: 0, size: 4096 }, resparsed_files[0].chunks[0]);
        assert_eq!(Chunk::Fill { start: 4096, size: 4, value: 2 }, resparsed_files[0].chunks[1]);
        assert_eq!(Chunk::DontCare { size: BLK_SIZE }, resparsed_files[0].chunks[2]);

        // Make assertsion about the second resparsed file
        assert_eq!(2, resparsed_files[1].chunks.len());
        assert_eq!(Chunk::DontCare { size: 2 * BLK_SIZE }, resparsed_files[1].chunks[0]);
        assert_eq!(Chunk::Raw { start: 0, size: 4096 }, resparsed_files[1].chunks[1]);
    }

    ////////////////////////////////////////////////////////////////////////////
    // Tests for add_sparse_chunk

    #[test]
    fn test_add_sparse_chunk_adds_empty() {
        let init_vec = Vec::<Chunk>::new();
        let mut res = init_vec.clone();
        add_sparse_chunk(&mut res, Chunk::Fill { start: 0, size: 4096, value: 1 }).unwrap();
        assert_eq!(0, init_vec.len());
        assert_ne!(init_vec, res);
        assert_eq!(Chunk::Fill { start: 0, size: 4096, value: 1 }, res[0]);
    }

    #[test]
    fn test_add_sparse_chunk_fill() {
        // Test merge
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Fill { start: 0, size: 8192, value: 1 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Fill { start: 0, size: 8192, value: 1 }).unwrap();
            assert_eq!(1, res.len());
            assert_eq!(Chunk::Fill { start: 0, size: 16384, value: 1 }, res[0]);
        }

        // Test dont merge on different value
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Fill { start: 0, size: 4096, value: 1 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Fill { start: 0, size: 4096, value: 2 }).unwrap();
            assert_ne!(res, init_vec);
            assert_eq!(2, res.len());
            assert_eq!(
                res,
                [
                    Chunk::Fill { start: 0, size: 4096, value: 1 },
                    Chunk::Fill { start: 0, size: 4096, value: 2 }
                ]
            );
        }

        // Test dont merge on different type
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Fill { start: 0, size: 4096, value: 2 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::DontCare { size: 4096 }).unwrap();
            assert_ne!(res, init_vec);
            assert_eq!(2, res.len());
            assert_eq!(
                res,
                [Chunk::Fill { start: 0, size: 4096, value: 2 }, Chunk::DontCare { size: 4096 }]
            );
        }
    }

    #[test]
    fn test_add_sparse_chunk_dont_care() {
        // Test they merge
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::DontCare { size: 4096 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::DontCare { size: 4096 }).unwrap();
            assert_eq!(1, res.len());
            assert_eq!(Chunk::DontCare { size: 8192 }, res[0]);
        }

        // Test they dont merge on different type
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::DontCare { size: 4096 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Fill { start: 0, size: 4096, value: 1 }).unwrap();
            assert_eq!(2, res.len());
            assert_eq!(
                res,
                [Chunk::DontCare { size: 4096 }, Chunk::Fill { start: 0, size: 4096, value: 1 }]
            );
        }
    }

    #[test]
    fn test_add_sparse_chunk_raw() {
        // Test they merge
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Raw { start: 0, size: 12288 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Raw { start: 0, size: 16384 }).unwrap();
            assert_eq!(1, res.len());
            assert_eq!(Chunk::Raw { start: 0, size: 28672 }, res[0]);
        }

        // Test they dont merge on different type
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Raw { start: 0, size: 12288 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Fill { start: 3, size: 8192, value: 1 }).unwrap();
            assert_eq!(2, res.len());
            assert_eq!(
                res,
                [
                    Chunk::Raw { start: 0, size: 12288 },
                    Chunk::Fill { start: 3, size: 8192, value: 1 }
                ]
            );
        }
    }

    #[test]
    fn test_add_sparse_chunk_crc32() {
        // Test they dont merge on same type (Crc32 is special)
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Crc32 { checksum: 1234 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Crc32 { checksum: 2345 }).unwrap();
            assert_eq!(2, res.len());
            assert_eq!(res, [Chunk::Crc32 { checksum: 1234 }, Chunk::Crc32 { checksum: 2345 }]);
        }

        // Test they dont merge on different type
        {
            let mut init_vec = Vec::<Chunk>::new();
            init_vec.push(Chunk::Crc32 { checksum: 1234 });
            let mut res = init_vec.clone();
            add_sparse_chunk(&mut res, Chunk::Fill { start: 0, size: 4096, value: 1 }).unwrap();
            assert_eq!(2, res.len());
            assert_eq!(
                res,
                [Chunk::Crc32 { checksum: 1234 }, Chunk::Fill { start: 0, size: 4096, value: 1 }]
            );
        }
    }

    ////////////////////////////////////////////////////////////////////////////
    // Integration
    //

    #[test]
    fn test_roundtrip() {
        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, _temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut buf = Vec::<u8>::new();
        buf.resize(100 * 4096, 0);
        rng.fill_bytes(&mut buf);
        file.write_all(&buf).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let content_size = buf.len();

        // build a sparse file
        let mut sparse_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        SparseImageBuilder::new()
            .add_chunk(DataSource::Buffer(Box::new([0xffu8; 8192])))
            .add_chunk(DataSource::Reader(Box::new(file), content_size as u64))
            .add_chunk(DataSource::Skip(16384))
            .add_chunk(DataSource::Skip(4096))
            .build(&mut sparse_file)
            .expect("Build sparse image failed");
        sparse_file.seek(SeekFrom::Start(0)).unwrap();

        let mut orig_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        unsparse(&mut sparse_file, &mut orig_file).expect("unsparse failed");
        orig_file.seek(SeekFrom::Start(0)).unwrap();

        let mut unsparsed_bytes = vec![];
        orig_file.read_to_end(&mut unsparsed_bytes).expect("Failed to read unsparsed image");
        assert_eq!(unsparsed_bytes.len(), 8192 + 20480 + content_size);
        assert_eq!(&unsparsed_bytes[..8192], &[0xffu8; 8192]);
        assert_eq!(buf.len(), content_size);
        assert_eq!(&unsparsed_bytes[8192..8192 + content_size], &buf[..]);
        assert_eq!(&unsparsed_bytes[8192 + content_size..], &[0u8; 20480]);
    }
}
