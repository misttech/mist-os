// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use crate::device::mem::new_null_file;
use crate::device::remote_binder::RemoteBinderDevice;
use crate::device::DeviceOps;
use crate::fs::fuchsia::new_remote_file;
use crate::mm::memory::MemoryObject;
use crate::mm::{
    DesiredAddress, MappingName, MappingOptions, MemoryAccessor, MemoryAccessorExt, ProtectionFlags,
};
use crate::mutable_state::Guard;
use crate::security;
use crate::task::{
    CurrentTask, EventHandler, Kernel, SchedulerPolicy, SimpleWaiter, Task, WaitCanceler,
    WaitQueue, Waiter,
};
use crate::vfs::buffers::{InputBuffer, OutputBuffer, VecInputBuffer};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    BinderDriverReleaser, CacheMode, CurrentTaskAndLocked, DirectoryEntryType, FdFlags, FdNumber,
    FileHandle, FileObject, FileOps, FileSystem, FileSystemHandle, FileSystemOps,
    FileSystemOptions, FileWriteGuardRef, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    FsString, NamespaceNode, SpecialNode, VecDirectory, VecDirectoryEntry,
};
use bitflags::bitflags;
use fidl::endpoints::ClientEnd;
use fuchsia_inspect_contrib::profile_duration;
use starnix_lifecycle::AtomicU64Counter;
use starnix_logging::{
    log_error, log_trace, log_warn, trace_duration, track_stub, with_zx_name, CATEGORY_STARNIX,
};
use starnix_sync::{
    DeviceOpen, FileOpsCore, InterruptibleEvent, LockBefore, Locked, Mutex, MutexGuard,
    ResourceAccessorAddFile, RwLock, Unlocked,
};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::convert::IntoFidl as _;
use starnix_types::ownership::{
    release_after, release_on_error, DropGuard, OwnedRef, Releasable, ReleaseGuard, Share, TempRef,
    WeakRef,
};
use starnix_types::user_buffer::UserBuffer;
use starnix_types::vfs::default_statfs;
use starnix_uapi::arc_key::ArcKey;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{Errno, EINTR};
use starnix_uapi::file_mode::mode;
use starnix_uapi::math::round_up_to_increment;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::union::struct_with_union_into_bytes;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    binder_buffer_object, binder_driver_command_protocol,
    binder_driver_command_protocol_BC_ACQUIRE, binder_driver_command_protocol_BC_ACQUIRE_DONE,
    binder_driver_command_protocol_BC_CLEAR_DEATH_NOTIFICATION,
    binder_driver_command_protocol_BC_DEAD_BINDER_DONE, binder_driver_command_protocol_BC_DECREFS,
    binder_driver_command_protocol_BC_ENTER_LOOPER, binder_driver_command_protocol_BC_FREE_BUFFER,
    binder_driver_command_protocol_BC_INCREFS, binder_driver_command_protocol_BC_INCREFS_DONE,
    binder_driver_command_protocol_BC_REGISTER_LOOPER, binder_driver_command_protocol_BC_RELEASE,
    binder_driver_command_protocol_BC_REPLY, binder_driver_command_protocol_BC_REPLY_SG,
    binder_driver_command_protocol_BC_REQUEST_DEATH_NOTIFICATION,
    binder_driver_command_protocol_BC_TRANSACTION,
    binder_driver_command_protocol_BC_TRANSACTION_SG, binder_driver_return_protocol,
    binder_driver_return_protocol_BR_ACQUIRE,
    binder_driver_return_protocol_BR_CLEAR_DEATH_NOTIFICATION_DONE,
    binder_driver_return_protocol_BR_DEAD_BINDER, binder_driver_return_protocol_BR_DEAD_REPLY,
    binder_driver_return_protocol_BR_DECREFS, binder_driver_return_protocol_BR_ERROR,
    binder_driver_return_protocol_BR_FAILED_REPLY, binder_driver_return_protocol_BR_INCREFS,
    binder_driver_return_protocol_BR_RELEASE, binder_driver_return_protocol_BR_REPLY,
    binder_driver_return_protocol_BR_SPAWN_LOOPER, binder_driver_return_protocol_BR_TRANSACTION,
    binder_driver_return_protocol_BR_TRANSACTION_COMPLETE,
    binder_driver_return_protocol_BR_TRANSACTION_SEC_CTX, binder_fd_array_object,
    binder_object_header, binder_transaction_data,
    binder_transaction_data__bindgen_ty_2__bindgen_ty_1, binder_transaction_data_sg,
    binder_uintptr_t, binder_version, binder_write_read, errno, errno_from_code, error,
    flat_binder_object, pid_t, statfs, transaction_flags_TF_ONE_WAY, uapi, BINDERFS_SUPER_MAGIC,
    BINDER_BUFFER_FLAG_HAS_PARENT, BINDER_CURRENT_PROTOCOL_VERSION, BINDER_TYPE_BINDER,
    BINDER_TYPE_FD, BINDER_TYPE_FDA, BINDER_TYPE_HANDLE, BINDER_TYPE_PTR,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes};
use {fidl_fuchsia_posix as fposix, fidl_fuchsia_starnix_binder as fbinder};

// The name used to track the duration of a local binder ioctl.
const NAME_BINDER_IOCTL: &'static CStr = c"binder_ioctl";

/// Allows for sequential reading of a task's userspace memory.
pub struct UserMemoryCursor {
    buffer: VecInputBuffer,
}

impl UserMemoryCursor {
    /// Create a new [`UserMemoryCursor`] starting at userspace address `addr` of length `len`.
    /// Upon creation, the cursor reads the entire user buffer then caches it.
    /// Any reads past `addr + len` will fail with `EINVAL`.
    pub fn new(ma: &dyn MemoryAccessor, addr: UserAddress, len: u64) -> Result<Self, Errno> {
        let buffer = ma.read_buffer(&UserBuffer { address: addr, length: len as usize })?;
        Ok(Self { buffer: buffer.into() })
    }

    /// Read an object from userspace memory and increment the read position.
    pub fn read_object<T: FromBytes>(&mut self) -> Result<T, Errno> {
        self.buffer.read_object::<T>()
    }

    /// The total number of bytes read.
    pub fn bytes_read(&self) -> usize {
        self.buffer.bytes_read()
    }
}

pub type BinderDevice = Arc<BinderDriverReleaser>;

impl DeviceOps for BinderDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let identifier = self.create_local_process(current_task.get_pid());
        log_trace!("opened new BinderConnection id={}", identifier);
        Ok(Box::new(BinderConnection { identifier, device: self.clone() }))
    }
}

/// An instance of the binder driver, associated with the process that opened the binder device.
#[derive(Debug)]
struct BinderConnection {
    /// The process that opened the binder device.
    identifier: u64,
    /// The implementation of the binder driver.
    device: BinderDevice,
}

impl BinderConnection {
    fn proc(&self, current_task: &CurrentTask) -> Result<OwnedRef<BinderProcess>, Errno> {
        let process = self.device.find_process(self.identifier)?;
        if process.pid == current_task.get_pid() {
            Ok(process)
        } else {
            process.release(current_task.kernel());
            error!(EINVAL)
        }
    }

    pub fn interrupt(&self) {
        log_trace!("interrupting BinderConnection id={}", self.identifier);
        if let Some(binder_process) = self.device.procs.read().get(&self.identifier) {
            binder_process.interrupt();
        }
    }

    fn close(&self, kernel: &Kernel) {
        log_trace!("closing BinderConnection id={}", self.identifier);
        if let Some(binder_process) = self.device.procs.write().remove(&self.identifier) {
            binder_process.close();
            binder_process.release(kernel);
        }
    }
}

impl FileOps for BinderConnection {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        self.close(current_task.kernel());
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let binder_process = self.proc(current_task);
        release_after!(binder_process, current_task.kernel(), {
            Ok(match &binder_process {
                Ok(binder_process) => {
                    let binder_thread =
                        binder_process.lock().find_or_register_thread(current_task.get_tid());
                    release_after!(binder_thread, current_task.kernel(), {
                        let mut thread_state = binder_thread.lock();
                        let mut process_command_queue = binder_process.command_queue.lock();
                        BinderDriver::get_active_queue(
                            &mut thread_state,
                            &mut process_command_queue,
                        )
                        .query_events()
                    })
                }
                Err(_) => FdEvents::POLLERR,
            })
        })
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        log_trace!("binder wait_async");
        let binder_process = self.proc(current_task);
        release_after!(binder_process, current_task.kernel(), {
            match &binder_process {
                Ok(binder_process) => {
                    let binder_thread =
                        binder_process.lock().find_or_register_thread(current_task.get_tid());
                    release_after!(binder_thread, current_task.kernel(), {
                        Some(self.device.wait_async(
                            &binder_process,
                            &binder_thread,
                            waiter,
                            events,
                            handler,
                        ))
                    })
                }
                Err(_) => {
                    handler.handle(FdEvents::POLLERR);
                    Some(waiter.fake_wait())
                }
            }
        })
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let binder_process = self.proc(current_task)?;
        release_after!(binder_process, current_task.kernel(), {
            self.device.ioctl(locked, current_task, &binder_process, request, arg)
        })
    }

    fn get_memory(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        error!(EINVAL)
    }

    fn mmap(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        _memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        mapping_options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        let binder_process = self.proc(current_task)?;
        release_after!(binder_process, current_task.kernel(), {
            self.device.mmap(
                current_task,
                &binder_process,
                addr,
                length,
                prot_flags,
                mapping_options,
                filename,
            )
        })
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EOPNOTSUPP)
    }

    fn flush(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        // Errors are not meaningful on flush.
        let Ok(binder_process) = self.proc(current_task) else { return };
        release_after!(binder_process, current_task.kernel(), {
            binder_process.kick_all_threads()
        });
    }
}

/// A connection to a binder driver from a remote process.
#[derive(Debug)]
pub struct RemoteBinderConnection {
    binder_connection: BinderConnection,
}

impl RemoteBinderConnection {
    pub fn map_external_vmo(
        &self,
        current_task: &CurrentTask,
        vmo: fidl::Vmo,
        mapped_address: u64,
    ) -> Result<(), Errno> {
        let binder_process = self.binder_connection.proc(current_task)?;
        release_after!(binder_process, current_task.kernel(), {
            binder_process.map_external_vmo(vmo, mapped_address)
        })
    }

    pub fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<(), Errno> {
        let binder_process = self.binder_connection.proc(current_task)?;
        release_after!(binder_process, current_task.kernel(), {
            self.binder_connection
                .device
                .ioctl(locked, current_task, &binder_process, request, arg)
                .map(|_| ())
        })
    }

    pub fn interrupt(&self) {
        self.binder_connection.interrupt();
    }

    pub fn close(&self, kernel: &Kernel) {
        self.binder_connection.close(kernel);
    }
}

#[derive(Debug, Default)]
struct BinderProcessState {
    /// Maximum number of thread to spawn.
    max_thread_count: usize,
    /// Whether a new thread has been requested, but not registered yet.
    thread_requested: bool,
    /// The set of threads that are interacting with the binder driver.
    thread_pool: ThreadPool,
    /// Binder objects hosted by the process shared with other processes.
    objects: BTreeMap<UserAddress, Arc<BinderObject>>,
    /// Handle table of remote binder objects.
    handles: ReleaseGuard<HandleTable>,
    /// State associated with active transactions, keyed by the userspace addresses of the buffers
    /// allocated to them. When the process frees a transaction buffer with `BC_FREE_BUFFER`, the
    /// state is dropped, releasing temporary strong references and the memory allocated to the
    /// transaction.
    active_transactions: BTreeMap<UserAddress, ReleaseGuard<ActiveTransaction>>,
    /// The list of processes that should be notified if this process dies.
    death_subscribers: Vec<(WeakRef<BinderProcess>, binder_uintptr_t)>,
    /// Whether the binder connection for this process is closed. Once closed, any blocking
    /// operation will be aborted and return an EBADF error.
    closed: bool,
    /// Whether the binder connection for this process is interrupted. A process that is
    /// interrupted either just before or while waiting must abort the operation and return a EINTR
    /// error.
    interrupted: bool,
}

#[derive(Default, Debug)]
struct CommandQueueWithWaitQueue {
    commands: VecDeque<Command>,
    waiters: WaitQueue,
}

impl CommandQueueWithWaitQueue {
    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn pop_front(&mut self) -> Option<Command> {
        self.commands.pop_front()
    }

    fn push_back(&mut self, command: Command) {
        self.commands.push_back(command);
        self.waiters.notify_fd_events(FdEvents::POLLIN);
    }

    fn has_waiters(&self) -> bool {
        !self.waiters.is_empty()
    }

    fn query_events(&self) -> FdEvents {
        if self.is_empty() {
            FdEvents::POLLOUT
        } else {
            FdEvents::POLLIN | FdEvents::POLLOUT
        }
    }

    fn wait_async_simple(&self, waiter: &mut SimpleWaiter) {
        self.waiters.wait_async_simple(waiter);
    }

    fn wait_async_fd_events(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.waiters.wait_async_fd_events(waiter, events, handler)
    }

    fn notify_all(&self) {
        self.waiters.notify_all();
    }
}

/// An active binder transaction.
#[derive(Debug)]
struct ActiveTransaction {
    /// The transaction's request type.
    request_type: RequestType,
    /// The state associated with the transaction. Not read, exists to be dropped along with the
    /// [`ActiveTransaction`] object.
    state: ReleaseGuard<TransactionState>,
}

impl Releasable for ActiveTransaction {
    type Context<'a: 'b, 'b> = ();

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        self.state.release(context);
    }
}

/// State held for the duration of a transaction. When a transaction completes (or fails), this
/// state is dropped, decrementing temporary strong references to binder objects.
#[derive(Debug)]
struct TransactionState {
    kernel: Arc<Kernel>,
    /// The process whose handle table `handles` belong to.
    proc: WeakRef<BinderProcess>,
    /// The pid of the target process.
    pid: pid_t,
    /// The remote resource accessor of the target process. This is None when the receiving process
    /// is a local process.
    remote_resource_accessor: Option<Arc<RemoteResourceAccessor>>,
    /// The objects to strongly owned for the duration of the transaction.
    guards: Vec<StrongRefGuard>,
    /// The handles to decrement their strong reference count.
    handles: Vec<Handle>,
    /// The FDs of the target process that the kernel is responsible for closing, because they were
    /// sent with BINDER_TYPE_FDA.
    owned_fds: Vec<FdNumber>,
}

impl Releasable for TransactionState {
    type Context<'a: 'b, 'b> = ();

    fn release<'a: 'b, 'b>(self, _: Self::Context<'a, 'b>) {
        log_trace!("Releasing binder TransactionState");
        let mut drop_actions = RefCountActions::default();
        // Release the owned objects unconditionally.
        for guard in self.guards {
            guard.release(&mut drop_actions);
        }
        if let Some(proc) = self.proc.upgrade() {
            // Release handles only if the owning process is still alive.
            let mut proc_state = proc.lock();
            for handle in &self.handles {
                if let Err(error) =
                    proc_state.handles.dec_strong(handle.object_index(), &mut drop_actions)
                {
                    // Ignore the error because there is little we can do about it.
                    // Panicking would be wrong, in case the client issued an extra strong decrement.
                    log_warn!(
                        "Error when dropping transaction state for process {}: {:?}",
                        proc.pid,
                        error
                    );
                }
            }
        }
        // Releasing action must be done without holding BinderProcess lock.
        drop_actions.release(());

        // Close the owned fd.
        if !self.owned_fds.is_empty() {
            let weak_task = self.kernel.pids.read().get_task(self.pid);
            if let Some(task) = weak_task.upgrade() {
                let resource_accessor =
                    get_resource_accessor(task.deref(), &self.remote_resource_accessor);
                for fd in &self.owned_fds {
                    if let Err(error) = resource_accessor.close_fd(*fd) {
                        log_warn!(
                            "Error when dropping transaction state while closing fd for task {}: {:?}",
                            self.pid,
                            error
                        );
                    }
                }
            };
        }
    }
}

/// Transaction state held during the processing and dispatching of a transaction. In the event of
/// an error while dispatching a transaction, this object is meant to cleanup any temporary
/// resources that were allocated. Once a transaction has been dispatched successfully, this object
/// can be converted into a [`TransactionState`] to be held for the lifetime of the transaction.
struct TransientTransactionState<'a> {
    /// The part of the transient state that will live for the lifetime of the transaction.
    state: Option<ReleaseGuard<TransactionState>>,
    /// The task to which the transient file descriptors belong.
    accessor: &'a dyn ResourceAccessor,
    /// The file descriptors to close in case of an error.
    transient_fds: Vec<FdNumber>,
    /// A guard that will ensure a panic on drop if `state` has not been released.
    drop_guard: DropGuard,
}

impl<'a> Releasable for TransientTransactionState<'a> {
    type Context<'b: 'c, 'c> = ();
    fn release<'b: 'c, 'c>(self, _: Self::Context<'b, 'c>) {
        for fd in &self.transient_fds {
            let _: Result<(), Errno> = self.accessor.close_fd(*fd);
        }
        self.state.release(());
        self.drop_guard.disarm();
    }
}

impl<'a> std::fmt::Debug for TransientTransactionState<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransientTransactionState")
            .field("state", &self.state)
            .field("accessor", &self.accessor)
            .field("transient_fds", &self.transient_fds)
            .finish()
    }
}

impl<'a> TransientTransactionState<'a> {
    /// Creates a new [`TransientTransactionState`], whose resources will belong to `accessor` and
    /// `target_proc` for FDs and binder handles respectively.
    fn new(accessor: &'a dyn ResourceAccessor, target_proc: &BinderProcess) -> Self {
        TransientTransactionState {
            state: Some(
                TransactionState {
                    kernel: accessor.kernel().clone(),
                    proc: target_proc.weak_self.clone(),
                    pid: target_proc.pid,
                    remote_resource_accessor: target_proc.remote_resource_accessor.clone(),
                    guards: vec![],
                    handles: vec![],
                    owned_fds: vec![],
                }
                .into(),
            ),
            accessor,
            transient_fds: vec![],
            drop_guard: DropGuard::default(),
        }
        .into()
    }

    /// Schedule `handle` to have its strong reference count decremented when the transaction ends
    /// (both in case of success or failure).
    fn push_handle(&mut self, handle: Handle) {
        self.state.as_mut().unwrap().handles.push(handle)
    }

    /// Schedule `guard` to be released when the transaction ends (both in case of success or
    /// failure).
    fn push_guard(&mut self, guard: StrongRefGuard) {
        self.state.as_mut().unwrap().guards.push(guard);
    }

    /// Schedule `fd` to be removed from the file descriptor table when the transaction ends (both
    /// in case of success or failure).
    fn push_owned_fd(&mut self, fd: FdNumber) {
        self.state.as_mut().unwrap().owned_fds.push(fd)
    }

    /// Schedule `fd` to be removed from the file descriptor table if the transaction fails.
    fn push_transient_fd(&mut self, fd: FdNumber) {
        self.transient_fds.push(fd)
    }

    fn into_state(mut self) -> ReleaseGuard<TransactionState> {
        // Clear the transient FD list, so that these FDs no longer get closed.
        self.transient_fds.clear();
        let result = self.state.take().unwrap();
        self.release(());
        result
    }
}

/// The request type of a transaction.
#[derive(Debug)]
enum RequestType {
    /// A fire-and-forget request, which has special ordering guarantees.
    Oneway {
        /// The recipient of the transaction. Oneway transactions are ordered for a given binder
        /// object.
        object: Arc<BinderObject>,
    },
    /// A request/response type.
    RequestResponse,
}

#[derive(Debug)]
struct BinderProcess {
    /// Weak reference to self.
    weak_self: WeakRef<BinderProcess>,

    /// A global identifier at the driver level for this binder process.
    identifier: u64,

    /// The identifier of the process associated with this binder process.
    pid: pid_t,

    /// Resource accessor to access remote resource in case of a remote binder process. None in
    /// case of a local process.
    remote_resource_accessor: Option<Arc<RemoteResourceAccessor>>,

    // The mutable state of `BinderProcess` is protected by 3 locks. For ordering purpose, locks
    // must be taken in the order they are defined in this class, even across `BinderProcess`
    // instances.
    // Moreover, any `BinderThread` lock must be ordered after any `state` lock from a
    // `BinderProcess`.
    /// The [`SharedMemory`] region mapped in both the driver and the binder process. Allows for
    /// transactions to copy data once from the sender process into the receiver process.
    shared_memory: Mutex<Option<SharedMemory>>,

    /// The main mutable state of the `BinderProcess`.
    state: Mutex<BinderProcessState>,

    /// A queue for commands that could not be scheduled on any existing binder threads. Binder
    /// threads that exhaust their own queue will read from this one.
    ///
    /// When there are no commands in a thread's and the process' command queue, a binder thread can
    /// register with this [`WaitQueue`] to be notified when commands are available.
    command_queue: Mutex<CommandQueueWithWaitQueue>,
}

type BinderProcessGuard<'a> = Guard<'a, BinderProcess, MutexGuard<'a, BinderProcessState>>;

impl BinderProcess {
    #[allow(clippy::let_and_return)]
    fn new(
        identifier: u64,
        pid: pid_t,
        remote_resource_accessor: Option<Arc<RemoteResourceAccessor>>,
    ) -> OwnedRef<Self> {
        log_trace!("new BinderProcess id={}", identifier);
        let result = OwnedRef::new_cyclic(|weak_self| Self {
            weak_self,
            identifier,
            pid,
            remote_resource_accessor,
            shared_memory: Default::default(),
            state: Default::default(),
            command_queue: Default::default(),
        });
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = result.shared_memory.lock();
            let _l2 = result.lock();
            let _l3 = result.command_queue.lock();
        }
        result
    }

    fn lock<'a>(&'a self) -> BinderProcessGuard<'a> {
        Guard::new(self, self.state.lock())
    }

    fn close(&self) {
        log_trace!("closing BinderProcess id={}", self.identifier);
        let mut state = self.lock();
        if !state.closed {
            state.closed = true;
            state.thread_pool.notify_all();
            self.command_queue.lock().notify_all();
        }
    }

    fn interrupt(&self) {
        log_trace!("interrupting BinderProcess id={}", self.identifier);
        let mut state = self.lock();
        if !state.interrupted {
            state.interrupted = true;
            state.thread_pool.notify_all();
            self.command_queue.lock().notify_all();
        }
    }

    /// Make all blocked threads stop waiting and return nothing. This is used by flush to
    /// make userspace recheck whether binder is being shut down.
    fn kick_all_threads(&self) {
        log_trace!("kicking threads for BinderProcess id={}", self.identifier);
        let state = self.lock();
        for thread in state.thread_pool.0.values() {
            thread.lock().request_kick = true;
        }
        state.thread_pool.notify_all();
    }

    /// Return the `ResourceAccessor` to use to access the resources of this process.
    fn get_resource_accessor<'a>(
        &'a self,
        task: &'a dyn ResourceAccessor,
    ) -> &'a dyn ResourceAccessor {
        get_resource_accessor(task, &self.remote_resource_accessor)
    }

    /// Enqueues `command` for the process and wakes up any thread that is waiting for commands.
    pub fn enqueue_command(&self, command: Command) {
        log_trace!("BinderProcess id={} enqueuing command {:?}", self.identifier, command);
        self.command_queue.lock().push_back(command);
    }

    /// A binder thread is done reading a buffer allocated to a transaction. The binder
    /// driver can reclaim this buffer.
    fn handle_free_buffer(&self, buffer_ptr: UserAddress) -> Result<(), Errno> {
        log_trace!("BinderProcess id={} freeing buffer {:?}", self.identifier, buffer_ptr);
        // Drop the state associated with the now completed transaction.
        let active_transaction = self.lock().active_transactions.remove(&buffer_ptr);
        release_after!(active_transaction, (), {
            // Check if this was a oneway transaction and schedule the next oneway if this is the case.
            if let Some(ActiveTransaction {
                request_type: RequestType::Oneway { object }, ..
            }) = active_transaction.as_ref().map(|at| at.deref())
            {
                let mut object_state = object.lock();
                assert!(
                    object_state.handling_oneway_transaction,
                    "freeing a oneway buffer implies that a oneway transaction was being handled"
                );
                if let Some(transaction) = object_state.oneway_transactions.pop_front() {
                    // Drop the lock, as we've completed all mutations and don't want to hold this
                    // lock while acquiring any others.
                    drop(object_state);

                    // Schedule the transaction
                    self.enqueue_command(Command::OnewayTransaction(transaction));
                } else {
                    // No more oneway transactions queued, mark the queue handling as done.
                    object_state.handling_oneway_transaction = false;
                }
            }
        });

        // Reclaim the memory.
        let mut shared_memory_lock = self.shared_memory.lock();
        let shared_memory = shared_memory_lock.as_mut().ok_or_else(|| errno!(ENOMEM))?;
        shared_memory.free_buffer(buffer_ptr)
    }

    /// Handle a binder thread's request to increment/decrement a strong/weak reference to a remote
    /// binder object.
    fn handle_refcount_operation(
        &self,
        command: binder_driver_command_protocol,
        handle: Handle,
    ) -> Result<(), Errno> {
        let mut actions = RefCountActions::default();
        release_after!(actions, (), {
            self.lock().handle_refcount_operation(command, handle, &mut actions)
        })
    }

    /// Handle a binder thread's notification that it successfully incremented a strong/weak
    /// reference to a local (in-process) binder object. This is in response to a
    /// `BR_ACQUIRE`/`BR_INCREFS` command.
    fn handle_refcount_operation_done(
        &self,
        command: binder_driver_command_protocol,
        object: LocalBinderObject,
    ) -> Result<(), Errno> {
        let mut actions = RefCountActions::default();
        release_after!(actions, (), {
            self.lock().handle_refcount_operation_done(command, object, &mut actions)
        })
    }

    /// Subscribe a process to the death of the owner of `handle`.
    fn handle_request_death_notification(
        &self,
        handle: Handle,
        cookie: binder_uintptr_t,
    ) -> Result<(), Errno> {
        let proxy = match handle {
            Handle::ContextManager => {
                track_stub!(
                    TODO("https://fxbug.dev/322873363"),
                    "binder death notification for service manager"
                );
                return Ok(());
            }
            Handle::Object { index } => {
                let (proxy, guard) =
                    self.lock().handles.get(index).ok_or_else(|| errno!(ENOENT))?;

                // The object must have strong reference when used to find its owner process,
                // but there is no need to keep it alive afterwards.
                let mut actions = RefCountActions::default();
                guard.release(&mut actions);
                actions.release(());

                proxy
            }
        };
        if let Some(owner) = proxy.owner.upgrade() {
            owner.lock().death_subscribers.push((self.weak_self.clone(), cookie));
        } else {
            // The object is already dead. Notify immediately. To be noted: the requesting thread
            // cannot handle the notification, in case it is holding some mutex while processing a
            // oneway transaction (where its transaction stack will be empty). It is currently not
            // a problem, because enqueue_command never schedule on the current thread.
            self.enqueue_command(Command::DeadBinder(cookie));
        }
        Ok(())
    }

    /// Remove a previously subscribed death notification.
    fn handle_clear_death_notification(
        &self,
        handle: Handle,
        cookie: binder_uintptr_t,
    ) -> Result<(), Errno> {
        let owner = match handle {
            Handle::ContextManager => {
                track_stub!(
                    TODO("https://fxbug.dev/322873735"),
                    "binder clear death notification for service manager"
                );
                self.enqueue_command(Command::ClearDeathNotificationDone(cookie));
                return Ok(());
            }
            Handle::Object { index } => {
                self.lock().handles.get_owner(index).ok_or_else(|| errno!(ENOENT))?
            }
        };
        if let Some(owner) = owner.upgrade() {
            let mut owner = owner.lock();
            if let Some((idx, _)) = owner
                .death_subscribers
                .iter()
                .enumerate()
                .find(|(_idx, (proc, c))| proc.as_ptr() == self && *c == cookie)
            {
                owner.death_subscribers.swap_remove(idx);
            }
        }
        self.enqueue_command(Command::ClearDeathNotificationDone(cookie));
        Ok(())
    }

    /// Map the external vmo into the driver address space, recording the userspace address.
    fn map_external_vmo(&self, vmo: fidl::Vmo, mapped_address: u64) -> Result<(), Errno> {
        let mut shared_memory = self.shared_memory.lock();
        // Do not support mapping shared memory more than once.
        if shared_memory.is_some() {
            return error!(EINVAL);
        }
        let memory = MemoryObject::from(vmo);
        let size = memory.get_size();
        *shared_memory = Some(SharedMemory::map(&memory, mapped_address.into(), size as usize)?);
        Ok(())
    }
}

impl<'a> BinderProcessGuard<'a> {
    /// Return the `BinderThread` with the given `tid`, creating it if it doesn't exist.
    fn find_or_register_thread(&mut self, tid: pid_t) -> OwnedRef<BinderThread> {
        if let Some(thread) = self.thread_pool.0.get(&tid) {
            return OwnedRef::share(thread);
        }
        let thread = BinderThread::new(self, tid);
        self.thread_pool.0.insert(tid, OwnedRef::share(&thread));
        thread
    }

    /// Unregister the `BinderThread` with the given `tid`.
    fn unregister_thread(&mut self, current_task: &CurrentTask, tid: pid_t) {
        self.thread_pool.0.remove(&tid).release(current_task.kernel());
    }

    /// Inserts a reference to a binder object, returning a handle that represents it.
    /// The handle may be an existing handle if the object was already present in the table.
    /// The object must have at least a strong reference guarded by `guard` to ensure it is kept
    /// alive until it is added to the transaction.
    /// Returns the handle representing the object.
    pub fn insert_for_transaction(
        &mut self,
        guard: StrongRefGuard,
        actions: &mut RefCountActions,
    ) -> Handle {
        self.handles.insert_for_transaction(guard, actions)
    }

    /// Handle a binder thread's request to increment/decrement a strong/weak reference to a remote
    /// binder object.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn handle_refcount_operation(
        &mut self,
        command: binder_driver_command_protocol,
        handle: Handle,
        actions: &mut RefCountActions,
    ) -> Result<(), Errno> {
        let idx = match handle {
            Handle::ContextManager => {
                track_stub!(
                    TODO("https://fxbug.dev/322873629"),
                    "binder acquire/release refs for context manager object"
                );
                return Ok(());
            }
            Handle::Object { index } => index,
        };

        match command {
            binder_driver_command_protocol_BC_ACQUIRE => {
                log_trace!("Strong increment on handle {}", idx);
                self.handles.inc_strong(idx, actions)
            }
            binder_driver_command_protocol_BC_RELEASE => {
                log_trace!("Strong decrement on handle {}", idx);
                self.handles.dec_strong(idx, actions)
            }
            binder_driver_command_protocol_BC_INCREFS => {
                log_trace!("Weak increment on handle {}", idx);
                self.handles.inc_weak(idx, actions)
            }
            binder_driver_command_protocol_BC_DECREFS => {
                log_trace!("Weak decrement on handle {}", idx);
                self.handles.dec_weak(idx, actions)
            }
            _ => unreachable!(),
        }
    }

    /// Handle a binder thread's notification that it successfully incremented a strong/weak
    /// reference to a local (in-process) binder object. This is in response to a
    /// `BR_ACQUIRE`/`BR_INCREFS` command.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn handle_refcount_operation_done(
        &self,
        command: binder_driver_command_protocol,
        local: LocalBinderObject,
        actions: &mut RefCountActions,
    ) -> Result<(), Errno> {
        let object = self.find_object(&local).ok_or_else(|| errno!(EINVAL))?;
        match command {
            binder_driver_command_protocol_BC_INCREFS_DONE => {
                log_trace!("Acknowledging increment reference for {:?}", object);
                object.ack_incref(actions)
            }
            binder_driver_command_protocol_BC_ACQUIRE_DONE => {
                log_trace!("Acknowleding acquire done for {:?}", object);
                object.ack_acquire(actions)
            }
            _ => unreachable!(),
        }
    }

    /// Finds the binder object that corresponds to the process-local addresses `local`.
    pub fn find_object(&self, local: &LocalBinderObject) -> Option<&Arc<BinderObject>> {
        self.objects.get(&local.weak_ref_addr)
    }

    /// Finds the binder object that corresponds to the process-local addresses `local`, or creates
    /// a new [`BinderObject`] to represent the one in the process.
    pub fn find_or_register_object(
        &mut self,
        binder_thread: &BinderThread,
        local: LocalBinderObject,
        flags: BinderObjectFlags,
    ) -> StrongRefGuard {
        if let Some(object) = self.find_object(&local) {
            // The ref count can grow back from 0 in this instance because the object is being
            // registered again by its owner.
            object.inc_strong_unchecked(binder_thread)
        } else {
            let (object, guard) = BinderObject::new(self.base, local, flags);

            // Tell the owning process that a remote process now has a strong reference to
            // this object.
            binder_thread.lock().enqueue_command(Command::AcquireRef(object.local));

            self.objects.insert(object.local.weak_ref_addr, object);

            guard
        }
    }

    /// Whether the driver should request that the client starts a new thread.
    fn should_request_thread(&self, thread: &BinderThread) -> bool {
        !self.thread_requested
            && self.thread_pool.registered_threads() < self.max_thread_count
            && thread.lock().is_main_or_registered()
            && !self.thread_pool.has_available_thread()
    }

    /// Called back when the driver successfully asked the client to start a new thread.
    fn did_request_thread(&mut self) {
        self.thread_requested = true;
    }
}

impl Releasable for BinderProcess {
    type Context<'a: 'b, 'b> = &'a Kernel;

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        log_trace!("Releasing BinderProcess id={}", self.identifier);
        let state = self.state.into_inner();
        // Notify any subscribers that the objects this process owned are now dead.
        for (proc, cookie) in state.death_subscribers {
            if let Some(target_proc) = proc.upgrade() {
                target_proc.enqueue_command(Command::DeadBinder(cookie));
            }
        }

        // Notify all callers that had transactions scheduled for this process that the recipient is
        // dead.
        for command in self.command_queue.into_inner().commands {
            if let Command::Transaction { sender, .. } = command {
                if let Some(sender_thread) = sender.thread.upgrade() {
                    sender_thread
                        .lock()
                        .enqueue_command(Command::DeadReply { pop_transaction: true });
                }
            }
        }

        for transaction in state.active_transactions.into_values() {
            transaction.release(());
        }

        state.handles.release(());

        for thread in state.thread_pool.0.into_values() {
            thread.release(context);
        }
    }
}

/// The mapped VMO shared between userspace and the binder driver.
///
/// The binder driver copies messages from one process to another, which essentially amounts to
/// a copy between VMOs. It is not possible to copy directly between VMOs without an intermediate
/// copy, and the binder driver must only perform one copy for performance reasons.
///
/// The memory allocated to a binder process is shared with the binder driver, and mapped into
/// the kernel's address space so that a VMO read operation can copy directly into the mapped VMO.
#[derive(Debug)]
struct SharedMemory {
    /// The address in kernel address space where the VMO is mapped.
    kernel_address: *mut u8,
    /// The address in user address space where the VMO is mapped.
    user_address: UserAddress,
    /// The length of the shared memory mapping in bytes.
    length: usize,
    /// The map from offset to size of all the currently active allocations, ordered in ascending
    /// order.
    ///
    /// This is used by the allocator to find new allocations.
    ///
    /// TODO(qsr): This should evolved into a better allocator for performance reason. Currently,
    /// each new allocation is done in O(n) where n is the number of currently active allocations.
    allocations: BTreeMap<usize, usize>,
}

/// The user buffers containing the data to send to the recipient of a binder transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TransactionBuffers {
    /// The buffer containing the data of the transaction.
    data: UserBuffer,
    /// The buffer containing the offsets of objects inside the `data` buffer.
    offsets: UserBuffer,
    /// An optional buffer pointing to the security context of the client of the transaction.
    security_context: Option<UserBuffer>,
}

/// Contains the allocations for a transaction.
#[derive(Debug)]
struct SharedMemoryAllocation<'a> {
    data_buffer: SharedBuffer<'a, u8>,
    offsets_buffer: SharedBuffer<'a, binder_uintptr_t>,
    scatter_gather_buffer: SharedBuffer<'a, u8>,
    security_context_buffer: Option<SharedBuffer<'a, u8>>,
}

impl From<SharedMemoryAllocation<'_>> for TransactionBuffers {
    fn from(value: SharedMemoryAllocation<'_>) -> Self {
        Self {
            data: value.data_buffer.user_buffer(),
            offsets: value.offsets_buffer.user_buffer(),
            security_context: value.security_context_buffer.map(|x| x.user_buffer()),
        }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        log_trace!("Dropping shared memory allocation {:?}", self);
        let kernel_root_vmar = fuchsia_runtime::vmar_root_self();

        // SAFETY: This object hands out references to the mapped memory, but the borrow checker
        // ensures correct lifetimes.
        let res = unsafe { kernel_root_vmar.unmap(self.kernel_address as usize, self.length) };
        match res {
            Ok(()) => {}
            Err(status) => {
                log_error!("failed to unmap shared binder region from kernel: {:?}", status);
            }
        }
    }
}

// SAFETY: SharedMemory has exclusive ownership of the `kernel_address` pointer, so it is safe to
// send across threads.
unsafe impl Send for SharedMemory {}

impl SharedMemory {
    fn map(memory: &MemoryObject, user_address: UserAddress, length: usize) -> Result<Self, Errno> {
        profile_duration!("SharedMemoryMap");
        // Map the VMO into the kernel's address space.
        let kernel_root_vmar = fuchsia_runtime::vmar_root_self();
        let kernel_address = memory
            .map_in_vmar(
                &kernel_root_vmar,
                0,
                0,
                length,
                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE,
            )
            .map_err(|status| {
                log_error!("failed to map shared binder region in kernel: {:?}", status);
                errno!(ENOMEM)
            })?;
        Ok(Self {
            kernel_address: kernel_address as *mut u8,
            user_address,
            length,
            allocations: Default::default(),
        })
    }

    /// Allocate a buffer of size `length` from this memory block.
    fn allocate(&mut self, length: usize) -> Result<usize, TransactionError> {
        // The current candidate for an allocation.
        let mut candidate = 0;
        for (&ptr, &size) in &self.allocations {
            // If there is enough room at the current candidate location, stop looking.
            if ptr - candidate >= length {
                break;
            }
            // Otherwise, check after the current allocation.
            candidate = ptr + size;
        }
        // At this point, either `candidate` is correct, or the only remaining position is at the
        // end of the buffer. In both case, the allocation succeed if there is enough room between
        // the candidate and the end of the buffer.
        if self.length - candidate < length {
            return Err(TransactionError::Failure);
        }
        self.allocations.insert(candidate, length);
        Ok(candidate)
    }

    /// Allocates three buffers large enough to hold the requested data, offsets, and scatter-gather
    /// buffer lengths, inserting padding between data and offsets as needed. `offsets_length` and
    /// `sg_buffers_length` must be 8-byte aligned.
    ///
    /// NOTE: When `data_length` is zero, a minimum data buffer size of 8 bytes is still allocated.
    /// This is because clients expect their buffer addresses to be uniquely associated with a
    /// transaction. Returning the same address for different transactions will break oneway
    /// transactions that have no payload.
    //
    // This is a temporary implementation of an allocator and should be replaced by something
    // more sophisticated. It currently implements a bump allocator strategy.
    fn allocate_buffers(
        &mut self,
        data_length: usize,
        offsets_length: usize,
        sg_buffers_length: usize,
        security_context_buffer_length: usize,
    ) -> Result<SharedMemoryAllocation<'_>, TransactionError> {
        // Round `data_length` up to the nearest multiple of 8, so that the offsets buffer is
        // aligned when we pack it next to the data buffer.
        let data_cap = round_up_to_increment(data_length, std::mem::size_of::<binder_uintptr_t>())?;
        // Ensure that we allocate at least 8 bytes, so that each buffer returned is uniquely
        // associated with a transaction. Otherwise, multiple zero-sized allocations will have the
        // same address and there will be no way of distinguishing which transaction they belong to.
        let data_cap = std::cmp::max(data_cap, std::mem::size_of::<binder_uintptr_t>());
        // Ensure that the offsets and buffers lengths are valid.
        if offsets_length % std::mem::size_of::<binder_uintptr_t>() != 0
            || sg_buffers_length % std::mem::size_of::<binder_uintptr_t>() != 0
            || security_context_buffer_length % std::mem::size_of::<binder_uintptr_t>() != 0
        {
            return Err(TransactionError::Malformed(errno!(EINVAL)));
        }
        let total_length = data_cap
            .checked_add(offsets_length)
            .and_then(|v| v.checked_add(sg_buffers_length))
            .and_then(|v| v.checked_add(security_context_buffer_length))
            .ok_or_else(|| errno!(EINVAL))?;
        let base_offset = self.allocate(total_length)?;

        // SAFETY: The offsets and lengths have been bounds-checked above. Constructing a
        // `SharedBuffer` should be safe.
        unsafe {
            Ok(SharedMemoryAllocation {
                data_buffer: SharedBuffer::new_unchecked(self, base_offset, data_length),
                offsets_buffer: SharedBuffer::new_unchecked(
                    self,
                    base_offset + data_cap,
                    offsets_length,
                ),
                scatter_gather_buffer: SharedBuffer::new_unchecked(
                    self,
                    base_offset + data_cap + offsets_length,
                    sg_buffers_length,
                ),
                security_context_buffer: (security_context_buffer_length > 0).then(|| {
                    SharedBuffer::new_unchecked(
                        self,
                        base_offset + data_cap + offsets_length + sg_buffers_length,
                        security_context_buffer_length,
                    )
                }),
            })
        }
    }

    // Reclaim the buffer so that it can be reused.
    fn free_buffer(&mut self, buffer: UserAddress) -> Result<(), Errno> {
        // Sanity check that the buffer being freed came from this memory region.
        if buffer < self.user_address || buffer >= self.user_address + self.length {
            return error!(EINVAL);
        }
        let offset = buffer - self.user_address;
        self.allocations.remove(&offset);
        Ok(())
    }
}

/// A buffer of memory allocated from a binder process' [`SharedMemory`].
#[derive(Debug)]
struct SharedBuffer<'a, T> {
    memory: &'a SharedMemory,
    /// Offset into the shared memory region where the buffer begins.
    offset: usize,
    /// The length of the buffer in bytes.
    length: usize,
    // A zero-sized type that satisfies the compiler's need for the struct to reference `T`, which
    // is used in `as_mut_bytes` and `as_bytes`.
    _phantom_data: std::marker::PhantomData<T>,
}

impl<'a, T: IntoBytes> SharedBuffer<'a, T> {
    /// Creates a new `SharedBuffer`, which represents a sub-region of `memory` starting at `offset`
    /// bytes, with `length` bytes.
    ///
    /// This is unsafe because the caller is responsible for bounds-checking the sub-region and
    /// ensuring it is not aliased.
    unsafe fn new_unchecked(memory: &'a SharedMemory, offset: usize, length: usize) -> Self {
        Self { memory, offset, length, _phantom_data: std::marker::PhantomData }
    }

    /// Returns a mutable slice of the buffer.
    fn as_mut_bytes(&mut self) -> &'a mut [T] {
        // SAFETY: `offset + length` was bounds-checked by `allocate_buffers`, and the
        // memory region pointed to was zero-allocated by mapping a new VMO.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.memory.kernel_address.add(self.offset) as *mut T,
                self.length / std::mem::size_of::<T>(),
            )
        }
    }

    /// Returns an immutable slice of the buffer.
    fn as_bytes(&self) -> &'a [T] {
        // SAFETY: `offset + length` was bounds-checked by `allocate_buffers`, and the
        // memory region pointed to was zero-allocated by mapping a new VMO.
        unsafe {
            std::slice::from_raw_parts(
                self.memory.kernel_address.add(self.offset) as *const T,
                self.length / std::mem::size_of::<T>(),
            )
        }
    }

    /// The userspace address and length of the buffer.
    fn user_buffer(&self) -> UserBuffer {
        UserBuffer { address: self.memory.user_address + self.offset, length: self.length }
    }
}

/// The set of threads that are interacting with the binder driver for a given process.
#[derive(Debug, Default)]
struct ThreadPool(BTreeMap<pid_t, OwnedRef<BinderThread>>);

impl ThreadPool {
    fn has_available_thread(&self) -> bool {
        self.0.values().any(|t| t.lock().is_available())
    }

    fn notify_all(&self) {
        for t in self.0.values() {
            t.lock().command_queue.notify_all();
        }
    }

    /// The number of registered thread in the pool. This doesn't count the main thread.
    fn registered_threads(&self) -> usize {
        self.0.values().filter(|t| t.lock().is_registered()).count()
    }
}

/// Table containing handles to remote binder objects.
#[derive(Debug, Default)]
struct HandleTable {
    table: slab::Slab<BinderObjectRef>,
}

/// The HandleTable is released at the time the BinderProcess is released. At this moment, any
/// reference to object owned by another BinderProcess need to be clean.
impl Releasable for HandleTable {
    type Context<'a: 'b, 'b> = ();
    fn release<'a: 'b, 'b>(self, _: ()) {
        for (_, r) in self.table.into_iter() {
            let mut actions = RefCountActions::default();
            r.clean_refs(&mut actions);
            actions.release(());
        }
    }
}

/// A reference to a binder object in another process.
#[derive(Debug)]
struct BinderObjectRef {
    /// The associated BinderObject
    binder_object: Arc<BinderObject>,
    /// The number of strong references to this `BinderObjectRef`.
    strong_count: usize,
    /// If not None, the guard representing the current guard reference that this handles has on the
    /// `BinderObject` because its own guard count is strictly positive.
    strong_guard: Option<StrongRefGuard>,
    /// The number of weak references to this `BinderObjectRef`.
    weak_count: usize,
    /// If not None, the guard representing the current weak reference that this handles has on the
    /// `BinderObject` because its own weak count is strictly positive.
    weak_guard: Option<WeakRefGuard>,
}

/// Assert that a dropped reference do not have any reference left, as this would keep an object
/// owned by another process alive.
#[cfg(any(test, debug_assertions))]
impl Drop for BinderObjectRef {
    fn drop(&mut self) {
        assert!(!self.has_ref());
    }
}

impl BinderObjectRef {
    /// Build a new reference to the given object. The reference will start with a strong count of
    /// 1.
    fn new(guard: StrongRefGuard) -> Self {
        let binder_object = guard.binder_object.clone();
        Self {
            binder_object,
            strong_count: 1,
            strong_guard: Some(guard),
            weak_count: 0,
            weak_guard: None,
        }
    }

    /// Returns whether this object has still any strong or weak reference. The object must be kept
    /// in the handle table until this is false.
    fn has_ref(&self) -> bool {
        self.weak_count > 0 || self.strong_count > 0
    }

    /// Free any reference held on `binder_object` by this reference.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn clean_refs(mut self, actions: &mut RefCountActions) {
        if let Some(guard) = self.strong_guard.take() {
            debug_assert!(
                self.strong_count > 0,
                "The strong count must be strictly positive when the strong guard is not None"
            );
            guard.release(actions);
            self.strong_count = 0;
        }
        if let Some(guard) = self.weak_guard.take() {
            debug_assert!(
                self.weak_count > 0,
                "The weak count must be strictly positive when the weak guard is not None"
            );
            guard.release(actions);
            self.weak_count = 0;
        }
    }

    /// Increments the strong reference count of the binder object reference.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn inc_strong(&mut self, _actions: &mut RefCountActions) -> Result<(), Errno> {
        assert!(self.has_ref());
        if self.strong_count == 0 {
            let guard = self.binder_object.inc_strong_checked()?;
            self.strong_guard = Some(guard);
        }
        self.strong_count += 1;
        Ok(())
    }

    /// Increments the strong reference count of the binder object reference while holding a strong
    /// reference guard on the object.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn inc_strong_with_guard(&mut self, guard: StrongRefGuard, actions: &mut RefCountActions) {
        assert!(self.has_ref());
        if self.strong_count == 0 {
            self.strong_guard = Some(guard);
        } else {
            guard.release(actions);
        }
        self.strong_count += 1;
    }

    /// Increments the weak reference count of the binder object reference.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn inc_weak(&mut self, actions: &mut RefCountActions) {
        assert!(self.has_ref());
        if self.weak_count == 0 {
            let guard = self.binder_object.inc_weak(actions);
            self.weak_guard = Some(guard);
        }
        self.weak_count += 1;
    }

    /// Decrements the strong reference count of the binder object reference.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn dec_strong(&mut self, actions: &mut RefCountActions) -> Result<(), Errno> {
        if self.strong_count == 0 {
            return error!(EINVAL);
        }
        if self.strong_count == 1 {
            let Some(guard) = self.strong_guard.take() else {
                panic!("No guard while strong count is 1");
            };
            guard.release(actions);
        }
        self.strong_count -= 1;
        Ok(())
    }

    /// Decrements the weak reference count of the binder object reference.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn dec_weak(&mut self, actions: &mut RefCountActions) -> Result<(), Errno> {
        if self.weak_count == 0 {
            return error!(EINVAL);
        }
        if self.weak_count == 1 {
            let Some(guard) = self.weak_guard.take() else {
                panic!("No guard while strong count is 1");
            };
            guard.release(actions);
        }
        self.weak_count -= 1;
        Ok(())
    }

    fn is_ref_to_object(&self, object: &Arc<BinderObject>) -> bool {
        if Arc::as_ptr(&self.binder_object) == Arc::as_ptr(object) {
            return true;
        }

        let deep_equal = self.binder_object.local.weak_ref_addr == object.local.weak_ref_addr
            && self.binder_object.owner.as_ptr() == object.owner.as_ptr();
        // This shouldn't be possible. We have it here as a debugging check.
        assert!(
            !deep_equal,
            "Two different BinderObjects were found referring to the same underlying object: {object:?} and {self:?}"
        );

        false
    }
}

/// A set of `BinderObject` whose reference counts may have changed. Releasing it will enqueue all
/// the corresponding actions and remove any freed object from the owner process.
#[derive(Default)]
struct RefCountActions {
    objects: BTreeSet<ArcKey<BinderObject>>,
    drop_guard: DropGuard,
}

impl Deref for RefCountActions {
    type Target = BTreeSet<ArcKey<BinderObject>>;

    fn deref(&self) -> &Self::Target {
        &self.objects
    }
}

impl DerefMut for RefCountActions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.objects
    }
}

impl Releasable for RefCountActions {
    type Context<'a: 'b, 'b> = ();

    fn release<'a: 'b, 'b>(self, _context: ()) {
        for object in self.objects.into_iter() {
            object.apply_deferred_refcounts();
        }
        self.drop_guard.disarm();
    }
}

impl RefCountActions {
    #[cfg(test)]
    fn default_released() -> Self {
        let r = RefCountActions::default();
        r.drop_guard.disarm();
        r
    }
}

impl HandleTable {
    /// Inserts a reference to a binder object, returning a handle that represents it.
    /// The handle may be an existing handle if the object was already present in the table.
    /// Transfer the reference represented by `guard` to the handle.
    /// A new handle will have a single strong reference, an existing handle will have its strong
    /// reference count incremented by one.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess` and the handle representing the object.
    fn insert_for_transaction(
        &mut self,
        guard: StrongRefGuard,
        actions: &mut RefCountActions,
    ) -> Handle {
        let index = if let Some((existing_idx, object_ref)) =
            self.find_ref_for_object(&guard.binder_object)
        {
            // Increment the number of reference to the handle as expected by the caller.
            object_ref.inc_strong_with_guard(guard, actions);
            existing_idx
        } else {
            // The new handle will be created with a strong reference as expected by the
            // caller.
            self.table.insert(BinderObjectRef::new(guard))
        };
        Handle::Object { index }
    }

    fn find_ref_for_object(
        &mut self,
        object: &Arc<BinderObject>,
    ) -> Option<(usize, &mut BinderObjectRef)> {
        self.table.iter_mut().filter(|(_, object_ref)| object_ref.is_ref_to_object(object)).next()
    }

    /// Retrieves a reference to a binder object at index `idx`. Returns None if the index doesn't
    /// exist or the object has no strong reference, otherwise returns the object and a guard to
    /// ensure it is kept alive while used.
    fn get(&self, idx: usize) -> Option<(Arc<BinderObject>, StrongRefGuard)> {
        let object_ref = self.table.get(idx)?;
        match object_ref.binder_object.inc_strong_checked() {
            Ok(guard) => Some((object_ref.binder_object.clone(), guard)),
            _ => None,
        }
    }

    /// Retrieves the owner of the binder object at index `idx`, whether the object has strong
    /// references or not.
    fn get_owner(&self, idx: usize) -> Option<WeakRef<BinderProcess>> {
        self.table.get(idx).map(|r| r.binder_object.owner.clone())
    }

    /// Increments the strong reference count of the binder object reference at index `idx`,
    /// failing if the object no longer exists.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn inc_strong(&mut self, idx: usize, actions: &mut RefCountActions) -> Result<(), Errno> {
        Ok(self.table.get_mut(idx).ok_or_else(|| errno!(ENOENT))?.inc_strong(actions)?)
    }

    /// Increments the weak reference count of the binder object reference at index `idx`, failing
    /// if the object does not exist.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn inc_weak(&mut self, idx: usize, actions: &mut RefCountActions) -> Result<(), Errno> {
        Ok(self.table.get_mut(idx).ok_or_else(|| errno!(ENOENT))?.inc_weak(actions))
    }

    /// Decrements the strong reference count of the binder object reference at index `idx`, failing
    /// if the object no longer exists.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn dec_strong(&mut self, idx: usize, actions: &mut RefCountActions) -> Result<(), Errno> {
        let object_ref = self.table.get_mut(idx).ok_or_else(|| errno!(ENOENT))?;
        object_ref.dec_strong(actions)?;
        if !object_ref.has_ref() {
            self.table.remove(idx);
        }
        Ok(())
    }

    /// Decrements the weak reference count of the binder object reference at index `idx`, failing
    /// if the object does not exist.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn dec_weak(&mut self, idx: usize, actions: &mut RefCountActions) -> Result<(), Errno> {
        let object_ref = self.table.get_mut(idx).ok_or_else(|| errno!(ENOENT))?;
        object_ref.dec_weak(actions)?;
        if !object_ref.has_ref() {
            self.table.remove(idx);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct BinderThread {
    /// Weak reference to self.
    weak_self: WeakRef<BinderThread>,

    tid: pid_t,
    /// The mutable state of the binder thread, protected by a single lock.
    state: Mutex<BinderThreadState>,
}

impl BinderThread {
    fn new(_binder_proc: &BinderProcessGuard<'_>, tid: pid_t) -> OwnedRef<Self> {
        let state = Mutex::new(BinderThreadState::new(tid));
        #[cfg(any(test, debug_assertions))]
        {
            // The state must be acquired after the mutable state from the `BinderProcess` and before
            // `command_queue`. `binder_proc` being a guard, the mutable state of `BinderProcess` is
            // already locked.
            let _l1 = state.lock();
            let _l2 = _binder_proc.base.command_queue.lock();
        }
        OwnedRef::new_cyclic(|weak_self| Self { weak_self, tid, state })
    }

    /// Acquire the lock to the binder thread's mutable state.
    pub fn lock(&self) -> MutexGuard<'_, BinderThreadState> {
        self.state.lock()
    }
}

impl Releasable for BinderThread {
    type Context<'a: 'b, 'b> = &'a Kernel;

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        self.state.into_inner().release(context);
    }
}

/// The mutable state of a binder thread.
#[derive(Debug)]
struct BinderThreadState {
    tid: pid_t,
    /// The registered state of the thread.
    registration: RegistrationState,
    /// The stack of transactions that are active for this thread.
    transactions: Vec<TransactionRole>,
    /// The binder driver uses this queue to communicate with a binder thread. When a binder thread
    /// issues a [`uapi::BINDER_WRITE_READ`] ioctl, it will read from this command queue.
    command_queue: CommandQueueWithWaitQueue,
    /// The thread should finish waiting without returning anything, then reset the flag. Used by
    /// kick_all_threads by flush.
    request_kick: bool,
}

impl BinderThreadState {
    fn new(tid: pid_t) -> Self {
        Self {
            tid,
            registration: RegistrationState::default(),
            transactions: Default::default(),
            command_queue: Default::default(),
            request_kick: false,
        }
    }

    pub fn is_main_or_registered(&self) -> bool {
        self.registration != RegistrationState::Unregistered
    }

    pub fn is_registered(&self) -> bool {
        self.registration == RegistrationState::Auxilliary
    }

    pub fn is_available(&self) -> bool {
        if !self.is_main_or_registered() {
            log_trace!("thread {:?} not registered {:?}", self.tid, self.registration);
            return false;
        }
        if !self.command_queue.is_empty() {
            log_trace!("thread {:?} has non empty queue", self.tid);
            return false;
        }
        if !self.command_queue.has_waiters() {
            log_trace!("thread {:?} is not waiting", self.tid);
            return false;
        }
        if !self.transactions.is_empty() {
            log_trace!("thread {:?} is in a transaction {:?}", self.tid, self.transactions);
            return false;
        }
        log_trace!("thread {:?} is available", self.tid);
        true
    }

    /// Handle a binder thread's request to register itself with the binder driver.
    /// This makes the binder thread eligible for receiving commands from the driver.
    pub fn handle_looper_registration(
        &mut self,
        binder_process: &mut BinderProcessGuard<'_>,
        registration: RegistrationState,
    ) -> Result<(), Errno> {
        log_trace!("BinderThreadState id={} looper registration", self.tid);
        if self.is_main_or_registered() {
            // This thread is already registered.
            error!(EINVAL)
        } else {
            if registration == RegistrationState::Auxilliary {
                binder_process.thread_requested = false;
            }
            self.registration = registration;
            Ok(())
        }
    }

    /// Enqueues `command` for the thread and wakes it up if necessary.
    pub fn enqueue_command(&mut self, command: Command) {
        log_trace!("BinderThreadState id={} enqueuing command {:?}", self.tid, command);
        self.command_queue.push_back(command);
    }

    /// Get the binder process and thread to reply to, or fail if there is no ongoing transaction or
    /// the calling process/thread are dead.
    pub fn pop_transaction_caller(
        &mut self,
        current_task: &CurrentTask,
    ) -> Result<
        (TempRef<'static, BinderProcess>, TempRef<'static, BinderThread>, SchedulerGuard),
        TransactionError,
    > {
        let transaction = self.transactions.pop().ok_or_else(|| errno!(EINVAL))?;
        match transaction {
            TransactionRole::Receiver(peer, policy) => {
                log_trace!(
                    "binder transaction popped from thread {} for peer {:?}",
                    self.tid,
                    peer
                );
                let (process, thread) = release_on_error!(policy, current_task, {
                    peer.upgrade().ok_or(TransactionError::Dead)
                });
                Ok((process, thread, policy))
            }
            TransactionRole::Sender => {
                log_warn!("caller got confused, nothing to reply to!");
                error!(EINVAL)?
            }
        }
    }
}

impl Releasable for BinderThreadState {
    type Context<'a: 'b, 'b> = &'a Kernel;

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        log_trace!("Dropping BinderThreadState id={}", self.tid);
        // If there are any transactions queued, we need to tell the caller that this thread is now
        // dead.
        for command in self.command_queue.commands {
            if let Command::Transaction { sender, .. } = command {
                if let Some(sender_thread) = sender.thread.upgrade() {
                    sender_thread
                        .lock()
                        .enqueue_command(Command::DeadReply { pop_transaction: true });
                }
            }
        }

        // If there are any transactions that this thread was processing, we need to tell the caller
        // that this thread is now dead and to not expect a reply.

        // The policy need to be restored to the initial one.
        let mut updated_scheduler_policy = false;
        for transaction in self.transactions {
            if let TransactionRole::Receiver(peer, policy) = transaction {
                if !updated_scheduler_policy {
                    updated_scheduler_policy = policy.release_for_task(context, self.tid);
                } else {
                    policy.disarm();
                }
                if let Some(peer_thread) = peer.thread.upgrade() {
                    peer_thread
                        .lock()
                        .enqueue_command(Command::DeadReply { pop_transaction: true });
                }
            }
        }
    }
}

/// The registration state of a thread.
#[derive(Debug, PartialEq, Eq)]
enum RegistrationState {
    /// The thread is not registered.
    Unregistered,
    /// The thread is the main binder thread.
    Main,
    /// The thread is an auxiliary binder thread.
    Auxilliary,
}

impl Default for RegistrationState {
    fn default() -> Self {
        Self::Unregistered
    }
}

/// A pair of weak references to the process and thread of a binder transaction peer.
#[derive(Debug)]
struct WeakBinderPeer {
    proc: WeakRef<BinderProcess>,
    thread: WeakRef<BinderThread>,
}

impl WeakBinderPeer {
    fn new(proc: &BinderProcess, thread: &BinderThread) -> Self {
        Self { proc: proc.weak_self.clone(), thread: thread.weak_self.clone() }
    }

    /// Upgrades the process and thread weak references as a tuple.
    fn upgrade(&self) -> Option<(TempRef<'static, BinderProcess>, TempRef<'static, BinderThread>)> {
        self.proc
            .upgrade()
            .map(TempRef::into_static)
            .zip(self.thread.upgrade().map(TempRef::into_static))
    }
}

/// Commands for a binder thread to execute.
#[derive(Debug)]
enum Command {
    /// Notifies a binder thread that a remote process acquired a strong reference to the specified
    /// binder object. The object should not be destroyed until a [`Command::ReleaseRef`] is
    /// delivered.
    AcquireRef(LocalBinderObject),
    /// Notifies a binder thread that there are no longer any remote processes holding strong
    /// references to the specified binder object. The object may still have references within the
    /// owning process.
    ReleaseRef(LocalBinderObject),
    /// Notifies a binder thread that a remote process acquired a weak reference to the specified
    /// binder object. The object should not be destroyed until a [`Command::DecRef`] is
    /// delivered.
    IncRef(LocalBinderObject),
    /// Notifies a binder thread that there are no longer any remote processes holding weak
    /// references to the specified binder object. The object may still have references within the
    /// owning process.
    DecRef(LocalBinderObject),
    /// Notifies a binder thread that the last processed command contained an error.
    Error(i32),
    /// Commands a binder thread to start processing an incoming oneway transaction, which requires
    /// no reply.
    OnewayTransaction(TransactionData),
    /// Commands a binder thread to start processing an incoming synchronous transaction from
    /// another binder process.
    /// Sent from the client to the server.
    Transaction {
        /// The binder peer that sent this transaction.
        sender: WeakBinderPeer,
        /// The transaction payload.
        data: TransactionData,
        /// The eventual policy to use when the transaction is running.
        scheduler_policy: Option<SchedulerPolicy>,
    },
    /// Commands a binder thread to process an incoming reply to its transaction.
    /// Sent from the server to the client.
    Reply(TransactionData),
    /// Notifies a binder thread that a transaction has completed.
    /// Sent from binder to the server.
    TransactionComplete,
    /// Notifies a binder thread that a oneway transaction has been sent.
    /// Sent from binder to the client.
    OnewayTransactionComplete,
    /// The transaction was well formed but failed. Possible causes are a nonexistent handle, no
    /// more memory available to allocate a buffer.
    FailedReply,
    /// Notifies the initiator of a transaction that the recipient is dead.
    DeadReply {
        /// Whether the initiator must pop the `TransactionRole::Sender` from its transaction
        /// stack.
        pop_transaction: bool,
    },
    /// Notifies a binder process that a binder object has died.
    DeadBinder(binder_uintptr_t),
    /// Notifies a binder process that the death notification has been cleared.
    ClearDeathNotificationDone(binder_uintptr_t),
    /// Notified the binder process that it should spawn a new looper.
    SpawnLooper,
}

impl Command {
    /// Returns the command's BR_* code for serialization.
    fn driver_return_code(&self) -> binder_driver_return_protocol {
        match self {
            Self::AcquireRef(..) => binder_driver_return_protocol_BR_ACQUIRE,
            Self::ReleaseRef(..) => binder_driver_return_protocol_BR_RELEASE,
            Self::IncRef(..) => binder_driver_return_protocol_BR_INCREFS,
            Self::DecRef(..) => binder_driver_return_protocol_BR_DECREFS,
            Self::Error(..) => binder_driver_return_protocol_BR_ERROR,
            Self::OnewayTransaction(data) | Self::Transaction { data, .. } => {
                if data.buffers.security_context.is_none() {
                    binder_driver_return_protocol_BR_TRANSACTION
                } else {
                    binder_driver_return_protocol_BR_TRANSACTION_SEC_CTX
                }
            }
            Self::Reply(..) => binder_driver_return_protocol_BR_REPLY,
            Self::TransactionComplete | Self::OnewayTransactionComplete => {
                binder_driver_return_protocol_BR_TRANSACTION_COMPLETE
            }
            Self::FailedReply => binder_driver_return_protocol_BR_FAILED_REPLY,
            Self::DeadReply { .. } => binder_driver_return_protocol_BR_DEAD_REPLY,
            Self::DeadBinder(..) => binder_driver_return_protocol_BR_DEAD_BINDER,
            Self::ClearDeathNotificationDone(..) => {
                binder_driver_return_protocol_BR_CLEAR_DEATH_NOTIFICATION_DONE
            }
            Self::SpawnLooper => binder_driver_return_protocol_BR_SPAWN_LOOPER,
        }
    }

    /// Serializes and writes the command into userspace memory at `buffer`.
    fn write_to_memory(
        &self,
        resource_accessor: &dyn ResourceAccessor,
        buffer: &UserBuffer,
    ) -> Result<usize, Errno> {
        match self {
            Self::AcquireRef(obj)
            | Self::ReleaseRef(obj)
            | Self::IncRef(obj)
            | Self::DecRef(obj) => {
                #[repr(C, packed)]
                #[derive(IntoBytes, Immutable)]
                struct AcquireRefData {
                    command: binder_driver_return_protocol,
                    weak_ref_addr: u64,
                    strong_ref_addr: u64,
                }
                if buffer.length < std::mem::size_of::<AcquireRefData>() {
                    return error!(ENOMEM);
                }
                resource_accessor.write_object(
                    UserRef::new(buffer.address),
                    &AcquireRefData {
                        command: self.driver_return_code(),
                        weak_ref_addr: obj.weak_ref_addr.ptr() as u64,
                        strong_ref_addr: obj.strong_ref_addr.ptr() as u64,
                    },
                )
            }
            Self::Error(error_val) => {
                #[repr(C, packed)]
                #[derive(IntoBytes, Immutable)]
                struct ErrorData {
                    command: binder_driver_return_protocol,
                    error_val: i32,
                }
                if buffer.length < std::mem::size_of::<ErrorData>() {
                    return error!(ENOMEM);
                }
                resource_accessor.write_object(
                    UserRef::new(buffer.address),
                    &ErrorData { command: self.driver_return_code(), error_val: *error_val },
                )
            }
            Self::OnewayTransaction(data) | Self::Transaction { data, .. } | Self::Reply(data) => {
                if let Some(security_context_buffer) = data.buffers.security_context.as_ref() {
                    #[repr(C, packed)]
                    #[derive(IntoBytes, Immutable)]
                    struct TransactionData {
                        command: binder_driver_return_protocol,
                        data: [u8; std::mem::size_of::<binder_transaction_data>()],
                        secctx: binder_uintptr_t,
                    }

                    if buffer.length < std::mem::size_of::<TransactionData>() {
                        return error!(ENOMEM);
                    }
                    resource_accessor.write_object(
                        UserRef::new(buffer.address),
                        &TransactionData {
                            command: self.driver_return_code(),
                            data: data.as_bytes(),
                            secctx: security_context_buffer.address.ptr() as binder_uintptr_t,
                        },
                    )
                } else {
                    #[repr(C, packed)]
                    #[derive(IntoBytes, Immutable)]
                    struct TransactionData {
                        command: binder_driver_return_protocol,
                        data: [u8; std::mem::size_of::<binder_transaction_data>()],
                    }

                    if buffer.length < std::mem::size_of::<TransactionData>() {
                        return error!(ENOMEM);
                    }
                    resource_accessor.write_object(
                        UserRef::new(buffer.address),
                        &TransactionData {
                            command: self.driver_return_code(),
                            data: data.as_bytes(),
                        },
                    )
                }
            }
            Self::TransactionComplete
            | Self::OnewayTransactionComplete
            | Self::FailedReply
            | Self::DeadReply { .. }
            | Self::SpawnLooper => {
                if buffer.length < std::mem::size_of::<binder_driver_return_protocol>() {
                    return error!(ENOMEM);
                }
                resource_accessor
                    .write_object(UserRef::new(buffer.address), &self.driver_return_code())
            }
            Self::DeadBinder(cookie) | Self::ClearDeathNotificationDone(cookie) => {
                #[repr(C, packed)]
                #[derive(IntoBytes, Immutable)]
                struct DeadBinderData {
                    command: binder_driver_return_protocol,
                    cookie: binder_uintptr_t,
                }
                if buffer.length < std::mem::size_of::<DeadBinderData>() {
                    return error!(ENOMEM);
                }
                resource_accessor.write_object(
                    UserRef::new(buffer.address),
                    &DeadBinderData { command: self.driver_return_code(), cookie: *cookie },
                )
            }
        }
    }
}

/// The state of a given reference count for an object (strong or weak).
///
/// The client only has an eventually-consistent view of the reference count, because 1) the actual
/// reference count is maintained by the Binder driver and 2) the increment (BR_ACQUIRE/BR_INCREFS)
/// and decrement (BR_RELEASE/BR_DECREFS) commands to inform the clients of variations are
/// asynchronously delivered through multiple queues. Furthermore, clients may not process them in
/// order, as they usually handle the received commands in a thread pool.
///
/// In order to guarantee that a decrease is always processed by the client after the corresponding
/// increase (otherwise the client may incorrectly free the object), the Binder protocol mandates
/// that increase commands (BR_ACQUIRE/BR_INCREFS) must be acknowledged
/// (BC_ACQUIRE_DONE/BC_INCREFS_DONE) and, only then, the corresponding decrease
/// (BR_RELEASE/BR_DECREFS) can be enqueued.
///
/// As an optimization, we only report actionable transitions to the client, i.e. from 0 to 1 and
/// from 1 to 0.
///
/// The three states in this enum correspond to the three possible client states from the driver's
/// point of view, and the parameter is the actual reference count maintained by the Binder driver.
/// Note that there is no "transitioning from 1 to 0" state, because it is safe to pretend that
/// enqueued decreases take effect instantly.
///
/// Because of lock order constraints, it may not be possible for users of this class to enqueue
/// increase/decrease commands at the same time as the corresponding manipulations to the reference
/// count. In order to support this scenario, this class supports "deferred" operations, which are
/// equivalent to the "immediate" ones but they are divided in two steps: a first step that simply
/// records the updated reference count, and a second step which determines what commands should be
/// enqueued to propagate the new reference count to the client.
#[derive(Debug, PartialEq, Eq)]
enum ObjectReferenceCount {
    /// The count as known to the client is zero (this is both the initial state and the state after
    /// a decrease from one to zero).
    NoRef(usize),
    /// The client is transitioning from zero to one (i.e. it has been sent an increase to its
    /// reference count, but didn't yet acknowledge it).
    WaitingAck(usize),
    /// The count as known to the client is one (i.e. the client did notify that it took into
    /// account an increase of the reference count, and has not been sent a decrease since).
    HasRef(usize),
}

impl Default for ObjectReferenceCount {
    fn default() -> ObjectReferenceCount {
        ObjectReferenceCount::NoRef(0)
    }
}

impl ObjectReferenceCount {
    /// Returns true if the client's view of the reference count is either one or transitioning to
    /// one.
    fn has_ref(&self) -> bool {
        match self {
            Self::NoRef(_) => false,
            Self::WaitingAck(_) | Self::HasRef(_) => true,
        }
    }

    /// Returns the actual reference count.
    fn count(&self) -> usize {
        let (Self::NoRef(x) | Self::WaitingAck(x) | Self::HasRef(x)) = self;
        *x
    }

    /// Increments the reference count of the object and applies it immediately to the client state.
    ///
    /// If it returns true, the caller *MUST* enqueue an increment command.
    #[must_use]
    fn inc_immediate(&mut self) -> bool {
        self.inc_deferred();
        self.apply_deferred_inc()
    }

    /// Increments the reference count of the object.
    fn inc_deferred(&mut self) {
        let (Self::NoRef(ref mut x) | Self::WaitingAck(ref mut x) | Self::HasRef(ref mut x)) = self;
        *x += 1;
    }

    /// Decrements the reference count of the object.
    fn dec_deferred(&mut self) {
        let (Self::NoRef(ref mut x) | Self::WaitingAck(ref mut x) | Self::HasRef(ref mut x)) = self;
        if *x == 0 {
            panic!("dec called with no reference");
        } else {
            *x -= 1;
        }
    }

    /// Applies any deferred increment to the client state.
    ///
    /// If it returns true, the caller *MUST* enqueue an increment command.
    #[must_use]
    fn apply_deferred_inc(&mut self) -> bool {
        match *self {
            ObjectReferenceCount::NoRef(n) if n > 0 => {
                *self = ObjectReferenceCount::WaitingAck(n);
                true
            }
            _ => false,
        }
    }

    /// Applies any deferred decrement to the client state.
    ///
    /// If it returns true, the caller *MUST* enqueue a decrement command.
    #[must_use]
    fn apply_deferred_dec(&mut self) -> bool {
        if *self == ObjectReferenceCount::HasRef(0) {
            *self = ObjectReferenceCount::NoRef(0);
            true
        } else {
            false
        }
    }

    /// Acknowledge a client ack for a reference count increase.
    fn ack(&mut self) -> Result<(), Errno> {
        match self {
            Self::WaitingAck(x) => {
                *self = Self::HasRef(*x);
                Ok(())
            }
            _ => error!(EINVAL),
        }
    }

    /// Returns whether this reference count is waiting an acknowledgement for an increase.
    fn is_waiting_ack(&self) -> bool {
        matches!(self, ObjectReferenceCount::WaitingAck(_))
    }
}

/// Mutable state of a [`BinderObject`], mainly for handling the ordering guarantees of oneway
/// transactions.
#[derive(Debug, Default)]
struct BinderObjectMutableState {
    /// Command queue for oneway transactions on this binder object. Oneway transactions are
    /// guaranteed to be dispatched in the order they are submitted to the driver, and one at a
    /// time.
    oneway_transactions: VecDeque<TransactionData>,
    /// Whether a binder thread is currently handling a oneway transaction. This will get cleared
    /// when there are no more transactions in the `oneway_transactions` and a binder thread freed
    /// the buffer associated with the last oneway transaction.
    handling_oneway_transaction: bool,

    /// The weak reference count of this object.
    weak_count: ObjectReferenceCount,

    /// The strong reference count of this object.
    strong_count: ObjectReferenceCount,
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct BinderObjectFlags: u32 {
        /// Not implemented.
        const ACCEPTS_FDS = uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_ACCEPTS_FDS;
        /// Whether the binder transaction receiver wants access to the sender selinux context.
        const TXN_SECURITY_CTX = uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_TXN_SECURITY_CTX;
        /// Whether the binder transaction receiver inherit the scheduler policy of the caller.
        const INHERIT_RT = uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_INHERIT_RT;

        /// Not implemented
        const PRIORITY_MASK = uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_PRIORITY_MASK;
        /// Not implemented
        const SCHED_POLICY_MASK = uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_SCHED_POLICY_MASK;
    }
}
impl BinderObjectFlags {
    fn parse(value: u32) -> Result<Self, Errno> {
        Self::from_bits(value).ok_or_else(|| {
            log_error!("Unknown flag value for object: {:#}", value);
            errno!(EINVAL)
        })
    }

    fn get_scheduler_policy(&self) -> Option<SchedulerPolicy> {
        let bits = self.bits();
        let priority = bits & uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_PRIORITY_MASK;
        let sched_policy = (bits
            & uapi::flat_binder_object_flags_FLAT_BINDER_FLAG_SCHED_POLICY_MASK)
            >> uapi::flat_binder_object_shifts_FLAT_BINDER_FLAG_SCHED_POLICY_SHIFT;
        if priority == 0 && sched_policy == 0 {
            None
        } else {
            match SchedulerPolicy::from_binder(sched_policy, priority) {
                Ok(policy) => policy,
                Err(e) => {
                    log_warn!("Unable to parse policy {sched_policy}:{priority}: {e:?}");
                    None
                }
            }
        }
    }
}

/// A binder object, which is owned by a process. Process-local unique memory addresses identify it
/// to the owner.
#[derive(Debug)]
struct BinderObject {
    /// The owner of the binder object. If the owner cannot be promoted to a strong reference,
    /// the object is dead.
    owner: WeakRef<BinderProcess>,
    /// The addresses to the binder (weak and strong) in the owner's address space. These are
    /// treated as opaque identifiers in the driver, and only have meaning to the owning process.
    local: LocalBinderObject,
    /// The flags for the binder object.
    flags: BinderObjectFlags,
    /// Mutable state for the binder object, protected behind a mutex.
    state: Mutex<BinderObjectMutableState>,
}

/// Assert that a dropped object from a live process has no reference.
#[cfg(any(test, debug_assertions))]
impl Drop for BinderObject {
    fn drop(&mut self) {
        if self.owner.upgrade().is_some() {
            assert!(!self.has_ref());
        }
    }
}

/// Trait used to configure the RefGuard.
trait RefReleaser: std::fmt::Debug {
    /// Decrement the relevant ref count for this releaser.
    fn dec_ref(binder_object: &Arc<BinderObject>, actions: &mut RefCountActions);
}

/// The releaser for a strong reference.
#[derive(Debug)]
struct StrongRefReleaser {}

impl RefReleaser for StrongRefReleaser {
    /// Decrements the strong reference count of the binder object.
    fn dec_ref(binder_object: &Arc<BinderObject>, actions: &mut RefCountActions) {
        binder_object.lock().strong_count.dec_deferred();
        actions.insert(ArcKey(binder_object.clone()));
    }
}

/// The releaser for a weak reference.
#[derive(Debug)]
struct WeakRefReleaser {}

impl RefReleaser for WeakRefReleaser {
    /// Decrements the weak reference count of the binder object.
    fn dec_ref(binder_object: &Arc<BinderObject>, actions: &mut RefCountActions) {
        binder_object.lock().weak_count.dec_deferred();
        actions.insert(ArcKey(binder_object.clone()));
    }
}

/// The guard for a given `RefReleaser`. It is wrapped in a ReleaseGuard to ensure its lifecycle
/// constraints are handled correctly.
#[derive(Debug)]
struct RefGuardInner<R: RefReleaser> {
    binder_object: Arc<BinderObject>,
    phantom: std::marker::PhantomData<R>,
}

impl<R: RefReleaser> Releasable for RefGuardInner<R> {
    type Context<'a: 'b, 'b> = &'a mut RefCountActions;

    fn release<'a: 'b, 'b>(self, context: &mut RefCountActions) {
        R::dec_ref(&self.binder_object, context);
    }
}

/// A guard for the specified reference type. The guard must be released exactly once before going
/// out of scope to release the reference it represents.
#[derive(Debug)]
struct RefGuard<R: RefReleaser>(RefGuardInner<R>);

impl<R: RefReleaser> Deref for RefGuard<R> {
    type Target = RefGuardInner<R>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<R: RefReleaser> DerefMut for RefGuard<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<R: RefReleaser> RefGuard<R> {
    fn new(binder_object: Arc<BinderObject>) -> Self {
        RefGuard(RefGuardInner::<R> { binder_object, phantom: Default::default() })
    }
}

impl<R: RefReleaser> Releasable for RefGuard<R> {
    type Context<'a: 'b, 'b> = &'a mut RefCountActions;

    fn release<'a: 'b, 'b>(self, context: &mut RefCountActions) {
        self.0.release(context);
    }
}

/// Type alias for the specific guards for strong references.
type StrongRefGuard = RefGuard<StrongRefReleaser>;
/// Type alias for the specific guards for weak references.
type WeakRefGuard = RefGuard<WeakRefReleaser>;

impl BinderObject {
    /// Creates a new BinderObject. It is the responsibility of the caller to sent a `BR_ACQUIRE`
    /// to the owning process.
    fn new(
        owner: &BinderProcess,
        local: LocalBinderObject,
        flags: BinderObjectFlags,
    ) -> (Arc<Self>, StrongRefGuard) {
        log_trace!(
            "New binder object {:?} in process {:?} with flags {:?}",
            local,
            owner.identifier,
            flags
        );
        let object = Arc::new(Self {
            owner: owner.weak_self.clone(),
            local,
            flags,
            state: Mutex::new(BinderObjectMutableState {
                strong_count: ObjectReferenceCount::WaitingAck(1),
                ..Default::default()
            }),
        });
        let guard = StrongRefGuard::new(object.clone());
        (object, guard)
    }

    fn new_context_manager_marker(
        context_manager: &BinderProcess,
        flags: BinderObjectFlags,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner: context_manager.weak_self.clone(),
            local: Default::default(),
            flags,
            state: Default::default(),
        })
    }

    /// Locks the mutable state of the binder object for exclusive access.
    fn lock(&self) -> MutexGuard<'_, BinderObjectMutableState> {
        self.state.lock()
    }

    /// Returns whether the object has any reference, or is waiting for an acknowledgement from the
    /// owning process. The object cannot be removed from the object table has long as this is
    /// true.
    #[cfg(any(test, debug_assertions))]
    fn has_ref(&self) -> bool {
        let state = self.lock();
        state.weak_count.has_ref() || state.strong_count.has_ref()
    }

    /// Increments the strong reference count of the binder object. Allows to raise the strong
    /// count from 0 to 1.
    fn inc_strong_unchecked(self: &Arc<Self>, binder_thread: &BinderThread) -> StrongRefGuard {
        let mut state = self.lock();
        if state.strong_count.inc_immediate() {
            binder_thread.lock().enqueue_command(Command::AcquireRef(self.local));
        }
        StrongRefGuard::new(Arc::clone(self))
    }

    /// Increments the strong reference count of the binder object. Fails is the current strong
    /// count is 0.
    fn inc_strong_checked(self: &Arc<Self>) -> Result<StrongRefGuard, Errno> {
        let mut state = self.lock();
        if state.strong_count.count() == 0 {
            return error!(EINVAL);
        }
        assert!(
            !state.strong_count.inc_immediate(),
            "tried to resurrect an object that had no strong references in its owner"
        );
        Ok(StrongRefGuard::new(Arc::clone(self)))
    }

    /// Increments the weak reference count of the binder object.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn inc_weak(self: &Arc<Self>, actions: &mut RefCountActions) -> WeakRefGuard {
        self.lock().weak_count.inc_deferred();
        actions.insert(ArcKey(self.clone()));
        WeakRefGuard::new(self.clone())
    }

    /// Acknowledge the BC_ACQUIRE_DONE command received from the object owner.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn ack_acquire(self: &Arc<Self>, actions: &mut RefCountActions) -> Result<(), Errno> {
        self.lock().strong_count.ack()?;
        actions.insert(ArcKey(self.clone()));
        Ok(())
    }

    /// Acknowledge the BC_INCREFS_DONE command received from the object owner.
    /// Takes a `RefCountActions` that must be `release`d without holding a lock on
    /// `BinderProcess`.
    fn ack_incref(self: &Arc<Self>, actions: &mut RefCountActions) -> Result<(), Errno> {
        self.lock().weak_count.ack()?;
        actions.insert(ArcKey(self.clone()));
        Ok(())
    }

    fn apply_deferred_refcounts(self: &Arc<Self>) {
        let Some(process) = self.owner.upgrade() else {
            return;
        };

        let mut commands = Vec::new();

        {
            let mut process_state = process.lock();
            let mut object_state = self.lock();

            // Enqueue increase actions.
            assert!(
                !object_state.strong_count.apply_deferred_inc(),
                "The strong refcount is never incremented deferredly"
            );
            if object_state.weak_count.apply_deferred_inc() {
                commands.push(Command::IncRef(self.local));
            }

            // No decrease actions are enqueued while waiting for any acknowledgement.
            let mut did_decrease = false;
            if !object_state.strong_count.is_waiting_ack()
                && !object_state.weak_count.is_waiting_ack()
            {
                if object_state.strong_count.apply_deferred_dec() {
                    commands.push(Command::ReleaseRef(self.local));
                    did_decrease = true;
                }
                if object_state.weak_count.apply_deferred_dec() {
                    commands.push(Command::DecRef(self.local));
                    did_decrease = true;
                }
            }

            // Forget this object if we have just remove the last reference to it.
            if did_decrease
                && !object_state.strong_count.has_ref()
                && !object_state.weak_count.has_ref()
            {
                let removed = process_state.objects.remove(&self.local.weak_ref_addr);
                assert_eq!(
                    removed.as_ref().map(Arc::as_ptr),
                    Some(Arc::as_ptr(self)),
                    "Did not remove the expected BinderObject"
                );
            }
        }

        for command in commands {
            process.enqueue_command(command)
        }
    }
}

/// A binder object.
/// All addresses are in the owning process' address space.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
struct LocalBinderObject {
    /// Address to the weak ref-count structure. This uniquely identifies a binder object within
    /// a process. Guaranteed to exist.
    weak_ref_addr: UserAddress,
    /// Address to the strong ref-count structure (actual object). May not exist if the object was
    /// destroyed.
    strong_ref_addr: UserAddress,
}

/// A binder thread's role (sender or receiver) in a synchronous transaction. Oneway transactions
/// do not record roles, since they end as soon as they begin.
#[derive(Debug)]
enum TransactionRole {
    /// The binder thread initiated the transaction and is awaiting a reply from a peer.
    Sender,
    /// The binder thread is receiving a transaction and is expected to reply to the peer binder
    /// process and thread.
    Receiver(WeakBinderPeer, SchedulerGuard),
}

#[derive(Debug, Default)]
struct SchedulerGuard(Option<ReleaseGuard<SchedulerPolicy>>);

impl SchedulerGuard {
    fn release_for_task(self, kernel: &Kernel, tid: pid_t) -> bool {
        let task = kernel.pids.read().get_task(tid);
        if let Some(task) = task.upgrade() {
            self.release(&task);
            return true;
        } else {
            // The task has been killed. There is no policy to update.
            self.disarm();
            return false;
        };
    }

    fn disarm(self) {
        if let Some(policy) = self.0 {
            ReleaseGuard::take(policy);
        }
    }
}

impl From<SchedulerPolicy> for SchedulerGuard {
    fn from(policy: SchedulerPolicy) -> Self {
        Self(Some(policy.into()))
    }
}

impl Releasable for SchedulerGuard {
    type Context<'a: 'b, 'b> = &'a Task;

    fn release<'a: 'b, 'b>(self, task: Self::Context<'a, 'b>) {
        if let Some(policy) = self.0 {
            let policy = ReleaseGuard::take(policy);
            if let Err(e) = task.set_scheduler_policy(policy) {
                log_warn!("Unable to update policy of task {} to {policy:?}: {e:?}", task.id);
            }
        }
    }
}

/// Non-union version of [`binder_transaction_data`].
#[derive(Debug, PartialEq, Eq)]
struct TransactionData {
    peer_pid: pid_t,
    peer_tid: pid_t,
    peer_euid: u32,

    object: FlatBinderObject,
    code: u32,
    flags: u32,

    buffers: TransactionBuffers,
}

impl TransactionData {
    fn as_bytes(&self) -> [u8; std::mem::size_of::<binder_transaction_data>()] {
        match self.object {
            FlatBinderObject::Remote { handle } => {
                struct_with_union_into_bytes!(binder_transaction_data {
                    target.handle: handle.into(),
                    cookie: 0,
                    code: self.code,
                    flags: self.flags,
                    sender_pid: self.peer_pid,
                    sender_euid: self.peer_euid,
                    data_size: self.buffers.data.length as u64,
                    offsets_size: self.buffers.offsets.length as u64,
                    data.ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                         buffer: self.buffers.data.address.ptr() as u64,
                         offsets: self.buffers.offsets.address.ptr() as u64,
                     },
                })
            }
            FlatBinderObject::Local { ref object } => {
                struct_with_union_into_bytes!(binder_transaction_data {
                    target.ptr: object.weak_ref_addr.ptr() as u64,
                    cookie: object.strong_ref_addr.ptr() as u64,
                    code: self.code,
                    flags: self.flags,
                    sender_pid: self.peer_pid,
                    sender_euid: self.peer_euid,
                    data_size: self.buffers.data.length as u64,
                    offsets_size: self.buffers.offsets.length as u64,
                    data.ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                         buffer: self.buffers.data.address.ptr() as u64,
                         offsets: self.buffers.offsets.address.ptr() as u64,
                     },
                })
            }
        }
    }
}

/// Non-union version of [`flat_binder_object`].
#[derive(Debug, PartialEq, Eq)]
enum FlatBinderObject {
    Local { object: LocalBinderObject },
    Remote { handle: Handle },
}

/// A handle to a binder object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    /// Special handle 0 to an object representing the process which has become the context manager.
    /// Processes may rendezvous at this handle to perform service discovery.
    ContextManager,
    /// A handle to a binder object in another process.
    Object {
        /// The index of the binder object in a process' handle table.
        /// This is `handle - 1`, because the special handle 0 is reserved.
        index: usize,
    },
}

impl Handle {
    pub const fn from_raw(handle: u32) -> Handle {
        if handle == 0 {
            Handle::ContextManager
        } else {
            Handle::Object { index: handle as usize - 1 }
        }
    }

    /// Returns the underlying object index the handle represents, panicking if the handle was the
    /// special `0` handle.
    pub fn object_index(&self) -> usize {
        match self {
            Handle::ContextManager => {
                panic!("handle does not have an object index")
            }
            Handle::Object { index } => *index,
        }
    }

    pub fn is_handle_0(&self) -> bool {
        match self {
            Handle::ContextManager => true,
            Handle::Object { .. } => false,
        }
    }
}

impl From<u32> for Handle {
    fn from(handle: u32) -> Self {
        Handle::from_raw(handle)
    }
}

impl From<Handle> for u32 {
    fn from(handle: Handle) -> Self {
        match handle {
            Handle::ContextManager => 0,
            Handle::Object { index } => (index as u32) + 1,
        }
    }
}

impl std::fmt::Display for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Handle::ContextManager => f.write_str("0"),
            Handle::Object { index } => f.write_fmt(format_args!("{}", index + 1)),
        }
    }
}

/// Abstraction for accessing resources of a given process, whether it is a current process or a
/// remote one.
trait ResourceAccessor: std::fmt::Debug + MemoryAccessor {
    // File related methods.
    fn close_fd(&self, fd: FdNumber) -> Result<(), Errno>;
    fn get_file_with_flags(
        &self,
        current_task: &CurrentTask,
        fd: FdNumber,
    ) -> Result<(FileHandle, FdFlags), Errno>;
    fn add_file_with_flags(
        &self,
        locked: &mut Locked<'_, ResourceAccessorAddFile>,
        current_task: &CurrentTask,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno>;

    // State related methods.
    fn kernel(&self) -> &Arc<Kernel>;

    // Convenience method to allow passing a MemoryAccessor as a parameter.
    fn as_memory_accessor(&self) -> &dyn MemoryAccessor;
}

/// Return the `ResourceAccessor` to use to access the resources of `task`. If
/// `remote_resource_accessor` is not empty, the task is remote, and it should be used instead.
fn get_resource_accessor<'a>(
    task: &'a dyn ResourceAccessor,
    remote_resource_accessor: &'a Option<Arc<RemoteResourceAccessor>>,
) -> &'a dyn ResourceAccessor {
    if let Some(resource_accessor) = remote_resource_accessor {
        resource_accessor.as_ref()
    } else {
        task
    }
}

impl MemoryAccessorExt for dyn ResourceAccessor + '_ {}

/// Implementation of `ResourceAccessor` for a remote client.
struct RemoteResourceAccessor {
    kernel: Arc<Kernel>,
    process: zx::Process,
    process_accessor: fbinder::ProcessAccessorSynchronousProxy,
}

impl RemoteResourceAccessor {
    fn map_fidl_posix_errno(e: fposix::Errno) -> Errno {
        errno_from_code!(e.into_primitive() as i16)
    }

    fn run_file_request(
        &self,
        request: fbinder::FileRequest,
    ) -> Result<fbinder::FileResponse, Errno> {
        let result = self
            .process_accessor
            .file_request(request, zx::MonotonicInstant::INFINITE)
            .map_err(|_| errno!(ENOENT))?;
        result.map_err(|e| errno_from_code!(e.into_primitive() as i16))
    }
}

impl std::fmt::Debug for RemoteResourceAccessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteResourceAccessor").finish()
    }
}

// The maximal size of buffers that zircon supports for process_{read|write}_memory.
const MAX_PROCESS_READ_WRITE_MEMORY_BUFFER_SIZE: usize = 64 * 1024 * 1024;

impl MemoryAccessor for RemoteResourceAccessor {
    fn read_memory<'a>(
        &self,
        addr: UserAddress,
        mut unread_bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        profile_duration!("RemoteReadMemory");
        let mut addr = addr.ptr();
        let unread_bytes_ptr = unread_bytes.as_mut_ptr();
        let unread_bytes_len = unread_bytes.len();
        while !unread_bytes.is_empty() {
            let len = std::cmp::min(unread_bytes.len(), MAX_PROCESS_READ_WRITE_MEMORY_BUFFER_SIZE);
            let (read_bytes, _unread_bytes) = self
                .process
                .read_memory_uninit(addr, &mut unread_bytes[..len])
                .map_err(|_| errno!(EINVAL))?;
            let bytes_count = read_bytes.len();
            // bytes_count can be less than len when:
            // - there is a fault
            // - the reading is done across 2 mappings
            // To detect this, this only fails when nothing could be read. Otherwise, a new
            // read will be issued with the remaining of the buffer, and a fault will be
            // detected when no byte can be read.
            if bytes_count == 0 {
                return error!(EFAULT);
            }
            addr += bytes_count;
            // Note that we can't use `_unread_bytes` because it does not extend
            // to the end of `unread_bytes`. We pass `unread_bytes[..len]` to
            // `read_memory_uninit` so the returned unread bytes would be
            // `unread_bytes[bytes_count..len]` vs. `unread_bytes[bytes_count..]`
            // which is what we want.
            unread_bytes = &mut unread_bytes[bytes_count..];
        }

        debug_assert_eq!(unread_bytes.len(), 0);
        // SAFETY: [MaybeUninit<T>] and [T] have the same layout. All bytes have been
        // initialized.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(unread_bytes_ptr as *mut u8, unread_bytes_len)
        };
        Ok(bytes)
    }

    fn read_memory_partial_until_null_byte<'a>(
        &self,
        _addr: UserAddress,
        _bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        error!(ENOTSUP)
    }

    fn read_memory_partial<'a>(
        &self,
        _addr: UserAddress,
        _bytes: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], Errno> {
        error!(ENOTSUP)
    }

    fn write_memory(&self, addr: UserAddress, bytes: &[u8]) -> Result<usize, Errno> {
        profile_duration!("RemoteWriteMemory");
        let vmo = with_zx_name(
            zx::Vmo::create(bytes.len() as u64).map_err(|_| errno!(EINVAL))?,
            b"starnix:device_binder",
        );
        vmo.write(bytes, 0).map_err(|_| errno!(EFAULT))?;
        vmo.set_content_size(&(bytes.len() as u64)).map_err(|_| errno!(EINVAL))?;
        self.process_accessor
            .write_memory(addr.ptr() as u64, vmo, zx::MonotonicInstant::INFINITE)
            .map_err(|_| errno!(ENOENT))?
            .map_err(Self::map_fidl_posix_errno)?;
        Ok(bytes.len())
    }

    fn write_memory_partial(&self, _addr: UserAddress, _bytes: &[u8]) -> Result<usize, Errno> {
        error!(ENOTSUP)
    }

    fn zero(&self, _addr: UserAddress, _length: usize) -> Result<usize, Errno> {
        error!(ENOTSUP)
    }
}

impl ResourceAccessor for RemoteResourceAccessor {
    fn close_fd(&self, fd: FdNumber) -> Result<(), Errno> {
        self.run_file_request(fbinder::FileRequest {
            close_requests: Some(vec![fd.raw()]),
            ..Default::default()
        })
        .map(|_| ())
    }

    fn get_file_with_flags(
        &self,
        current_task: &CurrentTask,
        fd: FdNumber,
    ) -> Result<(FileHandle, FdFlags), Errno> {
        profile_duration!("RemoteGetFile");
        let response = self.run_file_request(fbinder::FileRequest {
            get_requests: Some(vec![fd.raw()]),
            ..Default::default()
        })?;
        if let Some(mut files) = response.get_responses {
            // Validate that the server returned a single response, as a single fd was sent.
            if files.len() == 1 {
                let file = files.pop().unwrap();
                let Some(flags) = file.flags else {
                    log_warn!("Incorrect response to file request. Missing flags.");
                    return error!(ENOENT);
                };
                if let Some(handle) = file.file {
                    return Ok((
                        new_remote_file(current_task, handle, flags.into_fidl())?,
                        FdFlags::empty(),
                    ));
                } else {
                    return Ok((new_null_file(current_task, flags.into_fidl()), FdFlags::empty()));
                }
            }
        }
        error!(ENOENT)
    }

    fn add_file_with_flags(
        &self,
        _locked: &mut Locked<'_, ResourceAccessorAddFile>,
        current_task: &CurrentTask,
        file: FileHandle,
        _flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        profile_duration!("RemoteAddFile");
        let flags: fbinder::FileFlags = file.flags().into_fidl();
        let handle = file.to_handle(current_task)?;
        let response = self.run_file_request(fbinder::FileRequest {
            add_requests: Some(vec![fbinder::FileHandle {
                file: handle,
                flags: Some(flags),
                ..fbinder::FileHandle::default()
            }]),
            ..Default::default()
        })?;
        if let Some(fds) = response.add_responses {
            // Validate that the server returned a single response, as a single file was sent.
            if fds.len() == 1 {
                let fd = fds[0];
                return Ok(FdNumber::from_raw(fd));
            }
        }
        error!(ENOENT)
    }

    fn kernel(&self) -> &Arc<Kernel> {
        &self.kernel
    }

    fn as_memory_accessor(&self) -> &dyn MemoryAccessor {
        self
    }
}

/// Implementation of `ResourceAccessor` for a local client represented as a `CurrentTask`.
impl ResourceAccessor for CurrentTask {
    fn close_fd(&self, fd: FdNumber) -> Result<(), Errno> {
        log_trace!("Closing fd {:?}", fd);
        self.files.close(fd)
    }
    fn get_file_with_flags(
        &self,
        _current_task: &CurrentTask,
        fd: FdNumber,
    ) -> Result<(FileHandle, FdFlags), Errno> {
        log_trace!("Getting file {:?} with flags", fd);
        // TODO: Should we allow O_PATH here?
        self.files.get_allowing_opath_with_flags(fd)
    }
    fn add_file_with_flags(
        &self,
        _locked: &mut Locked<'_, ResourceAccessorAddFile>,
        _current_task: &CurrentTask,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        log_trace!("Adding file {:?} with flags {:?}", file, flags);
        self.add_file(file, flags)
    }

    fn kernel(&self) -> &Arc<Kernel> {
        (self as &Task).kernel()
    }

    fn as_memory_accessor(&self) -> &dyn MemoryAccessor {
        self
    }
}

/// Implementation of `ResourceAccessor` for a local client represented as a `Task`.
impl ResourceAccessor for Task {
    fn close_fd(&self, fd: FdNumber) -> Result<(), Errno> {
        log_trace!("Closing fd {:?}", fd);
        self.files.close(fd)
    }
    fn get_file_with_flags(
        &self,
        _current_task: &CurrentTask,
        fd: FdNumber,
    ) -> Result<(FileHandle, FdFlags), Errno> {
        log_trace!("Getting file {:?} with flags", fd);
        // TODO: Should we allow O_PATH here?
        self.files.get_allowing_opath_with_flags(fd)
    }
    fn add_file_with_flags(
        &self,
        _locked: &mut Locked<'_, ResourceAccessorAddFile>,
        _current_task: &CurrentTask,
        file: FileHandle,
        flags: FdFlags,
    ) -> Result<FdNumber, Errno> {
        log_trace!("Adding file {:?} with flags {:?}", file, flags);
        self.add_file(file, flags)
    }

    fn kernel(&self) -> &Arc<Kernel> {
        (self as &Task).kernel()
    }

    fn as_memory_accessor(&self) -> &dyn MemoryAccessor {
        self
    }
}

/// Android's binder kernel driver implementation.
#[derive(Debug)]
pub struct BinderDriver {
    /// The context manager, the object represented by the zero handle.
    context_manager: Mutex<Option<Arc<BinderObject>>>,

    /// Manages the internal state of each process interacting with the binder driver.
    ///
    /// The Driver owns the BinderProcess. There can be at most one connection to the binder driver
    /// per process. When the last file descriptor to the binder in the process is closed, the
    /// value is removed from the map.
    procs: RwLock<BTreeMap<u64, OwnedRef<BinderProcess>>>,

    /// The identifier to use for the next created `BinderProcess`.
    next_identifier: AtomicU64Counter,
}

impl Releasable for BinderDriver {
    type Context<'a: 'b, 'b> = CurrentTaskAndLocked<'a, 'b>;

    fn release<'a: 'b, 'b>(mut self, context: Self::Context<'a, 'b>) {
        let (_locked, current_task) = context;
        for binder_process in std::mem::take(self.procs.get_mut()).into_values() {
            binder_process.release(current_task.kernel());
        }
    }
}

impl Default for BinderDriver {
    #[allow(clippy::let_and_return)]
    fn default() -> Self {
        let driver = Self {
            context_manager: Default::default(),
            procs: Default::default(),
            next_identifier: Default::default(),
        };
        #[cfg(any(test, debug_assertions))]
        {
            let _l1 = driver.context_manager.lock();
            let _l2 = driver.procs.read();
        }
        driver
    }
}

impl BinderDriver {
    fn find_process(&self, identifier: u64) -> Result<OwnedRef<BinderProcess>, Errno> {
        self.procs.read().get(&identifier).map(OwnedRef::share).ok_or_else(|| errno!(ENOENT))
    }

    /// Creates and register the binder process state to represent a local process with `pid`.
    fn create_local_process(&self, pid: pid_t) -> u64 {
        self.create_process(pid, None)
    }

    /// Creates and register the binder process state to represent a remote process with `pid`.
    fn create_remote_process(&self, pid: pid_t, resource_accessor: RemoteResourceAccessor) -> u64 {
        self.create_process(pid, Some(Arc::new(resource_accessor)))
    }

    /// Creates and register the binder process state to represent a process with `pid`.
    fn create_process(
        &self,
        pid: pid_t,
        resource_accessor: Option<Arc<RemoteResourceAccessor>>,
    ) -> u64 {
        let identifier = self.next_identifier.next();
        let binder_process = BinderProcess::new(identifier, pid, resource_accessor);
        assert!(
            self.procs.write().insert(identifier, binder_process).is_none(),
            "process with same pid created"
        );
        identifier
    }

    /// Creates the binder process and thread state to represent a process with `pid` and one main
    /// thread.
    #[cfg(test)]
    /// Return a `RemoteBinderConnection` that can be used to driver a remote connection to the
    /// binder device represented by this driver.
    fn create_process_and_thread(
        &self,
        pid: pid_t,
    ) -> (OwnedRef<BinderProcess>, OwnedRef<BinderThread>) {
        let identifier = self.create_local_process(pid);
        let binder_process = self.find_process(identifier).expect("find_process");
        let binder_thread = binder_process.lock().find_or_register_thread(pid);
        (binder_process, binder_thread)
    }

    /// Return a `RemoteBinderConnection` that can be used to driver a remote connection to the
    /// binder device represented by this driver.
    pub fn open_remote(
        this: &BinderDevice,
        current_task: &CurrentTask,
        process_accessor: ClientEnd<fbinder::ProcessAccessorMarker>,
        process: zx::Process,
    ) -> Arc<RemoteBinderConnection> {
        let process_accessor =
            fbinder::ProcessAccessorSynchronousProxy::new(process_accessor.into_channel());
        let identifier = this.create_remote_process(
            current_task.get_pid(),
            RemoteResourceAccessor {
                kernel: current_task.kernel().clone(),
                process_accessor,
                process,
            },
        );
        Arc::new(RemoteBinderConnection {
            binder_connection: BinderConnection { identifier, device: Arc::clone(this) },
        })
    }

    fn get_context_manager(
        &self,
        current_task: &CurrentTask,
    ) -> Result<(Arc<BinderObject>, TempRef<'_, BinderProcess>), Errno> {
        let mut context_manager = self.context_manager.lock();
        if let Some(context_manager_object) = context_manager.as_ref().cloned() {
            match context_manager_object.owner.upgrade().map(TempRef::into_static) {
                Some(owner) => {
                    return Ok((context_manager_object, owner));
                }
                None => {
                    *context_manager = None;
                }
            }
        }

        log_trace!(
            "Task {} tried to get context manager but one is not registered or dead. \
            Avoid the race condition by waiting until the context manager is ready.",
            current_task.task.id
        );
        error!(ENOENT)
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        binder_proc: &BinderProcess,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        trace_duration!(CATEGORY_STARNIX, NAME_BINDER_IOCTL, "request" => request);
        let user_arg = UserAddress::from(arg);
        let binder_thread = binder_proc.lock().find_or_register_thread(current_task.get_tid());
        release_after!(binder_thread, current_task.kernel(), {
            match request {
                uapi::BINDER_VERSION => {
                    // A thread is requesting the version of this binder driver.
                    if user_arg.is_null() {
                        return error!(EINVAL);
                    }
                    let response =
                        binder_version { protocol_version: BINDER_CURRENT_PROTOCOL_VERSION as i32 };
                    log_trace!("binder version is {:?}", response);
                    binder_proc
                        .get_resource_accessor(current_task)
                        .write_object(UserRef::new(user_arg), &response)?;
                    Ok(SUCCESS)
                }
                uapi::BINDER_SET_CONTEXT_MGR | uapi::BINDER_SET_CONTEXT_MGR_EXT => {
                    // A process is registering itself as the context manager.
                    if user_arg.is_null() {
                        return error!(EINVAL);
                    }

                    let flags = if request == uapi::BINDER_SET_CONTEXT_MGR_EXT {
                        let user_ref = UserRef::<flat_binder_object>::new(user_arg);
                        let flat_binder_object = binder_proc
                            .get_resource_accessor(current_task)
                            .read_object(user_ref)?;
                        BinderObjectFlags::parse(flat_binder_object.flags)?
                    } else {
                        BinderObjectFlags::empty()
                    };

                    log_trace!("binder setting context manager with flags {:x}", flags);

                    *self.context_manager.lock() =
                        Some(BinderObject::new_context_manager_marker(binder_proc, flags));
                    Ok(SUCCESS)
                }
                uapi::BINDER_WRITE_READ => {
                    // A thread is requesting to exchange data with the binder driver.
                    if user_arg.is_null() {
                        return error!(EINVAL);
                    }

                    let resource_accessor = binder_proc.get_resource_accessor(current_task);
                    let user_ref = UserRef::<binder_write_read>::new(user_arg);
                    let mut input = resource_accessor.read_object(user_ref)?;

                    log_trace!("binder write/read request start {:?}", input);

                    // We will be writing this back to userspace, don't trust what the client gave us.
                    input.write_consumed = 0;
                    input.read_consumed = 0;

                    if input.write_size > 0 {
                        // The calling thread wants to write some data to the binder driver.
                        let mut cursor = UserMemoryCursor::new(
                            resource_accessor.as_memory_accessor(),
                            UserAddress::from(input.write_buffer),
                            input.write_size,
                        )?;

                        // Handle all the data the calling thread sent, which may include multiple
                        // commands.
                        while cursor.bytes_read() < input.write_size as usize {
                            self.handle_thread_write(
                                locked,
                                current_task,
                                binder_proc,
                                &binder_thread,
                                &mut cursor,
                            )?;
                        }
                        input.write_consumed = cursor.bytes_read() as u64;
                    }

                    if input.read_size > 0 {
                        // The calling thread wants to read some data from the binder driver, blocking
                        // if there is nothing immediately available.
                        let read_buffer = UserBuffer {
                            address: UserAddress::from(input.read_buffer),
                            length: input.read_size as usize,
                        };
                        let read_result = match self.handle_thread_read(
                            current_task,
                            binder_proc,
                            &binder_thread,
                            &read_buffer,
                        ) {
                            // If the wait was interrupted and some command has been consumed, return a
                            // success.
                            Err(err) if err == EINTR && input.write_consumed > 0 => Ok(0),
                            r => r,
                        };
                        input.read_consumed = read_result? as u64;
                    }

                    log_trace!("binder write/read request end {:?}", input);

                    // Write back to the calling thread how much data was read/written.
                    resource_accessor.write_object(user_ref, &input)?;
                    Ok(SUCCESS)
                }
                uapi::BINDER_SET_MAX_THREADS => {
                    if user_arg.is_null() {
                        return error!(EINVAL);
                    }

                    let user_ref = UserRef::<u32>::new(user_arg);
                    let new_max_threads = binder_proc
                        .get_resource_accessor(current_task)
                        .read_object(user_ref)? as usize;
                    log_trace!("setting max binder threads to {}", new_max_threads);
                    binder_proc.lock().max_thread_count = new_max_threads;
                    Ok(SUCCESS)
                }
                uapi::BINDER_ENABLE_ONEWAY_SPAM_DETECTION => {
                    track_stub!(
                        TODO("https://fxbug.dev/322874289"),
                        "binder ENABLE_ONEWAY_SPAM_DETECTION"
                    );
                    Ok(SUCCESS)
                }
                uapi::BINDER_THREAD_EXIT => {
                    log_trace!("binder thread {} exiting", binder_thread.tid);
                    binder_proc.lock().unregister_thread(current_task, binder_thread.tid);
                    Ok(SUCCESS)
                }
                uapi::BINDER_GET_NODE_DEBUG_INFO => {
                    track_stub!(TODO("https://fxbug.dev/322874232"), "binder GET_NODE_DEBUG_INFO");
                    error!(EOPNOTSUPP)
                }
                uapi::BINDER_GET_NODE_INFO_FOR_REF => {
                    track_stub!(
                        TODO("https://fxbug.dev/322874148"),
                        "binder GET_NODE_INFO_FOR_REF"
                    );
                    error!(EOPNOTSUPP)
                }
                uapi::BINDER_FREEZE => {
                    track_stub!(TODO("https://fxbug.dev/322874189"), "binder BINDER_FREEZE");
                    error!(EOPNOTSUPP)
                }
                _ => {
                    track_stub!(
                        TODO("https://fxbug.dev/322874384"),
                        "binder unknown ioctl",
                        request
                    );
                    log_error!("binder received unknown ioctl request 0x{:08x}", request);
                    error!(EINVAL)
                }
            }
        })
    }

    /// Consumes one command from the userspace binder_write_read buffer and handles it.
    /// This method will never block.
    fn handle_thread_write<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        binder_proc: &BinderProcess,
        binder_thread: &BinderThread,
        cursor: &mut UserMemoryCursor,
    ) -> Result<(), Errno>
    where
        L: LockBefore<ResourceAccessorAddFile>,
    {
        profile_duration!("ThreadWrite");
        let command = cursor.read_object::<binder_driver_command_protocol>()?;
        let result = match command {
            binder_driver_command_protocol_BC_ENTER_LOOPER => {
                profile_duration!("EnterLooper");
                let mut proc_state = binder_proc.lock();
                binder_thread
                    .lock()
                    .handle_looper_registration(&mut proc_state, RegistrationState::Main)
            }
            binder_driver_command_protocol_BC_REGISTER_LOOPER => {
                profile_duration!("RegisterLooper");
                let mut proc_state = binder_proc.lock();
                binder_thread
                    .lock()
                    .handle_looper_registration(&mut proc_state, RegistrationState::Auxilliary)
            }
            binder_driver_command_protocol_BC_INCREFS
            | binder_driver_command_protocol_BC_ACQUIRE
            | binder_driver_command_protocol_BC_DECREFS
            | binder_driver_command_protocol_BC_RELEASE => {
                profile_duration!("Refcount");
                let handle = cursor.read_object::<u32>()?.into();
                binder_proc.handle_refcount_operation(command, handle)
            }
            binder_driver_command_protocol_BC_INCREFS_DONE
            | binder_driver_command_protocol_BC_ACQUIRE_DONE => {
                profile_duration!("RefcountDone");
                let object = LocalBinderObject {
                    weak_ref_addr: UserAddress::from(cursor.read_object::<binder_uintptr_t>()?),
                    strong_ref_addr: UserAddress::from(cursor.read_object::<binder_uintptr_t>()?),
                };
                binder_proc.handle_refcount_operation_done(command, object)
            }
            binder_driver_command_protocol_BC_FREE_BUFFER => {
                profile_duration!("FreeBuffer");
                let buffer_ptr = UserAddress::from(cursor.read_object::<binder_uintptr_t>()?);
                binder_proc.handle_free_buffer(buffer_ptr)
            }
            binder_driver_command_protocol_BC_REQUEST_DEATH_NOTIFICATION => {
                profile_duration!("RequestDeathNotif");
                let handle = cursor.read_object::<u32>()?.into();
                let cookie = cursor.read_object::<binder_uintptr_t>()?;
                binder_proc.handle_request_death_notification(handle, cookie)
            }
            binder_driver_command_protocol_BC_CLEAR_DEATH_NOTIFICATION => {
                profile_duration!("ClearDeathNotif");
                let handle = cursor.read_object::<u32>()?.into();
                let cookie = cursor.read_object::<binder_uintptr_t>()?;
                binder_proc.handle_clear_death_notification(handle, cookie)
            }
            binder_driver_command_protocol_BC_DEAD_BINDER_DONE => {
                profile_duration!("DeadBinderDone");
                let _cookie = cursor.read_object::<binder_uintptr_t>()?;
                Ok(())
            }
            binder_driver_command_protocol_BC_TRANSACTION => {
                profile_duration!("Transaction");
                let data = cursor.read_object::<binder_transaction_data>()?;
                self.handle_transaction(
                    locked,
                    current_task,
                    binder_proc,
                    binder_thread,
                    binder_transaction_data_sg { transaction_data: data, buffers_size: 0 },
                )
                .or_else(|err| err.dispatch(binder_thread))
            }
            binder_driver_command_protocol_BC_REPLY => {
                profile_duration!("Reply");
                let data = cursor.read_object::<binder_transaction_data>()?;
                self.handle_reply(
                    locked,
                    current_task,
                    binder_proc,
                    binder_thread,
                    binder_transaction_data_sg { transaction_data: data, buffers_size: 0 },
                )
                .or_else(|err| err.dispatch(binder_thread))
            }
            binder_driver_command_protocol_BC_TRANSACTION_SG => {
                profile_duration!("Transaction");
                let data = cursor.read_object::<binder_transaction_data_sg>()?;
                self.handle_transaction(locked, current_task, binder_proc, binder_thread, data)
                    .or_else(|err| err.dispatch(binder_thread))
            }
            binder_driver_command_protocol_BC_REPLY_SG => {
                profile_duration!("Reply");
                let data = cursor.read_object::<binder_transaction_data_sg>()?;
                self.handle_reply(locked, current_task, binder_proc, binder_thread, data)
                    .or_else(|err| err.dispatch(binder_thread))
            }
            _ => {
                log_error!("binder received unknown RW command: {:#08x}", command);
                error!(EINVAL)
            }
        };

        if let Err(err) = &result {
            // TODO(https://fxbug.dev/42068804): Right now there are many errors that happen that are due to
            // errors in the kernel driver and not because of an issue in userspace. Until the
            // driver is more stable, log these.
            log_error!("binder command {:#x} failed: {:?}", command, err);
        }
        result
    }

    /// A binder thread is starting a transaction on a remote binder object.
    fn handle_transaction<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        binder_proc: &BinderProcess,
        binder_thread: &BinderThread,
        data: binder_transaction_data_sg,
    ) -> Result<(), TransactionError>
    where
        L: LockBefore<ResourceAccessorAddFile>,
    {
        // SAFETY: Transactions can only refer to handles.
        let handle = unsafe { data.transaction_data.target.handle }.into();

        let (object, target_proc, mut guard) = match handle {
            Handle::ContextManager => {
                let (object, owner) = self.get_context_manager(current_task)?;
                (object, Some(owner), None)
            }
            Handle::Object { index } => {
                let binder_proc = binder_proc.lock();
                let (object, guard) =
                    binder_proc.handles.get(index).ok_or(TransactionError::Failure)?;
                let owner = object.owner.upgrade().map(TempRef::into_static);
                (object, owner, Some(guard))
            }
        };

        let mut actions = RefCountActions::default();
        release_after!(actions, (), {
            release_after!(guard, &mut actions, {
                let target_proc = target_proc.ok_or(TransactionError::Dead)?;
                let weak_task = current_task.get_task(target_proc.pid);
                let target_task = weak_task.upgrade().ok_or(TransactionError::Dead)?;
                let security_context: Option<FsString> =
                    if object.flags.contains(BinderObjectFlags::TXN_SECURITY_CTX) {
                        let mut security_context = FsString::from(
                            security::task_get_context(&current_task, &target_task)
                                .unwrap_or_default(),
                        );
                        security_context.push(b'\0');
                        Some(security_context)
                    } else {
                        None
                    };

                // Copy the transaction data to the target process.
                let (buffers, mut transaction_state) = self.copy_transaction_buffers(
                    locked,
                    current_task,
                    binder_proc.get_resource_accessor(current_task),
                    binder_proc,
                    binder_thread,
                    target_proc.get_resource_accessor(target_task.deref()),
                    &target_proc,
                    &data,
                    security_context.as_ref().map(|c| c.as_ref()),
                )?;

                let transaction = TransactionData {
                    peer_pid: binder_proc.pid,
                    peer_tid: binder_thread.tid,
                    peer_euid: current_task.creds().euid,
                    object: {
                        if handle.is_handle_0() {
                            // This handle (0) always refers to the context manager, which is always
                            // "remote", even for the context manager itself.
                            FlatBinderObject::Remote { handle }
                        } else {
                            FlatBinderObject::Local { object: object.local }
                        }
                    },
                    code: data.transaction_data.code,
                    flags: data.transaction_data.flags,

                    buffers: buffers.clone(),
                };

                if let Some(guard) = guard.take() {
                    transaction_state.push_guard(guard);
                }

                let (target_thread, command) =
                    if data.transaction_data.flags & transaction_flags_TF_ONE_WAY != 0 {
                        profile_duration!("TransactionOneWay");
                        // The caller is not expecting a reply.
                        binder_thread.lock().enqueue_command(Command::OnewayTransactionComplete);

                        // Register the transaction buffer.
                        target_proc.lock().active_transactions.insert(
                            buffers.data.address,
                            ActiveTransaction {
                                request_type: RequestType::Oneway { object: object.clone() },
                                state: transaction_state.into_state(),
                            }
                            .into(),
                        );

                        // Oneway transactions are enqueued on the binder object and processed one at a time.
                        // This guarantees that oneway transactions are processed in the order they are
                        // submitted, and one at a time.
                        let mut object_state = object.lock();
                        if object_state.handling_oneway_transaction {
                            // Currently, a oneway transaction is being handled. Queue this one so that it is
                            // scheduled when the buffer from the in-progress transaction is freed.
                            object_state.oneway_transactions.push_back(transaction);
                            return Ok(());
                        }

                        // No oneway transactions are being handled, which means that no buffer will be
                        // freed, kicking off scheduling from the oneway queue. Instead, we must schedule
                        // the transaction regularly, but mark the object as handling a oneway transaction.
                        object_state.handling_oneway_transaction = true;

                        (None, Command::OnewayTransaction(transaction))
                    } else {
                        profile_duration!("TransactionTwoWay");
                        let target_thread = match match binder_thread.lock().transactions.last() {
                            Some(TransactionRole::Receiver(rx, _)) => rx.upgrade(),
                            _ => None,
                        } {
                            Some((proc, thread)) if proc.pid == target_proc.pid => Some(thread),
                            _ => None,
                        };

                        // Make the sender thread part of the transaction so it doesn't get scheduled to handle
                        // any other transactions.
                        binder_thread.lock().transactions.push(TransactionRole::Sender);

                        // Register the transaction buffer.
                        target_proc.lock().active_transactions.insert(
                            buffers.data.address,
                            ActiveTransaction {
                                request_type: RequestType::RequestResponse,
                                state: transaction_state.into_state(),
                            }
                            .into(),
                        );

                        // The object flags have 2 ways to declare a scheduler policy for the
                        // transaction.
                        // 1. It might contains a specific minimal policy to use.
                        // 2. It might declare that the transaction supports priority inheritance.
                        // The results must always be the best policy according to these rules.
                        let mut scheduler_policy = object.flags.get_scheduler_policy();
                        if object.flags.contains(BinderObjectFlags::INHERIT_RT) {
                            let current_policy = current_task.read().scheduler_policy;
                            scheduler_policy = scheduler_policy
                                .map(|p| if p > current_policy { p } else { current_policy })
                                .or(Some(current_policy));
                        }

                        (
                            target_thread,
                            Command::Transaction {
                                sender: WeakBinderPeer::new(binder_proc, binder_thread),
                                data: transaction,
                                scheduler_policy,
                            },
                        )
                    };

                // Schedule the transaction on the target_thread if it is specified, otherwise use the
                // process' command queue.
                if let Some(target_thread) = target_thread {
                    target_thread.lock().enqueue_command(command);
                } else {
                    target_proc.enqueue_command(command);
                }
                Ok(())
            })
        })
    }

    /// A binder thread is sending a reply to a transaction.
    fn handle_reply<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        binder_proc: &BinderProcess,
        binder_thread: &BinderThread,
        data: binder_transaction_data_sg,
    ) -> Result<(), TransactionError>
    where
        L: LockBefore<ResourceAccessorAddFile>,
    {
        // Find the process and thread that initiated the transaction. This reply is for them.
        let (target_proc, target_thread, policy) =
            binder_thread.lock().pop_transaction_caller(current_task)?;
        if let Err(e) = release_after!(policy, current_task, || -> Result<(), TransactionError> {
            let weak_task = current_task.get_task(target_proc.pid);
            let target_task = weak_task.upgrade().ok_or(TransactionError::Dead)?;

            // Copy the transaction data to the target process.
            let (buffers, transaction_state) = self.copy_transaction_buffers(
                locked,
                current_task,
                binder_proc.get_resource_accessor(current_task),
                binder_proc,
                binder_thread,
                target_proc.get_resource_accessor(target_task.deref()),
                &target_proc,
                &data,
                None,
            )?;

            // Register the transaction buffer.
            target_proc.lock().active_transactions.insert(
                buffers.data.address,
                ActiveTransaction {
                    request_type: RequestType::RequestResponse,
                    state: transaction_state.into_state(),
                }
                .into(),
            );

            // Schedule the transaction on the target process' command queue.
            target_thread.lock().enqueue_command(Command::Reply(TransactionData {
                peer_pid: binder_proc.pid,
                peer_tid: binder_thread.tid,
                peer_euid: current_task.creds().euid,

                object: FlatBinderObject::Remote { handle: Handle::ContextManager },
                code: data.transaction_data.code,
                flags: data.transaction_data.flags,

                buffers,
            }));

            // Schedule the transaction complete command on the caller's command queue.
            binder_thread.lock().enqueue_command(Command::TransactionComplete);

            Ok(())
        }) {
            // Sending to the target process failed, notify of the transaction failure.
            let _ = e.dispatch(&target_thread);
            return Err(e);
        }
        Ok(())
    }

    /// Select which command queue to read from, preferring the thread-local one.
    /// If a transaction is pending, deadlocks can happen if reading from the process queue.
    fn get_active_queue<'a>(
        thread_state: &'a mut BinderThreadState,
        proc_command_queue: &'a mut CommandQueueWithWaitQueue,
    ) -> &'a mut CommandQueueWithWaitQueue {
        if !thread_state.command_queue.is_empty() || !thread_state.transactions.is_empty() {
            &mut thread_state.command_queue
        } else {
            proc_command_queue
        }
    }

    /// Dequeues a command from the thread's commands' queue, or blocks until commands are available.
    fn handle_thread_read(
        &self,
        current_task: &CurrentTask,
        binder_proc: &BinderProcess,
        binder_thread: &BinderThread,
        read_buffer: &UserBuffer,
    ) -> Result<usize, Errno> {
        profile_duration!("ThreadRead");
        let resource_accessor = binder_proc.get_resource_accessor(current_task);
        loop {
            {
                profile_duration!("RequestThread");
                let mut binder_proc_state = binder_proc.lock();

                if binder_proc_state.should_request_thread(binder_thread) {
                    let bytes_written =
                        Command::SpawnLooper.write_to_memory(resource_accessor, read_buffer)?;
                    binder_proc_state.did_request_thread();
                    return Ok(bytes_written);
                }
            }

            // THREADING: Always acquire the [`BinderThread::state`] lock before the
            // [`BinderProcess::command_queue`] lock or else it may lead to deadlock.
            let mut thread_state = binder_thread.lock();
            let mut proc_command_queue = binder_proc.command_queue.lock();

            if thread_state.request_kick {
                thread_state.request_kick = false;
                return Ok(0);
            }

            // Select which command queue to read from, preferring the thread-local one.
            // If a transaction is pending, deadlocks can happen if reading from the process queue.
            let command_queue = Self::get_active_queue(&mut thread_state, &mut proc_command_queue);

            if let Some(command) = command_queue.pop_front() {
                profile_duration!("ThreadReadCommand");
                // Attempt to write the command to the thread's buffer.
                let bytes_written = command.write_to_memory(resource_accessor, read_buffer)?;
                match command {
                    Command::Transaction { sender, scheduler_policy, .. } => {
                        // The transaction is synchronous and we're expected to give a reply, so
                        // push the transaction onto the transaction stack.

                        // If the transaction must inherit the sender policy, let update the
                        // scheduler policy, and keep track of the previous one.
                        let scheduler_policy = (|| {
                            if let Some(policy) = scheduler_policy {
                                let old_policy = current_task.read().scheduler_policy;
                                if old_policy < policy {
                                    match current_task.set_scheduler_policy(policy) {
                                        Ok(()) => return SchedulerGuard::from(old_policy),
                                        Err(e) => {
                                            log_warn!("Unable to update policy of task {} to {policy:?}: {e:?}", current_task.id);
                                        }
                                    }
                                }
                            }
                            SchedulerGuard::default()
                        })();
                        let tx = TransactionRole::Receiver(sender, scheduler_policy);
                        thread_state.transactions.push(tx);
                    }
                    Command::DeadReply { pop_transaction: true } | Command::Reply(..) => {
                        // The sender got a reply, pop the sender entry from the transaction stack.
                        let transaction =
                            thread_state.transactions.pop().expect("transaction stack underflow!");
                        // Command::Reply is sent to the receiver side. So the popped transaction
                        // must be a Sender role.
                        assert!(matches!(transaction, TransactionRole::Sender));
                    }
                    Command::TransactionComplete
                    | Command::OnewayTransaction(..)
                    | Command::OnewayTransactionComplete
                    | Command::AcquireRef(..)
                    | Command::ReleaseRef(..)
                    | Command::IncRef(..)
                    | Command::DecRef(..)
                    | Command::Error(..)
                    | Command::FailedReply
                    | Command::DeadReply { .. }
                    | Command::DeadBinder(..)
                    | Command::ClearDeathNotificationDone(..)
                    | Command::SpawnLooper => {}
                }

                return Ok(bytes_written);
            }

            // No commands readily available to read. Wait for work. The thread will wait on both
            // the thread queue and the process queue, and loop back to check whether some work is
            // available.
            profile_duration!("ThreadReadWaitForCommand");
            let event = InterruptibleEvent::new();
            let (mut waiter, guard) = SimpleWaiter::new(&event);
            proc_command_queue.wait_async_simple(&mut waiter);
            thread_state.command_queue.wait_async_simple(&mut waiter);
            drop(thread_state);
            drop(proc_command_queue);

            {
                let mut proc_state = binder_proc.lock();
                // Ensure the file descriptor has not been closed or interrupted, after registering
                // for the waiters but before waiting.
                if proc_state.closed {
                    return error!(EBADF);
                }

                if proc_state.interrupted {
                    proc_state.interrupted = false;
                    return error!(EINTR);
                }
            }

            // Put this thread to sleep.
            current_task.block_until(guard, zx::MonotonicInstant::INFINITE)?;
        }
    }

    /// Copies transaction buffers from the source process' address space to a new buffer in the
    /// target process' shared binder VMO.
    /// Returns the transaction buffers in the target process, as well as the transaction state.
    ///
    /// If `security_context` is present, it must be null terminated.
    fn copy_transaction_buffers<'a, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        source_resource_accessor: &dyn ResourceAccessor,
        source_proc: &BinderProcess,
        source_thread: &BinderThread,
        target_resource_accessor: &'a dyn ResourceAccessor,
        target_proc: &BinderProcess,
        data: &binder_transaction_data_sg,
        security_context: Option<&FsStr>,
    ) -> Result<(TransactionBuffers, TransientTransactionState<'a>), TransactionError>
    where
        L: LockBefore<ResourceAccessorAddFile>,
    {
        profile_duration!("CopyTransactionBuffers");

        // Get the shared memory of the target process.
        let mut shared_memory_lock = target_proc.shared_memory.lock();
        let shared_memory = shared_memory_lock.as_mut().ok_or_else(|| errno!(ENOMEM))?;

        // Allocate a buffer from the target process' shared memory.
        let mut allocations = shared_memory.allocate_buffers(
            data.transaction_data.data_size as usize,
            data.transaction_data.offsets_size as usize,
            data.buffers_size as usize,
            round_up_to_increment(
                security_context.map(|s| s.len()).unwrap_or(0),
                std::mem::size_of::<binder_uintptr_t>(),
            )?,
        )?;

        // Copy the security context content.
        if let Some(data) = security_context {
            let security_buffer = allocations.security_context_buffer.as_mut().unwrap();
            security_buffer.as_mut_bytes()[..data.len()].copy_from_slice(data);
        }

        // SAFETY: `binder_transaction_data` was read from a userspace VMO, which means that all
        // bytes are defined, making union access safe (even if the value is garbage).
        let userspace_addrs = unsafe { data.transaction_data.data.ptr };

        // Copy the data straight into the target's buffer.
        source_resource_accessor.read_memory_to_slice(
            UserAddress::from(userspace_addrs.buffer),
            allocations.data_buffer.as_mut_bytes(),
        )?;
        source_resource_accessor.read_objects_to_slice(
            UserRef::new(UserAddress::from(userspace_addrs.offsets)),
            allocations.offsets_buffer.as_mut_bytes(),
        )?;

        // Translate any handles/fds from the source process' handle table to the target process'
        // handle table.
        let transient_transaction_state = self.translate_objects(
            locked,
            current_task,
            source_resource_accessor,
            source_proc,
            source_thread,
            target_resource_accessor,
            target_proc,
            allocations.offsets_buffer.as_bytes(),
            allocations.data_buffer.as_mut_bytes(),
            &mut allocations.scatter_gather_buffer,
        )?;

        Ok((allocations.into(), transient_transaction_state))
    }

    /// Translates binder object handles/FDs from the sending process to the receiver process,
    /// patching the transaction data as needed.
    ///
    /// When a binder object is sent from one process to another, it must be added to the receiving
    /// process' handle table. Conversely, a handle being sent to the process that owns the
    /// underlying binder object should receive the actual pointers to the object.
    ///
    /// When a binder buffer object is sent from one process to another, the buffer described by the
    /// buffer object must be copied into the receiver's address space.
    ///
    /// When a binder file descriptor object is sent from one process to another, the file
    /// descriptor must be `dup`-ed into the receiver's FD table.
    ///
    /// Returns [`TransientTransactionState`], which contains the handles in the target process'
    /// handle table for which temporary strong references were acquired, along with duped FDs. This
    /// object takes care of releasing these resources when dropped, due to an error or a
    /// `BC_FREE_BUFFER` command.
    fn translate_objects<'a, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        source_resource_accessor: &dyn ResourceAccessor,
        source_proc: &BinderProcess,
        source_thread: &BinderThread,
        target_resource_accessor: &'a dyn ResourceAccessor,
        target_proc: &BinderProcess,
        offsets: &[binder_uintptr_t],
        transaction_data: &mut [u8],
        sg_buffer: &mut SharedBuffer<'_, u8>,
    ) -> Result<TransientTransactionState<'a>, TransactionError>
    where
        L: LockBefore<ResourceAccessorAddFile>,
    {
        profile_duration!("TranslateObjects");

        let mut transaction_state =
            TransientTransactionState::new(target_resource_accessor, target_proc);
        release_on_error!(transaction_state, (), {
            let mut sg_remaining_buffer = sg_buffer.user_buffer();
            let mut sg_buffer_offset = 0;
            for (offset_idx, object_offset) in offsets.iter().map(|o| *o as usize).enumerate() {
                // Bounds-check the offset.
                if object_offset >= transaction_data.len() {
                    return error!(EINVAL)?;
                }
                let serialized_object =
                    SerializedBinderObject::from_bytes(&transaction_data[object_offset..])?;
                let translated_object = match serialized_object {
                    SerializedBinderObject::Handle { handle, flags, cookie } => {
                        match handle {
                            Handle::ContextManager => {
                                // The special handle 0 does not need to be translated. It is universal.
                                serialized_object
                            }
                            Handle::Object { index } => {
                                // 1. Find the object and add a guard on it in the
                                //    transaction to ensures the receiving process keep
                                //    it alive until the transactions is finished
                                let (proxy, guard) = source_proc
                                    .lock()
                                    .handles
                                    .get(index)
                                    .ok_or(TransactionError::Failure)?;
                                if proxy.owner.as_ptr() == target_proc {
                                    // The binder object belongs to the receiving process.

                                    transaction_state.push_guard(guard);

                                    // 2. Convert the binder object from a handle to a local object.
                                    SerializedBinderObject::Object { local: proxy.local, flags }
                                } else {
                                    // The binder object does not belong to the receiving
                                    // process.

                                    // Insert the handle in the handle table of the receiving process
                                    // and add a strong reference to it to ensure it survives for the
                                    // lifetime of the transaction.
                                    let mut actions = RefCountActions::default();
                                    let new_handle = target_proc
                                        .lock()
                                        .insert_for_transaction(guard, &mut actions);
                                    actions.release(());
                                    // Tie this handle's strong reference to be held as long as this
                                    // buffer.
                                    transaction_state.push_handle(new_handle);
                                    SerializedBinderObject::Handle {
                                        handle: new_handle,
                                        flags,
                                        cookie,
                                    }
                                }
                            }
                        }
                    }
                    SerializedBinderObject::Object { local, flags } => {
                        let mut actions = RefCountActions::default();
                        release_after!(actions, (), {
                            // We are passing a binder object across process boundaries. We need
                            // to translate this address to some handle.

                            // Register this binder object if it hasn't already been registered.
                            let guard = source_proc.lock().find_or_register_object(
                                source_thread,
                                local,
                                flags,
                            );
                            // Create a handle in the receiving process that references the binder object
                            // in the sender's process.
                            let handle =
                                target_proc.lock().insert_for_transaction(guard, &mut actions);
                            // Tie this handle's strong reference to be held as long as this buffer.
                            transaction_state.push_handle(handle);

                            // Translate the serialized object into a handle.
                            SerializedBinderObject::Handle { handle, flags, cookie: 0 }
                        })
                    }
                    SerializedBinderObject::File { fd, flags, cookie } => {
                        let (file, fd_flags) =
                            source_resource_accessor.get_file_with_flags(current_task, fd)?;
                        let mut locked = locked.cast_locked::<ResourceAccessorAddFile>();
                        let new_fd = target_resource_accessor.add_file_with_flags(
                            &mut locked,
                            current_task,
                            file,
                            fd_flags,
                        )?;

                        // Close this FD if the transaction fails.
                        transaction_state.push_transient_fd(new_fd);

                        SerializedBinderObject::File { fd: new_fd, flags, cookie }
                    }
                    SerializedBinderObject::Buffer {
                        buffer,
                        length,
                        flags,
                        parent,
                        parent_offset,
                    } => {
                        // Copy the memory pointed to by this buffer object into the receiver.
                        if length > sg_remaining_buffer.length {
                            return error!(EINVAL)?;
                        }
                        source_resource_accessor.read_memory_to_slice(
                            buffer,
                            &mut sg_buffer.as_mut_bytes()
                                [sg_buffer_offset..sg_buffer_offset + length],
                        )?;

                        let translated_buffer_address = sg_remaining_buffer.address;

                        // If the buffer has a parent, it means that the parent buffer has a pointer to
                        // this buffer. This pointer will need to be translated to the receiver's
                        // address space.
                        if flags & BINDER_BUFFER_FLAG_HAS_PARENT != 0 {
                            // The parent buffer must come earlier in the object list and already be
                            // copied into the receiver's address space. Otherwise we would be fixing
                            // up memory in the sender's address space, which is marked const in the
                            // userspace runtime.
                            if parent >= offset_idx {
                                return error!(EINVAL)?;
                            }

                            // Find the parent buffer payload. There is a pointer in the buffer
                            // that points to this object.
                            let parent_buffer_payload = find_parent_buffer(
                                transaction_data,
                                sg_buffer,
                                offsets[parent] as usize,
                            )?;

                            // Bounds-check that the offset is within the buffer.
                            if parent_offset >= parent_buffer_payload.len() {
                                return error!(EINVAL)?;
                            }

                            // Patch the pointer with the translated address.
                            translated_buffer_address
                                .write_to_prefix(&mut parent_buffer_payload[parent_offset..])
                                .map_err(|_| errno!(EINVAL))?;
                        }

                        // Update the scatter-gather buffer to account for the buffer we just wrote.
                        // We pad the length of this buffer so that the next buffer starts at an aligned
                        // offset.
                        let padded_length =
                            round_up_to_increment(length, std::mem::size_of::<binder_uintptr_t>())?;
                        sg_remaining_buffer = UserBuffer {
                            address: sg_remaining_buffer.address + padded_length,
                            length: sg_remaining_buffer.length - padded_length,
                        };
                        sg_buffer_offset += padded_length;

                        // Patch this buffer with the translated address.
                        SerializedBinderObject::Buffer {
                            buffer: translated_buffer_address,
                            length,
                            flags,
                            parent,
                            parent_offset,
                        }
                    }
                    SerializedBinderObject::FileArray { num_fds, parent, parent_offset } => {
                        // The parent buffer must come earlier in the object list and already be
                        // copied into the receiver's address space. Otherwise we would be fixing
                        // up memory in the sender's address space, which is marked const in the
                        // userspace runtime.
                        if parent >= offset_idx {
                            return error!(EINVAL)?;
                        }

                        // Find the parent buffer payload. The file descriptor array is in here.
                        let parent_buffer_payload = find_parent_buffer(
                            transaction_data,
                            sg_buffer,
                            offsets[parent] as usize,
                        )?;

                        // Bounds-check that the offset is within the buffer.
                        if parent_offset >= parent_buffer_payload.len() {
                            return error!(EINVAL)?;
                        }

                        // Verify alignment and size before reading the data as a [u32].
                        let (layout, _) =
                            zerocopy::Ref::<&mut [u8], [u32]>::from_prefix_with_elems(
                                &mut parent_buffer_payload[parent_offset..],
                                num_fds,
                            )
                            .map_err(|_| errno!(EINVAL))?;
                        let fd_array = zerocopy::Ref::into_mut(layout);

                        // Dup each file descriptor and re-write the value of the new FD.
                        for fd in fd_array {
                            let (file, flags) = source_resource_accessor.get_file_with_flags(
                                current_task,
                                FdNumber::from_raw(*fd as i32),
                            )?;
                            let mut locked = locked.cast_locked::<ResourceAccessorAddFile>();
                            let new_fd = target_resource_accessor.add_file_with_flags(
                                &mut locked,
                                current_task,
                                file,
                                flags,
                            )?;

                            // Close this FD if the transaction ends either by success or failure.
                            transaction_state.push_owned_fd(new_fd);

                            *fd = new_fd.raw() as u32;
                        }

                        SerializedBinderObject::FileArray { num_fds, parent, parent_offset }
                    }
                };

                translated_object.write_to(&mut transaction_data[object_offset..])?;
            }

            Ok(())
        });
        Ok(transaction_state)
    }

    fn mmap(
        &self,
        current_task: &CurrentTask,
        binder_proc: &BinderProcess,
        addr: DesiredAddress,
        length: usize,
        prot_flags: ProtectionFlags,
        mapping_options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        profile_duration!("BinderMmap");

        // Do not support mapping shared memory more than once.
        let mut shared_memory = binder_proc.shared_memory.lock();
        if shared_memory.is_some() {
            return error!(EINVAL);
        }

        // Create a VMO that will be shared between the driver and the client process.
        let vmo = with_zx_name(
            zx::Vmo::create(length as u64).map_err(|_| errno!(ENOMEM))?,
            b"starnix:device_binder",
        );
        let memory = Arc::new(MemoryObject::from(vmo));

        // Map the VMO into the binder process' address space.
        let mm = current_task.mm().ok_or_else(|| errno!(EINVAL))?;
        let user_address = mm.map_memory(
            addr,
            memory.clone(),
            0,
            length,
            prot_flags,
            prot_flags.to_access(),
            mapping_options,
            MappingName::File(filename.into_active()),
            FileWriteGuardRef(None),
        )?;

        // Map the VMO into the driver's address space.
        match SharedMemory::map(&memory, user_address, length) {
            Ok(mem) => {
                *shared_memory = Some(mem);
                Ok(user_address)
            }
            Err(err) => {
                // Try to cleanup by unmapping from userspace, but ignore any errors. We
                // can't really recover from them.
                let _ = mm.unmap(user_address, length);
                Err(err)
            }
        }
    }

    fn wait_async(
        &self,
        binder_proc: &BinderProcess,
        binder_thread: &BinderThread,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        // THREADING: Always acquire the [`BinderThread::state`] lock before the
        // [`BinderProcess::command_queue`] lock or else it may lead to deadlock.
        let thread_state = binder_thread.lock();
        let proc_command_queue = binder_proc.command_queue.lock();

        let handler = match handler {
            EventHandler::None => EventHandler::None,
            EventHandler::Enqueue(e) => EventHandler::EnqueueOnce(Arc::new(Mutex::new(Some(e)))),
            EventHandler::EnqueueOnce(e) => EventHandler::EnqueueOnce(e),
        };

        let w1 = thread_state.command_queue.wait_async_fd_events(waiter, events, handler.clone());
        let w2 = proc_command_queue.waiters.wait_async_fd_events(waiter, events, handler);
        WaitCanceler::merge(w1, w2)
    }
}

/// Finds a buffer object's payload in the transaction. The buffer object describing the payload is
/// deserialized from `transaction_data` at `buffer_object_offset`. The actual payload is located in
/// `sg_buffer`. The buffer object must have already been validated and its payload copied to
/// `sg_buffer`. This is true for parent objects, as they are required to be processed before being
/// referenced by child objects.
fn find_parent_buffer<'a>(
    transaction_data: &[u8],
    sg_buffer: &mut SharedBuffer<'a, u8>,
    buffer_object_offset: usize,
) -> Result<&'a mut [u8], Errno> {
    // The buffer object has already been validated, since the requirement is that parent objects
    // are processed before their children. In addition, the payload has been written by us, so it
    // should be guaranteed to be valid. Still, it is possible for userspace to mutate this memory
    // while we are processing it, so we still perform checked arithmetic to avoid panics in
    // starnix.

    // Verify that the offset is within the transaction data.
    if buffer_object_offset >= transaction_data.len() {
        return error!(EINVAL);
    }

    // Deserialize the parent object buffer and extract the relevant data.
    let (buffer_payload_addr, buffer_payload_length) =
        match SerializedBinderObject::from_bytes(&transaction_data[buffer_object_offset..])? {
            SerializedBinderObject::Buffer { buffer, length, .. } => (buffer, length),
            _ => return error!(EINVAL)?,
        };

    // Calculate the start and end of the buffer payload in the scatter gather buffer.
    // The buffer payload will have been copied to the scatter gather buffer, so recover the
    // offset from its userspace address.
    if buffer_payload_addr < sg_buffer.user_buffer().address {
        // This should never happen unless userspace is messing with us, since we wrote this address
        // during translation.
        return error!(EINVAL);
    }
    let buffer_payload_start = buffer_payload_addr - sg_buffer.user_buffer().address;
    let buffer_payload_end =
        buffer_payload_start.checked_add(buffer_payload_length).ok_or_else(|| errno!(EINVAL))?;

    // Return a slice that represents the parent buffer.
    Ok(&mut sg_buffer.as_mut_bytes()[buffer_payload_start..buffer_payload_end])
}

/// Represents a serialized binder object embedded in transaction data.
#[derive(Debug, PartialEq, Eq)]
enum SerializedBinderObject {
    /// A `BINDER_TYPE_HANDLE` object. A handle to a remote binder object.
    Handle { handle: Handle, flags: BinderObjectFlags, cookie: binder_uintptr_t },
    /// A `BINDER_TYPE_BINDER` object. The in-process representation of a binder object.
    Object { local: LocalBinderObject, flags: BinderObjectFlags },
    /// A `BINDER_TYPE_FD` object. A file descriptor.
    File { fd: FdNumber, flags: BinderObjectFlags, cookie: binder_uintptr_t },
    /// A `BINDER_TYPE_PTR` object. Identifies a pointer in the transaction data that needs to be
    /// fixed up when the payload is copied into the destination process. Part of the scatter-gather
    /// implementation.
    Buffer { buffer: UserAddress, length: usize, parent: usize, parent_offset: usize, flags: u32 },
    /// A `BINDER_TYPE_FDA` object. Identifies an array of file descriptors in a parent buffer that
    /// must be duped into the receiver's file descriptor table.
    FileArray { num_fds: usize, parent: usize, parent_offset: usize },
}

impl SerializedBinderObject {
    /// Deserialize a binder object from `data`. `data` must be large enough to fit the size of the
    /// serialized object, or else this method fails.
    fn from_bytes(data: &[u8]) -> Result<Self, Errno> {
        let (object_header, _) =
            binder_object_header::read_from_prefix(data).map_err(|_| errno!(EINVAL))?;
        match object_header.type_ {
            BINDER_TYPE_BINDER => {
                let (object, _) =
                    flat_binder_object::read_from_prefix(data).map_err(|_| errno!(EINVAL))?;
                Ok(Self::Object {
                    local: LocalBinderObject {
                        // SAFETY: Union read.
                        weak_ref_addr: UserAddress::from(unsafe { object.__bindgen_anon_1.binder }),
                        strong_ref_addr: UserAddress::from(object.cookie),
                    },
                    flags: BinderObjectFlags::parse(object.flags)?,
                })
            }
            BINDER_TYPE_HANDLE => {
                let (object, _) =
                    flat_binder_object::read_from_prefix(data).map_err(|_| errno!(EINVAL))?;
                Ok(Self::Handle {
                    // SAFETY: Union read.
                    handle: unsafe { object.__bindgen_anon_1.handle }.into(),
                    flags: BinderObjectFlags::parse(object.flags)?,
                    cookie: object.cookie,
                })
            }
            BINDER_TYPE_FD => {
                let (object, _) =
                    flat_binder_object::read_from_prefix(data).map_err(|_| errno!(EINVAL))?;
                Ok(Self::File {
                    // SAFETY: Union read.
                    fd: FdNumber::from_raw(unsafe { object.__bindgen_anon_1.handle } as i32),
                    flags: BinderObjectFlags::parse(object.flags)?,
                    cookie: object.cookie,
                })
            }
            BINDER_TYPE_PTR => {
                let (object, _) =
                    binder_buffer_object::read_from_prefix(data).map_err(|_| errno!(EINVAL))?;
                Ok(Self::Buffer {
                    buffer: UserAddress::from(object.buffer),
                    length: object.length as usize,
                    parent: object.parent as usize,
                    parent_offset: object.parent_offset as usize,
                    flags: object.flags,
                })
            }
            BINDER_TYPE_FDA => {
                let (object, _) =
                    binder_fd_array_object::read_from_prefix(data).map_err(|_| errno!(EINVAL))?;
                Ok(Self::FileArray {
                    num_fds: object.num_fds as usize,
                    parent: object.parent as usize,
                    parent_offset: object.parent_offset as usize,
                })
            }
            object_type => {
                track_stub!(
                    TODO("https://fxbug.dev/322873261"),
                    "binder unknown object type",
                    object_type
                );
                error!(EINVAL)
            }
        }
    }

    /// Writes the serialized object back to `data`. `data` must be large enough to fit the
    /// serialized object, or else this method fails.
    fn write_to(self, data: &mut [u8]) -> Result<(), Errno> {
        match self {
            SerializedBinderObject::Handle { handle, flags, cookie } => {
                struct_with_union_into_bytes!(flat_binder_object {
                    hdr.type_: BINDER_TYPE_HANDLE,
                    __bindgen_anon_1.handle: handle.into(),
                    flags: flags.bits(),
                    cookie: cookie,
                })
                .write_to_prefix(data)
                .ok()
            }
            SerializedBinderObject::Object { local, flags } => {
                struct_with_union_into_bytes!(flat_binder_object {
                    hdr.type_: BINDER_TYPE_BINDER,
                    __bindgen_anon_1.binder: local.weak_ref_addr.ptr() as u64,
                    flags: flags.bits(),
                    cookie: local.strong_ref_addr.ptr() as u64,
                })
                .write_to_prefix(data)
                .ok()
            }
            SerializedBinderObject::File { fd, flags, cookie } => {
                struct_with_union_into_bytes!(flat_binder_object {
                    hdr.type_: BINDER_TYPE_FD,
                    __bindgen_anon_1.handle: fd.raw() as u32,
                    flags: flags.bits(),
                    cookie: cookie,
                })
                .write_to_prefix(data)
                .ok()
            }
            SerializedBinderObject::Buffer { buffer, length, parent, parent_offset, flags } => {
                binder_buffer_object {
                    hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                    buffer: buffer.ptr() as u64,
                    length: length as u64,
                    parent: parent as u64,
                    parent_offset: parent_offset as u64,
                    flags,
                }
                .write_to_prefix(data)
                .ok()
            }
            SerializedBinderObject::FileArray { num_fds, parent, parent_offset } => {
                binder_fd_array_object {
                    hdr: binder_object_header { type_: BINDER_TYPE_FDA },
                    pad: 0,
                    num_fds: num_fds as u64,
                    parent: parent as u64,
                    parent_offset: parent_offset as u64,
                }
                .write_to_prefix(data)
                .ok()
            }
        }
        .ok_or_else(|| errno!(EINVAL))
    }
}

/// An error processing a binder transaction/reply.
///
/// Some errors, like a malformed transaction request, should be propagated as the return value of
/// an ioctl. Other errors, like a dead recipient or invalid binder handle, should be propagated
/// through a command read by the binder thread.
///
/// This type differentiates between these strategies.
#[derive(Debug, Eq, PartialEq)]
enum TransactionError {
    /// The transaction payload was malformed. Send a [`Command::Error`] command to the issuing
    /// thread.
    Malformed(Errno),
    /// The transaction payload was correctly formed, but either the recipient, or a handle embedded
    /// in the transaction, is invalid. Send a [`Command::FailedReply`] command to the issuing
    /// thread.
    Failure,
    /// The transaction payload was correctly formed, but either the recipient, or a handle embedded
    /// in the transaction, is dead. Send a [`Command::DeadReply`] command to the issuing thread.
    Dead,
}

impl TransactionError {
    /// Dispatches the error, by potentially queueing a command to `binder_thread` and/or returning
    /// an error.
    fn dispatch(&self, binder_thread: &BinderThread) -> Result<(), Errno> {
        log_trace!("Dispatching transaction error {:?} for thread {}", self, binder_thread.tid);
        binder_thread.lock().enqueue_command(match self {
            TransactionError::Malformed(err) => {
                log_warn!(
                    "binder thread {} sent a malformed transaction: {:?}",
                    binder_thread.tid,
                    &err
                );
                // Negate the value, as the binder runtime assumes error values are already
                // negative.
                Command::Error(err.return_value() as i32)
            }
            TransactionError::Failure => Command::FailedReply,
            TransactionError::Dead => Command::DeadReply { pop_transaction: false },
        });
        Ok(())
    }
}

impl From<Errno> for TransactionError {
    fn from(errno: Errno) -> TransactionError {
        TransactionError::Malformed(errno)
    }
}

pub struct BinderFs;
impl FileSystemOps for BinderFs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(BINDERFS_SUPER_MAGIC))
    }
    fn name(&self) -> &'static FsStr {
        "binder".into()
    }
}

const DEFAULT_BINDERS: [&str; 3] = ["binder", "hwbinder", "vndbinder"];

#[derive(Debug)]
struct BinderFsDir {
    devices: BTreeMap<FsString, DeviceType>,
}

impl BinderFsDir {
    fn new(locked: &mut Locked<'_, Unlocked>, current_task: &CurrentTask) -> Result<Self, Errno> {
        let kernel = current_task.kernel();
        let registry = &kernel.device_registry;
        let mut devices = BTreeMap::<FsString, DeviceType>::default();
        let remote_device = registry.register_misc_device(
            locked,
            current_task,
            "remote-binder".into(),
            RemoteBinderDevice {},
        )?;
        devices.insert("remote".into(), remote_device.metadata.device_type);

        for name in DEFAULT_BINDERS {
            let driver = BinderDevice::default();
            let device =
                registry.register_misc_device(locked, current_task, name.into(), driver.clone())?;
            devices.insert(name.into(), device.metadata.device_type);
            kernel.binders.write().insert(device.metadata.device_type, driver);
        }

        Ok(Self { devices })
    }
}

impl FsNodeOps for BinderFsDir {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let entries = self
            .devices
            .keys()
            .map(|name| VecDirectoryEntry {
                entry_type: DirectoryEntryType::CHR,
                name: name.clone(),
                inode: None,
            })
            .collect::<Vec<_>>();
        Ok(VecDirectory::new_file(entries))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        if let Some(dev) = self.devices.get(name) {
            let mode = if name == "remote" { mode!(IFCHR, 0o444) } else { mode!(IFCHR, 0o600) };
            Ok(node.fs().create_node(current_task, SpecialNode, |ino| {
                let mut info = FsNodeInfo::new(ino, mode, FsCred::root());
                info.rdev = *dev;
                info
            }))
        } else {
            error!(ENOENT, format!("looking for {name}"))
        }
    }
}

impl BinderFs {
    pub fn new_fs(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        options: FileSystemOptions,
    ) -> Result<FileSystemHandle, Errno> {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Permanent, BinderFs, options)?;
        fs.set_root(BinderFsDir::new(locked, current_task)?);
        Ok(fs)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::mm::PAGE_SIZE;
    use crate::testing::*;
    use crate::vfs::{anon_fs, Anon};
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, RequestStream, ServerEnd};
    use fuchsia_async as fasync;
    use fuchsia_async::LocalExecutor;
    use futures::TryStreamExt;
    use memoffset::offset_of;
    use starnix_uapi::errors::{EBADF, EINVAL};
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::{
        binder_transaction_data__bindgen_ty_1, binder_transaction_data__bindgen_ty_2,
        BINDER_TYPE_WEAK_HANDLE,
    };
    use std::sync::Weak;
    use zerocopy::{FromZeros, KnownLayout};

    const BASE_ADDR: UserAddress = UserAddress::const_from(0x0000000000000100);
    const VMO_LENGTH: usize = 4096;

    impl ResourceAccessor for AutoReleasableTask {
        fn close_fd(&self, fd: FdNumber) -> Result<(), Errno> {
            self.deref().close_fd(fd)
        }
        fn get_file_with_flags(
            &self,
            current_task: &CurrentTask,
            fd: FdNumber,
        ) -> Result<(FileHandle, FdFlags), Errno> {
            self.deref().get_file_with_flags(current_task, fd)
        }
        fn add_file_with_flags(
            &self,
            locked: &mut Locked<'_, ResourceAccessorAddFile>,
            current_task: &CurrentTask,
            file: FileHandle,
            flags: FdFlags,
        ) -> Result<FdNumber, Errno> {
            self.deref().add_file_with_flags(locked, current_task, file, flags)
        }
        fn kernel(&self) -> &Arc<Kernel> {
            &self.deref().kernel()
        }
        fn as_memory_accessor(&self) -> &dyn MemoryAccessor {
            self.deref().as_memory_accessor()
        }
    }

    struct BinderProcessFixture {
        device: Weak<BinderDriverReleaser>,
        proc: OwnedRef<BinderProcess>,
        thread: OwnedRef<BinderThread>,
        task: AutoReleasableTask,
    }

    impl BinderProcessFixture {
        fn new(
            locked: &mut Locked<'_, Unlocked>,
            current_task: &CurrentTask,
            device: &BinderDevice,
        ) -> Self {
            let task = create_task(locked, current_task.kernel(), "task");
            let (proc, thread) = device.create_process_and_thread(task.get_pid());

            mmap_shared_memory(&device, &task, &proc);
            Self { device: Arc::downgrade(device), proc, thread, task }
        }

        fn lock_shared_memory(&self) -> starnix_sync::MappedMutexGuard<'_, SharedMemory> {
            starnix_sync::MutexGuard::map(self.proc.shared_memory.lock(), |value| {
                value.as_mut().unwrap()
            })
        }
    }

    impl Drop for BinderProcessFixture {
        fn drop(&mut self) {
            OwnedRef::take(&mut self.thread).release(self.task.kernel());
            if let Some(device) = self.device.upgrade() {
                device.procs.write().remove(&self.proc.identifier).release(self.task.kernel());
            }
            OwnedRef::take(&mut self.proc).release(self.task.kernel());
        }
    }

    /// Fills the provided shared memory with n buffers, each spanning 1/n-th of the memory.
    fn fill_with_buffers(shared_memory: &mut SharedMemory, n: usize) -> Vec<UserAddress> {
        let mut addresses = vec![];
        for _ in 0..n {
            let address = {
                let buffer = shared_memory
                    .allocate_buffers(VMO_LENGTH / n, 0, 0, 0)
                    .unwrap_or_else(|_| panic!("allocate {n:?}-th buffer"))
                    .data_buffer;
                buffer.memory.user_address + buffer.offset
            };
            addresses.push(address);
        }
        addresses
    }

    /// Simulates an mmap call on the binder driver, setting up shared memory between the driver and
    /// `proc`.
    fn mmap_shared_memory(driver: &BinderDriver, current_task: &CurrentTask, proc: &BinderProcess) {
        let prot_flags = ProtectionFlags::READ;
        driver
            .mmap(
                current_task,
                proc,
                DesiredAddress::Any,
                VMO_LENGTH,
                prot_flags,
                MappingOptions::empty(),
                NamespaceNode::new_anonymous_unrooted(
                    FsNode::new_root(PanickingFsNode).into_handle(),
                ),
            )
            .expect("mmap");
    }

    /// Registers a binder object to `owner`.
    fn register_binder_object(
        owner: &BinderProcess,
        weak_ref_addr: UserAddress,
        strong_ref_addr: UserAddress,
    ) -> (Arc<BinderObject>, StrongRefGuard) {
        let (object, guard) = BinderObject::new(
            owner,
            LocalBinderObject { weak_ref_addr, strong_ref_addr },
            BinderObjectFlags::empty(),
        );
        owner.lock().objects.insert(weak_ref_addr, object.clone());
        (object, guard)
    }

    fn assert_flags_are_equivalent(f1: fbinder::FileFlags, f2: OpenFlags) {
        assert_eq!(f1, f2.into_fidl());
        assert_eq!(f2, f1.into_fidl());
    }

    #[::fuchsia::test]
    fn test_flags_conversion() {
        assert_flags_are_equivalent(
            fbinder::FileFlags::RIGHT_READABLE | fbinder::FileFlags::RIGHT_WRITABLE,
            OpenFlags::RDWR,
        );
        assert_flags_are_equivalent(fbinder::FileFlags::RIGHT_READABLE, OpenFlags::RDONLY);
        assert_flags_are_equivalent(fbinder::FileFlags::RIGHT_WRITABLE, OpenFlags::WRONLY);
        assert_flags_are_equivalent(
            fbinder::FileFlags::RIGHT_READABLE | fbinder::FileFlags::DIRECTORY,
            OpenFlags::DIRECTORY,
        );
    }

    #[fuchsia::test]
    fn handle_tests() {
        assert_matches!(Handle::from(0), Handle::ContextManager);
        assert_matches!(Handle::from(1), Handle::Object { index: 0 });
        assert_matches!(Handle::from(2), Handle::Object { index: 1 });
        assert_matches!(Handle::from(99), Handle::Object { index: 98 });
    }

    #[fuchsia::test]
    async fn handle_0_succeeds_when_context_manager_is_set() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let context_manager =
                BinderObject::new_context_manager_marker(&sender.proc, BinderObjectFlags::empty());
            *device.context_manager.lock() = Some(context_manager.clone());
            let (object, owner) =
                device.get_context_manager(&sender.task).expect("failed to find handle 0");
            assert_eq!(OwnedRef::as_ptr(&sender.proc), TempRef::as_ptr(&owner));
            assert!(Arc::ptr_eq(&context_manager, &object));
        });
    }

    #[fuchsia::test]
    async fn fail_to_retrieve_non_existing_handle() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            assert!(&sender.proc.lock().handles.get(3).is_none());
        });
    }

    #[fuchsia::test]
    async fn handle_is_not_dropped_after_transaction_finishes_if_it_already_existed() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let proc_1 = BinderProcessFixture::new(&mut locked, current_task, &device);
            let proc_2 = BinderProcessFixture::new(&mut locked, current_task, &device);

            let (transaction_ref, guard) = register_binder_object(
                &proc_1.proc,
                UserAddress::from(0xffffffffffffffff),
                UserAddress::from(0x1111111111111111),
            );
            scopeguard::defer! {
                transaction_ref.ack_acquire(&mut RefCountActions::default_released()).expect("ack_acquire");
                transaction_ref.apply_deferred_refcounts();
            }

            // Insert the transaction once.
            let _ = proc_2
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            let guard = transaction_ref.inc_strong_checked().expect("inc_strong");
            // Insert the same object.
            let handle = proc_2
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // The object should be present in the handle table until a strong decrement.
            let (retrieved_object, guard) =
                proc_2.proc.lock().handles.get(handle.object_index()).expect("valid object");
            assert!(Arc::ptr_eq(&retrieved_object, &transaction_ref));
            guard.release(&mut RefCountActions::default_released());

            // Drop the transaction reference.
            proc_2
                .proc
                .lock()
                .handles
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong");

            // The handle should not have been dropped, as it was already in the table beforehand.
            let (retrieved_object, guard) =
                proc_2.proc.lock().handles.get(handle.object_index()).expect("valid object");
            assert!(Arc::ptr_eq(&retrieved_object, &transaction_ref));
            guard.release(&mut RefCountActions::default_released());
        });
    }

    #[fuchsia::test]
    async fn handle_is_dropped_after_transaction_finishes() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let proc_1 = BinderProcessFixture::new(&mut locked, current_task, &device);
            let proc_2 = BinderProcessFixture::new(&mut locked, current_task, &device);

            let (transaction_ref, guard) = register_binder_object(
                &proc_1.proc,
                UserAddress::from(0xffffffffffffffff),
                UserAddress::from(0x1111111111111111),
            );
            scopeguard::defer! {
                transaction_ref.ack_acquire(&mut RefCountActions::default_released()).expect("ack_acquire");
                transaction_ref.apply_deferred_refcounts();
            }

            // Transactions always take a strong reference to binder objects.
            let handle = proc_2
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // The object should be present in the handle table until a strong decrement.
            let (retrieved_object, guard) =
                proc_2.proc.lock().handles.get(handle.object_index()).expect("valid object");
            assert!(Arc::ptr_eq(&retrieved_object, &transaction_ref));
            guard.release(&mut RefCountActions::default_released());

            // Drop the transaction reference.
            proc_2
                .proc
                .lock()
                .handles
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong");

            // The handle should now have been dropped.
            assert!(proc_2.proc.lock().handles.get(handle.object_index()).is_none());
        });
    }

    #[fuchsia::test]
    async fn handle_is_dropped_after_last_weak_ref_released() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let proc_1 = BinderProcessFixture::new(&mut locked, current_task, &device);
            let proc_2 = BinderProcessFixture::new(&mut locked, current_task, &device);

            let (transaction_ref, guard) = register_binder_object(
                &proc_1.proc,
                UserAddress::from(0xffffffffffffffff),
                UserAddress::from(0x1111111111111111),
            );

            // Keep guard to simulate another process keeping a reference.
            let other_process_guard = transaction_ref.inc_strong_checked();

            scopeguard::defer! {
                // Other process releases the object.
                other_process_guard.release(&mut RefCountActions::default_released());
                // Ack the initial acquire.
                transaction_ref.ack_acquire(&mut RefCountActions::default_released()).expect("ack_acquire");
                transaction_ref.apply_deferred_refcounts();
            }

            // The handle starts with a strong ref.
            let handle = proc_2
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Acquire a weak reference.
            proc_2
                .proc
                .lock()
                .handles
                .inc_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect("inc_weak");

            // The object should be present in the handle table.
            let (retrieved_object, guard) =
                proc_2.proc.lock().handles.get(handle.object_index()).expect("valid object");
            assert!(Arc::ptr_eq(&retrieved_object, &transaction_ref));
            guard.release(&mut RefCountActions::default_released());

            // Drop the strong reference. The handle should still be present as there is an outstanding
            // weak reference.
            proc_2
                .proc
                .lock()
                .handles
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong");
            let (retrieved_object, guard) =
                proc_2.proc.lock().handles.get(handle.object_index()).expect("valid object");
            assert!(Arc::ptr_eq(&retrieved_object, &transaction_ref));
            guard.release(&mut RefCountActions::default_released());

            // Drop the weak reference. The handle should now be gone, even though the underlying object
            // is still alive (another process could have references to it).
            proc_2
                .proc
                .lock()
                .handles
                .dec_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_weak");
            assert!(
                proc_2.proc.lock().handles.get(handle.object_index()).is_none(),
                "handle should be dropped"
            );
        });
    }

    #[fuchsia::test]
    fn shared_memory_allocation_fails_with_invalid_offsets_length() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");
        shared_memory
            .allocate_buffers(3, 1, 0, 0)
            .expect_err("offsets_length should be multiple of 8");
        shared_memory
            .allocate_buffers(3, 8, 1, 0)
            .expect_err("buffers_length should be multiple of 8");
        shared_memory
            .allocate_buffers(3, 8, 0, 1)
            .expect_err("security_context_buffer_length should be multiple of 8");
    }

    #[fuchsia::test]
    fn shared_memory_allocation_aligns_offsets_buffer() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");

        const DATA_LEN: usize = 3;
        const OFFSETS_COUNT: usize = 1;
        const OFFSETS_LEN: usize = std::mem::size_of::<binder_uintptr_t>() * OFFSETS_COUNT;
        const BUFFERS_LEN: usize = 8;
        const SECURITY_CONTEXT_BUFFER_LEN: usize = 24;
        let allocations = shared_memory
            .allocate_buffers(DATA_LEN, OFFSETS_LEN, BUFFERS_LEN, SECURITY_CONTEXT_BUFFER_LEN)
            .expect("allocate buffer");
        assert_eq!(
            allocations.data_buffer.user_buffer(),
            UserBuffer { address: BASE_ADDR, length: DATA_LEN }
        );
        assert_eq!(
            allocations.offsets_buffer.user_buffer(),
            UserBuffer { address: BASE_ADDR + 8usize, length: OFFSETS_LEN }
        );
        assert_eq!(
            allocations.scatter_gather_buffer.user_buffer(),
            UserBuffer { address: BASE_ADDR + 8usize + OFFSETS_LEN, length: BUFFERS_LEN }
        );
        assert_eq!(
            allocations.security_context_buffer.as_ref().expect("security_context").user_buffer(),
            UserBuffer {
                address: BASE_ADDR + 8usize + OFFSETS_LEN + BUFFERS_LEN,
                length: SECURITY_CONTEXT_BUFFER_LEN
            }
        );
        assert_eq!(allocations.data_buffer.as_bytes().len(), DATA_LEN);
        assert_eq!(allocations.offsets_buffer.as_bytes().len(), OFFSETS_COUNT);
        assert_eq!(allocations.scatter_gather_buffer.as_bytes().len(), BUFFERS_LEN);
        assert_eq!(
            allocations
                .security_context_buffer
                .as_ref()
                .expect("security_context")
                .as_bytes()
                .len(),
            SECURITY_CONTEXT_BUFFER_LEN
        );
    }

    #[fuchsia::test]
    fn shared_memory_allocation_buffers_correctly_write_through() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");

        const DATA_LEN: usize = 256;
        const OFFSETS_COUNT: usize = 4;
        const OFFSETS_LEN: usize = std::mem::size_of::<binder_uintptr_t>() * OFFSETS_COUNT;
        let mut allocations =
            shared_memory.allocate_buffers(DATA_LEN, OFFSETS_LEN, 0, 0).expect("allocate buffer");

        // Write data to the allocated buffers.
        const DATA_FILL: u8 = 0xff;
        allocations.data_buffer.as_mut_bytes().fill(0xff);

        const OFFSETS_FILL: binder_uintptr_t = 0xDEADBEEFDEADBEEF;
        allocations.offsets_buffer.as_mut_bytes().fill(OFFSETS_FILL);

        // Check that the correct bit patterns were written through to the underlying VMO.
        let mut data = [0u8; DATA_LEN];
        memory.read(&mut data, 0).expect("read VMO failed");
        assert!(data.iter().all(|b| *b == DATA_FILL));

        let mut data = [0u64; OFFSETS_COUNT];
        memory.read(data.as_mut_bytes(), DATA_LEN as u64).expect("read VMO failed");
        assert!(data.iter().all(|b| *b == OFFSETS_FILL));
    }

    #[fuchsia::test]
    fn shared_memory_allocates_multiple_buffers() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");

        // Check that two buffers allocated from the same shared memory region don't overlap.
        const BUF1_DATA_LEN: usize = 64;
        const BUF1_OFFSETS_LEN: usize = 8;
        const BUF1_BUFFERS_LEN: usize = 8;
        const BUF1_SECURITY_CONTEXT_BUFFER_LEN: usize = 8;
        let allocations = shared_memory
            .allocate_buffers(
                BUF1_DATA_LEN,
                BUF1_OFFSETS_LEN,
                BUF1_BUFFERS_LEN,
                BUF1_SECURITY_CONTEXT_BUFFER_LEN,
            )
            .expect("allocate buffer 1");
        assert_eq!(
            allocations.data_buffer.user_buffer(),
            UserBuffer { address: BASE_ADDR, length: BUF1_DATA_LEN }
        );
        assert_eq!(
            allocations.offsets_buffer.user_buffer(),
            UserBuffer { address: BASE_ADDR + BUF1_DATA_LEN, length: BUF1_OFFSETS_LEN }
        );
        assert_eq!(
            allocations.scatter_gather_buffer.user_buffer(),
            UserBuffer {
                address: BASE_ADDR + BUF1_DATA_LEN + BUF1_OFFSETS_LEN,
                length: BUF1_BUFFERS_LEN
            }
        );
        assert_eq!(
            allocations.security_context_buffer.expect("security_context").user_buffer(),
            UserBuffer {
                address: BASE_ADDR + BUF1_DATA_LEN + BUF1_OFFSETS_LEN + BUF1_BUFFERS_LEN,
                length: BUF1_SECURITY_CONTEXT_BUFFER_LEN
            }
        );

        const BUF2_DATA_LEN: usize = 32;
        const BUF2_OFFSETS_LEN: usize = 0;
        const BUF2_BUFFERS_LEN: usize = 0;
        const BUF2_SECURITY_CONTEXT_BUFFER_LEN: usize = 0;
        let allocations = shared_memory
            .allocate_buffers(
                BUF2_DATA_LEN,
                BUF2_OFFSETS_LEN,
                BUF2_BUFFERS_LEN,
                BUF2_SECURITY_CONTEXT_BUFFER_LEN,
            )
            .expect("allocate buffer 2");
        assert_eq!(
            allocations.data_buffer.user_buffer(),
            UserBuffer {
                address: BASE_ADDR
                    + BUF1_DATA_LEN
                    + BUF1_OFFSETS_LEN
                    + BUF1_BUFFERS_LEN
                    + BUF1_SECURITY_CONTEXT_BUFFER_LEN,
                length: BUF2_DATA_LEN
            }
        );
        assert_eq!(
            allocations.offsets_buffer.user_buffer(),
            UserBuffer {
                address: BASE_ADDR
                    + BUF1_DATA_LEN
                    + BUF1_OFFSETS_LEN
                    + BUF1_BUFFERS_LEN
                    + BUF1_SECURITY_CONTEXT_BUFFER_LEN
                    + BUF2_DATA_LEN,
                length: BUF2_OFFSETS_LEN
            }
        );
    }

    #[fuchsia::test]
    fn shared_memory_too_large_allocation_fails() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");

        shared_memory
            .allocate_buffers(VMO_LENGTH + 1, 0, 0, 0)
            .expect_err("out-of-bounds allocation");
        shared_memory.allocate_buffers(VMO_LENGTH, 8, 0, 0).expect_err("out-of-bounds allocation");
        shared_memory
            .allocate_buffers(VMO_LENGTH - 8, 8, 8, 0)
            .expect_err("out-of-bounds allocation");

        shared_memory.allocate_buffers(VMO_LENGTH, 0, 0, 0).expect("allocate buffer");

        // Now that the previous buffer allocation succeeded, there should be no more room.
        shared_memory.allocate_buffers(1, 0, 0, 0).expect_err("out-of-bounds allocation");
    }

    #[fuchsia::test]
    fn shared_memory_allocation_wraps_in_order() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");
        let n = 4;

        for buffer in fill_with_buffers(&mut shared_memory, n) {
            shared_memory
                .allocate_buffers(VMO_LENGTH / n, 0, 0, 0)
                .expect_err(&format!("allocated buffer when shared memory was full {n:?}"));

            shared_memory.free_buffer(buffer).expect("didn't free buffer");

            shared_memory.allocate_buffers(VMO_LENGTH / n, 0, 0, 0).unwrap_or_else(|_| {
                panic!("couldn't allocate new buffer even after {n:?}-th was released")
            });
        }
    }

    #[fuchsia::test]
    fn shared_memory_allocation_single() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");
        let n = 1;
        let buffers = fill_with_buffers(&mut shared_memory, n);

        shared_memory
            .allocate_buffers(VMO_LENGTH / n, 0, 0, 0)
            .expect_err("could allocate when buffer was full");
        shared_memory.free_buffer(buffers[0]).expect("didn't free buffer");
        shared_memory
            .allocate_buffers(VMO_LENGTH / n, 0, 0, 0)
            .expect("couldn't allocate even after first slot opened up");
    }

    #[fuchsia::test]
    fn shared_memory_allocation_can_allocate_in_hole() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");
        let n = 4;

        let buffers = fill_with_buffers(&mut shared_memory, n);

        // Free all the buffers in reverse order, and verify that the new buffer isn't allocated
        // until the first buffer is freed.
        shared_memory
            .allocate_buffers(VMO_LENGTH / n, 0, 0, 0)
            .expect_err("cannot allocate when full");
        shared_memory.free_buffer(buffers[1]).expect("didn't free buffer");
        shared_memory.allocate_buffers(VMO_LENGTH / n, 0, 0, 0).expect("can allocate in hole");
    }

    #[fuchsia::test]
    fn shared_memory_allocation_doesnt_wrap_when_cant_fit() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");
        let n = 4;

        let buffers = fill_with_buffers(&mut shared_memory, n);

        shared_memory.free_buffer(buffers[0]).expect("didn't free buffer");
        // Allocate slightly more than what can fit at the start (after freeing the first 1/4th).
        shared_memory
            .allocate_buffers((VMO_LENGTH / n) + 1, 0, 0, 0)
            .expect_err("allocated over existing buffer");
        shared_memory.free_buffer(buffers[1]).expect("didn't free buffer");
        shared_memory
            .allocate_buffers((VMO_LENGTH / n) + 1, 0, 0, 0)
            .expect("couldn't allocate when there was enough space");
    }

    #[fuchsia::test]
    fn shared_memory_allocation_doesnt_wrap_when_cant_fit_at_end() {
        let memory =
            MemoryObject::from(zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"));
        let mut shared_memory =
            SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH).expect("failed to map shared memory");

        // Test that a buffer can still be allocated even if it can't fit at the end, but can fit
        // at the start by first allocating 3/4 of the memory.
        let buffer_1 = {
            let allocations =
                shared_memory.allocate_buffers(VMO_LENGTH / 4, 0, 0, 0).expect("couldn't allocate");
            allocations.data_buffer.memory.user_address + allocations.data_buffer.offset
        };
        let buffer_2 = {
            let allocations =
                shared_memory.allocate_buffers(VMO_LENGTH / 4, 0, 0, 0).expect("couldn't allocate");
            allocations.data_buffer.memory.user_address + allocations.data_buffer.offset
        };
        let buffer_3 = {
            let allocations =
                shared_memory.allocate_buffers(VMO_LENGTH / 4, 0, 0, 0).expect("couldn't allocate");
            allocations.data_buffer.memory.user_address + allocations.data_buffer.offset
        };

        // Attempt to allocate a buffer at the end that is larger than 1/4th.
        shared_memory
            .allocate_buffers(VMO_LENGTH / 3, 0, 0, 0)
            .expect_err("allocated over existing buffer");
        // Now free all the buffers at the start.
        shared_memory.free_buffer(buffer_1).expect("didn't free buffer");
        shared_memory.free_buffer(buffer_2).expect("didn't free buffer");
        shared_memory.free_buffer(buffer_3).expect("didn't free buffer");

        // Try the allocation again.
        shared_memory
            .allocate_buffers(VMO_LENGTH / 3, 0, 0, 0)
            .expect("failed even though there was room at start");
    }

    #[fuchsia::test]
    async fn binder_object_enqueues_release_command_when_dropped() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let proc = BinderProcessFixture::new(&mut locked, current_task, &device);

            const LOCAL_BINDER_OBJECT: LocalBinderObject = LocalBinderObject {
                weak_ref_addr: UserAddress::const_from(0x0000000000000010),
                strong_ref_addr: UserAddress::const_from(0x0000000000000100),
            };

            let (object, guard) = register_binder_object(
                &proc.proc,
                LOCAL_BINDER_OBJECT.weak_ref_addr,
                LOCAL_BINDER_OBJECT.strong_ref_addr,
            );

            let mut actions = RefCountActions::default();
            object.ack_acquire(&mut actions).expect("ack_acquire");
            guard.release(&mut actions);
            actions.release(());

            assert_matches!(
                &proc.proc.command_queue.lock().commands.front(),
                Some(Command::ReleaseRef(LOCAL_BINDER_OBJECT))
            );
        });
    }

    #[fuchsia::test]
    async fn handle_table_refs() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let proc = BinderProcessFixture::new(&mut locked, current_task, &device);

            let (object, guard) = register_binder_object(
                &proc.proc,
                UserAddress::from(0x0000000000000010),
                UserAddress::from(0x0000000000000100),
            );

            // Simulate another process keeping a strong reference.
            let other_process_guard = object.inc_strong_checked();

            let mut handle_table = HandleTable::default();

            // Starts with one strong reference.
            let handle = handle_table
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            handle_table
                .inc_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("inc_strong 1");
            handle_table
                .inc_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("inc_strong 2");
            handle_table
                .inc_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect("inc_weak 0");
            handle_table
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong 2");
            handle_table
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong 1");

            // Remove the initial strong reference.
            handle_table
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong 0");

            // Removing more strong references should fail.
            handle_table
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect_err("dec_strong -1");

            // The object should still take up an entry in the handle table, as there is 1 weak
            // reference and it is maintained alive by another process.
            let (_, guard) = handle_table.get(handle.object_index()).expect("object still exists");
            guard.release(&mut RefCountActions::default_released());

            // Simulate another process droppping its reference.
            other_process_guard.release(&mut RefCountActions::default_released());
            object.apply_deferred_refcounts();
            // Ack the initial acquire.
            object.ack_acquire(&mut RefCountActions::default_released()).expect("ack_acquire");
            // Ack the subsequent incref from the test.
            object.ack_incref(&mut RefCountActions::default_released()).expect("ack_incref");

            // Our weak reference won't keep the object alive.
            assert!(handle_table.get(handle.object_index()).is_none(), "object should be dead");

            // Remove from our table.
            handle_table
                .dec_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_weak 0");
            object.apply_deferred_refcounts();

            // Another removal attempt will prove the handle has been removed.
            handle_table
                .dec_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect_err("handle should no longer exist");
        });
    }

    #[fuchsia::test]
    fn serialize_binder_handle() {
        let mut output = [0u8; std::mem::size_of::<flat_binder_object>()];

        SerializedBinderObject::Handle {
            handle: 2.into(),
            flags: BinderObjectFlags::parse(42).expect("parse"),
            cookie: 99,
        }
        .write_to(&mut output)
        .expect("write handle");
        assert_eq!(
            struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 42,
                cookie: 99,
                __bindgen_anon_1.handle: 2,
            }),
            output
        );
    }

    #[fuchsia::test]
    fn serialize_binder_object() {
        let mut output = [0u8; std::mem::size_of::<flat_binder_object>()];

        SerializedBinderObject::Object {
            local: LocalBinderObject {
                weak_ref_addr: UserAddress::from(0xDEADBEEF),
                strong_ref_addr: UserAddress::from(0xDEADDEAD),
            },
            flags: BinderObjectFlags::parse(42).expect("parse"),
        }
        .write_to(&mut output)
        .expect("write object");
        assert_eq!(
            struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_BINDER,
                flags: 42,
                cookie: 0xDEADDEAD,
                __bindgen_anon_1.binder: 0xDEADBEEF,
            }),
            output
        );
    }

    #[fuchsia::test]
    fn serialize_binder_fd() {
        let mut output = [0u8; std::mem::size_of::<flat_binder_object>()];

        SerializedBinderObject::File {
            fd: FdNumber::from_raw(2),
            flags: BinderObjectFlags::parse(42).expect("parse"),
            cookie: 99,
        }
        .write_to(&mut output)
        .expect("write fd");
        assert_eq!(
            struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_FD,
                flags: 42,
                cookie: 99,
                __bindgen_anon_1.handle: 2,
            }),
            output
        );
    }

    #[fuchsia::test]
    fn serialize_binder_buffer() {
        let mut output = [0u8; std::mem::size_of::<binder_buffer_object>()];

        SerializedBinderObject::Buffer {
            buffer: UserAddress::from(0xDEADBEEF),
            length: 0x100,
            parent: 1,
            parent_offset: 20,
            flags: 42,
        }
        .write_to(&mut output)
        .expect("write buffer");
        assert_eq!(
            binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: 0xDEADBEEF,
                length: 0x100,
                parent: 1,
                parent_offset: 20,
                flags: 42,
            }
            .as_bytes(),
            output
        );
    }

    #[fuchsia::test]
    fn serialize_binder_fd_array() {
        let mut output = [0u8; std::mem::size_of::<binder_fd_array_object>()];

        SerializedBinderObject::FileArray { num_fds: 2, parent: 1, parent_offset: 20 }
            .write_to(&mut output)
            .expect("write fd array");
        assert_eq!(
            binder_fd_array_object {
                hdr: binder_object_header { type_: BINDER_TYPE_FDA },
                num_fds: 2,
                parent: 1,
                parent_offset: 20,
                pad: 0,
            }
            .as_bytes(),
            output
        );
    }

    #[fuchsia::test]
    fn serialize_binder_buffer_too_small() {
        let mut output = [0u8; std::mem::size_of::<binder_uintptr_t>()];
        SerializedBinderObject::Handle {
            handle: 2.into(),
            flags: BinderObjectFlags::parse(42).expect("parse"),
            cookie: 99,
        }
        .write_to(&mut output)
        .expect_err("write handle should not succeed");
        SerializedBinderObject::Object {
            local: LocalBinderObject {
                weak_ref_addr: UserAddress::from(0xDEADBEEF),
                strong_ref_addr: UserAddress::from(0xDEADDEAD),
            },
            flags: BinderObjectFlags::parse(42).expect("parse"),
        }
        .write_to(&mut output)
        .expect_err("write object should not succeed");
        SerializedBinderObject::File {
            fd: FdNumber::from_raw(2),
            flags: BinderObjectFlags::parse(42).expect("parse"),
            cookie: 99,
        }
        .write_to(&mut output)
        .expect_err("write fd should not succeed");
    }

    #[fuchsia::test]
    fn deserialize_binder_handle() {
        let input = struct_with_union_into_bytes!(flat_binder_object {
            hdr.type_: BINDER_TYPE_HANDLE,
            flags: 42,
            cookie: 99,
            __bindgen_anon_1.handle: 2,
        });
        assert_eq!(
            SerializedBinderObject::from_bytes(&input).expect("read handle"),
            SerializedBinderObject::Handle {
                handle: 2.into(),
                flags: BinderObjectFlags::parse(42).expect("parse"),
                cookie: 99
            }
        );
    }

    #[fuchsia::test]
    fn deserialize_binder_object() {
        let input = struct_with_union_into_bytes!(flat_binder_object {
            hdr.type_: BINDER_TYPE_BINDER,
            flags: 42,
            cookie: 0xDEADDEAD,
            __bindgen_anon_1.binder: 0xDEADBEEF,
        });
        assert_eq!(
            SerializedBinderObject::from_bytes(&input).expect("read object"),
            SerializedBinderObject::Object {
                local: LocalBinderObject {
                    weak_ref_addr: UserAddress::from(0xDEADBEEF),
                    strong_ref_addr: UserAddress::from(0xDEADDEAD)
                },
                flags: BinderObjectFlags::parse(42).expect("parse")
            }
        );
    }

    #[fuchsia::test]
    fn deserialize_binder_fd() {
        let input = struct_with_union_into_bytes!(flat_binder_object {
            hdr.type_: BINDER_TYPE_FD,
            flags: 42,
            cookie: 99,
            __bindgen_anon_1.handle: 2,
        });
        assert_eq!(
            SerializedBinderObject::from_bytes(&input).expect("read handle"),
            SerializedBinderObject::File {
                fd: FdNumber::from_raw(2),
                flags: BinderObjectFlags::parse(42).expect("parse"),
                cookie: 99
            }
        );
    }

    #[fuchsia::test]
    fn deserialize_binder_buffer() {
        let input = binder_buffer_object {
            hdr: binder_object_header { type_: BINDER_TYPE_PTR },
            buffer: 0xDEADBEEF,
            length: 0x100,
            parent: 1,
            parent_offset: 20,
            flags: 42,
        };
        assert_eq!(
            SerializedBinderObject::from_bytes(input.as_bytes()).expect("read buffer"),
            SerializedBinderObject::Buffer {
                buffer: UserAddress::from(0xDEADBEEF),
                length: 0x100,
                parent: 1,
                parent_offset: 20,
                flags: 42,
            }
        );
    }

    #[fuchsia::test]
    fn deserialize_binder_fd_array() {
        let input = binder_fd_array_object {
            hdr: binder_object_header { type_: BINDER_TYPE_FDA },
            num_fds: 2,
            pad: 0,
            parent: 1,
            parent_offset: 20,
        };
        assert_eq!(
            SerializedBinderObject::from_bytes(input.as_bytes()).expect("read fd array"),
            SerializedBinderObject::FileArray { num_fds: 2, parent: 1, parent_offset: 20 }
        );
    }

    #[fuchsia::test]
    fn deserialize_unknown_object() {
        let input = struct_with_union_into_bytes!(flat_binder_object {
            hdr.type_: 9001,
            flags: 42,
            cookie: 99,
            __bindgen_anon_1.handle: 2,
        });
        SerializedBinderObject::from_bytes(&input).expect_err("read unknown object");
    }

    #[fuchsia::test]
    fn deserialize_input_too_small() {
        let input = struct_with_union_into_bytes!(flat_binder_object {
            hdr.type_: BINDER_TYPE_FD,
            flags: 42,
            cookie: 99,
            __bindgen_anon_1.handle: 2,
        });
        SerializedBinderObject::from_bytes(&input[..std::mem::size_of::<binder_uintptr_t>()])
            .expect_err("read buffer too small");
    }

    #[fuchsia::test]
    fn deserialize_unaligned() {
        let input = struct_with_union_into_bytes!(flat_binder_object {
            hdr.type_: BINDER_TYPE_HANDLE,
            flags: 42,
            cookie: 99,
            __bindgen_anon_1.handle: 2,
        });
        let mut unaligned_input = vec![];
        unaligned_input.push(0u8);
        unaligned_input.extend(input);
        SerializedBinderObject::from_bytes(&unaligned_input[1..]).expect("read unaligned object");
    }

    #[fuchsia::test]
    async fn copy_transaction_data_between_processes() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Explicitly install a VMO that we can read from later.
            let memory = MemoryObject::from(
                zx::Vmo::create(VMO_LENGTH as u64).expect("failed to create VMO"),
            );
            *receiver.proc.shared_memory.lock() = Some(
                SharedMemory::map(&memory, BASE_ADDR, VMO_LENGTH)
                    .expect("failed to map shared memory"),
            );

            // Map some memory for process 1.
            let data_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);

            // Write transaction data in process 1.
            const BINDER_DATA: &[u8; 8] = b"binder!!";
            let mut transaction_data = vec![];
            transaction_data.extend(BINDER_DATA);
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr: binder_object_header { type_: BINDER_TYPE_HANDLE },
                flags: 0,
                __bindgen_anon_1.handle: 0,
                cookie: 0,
            }));

            let offsets_addr = data_addr
                + sender
                    .task
                    .write_memory(data_addr, &transaction_data)
                    .expect("failed to write transaction data");

            // Write the offsets data (where in the data buffer `flat_binder_object`s are).
            let offsets_data: u64 = BINDER_DATA.len() as u64;
            sender
                .task
                .write_object(UserRef::new(offsets_addr), &offsets_data)
                .expect("failed to write offsets buffer");

            // Construct the `binder_transaction_data` struct that contains pointers to the data and
            // offsets buffers.
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: 1,
                    flags: 0,
                    sender_pid: sender.proc.pid,
                    sender_euid: 0,
                    target: binder_transaction_data__bindgen_ty_1 { handle: 0 },
                    cookie: 0,
                    data_size: transaction_data.len() as u64,
                    offsets_size: std::mem::size_of::<u64>() as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                },
                buffers_size: 0,
            };

            // Copy the data from process 1 to process 2
            let security_context = "hello\0".into();
            let (buffers, transaction_state) = device
                .copy_transaction_buffers(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &transaction,
                    Some(security_context),
                )
                .expect("copy data");
            let data_buffer = buffers.data;
            let offsets_buffer = buffers.offsets;
            let security_context_buffer = buffers.security_context.expect("security_context");

            // Check that the returned buffers are in-bounds of process 2's shared memory.
            assert!(data_buffer.address >= BASE_ADDR);
            assert!(data_buffer.address < BASE_ADDR + VMO_LENGTH);
            assert!(offsets_buffer.address >= BASE_ADDR);
            assert!(offsets_buffer.address < BASE_ADDR + VMO_LENGTH);
            assert!(security_context_buffer.address >= BASE_ADDR);
            assert!(security_context_buffer.address < BASE_ADDR + VMO_LENGTH);

            // Verify the contents of the copied data in process 2's shared memory VMO.
            let mut buffer = [0u8; BINDER_DATA.len() + std::mem::size_of::<flat_binder_object>()];
            memory
                .read(&mut buffer, (data_buffer.address - BASE_ADDR) as u64)
                .expect("failed to read data");
            assert_eq!(&buffer[..], &transaction_data);

            let mut buffer = [0u8; std::mem::size_of::<u64>()];
            memory
                .read(&mut buffer, (offsets_buffer.address - BASE_ADDR) as u64)
                .expect("failed to read offsets");
            assert_eq!(&buffer[..], offsets_data.as_bytes());
            let mut buffer = vec![0u8; security_context.len()];
            memory
                .read(&mut buffer[..], (security_context_buffer.address - BASE_ADDR) as u64)
                .expect("failed to read security_context");
            assert_eq!(&buffer[..], security_context);
            transaction_state.release(());
        });
    }

    #[fuchsia::test]
    async fn transaction_translate_binder_leaving_process() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            const BINDER_OBJECT: LocalBinderObject = LocalBinderObject {
                weak_ref_addr: UserAddress::const_from(0x0000000000000010),
                strong_ref_addr: UserAddress::const_from(0x0000000000000100),
            };

            const DATA_PREAMBLE: &[u8; 5] = b"stuff";

            let mut transaction_data = vec![];
            transaction_data.extend(DATA_PREAMBLE);
            let offsets = [transaction_data.len() as binder_uintptr_t];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_BINDER,
                flags: 0,
                cookie: BINDER_OBJECT.strong_ref_addr.ptr() as u64,
                __bindgen_anon_1.binder: BINDER_OBJECT.weak_ref_addr.ptr() as u64,
            }));

            const EXPECTED_HANDLE: Handle = Handle::from_raw(1);

            let transaction_state = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles");

            // Verify that the new handle was returned in `transaction_state` so that it gets dropped
            // at the end of the transaction.
            assert_eq!(transaction_state.state.as_ref().unwrap().handles[0], EXPECTED_HANDLE);

            // Verify that the transaction data was mutated.
            let mut expected_transaction_data = vec![];
            expected_transaction_data.extend(DATA_PREAMBLE);
            expected_transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: EXPECTED_HANDLE.into(),
            }));
            assert_eq!(&expected_transaction_data, &transaction_data);

            // Verify that a handle was created in the receiver.
            let (object, guard) = receiver
                .proc
                .lock()
                .handles
                .get(EXPECTED_HANDLE.object_index())
                .expect("expected handle not present");
            guard.release(&mut RefCountActions::default_released());
            assert_eq!(object.owner.as_ptr(), OwnedRef::as_ptr(&sender.proc));
            assert_eq!(object.local, BINDER_OBJECT);

            // Verify that a strong acquire command is sent to the sender process (on the same thread
            // that sent the transaction).
            assert_matches!(
                &sender.thread.lock().command_queue.commands.front(),
                Some(Command::AcquireRef(BINDER_OBJECT))
            );
            transaction_state.release(());
        });
    }

    #[fuchsia::test]
    async fn transaction_translate_binder_handle_entering_owning_process() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            let (binder_object, guard) = register_binder_object(
                &receiver.proc,
                UserAddress::from(0x0000000000000010),
                UserAddress::from(0x0000000000000100),
            );
            scopeguard::defer! {
                binder_object.ack_acquire(&mut RefCountActions::default_released()).expect("ack_acquire");
                binder_object.apply_deferred_refcounts();
            }

            // Pretend the binder object was given to the sender earlier, so it can be sent back.
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());
            // Clear the strong reference.
            scopeguard::defer! {
                sender.proc.lock().handles.dec_strong(handle.object_index(), &mut RefCountActions::default_released()).expect("dec_strong");
            }

            const DATA_PREAMBLE: &[u8; 5] = b"stuff";

            let mut transaction_data = vec![];
            transaction_data.extend(DATA_PREAMBLE);
            let offsets = [transaction_data.len() as binder_uintptr_t];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: handle.into(),
            }));

            device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles")
                .release(());

            // Verify that the transaction data was mutated.
            let mut expected_transaction_data = vec![];
            expected_transaction_data.extend(DATA_PREAMBLE);
            expected_transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_BINDER,
                flags: 0,
                cookie: binder_object.local.strong_ref_addr.ptr() as u64,
                __bindgen_anon_1.binder: binder_object.local.weak_ref_addr.ptr() as u64,
            }));
            assert_eq!(&expected_transaction_data, &transaction_data);
        });
    }

    #[fuchsia::test]
    async fn transaction_translate_binder_handle_passed_between_non_owning_processes() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let owner = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            let binder_object = LocalBinderObject {
                weak_ref_addr: UserAddress::from(0x0000000000000010),
                strong_ref_addr: UserAddress::from(0x0000000000000100),
            };

            const SENDING_HANDLE: Handle = Handle::from_raw(1);
            const RECEIVING_HANDLE: Handle = Handle::from_raw(2);

            // Pretend the binder object was given to the sender earlier.
            let (_, guard) =
                BinderObject::new(&owner.proc, binder_object, BinderObjectFlags::empty());
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());
            assert_eq!(SENDING_HANDLE, handle);

            // Give the receiver another handle so that the input handle number and output handle
            // number aren't the same.
            let (_, guard) = BinderObject::new(
                &owner.proc,
                LocalBinderObject::default(),
                BinderObjectFlags::empty(),
            );
            receiver
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            const DATA_PREAMBLE: &[u8; 5] = b"stuff";

            let mut transaction_data = vec![];
            transaction_data.extend(DATA_PREAMBLE);
            let offsets = [transaction_data.len() as binder_uintptr_t];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: SENDING_HANDLE.into(),
            }));

            let transaction_state = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles");

            // Verify that the new handle was returned in `transaction_state` so that it gets dropped
            // at the end of the transaction.
            assert_eq!(transaction_state.state.as_ref().unwrap().handles[0], RECEIVING_HANDLE);

            // Verify that the transaction data was mutated.
            let mut expected_transaction_data = vec![];
            expected_transaction_data.extend(DATA_PREAMBLE);
            expected_transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: RECEIVING_HANDLE.into(),
            }));
            assert_eq!(&expected_transaction_data, &transaction_data);

            // Verify that a handle was created in the receiver.
            let (object, guard) = receiver
                .proc
                .lock()
                .handles
                .get(RECEIVING_HANDLE.object_index())
                .expect("expected handle not present");
            guard.release(&mut RefCountActions::default_released());
            assert_eq!(object.owner.as_ptr(), OwnedRef::as_ptr(&owner.proc));
            assert_eq!(object.local, binder_object);
            transaction_state.release(());
        });
    }

    #[fuchsia::test]
    async fn transaction_translate_binder_handles_with_same_address() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let other_proc = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            let binder_object_addr = LocalBinderObject {
                weak_ref_addr: UserAddress::from(0x0000000000000010),
                strong_ref_addr: UserAddress::from(0x0000000000000100),
            };

            const SENDING_HANDLE_SENDER: Handle = Handle::from_raw(1);
            const SENDING_HANDLE_OTHER: Handle = Handle::from_raw(2);
            const RECEIVING_HANDLE_SENDER: Handle = Handle::from_raw(2);
            const RECEIVING_HANDLE_OTHER: Handle = Handle::from_raw(3);

            // Add both objects (sender owned and other owned) to sender handle table.
            let (_, sender_guard) =
                BinderObject::new(&sender.proc, binder_object_addr, BinderObjectFlags::empty());
            let (_, other_guard) =
                BinderObject::new(&other_proc.proc, binder_object_addr, BinderObjectFlags::empty());
            assert_eq!(
                sender
                    .proc
                    .lock()
                    .handles
                    .insert_for_transaction(sender_guard, &mut RefCountActions::default_released()),
                SENDING_HANDLE_SENDER
            );
            assert_eq!(
                sender
                    .proc
                    .lock()
                    .handles
                    .insert_for_transaction(other_guard, &mut RefCountActions::default_released()),
                SENDING_HANDLE_OTHER
            );

            // Give the receiver another handle so that the input handle numbers and output handle
            // numbers aren't the same.
            let (_, guard) = BinderObject::new(
                &other_proc.proc,
                LocalBinderObject::default(),
                BinderObjectFlags::empty(),
            );
            receiver
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            const DATA_PREAMBLE: &[u8; 5] = b"stuff";

            let mut transaction_data = vec![];
            let mut offsets = vec![];
            transaction_data.extend(DATA_PREAMBLE);
            offsets.push(transaction_data.len() as binder_uintptr_t);
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: SENDING_HANDLE_SENDER.into(),
            }));
            offsets.push(transaction_data.len() as binder_uintptr_t);
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: SENDING_HANDLE_OTHER.into(),
            }));

            let transaction_state = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles");

            // Verify that the new handles were returned in `transaction_state` so that it gets dropped
            // at the end of the transaction.
            assert_eq!(
                transaction_state.state.as_ref().unwrap().handles[0],
                RECEIVING_HANDLE_SENDER
            );
            assert_eq!(
                transaction_state.state.as_ref().unwrap().handles[1],
                RECEIVING_HANDLE_OTHER
            );

            // Verify that the transaction data was mutated.
            let mut expected_transaction_data = vec![];
            expected_transaction_data.extend(DATA_PREAMBLE);
            expected_transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: RECEIVING_HANDLE_SENDER.into(),
            }));
            expected_transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: RECEIVING_HANDLE_OTHER.into(),
            }));
            assert_eq!(&expected_transaction_data, &transaction_data);

            // Verify that two handles were created in the receiver.
            let (object, guard) = receiver
                .proc
                .lock()
                .handles
                .get(RECEIVING_HANDLE_SENDER.object_index())
                .expect("expected handle not present");
            guard.release(&mut RefCountActions::default_released());
            assert_eq!(object.owner.as_ptr(), OwnedRef::as_ptr(&sender.proc));
            assert_eq!(object.local, binder_object_addr);
            let (object, guard) = receiver
                .proc
                .lock()
                .handles
                .get(RECEIVING_HANDLE_OTHER.object_index())
                .expect("expected handle not present");
            guard.release(&mut RefCountActions::default_released());
            assert_eq!(object.owner.as_ptr(), OwnedRef::as_ptr(&other_proc.proc));
            assert_eq!(object.local, binder_object_addr);
            transaction_state.release(());
        });
    }

    /// Tests that hwbinder's scatter-gather buffer-fix-up implementation is correct.
    #[fuchsia::test]
    async fn transaction_translate_buffers() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Allocate memory in the sender to hold all the buffers that will get submitted to the
            // binder driver.
            let sender_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);
            let mut writer = UserMemoryWriter::new(&sender.task, sender_addr);

            // Serialize a string into memory.
            const FOO_STR_LEN: i32 = 3;
            const FOO_STR_PADDED_LEN: u64 = 8;
            let sender_foo_addr = writer.write(b"foo");

            // Pad the next buffer to ensure 8-byte alignment.
            writer.write(&[0; FOO_STR_PADDED_LEN as usize - FOO_STR_LEN as usize]);

            // Serialize a C struct that points to the above string.
            #[repr(C)]
            #[derive(IntoBytes, Immutable)]
            struct Bar {
                foo_str: UserAddress,
                len: i32,
                _padding: u32,
            }
            let sender_bar_addr = writer.write_object(&Bar {
                foo_str: sender_foo_addr,
                len: FOO_STR_LEN,
                _padding: 0,
            });

            // Mark the start of the transaction data.
            let transaction_data_addr = writer.current_address();

            // Write the buffer object representing the C struct `Bar`.
            let sender_buffer0_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_bar_addr.ptr() as u64,
                length: std::mem::size_of::<Bar>() as u64,
                ..binder_buffer_object::default()
            });

            // Write the buffer object representing the "foo" string. Its parent is the C struct `Bar`,
            // which has a pointer to it.
            let sender_buffer1_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_foo_addr.ptr() as u64,
                length: FOO_STR_LEN as u64,
                // Mark this buffer as having a parent who references it. The driver will then read
                // the next two fields.
                flags: BINDER_BUFFER_FLAG_HAS_PARENT,
                // The index in the offsets array of the parent buffer.
                parent: 0,
                // The location in the parent buffer where a pointer to this object needs to be
                // fixed up.
                parent_offset: offset_of!(Bar, foo_str) as u64,
            });

            // Write the offsets array.
            let offsets_addr = writer.current_address();
            writer.write_object(&((sender_buffer0_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_buffer1_addr - transaction_data_addr) as u64));

            let end_data_addr = writer.current_address();

            // Construct the input for the binder driver to process.
            let input = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    target: binder_transaction_data__bindgen_ty_1 { handle: 1 },
                    data_size: (offsets_addr - transaction_data_addr) as u64,
                    offsets_size: (end_data_addr - offsets_addr) as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: transaction_data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                    ..binder_transaction_data::new_zeroed()
                },
                // Each buffer size must be rounded up to a multiple of 8 to ensure enough
                // space in the allocated target for 8-byte alignment.
                buffers_size: std::mem::size_of::<Bar>() as u64 + FOO_STR_PADDED_LEN,
            };

            // Perform the translation and copying.
            let (buffers, transaction_state) = device
                .copy_transaction_buffers(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &input,
                    None,
                )
                .expect("copy_transaction_buffers");
            transaction_state.release(());
            let data_buffer = buffers.data;

            // Read back the translated objects from the receiver's memory.
            let translated_objects = receiver
                .task
                .read_objects_to_array::<binder_buffer_object, 2>(UserRef::new(data_buffer.address))
                .expect("read output");

            // Check that the second buffer is the string "foo".
            let foo_addr = UserAddress::from(translated_objects[1].buffer);
            let str = receiver.task.read_memory_to_array::<3>(foo_addr).expect("read buffer 1");
            assert_eq!(&str, b"foo");

            // Check that the first buffer points to the string "foo".
            let foo_ptr: UserAddress = receiver
                .task
                .read_object(UserRef::new(UserAddress::from(translated_objects[0].buffer)))
                .expect("read buffer 0");
            assert_eq!(foo_ptr, foo_addr);
        });
    }

    /// Tests that when the scatter-gather buffer size reported by userspace is too small, we stop
    /// processing and fail, instead of skipping a buffer object that doesn't fit.
    #[fuchsia::test]
    async fn transaction_fails_when_sg_buffer_size_is_too_small() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Allocate memory in the sender to hold all the buffers that will get submitted to the
            // binder driver.
            let sender_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);
            let mut writer = UserMemoryWriter::new(&sender.task, sender_addr);

            // Serialize a series of buffers that point to empty data. Each successive buffer is smaller
            // than the last.
            let buffer_objects = [8, 7, 6]
                .iter()
                .map(|size| binder_buffer_object {
                    hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                    buffer: writer
                        .write(&{
                            let mut data = vec![];
                            data.resize(*size, 0u8);
                            data
                        })
                        .ptr() as u64,
                    length: *size as u64,
                    ..binder_buffer_object::default()
                })
                .collect::<Vec<_>>();

            // Mark the start of the transaction data.
            let transaction_data_addr = writer.current_address();

            // Write the buffer objects to the transaction payload.
            let offsets = buffer_objects
                .into_iter()
                .map(|buffer_object| {
                    (writer.write_object(&buffer_object) - transaction_data_addr) as u64
                })
                .collect::<Vec<_>>();

            // Write the offsets array.
            let offsets_addr = writer.current_address();
            for offset in offsets {
                writer.write_object(&offset);
            }

            let end_data_addr = writer.current_address();

            // Construct the input for the binder driver to process.
            let input = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    target: binder_transaction_data__bindgen_ty_1 { handle: 1 },
                    data_size: (offsets_addr - transaction_data_addr) as u64,
                    offsets_size: (end_data_addr - offsets_addr) as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: transaction_data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                    ..binder_transaction_data::new_zeroed()
                },
                // Make the buffers size only fit the first buffer fully (size 8). The remaining space
                // should be 6 bytes, so that the second buffer doesn't fit but the next one does.
                buffers_size: 8 + 6,
            };

            // Perform the translation and copying.
            device
                .copy_transaction_buffers(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &input,
                    None,
                )
                .expect_err("copy_transaction_buffers should fail");
        });
    }

    /// Tests that when a scatter-gather buffer refers to a parent that comes *after* it in the
    /// object list, the transaction fails.
    #[fuchsia::test]
    async fn transaction_fails_when_sg_buffer_parent_is_out_of_order() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Allocate memory in the sender to hold all the buffers that will get submitted to the
            // binder driver.
            let sender_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);
            let mut writer = UserMemoryWriter::new(&sender.task, sender_addr);

            // Write the data for two buffer objects.
            const BUFFER_DATA_LEN: usize = 8;
            let buf0_addr = writer.write(&[0; BUFFER_DATA_LEN]);
            let buf1_addr = writer.write(&[0; BUFFER_DATA_LEN]);

            // Mark the start of the transaction data.
            let transaction_data_addr = writer.current_address();

            // Write a buffer object that marks a future buffer as its parent.
            let sender_buffer0_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: buf0_addr.ptr() as u64,
                length: BUFFER_DATA_LEN as u64,
                // Mark this buffer as having a parent who references it. The driver will then read
                // the next two fields.
                flags: BINDER_BUFFER_FLAG_HAS_PARENT,
                parent: 0,
                parent_offset: 0,
            });

            // Write a buffer object that acts as the first buffers parent (contains a pointer to the
            // first buffer).
            let sender_buffer1_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: buf1_addr.ptr() as u64,
                length: BUFFER_DATA_LEN as u64,
                ..binder_buffer_object::default()
            });

            // Write the offsets array.
            let offsets_addr = writer.current_address();
            writer.write_object(&((sender_buffer0_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_buffer1_addr - transaction_data_addr) as u64));

            let end_data_addr = writer.current_address();

            // Construct the input for the binder driver to process.
            let input = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    target: binder_transaction_data__bindgen_ty_1 { handle: 1 },
                    data_size: (offsets_addr - transaction_data_addr) as u64,
                    offsets_size: (end_data_addr - offsets_addr) as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: transaction_data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                    ..binder_transaction_data::new_zeroed()
                },
                buffers_size: BUFFER_DATA_LEN as u64 * 2,
            };

            // Perform the translation and copying.
            device
                .copy_transaction_buffers(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &input,
                    None,
                )
                .expect_err("copy_transaction_buffers should fail");
        });
    }

    #[fuchsia::test]
    async fn transaction_translate_fd_array() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Open a file in the sender process that we won't be using. It is there to occupy a file
            // descriptor so that the translation doesn't happen to use the same FDs for receiver and
            // sender, potentially hiding a bug.
            sender.task.add_file(PanickingFile::new_file(&sender.task), FdFlags::empty()).unwrap();

            // Open two files in the sender process. These will be sent in the transaction.
            let files =
                [PanickingFile::new_file(&sender.task), PanickingFile::new_file(&sender.task)];
            let sender_fds = files
                .iter()
                .map(|file| sender.task.add_file(file.clone(), FdFlags::CLOEXEC).expect("add file"))
                .collect::<Vec<_>>();

            // Ensure that the receiver task has no file descriptors.
            assert!(receiver.task.files.get_all_fds().is_empty(), "receiver already has files");

            // Allocate memory in the sender to hold all the buffers that will get submitted to the
            // binder driver.
            let sender_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);
            let mut writer = UserMemoryWriter::new(&sender.task, sender_addr);

            // Serialize a simple buffer. This will ensure that the FD array being translated is not at
            // the beginning of the buffer, exercising the offset math.
            let sender_padding_addr = writer.write(&[0; 8]);

            // Serialize a C struct with an fd array.
            #[repr(C)]
            #[derive(IntoBytes, KnownLayout, FromBytes, Immutable)]
            struct Bar {
                len: u32,
                fds: [u32; 2],
                _padding: u32,
            }
            let sender_bar_addr = writer.write_object(&Bar {
                len: 2,
                fds: [sender_fds[0].raw() as u32, sender_fds[1].raw() as u32],
                _padding: 0,
            });

            // Mark the start of the transaction data.
            let transaction_data_addr = writer.current_address();

            // Write the buffer object representing the padding.
            let sender_padding_buffer_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_padding_addr.ptr() as u64,
                length: 8,
                ..binder_buffer_object::default()
            });

            // Write the buffer object representing the C struct `Bar`.
            let sender_buffer_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_bar_addr.ptr() as u64,
                length: std::mem::size_of::<Bar>() as u64,
                ..binder_buffer_object::default()
            });

            // Write the fd array object that tells the kernel where the file descriptors are in the
            // `Bar` buffer.
            let sender_fd_array_addr = writer.write_object(&binder_fd_array_object {
                hdr: binder_object_header { type_: BINDER_TYPE_FDA },
                pad: 0,
                num_fds: sender_fds.len() as u64,
                // The index in the offsets array of the parent buffer.
                parent: 1,
                // The location in the parent buffer where the FDs are, which need to be duped.
                parent_offset: offset_of!(Bar, fds) as u64,
            });

            // Write the offsets array.
            let offsets_addr = writer.current_address();
            writer.write_object(&((sender_padding_buffer_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_buffer_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_fd_array_addr - transaction_data_addr) as u64));

            let end_data_addr = writer.current_address();

            // Construct the input for the binder driver to process.
            let input = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    data_size: (offsets_addr - transaction_data_addr) as u64,
                    offsets_size: (end_data_addr - offsets_addr) as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: transaction_data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                    ..binder_transaction_data::new_zeroed()
                },
                buffers_size: std::mem::size_of::<Bar>() as u64 + 8,
            };

            // Perform the translation and copying.
            device
                .handle_transaction(&mut locked, &sender.task, &sender.proc, &sender.thread, input)
                .expect("transaction queued");

            // Get the data buffer out of the receiver's queue.
            let data_buffer = match receiver
                .proc
                .command_queue
                .lock()
                .commands
                .pop_front()
                .expect("the transaction should be queued on the process")
            {
                Command::Transaction {
                    data: TransactionData { buffers: TransactionBuffers { data, .. }, .. },
                    ..
                } => data,
                _ => panic!("unexpected command in process queue"),
            };

            // Start reading from the receiver's memory, which holds the translated transaction.
            let mut reader = UserMemoryCursor::new(
                &receiver.task,
                data_buffer.address,
                data_buffer.length as u64,
            )
            .expect("create memory cursor");

            // Skip the first object, it was only there to pad the next one.
            reader.read_object::<binder_buffer_object>().expect("read padding buffer");

            // Read back the buffer object representing `Bar`.
            let bar_buffer_object =
                reader.read_object::<binder_buffer_object>().expect("read bar buffer object");
            let translated_bar = receiver
                .task
                .read_object::<Bar>(UserRef::new(UserAddress::from(bar_buffer_object.buffer)))
                .expect("read Bar");

            // Verify that the fds have been translated.
            let (receiver_file, receiver_fd_flags) = receiver
                .task
                .files
                .get_allowing_opath_with_flags(FdNumber::from_raw(translated_bar.fds[0] as i32))
                .expect("FD not found in receiver");
            assert!(
                Arc::ptr_eq(&receiver_file, &files[0]),
                "FD in receiver does not refer to the same file as sender"
            );
            assert_eq!(receiver_fd_flags, FdFlags::CLOEXEC);
            let (receiver_file, receiver_fd_flags) = receiver
                .task
                .files
                .get_allowing_opath_with_flags(FdNumber::from_raw(translated_bar.fds[1] as i32))
                .expect("FD not found in receiver");
            assert!(
                Arc::ptr_eq(&receiver_file, &files[1]),
                "FD in receiver does not refer to the same file as sender"
            );
            assert_eq!(receiver_fd_flags, FdFlags::CLOEXEC);

            // Release the buffer in the receiver and verify that the associated FDs have been closed.
            receiver.proc.handle_free_buffer(data_buffer.address).expect("failed to free buffer");
            assert!(
                receiver
                    .task
                    .files
                    .get_allowing_opath(FdNumber::from_raw(translated_bar.fds[0] as i32))
                    .expect_err("file should be closed")
                    == EBADF
            );
            assert!(
                receiver
                    .task
                    .files
                    .get_allowing_opath(FdNumber::from_raw(translated_bar.fds[1] as i32))
                    .expect_err("file should be closed")
                    == EBADF
            );
        });
    }
    #[fuchsia::test]
    async fn transaction_receiver_exits_after_getting_fd_array() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Open a file in the sender process that we won't be using. It is there to occupy a file
            // descriptor so that the translation doesn't happen to use the same FDs for receiver and
            // sender, potentially hiding a bug.
            sender.task.add_file(PanickingFile::new_file(&sender.task), FdFlags::empty()).unwrap();

            // Open two files in the sender process. These will be sent in the transaction.
            let files =
                [PanickingFile::new_file(&sender.task), PanickingFile::new_file(&sender.task)];
            let sender_fds = files
                .into_iter()
                .map(|file| sender.task.add_file(file, FdFlags::CLOEXEC).expect("add file"))
                .collect::<Vec<_>>();

            // Ensure that the receiver task has no file descriptors.
            assert!(receiver.task.files.get_all_fds().is_empty(), "receiver already has files");

            // Allocate memory in the sender to hold all the buffers that will get submitted to the
            // binder driver.
            let sender_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);
            let mut writer = UserMemoryWriter::new(&sender.task, sender_addr);

            // Serialize a simple buffer. This will ensure that the FD array being translated is not at
            // the beginning of the buffer, exercising the offset math.
            let sender_padding_addr = writer.write(&[0; 8]);

            // Serialize a C struct with an fd array.
            #[repr(C)]
            #[derive(IntoBytes, KnownLayout, FromBytes, Immutable)]
            struct Bar {
                len: u32,
                fds: [u32; 2],
                _padding: u32,
            }
            let sender_bar_addr = writer.write_object(&Bar {
                len: 2,
                fds: [sender_fds[0].raw() as u32, sender_fds[1].raw() as u32],
                _padding: 0,
            });

            // Mark the start of the transaction data.
            let transaction_data_addr = writer.current_address();

            // Write the buffer object representing the padding.
            let sender_padding_buffer_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_padding_addr.ptr() as u64,
                length: 8,
                ..binder_buffer_object::default()
            });

            // Write the buffer object representing the C struct `Bar`.
            let sender_buffer_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_bar_addr.ptr() as u64,
                length: std::mem::size_of::<Bar>() as u64,
                ..binder_buffer_object::default()
            });

            // Write the fd array object that tells the kernel where the file descriptors are in the
            // `Bar` buffer.
            let sender_fd_array_addr = writer.write_object(&binder_fd_array_object {
                hdr: binder_object_header { type_: BINDER_TYPE_FDA },
                pad: 0,
                num_fds: sender_fds.len() as u64,
                // The index in the offsets array of the parent buffer.
                parent: 1,
                // The location in the parent buffer where the FDs are, which need to be duped.
                parent_offset: offset_of!(Bar, fds) as u64,
            });

            // Write the offsets array.
            let offsets_addr = writer.current_address();
            writer.write_object(&((sender_padding_buffer_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_buffer_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_fd_array_addr - transaction_data_addr) as u64));

            let end_data_addr = writer.current_address();

            // Construct the input for the binder driver to process.
            let input = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    data_size: (offsets_addr - transaction_data_addr) as u64,
                    offsets_size: (end_data_addr - offsets_addr) as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: transaction_data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                    ..binder_transaction_data::new_zeroed()
                },
                buffers_size: std::mem::size_of::<Bar>() as u64 + 8,
            };

            // Perform the translation and copying.
            device
                .handle_transaction(&mut locked, &sender.task, &sender.proc, &sender.thread, input)
                .expect("transaction queued");
        });
    }

    #[fuchsia::test]
    async fn transaction_fd_array_sender_cancels() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Open a file in the sender process that we won't be using. It is there to occupy a file
            // descriptor so that the translation doesn't happen to use the same FDs for receiver and
            // sender, potentially hiding a bug.
            sender.task.add_file(PanickingFile::new_file(&sender.task), FdFlags::empty()).unwrap();

            // Open two files in the sender process. These will be sent in the transaction.
            let files =
                [PanickingFile::new_file(&sender.task), PanickingFile::new_file(&sender.task)];
            let sender_fds = files
                .into_iter()
                .map(|file| sender.task.add_file(file, FdFlags::CLOEXEC).expect("add file"))
                .collect::<Vec<_>>();

            // Ensure that the receiver task has no file descriptors.
            assert!(receiver.task.files.get_all_fds().is_empty(), "receiver already has files");

            // Allocate memory in the sender to hold all the buffers that will get submitted to the
            // binder driver.
            let sender_addr =
                map_memory(&mut locked, &sender.task, UserAddress::default(), *PAGE_SIZE);
            let mut writer = UserMemoryWriter::new(&sender.task, sender_addr);

            // Serialize a simple buffer. This will ensure that the FD array being translated is not at
            // the beginning of the buffer, exercising the offset math.
            let sender_padding_addr = writer.write(&[0; 8]);

            // Serialize a C struct with an fd array.
            #[repr(C)]
            #[derive(IntoBytes, KnownLayout, FromBytes, Immutable)]
            struct Bar {
                len: u32,
                fds: [u32; 2],
                _padding: u32,
            }
            let sender_bar_addr = writer.write_object(&Bar {
                len: 2,
                fds: [sender_fds[0].raw() as u32, sender_fds[1].raw() as u32],
                _padding: 0,
            });

            // Mark the start of the transaction data.
            let transaction_data_addr = writer.current_address();

            // Write the buffer object representing the padding.
            let sender_padding_buffer_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_padding_addr.ptr() as u64,
                length: 8,
                ..binder_buffer_object::default()
            });

            // Write the buffer object representing the C struct `Bar`.
            let sender_buffer_addr = writer.write_object(&binder_buffer_object {
                hdr: binder_object_header { type_: BINDER_TYPE_PTR },
                buffer: sender_bar_addr.ptr() as u64,
                length: std::mem::size_of::<Bar>() as u64,
                ..binder_buffer_object::default()
            });

            // Write the fd array object that tells the kernel where the file descriptors are in the
            // `Bar` buffer.
            let sender_fd_array_addr = writer.write_object(&binder_fd_array_object {
                hdr: binder_object_header { type_: BINDER_TYPE_FDA },
                pad: 0,
                num_fds: sender_fds.len() as u64,
                // The index in the offsets array of the parent buffer.
                parent: 1,
                // The location in the parent buffer where the FDs are, which need to be duped.
                parent_offset: offset_of!(Bar, fds) as u64,
            });

            // Write the offsets array.
            let offsets_addr = writer.current_address();
            writer.write_object(&((sender_padding_buffer_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_buffer_addr - transaction_data_addr) as u64));
            writer.write_object(&((sender_fd_array_addr - transaction_data_addr) as u64));

            let end_data_addr = writer.current_address();

            // Construct the input for the binder driver to process.
            let input = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    data_size: (offsets_addr - transaction_data_addr) as u64,
                    offsets_size: (end_data_addr - offsets_addr) as u64,
                    data: binder_transaction_data__bindgen_ty_2 {
                        ptr: binder_transaction_data__bindgen_ty_2__bindgen_ty_1 {
                            buffer: transaction_data_addr.ptr() as u64,
                            offsets: offsets_addr.ptr() as u64,
                        },
                    },
                    ..binder_transaction_data::new_zeroed()
                },
                buffers_size: std::mem::size_of::<Bar>() as u64 + 8,
            };

            // Perform the translation and copying.
            let (_, transient_state) = device
                .copy_transaction_buffers(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &input,
                    None,
                )
                .expect("copy_transaction_buffers");

            // The receiver should have the fd.
            let fd = transient_state.state.as_ref().unwrap().owned_fds[0];
            assert!(
                receiver.task.files.get_allowing_opath(fd).is_ok(),
                "file should be translated"
            );

            // Release the result, which should close the fds in the receiver.
            transient_state.release(());
            assert!(
                receiver.task.files.get_allowing_opath(fd).expect_err("file should be closed")
                    == EBADF
            );
        });
    }

    #[fuchsia::test]
    async fn transaction_translation_fails_on_invalid_handle() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            let mut transaction_data = vec![];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: 42,
            }));

            let transaction_ref_error = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &[0 as binder_uintptr_t],
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect_err("translate handles unexpectedly succeeded");

            assert_eq!(transaction_ref_error, TransactionError::Failure);
        });
    }

    #[fuchsia::test]
    async fn transaction_translation_fails_on_invalid_object_type() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            let mut transaction_data = vec![];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_WEAK_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: 42,
            }));

            let transaction_ref_error = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &[0 as binder_uintptr_t],
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect_err("translate handles unexpectedly succeeded");

            assert_eq!(transaction_ref_error, TransactionError::Malformed(errno!(EINVAL)));
        });
    }

    #[fuchsia::test]
    async fn transaction_drop_references_on_failed_transaction() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            let binder_object = LocalBinderObject {
                weak_ref_addr: UserAddress::from(0x0000000000000010),
                strong_ref_addr: UserAddress::from(0x0000000000000100),
            };

            let mut transaction_data = vec![];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_BINDER,
                flags: 0,
                cookie: binder_object.strong_ref_addr.ptr() as u64,
                __bindgen_anon_1.binder: binder_object.weak_ref_addr.ptr() as u64,
            }));
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_HANDLE,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: 42,
            }));

            device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &[
                        0 as binder_uintptr_t,
                        std::mem::size_of::<flat_binder_object>() as binder_uintptr_t,
                    ],
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect_err("translate handles unexpectedly succeeded");

            // Ensure that the handle created in the receiving process is not present.
            assert!(
                receiver.proc.lock().handles.get(0).is_none(),
                "handle present when it should have been dropped"
            );
        });
    }

    // Open the binder device, which creates an instance of the binder device associated with
    // the process.
    fn open_binder_fd(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        binder_driver: &BinderDevice,
    ) -> FileHandle {
        // `open()` requires an `FsNode` so create one in `AnonFs`.
        let fs = anon_fs(current_task.kernel());
        let node = fs.create_node(
            &current_task,
            Anon::default(),
            FsNodeInfo::new_factory(FileMode::from_bits(0o600), current_task.as_fscred()),
        );

        let mut locked = locked.cast_locked::<DeviceOpen>();
        FileObject::new_anonymous(
            current_task,
            binder_driver
                .open(&mut locked, &current_task, DeviceType::NONE, &node, OpenFlags::RDWR)
                .expect("binder dev open failed"),
            node,
            OpenFlags::RDWR,
        )
    }

    #[fuchsia::test]
    async fn close_binder() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let binder_driver = BinderDevice::default();

            let binder_fd = open_binder_fd(&mut locked, &current_task, &binder_driver);
            let binder_connection =
                binder_fd.downcast_file::<BinderConnection>().expect("must be a BinderConnection");
            let identifier = binder_connection.identifier;

            // Ensure that the binder driver has created process state.
            binder_driver
                .find_process(identifier)
                .expect("failed to find process")
                .release(current_task.kernel());

            // Close the file descriptor.
            std::mem::drop(binder_fd);
            current_task.trigger_delayed_releaser(&mut locked);

            // Verify that the process state no longer exists.
            binder_driver.find_process(identifier).expect_err("process was not cleaned up");
        });
    }

    #[fuchsia::test]
    async fn flush_kicks_threads() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let binder_driver = BinderDevice::default();

            // Open the binder device, which creates an instance of the binder device associated with
            // the process.
            let binder_fd = open_binder_fd(&mut locked, &current_task, &binder_driver);
            let binder_connection =
                binder_fd.downcast_file::<BinderConnection>().expect("must be a BinderConnection");
            let binder_proc = binder_connection.proc(&current_task).unwrap();
            let binder_thread = binder_proc.lock().find_or_register_thread(binder_proc.pid);

            let thread = std::thread::spawn({
                let task = current_task.weak_task();
                let binder_proc = WeakRef::from(&binder_proc);
                move || {
                    let task = if let Some(task) = task.upgrade() {
                        task
                    } else {
                        return;
                    };
                    let binder_proc = if let Some(binder_proc) = binder_proc.upgrade() {
                        binder_proc
                    } else {
                        return;
                    };
                    // Wait for the task to start waiting.
                    while !task.read().is_blocked() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    // Do the kick.
                    binder_proc.kick_all_threads();
                }
            });

            let read_buffer_addr =
                map_memory(&mut locked, &current_task, UserAddress::default(), *PAGE_SIZE);
            let bytes_read = binder_driver
                .handle_thread_read(
                    &current_task,
                    &binder_proc,
                    &binder_thread,
                    &UserBuffer { address: read_buffer_addr, length: *PAGE_SIZE as usize },
                )
                .unwrap();
            assert_eq!(bytes_read, 0);
            thread.join().expect("join");
            binder_thread.release(current_task.kernel());
            binder_proc.release(current_task.kernel());
        });
    }

    #[fuchsia::test]
    async fn decrementing_refs_on_dead_binder_succeeds() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let owner = BinderProcessFixture::new(&mut locked, current_task, &device);
            let client = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Register an object with the owner.
            let guard = owner.proc.lock().find_or_register_object(
                &owner.thread,
                LocalBinderObject {
                    weak_ref_addr: UserAddress::from(0x0000000000000001),
                    strong_ref_addr: UserAddress::from(0x0000000000000002),
                },
                BinderObjectFlags::empty(),
            );

            // Keep a weak reference to the object.
            let weak_object = Arc::downgrade(&guard.binder_object);

            // Insert a handle to the object in the client. This also retains a strong reference.
            let handle = client
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Grab a weak reference.
            client
                .proc
                .lock()
                .handles
                .inc_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect("inc_weak");

            // Now the owner process dies.
            std::mem::drop(owner);

            // Confirm that the object is considered dead. The representation is still alive, but the
            // owner is dead.
            let (object, guard) = client
                .proc
                .lock()
                .handles
                .get(handle.object_index())
                .expect("expected handle not present");
            guard.release(&mut RefCountActions::default_released());
            assert!(object.owner.upgrade().is_none(), "owner should be dead");
            std::mem::drop(object);

            // Decrement the weak reference. This should prove that the handle is still occupied.
            client
                .proc
                .lock()
                .handles
                .dec_weak(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_weak");

            // Decrement the last strong reference.
            client
                .proc
                .lock()
                .handles
                .dec_strong(handle.object_index(), &mut RefCountActions::default_released())
                .expect("dec_strong");

            // Confirm that now the handle has been removed from the table.
            assert!(
                client.proc.lock().handles.get(handle.object_index()).is_none(),
                "handle should have been dropped"
            );

            // Now the binder object representation should also be gone.
            assert!(weak_object.upgrade().is_none(), "object should be dead");
        });
    }

    #[fuchsia::test]
    async fn death_notification_fires_when_process_dies() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Register an object with the owner.
            let guard = sender.proc.lock().find_or_register_object(
                &sender.thread,
                LocalBinderObject {
                    weak_ref_addr: UserAddress::from(0x0000000000000001),
                    strong_ref_addr: UserAddress::from(0x0000000000000002),
                },
                BinderObjectFlags::empty(),
            );

            // Insert a handle to the object in the client. This also retains a strong reference.
            let handle = receiver
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            const DEATH_NOTIFICATION_COOKIE: binder_uintptr_t = 0xDEADBEEF;

            // Register a death notification handler.
            receiver
                .proc
                .handle_request_death_notification(handle, DEATH_NOTIFICATION_COOKIE)
                .expect("request death notification");

            // Now the owner process dies.
            std::mem::drop(sender);

            // The client process should have a notification waiting.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::DeadBinder(DEATH_NOTIFICATION_COOKIE))
            );
        });
    }

    #[fuchsia::test]
    async fn death_notification_fires_when_request_for_death_notification_is_made_on_dead_binder() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Register an object with the sender.
            let guard = sender.proc.lock().find_or_register_object(
                &sender.thread,
                LocalBinderObject {
                    weak_ref_addr: UserAddress::from(0x0000000000000001),
                    strong_ref_addr: UserAddress::from(0x0000000000000002),
                },
                BinderObjectFlags::empty(),
            );

            // Insert a handle to the object in the receiver. This also retains a strong reference.
            let handle = receiver
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Now the sender process dies.
            std::mem::drop(sender);

            const DEATH_NOTIFICATION_COOKIE: binder_uintptr_t = 0xDEADBEEF;

            // Register a death notification handler.
            receiver
                .proc
                .handle_request_death_notification(handle, DEATH_NOTIFICATION_COOKIE)
                .expect("request death notification");

            // The receiver thread should not have a notification, as the calling thread is not allowed
            // to receive it, or else a deadlock may occur if the thread is in the middle of a
            // transaction. Since there is only one thread, check the process command queue.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::DeadBinder(DEATH_NOTIFICATION_COOKIE))
            );
        });
    }

    #[fuchsia::test]
    async fn death_notification_is_cleared_before_process_dies() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let owner = BinderProcessFixture::new(&mut locked, current_task, &device);
            let client = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Register an object with the owner.
            let guard = owner.proc.lock().find_or_register_object(
                &owner.thread,
                LocalBinderObject {
                    weak_ref_addr: UserAddress::from(0x0000000000000001),
                    strong_ref_addr: UserAddress::from(0x0000000000000002),
                },
                BinderObjectFlags::empty(),
            );

            // Insert a handle to the object in the client. This also retains a strong reference.
            let handle = client
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            let death_notification_cookie = 0xDEADBEEF;

            // Register a death notification handler.
            client
                .proc
                .handle_request_death_notification(handle, death_notification_cookie)
                .expect("request death notification");

            // Now clear the death notification handler.
            client
                .proc
                .handle_clear_death_notification(handle, death_notification_cookie)
                .expect("clear death notification");

            // Check that the client received an acknowlgement
            {
                let mut queue = client.proc.command_queue.lock();
                assert_eq!(queue.commands.len(), 1);
                assert!(matches!(queue.commands[0], Command::ClearDeathNotificationDone(_)));

                // Clear the command queue.
                queue.commands.clear();
            }

            // Pretend the client thread is waiting for commands, so that it can be scheduled commands.
            let fake_waiter = Waiter::new();
            {
                let mut state = client.thread.lock();
                state.registration = RegistrationState::Main;
                state.command_queue.waiters.wait_async(&fake_waiter);
            }

            // Now the owner process dies.
            std::mem::drop(owner);

            // The client thread should have no notification.
            assert!(client.thread.lock().command_queue.is_empty());

            // The client process should have no notification.
            assert!(client.proc.command_queue.lock().commands.is_empty());
        });
    }

    #[fuchsia::test]
    async fn send_fd_in_transaction() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            // Open a file in the sender process.
            let file = PanickingFile::new_file(&sender.task);
            let sender_fd = sender.task.add_file(file.clone(), FdFlags::CLOEXEC).expect("add file");

            // Send the fd in a transaction. `flags` and `cookie` are set so that we can ensure binder
            // driver doesn't touch them/passes them through.
            let mut transaction_data = struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_FD,
                flags: 42,
                cookie: 51,
                __bindgen_anon_1.handle: sender_fd.raw() as u32,
            });
            let offsets = [0];

            let transient_transaction_state = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles");

            // Simulate success by converting the transient state.
            let transaction_state = transient_transaction_state.into_state();

            // The receiver should now have a file.
            let receiver_fd = receiver
                .task
                .files
                .get_all_fds()
                .first()
                .cloned()
                .expect("receiver should have FD");

            // The FD should have the same flags.
            assert_eq!(
                receiver.task.files.get_fd_flags_allowing_opath(receiver_fd).expect("get flags"),
                FdFlags::CLOEXEC
            );

            // The FD should point to the same file.
            assert!(
                Arc::ptr_eq(
                    &receiver
                        .task
                        .files
                        .get_allowing_opath(receiver_fd)
                        .expect("receiver should have FD"),
                    &file
                ),
                "FDs from sender and receiver don't point to the same file"
            );

            let expected_transaction_data = struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_FD,
                flags: 42,
                cookie: 51,
                __bindgen_anon_1.handle: receiver_fd.raw() as u32,
            });

            assert_eq!(expected_transaction_data, transaction_data);
            transaction_state.release(());
        });
    }

    #[fuchsia::test]
    async fn cleanup_fd_in_failed_transaction() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            // Open a file in the sender process.
            let sender_fd = sender
                .task
                .add_file(PanickingFile::new_file(&sender.task), FdFlags::CLOEXEC)
                .expect("add file");

            // Send the fd in a transaction.
            let mut transaction_data = struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_FD,
                flags: 0,
                cookie: 0,
                __bindgen_anon_1.handle: sender_fd.raw() as u32,
            });
            let offsets = [0];

            let transaction_state = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles");

            assert!(!receiver.task.files.get_all_fds().is_empty(), "receiver should have a file");

            // Simulate an error, which will release the transaction state.
            transaction_state.release(());

            assert!(
                receiver.task.files.get_all_fds().is_empty(),
                "receiver should not have any files"
            );
        });
    }

    #[fuchsia::test]
    async fn cleanup_refs_in_successful_transaction() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);
            let mut receiver_shared_memory = receiver.lock_shared_memory();
            let mut allocations =
                receiver_shared_memory.allocate_buffers(0, 0, 0, 0).expect("allocate buffers");

            const BINDER_OBJECT: LocalBinderObject = LocalBinderObject {
                weak_ref_addr: UserAddress::const_from(0x0000000000000010),
                strong_ref_addr: UserAddress::const_from(0x0000000000000100),
            };

            const DATA_PREAMBLE: &[u8; 5] = b"stuff";

            let mut transaction_data = vec![];
            transaction_data.extend(DATA_PREAMBLE);
            let offsets = [transaction_data.len() as binder_uintptr_t];
            transaction_data.extend(struct_with_union_into_bytes!(flat_binder_object {
                hdr.type_: BINDER_TYPE_BINDER,
                flags: 0,
                cookie: BINDER_OBJECT.strong_ref_addr.ptr() as u64,
                __bindgen_anon_1.binder: BINDER_OBJECT.weak_ref_addr.ptr() as u64,
            }));

            const EXPECTED_HANDLE: Handle = Handle::from_raw(1);

            let transaction_state = device
                .translate_objects(
                    &mut locked,
                    &sender.task,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    &receiver.task,
                    &receiver.proc,
                    &offsets,
                    &mut transaction_data,
                    &mut allocations.scatter_gather_buffer,
                )
                .expect("failed to translate handles");

            let (object, guard) = receiver
                .proc
                .lock()
                .handles
                .get(EXPECTED_HANDLE.object_index())
                .expect("expected handle not present");
            guard.release(&mut RefCountActions::default_released());
            object.ack_acquire(&mut RefCountActions::default_released()).expect("ack_acquire");

            // Verify that a strong acquire command is sent to the sender process (on the same thread
            // that sent the transaction).
            assert_matches!(
                sender.thread.lock().command_queue.commands.front(),
                Some(Command::AcquireRef(BINDER_OBJECT))
            );
            sender.thread.lock().command_queue.commands.pop_front().unwrap();

            // Simulate a successful transaction by converting the transient state.
            let transaction_state = transaction_state.into_state();
            transaction_state.release(());

            // Verify that a strong release command is sent to the sender process.
            assert_matches!(
                &sender.proc.command_queue.lock().commands.front(),
                Some(Command::ReleaseRef(BINDER_OBJECT))
            );
        });
    }

    #[fuchsia::test]
    async fn transaction_error_dispatch() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let proc = BinderProcessFixture::new(&mut locked, current_task, &device);

            TransactionError::Malformed(errno!(EINVAL)).dispatch(&proc.thread).expect("no error");
            assert_matches!(
                proc.thread.lock().command_queue.pop_front(),
                Some(Command::Error(val)) if val == EINVAL.return_value() as i32
            );

            TransactionError::Failure.dispatch(&proc.thread).expect("no error");
            assert_matches!(
                proc.thread.lock().command_queue.pop_front(),
                Some(Command::FailedReply)
            );

            TransactionError::Dead.dispatch(&proc.thread).expect("no error");
            assert_matches!(
                proc.thread.lock().command_queue.pop_front(),
                Some(Command::DeadReply { pop_transaction: false })
            );
        });
    }

    #[fuchsia::test]
    async fn next_oneway_transaction_scheduled_after_buffer_freed() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (object, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Construct a oneway transaction to send from the sender to the receiver.
            const FIRST_TRANSACTION_CODE: u32 = 42;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: FIRST_TRANSACTION_CODE,
                    flags: transaction_flags_TF_ONE_WAY,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };

            // Submit the transaction.
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");

            // The thread is ineligible to take the command (not sleeping) so check the process queue.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::OnewayTransaction(TransactionData {
                    code: FIRST_TRANSACTION_CODE,
                    ..
                }))
            );

            // The object should not have the transaction queued on it, as it was immediately scheduled.
            // But it should be marked as handling a oneway.
            assert!(
                object.lock().handling_oneway_transaction,
                "object oneway queue should be marked as being handled"
            );

            // Queue another transaction.
            const SECOND_TRANSACTION_CODE: u32 = 43;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: SECOND_TRANSACTION_CODE,
                    flags: transaction_flags_TF_ONE_WAY,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("transaction queued");

            // There should now be an entry in the queue.
            assert_eq!(object.lock().oneway_transactions.len(), 1);

            // The process queue should be unchanged. Simulate dispatching the command.
            let buffer_addr = match receiver
                .proc
                .command_queue
                .lock()
                .commands
                .pop_front()
                .expect("the first oneway transaction should be queued on the process")
            {
                Command::OnewayTransaction(TransactionData {
                    code: FIRST_TRANSACTION_CODE,
                    buffers: TransactionBuffers { data, .. },
                    ..
                }) => data.address,
                _ => panic!("unexpected command in process queue"),
            };

            // Now the receiver issues the `BC_FREE_BUFFER` command, which should queue up the next
            // oneway transaction, guaranteeing sequential execution.
            receiver.proc.handle_free_buffer(buffer_addr).expect("failed to free buffer");

            assert!(
                object.lock().oneway_transactions.is_empty(),
                "oneway queue should now be empty"
            );
            assert!(
                object.lock().handling_oneway_transaction,
                "object oneway queue should still be marked as being handled"
            );

            // The process queue should have a new transaction. Simulate dispatching the command.
            let buffer_addr = match receiver
                .proc
                .command_queue
                .lock()
                .commands
                .pop_front()
                .expect("the second oneway transaction should be queued on the process")
            {
                Command::OnewayTransaction(TransactionData {
                    code: SECOND_TRANSACTION_CODE,
                    buffers: TransactionBuffers { data, .. },
                    ..
                }) => data.address,
                _ => panic!("unexpected command in process queue"),
            };

            // Now the receiver issues the `BC_FREE_BUFFER` command, which should end oneway handling.
            receiver.proc.handle_free_buffer(buffer_addr).expect("failed to free buffer");

            assert!(
                object.lock().oneway_transactions.is_empty(),
                "oneway queue should still be empty"
            );
            assert!(
                !object.lock().handling_oneway_transaction,
                "object oneway queue should no longer be marked as being handled"
            );
        });
    }

    #[fuchsia::test]
    async fn synchronous_transactions_bypass_oneway_transaction_queue() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (object, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Construct a oneway transaction to send from the sender to the receiver.
            const ONEWAY_TRANSACTION_CODE: u32 = 42;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: ONEWAY_TRANSACTION_CODE,
                    flags: transaction_flags_TF_ONE_WAY,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };

            // Submit the transaction twice so that the queue is populated.
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");

            // The thread is ineligible to take the command (not sleeping) so check (and dequeue)
            // the process queue.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.pop_front(),
                Some(Command::OnewayTransaction(TransactionData {
                    code: ONEWAY_TRANSACTION_CODE,
                    ..
                }))
            );

            // The object should also have the second transaction queued on it.
            assert!(
                object.lock().handling_oneway_transaction,
                "object oneway queue should be marked as being handled"
            );
            assert_eq!(
                object.lock().oneway_transactions.len(),
                1,
                "object oneway queue should have second transaction queued"
            );

            // Queue a synchronous (request/response) transaction.
            const SYNC_TRANSACTION_CODE: u32 = 43;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: SYNC_TRANSACTION_CODE,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("sync transaction queued");

            assert_eq!(
                object.lock().oneway_transactions.len(),
                1,
                "oneway queue should not have grown"
            );

            // The process queue should now have the synchronous transaction queued.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.pop_front(),
                Some(Command::Transaction {
                    data: TransactionData { code: SYNC_TRANSACTION_CODE, .. },
                    ..
                })
            );
        });
    }

    #[fuchsia::test]
    async fn dead_reply_when_transaction_recipient_proc_dies() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Construct a synchronous transaction to send from the sender to the receiver.
            const FIRST_TRANSACTION_CODE: u32 = 42;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: FIRST_TRANSACTION_CODE,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };

            // Submit the transaction.
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");

            // Check that there are no commands waiting for the sending thread.
            assert!(sender.thread.lock().command_queue.is_empty());

            // Check that the receiving process has a transaction scheduled.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::Transaction { .. })
            );

            // Drop the receiving process.
            std::mem::drop(receiver);

            // Check that there is a dead reply command for the sending thread.
            assert_matches!(
                sender.thread.lock().command_queue.commands.front(),
                Some(Command::DeadReply { pop_transaction: true })
            );
            assert_matches!(sender.thread.lock().transactions.pop(), Some(TransactionRole::Sender));
        });
    }

    #[fuchsia::test]
    async fn dead_reply_when_transaction_recipient_thread_dies() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Construct a synchronous transaction to send from the sender to the receiver.
            const FIRST_TRANSACTION_CODE: u32 = 42;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: FIRST_TRANSACTION_CODE,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };

            // Submit the transaction.
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");

            // Check that there are no commands waiting for the sending thread.
            assert!(sender.thread.lock().command_queue.is_empty());

            // Check that the receiving process has a transaction scheduled.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::Transaction { .. })
            );

            // Drop the receiving process.
            std::mem::drop(receiver);

            // Check that there is a dead reply command for the sending thread.
            assert_matches!(
                sender.thread.lock().command_queue.commands.front(),
                Some(Command::DeadReply { pop_transaction: true })
            );
            assert_matches!(sender.thread.lock().transactions.pop(), Some(TransactionRole::Sender));
        });
    }

    #[fuchsia::test]
    async fn dead_reply_when_transaction_recipient_thread_dies_while_processing_reply() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Construct a synchronous transaction to send from the sender to the receiver.
            const FIRST_TRANSACTION_CODE: u32 = 42;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: FIRST_TRANSACTION_CODE,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };

            // Make the receiver thread look eligible for transactions.
            // Pretend the client thread is waiting for commands, so that it can be scheduled commands.
            let fake_waiter = Waiter::new();
            {
                let mut thread_state = receiver.thread.lock();
                thread_state.registration = RegistrationState::Main;
                thread_state.command_queue.waiters.wait_async(&fake_waiter);
            }

            // Submit the transaction.
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");

            // Check that there are no commands waiting for the sending thread.
            assert!(sender.thread.lock().command_queue.is_empty());

            // Check that the receiving process has a transaction scheduled.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::Transaction { .. })
            );

            // Have the thread dequeue the command.
            let read_buffer_addr =
                map_memory(&mut locked, &receiver.task, UserAddress::default(), *PAGE_SIZE);
            device
                .handle_thread_read(
                    &receiver.task,
                    &receiver.proc,
                    &receiver.thread,
                    &UserBuffer { address: read_buffer_addr, length: *PAGE_SIZE as usize },
                )
                .expect("read command");

            // The thread should now have an empty command list and an ongoing transaction.
            assert!(receiver.thread.lock().command_queue.is_empty());
            assert!(!receiver.thread.lock().transactions.is_empty());

            // Drop the receiving process and thread.
            std::mem::drop(receiver);

            // Check that there is a dead reply command for the sending thread.
            assert_matches!(
                sender.thread.lock().command_queue.commands.front(),
                Some(Command::DeadReply { pop_transaction: true })
            );
            assert_matches!(sender.thread.lock().transactions.pop(), Some(TransactionRole::Sender));
        });
    }

    #[fuchsia::test]
    async fn failed_reply_when_transaction_reply_is_too_big() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let device = BinderDevice::default();
            let sender = BinderProcessFixture::new(&mut locked, current_task, &device);
            let receiver = BinderProcessFixture::new(&mut locked, current_task, &device);

            // Insert a binder object for the receiver, and grab a handle to it in the sender.
            const OBJECT_ADDR: UserAddress = UserAddress::const_from(0x01);
            let (_, guard) =
                register_binder_object(&receiver.proc, OBJECT_ADDR, OBJECT_ADDR + 1u64);
            let handle = sender
                .proc
                .lock()
                .handles
                .insert_for_transaction(guard, &mut RefCountActions::default_released());

            // Construct a synchronous transaction to send from the sender to the receiver.
            const FIRST_TRANSACTION_CODE: u32 = 42;
            let transaction = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: FIRST_TRANSACTION_CODE,
                    target: binder_transaction_data__bindgen_ty_1 { handle: handle.into() },
                    ..binder_transaction_data::default()
                },
                buffers_size: 0,
            };

            // Make the receiver thread look eligible for transactions.
            // Pretend the client thread is waiting for commands, so that it can be scheduled commands.
            let fake_waiter = Waiter::new();
            {
                let mut thread_state = receiver.thread.lock();
                thread_state.registration = RegistrationState::Main;
                thread_state.command_queue.waiters.wait_async(&fake_waiter);
            }

            // Submit the transaction.
            device
                .handle_transaction(
                    &mut locked,
                    &sender.task,
                    &sender.proc,
                    &sender.thread,
                    transaction,
                )
                .expect("failed to handle the transaction");

            // Check that there are no commands waiting for the sending thread.
            assert!(sender.thread.lock().command_queue.is_empty());

            // Check that the receiving process has a transaction scheduled.
            assert_matches!(
                receiver.proc.command_queue.lock().commands.front(),
                Some(Command::Transaction { .. })
            );

            // Have the thread dequeue the command.
            let read_buffer_addr =
                map_memory(&mut locked, &receiver.task, UserAddress::default(), *PAGE_SIZE);
            device
                .handle_thread_read(
                    &receiver.task,
                    &receiver.proc,
                    &receiver.thread,
                    &UserBuffer { address: read_buffer_addr, length: *PAGE_SIZE as usize },
                )
                .expect("read command");

            // The thread should now have an empty command list and an ongoing transaction.
            assert!(receiver.thread.lock().command_queue.is_empty());
            assert!(!receiver.thread.lock().transactions.is_empty());

            // Respond to the transaction with too much data.
            let reply = binder_transaction_data_sg {
                transaction_data: binder_transaction_data {
                    code: FIRST_TRANSACTION_CODE,
                    ..binder_transaction_data::default()
                },
                buffers_size: (u64::MAX / 2) & !7,
            };

            // Submit the reply.
            assert_eq!(
                device
                    .handle_reply(
                        &mut locked,
                        &receiver.task,
                        &receiver.proc,
                        &receiver.thread,
                        reply
                    )
                    .expect_err("transaction should have failed"),
                TransactionError::Failure
            );

            assert!(receiver.thread.lock().transactions.is_empty());
            assert_matches!(
                sender.thread.lock().command_queue.pop_front(),
                Some(Command::FailedReply)
            );
        });
    }

    #[fuchsia::test]
    async fn connect_to_multiple_binder() {
        spawn_kernel_and_run(|mut locked, current_task| {
            let driver = BinderDevice::default();

            // Opening the driver twice from the same task must succeed.
            let _d1 = open_binder_fd(&mut locked, &current_task, &driver);
            let _d2 = open_binder_fd(&mut locked, &current_task, &driver);
        });
    }

    pub type TestFdTable = BTreeMap<i32, fbinder::FileHandle>;
    /// Run a test implementation of the ProcessAccessor protocol.
    /// The test implementation starts with an empty fd table, and updates it depending on the
    /// client calls. The future will resolve when the client is disconnected and return the
    /// current fd table at that point.
    pub async fn run_process_accessor(
        server_end: ServerEnd<fbinder::ProcessAccessorMarker>,
    ) -> Result<TestFdTable, anyhow::Error> {
        let mut stream = fbinder::ProcessAccessorRequestStream::from_channel(
            fasync::Channel::from_channel(server_end.into_channel()),
        );
        // The fd table is per connection.
        let mut next_fd = 0;
        let mut fds: TestFdTable = Default::default();
        'event_loop: while let Some(event) = stream.try_next().await? {
            match event {
                fbinder::ProcessAccessorRequest::WriteMemory { address, content, responder } => {
                    let size = content.get_content_size()?;
                    // SAFETY: This is not safe and rely on the client being correct.
                    let buffer = unsafe {
                        std::slice::from_raw_parts_mut(address as *mut u8, size as usize)
                    };
                    content.read(buffer, 0)?;
                    responder.send(Ok(()))?;
                }
                fbinder::ProcessAccessorRequest::FileRequest { payload, responder } => {
                    let mut response = fbinder::FileResponse::default();
                    for fd in payload.close_requests.unwrap_or(vec![]) {
                        if fds.remove(&fd).is_none() {
                            responder.send(Err(fposix::Errno::Ebadf))?;
                            continue 'event_loop;
                        }
                    }
                    for fd in payload.get_requests.unwrap_or(vec![]) {
                        if let Some(file) = fds.remove(&fd) {
                            response.get_responses.get_or_insert_with(Vec::new).push(file);
                        } else {
                            responder.send(Err(fposix::Errno::Ebadf))?;
                            continue 'event_loop;
                        }
                    }
                    for file in payload.add_requests.unwrap_or(vec![]) {
                        let fd = next_fd;
                        next_fd += 1;
                        fds.insert(fd, file);
                        response.add_responses.get_or_insert_with(Vec::new).push(fd);
                    }
                    responder.send(Ok(response))?;
                }
                fbinder::ProcessAccessorRequest::_UnknownMethod { ordinal, .. } => {
                    log_warn!("Unknown ProcessAccessor ordinal: {}", ordinal);
                }
            }
        }
        Ok(fds)
    }

    /// Spawn a new thread that will run a test implementation of the ProcessAccessor
    /// protocol.
    /// The test implementation starts with an empty fd table, and updates it depending
    /// on the client calls. The thread will stop when the client is disconnected.
    /// This function will then return the current fd table at that point.
    fn spawn_new_process_accessor_thread(
        server_end: ServerEnd<fbinder::ProcessAccessorMarker>,
    ) -> std::thread::JoinHandle<Result<TestFdTable, anyhow::Error>> {
        std::thread::spawn(move || {
            let mut executor = LocalExecutor::new();
            executor.run_singlethreaded(run_process_accessor(server_end))
        })
    }

    #[::fuchsia::test]
    async fn remote_binder_task() {
        const vector_size: usize = 128 * 1024 * 1024;
        let (process_accessor_client_end, process_accessor_server_end) =
            create_endpoints::<fbinder::ProcessAccessorMarker>();

        let process_accessor_thread =
            spawn_new_process_accessor_thread(process_accessor_server_end);

        let process_accessor = fbinder::ProcessAccessorSynchronousProxy::new(
            process_accessor_client_end.into_channel(),
        );

        spawn_kernel_and_run(|locked, task| {
            let process = fuchsia_runtime::process_self()
                .duplicate(zx::Rights::SAME_RIGHTS)
                .expect("process");
            let remote_binder_task =
                RemoteResourceAccessor { process_accessor, process, kernel: task.kernel().clone() };
            let mut vector = Vec::with_capacity(vector_size);
            for i in 0..vector_size {
                vector.push((i & 255) as u8);
            }
            let other_vector = remote_binder_task
                .read_memory_to_vec((vector.as_ptr() as u64).into(), vector_size)
                .expect("read_memory");
            assert_eq!(vector[1], 1);
            assert_eq!(vector, other_vector);
            vector.clear();
            vector.resize(vector_size, 0);
            remote_binder_task
                .write_memory((vector.as_ptr() as u64).into(), &other_vector)
                .expect("read_memory");
            assert_eq!(vector[1], 1);
            assert_eq!(vector, other_vector);

            let mut locked = locked.cast_locked::<ResourceAccessorAddFile>();
            let fd0 = remote_binder_task
                .add_file_with_flags(
                    &mut locked,
                    &task,
                    new_null_file(&task, OpenFlags::RDWR),
                    FdFlags::empty(),
                )
                .expect("add_file_with_flags");
            assert_eq!(fd0.raw(), 0);
            let fd1 = remote_binder_task
                .add_file_with_flags(
                    &mut locked,
                    &task,
                    new_null_file(&task, OpenFlags::WRONLY),
                    FdFlags::empty(),
                )
                .expect("add_file_with_flags");
            assert_eq!(fd1.raw(), 1);
            let fd2 = remote_binder_task
                .add_file_with_flags(
                    &mut locked,
                    &task,
                    new_null_file(&task, OpenFlags::RDONLY),
                    FdFlags::empty(),
                )
                .expect("add_file_with_flags");
            assert_eq!(fd2.raw(), 2);

            assert_eq!(remote_binder_task.close_fd(fd1), Ok(()));
            let (handle, flags) =
                remote_binder_task.get_file_with_flags(&task, fd0).expect("get_file_with_flags");
            assert_eq!(flags, FdFlags::empty());
            assert_eq!(handle.flags(), OpenFlags::RDWR);

            assert_eq!(
                remote_binder_task
                    .get_file_with_flags(&task, FdNumber::from_raw(3))
                    .expect_err("bad fd"),
                errno!(EBADF)
            );

            std::mem::drop(remote_binder_task);
            let fds = process_accessor_thread.join().expect("join").expect("fds");
            assert_eq!(fds.len(), 1);
        });
    }
}
