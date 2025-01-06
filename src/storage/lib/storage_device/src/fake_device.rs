// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::buffer::{BufferFuture, BufferRef, MutableBufferRef};
use crate::buffer_allocator::{BufferAllocator, BufferSource};
use crate::{Device, DeviceHolder};
use anyhow::{ensure, Error};
use async_trait::async_trait;
use block_protocol::WriteOptions;
use rand::Rng;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub enum Op {
    Read,
    Write,
    Flush,
}

/// A Device backed by a memory buffer.
pub struct FakeDevice {
    allocator: BufferAllocator,
    data: Mutex<(/* data: */ Vec<u8>, /* blocks_written_since_last_flush: */ Vec<usize>)>,
    closed: AtomicBool,
    operation_closure: Box<dyn Fn(Op) -> Result<(), Error> + Send + Sync>,
    read_only: AtomicBool,
    poisoned: AtomicBool,
}

const TRANSFER_HEAP_SIZE: usize = 64 * 1024 * 1024;

impl FakeDevice {
    pub fn new(block_count: u64, block_size: u32) -> Self {
        let allocator =
            BufferAllocator::new(block_size as usize, BufferSource::new(TRANSFER_HEAP_SIZE));
        Self {
            allocator,
            data: Mutex::new((
                vec![0 as u8; block_count as usize * block_size as usize],
                Vec::new(),
            )),
            closed: AtomicBool::new(false),
            operation_closure: Box::new(|_: Op| Ok(())),
            read_only: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
        }
    }

    /// Sets a callback that will run at the beginning of read, write, and flush which will forward
    /// any errors, and proceed on Ok().
    pub fn set_op_callback(
        &mut self,
        cb: impl Fn(Op) -> Result<(), Error> + Send + Sync + 'static,
    ) {
        self.operation_closure = Box::new(cb);
    }

    /// Creates a fake block device from an image (which can be anything that implements
    /// std::io::Read).  The size of the device is determined by how much data is read.
    pub fn from_image(
        mut reader: impl std::io::Read,
        block_size: u32,
    ) -> Result<Self, std::io::Error> {
        let allocator =
            BufferAllocator::new(block_size as usize, BufferSource::new(TRANSFER_HEAP_SIZE));
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;
        Ok(Self {
            allocator,
            data: Mutex::new((data, Vec::new())),
            closed: AtomicBool::new(false),
            operation_closure: Box::new(|_| Ok(())),
            read_only: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl Device for FakeDevice {
    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        assert!(!self.closed.load(Ordering::Relaxed));
        self.allocator.allocate_buffer(size)
    }

    fn block_size(&self) -> u32 {
        self.allocator.block_size() as u32
    }

    fn block_count(&self) -> u64 {
        self.data.lock().unwrap().0.len() as u64 / self.block_size() as u64
    }

    async fn read(&self, offset: u64, mut buffer: MutableBufferRef<'_>) -> Result<(), Error> {
        ensure!(!self.closed.load(Ordering::Relaxed));
        (self.operation_closure)(Op::Read)?;
        let offset = offset as usize;
        assert_eq!(offset % self.allocator.block_size(), 0);
        let data = self.data.lock().unwrap();
        let size = buffer.len();
        assert!(
            offset + size <= data.0.len(),
            "offset: {} len: {} data.len: {}",
            offset,
            size,
            data.0.len()
        );
        buffer.as_mut_slice().copy_from_slice(&data.0[offset..offset + size]);
        Ok(())
    }

    async fn write_with_opts(
        &self,
        offset: u64,
        buffer: BufferRef<'_>,
        _opts: WriteOptions,
    ) -> Result<(), Error> {
        ensure!(!self.closed.load(Ordering::Relaxed));
        ensure!(!self.read_only.load(Ordering::Relaxed));
        (self.operation_closure)(Op::Write)?;
        let offset = offset as usize;
        assert_eq!(offset % self.allocator.block_size(), 0);
        let mut data = self.data.lock().unwrap();
        let size = buffer.len();
        assert!(
            offset + size <= data.0.len(),
            "offset: {} len: {} data.len: {}",
            offset,
            size,
            data.0.len()
        );
        data.0[offset..offset + size].copy_from_slice(buffer.as_slice());
        let first_block = offset / self.allocator.block_size();
        for block in first_block..first_block + size / self.allocator.block_size() {
            data.1.push(block)
        }
        Ok(())
    }

    async fn trim(&self, range: Range<u64>) -> Result<(), Error> {
        ensure!(!self.closed.load(Ordering::Relaxed));
        ensure!(!self.read_only.load(Ordering::Relaxed));
        assert_eq!(range.start % self.block_size() as u64, 0);
        assert_eq!(range.end % self.block_size() as u64, 0);
        // Blast over the range to simulate it being used for something else.
        let mut data = self.data.lock().unwrap();
        data.0[range.start as usize..range.end as usize].fill(0xab);
        Ok(())
    }

    async fn close(&self) -> Result<(), Error> {
        self.closed.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn flush(&self) -> Result<(), Error> {
        self.data.lock().unwrap().1.clear();
        (self.operation_closure)(Op::Flush)
    }

    fn reopen(&self, read_only: bool) {
        self.closed.store(false, Ordering::Relaxed);
        self.read_only.store(read_only, Ordering::Relaxed);
    }

    fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::Relaxed)
    }

    fn supports_trim(&self) -> bool {
        true
    }

    fn snapshot(&self) -> Result<DeviceHolder, Error> {
        let allocator =
            BufferAllocator::new(self.block_size() as usize, BufferSource::new(TRANSFER_HEAP_SIZE));
        Ok(DeviceHolder::new(Self {
            allocator,
            data: Mutex::new(self.data.lock().unwrap().clone()),
            closed: AtomicBool::new(false),
            operation_closure: Box::new(|_: Op| Ok(())),
            read_only: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
        }))
    }

    fn discard_random_since_last_flush(&self) -> Result<(), Error> {
        let bs = self.allocator.block_size();
        let mut rng = rand::thread_rng();
        let mut guard = self.data.lock().unwrap();
        let (ref mut data, ref mut blocks_written) = &mut *guard;
        log::info!("Discarding from {blocks_written:?}");
        let mut discarded = Vec::new();
        for block in blocks_written.drain(..) {
            if rng.gen() {
                data[block * bs..(block + 1) * bs].fill(0xaf);
                discarded.push(block);
            }
        }
        log::info!("Discarded {discarded:?}");
        Ok(())
    }

    /// Sets the poisoned state for the device. A poisoned device will panic the thread that
    /// performs Drop on it.
    fn poison(&self) -> Result<(), Error> {
        self.poisoned.store(true, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for FakeDevice {
    fn drop(&mut self) {
        if self.poisoned.load(Ordering::Relaxed) {
            panic!("This device was poisoned to crash whomever is holding a reference here.");
        }
    }
}
