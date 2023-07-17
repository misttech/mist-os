// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    convert::TryInto,
    io::{Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};
use thiserror::Error;
use tracing::error;

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
