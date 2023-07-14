// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{format::SPARSE_HEADER_SIZE, Chunk, Reader, SparseHeader, Writer, BLK_SIZE},
    anyhow::Result,
    std::io::{Cursor, SeekFrom},
};

/// Input data for a SparseImageBuilder.
pub enum DataSource {
    Buffer(Box<[u8]>),
    Reader(Box<dyn Reader>, u64),
    /// Skips this many bytes.
    Skip(u64),
    /// Repeats the given u32, this many times.
    Fill(u32, u64),
}

/// Builds sparse image files from a set of input DataSources.
pub struct SparseImageBuilder {
    block_size: u32,
    chunks: Vec<DataSource>,
}

impl SparseImageBuilder {
    pub fn new() -> Self {
        Self { block_size: BLK_SIZE as u32, chunks: vec![] }
    }

    pub fn set_block_size(mut self, block_size: u32) -> Self {
        self.block_size = block_size;
        self
    }

    pub fn add_chunk(mut self, source: DataSource) -> Self {
        self.chunks.push(source);
        self
    }

    #[tracing::instrument(skip(self, output))]
    pub fn build<W: Writer>(self, output: &mut W) -> Result<()> {
        // We'll fill the header in later.
        output.seek(SeekFrom::Start(SPARSE_HEADER_SIZE as u64))?;

        let mut num_blocks = 0;
        let num_chunks = self.chunks.len() as u32;
        let mut current_offset = 0;
        for input_chunk in self.chunks {
            let (chunk, mut source) = match input_chunk {
                DataSource::Buffer(buf) => {
                    assert!(buf.len() % self.block_size as usize == 0);
                    (
                        Chunk::Raw { start: current_offset, size: buf.len() },
                        Some(Box::new(Cursor::new(buf)) as Box<dyn Reader>),
                    )
                }
                DataSource::Reader(reader, size) => {
                    assert!(size % self.block_size as u64 == 0);
                    (Chunk::Raw { start: current_offset, size: size as usize }, Some(reader))
                }
                DataSource::Skip(size) => {
                    assert!(size % self.block_size as u64 == 0);
                    (Chunk::DontCare { start: current_offset, size: size as usize }, None)
                }
                DataSource::Fill(value, count) => {
                    let size = count as usize * std::mem::size_of::<u32>();
                    (Chunk::Fill { start: current_offset, size, value }, None)
                }
            };
            chunk.write(source.as_mut(), output)?;
            num_blocks += chunk.output_blocks(self.block_size as usize) as u32;
            current_offset += chunk.output_size() as u64;
        }

        output.seek(SeekFrom::Start(0))?;
        let header = SparseHeader::new(self.block_size, num_blocks, num_chunks);
        let header_bytes: Vec<u8> = bincode::serialize(&header)?;
        std::io::copy(&mut Cursor::new(header_bytes), output)?;

        output.flush()?;
        Ok(())
    }
}
