// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::BTreeMap,
    convert::TryInto,
    io::{Read, Seek, SeekFrom},
    mem::size_of_val,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tracing::error;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

#[cfg(target_os = "fuchsia")]
pub use self::fuchsia::*;

#[derive(Error, Debug, PartialEq)]
pub enum ReaderError {
    #[error("Read error at: 0x{:X}", _0)]
    Read(u64),
    #[error("Out of bound read 0x{:X} when size is 0x{:X}", _0, _1)]
    OutOfBounds(u64, u64),
}

pub trait Reader: Send + Sync {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError>;
}

// For simpler usage of Reader trait objects with Parser, we also implement the Reader trait
// for Arc and Box. This allows callers of Parser::new to pass trait objects or real objects
// without having to create custom wrappers or duplicate implementations.

impl Reader for Box<dyn Reader> {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
        self.as_ref().read(offset, data)
    }
}

impl Reader for Arc<dyn Reader> {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
        self.as_ref().read(offset, data)
    }
}

/// IoAdapter wraps any reader that supports std::io::{Read|Seek}.
pub struct IoAdapter<T>(Mutex<T>);

impl<T> IoAdapter<T> {
    pub fn new(inner: T) -> Self {
        Self(Mutex::new(inner))
    }
}

impl<T: Read + Seek + Send + Sync> Reader for IoAdapter<T> {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
        let mut reader = self.0.lock().unwrap();
        reader.seek(SeekFrom::Start(offset)).map_err(|_| ReaderError::Read(offset))?;
        reader.read_exact(data).map_err(|_| ReaderError::Read(offset))
    }
}

pub struct VecReader {
    data: Vec<u8>,
}

impl Reader for VecReader {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
        let data_len = data.len() as u64;
        let self_data_len = self.data.len() as u64;
        let offset_max = offset + data_len;
        if offset_max > self_data_len {
            return Err(ReaderError::OutOfBounds(offset_max, self_data_len));
        }

        let offset_for_range: usize = offset.try_into().unwrap();

        match self.data.get(offset_for_range..offset_for_range + data.len()) {
            Some(slice) => {
                data.clone_from_slice(slice);
                Ok(())
            }
            None => Err(ReaderError::Read(offset)),
        }
    }
}

impl VecReader {
    pub fn new(filesystem: Vec<u8>) -> Self {
        VecReader { data: filesystem }
    }
}

pub struct AndroidSparseReader<R: Reader> {
    inner: R,
    header: SparseHeader,
    chunks: BTreeMap<usize, SparseChunk>,
}

/// Copied from system/core/libsparse/sparse_format.h
#[derive(AsBytes, FromZeroes, FromBytes, Default, Debug)]
#[repr(C)]
struct SparseHeader {
    /// 0xed26ff3a
    magic: u32,
    /// (0x1) - reject images with higher major versions
    major_version: u16,
    /// (0x0) - allow images with higher minor versions
    minor_version: u16,
    /// 28 bytes for first revision of the file format
    file_hdr_sz: u16,
    /// 12 bytes for first revision of the file format
    chunk_hdr_sz: u16,
    /// block size in bytes, must be a multiple of 4 (4096)
    blk_sz: u32,
    /// total blocks in the non-sparse output image
    total_blks: u32,
    /// total chunks in the sparse input image
    total_chunks: u32,
    /// CRC32 checksum of the original data, counting "don't care"
    /// as 0. Standard 802.3 polynomial, use a Public Domain
    /// table implementation
    image_checksum: u32,
}

const SPARSE_HEADER_MAGIC: u32 = 0xed26ff3a;

/// Copied from system/core/libsparse/sparse_format.h
#[derive(AsBytes, FromZeroes, FromBytes, Default)]
#[repr(C)]
struct RawChunkHeader {
    /// 0xCAC1 -> raw; 0xCAC2 -> fill; 0xCAC3 -> don't care
    chunk_type: u16,
    _reserved: u16,
    /// in blocks in output image
    chunk_sz: u32,
    /// in bytes of chunk input file including chunk header and data
    total_sz: u32,
}

#[derive(Debug)]
enum SparseChunk {
    Raw { in_offset: u64, in_size: u32 },
    Fill { fill: [u8; 4] },
    DontCare,
}

const CHUNK_TYPE_RAW: u16 = 0xCAC1;
const CHUNK_TYPE_FILL: u16 = 0xCAC2;
const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;

impl<R: Reader> AndroidSparseReader<R> {
    pub fn new(inner: R) -> Result<Self, anyhow::Error> {
        let mut header = SparseHeader::default();
        inner.read(0, header.as_bytes_mut())?;
        let mut chunks = BTreeMap::new();
        if header.magic == SPARSE_HEADER_MAGIC {
            if header.major_version != 1 {
                anyhow::bail!("unknown sparse image major version {}", header.major_version);
            }
            let mut in_offset = size_of_val(&header) as u64;
            let mut out_offset = 0;
            for _ in 0..header.total_chunks {
                let mut chunk_header = RawChunkHeader::default();
                inner.read(in_offset, chunk_header.as_bytes_mut())?;
                let data_offset = in_offset + size_of_val(&chunk_header) as u64;
                let data_size = chunk_header.total_sz - size_of_val(&chunk_header) as u32;
                in_offset += chunk_header.total_sz as u64;
                let chunk_out_offset = out_offset;
                out_offset += chunk_header.chunk_sz as usize * header.blk_sz as usize;
                let chunk = match chunk_header.chunk_type {
                    CHUNK_TYPE_RAW => {
                        SparseChunk::Raw { in_offset: data_offset, in_size: data_size }
                    }
                    CHUNK_TYPE_FILL => {
                        let mut fill = [0u8; 4];
                        if data_size as usize != size_of_val(&fill) {
                            anyhow::bail!(
                                "fill chunk of sparse image is the wrong size: {}, should be {}",
                                data_size,
                                size_of_val(&fill),
                            );
                        }
                        inner.read(data_offset, fill.as_bytes_mut())?;
                        SparseChunk::Fill { fill }
                    }
                    CHUNK_TYPE_DONT_CARE => SparseChunk::DontCare,
                    e => anyhow::bail!("Invalid chunk type: {:?}", e),
                };
                chunks.insert(chunk_out_offset, chunk);
            }
        }
        Ok(Self { inner, header, chunks })
    }
}

impl<R: Reader> Reader for AndroidSparseReader<R> {
    fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
        let offset_usize = offset as usize;
        if self.header.magic != SPARSE_HEADER_MAGIC {
            return self.inner.read(offset, data);
        }
        let total_size = self.header.total_blks as u64 * self.header.blk_sz as u64;

        let (chunk_start, chunk) = match self.chunks.range(..offset_usize + 1).next_back() {
            Some(x) => x,
            _ => return Err(ReaderError::OutOfBounds(offset, total_size)),
        };
        match chunk {
            SparseChunk::Raw { in_offset, in_size } => {
                let chunk_offset = offset - *chunk_start as u64;
                if chunk_offset > *in_size as u64 {
                    return Err(ReaderError::OutOfBounds(chunk_offset, total_size));
                }
                self.inner.read(*in_offset + chunk_offset, data)?;
            }
            SparseChunk::Fill { fill } => {
                for i in offset_usize..offset_usize + data.len() {
                    data[i - offset_usize] = fill[offset_usize % fill.len()];
                }
            }
            SparseChunk::DontCare => {}
        }
        Ok(())
    }
}

#[cfg(target_os = "fuchsia")]
mod fuchsia {
    use {
        super::{Reader, ReaderError},
        anyhow::Error,
        fidl::endpoints::ClientEnd,
        fidl_fuchsia_hardware_block::BlockMarker,
        fuchsia_zircon as zx,
        remote_block_device::{Cache, RemoteBlockClientSync},
        std::sync::{Arc, Mutex},
        tracing::error,
    };

    pub struct VmoReader {
        vmo: Arc<zx::Vmo>,
    }

    impl Reader for VmoReader {
        fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
            match self.vmo.read(data, offset) {
                Ok(_) => Ok(()),
                Err(zx::Status::OUT_OF_RANGE) => {
                    let size = self.vmo.get_size().map_err(|_| ReaderError::Read(std::u64::MAX))?;
                    Err(ReaderError::OutOfBounds(offset, size))
                }
                Err(_) => Err(ReaderError::Read(offset)),
            }
        }
    }

    impl VmoReader {
        pub fn new(vmo: Arc<zx::Vmo>) -> Self {
            VmoReader { vmo }
        }
    }

    pub struct BlockDeviceReader {
        block_cache: Mutex<Cache>,
    }

    impl Reader for BlockDeviceReader {
        fn read(&self, offset: u64, data: &mut [u8]) -> Result<(), ReaderError> {
            self.block_cache.lock().unwrap().read_at(data, offset).map_err(|e| {
                error!("Encountered error while reading block device: {}", e);
                ReaderError::Read(offset)
            })
        }
    }

    impl BlockDeviceReader {
        pub fn from_client_end(client_end: ClientEnd<BlockMarker>) -> Result<Self, Error> {
            Ok(Self {
                block_cache: Mutex::new(Cache::new(RemoteBlockClientSync::new(client_end)?)?),
            })
        }
    }
}
