// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::log::*;
use crate::object_handle::{ObjectHandle, ReadObjectHandle};
use crate::object_store::journal::JournalHandle;
use crate::range::RangeExt;
use anyhow::Error;
use async_trait::async_trait;
use std::cmp::min;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage_device::buffer::{BufferFuture, MutableBufferRef};
use storage_device::Device;

// Extents are logically contiguous, so we don't need to store their start offset.
#[derive(Debug)]
struct Extent {
    // Keep track of the offset of the transaction in which the extent was added, which is necessary
    // for discard_extents.
    added_offset: u64,
    device_range: Range<u64>,
}
/// To read the super-block and journal, we use this handle since we cannot use DataObjectHandle
/// until we've replayed the whole journal.  Clients must supply the extents to be used.
pub struct BootstrapObjectHandle {
    object_id: u64,
    device: Arc<dyn Device>,
    start_offset: u64,
    end_offset: u64,
    // A list of extents we know of for the handle; they are logically contiguous from
    // `start_offset`.
    extents: Vec<Extent>,
    trace: AtomicBool,
}

impl BootstrapObjectHandle {
    pub fn new(object_id: u64, device: Arc<dyn Device>) -> Self {
        Self {
            object_id,
            device,
            start_offset: 0,
            end_offset: 0,
            extents: Vec::new(),
            trace: AtomicBool::new(false),
        }
    }

    pub fn new_with_start_offset(
        object_id: u64,
        device: Arc<dyn Device>,
        start_offset: u64,
    ) -> Self {
        Self {
            object_id,
            device,
            start_offset,
            end_offset: start_offset,
            extents: Vec::new(),
            trace: AtomicBool::new(false),
        }
    }
}

impl ObjectHandle for BootstrapObjectHandle {
    fn object_id(&self) -> u64 {
        self.object_id
    }

    fn allocate_buffer(&self, size: usize) -> BufferFuture<'_> {
        self.device.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.device.block_size().into()
    }

    fn set_trace(&self, trace: bool) {
        let old_value = self.trace.swap(trace, Ordering::Relaxed);
        if trace != old_value {
            info!(oid = self.object_id, trace; "JH: trace");
        }
    }
}

#[async_trait]
impl ReadObjectHandle for BootstrapObjectHandle {
    async fn read(&self, mut offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
        assert!(offset >= self.start_offset);
        let trace = self.trace.load(Ordering::Relaxed);
        if trace {
            info!(len = buf.len(), offset; "JH: read");
        }
        let len = buf.len();
        let mut buf_offset = 0;
        let mut file_offset = self.start_offset;
        for extent in &self.extents {
            let device_range = &extent.device_range;
            let extent_len = device_range.end - device_range.start;
            if offset < file_offset + extent_len {
                if trace {
                    info!(device_range:?; "JH: matching extent");
                }
                let device_offset = device_range.start + offset - file_offset;
                let to_read =
                    min(device_range.end - device_offset, (len - buf_offset) as u64) as usize;
                assert!(buf_offset % self.device.block_size() as usize == 0);
                self.device
                    .read(
                        device_offset,
                        buf.reborrow().subslice_mut(buf_offset..buf_offset + to_read),
                    )
                    .await?;
                buf_offset += to_read;
                if buf_offset == len {
                    break;
                }
                offset += to_read as u64;
            }
            file_offset += extent_len;
        }
        Ok(buf_offset)
    }

    fn get_size(&self) -> u64 {
        self.end_offset
    }
}

impl JournalHandle for BootstrapObjectHandle {
    fn end_offset(&self) -> Option<u64> {
        Some(self.end_offset)
    }

    fn push_extent(&mut self, added_offset: u64, device_range: Range<u64>) {
        self.end_offset += device_range.length().unwrap();
        debug_assert!(
            self.extents.last().map_or(true, |e| e.added_offset <= added_offset),
            "last extent added at {}; this added at {added_offset}",
            self.extents.last().unwrap().added_offset
        );
        self.extents.push(Extent { added_offset, device_range });
    }

    fn discard_extents(&mut self, discard_offset: u64) {
        let index = self.extents.partition_point(|extent| extent.added_offset < discard_offset);
        if index == self.extents.len() {
            return;
        }
        let discarded = self.extents.drain(index..);
        let trace = self.trace.load(Ordering::Relaxed);
        for extent in discarded {
            self.end_offset -= extent.device_range.length().unwrap();
            if trace {
                info!(discard_offset, extent:?; "JH: Discarded extent");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BootstrapObjectHandle;
    use crate::object_handle::ReadObjectHandle as _;
    use crate::object_store::journal::JournalHandle as _;
    use std::sync::Arc;
    use storage_device::fake_device::FakeDevice;
    use storage_device::Device as _;

    #[fuchsia::test]
    async fn test_discard_extents() {
        let device = Arc::new(FakeDevice::new(64, 512));
        let mut handle = BootstrapObjectHandle::new(1, device.clone());

        let mut buffer = device.allocate_buffer(1024).await;
        buffer.as_mut_slice().fill(1);
        device.write(0, buffer.as_ref()).await.unwrap();
        buffer.as_mut_slice().fill(2);
        device.write(1024, buffer.as_ref()).await.unwrap();
        buffer.as_mut_slice().fill(0);

        handle.push_extent(0, 1024..2048);
        handle.push_extent(131072, 0..1024);

        assert_eq!(handle.get_size(), 2048);
        handle.read(0, buffer.as_mut()).await.unwrap();
        assert_eq!(buffer.as_slice(), &[2u8; 1024]);
        handle.read(1024, buffer.as_mut()).await.unwrap();
        assert_eq!(buffer.as_slice(), &[1u8; 1024]);

        // Discard at an offset greater than any extent was added, which should be a NOP.
        handle.discard_extents(131073);

        assert_eq!(handle.get_size(), 2048);
        assert_eq!(handle.read(0, buffer.as_mut()).await.unwrap(), 1024);
        assert_eq!(buffer.as_slice(), &[2u8; 1024]);
        assert_eq!(handle.read(1024, buffer.as_mut()).await.unwrap(), 1024);
        assert_eq!(buffer.as_slice(), &[1u8; 1024]);

        // Discard the second extent.
        handle.discard_extents(131072);

        assert_eq!(handle.get_size(), 1024);
        assert_eq!(handle.read(0, buffer.as_mut()).await.unwrap(), 1024);
        assert_eq!(buffer.as_slice(), &[2u8; 1024]);
        assert_eq!(handle.read(1024, buffer.as_mut()).await.unwrap(), 0);
    }
}
