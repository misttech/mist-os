// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{
    DesiredAddress, MappingName, MappingOptions, MemoryAccessorExt, ProtectionFlags,
    RemoteMemoryManager, TaskMemoryAccessor,
};
use crate::task::{CurrentTask, KernelThreads, SimpleWaiter, WaitQueue};
use crate::vfs::eventfd::EventFdFileObject;
use crate::vfs::{
    checked_add_offset_and_length, FdNumber, FileHandle, InputBuffer, OutputBuffer,
    UserBuffersInputBuffer, UserBuffersOutputBuffer, VecInputBuffer, VecOutputBuffer,
    WeakFileHandle,
};
use smallvec::smallvec;
use starnix_logging::track_stub;
use starnix_sync::{InterruptibleEvent, Locked, Mutex, Unlocked};
use starnix_syscalls::SyscallResult;
use starnix_types::user_buffer::{UserBuffer, UserBuffers};
use starnix_uapi::errors::{Errno, EINTR, ETIMEDOUT};
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::{
    aio_context_t, errno, error, io_event, iocb, IOCB_CMD_PREAD, IOCB_CMD_PREADV, IOCB_CMD_PWRITE,
    IOCB_CMD_PWRITEV, IOCB_FLAG_RESFD,
};
use std::collections::VecDeque;
use std::sync::Arc;
use zerocopy::IntoBytes;

/// From aio.go in gVisor.
const AIO_RING_SIZE: usize = 32;

/// Kernel state-machine-based implementation of asynchronous I/O.
/// See https://man7.org/linux/man-pages/man7/aio.7.html#NOTES
pub struct AioContext {
    inner: Arc<AioContextInner>,
}

impl AioContext {
    pub fn create(
        current_task: &CurrentTask,
        max_operations: usize,
    ) -> Result<aio_context_t, Errno> {
        let context = Arc::new(AioContext { inner: AioContextInner::new(max_operations) });
        context.inner.spawn_worker(&current_task.kernel().kthreads, WorkerType::Read);
        context.inner.spawn_worker(&current_task.kernel().kthreads, WorkerType::Write);
        let context_addr = current_task.mm().ok_or_else(|| errno!(EINVAL))?.map_anonymous(
            DesiredAddress::Any,
            AIO_RING_SIZE,
            ProtectionFlags::READ | ProtectionFlags::WRITE,
            MappingOptions::ANONYMOUS | MappingOptions::DONT_EXPAND,
            MappingName::AioContext(context),
        )?;
        Ok(context_addr.ptr() as aio_context_t)
    }

    pub fn get_events(
        &self,
        current_task: &CurrentTask,
        min_results: usize,
        max_results: usize,
        deadline: zx::MonotonicInstant,
    ) -> Result<Vec<io_event>, Errno> {
        self.inner.get_events(current_task, min_results, max_results, deadline)
    }

    pub fn submit(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        control_block: iocb,
        iocb_addr: UserAddress,
    ) -> Result<(), Errno> {
        self.inner.submit(current_task, control_block, iocb_addr)
    }
}

impl std::fmt::Debug for AioContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AioContext").finish()
    }
}

impl std::cmp::PartialEq for AioContext {
    fn eq(&self, other: &AioContext) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl std::cmp::Eq for AioContext {}

impl Drop for AioContext {
    fn drop(&mut self) {
        self.inner.stop();
    }
}

struct AioContextInner {
    operations: OperationQueue,
    results: ResultQueue,
}

impl AioContextInner {
    fn new(max_operations: usize) -> Arc<Self> {
        Arc::new(Self {
            operations: OperationQueue::new(max_operations),
            results: Default::default(),
        })
    }

    fn stop(&self) {
        self.operations.stop();
    }

    fn get_events(
        &self,
        current_task: &CurrentTask,
        min_results: usize,
        max_results: usize,
        deadline: zx::MonotonicInstant,
    ) -> Result<Vec<io_event>, Errno> {
        let mut events = self.results.dequeue(max_results);
        if events.len() >= min_results {
            return Ok(events);
        }
        let event = InterruptibleEvent::new();
        loop {
            let (mut waiter, guard) = SimpleWaiter::new(&event);
            self.results.waiters.wait_async_simple(&mut waiter);
            events.extend(self.results.dequeue(max_results - events.len()));
            if events.len() >= min_results {
                return Ok(events);
            }
            match current_task.block_until(guard, deadline) {
                Err(err) if err == ETIMEDOUT => {
                    return Ok(events);
                }
                Err(err) if err == EINTR => {
                    if events.is_empty() {
                        Err(err)
                    } else {
                        return Ok(events);
                    }
                }
                result => result,
            }?;
        }
    }

    fn submit(
        self: &Arc<Self>,
        current_task: &CurrentTask,
        control_block: iocb,
        iocb_addr: UserAddress,
    ) -> Result<(), Errno> {
        let op = IoOperation::new(current_task, control_block, iocb_addr)?;
        self.operations.enqueue(op)
    }

    fn spawn_worker(self: &Arc<Self>, kthreads: &KernelThreads, worker_type: WorkerType) {
        let inner = self.clone();
        kthreads.spawn(move |locked, current_task| {
            inner.perform_next_action(locked, current_task, worker_type)
        });
    }

    fn perform_next_action(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        worker_type: WorkerType,
    ) {
        while let Ok(IoAction::Op(op)) =
            self.operations.block_until_dequeue(current_task, worker_type)
        {
            let Some(result) = op.execute(locked, current_task) else {
                return;
            };
            self.results.enqueue(op.complete(result));

            if let Some(eventfd) = op.eventfd {
                if let Some(eventfd) = eventfd.upgrade() {
                    let mut input_buffer = VecInputBuffer::new(1u64.as_bytes());
                    let _ = eventfd.write(locked, current_task, &mut input_buffer);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkerType {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy)]
enum OpType {
    PRead,
    PReadV,
    // TODO: IOCB_CMD_FSYNC
    // TODO: IOCB_CMD_FDSYNC
    // TODO: IOCB_CMD_POLL
    // TODO: IOCB_CMD_NOOP
    PWrite,
    PWriteV,
}

impl OpType {
    fn worker_type(self) -> WorkerType {
        match self {
            OpType::PRead | OpType::PReadV => WorkerType::Read,
            OpType::PWrite | OpType::PWriteV => WorkerType::Write,
        }
    }
}

impl TryFrom<u32> for OpType {
    type Error = Errno;

    fn try_from(opcode: u32) -> Result<Self, Self::Error> {
        match opcode {
            IOCB_CMD_PREAD => Ok(Self::PRead),
            IOCB_CMD_PREADV => Ok(Self::PReadV),
            IOCB_CMD_PWRITE => Ok(Self::PWrite),
            IOCB_CMD_PWRITEV => Ok(Self::PWriteV),
            _ => {
                track_stub!(TODO("https://fxbug.dev/297433877"), "io_submit opcode", opcode);
                return error!(ENOSYS);
            }
        }
    }
}
struct IoOperation {
    op_type: OpType,
    file: WeakFileHandle,
    mm: RemoteMemoryManager,
    buffers: UserBuffers,
    offset: usize,
    id: u64,
    iocb_addr: UserAddress,
    eventfd: Option<WeakFileHandle>,
}

impl IoOperation {
    fn new(
        current_task: &CurrentTask,
        control_block: iocb,
        iocb_addr: UserAddress,
    ) -> Result<Self, Errno> {
        if control_block.aio_reserved2 != 0 {
            return error!(EINVAL);
        }
        let file = current_task.files.get(FdNumber::from_raw(control_block.aio_fildes as i32))?;
        let op_type = (control_block.aio_lio_opcode as u32).try_into()?;
        let offset = control_block.aio_offset.try_into().map_err(|_| errno!(EINVAL))?;
        let flags = control_block.aio_flags;

        match op_type {
            OpType::PRead | OpType::PReadV => {
                if !file.can_read() {
                    return error!(EBADF);
                }
            }
            OpType::PWrite | OpType::PWriteV => {
                if !file.can_write() {
                    return error!(EBADF);
                }
            }
        }
        let mut buffers = match op_type {
            OpType::PRead | OpType::PWrite => smallvec![UserBuffer {
                address: control_block.aio_buf.into(),
                length: control_block.aio_nbytes as usize,
            }],
            OpType::PReadV | OpType::PWriteV => {
                let count: i32 = control_block.aio_nbytes.try_into().map_err(|_| errno!(EINVAL))?;
                current_task.read_iovec(control_block.aio_buf.into(), count.into())?
            }
        };

        // Validate the user buffers and offset synchronously.
        let buffer_length = UserBuffer::cap_buffers_to_max_rw_count(
            current_task.maximum_valid_address().ok_or_else(|| errno!(EINVAL))?,
            &mut buffers,
        )?;
        checked_add_offset_and_length(offset, buffer_length)?;

        let eventfd = if flags & IOCB_FLAG_RESFD != 0 {
            let eventfd =
                current_task.files.get(FdNumber::from_raw(control_block.aio_resfd as i32))?;
            if eventfd.downcast_file::<EventFdFileObject>().is_none() {
                return error!(EINVAL);
            }
            Some(Arc::downgrade(&eventfd))
        } else {
            None
        };

        Ok(IoOperation {
            op_type,
            file: Arc::downgrade(&file),
            mm: current_task.mm().ok_or_else(|| errno!(EINVAL))?.as_remote(),
            buffers,
            offset,
            id: control_block.aio_data,
            iocb_addr,
            eventfd,
        })
    }

    fn execute(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
    ) -> Option<Result<SyscallResult, Errno>> {
        let Some(file) = self.file.upgrade() else {
            // The FileHandle can close while async IO operations are ongoing.
            // Ignore this operation when this happens.
            return None;
        };

        let result = match self.op_type {
            OpType::PRead | OpType::PReadV => {
                self.do_read(locked, current_task, file).map(Into::into)
            }
            OpType::PWrite | OpType::PWriteV => {
                self.do_write(locked, current_task, file).map(Into::into)
            }
        };
        Some(result)
    }

    fn complete(&self, result: Result<SyscallResult, Errno>) -> io_event {
        let res = match result {
            Ok(return_value) => return_value.value() as i64,
            Err(errno) => errno.return_value() as i64,
        };

        io_event { data: self.id, obj: self.iocb_addr.into(), res, ..Default::default() }
    }

    fn do_read(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        file: FileHandle,
    ) -> Result<usize, Errno> {
        let buffers = self.buffers.clone();
        let mut output_buffer = {
            let sink = UserBuffersOutputBuffer::remote_new(&self.mm, buffers.clone())?;
            VecOutputBuffer::new(sink.available())
        };

        file.read_at(locked, current_task, self.offset, &mut output_buffer)?;

        let mut sink = UserBuffersOutputBuffer::remote_new(&self.mm, buffers)?;
        sink.write(&output_buffer.data())
    }

    fn do_write(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        file: FileHandle,
    ) -> Result<usize, Errno> {
        let mut input_buffer = {
            let mut source = UserBuffersInputBuffer::remote_new(&self.mm, self.buffers.clone())?;
            VecInputBuffer::new(&source.read_all()?)
        };

        file.write_at(locked, current_task, self.offset, &mut input_buffer)
    }
}

enum IoAction {
    Op(IoOperation),
    Stop,
}

#[derive(Default)]
struct PendingOperations {
    is_stopped: bool,
    // We currently queue the read and write operations to separate threads.
    // That behavior is incorrect, but it keeps our clients working well enough while we work on
    // getting the correct parallelism.
    read_ops: VecDeque<IoOperation>,
    write_ops: VecDeque<IoOperation>,
}

impl PendingOperations {
    fn ops_for(&mut self, worker_type: WorkerType) -> &mut VecDeque<IoOperation> {
        match worker_type {
            WorkerType::Read => &mut self.read_ops,
            WorkerType::Write => &mut self.write_ops,
        }
    }

    fn ops_len(&self) -> usize {
        self.read_ops.len() + self.write_ops.len()
    }
}

struct OperationQueue {
    max_operations: usize,
    pending: Mutex<PendingOperations>,
    read_waiters: WaitQueue,
    write_waiters: WaitQueue,
}

impl OperationQueue {
    fn new(max_operations: usize) -> Self {
        Self {
            max_operations,
            pending: Default::default(),
            read_waiters: Default::default(),
            write_waiters: Default::default(),
        }
    }

    fn waiters_for(&self, worker_type: WorkerType) -> &WaitQueue {
        match worker_type {
            WorkerType::Read => &self.read_waiters,
            WorkerType::Write => &self.write_waiters,
        }
    }

    fn enqueue(&self, op: IoOperation) -> Result<(), Errno> {
        let worker_type = op.op_type.worker_type();
        {
            let mut pending = self.pending.lock();
            if pending.is_stopped {
                return error!(EINVAL);
            }
            if pending.ops_len() >= self.max_operations {
                return error!(EAGAIN);
            }
            pending.ops_for(worker_type).push_back(op);
        }
        self.waiters_for(worker_type).notify_unordered_count(1);
        Ok(())
    }

    fn stop(&self) {
        let mut pending = self.pending.lock();
        pending.is_stopped = true;
        pending.read_ops.clear();
        pending.write_ops.clear();
        self.read_waiters.notify_all();
        self.write_waiters.notify_all();
    }

    fn dequeue(&self, worker_type: WorkerType) -> Option<IoAction> {
        let mut pending = self.pending.lock();
        if pending.is_stopped {
            return Some(IoAction::Stop);
        }
        pending.ops_for(worker_type).pop_front().map(IoAction::Op)
    }

    fn block_until_dequeue(
        &self,
        current_task: &CurrentTask,
        worker_type: WorkerType,
    ) -> Result<IoAction, Errno> {
        if let Some(action) = self.dequeue(worker_type) {
            return Ok(action);
        }
        loop {
            let event = InterruptibleEvent::new();
            let (mut waiter, guard) = SimpleWaiter::new(&event);
            self.waiters_for(worker_type).wait_async_simple(&mut waiter);
            if let Some(action) = self.dequeue(worker_type) {
                return Ok(action);
            }
            current_task.block_until(guard, zx::MonotonicInstant::INFINITE)?;
        }
    }
}

#[derive(Default)]
struct ResultQueue {
    waiters: WaitQueue,
    events: Mutex<VecDeque<io_event>>,
}

impl ResultQueue {
    fn enqueue(&self, event: io_event) {
        self.events.lock().push_back(event);
        self.waiters.notify_unordered_count(1);
    }

    fn dequeue(&self, limit: usize) -> Vec<io_event> {
        let mut events = self.events.lock();
        let len = std::cmp::min(events.len(), limit);
        events.drain(..len).collect()
    }
}
