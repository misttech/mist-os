// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{MemoryAccessor, MemoryManager};
use crate::types::{error, Errno, UserAddress, UserBuffer};

/// The callback for `OutputBuffer::write_each`. The callback is passed the buffers to write to in
/// order, and must return for each, how many bytes has been written.
pub type OutputBufferCallback<'a> = dyn FnMut(&mut [u8]) -> Result<usize, Errno> + 'a;

/// The OutputBuffer allows for writing bytes to a buffer.
pub trait OutputBuffer: std::fmt::Debug {
    /// Calls `callback` for each segment to write data for. `callback` must returns the number of
    /// bytes actually written. When it returns less than the size of the input buffer, the write
    /// is stopped.
    ///
    /// Returns the total number of bytes written.
    fn write_each(&mut self, callback: &mut OutputBufferCallback<'_>) -> Result<usize, Errno>;

    /// Returns the number of bytes available to be written into the buffer.
    fn available(&self) -> usize;

    /// Returns the number of bytes already written into the buffer.
    fn bytes_written(&self) -> usize;

    /// Write the content of `buffer` into this buffer. If this buffer is too small, the write will
    /// be partial.
    ///
    /// Returns the number of bytes written in this buffer.
    fn write(&mut self, mut buffer: &[u8]) -> Result<usize, Errno> {
        self.write_each(&mut move |data| {
            let size = std::cmp::min(buffer.len(), data.len());
            data[0..size].clone_from_slice(&buffer[0..size]);
            buffer = &buffer[size..];
            Ok(size)
        })
    }

    /// Write the content of `buffer` into this buffer. It is an error to pass a buffer larger than
    /// the number of bytes available in this buffer. In that case, the content of the buffer after
    /// the operation is unspecified.
    ///
    /// In case of success, always returns `buffer.len()`.
    fn write_all(&mut self, buffer: &[u8]) -> Result<usize, Errno> {
        let size = self.write(buffer)?;
        if size != buffer.len() {
            error!(EINVAL)
        } else {
            Ok(size)
        }
    }

    /// Write the content of the given `InputBuffer` into this buffer. The number of bytes written
    /// will be the smallest between the number of bytes available in this buffer and in the
    /// `InputBuffer`.
    ///
    /// Returns the number of bytes read and written.
    fn write_buffer(&mut self, input: &mut dyn InputBuffer) -> Result<usize, Errno> {
        self.write_each(&mut move |data| {
            let size = std::cmp::min(data.len(), input.available());
            input.read_exact(&mut data[0..size])
        })
    }
}

/// The callback for `InputBuffer::peek_each` and `InputBuffer::read_each`. The callback is passed
/// the buffers to write to in order, and must return for each, how many bytes has been read.

pub type InputBufferCallback<'a> = dyn FnMut(&[u8]) -> Result<usize, Errno> + 'a;

/// The InputBuffer allows for writing bytes to a buffer.
pub trait InputBuffer: std::fmt::Debug {
    /// Calls `callback` for each segment to peek data from. `callback` must returns the number of
    /// bytes actually peeked. When it returns less than the size of the output buffer, the read
    /// is stopped.
    ///
    /// Returns the total number of bytes peeked.
    fn peek_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno>;

    /// Returns the number of bytes available to be read from the buffer.
    fn available(&self) -> usize;

    /// Returns the number of bytes already read from the buffer.
    fn bytes_read(&self) -> usize;

    /// Clear the remaining content in the buffer. Returns the number of bytes swallowed. After
    /// this method returns, `available()` will returns 0.
    fn drain(&mut self) -> usize;

    /// Consumes `length` bytes of data from this buffer.
    fn advance(&mut self, length: usize) -> Result<(), Errno>;

    /// Calls `callback` for each segment to read data from. `callback` must returns the number of
    /// bytes actually read. When it returns less than the size of the output buffer, the read
    /// is stopped.
    ///
    /// Returns the total number of bytes read.
    fn read_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno> {
        let length = self.peek_each(callback)?;
        self.advance(length)?;
        Ok(length)
    }

    /// Read all the remaining content in this buffer and returns it as a `Vec`.
    fn read_all(&mut self) -> Result<Vec<u8>, Errno> {
        let result = self.peek_all()?;
        let drain_result = self.drain();
        assert!(result.len() == drain_result);
        Ok(result)
    }

    /// Peek all the remaining content in this buffer and returns it as a `Vec`.
    fn peek_all(&mut self) -> Result<Vec<u8>, Errno> {
        let mut result = vec![];
        result.reserve(self.available());
        self.peek_each(&mut |data| {
            result.extend_from_slice(data);
            Ok(data.len())
        })?;
        Ok(result)
    }

    /// Write the content of this buffer into `buffer`.
    /// If `buffer` is too small, the read will be partial.
    /// If `buffer` is too large, the remaining bytes will be left untouched.
    ///
    /// Returns the number of bytes read from this buffer.
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        let mut index = 0;
        self.read_each(&mut move |data| {
            let size = std::cmp::min(buffer.len() - index, data.len());
            buffer[index..index + size].clone_from_slice(&data[0..size]);
            index += size;
            Ok(size)
        })
    }

    /// Read the exact number of bytes required to fill buf.
    ///
    /// If `buffer` is larger than the number of available bytes, an error will be returned.
    ///
    /// In case of success, always returns `buffer.len()`.
    fn read_exact(&mut self, buffer: &mut [u8]) -> Result<usize, Errno> {
        let size = self.read(buffer)?;
        if size != buffer.len() {
            error!(EINVAL)
        } else {
            Ok(size)
        }
    }
}

/// An OutputBuffer that write data to user space memory through a `MemoryManager`.
pub struct UserBuffersOutputBuffer<'a> {
    mm: &'a MemoryManager,
    buffers: Vec<UserBuffer>,
    available: usize,
    bytes_written: usize,
}

impl<'a> std::fmt::Debug for UserBuffersOutputBuffer<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserBuffersOutputBuffer")
            .field("buffers", &self.buffers)
            .field("available", &self.available)
            .field("bytes_written", &self.bytes_written)
            .finish()
    }
}

impl<'a> UserBuffersOutputBuffer<'a> {
    pub fn new(mm: &'a MemoryManager, mut buffers: Vec<UserBuffer>) -> Result<Self, Errno> {
        // Reverse the buffers as the element will be removed as they are handled.
        buffers.reverse();
        let available = UserBuffer::get_total_length(&buffers)?;
        Ok(Self { mm, buffers, available, bytes_written: 0 })
    }

    pub fn new_at(mm: &'a MemoryManager, address: UserAddress, length: usize) -> Self {
        Self {
            mm,
            buffers: vec![UserBuffer { address, length }],
            available: length,
            bytes_written: 0,
        }
    }
}

impl<'a> OutputBuffer for UserBuffersOutputBuffer<'a> {
    fn write_each(&mut self, callback: &mut OutputBufferCallback<'_>) -> Result<usize, Errno> {
        let mut bytes_written = 0;
        while let Some(mut buffer) = self.buffers.pop() {
            if buffer.is_null() {
                continue;
            }
            let mut bytes = vec![0; buffer.length];
            let result = callback(&mut bytes)?;
            if result > buffer.length {
                return error!(EINVAL);
            }
            bytes_written += self.mm.write_memory(buffer.address, &bytes[0..result])?;
            buffer.advance(result)?;
            self.available -= result;
            self.bytes_written += result;
            if !buffer.is_empty() {
                self.buffers.push(buffer);
                break;
            }
        }
        Ok(bytes_written)
    }

    fn available(&self) -> usize {
        self.available
    }
    fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

/// An InputBuffer that read data from user space memory through a `MemoryManager`.
pub struct UserBuffersInputBuffer<'a> {
    mm: &'a MemoryManager,
    buffers: Vec<UserBuffer>,
    available: usize,
    bytes_read: usize,
}

impl<'a> std::fmt::Debug for UserBuffersInputBuffer<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserBuffersOutputBuffer")
            .field("buffers", &self.buffers)
            .field("available", &self.available)
            .field("bytes_read", &self.bytes_read)
            .finish()
    }
}

impl<'a> UserBuffersInputBuffer<'a> {
    pub fn new(mm: &'a MemoryManager, mut buffers: Vec<UserBuffer>) -> Result<Self, Errno> {
        // Reverse the buffers as the element will be removed as they are handled.
        buffers.reverse();
        let available = UserBuffer::get_total_length(&buffers)?;
        Ok(Self { mm, buffers, available, bytes_read: 0 })
    }

    pub fn new_at(mm: &'a MemoryManager, address: UserAddress, length: usize) -> Self {
        Self { mm, buffers: vec![UserBuffer { address, length }], available: length, bytes_read: 0 }
    }
}

impl<'a> InputBuffer for UserBuffersInputBuffer<'a> {
    fn peek_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno> {
        let mut read = 0;
        for buffer in self.buffers.iter().rev() {
            if buffer.is_null() {
                continue;
            }
            let mut bytes = vec![0; buffer.length];
            self.mm.read_memory(buffer.address, &mut bytes)?;
            let result = callback(&bytes)?;
            if result > buffer.length {
                return error!(EINVAL);
            }
            read += result;
            if result != buffer.length {
                break;
            }
        }
        Ok(read)
    }

    fn drain(&mut self) -> usize {
        let result = self.available;
        self.bytes_read += self.available;
        self.available = 0;
        self.buffers.clear();
        result
    }

    fn advance(&mut self, mut length: usize) -> Result<(), Errno> {
        if length > self.available {
            return error!(EINVAL);
        }
        self.available -= length;
        self.bytes_read += length;
        while let Some(mut buffer) = self.buffers.pop() {
            if length < buffer.length {
                buffer.advance(length)?;
                self.buffers.push(buffer);
                return Ok(());
            }
            length -= buffer.length;
            if length == 0 {
                return Ok(());
            }
        }
        if length != 0 {
            error!(EINVAL)
        } else {
            Ok(())
        }
    }

    fn available(&self) -> usize {
        self.available
    }
    fn bytes_read(&self) -> usize {
        self.bytes_read
    }
}

#[cfg(test)]
/// An OutputBuffer that write data to an internal buffer.
#[derive(Debug)]
pub struct VecOutputBuffer {
    buffer: Vec<u8>,
    bytes_written: usize,
}

#[cfg(test)]
impl VecOutputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { buffer: vec![0; capacity], bytes_written: 0 }
    }

    pub fn data(&self) -> &[u8] {
        &self.buffer[0..self.bytes_written]
    }
}

#[cfg(test)]
impl OutputBuffer for VecOutputBuffer {
    fn write_each(&mut self, callback: &mut OutputBufferCallback<'_>) -> Result<usize, Errno> {
        let written = callback(&mut self.buffer.as_mut_slice()[self.bytes_written..])?;
        if self.bytes_written + written > self.buffer.len() {
            return error!(EINVAL);
        }
        self.bytes_written += written;
        Ok(written)
    }

    fn available(&self) -> usize {
        self.buffer.len() - self.bytes_written
    }

    fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

#[cfg(test)]
/// An OutputBuffer that read data from an internal buffer.
#[derive(Debug)]
pub struct VecInputBuffer {
    buffer: Vec<u8>,
    bytes_read: usize,
}

#[cfg(test)]
impl VecInputBuffer {
    pub fn new(buffer: &[u8]) -> Self {
        Self { buffer: buffer.to_vec(), bytes_read: 0 }
    }
}

#[cfg(test)]
impl InputBuffer for VecInputBuffer {
    fn peek_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno> {
        let read = callback(&self.buffer[self.bytes_read..])?;
        if self.bytes_read + read > self.buffer.len() {
            return error!(EINVAL);
        }
        Ok(read)
    }
    fn advance(&mut self, length: usize) -> Result<(), Errno> {
        if length > self.buffer.len() {
            return error!(EINVAL);
        }
        self.bytes_read += length;
        Ok(())
    }
    fn available(&self) -> usize {
        self.buffer.len() - self.bytes_read
    }
    fn bytes_read(&self) -> usize {
        self.bytes_read
    }
    fn drain(&mut self) -> usize {
        let result = self.available();
        self.bytes_read += result;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm::PAGE_SIZE;
    use crate::testing::*;

    #[::fuchsia::test]
    fn test_data_input_buffer() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let page_size = *PAGE_SIZE;
        let addr = map_memory(&current_task, UserAddress::default(), 64 * page_size);

        let data: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        mm.write_memory(addr, &data).expect("failed to write test data");

        let input_iovec = vec![
            UserBuffer { address: addr, length: 25 },
            UserBuffer { address: addr + 64usize, length: 12 },
        ];

        // Test incorrect callback.
        {
            let mut input_buffer = UserBuffersInputBuffer::new(mm, input_iovec.clone())
                .expect("UserBuffersInputBuffer");
            assert!(matches!(input_buffer.peek_each(&mut |data| Ok(data.len() + 1)), Err(_)));
        }

        // Test drain
        {
            let mut input_buffer = UserBuffersInputBuffer::new(mm, input_iovec.clone())
                .expect("UserBuffersInputBuffer");
            assert_eq!(input_buffer.available(), 37);
            assert_eq!(input_buffer.bytes_read(), 0);
            assert_eq!(input_buffer.drain(), 37);
            assert_eq!(input_buffer.available(), 0);
            assert_eq!(input_buffer.bytes_read(), 37);
        }

        // Test read_all
        {
            let mut input_buffer = UserBuffersInputBuffer::new(mm, input_iovec.clone())
                .expect("UserBuffersInputBuffer");
            assert_eq!(input_buffer.available(), 37);
            assert_eq!(input_buffer.bytes_read(), 0);
            let buffer = input_buffer.read_all().expect("read_all");
            assert_eq!(input_buffer.available(), 0);
            assert_eq!(input_buffer.bytes_read(), 37);
            assert_eq!(buffer.len(), 37);
            assert_eq!(&data[..25], &buffer[..25]);
            assert_eq!(&data[64..76], &buffer[25..37]);
        }

        // Test read
        {
            let mut input_buffer =
                UserBuffersInputBuffer::new(mm, input_iovec).expect("UserBuffersInputBuffer");
            let mut buffer = vec![0; 50];
            assert_eq!(input_buffer.available(), 37);
            assert_eq!(input_buffer.bytes_read(), 0);
            assert_eq!(input_buffer.read_exact(&mut buffer[0..20]).expect("read"), 20);
            assert_eq!(input_buffer.available(), 17);
            assert_eq!(input_buffer.bytes_read(), 20);
            assert_eq!(input_buffer.read_exact(&mut buffer[20..37]).expect("read"), 17);
            assert!(matches!(input_buffer.read_exact(&mut buffer[37..]), Err(_)));
            assert_eq!(input_buffer.available(), 0);
            assert_eq!(input_buffer.bytes_read(), 37);
            assert_eq!(&data[..25], &buffer[..25]);
            assert_eq!(&data[64..76], &buffer[25..37]);
        }
    }

    #[::fuchsia::test]
    fn test_data_output_buffer() {
        let (_kernel, current_task) = create_kernel_and_task();
        let mm = &current_task.mm;

        let page_size = *PAGE_SIZE;
        let addr = map_memory(&current_task, UserAddress::default(), 64 * page_size);

        let output_iovec = vec![
            UserBuffer { address: addr, length: 25 },
            UserBuffer { address: addr + 64usize, length: 12 },
        ];

        let data: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();

        // Test incorrect callback.
        {
            let mut output_buffer = UserBuffersOutputBuffer::new(mm, output_iovec.clone())
                .expect("UserBuffersOutputBuffer");
            assert!(matches!(output_buffer.write_each(&mut |data| Ok(data.len() + 1)), Err(_)));
        }

        // Test write
        {
            let mut output_buffer =
                UserBuffersOutputBuffer::new(mm, output_iovec).expect("UserBuffersOutputBuffer");
            assert_eq!(output_buffer.available(), 37);
            assert_eq!(output_buffer.bytes_written(), 0);
            assert_eq!(output_buffer.write_all(&data[0..20]).expect("write"), 20);
            assert_eq!(output_buffer.available(), 17);
            assert_eq!(output_buffer.bytes_written(), 20);
            assert_eq!(output_buffer.write_all(&data[20..37]).expect("write"), 17);
            assert_eq!(output_buffer.available(), 0);
            assert_eq!(output_buffer.bytes_written(), 37);
            assert!(matches!(output_buffer.write_all(&data[37..50]), Err(_)));

            let mut buffer = [0; 128];
            mm.read_memory(addr, &mut buffer).expect("failed to write test data");
            assert_eq!(&data[0..25], &buffer[0..25]);
            assert_eq!(&data[25..37], &buffer[64..76]);
        }
    }

    #[::fuchsia::test]
    fn test_vec_input_buffer() {
        let mut input_buffer = VecInputBuffer::new(b"helloworld");
        assert!(matches!(input_buffer.peek_each(&mut |data| Ok(data.len() + 1)), Err(_)));

        let mut input_buffer = VecInputBuffer::new(b"helloworld");
        assert_eq!(input_buffer.bytes_read(), 0);
        assert_eq!(input_buffer.available(), 10);
        assert_eq!(input_buffer.drain(), 10);
        assert_eq!(input_buffer.bytes_read(), 10);
        assert_eq!(input_buffer.available(), 0);

        let mut input_buffer = VecInputBuffer::new(b"helloworld");
        assert_eq!(input_buffer.bytes_read(), 0);
        assert_eq!(input_buffer.available(), 10);
        assert_eq!(&input_buffer.read_all().expect("read_all"), b"helloworld");
        assert_eq!(input_buffer.bytes_read(), 10);
        assert_eq!(input_buffer.available(), 0);

        let mut input_buffer = VecInputBuffer::new(b"helloworld");
        let mut buffer = [0; 5];
        assert_eq!(input_buffer.read_exact(&mut buffer).expect("read"), 5);
        assert_eq!(input_buffer.bytes_read(), 5);
        assert_eq!(input_buffer.available(), 5);
        assert_eq!(&buffer, b"hello");
        assert_eq!(input_buffer.read_exact(&mut buffer).expect("read"), 5);
        assert_eq!(input_buffer.bytes_read(), 10);
        assert_eq!(input_buffer.available(), 0);
        assert_eq!(&buffer, b"world");
        assert!(matches!(input_buffer.read_exact(&mut buffer), Err(_)));
    }

    #[::fuchsia::test]
    fn test_vec_output_buffer() {
        let mut output_buffer = VecOutputBuffer::new(10);
        assert!(matches!(output_buffer.write_each(&mut |data| Ok(data.len() + 1)), Err(_)));
        assert_eq!(output_buffer.bytes_written(), 0);
        assert_eq!(output_buffer.available(), 10);
        assert_eq!(output_buffer.write_all(b"hello").expect("write"), 5);
        assert_eq!(output_buffer.bytes_written(), 5);
        assert_eq!(output_buffer.available(), 5);
        assert_eq!(output_buffer.data(), b"hello");
        assert_eq!(output_buffer.write_all(b"world").expect("write"), 5);
        assert_eq!(output_buffer.bytes_written(), 10);
        assert_eq!(output_buffer.available(), 0);
        assert_eq!(output_buffer.data(), b"helloworld");
        assert!(matches!(output_buffer.write_all(b"foo"), Err(_)));
    }

    #[::fuchsia::test]
    fn test_vec_write_buffer() {
        let mut input_buffer = VecInputBuffer::new(b"helloworld");
        let mut output_buffer = VecOutputBuffer::new(20);
        assert_eq!(output_buffer.write_buffer(&mut input_buffer).expect("write_buffer"), 10);
        assert_eq!(output_buffer.data(), b"helloworld");
    }
}
