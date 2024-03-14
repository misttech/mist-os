// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        mpsc::{channel, Receiver},
        Arc, Weak,
    },
};

use smallvec::smallvec;
use starnix_sync::{Locked, Mutex, Unlocked};
use starnix_uapi::{
    errno, error, errors::Errno, io_event, ownership::OwnedRef, user_address::UserAddress,
    user_buffer::UserBuffer,
};
use std::sync::mpsc::Sender;
use zerocopy::IntoBytes;

use crate::{
    mm::MemoryManager,
    task::CurrentTask,
    vfs::{
        FileHandle, UserBuffersInputBuffer, UserBuffersOutputBuffer, VecInputBuffer, WeakFileHandle,
    },
};

/// Kernel state-machine-based implementation of asynchronous I/O.
/// See https://man7.org/linux/man-pages/man7/aio.7.html#NOTES
pub struct AioContext {
    // Weak reference to the context to pass to background threads.
    // Background thread will terminate when AioContext is dropped.
    weak_self: Weak<Mutex<Self>>,

    max_operations: usize,
    pending_operations: usize,

    // Return code from async I/O operations.
    // Enqueued from worker threads after an operation is complete.
    results: VecDeque<io_event>,

    // Channels to send operations to background threads.
    // Created lazily together with the thread that consumes operations when the corresponding
    // operation is queued.
    read_sender: Option<Sender<IoOperation>>,
    write_sender: Option<Sender<IoOperation>>,
}

pub enum IoOperationType {
    Read,
    Write,
}

pub struct IoOperation {
    pub op_type: IoOperationType,
    pub file: WeakFileHandle,
    pub buffer: UserBuffer,
    pub offset: usize,
    pub id: u64,
    pub iocb_addr: UserAddress,
    pub eventfd: Option<WeakFileHandle>,
}

impl AioContext {
    pub fn can_queue(&self) -> bool {
        self.pending_operations < self.max_operations
    }

    pub fn read_available_results(&mut self, max_nr: usize) -> Vec<io_event> {
        let len = std::cmp::min(self.results.len(), max_nr);
        self.results.drain(..len).collect()
    }

    pub fn queue_result(&mut self, result: io_event) {
        self.results.push_back(result);
    }

    fn spawn_read_thread(&mut self, current_task: &CurrentTask) {
        let (sender, receiver) = channel::<IoOperation>();
        self.read_sender = Some(sender);
        spawn_background_thread(current_task, self.weak_self.clone(), receiver, do_read_operation);
    }

    fn spawn_write_thread(&mut self, current_task: &CurrentTask) {
        let (sender, receiver) = channel::<IoOperation>();
        self.write_sender = Some(sender);
        spawn_background_thread(current_task, self.weak_self.clone(), receiver, do_write_operation);
    }

    pub fn queue_op(&mut self, current_task: &CurrentTask, op: IoOperation) -> Result<(), Errno> {
        if !self.can_queue() {
            return error!(EAGAIN);
        }
        match op.op_type {
            IoOperationType::Read => {
                if self.read_sender.is_none() {
                    self.spawn_read_thread(current_task);
                }
                self.read_sender
                    .as_ref()
                    .expect("read sender should be initialized")
                    .send(op)
                    .map_err(|_| errno!(EINVAL))
            }
            IoOperationType::Write => {
                if self.write_sender.is_none() {
                    self.spawn_write_thread(current_task);
                }
                self.write_sender
                    .as_ref()
                    .expect("write sender should be initialized")
                    .send(op)
                    .map_err(|_| errno!(EINVAL))
            }
        }
    }
}

fn spawn_background_thread<F>(
    current_task: &CurrentTask,
    weak_ctx: Weak<Mutex<AioContext>>,
    receiver: Receiver<IoOperation>,
    operation_fn: F,
) where
    F: Fn(
            &mut Locked<'_, Unlocked>,
            &CurrentTask,
            Arc<MemoryManager>,
            FileHandle,
            UserBuffer,
            usize,
        ) -> Result<usize, Errno>
        + Send
        + 'static,
{
    let weak_task = OwnedRef::downgrade(&current_task.task);

    current_task.kernel().kthreads.spawn(move |inner_locked, current_task| {
        // Move weak_task into the background thread.
        let weak_task = weak_task;

        while let Ok(op) = receiver.recv() {
            let memory_manager = {
                // Upgraded TempRef<Task> is only used to clone its MemoryManager, and dropped before
                // the blocking operation below.
                let Some(task) = weak_task.upgrade() else {
                    // The calling task can terminate while async IO operations are ongoing.
                    // Terminate the thread when this happens.
                    return;
                };
                task.mm().clone()
            };

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
                memory_manager,
                file,
                op.buffer,
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
    mm: Arc<MemoryManager>,
    file: FileHandle,
    buffer: UserBuffer,
    offset: usize,
) -> Result<usize, Errno> {
    let mut output_buffer =
        UserBuffersOutputBuffer::<MemoryManager>::vmo_new(mm.as_ref(), smallvec![buffer])?;
    if offset != 0 {
        file.read_at(locked, current_task, offset, &mut output_buffer)
    } else {
        file.read(locked, current_task, &mut output_buffer)
    }
}

fn do_write_operation(
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    mm: Arc<MemoryManager>,
    file: FileHandle,
    buffer: UserBuffer,
    offset: usize,
) -> Result<usize, Errno> {
    let mut input_buffer =
        UserBuffersInputBuffer::<MemoryManager>::vmo_new(mm.as_ref(), smallvec![buffer])?;
    if offset != 0 {
        file.write_at(locked, current_task, offset, &mut input_buffer)
    } else {
        file.write(locked, current_task, &mut input_buffer)
    }
}

pub struct AioContexts {
    contexts: HashMap<u64, Arc<Mutex<AioContext>>>,
    next_id: u64,
}

impl Default for AioContexts {
    fn default() -> Self {
        AioContexts { contexts: HashMap::default(), next_id: 1 }
    }
}

impl AioContexts {
    pub fn setup_context(&mut self, max_operations: u32) -> Result<u64, Errno> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or_else(|| errno!(ENOMEM))?;
        self.contexts.insert(
            id,
            Arc::new_cyclic(|weak_self| {
                Mutex::new(AioContext {
                    weak_self: weak_self.clone(),
                    max_operations: max_operations as usize,
                    pending_operations: 0,
                    results: VecDeque::new(),
                    read_sender: None,
                    write_sender: None,
                })
            }),
        );
        Ok(id)
    }

    pub fn get_context(&self, id: u64) -> Option<Arc<Mutex<AioContext>>> {
        self.contexts.get(&id).map(Arc::clone)
    }

    pub fn destroy_context(&mut self, id: u64) -> Result<(), Errno> {
        if let Some(_) = self.contexts.remove(&id) {
            Ok(())
        } else {
            error!(EINVAL)
        }
    }
}
