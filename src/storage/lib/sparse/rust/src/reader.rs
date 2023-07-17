// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{deserialize_from, Chunk, Reader, SparseHeader},
    anyhow::{ensure, Context, Result},
    byteorder::{ByteOrder as _, LE},
    std::io::{Read, Seek, SeekFrom},
};

/// SparseReader is an implementation of std::io::Read which transparently unpacks the underlying
/// sparse image as it is read.
/// If random access reads are not required, it is more performant to use `unsparse` to completely
/// unpack a sparse image.
pub struct SparseReader {
    reader: Box<dyn Reader + Send + Sync>,
    // Offset into the logical (unsparsed) image.
    offset: u64,
    // Size of the logical (unsparsed) image.
    size: u64,
    // The second field is the offset into `reader` at which the payload of the chunk appears, for
    // Raw chunks.
    chunks: Vec<(Chunk, Option<u64>)>,
}

impl SparseReader {
    /// Attempts to create a SparseReader from the given image.  Returns failure if the image is
    /// malformed.
    pub fn new(mut reader: Box<dyn Reader + Send + Sync>) -> Result<Self> {
        let header: SparseHeader =
            deserialize_from(&mut reader).context("Failed to read header")?;
        ensure!(header.valid(), "Invalid header");
        let num_chunks = header.total_chunks as usize;

        let mut chunks = vec![];
        let mut offset = 0;
        for _ in 0..num_chunks {
            let chunk = Chunk::read_metadata(&mut reader, offset, header.blk_sz)?;
            let data_offset = if chunk.chunk_type() == crate::format::CHUNK_TYPE_RAW {
                let data_offset = reader.stream_position()?;
                // Skip past the data payload
                reader.seek(SeekFrom::Current(chunk.output_size() as i64))?;
                Some(data_offset)
            } else {
                None
            };
            offset += chunk.output_size() as u64;
            chunks.push((chunk, data_offset));
        }

        reader.seek(SeekFrom::Start(0)).context("Failed to rewind reader")?;
        Ok(Self { reader, offset: 0, size: offset, chunks })
    }

    /// Returns the index of the current chunk in `self.chunks`.
    fn current_chunk(&self) -> Option<usize> {
        let mut off = 0;
        let mut i = 0;
        for (chunk, _) in &self.chunks {
            let size = chunk.output_size() as u64;
            if self.offset >= off && self.offset < off + size {
                return Some(i);
            }
            off += size;
            i += 1;
        }
        None
    }
}

// It's assumed that `reader` already points at the right offset to read from the chunk, and `buf`
// won't read past the end of the chunk.
// `output_offset` is the logical position in the output stream.
fn read_from_chunk<R: Reader>(
    reader: &mut R,
    chunk: &Chunk,
    output_offset: u64,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    match chunk {
        Chunk::Raw { .. } => reader.read(buf),
        Chunk::Fill { value, .. } => {
            let mut value_bytes = value.to_le_bytes();
            value_bytes.rotate_left(output_offset as usize % std::mem::size_of::<u32>());
            let value_rotated = LE::read_u32(&value_bytes);
            // Safety: `std::slice::align_to_mut` requires that everything in the dst slice is a
            // valid type, which is true when going from [u8; 4] to [u32; 1].
            let (prefix, wholes, suffix) = unsafe { buf.align_to_mut::<u32>() };
            prefix.copy_from_slice(&value_bytes[value_bytes.len() - prefix.len()..]);
            wholes.fill(value_rotated);
            suffix.copy_from_slice(&value_bytes[..suffix.len()]);
            Ok(buf.len())
        }
        Chunk::DontCare { .. } => {
            buf.fill(0);
            Ok(buf.len())
        }
        _ => unreachable!(),
    }
}

impl Read for SparseReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut bytes_read = 0;
        while bytes_read < buf.len() {
            let current_chunk_idx = match self.current_chunk() {
                Some(i) => i,
                None => return Ok(bytes_read),
            };
            let (current_chunk, chunk_start_offset) = &self.chunks[current_chunk_idx];
            let offset_in_chunk = self.offset - current_chunk.output_offset().unwrap();
            debug_assert!(offset_in_chunk < current_chunk.output_size() as u64);
            let to_read = std::cmp::min(
                buf.len() - bytes_read,
                current_chunk.output_size() - offset_in_chunk as usize,
            );
            if let Some(offset) = chunk_start_offset {
                self.reader.seek(SeekFrom::Start(*offset + offset_in_chunk))?;
            }
            let bytes_read_from_chunk = read_from_chunk(
                &mut self.reader,
                current_chunk,
                self.offset,
                &mut buf[bytes_read..bytes_read + to_read],
            )?;
            bytes_read += bytes_read_from_chunk;
            self.offset += bytes_read_from_chunk as u64;
        }
        Ok(bytes_read)
    }
}

impl Seek for SparseReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.offset = match pos {
            SeekFrom::Start(pos) => pos,
            SeekFrom::Current(delta) => self
                .offset
                .checked_add_signed(delta)
                .ok_or(std::io::Error::from(std::io::ErrorKind::InvalidInput))?,
            SeekFrom::End(delta) => self
                .size
                .checked_add_signed(delta)
                .ok_or(std::io::Error::from(std::io::ErrorKind::InvalidInput))?,
        };
        Ok(self.offset)
    }
}

#[cfg(test)]
mod test {
    use {
        crate::{
            builder::{DataSource, SparseImageBuilder},
            reader::SparseReader,
        },
        rand::{rngs::SmallRng, RngCore, SeedableRng},
        std::io::{Read as _, Seek as _, SeekFrom, Write as _},
        tempfile::{NamedTempFile, TempDir},
    };

    #[test]
    fn empty_reader() {
        let tmpdir = TempDir::new().unwrap();

        let mut sparse_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        SparseImageBuilder::new().build(&mut sparse_file).expect("Build sparse image failed");
        sparse_file.seek(SeekFrom::Start(0)).unwrap();

        let mut reader =
            SparseReader::new(Box::new(sparse_file)).expect("Failed to create SparseReader");

        let mut unsparsed_bytes = vec![];
        reader.read_to_end(&mut unsparsed_bytes).expect("Failed to read unsparsed image");
        assert_eq!(unsparsed_bytes.len(), 0);
    }

    #[test]
    fn seek() {
        let tmpdir = TempDir::new().unwrap();

        let data = {
            let mut data = Box::new([0u8; 8192]);
            let mut i: u8 = 0;
            for d in data.as_mut() {
                *d = i;
                i = i.wrapping_add(1);
            }
            data
        };

        let mut sparse_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        SparseImageBuilder::new()
            .add_chunk(DataSource::Buffer(data))
            .build(&mut sparse_file)
            .expect("Build sparse image failed");
        sparse_file.seek(SeekFrom::Start(0)).unwrap();
        let mut reader =
            SparseReader::new(Box::new(sparse_file)).expect("Failed to create SparseReader");

        let mut buf = [0u8; 1];
        assert_eq!(0, reader.seek(SeekFrom::Start(0)).unwrap());
        assert_eq!(1, reader.read(&mut buf).unwrap());
        assert_eq!(buf[0], 0u8);

        assert_eq!(100, reader.seek(SeekFrom::Start(100)).unwrap());
        assert_eq!(1, reader.read(&mut buf).unwrap());
        assert_eq!(buf[0], 100u8);

        assert_eq!(99, reader.seek(SeekFrom::Current(-2)).unwrap());
        assert_eq!(1, reader.read(&mut buf).unwrap());
        assert_eq!(buf[0], 99u8);

        assert_eq!(100, reader.seek(SeekFrom::Current(0)).unwrap());
        assert_eq!(1, reader.read(&mut buf).unwrap());
        assert_eq!(buf[0], 100u8);

        assert_eq!(102, reader.seek(SeekFrom::Current(1)).unwrap());
        assert_eq!(1, reader.read(&mut buf).unwrap());
        assert_eq!(buf[0], 102u8);

        assert_eq!(8191, reader.seek(SeekFrom::End(-1)).unwrap());
        assert_eq!(1, reader.read(&mut buf).unwrap());
        assert_eq!(buf[0], 255u8);

        assert_eq!(8192, reader.seek(SeekFrom::End(0)).unwrap());
        assert_eq!(0, reader.read(&mut buf).unwrap());

        assert_eq!(8193, reader.seek(SeekFrom::End(1)).unwrap());
        assert_eq!(0, reader.read(&mut buf).unwrap());
    }

    #[test]
    fn read_past_eof() {
        let tmpdir = TempDir::new().unwrap();

        let mut sparse_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        SparseImageBuilder::new()
            .add_chunk(DataSource::Buffer(Box::new([0xffu8; 8192])))
            .build(&mut sparse_file)
            .expect("Build sparse image failed");
        sparse_file.seek(SeekFrom::Start(0)).unwrap();

        let mut reader =
            SparseReader::new(Box::new(sparse_file)).expect("Failed to create SparseReader");

        let mut buf = [0u8; 2];

        reader.seek(SeekFrom::Start(8191)).expect("Seek failed");
        assert_eq!(reader.read(&mut buf).expect("Failed to read"), 1);

        reader.seek(SeekFrom::Start(8192)).expect("Seek failed");
        assert_eq!(reader.read(&mut buf).expect("Failed to read"), 0);
    }

    #[test]
    fn full_read() {
        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, _temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut data = Vec::<u8>::new();
        data.resize(100 * 4096, 0);
        rng.fill_bytes(&mut data);
        file.write_all(&data).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let content_size = data.len();

        let mut sparse_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        SparseImageBuilder::new()
            .add_chunk(DataSource::Buffer(Box::new([0xffu8; 8192])))
            .add_chunk(DataSource::Reader(Box::new(file)))
            .add_chunk(DataSource::Skip(16384))
            .add_chunk(DataSource::Fill(0xaaaa_aaaau32, 1024))
            .add_chunk(DataSource::Skip(4096))
            .build(&mut sparse_file)
            .expect("Build sparse image failed");
        sparse_file.seek(SeekFrom::Start(0)).unwrap();

        let mut reader =
            SparseReader::new(Box::new(sparse_file)).expect("Failed to create SparseReader");

        let mut unsparsed_bytes = vec![];
        reader.read_to_end(&mut unsparsed_bytes).expect("Failed to read unsparsed image");
        assert_eq!(unsparsed_bytes.len(), 8192 + content_size + 16384 + 4096 + 4096);
        assert_eq!(&unsparsed_bytes[..8192], &[0xffu8; 8192]);
        assert_eq!(&unsparsed_bytes[8192..8192 + content_size], &data[..]);
        assert_eq!(
            &unsparsed_bytes[8192 + content_size..8192 + content_size + 16384],
            &[0u8; 16384]
        );
        assert_eq!(
            &unsparsed_bytes[8192 + content_size + 16384..8192 + content_size + 16384 + 4096],
            &[0xaau8; 4096]
        );
        assert_eq!(&unsparsed_bytes[8192 + content_size + 16384 + 4096..], &[0u8; 4096]);
    }

    #[test]
    fn unaligned_reads() {
        let tmpdir = TempDir::new().unwrap();

        // Generate a large temporary file
        let (mut file, _temp_path) = NamedTempFile::new_in(&tmpdir).unwrap().into_parts();
        let mut rng = SmallRng::from_entropy();
        let mut data = Vec::<u8>::new();
        data.resize(100 * 4096, 0);
        rng.fill_bytes(&mut data);
        file.write_all(&data).unwrap();
        file.flush().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let content_size = data.len();

        let mut sparse_file = NamedTempFile::new_in(&tmpdir).unwrap().into_file();
        SparseImageBuilder::new()
            .add_chunk(DataSource::Buffer(Box::new([0xffu8; 8192])))
            .add_chunk(DataSource::Reader(Box::new(file)))
            .add_chunk(DataSource::Skip(16384))
            .add_chunk(DataSource::Fill(0x0102_0304u32, 1024))
            .add_chunk(DataSource::Skip(4096))
            .build(&mut sparse_file)
            .expect("Build sparse image failed");
        sparse_file.seek(SeekFrom::Start(0)).unwrap();

        let mut reader =
            SparseReader::new(Box::new(sparse_file)).expect("Failed to create SparseReader");

        let mut buffer = [0u8; 4096];

        // Do an unaligned read from each section

        // DataSource::Buffer
        reader.seek(SeekFrom::Start(10)).expect("Failed to seek");
        let _ = reader.read(&mut buffer[..20]).expect("Failed to read");
        assert_eq!(&buffer[..20], &[0xffu8; 20]);

        // DataSource::File
        reader.seek(SeekFrom::Start(8192 + 4095)).expect("Failed to seek");
        let _ = reader.read(&mut buffer[..2]).expect("Failed to read");
        assert_eq!(&buffer[..2], &data[4095..4097]);

        // DataSource::Skip
        reader.seek(SeekFrom::Start(8192 + content_size as u64 + 4090)).expect("Failed to seek");
        let _ = reader.read(&mut buffer[..6]).expect("Failed to read");
        assert_eq!(&buffer[..6], &[0u8; 6]);

        // DataSource::Fill
        reader
            .seek(SeekFrom::Start(8192 + content_size as u64 + 16384 + 3))
            .expect("Failed to seek");
        let _ = reader.read(&mut buffer[..9]).expect("Failed to read");
        // Bear in mind the byte ordering is LE, so 0x01020304 == [0x04, 0x03, 0x02, 0x01]
        assert_eq!(&buffer[..9], &[0x01, 0x04, 0x03, 0x02, 0x01, 0x04, 0x03, 0x02, 0x01]);

        // DataSource::Skip
        reader
            .seek(SeekFrom::Start(8192 + content_size as u64 + 16384 + 4096 + 1))
            .expect("Failed to seek");
        let _ = reader.read(&mut buffer[..4095]).expect("Failed to read");
        assert_eq!(&buffer[..4095], &[0u8; 4095]);

        // Do an unaligned read spanning two sections (the last Fill and Skip)
        reader
            .seek(SeekFrom::Start(8192 + content_size as u64 + 16384 + 4090))
            .expect("Failed to seek");
        let _ = reader.read(&mut buffer[..9]).expect("Failed to read");
        assert_eq!(&buffer[..9], &[0x02, 0x01, 0x04, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00]);
    }
}
