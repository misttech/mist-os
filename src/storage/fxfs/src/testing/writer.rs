// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::object_handle::{WriteBytes, WriteObjectHandle},
    anyhow::Error,
    async_trait::async_trait,
    storage_device::buffer::Buffer,
};

const BUFFER_SIZE: usize = 131_072;

pub struct Writer<'a> {
    handle: &'a dyn WriteObjectHandle,
    buffer: Buffer<'a>,
    offset: u64,
}

impl<'a> Writer<'a> {
    pub fn new(handle: &'a dyn WriteObjectHandle) -> Self {
        Self { handle, buffer: handle.allocate_buffer(BUFFER_SIZE), offset: 0 }
    }
}

#[async_trait]
impl WriteBytes for Writer<'_> {
    fn handle(&self) -> &dyn WriteObjectHandle {
        self.handle
    }

    async fn write_bytes(&mut self, mut buf: &[u8]) -> Result<(), Error> {
        while buf.len() > 0 {
            let to_do = std::cmp::min(buf.len(), BUFFER_SIZE);
            self.buffer.subslice_mut(..to_do).as_mut_slice().copy_from_slice(&buf[..to_do]);
            self.handle.write_or_append(Some(self.offset), self.buffer.subslice(..to_do)).await?;
            self.offset += to_do as u64;
            buf = &buf[to_do..];
        }
        Ok(())
    }

    async fn complete(&mut self) -> Result<(), Error> {
        self.handle.flush().await
    }

    async fn skip(&mut self, amount: u64) -> Result<(), Error> {
        let mut left = amount as usize;
        self.buffer.subslice_mut(..std::cmp::min(left, BUFFER_SIZE)).as_mut_slice().fill(0);
        while left > 0 {
            let to_do = std::cmp::min(left, BUFFER_SIZE);
            self.handle.write_or_append(Some(self.offset), self.buffer.subslice(..to_do)).await?;
            self.offset += to_do as u64;
            left -= to_do;
        }
        Ok(())
    }
}
