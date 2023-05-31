// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::TryInto;
use std::sync::Arc;

use crate::arch::uapi::blksize_t;
use crate::fs::{buffers::*, *};
use crate::lock::{Mutex, MutexGuard};
use crate::mm::MemoryAccessorExt;
use crate::mm::PAGE_SIZE;
use crate::signals::*;
use crate::syscalls::*;
use crate::task::*;
use crate::types::*;

const ATOMIC_IO_BYTES: blksize_t = 4096;
const PIPE_MAX_SIZE: usize = 1048576; // From pipe.go in gVisor.

fn round_up(value: usize, increment: usize) -> usize {
    (value + (increment - 1)) & !(increment - 1)
}

#[derive(Debug)]
pub struct Pipe {
    messages: MessageQueue,

    waiters: WaitQueue,

    /// The number of open readers.
    reader_count: usize,

    /// Whether the pipe has ever had a reader.
    had_reader: bool,

    /// The number of open writers.
    writer_count: usize,

    /// Whether the pipe has ever had a writer.
    had_writer: bool,
}

impl Default for Pipe {
    fn default() -> Self {
        // The default size of a pipe is 16 pages.
        let default_pipe_capacity = (*PAGE_SIZE * 16) as usize;

        Pipe {
            messages: MessageQueue::new(default_pipe_capacity),
            waiters: WaitQueue::default(),
            reader_count: 0,
            had_reader: false,
            writer_count: 0,
            had_writer: false,
        }
    }
}

pub type PipeHandle = Arc<Mutex<Pipe>>;

impl Pipe {
    pub fn new() -> PipeHandle {
        Arc::new(Mutex::new(Pipe::default()))
    }

    pub fn open(pipe: &Arc<Mutex<Self>>, flags: OpenFlags) -> Box<dyn FileOps> {
        let mut events = FdEvents::empty();
        let mut pipe_locked = pipe.lock();
        if flags.can_read() {
            if !pipe_locked.had_reader {
                events |= FdEvents::POLLOUT;
            }
            pipe_locked.add_reader();
        }
        if flags.can_write() {
            if !pipe_locked.had_writer {
                events |= FdEvents::POLLIN;
            }
            pipe_locked.add_writer();
        }
        if events != FdEvents::empty() {
            pipe_locked.waiters.notify_fd_events(events);
        }
        Box::new(PipeFileObject { pipe: Arc::clone(pipe) })
    }

    /// Increments the reader count for this pipe by 1.
    pub fn add_reader(&mut self) {
        self.reader_count += 1;
        self.had_reader = true;
    }

    /// Increments the writer count for this pipe by 1.
    pub fn add_writer(&mut self) {
        self.writer_count += 1;
        self.had_writer = true;
    }

    fn capacity(&self) -> usize {
        self.messages.capacity()
    }

    fn set_capacity(&mut self, mut requested_capacity: usize) -> Result<(), Errno> {
        if requested_capacity > PIPE_MAX_SIZE {
            return error!(EINVAL);
        }
        let page_size = *PAGE_SIZE as usize;
        if requested_capacity < page_size {
            requested_capacity = page_size;
        }
        requested_capacity = round_up(requested_capacity, page_size);
        self.messages.set_capacity(requested_capacity)
    }

    fn is_readable(&self) -> bool {
        !self.messages.is_empty() || (self.writer_count == 0 && self.had_writer)
    }

    /// Returns whether the pipe can accommodate at least part of a message of length `data_size`.
    fn is_writable(&self, data_size: usize) -> bool {
        // POSIX requires that a write smaller than PIPE_BUF be atomic, but requires no
        // atomicity for writes larger than this.
        self.had_reader
            && (self.messages.available_capacity() >= data_size
                || data_size > uapi::PIPE_BUF as usize)
    }

    pub fn read(&mut self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        // If there isn't any data to read from the pipe, then the behavior
        // depends on whether there are any open writers. If there is an
        // open writer, then we return EAGAIN, to signal that the callers
        // should wait for the writer to write something into the pipe.
        // Otherwise, we'll fall through the rest of this function and
        // return that we have read zero bytes, which will let the caller
        // know that they're done reading the pipe.

        if !self.is_readable() {
            return error!(EAGAIN);
        }

        self.messages.read_stream(data).map(|info| info.bytes_read)
    }

    pub fn write(
        &mut self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if !self.had_reader {
            return error!(EAGAIN);
        }

        if self.reader_count == 0 {
            send_signal(current_task, SignalInfo::default(SIGPIPE));
            return error!(EPIPE);
        }

        if !self.is_writable(data.available()) {
            return error!(EAGAIN);
        }

        self.messages.write_stream(data, None, &mut vec![])
    }

    fn query_events(&self) -> FdEvents {
        let mut events = FdEvents::empty();

        if self.is_readable() {
            let writer_closed = self.writer_count == 0 && self.had_writer;
            let has_data = !self.messages.is_empty();
            if writer_closed {
                events |= FdEvents::POLLHUP;
            }
            if !writer_closed || has_data {
                events |= FdEvents::POLLIN;
            }
        }

        if self.is_writable(1) {
            if self.reader_count == 0 && self.had_reader {
                events |= FdEvents::POLLERR;
            }

            events |= FdEvents::POLLOUT;
        }

        events
    }

    fn fcntl(
        &mut self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        match cmd {
            F_GETPIPE_SZ => Ok(self.capacity().into()),
            F_SETPIPE_SZ => {
                self.set_capacity(arg as usize)?;
                Ok(self.capacity().into())
            }
            _ => default_fcntl(cmd),
        }
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        match request {
            FIONREAD => {
                let addr = UserRef::<i32>::new(user_addr);
                let value: i32 = self.messages.len().try_into().map_err(|_| errno!(EINVAL))?;
                current_task.mm.write_object(addr, &value)?;
                Ok(SUCCESS)
            }
            _ => default_ioctl(request),
        }
    }

    fn notify_read(&self) {
        self.waiters.notify_fd_events(FdEvents::POLLOUT);
    }

    fn notify_write(&self) {
        self.waiters.notify_fd_events(FdEvents::POLLIN);
    }
}

/// Creates a new pipe between the two returned FileObjects.
///
/// The first FileObject is the read endpoint of the pipe. The second is the
/// write endpoint of the pipe. This order matches the order expected by
/// sys_pipe2().
pub fn new_pipe(current_task: &CurrentTask) -> Result<(FileHandle, FileHandle), Errno> {
    let fs = pipe_fs(current_task.kernel());
    let node = fs.create_node(SpecialNode, mode!(IFIFO, 0o600), current_task.as_fscred());
    node.info_write().blksize = ATOMIC_IO_BYTES;

    let open = |flags: OpenFlags| {
        Ok(FileObject::new_anonymous(
            node.open(current_task, flags, false)?,
            Arc::clone(&node),
            flags,
        ))
    };

    Ok((open(OpenFlags::RDONLY)?, open(OpenFlags::WRONLY)?))
}

struct PipeFs;
impl FileSystemOps for PipeFs {
    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        Ok(statfs::default(PIPEFS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        b"pipe"
    }
}
fn pipe_fs(kernel: &Kernel) -> &FileSystemHandle {
    kernel.pipe_fs.get_or_init(|| {
        FileSystem::new(kernel, CacheMode::Uncached, PipeFs, FileSystemOptions::default())
    })
}

pub struct PipeFileObject {
    pipe: Arc<Mutex<Pipe>>,
}

impl FileOps for PipeFileObject {
    fileops_impl_nonseekable!();

    fn close(&self, file: &FileObject) {
        let mut events = FdEvents::empty();
        let mut pipe = self.pipe.lock();
        if file.flags().can_read() {
            assert!(pipe.reader_count > 0);
            pipe.reader_count -= 1;
            if pipe.reader_count == 0 {
                events |= FdEvents::POLLOUT;
            }
        }
        if file.flags().can_write() {
            assert!(pipe.writer_count > 0);
            pipe.writer_count -= 1;
            if pipe.writer_count == 0 {
                if pipe.reader_count > 0 {
                    events |= FdEvents::POLLHUP;
                }
                if !pipe.messages.is_empty() {
                    events |= FdEvents::POLLIN;
                }
            }
        }
        if events != FdEvents::empty() {
            pipe.waiters.notify_fd_events(events);
        }
    }

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, || {
            let mut pipe = self.pipe.lock();
            let actual = pipe.read(data)?;
            if actual > 0 {
                pipe.notify_read();
            }
            Ok(actual)
        })
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        debug_assert!(data.bytes_read() == 0);

        let result = file.blocking_op(current_task, FdEvents::POLLOUT, None, || {
            let mut pipe = self.pipe.lock();
            let offset_before = data.bytes_read();
            let bytes_written = pipe.write(current_task, data)?;
            debug_assert!(data.bytes_read() - offset_before == bytes_written);
            if bytes_written > 0 {
                pipe.notify_write();
            }
            if data.available() > 0 {
                return error!(EAGAIN);
            }
            Ok(())
        });

        let bytes_written = data.bytes_read();
        if bytes_written == 0 {
            // We can only return an error if no data was actually sent. If partial data was
            // sent, swallow the error and return how much was sent.
            result?;
        }
        Ok(bytes_written)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.pipe.lock().waiters.wait_async_events(waiter, events, handler))
    }

    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        self.pipe.lock().query_events()
    }

    fn fcntl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        self.pipe.lock().fcntl(file, current_task, cmd, arg)
    }

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        self.pipe.lock().ioctl(file, current_task, request, user_addr)
    }
}

/// An OutputBuffer that will write the data to `pipe`.
#[derive(Debug)]
struct SpliceOutputBuffer<'a> {
    pipe: &'a mut Pipe,
    len: usize,
    available: usize,
}

impl<'a> OutputBuffer for SpliceOutputBuffer<'a> {
    fn write_each(&mut self, callback: &mut OutputBufferCallback<'_>) -> Result<usize, Errno> {
        let mut bytes = vec![0; self.available];
        let result = callback(&mut bytes)?;
        if result > 0 {
            self.pipe.messages.write_message(bytes.into());
            self.pipe.notify_write();
        }
        Ok(result)
    }

    fn available(&self) -> usize {
        self.available
    }

    fn bytes_written(&self) -> usize {
        self.len - self.available
    }
}

/// An InputBuffer that will read the data from `pipe`.
#[derive(Debug)]
struct SpliceInputBuffer<'a> {
    pipe: &'a mut Pipe,
    len: usize,
    available: usize,
}

impl<'a> InputBuffer for SpliceInputBuffer<'a> {
    fn peek_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno> {
        let mut read = 0;
        let mut available = self.available;
        for message in self.pipe.messages.messages() {
            let to_read = std::cmp::min(available, message.len());
            let result = callback(&message.data.bytes()[0..to_read])?;
            if result > to_read {
                return error!(EINVAL);
            }
            read += result;
            available -= result;
            if result != to_read {
                break;
            }
        }
        Ok(read)
    }

    fn available(&self) -> usize {
        self.available
    }

    fn bytes_read(&self) -> usize {
        self.len - self.available
    }

    fn drain(&mut self) -> usize {
        let result = self.available;
        self.available = 0;
        result
    }

    fn advance(&mut self, mut length: usize) -> Result<(), Errno> {
        if length == 0 {
            return Ok(());
        }
        if length > self.available {
            return error!(EINVAL);
        }
        self.available -= length;
        while let Some(mut message) = self.pipe.messages.read_message() {
            if let Some(data) = message.data.split_off(length) {
                // Some data is left in the message. Push it back.
                self.pipe.messages.write_front(data.into());
            }
            length -= message.len();
            if length == 0 {
                self.pipe.notify_read();
                return Ok(());
            }
        }
        panic!();
    }
}

impl PipeFileObject {
    /// Returns the result of `pregen` and a lock on pipe, once `condition` returns true, ensuring
    /// `pregen` is run before the pipe is locked.
    ///
    /// This will wait on `events` if the file is opened in blocking mode. If the file is opened in
    /// not blocking mode and `condition` is not realized, this will return EAGAIN.
    fn wait_for_condition<'a, F, G, V>(
        &'a self,
        current_task: &CurrentTask,
        file: &FileHandle,
        condition: F,
        pregen: G,
        events: FdEvents,
    ) -> Result<(V, MutexGuard<'a, Pipe>), Errno>
    where
        F: Fn(&Pipe) -> bool,
        G: Fn() -> Result<V, Errno>,
    {
        file.blocking_op(current_task, events, None, || {
            let other = pregen()?;
            let pipe = self.pipe.lock();
            if condition(&pipe) {
                Ok((other, pipe))
            } else {
                error!(EAGAIN)
            }
        })
    }

    /// Lock the pipe for reading, after having run `pregen`.
    fn lock_pipe_for_reading_with<'a, G, V>(
        &'a self,
        current_task: &CurrentTask,
        file: &FileHandle,
        pregen: G,
        non_blocking: bool,
    ) -> Result<(V, MutexGuard<'a, Pipe>), Errno>
    where
        G: Fn() -> Result<V, Errno>,
    {
        if non_blocking {
            let other = pregen()?;
            let pipe = self.pipe.lock();
            if !pipe.is_readable() {
                return error!(EAGAIN);
            }
            Ok((other, pipe))
        } else {
            self.wait_for_condition(
                current_task,
                file,
                |pipe| pipe.is_readable(),
                pregen,
                FdEvents::POLLIN | FdEvents::POLLHUP,
            )
        }
    }

    fn lock_pipe_for_reading<'a>(
        &'a self,
        current_task: &CurrentTask,
        file: &FileHandle,
        non_blocking: bool,
    ) -> Result<MutexGuard<'a, Pipe>, Errno> {
        self.lock_pipe_for_reading_with(current_task, file, || Ok(()), non_blocking).map(|(_, l)| l)
    }

    /// Lock the pipe for writing, after having run `pregen`.
    fn lock_pipe_for_writing_with<'a, G, V>(
        &'a self,
        current_task: &CurrentTask,
        file: &FileHandle,
        pregen: G,
        non_blocking: bool,
        len: usize,
    ) -> Result<(V, MutexGuard<'a, Pipe>), Errno>
    where
        G: Fn() -> Result<V, Errno>,
    {
        if non_blocking {
            let other = pregen()?;
            let pipe = self.pipe.lock();
            if !pipe.is_writable(len) {
                return error!(EAGAIN);
            }
            Ok((other, pipe))
        } else {
            self.wait_for_condition(
                current_task,
                file,
                |pipe| pipe.is_writable(len),
                pregen,
                FdEvents::POLLOUT,
            )
        }
    }

    fn lock_pipe_for_writing<'a>(
        &'a self,
        current_task: &CurrentTask,
        file: &FileHandle,
        non_blocking: bool,
        len: usize,
    ) -> Result<MutexGuard<'a, Pipe>, Errno> {
        self.lock_pipe_for_writing_with(current_task, file, || Ok(()), non_blocking, len)
            .map(|(_, l)| l)
    }

    /// Splice from this pipe to the `to` pipe.
    fn splice_to_pipe(from: &mut Pipe, to: &mut Pipe, len: usize) -> Result<usize, Errno> {
        if len == 0 {
            return Ok(0);
        }
        let len = std::cmp::min(
            len,
            std::cmp::min(from.messages.len(), to.messages.available_capacity()),
        );
        let mut left = len;
        while let Some(mut message) = from.messages.read_message() {
            if let Some(data) = message.data.split_off(left) {
                // Some data is left in the message. Push it back.
                from.messages.write_front(data.into());
            }
            left -= message.len();
            to.messages.write_message(message);
            if left == 0 {
                from.notify_read();
                to.notify_write();
                return Ok(len);
            }
        }
        panic!();
    }

    /// splice from the given file handle to this pipe.
    pub fn splice_from(
        &self,
        current_task: &CurrentTask,
        self_file: &FileHandle,
        from: &FileHandle,
        offset: Option<usize>,
        len: usize,
        non_blocking: bool,
    ) -> Result<usize, Errno> {
        // If both ends are pipes, locks on pipes must be taken such that the lock on this object
        // is always taken before the lock on the other.
        if let Some(pipe_file_object) = from.downcast_file::<PipeFileObject>() {
            let (mut write_pipe, mut read_pipe) = pipe_file_object.lock_pipe_for_reading_with(
                current_task,
                from,
                || self.lock_pipe_for_writing(current_task, self_file, non_blocking, len),
                non_blocking,
            )?;
            return Self::splice_to_pipe(&mut read_pipe, &mut write_pipe, len);
        }

        let mut pipe = self.lock_pipe_for_writing(current_task, self_file, non_blocking, len)?;
        let len = std::cmp::min(len, pipe.messages.available_capacity());
        let mut buffer = SpliceOutputBuffer { pipe: &mut pipe, len, available: len };
        from.read_raw(current_task, offset.unwrap_or(0), &mut buffer)
    }

    pub fn splice_to(
        &self,
        current_task: &CurrentTask,
        self_file: &FileHandle,
        to: &FileHandle,
        offset: Option<usize>,
        len: usize,
        non_blocking: bool,
    ) -> Result<usize, Errno> {
        // If both ends are pipes, locks on pipes must be taken such that the lock on this object
        // is always taken before the lock on the other.
        if let Some(pipe_file_object) = to.downcast_file::<PipeFileObject>() {
            let (mut read_pipe, mut write_pipe) = pipe_file_object.lock_pipe_for_writing_with(
                current_task,
                to,
                || self.lock_pipe_for_reading(current_task, self_file, non_blocking),
                non_blocking,
                len,
            )?;
            return Self::splice_to_pipe(&mut read_pipe, &mut write_pipe, len);
        }
        let mut pipe = self.lock_pipe_for_reading(current_task, self_file, non_blocking)?;

        let len = std::cmp::min(len, pipe.messages.len());
        let mut buffer = SpliceInputBuffer { pipe: &mut pipe, len, available: len };
        to.write_raw(current_task, offset.unwrap_or(0), &mut buffer)
    }
}
