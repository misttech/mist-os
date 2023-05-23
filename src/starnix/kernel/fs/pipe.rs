// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::convert::TryInto;
use std::sync::Arc;

use crate::arch::uapi::blksize_t;
use crate::fs::{buffers::*, *};
use crate::lock::Mutex;
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

    fn is_writable(&self) -> bool {
        self.messages.available_capacity() > 0 && self.had_reader
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

        // POSIX requires that a write smaller than PIPE_BUF be atomic, but requires no
        // atomicity for writes larger than this.
        if data.available() <= uapi::PIPE_BUF as usize {
            self.messages.write_datagram(data, None, &mut vec![])
        } else {
            self.messages.write_stream(data, None, &mut vec![])
        }
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

        if self.is_writable() {
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
}
fn pipe_fs(kernel: &Kernel) -> &FileSystemHandle {
    kernel.pipe_fs.get_or_init(|| {
        FileSystem::new(
            kernel,
            CacheMode::Uncached,
            PipeFs,
            FileSystemLabel::without_source("pipe"),
        )
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
        file.blocking_op(
            current_task,
            || {
                let mut pipe = self.pipe.lock();
                let actual = pipe.read(data)?;
                if actual > 0 {
                    pipe.waiters.notify_fd_events(FdEvents::POLLOUT);
                }
                Ok(BlockableOpsResult::Done(actual))
            },
            FdEvents::POLLIN | FdEvents::POLLHUP,
            None,
        )
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(
            current_task,
            || {
                let mut pipe = self.pipe.lock();
                match pipe.write(current_task, data) {
                    Ok(chunk) => {
                        if chunk > 0 {
                            pipe.waiters.notify_fd_events(FdEvents::POLLIN);
                        }
                    }
                    Err(errno) if errno == EPIPE && data.bytes_read() > 0 => {
                        return Ok(BlockableOpsResult::Done(data.bytes_read()))
                    }
                    Err(errno) => return Err(errno),
                };
                if data.available() > 0 {
                    Ok(BlockableOpsResult::Partial(data.bytes_read()))
                } else {
                    Ok(BlockableOpsResult::Done(data.bytes_read()))
                }
            },
            FdEvents::POLLOUT,
            None,
        )
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
