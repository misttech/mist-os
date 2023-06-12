// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, fs_args, CacheMode, DirentSink, FdEvents, FdNumber, FileObject,
        FileOps, FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode,
        FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString, SeekTarget, SymlinkTarget, XattrOp,
    },
    lock::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard},
    logging::{log_error, log_trace, not_implemented},
    syscalls::{SyscallArg, SyscallResult},
    task::{CurrentTask, EventHandler, Kernel, WaitCanceler, WaitQueue, Waiter},
    types::{
        errno,
        errno::{EINTR, EINVAL, ENOSYS},
        errno_from_code, error, off_t, statfs, time_from_timespec, uapi, DeviceType, Errno,
        FileMode, OpenFlags,
    },
};
use std::{
    collections::{btree_map::Entry, BTreeMap, VecDeque},
    sync::Arc,
};
use zerocopy::{AsBytes, FromBytes, FromZeroes};

#[derive(Debug, Default)]
pub struct DevFuse {
    connection: Arc<FuseConnection>,
}

impl FileOps for DevFuse {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        file.blocking_op(current_task, FdEvents::POLLIN, None, || self.connection.read(data))
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        self.connection.write(data)
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.connection.wait_async(waiter, events, handler)
    }

    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        self.connection.query_events()
    }
}

pub fn new_fuse_fs(
    task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    let mut mount_options = fs_args::generic_parse_mount_options(&options.params);
    let fd = fs_args::parse::<FdNumber>(
        mount_options.remove(b"fd" as &FsStr).ok_or_else(|| errno!(EINVAL))?,
    )?;
    let connection = task
        .files
        .get(fd)?
        .downcast_file::<DevFuse>()
        .ok_or_else(|| errno!(EINVAL))?
        .connection
        .clone();

    let fs =
        FileSystem::new(task.kernel(), CacheMode::Cached, FuseFs::new(connection.clone()), options);
    let fuse_node =
        Arc::new(FuseNode { connection: connection.clone(), nodeid: uapi::FUSE_ROOT_ID as u64 });
    let mut root_node = FsNode::new_root(fuse_node.clone());
    root_node.node_id = uapi::FUSE_ROOT_ID as u64;
    fs.set_root_node(root_node);
    connection.execute_operation(task, &fuse_node, FuseOperation::Init)?;
    Ok(fs)
}

#[derive(Debug)]
struct FuseFs {
    connection: Arc<FuseConnection>,
}

impl FuseFs {
    fn new(connection: Arc<FuseConnection>) -> Self {
        Self { connection }
    }
}

impl FileSystemOps for FuseFs {
    fn rename(
        &self,
        _fs: &FileSystem,
        _old_parent: &FsNodeHandle,
        _old_name: &FsStr,
        _new_parent: &FsNodeHandle,
        _new_name: &FsStr,
        _renamed: &FsNodeHandle,
        _replaced: Option<&FsNodeHandle>,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    fn generate_node_ids(&self) -> bool {
        true
    }

    fn statfs(&self, _fs: &FileSystem) -> Result<statfs, Errno> {
        error!(ENOTSUP)
    }
    fn name(&self) -> &'static FsStr {
        b"fuse"
    }
    fn unmount(&self) {
        self.connection.disconnect();
    }
}

#[derive(Debug)]
struct FuseNode {
    connection: Arc<FuseConnection>,
    nodeid: u64,
}

impl FuseNode {
    fn update_node_info(info: &mut FsNodeInfo, attributes: uapi::fuse_attr) -> Result<(), Errno> {
        info.ino = attributes.ino as uapi::ino_t;
        info.mode = FileMode::from_bits(attributes.mode);
        info.size = attributes.size.try_into().map_err(|_| errno!(EINVAL))?;
        info.blocks = attributes.blocks.try_into().map_err(|_| errno!(EINVAL))?;
        info.blksize = attributes.blksize.try_into().map_err(|_| errno!(EINVAL))?;
        info.uid = attributes.uid;
        info.gid = attributes.gid;
        info.link_count = attributes.nlink.try_into().map_err(|_| errno!(EINVAL))?;
        info.time_status_change = time_from_timespec(uapi::timespec {
            tv_sec: attributes.ctime as i64,
            tv_nsec: attributes.ctimensec as i64,
        })?;
        info.time_access = time_from_timespec(uapi::timespec {
            tv_sec: attributes.atime as i64,
            tv_nsec: attributes.atimensec as i64,
        })?;
        info.time_modify = time_from_timespec(uapi::timespec {
            tv_sec: attributes.mtime as i64,
            tv_nsec: attributes.mtimensec as i64,
        })?;
        info.rdev = DeviceType::from_bits(attributes.rdev as u64);
        Ok(())
    }

    /// Build a FsNodeHandle from a FuseResponse that is expected to be a FuseResponse::Entry.
    fn fs_node_from_entry(
        &self,
        node: &FsNode,
        response: FuseResponse,
    ) -> Result<FsNodeHandle, Errno> {
        let entry = if let FuseResponse::Entry(entry) = response {
            entry
        } else {
            return error!(EINVAL);
        };
        if entry.nodeid == 0 {
            return error!(ENOENT);
        }
        node.fs().get_or_create_node(Some(entry.nodeid), |id| {
            let fuse_node =
                Arc::new(FuseNode { connection: self.connection.clone(), nodeid: entry.nodeid });
            let mut info = FsNodeInfo::default();
            FuseNode::update_node_info(&mut info, entry.attr)?;
            Ok(FsNode::new_uncached(Box::new(fuse_node), &node.fs(), id, info))
        })
    }
}

struct FuseFileObject {
    connection: Arc<FuseConnection>,
    /// The response to the open calls from the userspace process.
    open_out: uapi::fuse_open_out,

    /// The current kernel. This is a temporary measure to access a task on close and flush.
    // TODO(https://fxbug.dev/128843): Remove this.
    kernel: Arc<Kernel>,
}

impl FuseFileObject {
    /// Returns the `FuseNode` associated with the opened file.
    fn get_fuse_node<'a>(&self, file: &'a FileObject) -> Result<&'a Arc<FuseNode>, Errno> {
        file.node().downcast_ops::<Arc<FuseNode>>().ok_or_else(|| errno!(ENOENT))
    }
}

impl FileOps for FuseFileObject {
    fn close(&self, file: &FileObject) {
        let node = if let Ok(node) = self.get_fuse_node(file) {
            node
        } else {
            log_error!("Unexpected file type");
            return;
        };
        // TODO(https://fxbug.dev/128843): This should receives a CurrentTask instead of relying on
        // the system task.
        if let Err(e) = self.connection.execute_operation(
            self.kernel.kthreads.system_task(),
            node,
            FuseOperation::Release(self.open_out),
        ) {
            log_error!("Error when relasing fh: {e:?}");
        }
    }

    fn flush(&self, file: &FileObject) {
        let node = if let Ok(node) = self.get_fuse_node(file) {
            node
        } else {
            log_error!("Unexpected file type");
            return;
        };
        // TODO(https://fxbug.dev/128843): This should receives a CurrentTask instead of relying on
        // the system task.
        if let Err(e) = self.connection.execute_operation(
            self.kernel.kthreads.system_task(),
            node,
            FuseOperation::Flush(self.open_out),
        ) {
            log_error!("Error when flushing fh: {e:?}");
        }
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let node = self.get_fuse_node(file)?;
        let response = self.connection.execute_operation(
            current_task,
            node,
            FuseOperation::Read(uapi::fuse_read_in {
                fh: self.open_out.fh,
                offset: offset.try_into().map_err(|_| errno!(EINVAL))?,
                size: data.available().try_into().unwrap_or(u32::MAX),
                read_flags: 0,
                lock_owner: 0,
                flags: 0,
                padding: 0,
            }),
        )?;
        let read_out = if let FuseResponse::Read(read_out) = response {
            read_out
        } else {
            return error!(EINVAL);
        };
        data.write(&read_out)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let node = self.get_fuse_node(file)?;
        let content = data.peek_all()?;
        let response = self.connection.execute_operation(
            current_task,
            node,
            FuseOperation::Write(
                uapi::fuse_write_in {
                    fh: self.open_out.fh,
                    offset: offset.try_into().map_err(|_| errno!(EINVAL))?,
                    size: content.len().try_into().map_err(|_| errno!(EINVAL))?,
                    write_flags: 0,
                    lock_owner: 0,
                    flags: 0,
                    padding: 0,
                },
                content,
            ),
        )?;
        let write_out = if let FuseResponse::Write(write_out) = response {
            write_out
        } else {
            return error!(EINVAL);
        };

        let written = write_out.size as usize;

        data.advance(written)?;
        Ok(written)
    }

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _current_offset: off_t,
        _whence: SeekTarget,
    ) -> Result<off_t, Errno> {
        not_implemented!("FileOps::seek");
        error!(ENOTSUP)
    }

    fn readdir(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        not_implemented!("FileOps::seek");
        error!(ENOTDIR)
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _request: u32,
        _arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        not_implemented!("FileOps::ioctl");
        error!(ENOTDIR)
    }

    fn fcntl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _cmd: u32,
        _arg: u64,
    ) -> Result<SyscallResult, Errno> {
        not_implemented!("FileOps::fcntl");
        error!(ENOTDIR)
    }
}

impl FsNodeOps for Arc<FuseNode> {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // The node already exists. The creation has been handled before calling this method.
        let flags = flags & !(OpenFlags::CREAT | OpenFlags::EXCL);
        let response =
            self.connection.execute_operation(current_task, self, FuseOperation::Open(flags))?;
        let open_out = if let FuseResponse::Open(open_out) = response {
            open_out
        } else {
            return error!(EINVAL);
        };
        Ok(Box::new(FuseFileObject {
            connection: self.connection.clone(),
            open_out,
            kernel: current_task.kernel().clone(),
        }))
    }

    fn lookup(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Lookup(name.to_owned()),
        )?;
        self.fs_node_from_entry(node, response)
    }

    fn mknod(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        let response = self.connection.execute_operation(
            current_task,
            self,
            FuseOperation::Mknod(
                uapi::fuse_mknod_in {
                    mode: mode.bits(),
                    rdev: dev.bits() as u32,
                    umask: current_task.fs().umask().bits(),
                    padding: 0,
                },
                name.to_owned(),
            ),
        )?;
        self.fs_node_from_entry(node, response)
    }

    fn mkdir(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        not_implemented!("FsNodeOps::mkdir");
        error!(ENOTSUP)
    }

    fn create_symlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        not_implemented!("FsNodeOps::create_symlink");
        error!(ENOTSUP)
    }

    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        not_implemented!("FsNodeOps::readlink");
        error!(ENOTSUP)
    }

    fn link(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::link");
        error!(ENOTSUP)
    }

    fn unlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::unlink");
        error!(ENOTSUP)
    }

    fn truncate(&self, _node: &FsNode, _length: u64) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::truncate");
        error!(ENOTSUP)
    }

    fn allocate(&self, _node: &FsNode, _offset: u64, _length: u64) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::allocate");
        error!(ENOTSUP)
    }

    fn update_info<'a>(
        &self,
        _node: &FsNode,
        current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        let response =
            self.connection.execute_operation(current_task, self, FuseOperation::GetAttr)?;
        let attr = if let FuseResponse::Attr(attr) = response {
            attr
        } else {
            return error!(EINVAL);
        };
        let mut info = info.write();
        FuseNode::update_node_info(&mut info, attr.attr)?;
        Ok(RwLockWriteGuard::downgrade(info))
    }

    fn get_xattr(&self, _node: &FsNode, _name: &FsStr) -> Result<FsString, Errno> {
        not_implemented!("FsNodeOps::get_xattr");
        error!(ENOTSUP)
    }

    fn set_xattr(
        &self,
        _node: &FsNode,
        _name: &FsStr,
        _value: &FsStr,
        _op: XattrOp,
    ) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::set_xattr");
        error!(ENOTSUP)
    }

    fn remove_xattr(&self, _node: &FsNode, _name: &FsStr) -> Result<(), Errno> {
        not_implemented!("FsNodeOps::remove_xattr");
        error!(ENOTSUP)
    }

    fn list_xattrs(&self, _node: &FsNode) -> Result<Vec<FsString>, Errno> {
        not_implemented!("FsNodeOps::list_xattrs");
        error!(ENOTSUP)
    }
}

#[derive(Debug, Default)]
struct FuseConnection {
    /// Mutable state of the connection.
    state: Mutex<FuseMutableState>,
}

impl FuseConnection {
    fn disconnect(&self) {
        self.state.lock().disconnect()
    }

    /// Execute the given operation on the `node`. If the operation is not asynchronous, this
    /// method will wait on the userspace process for a response. If the operation is interrupted,
    /// an interrupt will be sent to the userspace process and the operation will then block until
    /// the initial operation has a response. This block can only be interrupted by the filesystem
    /// being unmounted.
    fn execute_operation(
        &self,
        task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
    ) -> Result<FuseResponse, Errno> {
        let waiter = Waiter::new();
        let is_async = operation.is_async();
        let mut state = self.state.lock();
        if let Some(result) = operation.short_circuit(&state.operations_state) {
            return result;
        }
        let unique_id = state.queue_operation(task, node, operation, Some(&waiter))?;
        if is_async {
            return Ok(FuseResponse::None);
        }
        let mut first_loop = true;
        loop {
            if let Some(response) = state.get_response(unique_id) {
                return response;
            }
            match MutexGuard::unlocked(&mut state, || waiter.wait(task)) {
                Ok(()) => {}
                Err(e) if e == EINTR => {
                    // If interrupted by another process, send an interrupt command to the server
                    // the first time, then wait unconditionally.
                    if first_loop {
                        self.state.lock().interrupt(task, node, unique_id)?;
                        first_loop = false;
                    }
                }
                Err(e) => {
                    log_error!("Unexpected error: {e:?}");
                    return Err(e);
                }
            }
        }
    }

    fn wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.state.lock().waiters.wait_async_events(waiter, events, handler))
    }

    fn query_events(&self) -> FdEvents {
        self.state.lock().query_events()
    }

    fn read(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        self.state.lock().read(data)
    }

    fn write(&self, data: &mut dyn InputBuffer) -> Result<usize, Errno> {
        self.state.lock().write(data)
    }
}

/// A per connection state for operations that can be shortcircuited.
///
/// For a number of Fuse operation, Fuse protocol specifies that if they fail in a specific way,
/// they should not be sent to the server again and must be handled in a predefined way. This
/// structure keep track of these operations for a given connection.
#[derive(Debug, Default)]
struct OperationsState {
    /// Whether flush calls must be discarded and considered successful.
    discard_flush: bool,
}

#[derive(Debug, Default)]
struct FuseMutableState {
    /// Whether the mount has been disconnected.
    disconnected: bool,

    /// Last unique id used to identify messages between the kernel and user space.
    last_unique_id: u64,

    /// In progress operations.
    operations: BTreeMap<u64, RunningOperation>,

    /// Enqueued messages. These messages have not yet been sent to userspace. There should be
    /// multiple queues, but for now, push every messages to the same queue.
    /// New messages are added at the end of the queues. Read consume from the front of the queue.
    message_queue: VecDeque<FuseKernelMessage>,

    /// Queue to notify of new messages.
    waiters: WaitQueue,

    operations_state: OperationsState,
}

impl FuseMutableState {
    /// Disconnect the mount. Happens on unmount. Every filesystem operation will fail with EINTR,
    /// and every read/write on the /dev/fuse fd will fail with ENODEV.
    fn disconnect(&mut self) {
        if self.disconnected {
            return;
        }
        self.disconnected = true;
        self.message_queue.clear();
        for operation in &mut self.operations {
            operation.1.response = Some(error!(EINTR));
        }
        self.waiters.notify_all();
    }

    /// Queue the given operation on the internal queue for the userspace daemon to read. If
    /// `waiter` is not None, register `waiter` to be notified when userspace responds to the
    /// operation. This should only be used if the operation expects a response.
    fn queue_operation(
        &mut self,
        task: &CurrentTask,
        node: &FuseNode,
        operation: FuseOperation,
        waiter: Option<&Waiter>,
    ) -> Result<u64, Errno> {
        debug_assert!(waiter.is_some() == operation.has_response(), "{operation:?}");
        if self.disconnected {
            return error!(EINTR);
        }
        let operation = Arc::new(operation);
        self.last_unique_id += 1;
        let message = FuseKernelMessage::new(self.last_unique_id, task, node, operation);
        if let Some(waiter) = waiter {
            self.waiters.wait_async_value(waiter, self.last_unique_id);
        }
        if message.operation.has_response() {
            self.operations.insert(self.last_unique_id, message.operation.clone().into());
        }
        self.message_queue.push_back(message);
        self.waiters.notify_fd_events(FdEvents::POLLIN);
        Ok(self.last_unique_id)
    }

    /// Interrupt the operation with the given unique_id.
    ///
    /// If the operation is still enqueued, this will immediately dequeue the operation and return
    /// with an EINTR error.
    ///
    /// If not, it will send an interrupt operation.
    fn interrupt(
        &mut self,
        task: &CurrentTask,
        node: &FuseNode,
        unique_id: u64,
    ) -> Result<(), Errno> {
        debug_assert!(self.operations.contains_key(&unique_id));

        let mut in_queue = false;
        self.message_queue.retain(|m| {
            if m.header.unique == unique_id {
                self.operations.remove(&unique_id);
                in_queue = true;
                false
            } else {
                true
            }
        });
        if in_queue {
            // Nothing to do, the operation has been cancelled before being sent.
            return error!(EINTR);
        }
        self.queue_operation(task, node, FuseOperation::Interrupt(unique_id), None).map(|_| ())
    }

    /// Returns the response for the operation with the given identifier. Returns None if the
    /// operation is still in flight.
    fn get_response(&mut self, unique_id: u64) -> Option<Result<FuseResponse, Errno>> {
        match self.operations.entry(unique_id) {
            Entry::Vacant(_) => Some(error!(EINVAL)),
            Entry::Occupied(mut entry) => {
                let result = entry.get_mut().response.take();
                if result.is_some() {
                    entry.remove();
                }
                result
            }
        }
    }

    fn query_events(&self) -> FdEvents {
        let mut events = FdEvents::POLLOUT;
        if self.disconnected || !self.message_queue.is_empty() {
            events |= FdEvents::POLLIN
        };
        if self.disconnected {
            events |= FdEvents::POLLERR;
        }
        events
    }

    fn read(&mut self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        if self.disconnected {
            return error!(ENODEV);
        }
        if let Some(message) = self.message_queue.pop_front() {
            message.serialize(data)
        } else {
            error!(EAGAIN)
        }
    }

    fn write(&mut self, data: &mut dyn InputBuffer) -> Result<usize, Errno> {
        if self.disconnected {
            return error!(ENODEV);
        }
        let mut header = uapi::fuse_out_header::new_zeroed();
        if data.read(header.as_bytes_mut())? != std::mem::size_of::<uapi::fuse_out_header>() {
            return error!(EINVAL);
        }
        let remainder: usize = (header.len as usize) - std::mem::size_of::<uapi::fuse_out_header>();
        if data.available() < remainder {
            return error!(EINVAL);
        }
        self.waiters.notify_value_event(header.unique);
        let operation = self.operations.get_mut(&header.unique).ok_or_else(|| errno!(EINVAL))?;
        if header.error < 0 {
            log_trace!("Fuse: {operation:?} -> {header:?}");
            let code = i16::try_from(-header.error).unwrap_or(EINVAL.error_code() as i16);
            let errno = errno_from_code!(code);
            operation.response =
                Some(operation.operation.handle_error(&mut self.operations_state, errno));
        } else {
            let mut buffer = vec![0u8; remainder];
            if data.read(&mut buffer)? != remainder {
                return error!(EINVAL);
            }
            let response = operation.operation.parse_response(buffer)?;
            log_trace!("Fuse: {operation:?} -> {response:?}");
            if operation.operation.is_async() {
                operation.operation.handle_async(response)?;
            } else {
                operation.response = Some(Ok(response));
            }
        }
        if operation.operation.is_async() {
            self.operations.remove(&header.unique);
        }
        Ok(header.len as usize)
    }
}

/// An operation that is either queued to be send to userspace, or already sent to userspace and
/// waiting for a response.
#[derive(Debug)]
struct RunningOperation {
    operation: Arc<FuseOperation>,
    response: Option<Result<FuseResponse, Errno>>,
}

impl From<Arc<FuseOperation>> for RunningOperation {
    fn from(operation: Arc<FuseOperation>) -> Self {
        Self { operation, response: None }
    }
}

#[derive(Debug)]
struct FuseKernelMessage {
    header: uapi::fuse_in_header,
    operation: Arc<FuseOperation>,
}

impl FuseKernelMessage {
    fn new(
        unique: u64,
        task: &CurrentTask,
        node: &FuseNode,
        operation: Arc<FuseOperation>,
    ) -> Self {
        let creds = task.creds();
        Self {
            header: uapi::fuse_in_header {
                len: std::mem::size_of::<uapi::fuse_in_header>() as u32 + operation.len(),
                opcode: operation.opcode(),
                unique,
                nodeid: node.nodeid,
                uid: creds.uid,
                gid: creds.gid,
                pid: task.get_tid() as u32,
                padding: 0,
            },
            operation,
        }
    }

    fn serialize(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        let size = data.write(self.header.as_bytes())?;
        Ok(size + self.operation.serialize(data)?)
    }
}

bitflags::bitflags! {
    pub struct FuseInitFlags : u32 {
        const DONT_MASK = uapi::FUSE_DONT_MASK;
        const SPLICE_READ = uapi::FUSE_SPLICE_READ;
        const SPLICE_WRITE = uapi::FUSE_SPLICE_WRITE;
        const SPLICE_MOVE = uapi::FUSE_SPLICE_MOVE;
    }
}

#[derive(Debug)]
enum FuseOperation {
    Flush(uapi::fuse_open_out),
    GetAttr,
    Init,
    Interrupt(u64),
    Lookup(FsString),
    Mknod(uapi::fuse_mknod_in, FsString),
    Open(OpenFlags),
    Read(uapi::fuse_read_in),
    Release(uapi::fuse_open_out),
    Write(uapi::fuse_write_in, Vec<u8>),
}

impl FuseOperation {
    fn serialize(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        match self {
            Self::Flush(open_in) => {
                let message =
                    uapi::fuse_flush_in { fh: open_in.fh, unused: 0, padding: 0, lock_owner: 0 };
                data.write_all(message.as_bytes())
            }
            Self::GetAttr => Ok(0),
            Self::Init => {
                let message = uapi::fuse_init_in {
                    major: uapi::FUSE_KERNEL_VERSION,
                    minor: uapi::FUSE_KERNEL_MINOR_VERSION,
                    flags: FuseInitFlags::all().bits(),
                    ..Default::default()
                };
                data.write_all(message.as_bytes())
            }
            Self::Interrupt(unique) => {
                let message = uapi::fuse_interrupt_in { unique: *unique };
                data.write_all(message.as_bytes())
            }
            Self::Lookup(name) => data.write_all(name.as_bytes()),
            Self::Open(open_flags) => {
                let message = uapi::fuse_open_in { flags: open_flags.bits(), open_flags: 0 };
                data.write_all(message.as_bytes())
            }
            Self::Mknod(mknod_in, name) => {
                let mut len = data.write_all(mknod_in.as_bytes())?;
                len += data.write_all(name.as_bytes())?;
                Ok(len)
            }
            Self::Read(read_in) => data.write_all(read_in.as_bytes()),
            Self::Release(open_in) => {
                let message = uapi::fuse_release_in {
                    fh: open_in.fh,
                    flags: 0,
                    release_flags: 0,
                    lock_owner: 0,
                };
                data.write_all(message.as_bytes())
            }
            Self::Write(fuse_write_in, content) => {
                let mut len = data.write_all(fuse_write_in.as_bytes())?;
                len += data.write_all(content)?;
                Ok(len)
            }
        }
    }

    fn opcode(&self) -> u32 {
        match self {
            Self::Flush(_) => uapi::fuse_opcode_FUSE_FLUSH,
            Self::GetAttr => uapi::fuse_opcode_FUSE_GETATTR,
            Self::Init => uapi::fuse_opcode_FUSE_INIT,
            Self::Interrupt(_) => uapi::fuse_opcode_FUSE_INTERRUPT,
            Self::Lookup(_) => uapi::fuse_opcode_FUSE_LOOKUP,
            Self::Mknod(_, _) => uapi::fuse_opcode_FUSE_MKNOD,
            Self::Open(_) => uapi::fuse_opcode_FUSE_OPEN,
            Self::Read(_) => uapi::fuse_opcode_FUSE_READ,
            Self::Release(_) => uapi::fuse_opcode_FUSE_RELEASE,
            Self::Write(_, _) => uapi::fuse_opcode_FUSE_WRITE,
        }
    }

    fn len(&self) -> u32 {
        match self {
            Self::Flush(_) => std::mem::size_of::<uapi::fuse_flush_in>() as u32,
            Self::GetAttr => 0,
            Self::Init => std::mem::size_of::<uapi::fuse_init_in>() as u32,
            Self::Interrupt(_) => std::mem::size_of::<uapi::fuse_interrupt_in>() as u32,
            Self::Lookup(name) => name.as_bytes().len() as u32,
            Self::Mknod(_, name) => {
                (std::mem::size_of::<uapi::fuse_mknod_in>() + name.as_bytes().len()) as u32
            }
            Self::Open(_) => std::mem::size_of::<uapi::fuse_open_in>() as u32,
            Self::Read(_) => std::mem::size_of::<uapi::fuse_read_in>() as u32,
            Self::Release(_) => std::mem::size_of::<uapi::fuse_release_in>() as u32,
            Self::Write(_, content) => {
                (std::mem::size_of::<uapi::fuse_write_in>() + content.len()) as u32
            }
        }
    }

    fn has_response(&self) -> bool {
        !matches!(self, Self::Interrupt(_))
    }

    fn is_async(&self) -> bool {
        matches!(self, Self::Init)
    }

    fn to_response<T: FromBytes + AsBytes>(buffer: &[u8]) -> T {
        let mut result = T::new_zeroed();
        let length_to_copy = std::cmp::min(buffer.len(), std::mem::size_of::<T>());
        result.as_bytes_mut()[..length_to_copy].copy_from_slice(&buffer[..length_to_copy]);
        result
    }

    fn parse_response(&self, buffer: Vec<u8>) -> Result<FuseResponse, Errno> {
        debug_assert!(self.has_response());
        match self {
            Self::GetAttr => {
                Ok(FuseResponse::Attr(Self::to_response::<uapi::fuse_attr_out>(&buffer)))
            }
            Self::Init => Ok(FuseResponse::Init(Self::to_response::<uapi::fuse_init_out>(&buffer))),
            Self::Lookup(_) | Self::Mknod(_, _) => {
                Ok(FuseResponse::Entry(Self::to_response::<uapi::fuse_entry_out>(&buffer)))
            }
            Self::Open(_) => {
                Ok(FuseResponse::Open(Self::to_response::<uapi::fuse_open_out>(&buffer)))
            }
            Self::Read(_) => Ok(FuseResponse::Read(buffer)),
            Self::Release(_) | Self::Flush(_) => Ok(FuseResponse::None),
            Self::Write(_, _) => {
                Ok(FuseResponse::Write(Self::to_response::<uapi::fuse_write_out>(&buffer)))
            }
            Self::Interrupt(_) => {
                panic!("Response for operation without one");
            }
        }
    }

    fn handle_async(&self, response: FuseResponse) -> Result<(), Errno> {
        debug_assert!(self.is_async());
        if let Self::Init = self {
            debug_assert!(matches!(response, FuseResponse::Init(_)));
            // TODO(https://fxbug.dev/125496): save init response.
        }
        Ok(())
    }

    /// Short circuit calling the userspace daemon if required.
    ///
    /// Returns None if the userspace daemon needs to be called. Returns Some(response) otherwise.
    fn short_circuit(&self, state: &OperationsState) -> Option<Result<FuseResponse, Errno>> {
        match self {
            Self::Flush(_) if state.discard_flush => Some(Ok(FuseResponse::None)),
            _ => None,
        }
    }

    /// Handles an error from the userspace daemon.
    ///
    /// Given the `errno` returned by the userspace daemon, returns the response the caller should
    /// see. This can also update the `OperationState` to allow shortcircuit on future requests.
    fn handle_error(
        &self,
        state: &mut OperationsState,
        errno: Errno,
    ) -> Result<FuseResponse, Errno> {
        match self {
            Self::Flush(_) if errno == ENOSYS => {
                state.discard_flush = true;
                Ok(FuseResponse::None)
            }
            _ => Err(errno),
        }
    }
}

#[derive(Debug)]
enum FuseResponse {
    Attr(uapi::fuse_attr_out),
    Entry(uapi::fuse_entry_out),
    Init(uapi::fuse_init_out),
    Open(uapi::fuse_open_out),
    Read(Vec<u8>),
    Write(uapi::fuse_write_out),
    None,
}
