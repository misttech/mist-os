// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Weak};

use starnix_sync::{Locked, Mutex, Unlocked};
use starnix_uapi::errors::Errno;
use starnix_uapi::ownership::{OwnedRef, WeakRef};
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::user_buffer::UserBuffers;
use starnix_uapi::{errno, error, io_event};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;
use zerocopy::IntoBytes;

use crate::task::{CurrentTask, Task};
use crate::vfs::{
    FileHandle, InputBuffer, OutputBuffer, UserBuffersInputBuffer, UserBuffersOutputBuffer,
    VecInputBuffer, VecOutputBuffer, WeakFileHandle,
};

/// From aio.go in gVisor.
pub const AIO_RING_SIZE: usize = 32;

pub struct AioContext {
    // Channels to send operations to background threads.
    //
    // Created lazily together with the thread that consumes operations when the corresponding
    // operation is queued.
    read_sender: OnceLock<Sender<IoOperation>>,
    write_sender: OnceLock<Sender<IoOperation>>,

    // TODO: Refactor code from syscalls.rs into this file to make this field private.
    pub state: Arc<Mutex<AioContextState>>,
}

impl AioContext {
    pub fn new(max_operations: usize) -> Arc<Self> {
        Arc::new(Self {
            read_sender: Default::default(),
            write_sender: Default::default(),
            state: Arc::new(Mutex::new(AioContextState {
                max_operations: max_operations as usize,
                pending_operations: 0,
                results: VecDeque::new(),
            })),
        })
    }

    fn reader(&self, current_task: &CurrentTask) -> &Sender<IoOperation> {
        self.read_sender.get_or_init(|| {
            let (sender, receiver) = channel::<IoOperation>();
            spawn_background_thread(
                current_task,
                Arc::downgrade(&self.state),
                receiver,
                do_read_operation,
            );
            sender
        })
    }

    fn writer(&self, current_task: &CurrentTask) -> &Sender<IoOperation> {
        self.write_sender.get_or_init(|| {
            let (sender, receiver) = channel::<IoOperation>();
            spawn_background_thread(
                current_task,
                Arc::downgrade(&self.state),
                receiver,
                do_write_operation,
            );
            sender
        })
    }

    pub fn queue_op(&self, current_task: &CurrentTask, op: IoOperation) -> Result<(), Errno> {
        // TODO: We should increase the pending_operations count here as part of ensure that
        // there's room in the queue for this operation.
        if !self.state.lock().can_queue() {
            return error!(EAGAIN);
        }
        match op.op_type {
            IoOperationType::Read | IoOperationType::ReadV => {
                self.reader(current_task).send(op).map_err(|_| errno!(EINVAL))
            }
            IoOperationType::Write | IoOperationType::WriteV => {
                self.writer(current_task).send(op).map_err(|_| errno!(EINVAL))
            }
        }
    }
}

impl std::fmt::Debug for AioContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AioContext").finish()
    }
}

impl std::cmp::PartialEq for AioContext {
    fn eq(&self, _other: &AioContext) -> bool {
        false
    }
}

impl std::cmp::Eq for AioContext {}

/// Kernel state-machine-based implementation of asynchronous I/O.
/// See https://man7.org/linux/man-pages/man7/aio.7.html#NOTES
pub struct AioContextState {
    max_operations: usize,

    // TODO: Nothing ever modifies the number of pending operations.
    pending_operations: usize,

    // Return code from async I/O operations.
    // Enqueued from worker threads after an operation is complete.
    results: VecDeque<io_event>,
}

pub enum IoOperationType {
    Read,
    ReadV,
    Write,
    WriteV,
}

pub struct IoOperation {
    pub op_type: IoOperationType,
    pub file: WeakFileHandle,
    pub buffers: UserBuffers,
    pub offset: usize,
    pub id: u64,
    pub iocb_addr: UserAddress,
    pub eventfd: Option<WeakFileHandle>,
}

impl AioContextState {
    fn can_queue(&self) -> bool {
        self.pending_operations < self.max_operations
    }

    pub fn read_available_results(&mut self, max_nr: usize) -> Vec<io_event> {
        let len = std::cmp::min(self.results.len(), max_nr);
        self.results.drain(..len).collect()
    }

    fn queue_result(&mut self, result: io_event) {
        self.results.push_back(result);
    }
}

fn spawn_background_thread<F>(
    current_task: &CurrentTask,
    weak_ctx: Weak<Mutex<AioContextState>>,
    receiver: Receiver<IoOperation>,
    operation_fn: F,
) where
    F: Fn(
            &mut Locked<'_, Unlocked>,
            &CurrentTask,
            WeakRef<Task>,
            FileHandle,
            UserBuffers,
            usize,
        ) -> Result<usize, Errno>
        + Send
        + 'static,
{
    let weak_task = OwnedRef::downgrade(&current_task.task);

    current_task.kernel().kthreads.spawn(move |inner_locked, current_task| {
        while let Ok(op) = receiver.recv() {
            let Some(ctx) = weak_ctx.upgrade() else {
                // The AioContext can be destroyed while async IO operations are ongoing.
                // Terminate the thread when this happens.
                return;
            };

            let Some(file) = op.file.upgrade() else {
                // The FileHandle can close while async IO operations are ongoing.
                // Ignore this operation when this happens.
                continue;
            };

            let res = match operation_fn(
                inner_locked,
                current_task,
                weak_task.clone(),
                file,
                op.buffers,
                op.offset,
            ) {
                Ok(ret) => ret as i64,
                Err(err) => err.return_value() as i64,
            };

            {
                let mut ctx = ctx.lock();
                ctx.queue_result(io_event {
                    data: op.id,
                    obj: op.iocb_addr.into(),
                    res,
                    ..Default::default()
                });
            }

            if let Some(eventfd) = op.eventfd {
                if let Some(eventfd) = eventfd.upgrade() {
                    let mut input_buffer = VecInputBuffer::new(1u64.as_bytes());
                    let _ = eventfd.write(inner_locked, current_task, &mut input_buffer);
                }
            }
        }
    });
}

fn do_read_operation(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    weak_task: WeakRef<Task>,
    file: FileHandle,
    buffers: UserBuffers,
    offset: usize,
) -> Result<usize, Errno> {
    let mut output_buffer = {
        let Some(task) = weak_task.upgrade() else {
            return error!(EINVAL);
        };
        let sink = UserBuffersOutputBuffer::syscall_new(&task, buffers.clone())?;
        VecOutputBuffer::new(sink.available())
    };

    if offset != 0 {
        file.read_at(locked, current_task, offset, &mut output_buffer)?;
    } else {
        file.read(locked, current_task, &mut output_buffer)?;
    }

    let Some(task) = weak_task.upgrade() else {
        return error!(EINVAL);
    };
    let mut sink = UserBuffersOutputBuffer::syscall_new(&task, buffers)?;
    sink.write(&output_buffer.data())
}

fn do_write_operation(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    weak_task: WeakRef<Task>,
    file: FileHandle,
    buffers: UserBuffers,
    offset: usize,
) -> Result<usize, Errno> {
    let mut input_buffer = {
        let Some(task) = weak_task.upgrade() else {
            return error!(EINVAL);
        };
        let mut source = UserBuffersInputBuffer::syscall_new(&task, buffers)?;
        VecInputBuffer::new(&source.read_all()?)
    };

    if offset != 0 {
        file.write_at(locked, current_task, offset, &mut input_buffer)
    } else {
        file.write(locked, current_task, &mut input_buffer)
    }
}
