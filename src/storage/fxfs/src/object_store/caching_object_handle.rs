// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        data_buffer::MemDataBuffer,
        object_handle::{ObjectHandle, ReadObjectHandle},
        object_store::{DataObjectHandle, HandleOwner},
    },
    anyhow::Error,
    async_trait::async_trait,
    storage_device::buffer::{Buffer, MutableBufferRef},
};

pub struct CachingObjectHandle<S: HandleOwner> {
    handle: DataObjectHandle<S>,
    data: MemDataBuffer,
}

impl<S: HandleOwner> CachingObjectHandle<S> {
    pub fn new(handle: DataObjectHandle<S>) -> Self {
        let size = handle.get_size();
        Self { handle, data: MemDataBuffer::new(size) }
    }
}

impl<S: HandleOwner> ObjectHandle for CachingObjectHandle<S> {
    fn set_trace(&self, v: bool) {
        self.handle.set_trace(v);
    }

    fn object_id(&self) -> u64 {
        self.handle.object_id()
    }

    fn allocate_buffer(&self, size: usize) -> Buffer<'_> {
        self.handle.allocate_buffer(size)
    }

    fn block_size(&self) -> u64 {
        self.handle.block_size()
    }
}

#[async_trait]
impl<S: HandleOwner> ReadObjectHandle for CachingObjectHandle<S> {
    async fn read(&self, offset: u64, mut buf: MutableBufferRef<'_>) -> Result<usize, Error> {
        self.data.read(offset, buf.as_mut_slice(), &self.handle).await
    }

    fn get_size(&self) -> u64 {
        self.data.size()
    }
}
