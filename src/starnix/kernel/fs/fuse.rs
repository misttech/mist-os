// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, fs_args, CacheMode, FdEvents, FdNumber, FileObject, FileOps,
        FileSystem, FileSystemHandle, FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle,
        FsNodeInfo, FsNodeOps, FsStr, FsString, SymlinkTarget, XattrOp,
    },
    lock::{Mutex, RwLock, RwLockReadGuard},
    logging::{log_error, not_implemented},
    task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter},
    types::{
        errno, errno::EINTR, errno_from_code, error, statfs, time_from_timespec, uapi, DeviceType,
        Errno, FileMode, OpenFlags,
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
}

impl FsNodeOps for Arc<FuseNode> {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        not_implemented!("FsNodeOps::create_file_ops");
        error!(ENOTSUP)
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
        let entry = if let FuseResponse::Entry(entry) = response {
            entry
        } else {
            return error!(EINVAL);
        };
        node.fs().get_or_create_node(Some(entry.nodeid), |id| {
            let fuse_node =
                Arc::new(FuseNode { connection: self.connection.clone(), nodeid: entry.nodeid });
            let mut info = FsNodeInfo::default();
            FuseNode::update_node_info(&mut info, entry.attr)?;
            Ok(FsNode::new_uncached(Box::new(fuse_node), &node.fs(), id, info))
        })
    }

    fn mknod(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        not_implemented!("FsNodeOps::mknod");
        error!(ENOTSUP)
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
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        Ok(info.read())
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
        let unique_id = self.state.lock().queue_operation(task, node, operation, Some(&waiter))?;
        if is_async {
            return Ok(FuseResponse::None);
        }
        let mut first_loop = true;
        loop {
            if let Some(response) = self.state.lock().get_response(unique_id) {
                return response;
            }
            match waiter.wait(task) {
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
        if header.error != 0 {
            operation.response = Some(Err(errno_from_code!(header
                .error
                .try_into()
                .map_err(|e: std::num::TryFromIntError| errno!(EINVAL, e))?)));
        } else {
            let mut buffer = vec![0u8; remainder];
            if data.read(&mut buffer)? != remainder {
                return error!(EINVAL);
            }
            let response = operation.operation.parse_response(&buffer)?;
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
    Init,
    Interrupt(u64),
    Lookup(FsString),
}

impl FuseOperation {
    fn serialize(&self, data: &mut dyn OutputBuffer) -> Result<usize, Errno> {
        match self {
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
            Self::Lookup(name) => {
                let len = data.write_all(name.as_bytes())?;
                Ok(data.write_all(&[0])? + len)
            }
        }
    }

    fn opcode(&self) -> u32 {
        match self {
            Self::Init => uapi::fuse_opcode_FUSE_INIT,
            Self::Interrupt(_) => uapi::fuse_opcode_FUSE_INTERRUPT,
            Self::Lookup(_) => uapi::fuse_opcode_FUSE_LOOKUP,
        }
    }

    fn len(&self) -> u32 {
        match self {
            Self::Init => std::mem::size_of::<uapi::fuse_init_in>() as u32,
            Self::Interrupt(_) => std::mem::size_of::<uapi::fuse_interrupt_in>() as u32,
            Self::Lookup(name) => (name.as_bytes().len() + 1) as u32,
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

    fn parse_response(&self, buffer: &[u8]) -> Result<FuseResponse, Errno> {
        debug_assert!(self.has_response());
        match self {
            Self::Init => Ok(FuseResponse::Init(Self::to_response::<uapi::fuse_init_out>(buffer))),
            Self::Lookup(_) => {
                Ok(FuseResponse::Entry(Self::to_response::<uapi::fuse_entry_out>(buffer)))
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
}

#[derive(Debug)]
enum FuseResponse {
    Init(uapi::fuse_init_out),
    Entry(uapi::fuse_entry_out),
    None,
}
