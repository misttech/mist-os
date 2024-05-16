// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{read_to_vec, MemoryAccessorExt, NumberOfElementsRead, PAGE_SIZE},
    signals::{send_standard_signal, SignalInfo},
    task::{CurrentTask, EventHandler, Kernel, WaitCallback, WaitCanceler, WaitQueue, Waiter},
    vfs::{
        buffers::{
            Buffer, InputBuffer, InputBufferCallback, MessageQueue, OutputBuffer,
            OutputBufferCallback, PeekBufferSegmentsCallback,
        },
        default_fcntl, default_ioctl, fileops_impl_nonseekable, CacheMode, FileHandle, FileObject,
        FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNodeInfo, FsStr,
        SpecialNode,
    },
};
use starnix_sync::{FileOpsCore, LockBefore, Locked, Mutex, MutexGuard, Unlocked, WriteOps};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    auth::CAP_SYS_RESOURCE,
    errno, error,
    errors::Errno,
    file_mode::mode,
    open_flags::OpenFlags,
    signals::SIGPIPE,
    statfs, uapi,
    user_address::{UserAddress, UserRef},
    user_buffer::UserBuffer,
    vfs::{default_statfs, FdEvents},
    FIONREAD, F_GETPIPE_SZ, F_SETPIPE_SZ, PIPEFS_MAGIC,
};
use std::{cmp::Ordering, sync::Arc};

const ATOMIC_IO_BYTES: u16 = 4096;

/// The maximum size of a pipe, independent of task capabilities and sysctl limits.
const PIPE_MAX_SIZE: usize = 1 << 31;

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

pub type PipeHandle = Arc<Mutex<Pipe>>;

impl Pipe {
    pub fn new(default_pipe_capacity: usize) -> PipeHandle {
        Arc::new(Mutex::new(Pipe {
            messages: MessageQueue::new(default_pipe_capacity),
            waiters: WaitQueue::default(),
            reader_count: 0,
            had_reader: false,
            writer_count: 0,
            had_writer: false,
        }))
    }

    pub fn open(
        current_task: &CurrentTask,
        pipe: &Arc<Mutex<Self>>,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let mut events = FdEvents::empty();
        let mut pipe_locked = pipe.lock();
        let mut must_wait_events = FdEvents::empty();
        if flags.can_read() {
            if !pipe_locked.had_reader {
                events |= FdEvents::POLLOUT;
            }
            pipe_locked.add_reader();
            if !flags.contains(OpenFlags::NONBLOCK) && !flags.can_write() && !pipe_locked.had_writer
            {
                must_wait_events |= FdEvents::POLLIN;
            }
        }
        if flags.can_write() {
            // https://man7.org/linux/man-pages/man2/open.2.html says:
            //
            //  ENXIO  O_NONBLOCK | O_WRONLY is set, the named file is a FIFO,
            //         and no process has the FIFO open for reading.
            if flags.contains(OpenFlags::NONBLOCK) && pipe_locked.reader_count == 0 {
                assert!(!flags.can_read()); // Otherwise we would have called add_reader() above.
                return error!(ENXIO);
            }
            if !pipe_locked.had_writer {
                events |= FdEvents::POLLIN;
            }
            pipe_locked.add_writer();
            if !flags.contains(OpenFlags::NONBLOCK) && !pipe_locked.had_reader {
                must_wait_events |= FdEvents::POLLOUT;
            }
        }
        if events != FdEvents::empty() {
            pipe_locked.waiters.notify_fd_events(events);
        }
        let ops = PipeFileObject { pipe: Arc::clone(pipe) };
        if must_wait_events == FdEvents::empty() {
            return Ok(Box::new(ops));
        }

        // Ensures that the new PipeFileObject is closed if is it dropped before being returned.
        let ops = scopeguard::guard(ops, |ops| {
            ops.on_close(flags);
        });

        // Wait for the pipe to be connected.
        let waiter = Waiter::new();
        loop {
            pipe_locked.waiters.wait_async_fd_events(
                &waiter,
                must_wait_events,
                WaitCallback::none(),
            );
            std::mem::drop(pipe_locked);
            match waiter.wait(current_task) {
                Err(e) => {
                    return Err(e);
                }
                _ => {}
            }
            pipe_locked = pipe.lock();
            if pipe_locked.had_writer && pipe_locked.had_reader {
                return Ok(Box::new(scopeguard::ScopeGuard::into_inner(ops)));
            }
        }
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

    /// Called whenever a fd to the pipe is closed. Reset the pipe state if there is not more
    /// reader or writer.
    pub fn on_close(&mut self) {
        if self.reader_count == 0 && self.writer_count == 0 {
            self.had_reader = false;
            self.had_writer = false;
            self.messages = MessageQueue::new(self.messages.capacity());
            self.waiters = WaitQueue::default();
        }
    }

    fn capacity(&self) -> usize {
        self.messages.capacity()
    }

    fn set_capacity(
        &mut self,
        task: &CurrentTask,
        mut requested_capacity: usize,
    ) -> Result<(), Errno> {
        if requested_capacity > PIPE_MAX_SIZE {
            return error!(EINVAL);
        }
        if !task.creds().has_capability(CAP_SYS_RESOURCE)
            && requested_capacity
                > task
                    .kernel()
                    .system_limits
                    .pipe_max_size
                    .load(std::sync::atomic::Ordering::Relaxed)
        {
            return error!(EPERM);
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
        let available_capacity = self.messages.available_capacity();
        // POSIX requires that a write smaller than PIPE_BUF be atomic, but requires no
        // atomicity for writes larger than this.
        self.had_reader
            && (available_capacity >= data_size
                || (available_capacity > 0 && data_size > uapi::PIPE_BUF as usize))
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
            send_standard_signal(current_task, SignalInfo::default(SIGPIPE));
            return error!(EPIPE);
        }

        if !self.is_writable(data.available()) {
            return error!(EAGAIN);
        }

        self.messages.write_stream(data, None, &mut vec![])
    }

    fn query_events(&self, flags: OpenFlags) -> FdEvents {
        let mut events = FdEvents::empty();

        if flags.can_read() && self.is_readable() {
            let writer_closed = self.writer_count == 0 && self.had_writer;
            let has_data = !self.messages.is_empty();
            if writer_closed {
                events |= FdEvents::POLLHUP;
            }
            if !writer_closed || has_data {
                events |= FdEvents::POLLIN;
            }
        }

        if flags.can_write() && self.is_writable(1) {
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
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        match cmd {
            F_GETPIPE_SZ => Ok(self.capacity().into()),
            F_SETPIPE_SZ => {
                self.set_capacity(current_task, arg as usize)?;
                Ok(self.capacity().into())
            }
            _ => default_fcntl(cmd),
        }
    }

    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);
        match request {
            FIONREAD => {
                let addr = UserRef::<i32>::new(user_addr);
                let value: i32 = self.messages.len().try_into().map_err(|_| errno!(EINVAL))?;
                current_task.write_object(addr, &value)?;
                Ok(SUCCESS)
            }
            _ => default_ioctl(file, current_task, request, arg),
        }
    }

    fn notify_read(&self) {
        self.waiters.notify_fd_events(FdEvents::POLLOUT);
    }

    fn notify_write(&self) {
        self.waiters.notify_fd_events(FdEvents::POLLIN);
    }

    /// Splice from the `from` pipe to the `to` pipe.
    pub fn splice(from: &mut Pipe, to: &mut Pipe, len: usize) -> Result<usize, Errno> {
        if len == 0 {
            return Ok(0);
        }
        let mut bytes_transferred = 0;
        loop {
            let limit = std::cmp::min(len - bytes_transferred, to.messages.available_capacity());
            if limit == 0 {
                // We no longer want to transfer any bytes.
                break;
            }
            let Some(mut message) = from.messages.read_message() else {
                // The `from` pipe is empty.
                break;
            };
            if let Some(data) = message.data.split_off(limit) {
                // Some data is left in the message. Push it back.
                assert!(data.len() > 0);
                from.messages.write_front(data.into());
            }
            bytes_transferred += message.len();
            to.messages.write_message(message);
        }
        if bytes_transferred > 0 {
            from.notify_read();
            to.notify_write();
        }
        return Ok(bytes_transferred);
    }

    /// Tee from the `from` pipe to the `to` pipe.
    pub fn tee(from: &mut Pipe, to: &mut Pipe, len: usize) -> Result<usize, Errno> {
        if len == 0 {
            return Ok(0);
        }
        let mut bytes_transferred = 0;
        for message in from.messages.peek_queue().iter() {
            let limit = std::cmp::min(len - bytes_transferred, to.messages.available_capacity());
            if limit == 0 {
                break;
            }
            let message = message.clone_at_most(limit);
            bytes_transferred += message.len();
            to.messages.write_message(message);
        }
        if bytes_transferred > 0 {
            to.notify_write();
        }
        return Ok(bytes_transferred);
    }
}

/// Creates a new pipe between the two returned FileObjects.
///
/// The first FileObject is the read endpoint of the pipe. The second is the
/// write endpoint of the pipe. This order matches the order expected by
/// sys_pipe2().
pub fn new_pipe(current_task: &CurrentTask) -> Result<(FileHandle, FileHandle), Errno> {
    let fs = pipe_fs(current_task.kernel());
    let node = fs.create_node(current_task, SpecialNode, |id| {
        let mut info = FsNodeInfo::new(id, mode!(IFIFO, 0o600), current_task.as_fscred());
        info.blksize = ATOMIC_IO_BYTES.into();
        info
    });
    let pipe = node.fifo.as_ref().unwrap();
    {
        let mut state = pipe.lock();
        state.add_reader();
        state.add_writer();
    }

    let open = |flags: OpenFlags| {
        let ops = PipeFileObject { pipe: Arc::clone(pipe) };
        Ok(FileObject::new_anonymous(Box::new(ops), Arc::clone(&node), flags))
    };

    Ok((open(OpenFlags::RDONLY)?, open(OpenFlags::WRONLY)?))
}

struct PipeFs;
impl FileSystemOps for PipeFs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(default_statfs(PIPEFS_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "pipe".into()
    }
}
fn pipe_fs(kernel: &Arc<Kernel>) -> &FileSystemHandle {
    kernel.pipe_fs.get_or_init(|| {
        FileSystem::new(kernel, CacheMode::Uncached, PipeFs, FileSystemOptions::default())
            .expect("pipefs constructed with valid options")
    })
}

pub struct PipeFileObject {
    pipe: Arc<Mutex<Pipe>>,
}

impl FileOps for PipeFileObject {
    fileops_impl_nonseekable!();

    fn close(&self, file: &FileObject, _current_task: &CurrentTask) {
        self.on_close(file.flags());
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, WriteOps>,
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
        file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        mut events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        let flags = file.flags();
        if !flags.can_read() {
            events.remove(FdEvents::POLLIN);
        }
        if !flags.can_write() {
            events.remove(FdEvents::POLLOUT);
        }
        Some(self.pipe.lock().waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(self.pipe.lock().query_events(file.flags()))
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
        _locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.pipe.lock().ioctl(file, current_task, request, arg)
    }
}

/// An OutputBuffer that will write the data to `pipe`.
#[derive(Debug)]
struct SpliceOutputBuffer<'a> {
    pipe: &'a mut Pipe,
    len: usize,
    available: usize,
}

impl<'a> Buffer for SpliceOutputBuffer<'a> {
    fn segments_count(&self) -> Result<usize, Errno> {
        error!(ENOTSUP)
    }

    fn peek_each_segment(
        &mut self,
        _callback: &mut PeekBufferSegmentsCallback<'_>,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }
}

impl<'a> OutputBuffer for SpliceOutputBuffer<'a> {
    fn write_each(&mut self, callback: &mut OutputBufferCallback<'_>) -> Result<usize, Errno> {
        // SAFETY: `callback` returns the number of bytes read on success.
        let bytes = unsafe {
            read_to_vec::<u8, _>(self.available, |buf| callback(buf).map(NumberOfElementsRead))
        }?;
        let bytes_len = bytes.len();
        if bytes_len > 0 {
            self.pipe.messages.write_message(bytes.into());
            self.pipe.notify_write();
            self.available -= bytes_len;
        }
        Ok(bytes_len)
    }

    fn available(&self) -> usize {
        self.available
    }

    fn bytes_written(&self) -> usize {
        self.len - self.available
    }

    fn zero(&mut self) -> Result<usize, Errno> {
        let bytes = vec![0; self.available];
        let len = bytes.len();
        if len > 0 {
            self.pipe.messages.write_message(bytes.into());
            self.pipe.notify_write();
            self.available -= len;
        }
        Ok(len)
    }

    unsafe fn advance(&mut self, _length: usize) -> Result<(), Errno> {
        error!(ENOTSUP)
    }
}

/// An InputBuffer that will read the data from `pipe`.
#[derive(Debug)]
struct SpliceInputBuffer<'a> {
    pipe: &'a mut Pipe,
    len: usize,
    available: usize,
}

impl<'a> Buffer for SpliceInputBuffer<'a> {
    fn segments_count(&self) -> Result<usize, Errno> {
        Ok(self.pipe.messages.len())
    }

    fn peek_each_segment(
        &mut self,
        callback: &mut PeekBufferSegmentsCallback<'_>,
    ) -> Result<(), Errno> {
        let mut available = self.available;
        for message in self.pipe.messages.messages() {
            let to_read = std::cmp::min(available, message.len());
            let buffer = &message.data.bytes()[0..to_read];
            callback(&UserBuffer {
                address: UserAddress::from(buffer.as_ptr() as u64),
                length: buffer.len(),
            });
            available -= to_read;
        }
        Ok(())
    }
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
    /// Called whenever a fd to a pipe is closed.
    fn on_close(&self, flags: OpenFlags) {
        let mut events = FdEvents::empty();
        let mut pipe = self.pipe.lock();
        if flags.can_read() {
            assert!(pipe.reader_count > 0);
            pipe.reader_count -= 1;
            if pipe.reader_count == 0 {
                events |= FdEvents::POLLOUT;
            }
        }
        if flags.can_write() {
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
        pipe.on_close();
    }

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

    /// Splice from the given file handle to this pipe.
    ///
    /// The given file handle must not be a pipe. If you wish to splice between two pipes, use
    /// `lock_pipes` and `Pipe::splice`.
    pub fn splice_from<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        self_file: &FileHandle,
        from: &FileHandle,
        maybe_offset: Option<usize>,
        len: usize,
        non_blocking: bool,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        // If both ends are pipes, use `lock_pipes` and `Pipe::splice`.
        assert!(from.downcast_file::<PipeFileObject>().is_none());

        let mut pipe = self.lock_pipe_for_writing(current_task, self_file, non_blocking, len)?;
        let len = std::cmp::min(len, pipe.messages.available_capacity());
        let mut buffer = SpliceOutputBuffer { pipe: &mut pipe, len, available: len };
        if let Some(offset) = maybe_offset {
            from.read_at(locked, current_task, offset, &mut buffer)
        } else {
            from.read(locked, current_task, &mut buffer)
        }
    }

    /// Splice from this pipe to the given file handle.
    ///
    /// The given file handle must not be a pipe. If you wish to splice between two pipes, use
    /// `lock_pipes` and `Pipe::splice`.
    pub fn splice_to<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        self_file: &FileHandle,
        to: &FileHandle,
        maybe_offset: Option<usize>,
        len: usize,
        non_blocking: bool,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<WriteOps>,
    {
        // If both ends are pipes, use `lock_pipes` and `Pipe::splice`.
        assert!(to.downcast_file::<PipeFileObject>().is_none());

        let mut pipe = self.lock_pipe_for_reading(current_task, self_file, non_blocking)?;
        let len = std::cmp::min(len, pipe.messages.len());
        let mut buffer = SpliceInputBuffer { pipe: &mut pipe, len, available: len };
        if let Some(offset) = maybe_offset {
            to.write_at(locked, current_task, offset, &mut buffer)
        } else {
            to.write(locked, current_task, &mut buffer)
        }
    }

    /// Copy data from the given input buffer into the pipe.
    ///
    /// Returns the number of bytes transferred.
    pub fn vmsplice_from(
        &self,
        current_task: &CurrentTask,
        self_file: &FileHandle,
        data: &mut dyn InputBuffer,
        non_blocking: bool,
    ) -> Result<usize, Errno> {
        let data_len = data.available();
        let mut pipe =
            self.lock_pipe_for_writing(current_task, self_file, non_blocking, data_len)?;

        if pipe.reader_count == 0 {
            send_standard_signal(current_task, SignalInfo::default(SIGPIPE));
            return error!(EPIPE);
        }

        let mut available = std::cmp::min(data_len, pipe.messages.available_capacity());

        let bytes_transferred = data.read_each(&mut |bytes| {
            let actual = std::cmp::min(bytes.len(), available);
            pipe.messages.write_message(bytes[0..actual].to_vec().into());
            available -= actual;
            Ok(actual)
        })?;
        if bytes_transferred > 0 {
            pipe.notify_write();
        }
        Ok(bytes_transferred)
    }

    /// Copy data from the pipe to the given output buffer.
    ///
    /// Returns the number of bytes transferred.
    pub fn vmsplice_to(
        &self,
        current_task: &CurrentTask,
        self_file: &FileHandle,
        data: &mut dyn OutputBuffer,
        non_blocking: bool,
    ) -> Result<usize, Errno> {
        let data_len = data.available();
        let mut pipe = self.lock_pipe_for_reading(current_task, self_file, non_blocking)?;
        let mut available = std::cmp::min(data_len, pipe.messages.len());

        let mut bytes_transferred = 0;
        while let Some(mut message) = pipe.messages.read_message() {
            let requested = std::cmp::min(available, message.data.len());
            let actual = match data.write(&message.data.bytes()[0..requested]) {
                Ok(actual) => actual,
                Err(e) => {
                    pipe.messages.write_front(message);
                    return Err(e);
                }
            };
            assert_eq!(actual, requested);
            if let Some(data) = message.data.split_off(actual) {
                // Some data is left in the message. Push it back.
                pipe.messages.write_front(data.into());
            }
            bytes_transferred += actual;
            available -= actual;
            if available == 0 {
                break;
            }
        }
        if bytes_transferred > 0 {
            pipe.notify_read();
        }
        Ok(bytes_transferred)
    }

    /// Obtain the pipe objects from the given file handles, if they are both pipes.
    ///
    /// Returns EINVAL if one (or both) of the given file handles is not a pipe.
    ///
    /// Obtains the locks on the pipes in the correct order to avoid deadlocks.
    pub fn lock_pipes<'a, 'b>(
        current_task: &CurrentTask,
        file_in: &'a FileHandle,
        file_out: &'b FileHandle,
        len: usize,
        non_blocking: bool,
    ) -> Result<PipeOperands<'a, 'b>, Errno> {
        let pipe_in = file_in.downcast_file::<PipeFileObject>().ok_or_else(|| errno!(EINVAL))?;
        let pipe_out = file_out.downcast_file::<PipeFileObject>().ok_or_else(|| errno!(EINVAL))?;

        let node_cmp =
            Arc::as_ptr(&file_in.name.entry.node).cmp(&Arc::as_ptr(&file_out.name.entry.node));

        match node_cmp {
            Ordering::Equal => error!(EINVAL),
            Ordering::Less => {
                let (write, read) = pipe_in.lock_pipe_for_reading_with(
                    current_task,
                    file_in,
                    || pipe_out.lock_pipe_for_writing(current_task, file_out, non_blocking, len),
                    non_blocking,
                )?;
                Ok(PipeOperands { read, write })
            }
            Ordering::Greater => {
                let (read, write) = pipe_out.lock_pipe_for_writing_with(
                    current_task,
                    file_out,
                    || pipe_in.lock_pipe_for_reading(current_task, file_in, non_blocking),
                    non_blocking,
                    len,
                )?;
                Ok(PipeOperands { read, write })
            }
        }
    }
}

pub struct PipeOperands<'a, 'b> {
    pub read: MutexGuard<'a, Pipe>,
    pub write: MutexGuard<'b, Pipe>,
}
