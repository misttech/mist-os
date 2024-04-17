// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::DeviceMode,
    mm::PAGE_SIZE,
    selinux::hooks::current_task_hooks::{get_fs_node_security_id, post_setxattr},
    signals::{send_standard_signal, SignalInfo},
    task::{CurrentTask, Kernel, WaitQueue, Waiter},
    time::utc,
    vfs::{
        checked_add_offset_and_length,
        fsverity::FsVerityState,
        inotify,
        pipe::{Pipe, PipeHandle},
        rw_queue::RwQueue,
        socket::SocketHandle,
        FileObject, FileOps, FileSystem, FileSystemHandle, FileWriteGuard, FileWriteGuardMode,
        FileWriteGuardState, FsNodeReleaser, FsStr, FsString, MountInfo, NamespaceNode, OPathOps,
        RecordLockCommand, RecordLockOwner, RecordLocks, WeakFileHandle, MAX_LFS_FILESIZE,
    },
};
use bitflags::bitflags;
use fuchsia_zircon as zx;
use once_cell::sync::OnceCell;
use selinux::SecurityId;
use starnix_logging::{log_error, track_stub};
use starnix_sync::{
    DeviceOpen, FileOpsCore, FsNodeAllocate, LockBefore, LockEqualOrBefore, Locked, Mutex, RwLock,
    RwLockReadGuard,
};
use starnix_uapi::{
    as_any::AsAny,
    auth::{
        Credentials, FsCred, CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_FSETID, CAP_MKNOD,
        CAP_SYS_ADMIN, CAP_SYS_RESOURCE,
    },
    device_type::DeviceType,
    errno, error,
    errors::{Errno, EACCES},
    file_mode::{mode, Access, FileMode},
    fsverity_descriptor, gid_t, ino_t,
    open_flags::OpenFlags,
    ownership::Releasable,
    resource_limits::Resource,
    signals::SIGXFSZ,
    statx, statx_timestamp,
    time::{timespec_from_time, NANOS_PER_SECOND},
    timespec, uapi, uid_t, FALLOC_FL_COLLAPSE_RANGE, FALLOC_FL_INSERT_RANGE, FALLOC_FL_KEEP_SIZE,
    FALLOC_FL_PUNCH_HOLE, FALLOC_FL_UNSHARE_RANGE, FALLOC_FL_ZERO_RANGE, LOCK_EX, LOCK_NB, LOCK_SH,
    LOCK_UN, STATX_ATIME, STATX_ATTR_VERITY, STATX_BASIC_STATS, STATX_BLOCKS, STATX_CTIME,
    STATX_GID, STATX_INO, STATX_MTIME, STATX_NLINK, STATX_SIZE, STATX_UID, STATX__RESERVED,
    XATTR_TRUSTED_PREFIX, XATTR_USER_PREFIX,
};
use std::sync::{atomic::Ordering, Arc, Weak};
use syncio::zxio_node_attr_has_t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsNodeLinkBehavior {
    Allowed,
    Disallowed,
}

impl Default for FsNodeLinkBehavior {
    fn default() -> Self {
        FsNodeLinkBehavior::Allowed
    }
}

pub struct FsNode {
    /// Weak reference to the `FsNodeHandle` of this `FsNode`. This allows to retrieve the
    /// `FsNodeHandle` from a `FsNode`.
    pub weak_handle: WeakFsNodeHandle,

    /// The FsNodeOps for this FsNode.
    ///
    /// The FsNodeOps are implemented by the individual file systems to provide
    /// specific behaviors for this FsNode.
    ops: Box<dyn FsNodeOps>,

    /// The current kernel.
    // TODO(https://fxbug.dev/42080557): This is a temporary measure to access a task on drop.
    kernel: Weak<Kernel>,

    /// The FileSystem that owns this FsNode's tree.
    fs: Weak<FileSystem>,

    /// The node idenfier for this FsNode. By default, this will be used as the inode number of
    /// this node.
    pub node_id: ino_t,

    /// The pipe located at this node, if any.
    ///
    /// Used if, and only if, the node has a mode of FileMode::IFIFO.
    pub fifo: Option<PipeHandle>,

    /// The socket located at this node, if any.
    ///
    /// Used if, and only if, the node has a mode of FileMode::IFSOCK.
    ///
    /// The `OnceCell` is initialized when a new socket node is created:
    ///   - in `Socket::new` (e.g., from `sys_socket`)
    ///   - in `sys_bind`, before the node is given a name (i.e., before it could be accessed by
    ///     others)
    socket: OnceCell<SocketHandle>,

    /// A RwLock to synchronize append operations for this node.
    ///
    /// FileObjects writing with O_APPEND should grab a write() lock on this
    /// field to ensure they operate sequentially. FileObjects writing without
    /// O_APPEND should grab read() lock so that they can operate in parallel.
    pub append_lock: RwQueue,

    /// Mutable information about this node.
    ///
    /// This data is used to populate the uapi::stat structure.
    info: RwLock<FsNodeInfo>,

    /// Information about the locking information on this node.
    ///
    /// No other lock on this object may be taken while this lock is held.
    flock_info: Mutex<FlockInfo>,

    /// Records locks associated with this node.
    record_locks: RecordLocks,

    /// Whether this node can be linked into a directory.
    ///
    /// Only set for nodes created with `O_TMPFILE`.
    link_behavior: OnceCell<FsNodeLinkBehavior>,

    /// Tracks lock state for this file.
    pub write_guard_state: Mutex<FileWriteGuardState>,

    /// Cached Fsverity state associated with this node.
    pub fsverity: Mutex<FsVerityState>,

    /// Inotify watchers on this node. See inotify(7).
    pub watchers: inotify::InotifyWatchers,
}

pub type FsNodeHandle = Arc<FsNodeReleaser>;
pub type WeakFsNodeHandle = Weak<FsNodeReleaser>;

#[derive(Debug, Default, Clone)]
pub struct FsNodeSecurityId(Option<SecurityId>);

#[derive(Debug, Default, Clone)]
pub struct FsNodeInfo {
    pub ino: ino_t,
    pub mode: FileMode,
    pub link_count: usize,
    pub uid: uid_t,
    pub gid: gid_t,
    pub rdev: DeviceType,
    pub size: usize,
    pub blksize: usize,
    pub blocks: usize,
    pub time_status_change: zx::Time,
    pub time_access: zx::Time,
    pub time_modify: zx::Time,
    pub sid: FsNodeSecurityId,
}

impl FsNodeInfo {
    pub fn new(ino: ino_t, mode: FileMode, owner: FsCred) -> Self {
        let now = utc::utc_now();
        Self {
            ino,
            mode,
            link_count: if mode.is_dir() { 2 } else { 1 },
            uid: owner.uid,
            gid: owner.gid,
            blksize: DEFAULT_BYTES_PER_BLOCK,
            time_status_change: now,
            time_access: now,
            time_modify: now,
            ..Default::default()
        }
    }

    pub fn storage_size(&self) -> usize {
        self.blksize.saturating_mul(self.blocks)
    }

    pub fn new_factory(mode: FileMode, owner: FsCred) -> impl FnOnce(ino_t) -> Self {
        move |ino| Self::new(ino, mode, owner)
    }

    pub fn chmod(&mut self, mode: FileMode) {
        self.mode = (self.mode & !FileMode::PERMISSIONS) | (mode & FileMode::PERMISSIONS);
        self.time_status_change = utc::utc_now();
    }

    pub fn chown(&mut self, owner: Option<uid_t>, group: Option<gid_t>) {
        if let Some(owner) = owner {
            self.uid = owner;
        }
        if let Some(group) = group {
            self.gid = group;
        }
        // Clear the setuid and setgid bits if the file is executable and a regular file.
        if self.mode.is_reg() {
            if self.mode.intersects(FileMode::IXUSR | FileMode::IXGRP | FileMode::IXOTH) {
                self.mode &= !FileMode::ISUID;
            }
            self.clear_sgid_bit();
        }
        self.time_status_change = utc::utc_now();
    }

    fn clear_sgid_bit(&mut self) {
        // If the group execute bit is not set, the setgid bit actually indicates mandatory
        // locking and should not be cleared.
        if self.mode.intersects(FileMode::IXGRP) {
            self.mode &= !FileMode::ISGID;
        }
    }

    fn clear_suid_and_sgid_bits(&mut self) {
        self.mode &= !FileMode::ISUID;
        self.clear_sgid_bit();
    }

    pub fn cred(&self) -> FsCred {
        FsCred { uid: self.uid, gid: self.gid }
    }
}

#[derive(Default)]
struct FlockInfo {
    /// Whether the node is currently locked. The meaning of the different values are:
    /// - `None`: The node is not locked.
    /// - `Some(false)`: The node is locked non exclusively.
    /// - `Some(true)`: The node is locked exclusively.
    locked_exclusive: Option<bool>,
    /// The FileObject that hold the lock.
    locking_handles: Vec<WeakFileHandle>,
    /// The queue to notify process waiting on the lock.
    wait_queue: WaitQueue,
}

impl FlockInfo {
    /// Removes all file handle not holding `predicate` from the list of object holding the lock. If
    /// this empties the list, unlocks the node and notifies all waiting processes.
    pub fn retain<F>(&mut self, predicate: F)
    where
        F: Fn(&FileObject) -> bool,
    {
        if !self.locking_handles.is_empty() {
            self.locking_handles.retain(|w| {
                if let Some(fh) = w.upgrade() {
                    predicate(&fh)
                } else {
                    false
                }
            });
            if self.locking_handles.is_empty() {
                self.locked_exclusive = None;
                self.wait_queue.notify_all();
            }
        }
    }
}

/// `st_blksize` is measured in units of 512 bytes.
pub const DEFAULT_BYTES_PER_BLOCK: usize = 512;

pub struct FlockOperation {
    operation: u32,
}

impl FlockOperation {
    pub fn from_flags(operation: u32) -> Result<Self, Errno> {
        if operation & !(LOCK_SH | LOCK_EX | LOCK_UN | LOCK_NB) != 0 {
            return error!(EINVAL);
        }
        if [LOCK_SH, LOCK_EX, LOCK_UN].iter().filter(|&&o| operation & o == o).count() != 1 {
            return error!(EINVAL);
        }
        Ok(Self { operation })
    }

    pub fn is_unlock(&self) -> bool {
        self.operation & LOCK_UN > 0
    }

    pub fn is_lock_exclusive(&self) -> bool {
        self.operation & LOCK_EX > 0
    }

    pub fn is_blocking(&self) -> bool {
        self.operation & LOCK_NB == 0
    }
}

impl FileObject {
    /// Advisory locking.
    ///
    /// See flock(2).
    pub fn flock(
        &self,
        current_task: &CurrentTask,
        operation: FlockOperation,
    ) -> Result<(), Errno> {
        if self.flags().contains(OpenFlags::PATH) {
            return error!(EBADF);
        }
        loop {
            let mut flock_info = self.name.entry.node.flock_info.lock();
            if operation.is_unlock() {
                flock_info.retain(|fh| !std::ptr::eq(fh, self));
                return Ok(());
            }
            // Operation is a locking operation.
            // 1. File is not locked
            if flock_info.locked_exclusive.is_none() {
                flock_info.locked_exclusive = Some(operation.is_lock_exclusive());
                flock_info.locking_handles.push(self.weak_handle.clone());
                return Ok(());
            }

            let file_lock_is_exclusive = flock_info.locked_exclusive == Some(true);
            let fd_has_lock = flock_info
                .locking_handles
                .iter()
                .find_map(|w| {
                    w.upgrade().and_then(|fh| {
                        if std::ptr::eq(&fh as &FileObject, self) {
                            Some(())
                        } else {
                            None
                        }
                    })
                })
                .is_some();

            // 2. File is locked, but fd already have a lock
            if fd_has_lock {
                if operation.is_lock_exclusive() == file_lock_is_exclusive {
                    // Correct lock is already held, return.
                    return Ok(());
                } else {
                    // Incorrect lock is held. Release the lock and loop back to try to reacquire
                    // it. flock doesn't guarantee atomic lock type switching.
                    flock_info.retain(|fh| !std::ptr::eq(fh, self));
                    continue;
                }
            }

            // 3. File is locked, and fd doesn't have a lock.
            if !file_lock_is_exclusive && !operation.is_lock_exclusive() {
                // The lock is not exclusive, let's grab it.
                flock_info.locking_handles.push(self.weak_handle.clone());
                return Ok(());
            }

            // 4. The operation cannot be done at this time.
            if !operation.is_blocking() {
                return error!(EWOULDBLOCK);
            }

            // Register a waiter to be notified when the lock is released. Release the lock on
            // FlockInfo, and wait.
            let waiter = Waiter::new();
            flock_info.wait_queue.wait_async(&waiter);
            std::mem::drop(flock_info);
            waiter.wait(current_task)?;
        }
    }
}

// The inner mod is required because bitflags cannot pass the attribute through to the single
// variant, and attributes cannot be applied to macro invocations.
mod inner_flags {
    // Part of the code for the AT_STATX_SYNC_AS_STAT case that's produced by the macro triggers the
    // lint, but as a whole, the produced code is still correct.
    #![allow(clippy::bad_bit_mask)] // TODO(b/303500202) Remove once addressed in bitflags.
    use super::{bitflags, uapi};

    bitflags! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct StatxFlags: u32 {
            const AT_SYMLINK_NOFOLLOW = uapi::AT_SYMLINK_NOFOLLOW;
            const AT_EMPTY_PATH = uapi::AT_EMPTY_PATH;
            const AT_NO_AUTOMOUNT = uapi::AT_NO_AUTOMOUNT;
            const AT_STATX_SYNC_AS_STAT = uapi::AT_STATX_SYNC_AS_STAT;
            const AT_STATX_FORCE_SYNC = uapi::AT_STATX_FORCE_SYNC;
            const AT_STATX_DONT_SYNC = uapi::AT_STATX_DONT_SYNC;
            const STATX_ATTR_VERITY = uapi::STATX_ATTR_VERITY;
        }
    }
}

pub use inner_flags::StatxFlags;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnlinkKind {
    /// Unlink a directory.
    Directory,

    /// Unlink a non-directory.
    NonDirectory,
}

pub enum SymlinkTarget {
    Path(FsString),
    Node(NamespaceNode),
}

#[derive(PartialEq, Eq)]
pub enum XattrOp {
    /// Set the value of the extended attribute regardless of whether it exists.
    Set,
    /// Create a new extended attribute. Fail if it already exists.
    Create,
    /// Replace the value of the extended attribute. Fail if it doesn't exist.
    Replace,
}

impl XattrOp {
    pub fn into_flags(self) -> u32 {
        match self {
            Self::Set => 0,
            Self::Create => uapi::XATTR_CREATE,
            Self::Replace => uapi::XATTR_REPLACE,
        }
    }
}

/// Returns a value, or the size required to contains it.
#[derive(Clone, Debug)]
pub enum ValueOrSize<T> {
    Value(T),
    Size(usize),
}

impl<T> ValueOrSize<T> {
    pub fn map<F, U>(self, f: F) -> ValueOrSize<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Self::Size(s) => ValueOrSize::Size(s),
            Self::Value(v) => ValueOrSize::Value(f(v)),
        }
    }

    #[cfg(test)]
    pub fn unwrap(self) -> T {
        match self {
            Self::Size(_) => panic!("Unwrap ValueOrSize that is a Size"),
            Self::Value(v) => v,
        }
    }
}

impl<T> From<T> for ValueOrSize<T> {
    fn from(t: T) -> Self {
        Self::Value(t)
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum FallocMode {
    Allocate { keep_size: bool },
    PunchHole,
    Collapse,
    Zero { keep_size: bool },
    InsertRange,
    UnshareRange,
}

impl FallocMode {
    pub fn from_bits(mode: u32) -> Option<Self> {
        // `fallocate()` allows only the following values for `mode`.
        if mode == 0 {
            Some(Self::Allocate { keep_size: false })
        } else if mode == FALLOC_FL_KEEP_SIZE {
            Some(Self::Allocate { keep_size: true })
        } else if mode == FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE {
            Some(Self::PunchHole)
        } else if mode == FALLOC_FL_COLLAPSE_RANGE {
            Some(Self::Collapse)
        } else if mode == FALLOC_FL_ZERO_RANGE {
            Some(Self::Zero { keep_size: false })
        } else if mode == FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE {
            Some(Self::Zero { keep_size: true })
        } else if mode == FALLOC_FL_INSERT_RANGE {
            Some(Self::InsertRange)
        } else if mode == FALLOC_FL_UNSHARE_RANGE {
            Some(Self::UnshareRange)
        } else {
            None
        }
    }
}

pub trait FsNodeOps: Send + Sync + AsAny + 'static {
    /// Delegate the access check to the node.
    fn check_access(
        &self,
        node: &FsNode,
        current_task: &CurrentTask,
        access: Access,
        info: &RwLock<FsNodeInfo>,
    ) -> Result<(), Errno> {
        node.default_check_access_impl(current_task, access, info.read())
    }

    /// Build the `FileOps` for the file associated to this node.
    ///
    /// The returned FileOps will be used to create a FileObject, which might
    /// be assigned an FdNumber.
    fn create_file_ops(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>;

    /// Find an existing child node and populate the child parameter. Return the node.
    ///
    /// The child parameter is an empty node. Operations other than initialize may panic before
    /// initialize is called.
    fn lookup(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        // The default implementation here is suitable for filesystems that have permanent entries;
        // entries that already exist will get found in the cache and shouldn't get this far.
        error!(ENOENT, format!("looking for {name}"))
    }

    /// Create and return the given child node.
    ///
    /// The mode field of the FsNodeInfo indicates what kind of child to
    /// create.
    ///
    /// This function is never called with FileMode::IFDIR. The mkdir function
    /// is used to create directories instead.
    fn mknod(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno>;

    /// Create and return the given child node as a subdirectory.
    fn mkdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno>;

    /// Creates a symlink with the given `target` path.
    fn create_symlink(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno>;

    /// Creates an anonymous file.
    ///
    /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
    ///
    /// Used by O_TMPFILE.
    fn create_tmpfile(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EOPNOTSUPP)
    }

    /// Reads the symlink from this node.
    fn readlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        error!(EINVAL)
    }

    /// Create a hard link with the given name to the given child.
    fn link(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        error!(EPERM)
    }

    /// Remove the child with the given name, if the child exists.
    ///
    /// The UnlinkKind parameter indicates whether the caller intends to unlink
    /// a directory or a non-directory child.
    fn unlink(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno>;

    /// Change the length of the file.
    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    /// Manipulate allocated disk space for the file.
    fn allocate(
        &self,
        _locked: &mut Locked<'_, FsNodeAllocate>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _mode: FallocMode,
        _offset: u64,
        _length: u64,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    /// Update the supplied info with initial state (e.g. size) for the node.
    ///
    /// FsNode calls this method when created, to allow the FsNodeOps to
    /// set appropriate initial values in the FsNodeInfo.
    fn initial_info(&self, _info: &mut FsNodeInfo) {}

    /// Update node.info as needed.
    ///
    /// FsNode calls this method before converting the FsNodeInfo struct into
    /// the uapi::stat struct to give the file system a chance to update this data
    /// before it is used by clients.
    ///
    /// File systems that keep the FsNodeInfo up-to-date do not need to
    /// override this function.
    ///
    /// Return a read guard for the updated information.
    fn fetch_and_refresh_info<'a>(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        info: &'a RwLock<FsNodeInfo>,
    ) -> Result<RwLockReadGuard<'a, FsNodeInfo>, Errno> {
        Ok(info.read())
    }

    /// Indicates if the filesystem can manage the timestamps (i.e. atime, ctime, and mtime).
    ///
    /// Starnix updates the timestamps in node.info directly. However, if the filesystem can manage
    /// the timestamps, then Starnix does not need to do so. `node.info`` will be refreshed with the
    /// timestamps from the filesystem by calling `fetch_and_refresh_info(..)`.
    fn filesystem_manages_timestamps(&self, _node: &FsNode) -> bool {
        false
    }

    /// Update node attributes persistently.
    fn update_attributes(
        &self,
        _info: &FsNodeInfo,
        _has: zxio_node_attr_has_t,
    ) -> Result<(), Errno> {
        Ok(())
    }

    /// Get an extended attribute on the node.
    ///
    /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
    /// instead return the size of the attribute, and can return an ERANGE error if max_size is not
    /// 0, and lesser than the required size.
    fn get_xattr(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _max_size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        error!(ENOTSUP)
    }

    /// Set an extended attribute on the node.
    fn set_xattr(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _value: &FsStr,
        _op: XattrOp,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    fn remove_xattr(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
    ) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
    /// instead return the size of the 0 separated string needed to represent the value, and can
    /// return an ERANGE error if max_size is not 0, and lesser than the required size.
    fn list_xattrs(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _max_size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        error!(ENOTSUP)
    }

    /// Called when the FsNode is freed by the Kernel.
    fn forget(&self, _node: &FsNode, _current_task: &CurrentTask) -> Result<(), Errno> {
        Ok(())
    }

    ////////////////////
    // FS-Verity operations

    /// Marks that FS-Verity is being built. Writes fsverity descriptor and merkle tree, the latter
    /// computed by the filesystem.
    /// This should ensure there are no writable file handles. Returns EEXIST if the file was
    /// already fsverity-enabled. Returns EBUSY if this ioctl was already running on this file.
    fn enable_fsverity(&self, _descriptor: &fsverity_descriptor) -> Result<(), Errno> {
        error!(ENOTSUP)
    }

    /// Read fsverity descriptor, if the node is fsverity-enabled. Else returns ENODATA.
    fn get_fsverity_descriptor(&self, _log_blocksize: u8) -> Result<fsverity_descriptor, Errno> {
        error!(ENOTSUP)
    }
}

impl<T> From<T> for Box<dyn FsNodeOps>
where
    T: FsNodeOps,
{
    fn from(ops: T) -> Box<dyn FsNodeOps> {
        Box::new(ops)
    }
}

/// Implements [`FsNodeOps`] methods in a way that makes sense for symlinks.
/// You must implement [`FsNodeOps::readlink`].
macro_rules! fs_node_impl_symlink {
    () => {
        crate::vfs::fs_node_impl_not_dir!();

        fn create_file_ops(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &CurrentTask,
            _flags: starnix_uapi::open_flags::OpenFlags,
        ) -> Result<Box<dyn crate::vfs::FileOps>, starnix_uapi::errors::Errno> {
            unreachable!("Symlink nodes cannot be opened.");
        }
    };
}

macro_rules! fs_node_impl_dir_readonly {
    () => {
        fn mkdir(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS)
        }

        fn mknod(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _dev: starnix_uapi::device_type::DeviceType,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS)
        }

        fn create_symlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _target: &crate::vfs::FsStr,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS)
        }

        fn link(
            &self,
            _locked: &mut Locked<'_, FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _child: &crate::vfs::FsNodeHandle,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS)
        }

        fn unlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _child: &crate::vfs::FsNodeHandle,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS)
        }
    };
}

/// Implements [`FsNodeOps::set_xattr`] by delegating to another [`FsNodeOps`]
/// object.
macro_rules! fs_node_impl_xattr_delegate {
    ($self:ident, $delegate:expr) => {
        fn get_xattr(
            &$self,
            _node: &FsNode,
            _current_task: &CurrentTask,
            name: &crate::vfs::FsStr,
            _size: usize,
        ) -> Result<crate::vfs::ValueOrSize<crate::vfs::FsString>, starnix_uapi::errors::Errno> {
            Ok($delegate.get_xattr(name)?.into())
        }

        fn set_xattr(
            &$self,
            _node: &FsNode,
            _current_task: &CurrentTask,
            name: &crate::vfs::FsStr,
            value: &crate::vfs::FsStr,
            op: crate::vfs::XattrOp,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            $delegate.set_xattr(name, value, op)
        }

        fn remove_xattr(
            &$self,
            _node: &FsNode,
            _current_task: &CurrentTask,
            name: &crate::vfs::FsStr,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            $delegate.remove_xattr(name)
        }

        fn list_xattrs(
            &$self,
            _node: &FsNode,
            _current_task: &CurrentTask,
            _size: usize,
        ) -> Result<crate::vfs::ValueOrSize<Vec<crate::vfs::FsString>>, starnix_uapi::errors::Errno> {
            Ok($delegate.list_xattrs()?.into())
        }
    };
    ($delegate:expr) => { fs_node_impl_xattr_delegate(self, $delegate) };
}

/// Stubs out [`FsNodeOps`] methods that only apply to directories.
macro_rules! fs_node_impl_not_dir {
    () => {
        fn lookup(
            &self,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn mknod(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _dev: starnix_uapi::device_type::DeviceType,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn mkdir(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn create_symlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _target: &crate::vfs::FsStr,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<crate::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn unlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &crate::vfs::FsNode,
            _current_task: &crate::task::CurrentTask,
            _name: &crate::vfs::FsStr,
            _child: &crate::vfs::FsNodeHandle,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeUpdateType {
    Now,
    Omit,
    Time(zx::Time),
}

// Public re-export of macros allows them to be used like regular rust items.
pub(crate) use fs_node_impl_dir_readonly;
pub(crate) use fs_node_impl_not_dir;
pub(crate) use fs_node_impl_symlink;
pub(crate) use fs_node_impl_xattr_delegate;

pub struct SpecialNode;

impl FsNodeOps for SpecialNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        unreachable!("Special nodes cannot be opened.");
    }
}

impl FsNode {
    /// Create a new node with default value for the root of a filesystem.
    ///
    /// The node identifier and ino will be set by the filesystem on insertion. It will be owned by
    /// root and have a 777 permission.
    pub fn new_root(ops: impl FsNodeOps) -> Self {
        Self::new_root_with_properties(ops, |_| {})
    }

    /// Create a new node for the root of a filesystem.
    ///
    /// The provided callback allows the caller to set the properties of the node.
    /// The default value will provided a node owned by root, with permission 0777.
    /// The ino will be 0. If left as is, it will be set by the filesystem on insertion.
    pub fn new_root_with_properties<F>(ops: impl FsNodeOps, info_updater: F) -> Self
    where
        F: FnOnce(&mut FsNodeInfo),
    {
        let mut info = FsNodeInfo::new(0, mode!(IFDIR, 0o777), FsCred::root());
        info_updater(&mut info);
        Self::new_internal(Box::new(ops), Weak::new(), Weak::new(), 0, info, &Credentials::root())
    }

    /// Create a node without inserting it into the FileSystem node cache. This is usually not what
    /// you want! Only use if you're also using get_or_create_node, like ext4.
    pub fn new_uncached(
        current_task: &CurrentTask,
        ops: impl Into<Box<dyn FsNodeOps>>,
        fs: &FileSystemHandle,
        node_id: ino_t,
        info: FsNodeInfo,
    ) -> FsNodeHandle {
        let ops = ops.into();
        let creds = current_task.creds();
        Self::new_internal(ops, fs.kernel.clone(), Arc::downgrade(fs), node_id, info, &creds)
            .into_handle()
    }

    pub fn into_handle(mut self) -> FsNodeHandle {
        FsNodeHandle::new_cyclic(move |weak_handle| {
            self.weak_handle = weak_handle.clone();
            self.into()
        })
    }

    fn new_internal(
        ops: Box<dyn FsNodeOps>,
        kernel: Weak<Kernel>,
        fs: Weak<FileSystem>,
        node_id: ino_t,
        info: FsNodeInfo,
        credentials: &Credentials,
    ) -> Self {
        // Allow the FsNodeOps to populate initial info.
        let info = {
            let mut info = info;
            ops.initial_info(&mut info);
            info
        };

        let fifo = if info.mode.is_fifo() {
            let mut default_pipe_capacity = (*PAGE_SIZE * 16) as usize;
            if !credentials.has_capability(CAP_SYS_RESOURCE) {
                let kernel = kernel.upgrade().expect("Invalid kernel when creating fs node");
                let max_size = kernel.system_limits.pipe_max_size.load(Ordering::Relaxed);
                default_pipe_capacity = std::cmp::min(default_pipe_capacity, max_size);
            }

            Some(Pipe::new(default_pipe_capacity))
        } else {
            None
        };
        // The linter will fail in non test mode as it will not see the lock check.
        #[allow(clippy::let_and_return)]
        {
            let result = Self {
                weak_handle: Default::default(),
                kernel,
                ops,
                fs,
                node_id,
                fifo,
                socket: Default::default(),
                info: RwLock::new(info),
                append_lock: Default::default(),
                flock_info: Default::default(),
                record_locks: Default::default(),
                link_behavior: Default::default(),
                write_guard_state: Default::default(),
                fsverity: Mutex::new(FsVerityState::None),
                watchers: Default::default(),
            };
            #[cfg(any(test, debug_assertions))]
            {
                let _l1 = result.append_lock.read_for_lock_ordering();
                let _l2 = result.info.read();
                let _l3 = result.flock_info.lock();
                let _l4 = result.write_guard_state.lock();
                let _l5 = result.fsverity.lock();
            }
            result
        }
    }

    pub fn set_id(&mut self, node_id: ino_t) {
        debug_assert!(self.node_id == 0);
        self.node_id = node_id;
        if self.info.get_mut().ino == 0 {
            self.info.get_mut().ino = node_id;
        }
    }

    pub fn fs(&self) -> FileSystemHandle {
        self.fs.upgrade().expect("FileSystem did not live long enough")
    }

    pub fn set_fs(&mut self, fs: &FileSystemHandle) {
        debug_assert!(self.fs.ptr_eq(&Weak::new()));
        self.fs = Arc::downgrade(fs);
        self.kernel = fs.kernel.clone();
    }

    pub fn ops(&self) -> &dyn FsNodeOps {
        self.ops.as_ref()
    }

    /// Returns the `FsNode`'s `FsNodeOps` as a `&T`, or `None` if the downcast fails.
    pub fn downcast_ops<T>(&self) -> Option<&T>
    where
        T: 'static,
    {
        self.ops().as_any().downcast_ref::<T>()
    }

    pub fn on_file_closed(&self, file: &FileObject) {
        {
            let mut flock_info = self.flock_info.lock();
            // This function will drop the flock from `file` because the `WeakFileHandle` for
            // `file` will no longer upgrade to an `FileHandle`.
            flock_info.retain(|_| true);
        }
        self.record_lock_release(RecordLockOwner::FileObject(file.id));
    }

    pub fn record_lock(
        &self,
        current_task: &CurrentTask,
        file: &FileObject,
        cmd: RecordLockCommand,
        flock: uapi::flock,
    ) -> Result<Option<uapi::flock>, Errno> {
        self.record_locks.lock(current_task, file, cmd, flock)
    }

    /// Release all record locks acquired by the given owner.
    pub fn record_lock_release(&self, owner: RecordLockOwner) {
        self.record_locks.release_locks(owner);
    }

    pub fn create_file_ops<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().create_file_ops(&mut locked, self, current_task, flags)
    }

    pub fn open<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        flags: OpenFlags,
        check_access: bool,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<FileOpsCore>,
        L: LockBefore<DeviceOpen>,
    {
        // If O_PATH is set, there is no need to create a real FileOps because
        // most file operations are disabled.
        if flags.contains(OpenFlags::PATH) {
            return Ok(Box::new(OPathOps::new()));
        }

        if check_access {
            if flags.contains(OpenFlags::NOATIME) {
                self.check_o_noatime_allowed(current_task)?;
            }

            self.check_access(current_task, mount, Access::from_open_flags(flags))?;
        }

        let (mode, rdev) = {
            // Don't hold the info lock while calling into open_device or self.ops().
            // TODO: The mode and rdev are immutable and shouldn't require a lock to read.
            let info = self.info();
            (info.mode, info.rdev)
        };

        match mode & FileMode::IFMT {
            FileMode::IFCHR => current_task.kernel().open_device(
                locked,
                current_task,
                self,
                flags,
                rdev,
                DeviceMode::Char,
            ),
            FileMode::IFBLK => current_task.kernel().open_device(
                locked,
                current_task,
                self,
                flags,
                rdev,
                DeviceMode::Block,
            ),
            FileMode::IFIFO => Pipe::open(current_task, self.fifo.as_ref().unwrap(), flags),
            // UNIX domain sockets can't be opened.
            FileMode::IFSOCK => error!(ENXIO),
            _ => self.create_file_ops(locked, current_task, flags),
        }
    }

    pub fn lookup(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        self.check_access(current_task, mount, Access::EXEC)?;
        self.ops().lookup(self, current_task, name)
    }

    pub fn mknod<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        mut mode: FileMode,
        dev: DeviceType,
        mut owner: FsCred,
    ) -> Result<FsNodeHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        assert!(mode & FileMode::IFMT != FileMode::EMPTY, "mknod called without node type.");
        self.check_access(current_task, mount, Access::WRITE)?;
        self.update_metadata_for_child(current_task, &mut mode, &mut owner);

        if mode.is_dir() {
            let mut locked = locked.cast_locked::<FileOpsCore>();
            self.ops().mkdir(&mut locked, self, current_task, name, mode, owner)
        } else {
            // https://man7.org/linux/man-pages/man2/mknod.2.html says:
            //
            //   mode requested creation of something other than a regular
            //   file, FIFO (named pipe), or UNIX domain socket, and the
            //   caller is not privileged (Linux: does not have the
            //   CAP_MKNOD capability); also returned if the filesystem
            //   containing pathname does not support the type of node
            //   requested.
            let creds = current_task.creds();
            if !creds.has_capability(CAP_MKNOD) {
                if !matches!(mode.fmt(), FileMode::IFREG | FileMode::IFIFO | FileMode::IFSOCK) {
                    return error!(EPERM);
                }
            }
            let mut locked = locked.cast_locked::<FileOpsCore>();
            self.ops().mknod(&mut locked, self, current_task, name, mode, dev, owner)
        }
    }

    pub fn create_symlink<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        target: &FsStr,
        owner: FsCred,
    ) -> Result<FsNodeHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.check_access(current_task, mount, Access::WRITE)?;
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().create_symlink(&mut locked, self, current_task, name, target, owner)
    }

    pub fn create_tmpfile(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        mut mode: FileMode,
        mut owner: FsCred,
        link_behavior: FsNodeLinkBehavior,
    ) -> Result<FsNodeHandle, Errno> {
        self.check_access(current_task, mount, Access::WRITE)?;
        self.update_metadata_for_child(current_task, &mut mode, &mut owner);
        let node = self.ops().create_tmpfile(self, current_task, mode, owner)?;
        node.link_behavior.set(link_behavior).unwrap();
        Ok(node)
    }

    // This method does not attempt to update the atime of the node.
    // Use `NamespaceNode::readlink` which checks the mount flags and updates the atime accordingly.
    pub fn readlink(&self, current_task: &CurrentTask) -> Result<SymlinkTarget, Errno> {
        // TODO(qsr): Is there a permission check here?
        self.ops().readlink(self, current_task)
    }

    pub fn link<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<FsNodeHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.check_access(current_task, mount, Access::WRITE)?;

        if child.is_dir() {
            return error!(EPERM);
        }

        if matches!(child.link_behavior.get(), Some(FsNodeLinkBehavior::Disallowed)) {
            return error!(ENOENT);
        }

        // Check that `current_task` has permission to create the hard link.
        //
        // See description of /proc/sys/fs/protected_hardlinks in
        // https://man7.org/linux/man-pages/man5/proc.5.html for details of the security
        // vulnerabilities.
        //
        let creds = current_task.creds();
        let (child_uid, mode) = {
            let info = child.info();
            (info.uid, info.mode)
        };
        // Check that the the filesystem UID of the calling process (`current_task`) is the same as
        // the UID of the existing file. The check can be bypassed if the calling process has
        // `CAP_FOWNER` capability.
        if !creds.has_capability(CAP_FOWNER) && child_uid != creds.fsuid {
            // If current_task is not the user of the existing file, it needs to have read and write
            // access to the existing file.
            child.check_access(current_task, mount, Access::READ | Access::WRITE).map_err(|e| {
                // `check_access(..)` returns EACCES when the access rights doesn't match - change
                // it to EPERM to match Linux standards.
                if e == EACCES {
                    errno!(EPERM)
                } else {
                    e
                }
            })?;
            // There are also security issues that may arise when users link to setuid, setgid, or
            // special files.
            if mode.contains(FileMode::ISGID | FileMode::IXGRP) {
                return error!(EPERM);
            };
            if mode.contains(FileMode::ISUID) {
                return error!(EPERM);
            };
            if !mode.contains(FileMode::IFREG) {
                return error!(EPERM);
            };
        }

        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().link(&mut locked, self, current_task, name, child)?;
        Ok(child.clone())
    }

    pub fn unlink<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        child: &FsNodeHandle,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // The user must be able to search and write to the directory.
        self.check_access(current_task, mount, Access::EXEC | Access::WRITE)?;
        self.check_sticky_bit(current_task, child)?;
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().unlink(&mut locked, self, current_task, name, child)?;
        self.update_ctime_mtime();
        Ok(())
    }

    pub fn truncate<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        length: u64,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if self.is_dir() {
            return error!(EISDIR);
        }

        self.check_access(current_task, mount, Access::WRITE)?;

        self.truncate_common(locked, current_task, length)
    }

    /// Avoid calling this method directly. You probably want to call `FileObject::ftruncate()`
    /// which will also perform all file-descriptor based verifications.
    pub fn ftruncate<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        if self.is_dir() {
            // When truncating a file descriptor, if the descriptor references a directory,
            // return EINVAL. This is different from the truncate() syscall which returns EISDIR.
            //
            // See https://man7.org/linux/man-pages/man2/ftruncate.2.html#ERRORS
            return error!(EINVAL);
        }

        // For ftruncate, we do not need to check that the file node is writable.
        //
        // The file object that calls this method must verify that the file was opened
        // with write permissions.
        //
        // This matters because a file could be opened with O_CREAT + O_RDWR + 0444 mode.
        // The file descriptor returned from such an operation can be truncated, even
        // though the file was created with a read-only mode.
        //
        // See https://man7.org/linux/man-pages/man2/ftruncate.2.html#DESCRIPTION
        // which says:
        //
        // "With ftruncate(), the file must be open for writing; with truncate(),
        // the file must be writable."

        self.truncate_common(locked, current_task, length)
    }

    // Called by `truncate` and `ftruncate` above.
    fn truncate_common<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if length > MAX_LFS_FILESIZE as u64 {
            return error!(EINVAL);
        }
        if length > current_task.thread_group.get_rlimit(Resource::FSIZE) {
            send_standard_signal(current_task, SignalInfo::default(SIGXFSZ));
            return error!(EFBIG);
        }
        self.clear_suid_and_sgid_bits(current_task)?;
        // We have to take the append lock since otherwise it would be possible to truncate and for
        // an append to continue using the old size.
        // TODO(https://fxbug.dev/333540469): append lock must be before FileOpsCore, but this
        // method is also used in truncate
        let _guard = self.append_lock.read(current_task);

        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().truncate(&mut locked, self, current_task, length)?;
        self.update_ctime_mtime();
        Ok(())
    }

    /// Avoid calling this method directly. You probably want to call `FileObject::fallocate()`
    /// which will also perform additional verifications.
    pub fn fallocate<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FsNodeAllocate>,
    {
        let allocate_size = checked_add_offset_and_length(offset as usize, length as usize)
            .map_err(|_| errno!(EFBIG))? as u64;
        if allocate_size > current_task.thread_group.get_rlimit(Resource::FSIZE) {
            send_standard_signal(current_task, SignalInfo::default(SIGXFSZ));
            return error!(EFBIG);
        }

        self.clear_suid_and_sgid_bits(current_task)?;
        let _guard = self.append_lock.read(current_task);
        let mut locked = locked.cast_locked::<FsNodeAllocate>();
        self.ops().allocate(&mut locked, self, current_task, mode, offset, length)?;
        self.update_ctime_mtime();
        Ok(())
    }

    fn update_metadata_for_child(
        &self,
        current_task: &CurrentTask,
        mode: &mut FileMode,
        owner: &mut FsCred,
    ) {
        // The setgid bit on a directory causes the gid to be inherited by new children and the
        // setgid bit to be inherited by new child directories. See SetgidDirTest in gvisor.
        {
            let self_info = self.info();
            if self_info.mode.contains(FileMode::ISGID) {
                owner.gid = self_info.gid;
                if mode.is_dir() {
                    *mode |= FileMode::ISGID;
                }
            }
        }

        if !mode.is_dir() {
            // https://man7.org/linux/man-pages/man7/inode.7.html says:
            //
            //   For an executable file, the set-group-ID bit causes the
            //   effective group ID of a process that executes the file to change
            //   as described in execve(2).
            //
            // We need to check whether the current task has permission to create such a file.
            // See a similar check in `FsNode::chmod`.
            let creds = current_task.creds();
            if !creds.has_capability(CAP_FOWNER)
                && owner.gid != creds.fsgid
                && !creds.is_in_group(owner.gid)
            {
                *mode &= !FileMode::ISGID;
            }
        }
    }

    /// Checks if O_NOATIME is allowed,
    pub fn check_o_noatime_allowed(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        let creds = current_task.creds();

        // Per open(2),
        //
        //   O_NOATIME (since Linux 2.6.8)
        //      ...
        //
        //      This flag can be employed only if one of the following
        //      conditions is true:
        //
        //      *  The effective UID of the process matches the owner UID
        //         of the file.
        //
        //      *  The calling process has the CAP_FOWNER capability in
        //         its user namespace and the owner UID of the file has a
        //         mapping in the namespace.
        if creds.has_capability(CAP_FOWNER) || creds.fsuid == self.info().uid {
            Ok(())
        } else {
            error!(EPERM)
        }
    }

    pub fn default_check_access_impl(
        &self,
        current_task: &CurrentTask,
        access: Access,
        info: RwLockReadGuard<'_, FsNodeInfo>,
    ) -> Result<(), Errno> {
        let (node_uid, node_gid, mode) = (info.uid, info.gid, info.mode.bits());
        std::mem::drop(info);
        let creds = current_task.creds();

        let mode_flags = if creds.has_capability(CAP_DAC_OVERRIDE) {
            if self.is_dir() {
                0o7
            } else {
                // At least one of the EXEC bits must be set to execute files.
                0o6 | (mode & 0o100) >> 6 | (mode & 0o010) >> 3 | mode & 0o001
            }
        } else if creds.fsuid == node_uid {
            (mode & 0o700) >> 6
        } else if creds.is_in_group(node_gid) {
            (mode & 0o070) >> 3
        } else {
            mode & 0o007
        };
        if (mode_flags & access.rwx_bits()) != access.rwx_bits() {
            return error!(EACCES);
        }

        Ok(())
    }

    /// Check whether the node can be accessed in the current context with the specified access
    /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
    /// owner or is in the file's group.
    pub fn check_access(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        access: Access,
    ) -> Result<(), Errno> {
        if access.contains(Access::WRITE) {
            mount.check_readonly_filesystem()?;
        }

        self.ops.check_access(self, current_task, access, &self.info)
    }

    /// Check whether the stick bit, `S_ISVTX`, forbids the `current_task` from removing the given
    /// `child`. If this node has `S_ISVTX`, then either the child must be owned by the `fsuid` of
    /// `current_task` or `current_task` must have `CAP_FOWNER`.
    pub fn check_sticky_bit(
        &self,
        current_task: &CurrentTask,
        child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let creds = current_task.creds();
        if !creds.has_capability(CAP_FOWNER)
            && self.info().mode.contains(FileMode::ISVTX)
            && child.info().uid != creds.fsuid
        {
            return error!(EPERM);
        }
        Ok(())
    }

    /// Associates the provided socket with this file node.
    ///
    /// `set_socket` must be called before it is possible to look up `self`, since user space should
    ///  not be able to look up this node and find the socket missing.
    ///
    /// Note that it is a fatal error to call this method if a socket has already been bound for
    /// this node.
    ///
    /// # Parameters
    /// - `socket`: The socket to store in this file node.
    pub fn set_socket(&self, socket: SocketHandle) {
        assert!(self.socket.set(socket).is_ok());
    }

    /// Returns the socket associated with this node, if such a socket exists.
    pub fn socket(&self) -> Option<&SocketHandle> {
        self.socket.get()
    }

    fn update_attributes<F>(&self, mutator: F) -> Result<(), Errno>
    where
        F: FnOnce(&mut FsNodeInfo) -> Result<(), Errno>,
    {
        let mut info = self.info.write();
        let mut new_info = info.clone();
        mutator(&mut new_info)?;
        let mut has = zxio_node_attr_has_t { ..Default::default() };
        has.modification_time = info.time_modify != new_info.time_modify;
        has.access_time = info.time_access != new_info.time_access;
        has.mode = info.mode != new_info.mode;
        has.uid = info.uid != new_info.uid;
        has.gid = info.gid != new_info.gid;
        has.rdev = info.rdev != new_info.rdev;
        // Call `update_attributes(..)` to persist the changes for the following fields.
        if has.modification_time || has.access_time || has.mode || has.uid || has.gid || has.rdev {
            self.ops().update_attributes(&new_info, has)?;
        }
        *info = new_info;
        Ok(())
    }

    /// Set the permissions on this FsNode to the given values.
    ///
    /// Does not change the IFMT of the node.
    pub fn chmod(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        mut mode: FileMode,
    ) -> Result<(), Errno> {
        mount.check_readonly_filesystem()?;
        self.update_attributes(|info| {
            let creds = current_task.creds();
            if !creds.has_capability(CAP_FOWNER) {
                if info.uid != creds.euid {
                    return error!(EPERM);
                }
                if info.gid != creds.egid && !creds.is_in_group(info.gid) {
                    mode &= !FileMode::ISGID;
                }
            }
            info.chmod(mode);
            Ok(())
        })
    }

    /// Sets the owner and/or group on this FsNode.
    pub fn chown(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        owner: Option<uid_t>,
        group: Option<gid_t>,
    ) -> Result<(), Errno> {
        mount.check_readonly_filesystem()?;
        self.update_attributes(|info| {
            if !current_task.creds().has_capability(CAP_CHOWN) {
                let creds = current_task.creds();
                if info.uid != creds.euid {
                    return error!(EPERM);
                }
                if let Some(uid) = owner {
                    if info.uid != uid {
                        return error!(EPERM);
                    }
                }
                if let Some(gid) = group {
                    if !creds.is_in_group(gid) {
                        return error!(EPERM);
                    }
                }
            }
            info.chown(owner, group);
            Ok(())
        })
    }

    /// Whether this node is a regular file.
    pub fn is_reg(&self) -> bool {
        self.info().mode.is_reg()
    }

    /// Whether this node is a directory.
    pub fn is_dir(&self) -> bool {
        self.info().mode.is_dir()
    }

    /// Whether this node is a socket.
    pub fn is_sock(&self) -> bool {
        self.info().mode.is_sock()
    }

    /// Whether this node is a FIFO.
    pub fn is_fifo(&self) -> bool {
        self.info().mode.is_fifo()
    }

    /// Whether this node is a symbolic link.
    pub fn is_lnk(&self) -> bool {
        self.info().mode.is_lnk()
    }

    pub fn dev(&self) -> DeviceType {
        self.fs().dev_id
    }

    pub fn stat(&self, current_task: &CurrentTask) -> Result<uapi::stat, Errno> {
        let info = self.fetch_and_refresh_info(current_task)?;

        let time_to_kernel_timespec_pair = |t| {
            let timespec { tv_sec, tv_nsec } = timespec_from_time(t);
            let time = tv_sec.try_into().map_err(|_| errno!(EINVAL))?;
            let time_nsec = tv_nsec.try_into().map_err(|_| errno!(EINVAL))?;
            Ok((time, time_nsec))
        };

        let (st_atime, st_atime_nsec) = time_to_kernel_timespec_pair(info.time_access)?;
        let (st_mtime, st_mtime_nsec) = time_to_kernel_timespec_pair(info.time_modify)?;
        let (st_ctime, st_ctime_nsec) = time_to_kernel_timespec_pair(info.time_status_change)?;

        Ok(uapi::stat {
            st_dev: self.dev().bits(),
            st_ino: info.ino,
            st_nlink: info.link_count.try_into().map_err(|_| errno!(EINVAL))?,
            st_mode: info.mode.bits(),
            st_uid: info.uid,
            st_gid: info.gid,
            st_rdev: info.rdev.bits(),
            st_size: info.size.try_into().map_err(|_| errno!(EINVAL))?,
            st_blksize: info.blksize.try_into().map_err(|_| errno!(EINVAL))?,
            st_blocks: info.blocks.try_into().map_err(|_| errno!(EINVAL))?,
            st_atime,
            st_atime_nsec,
            st_mtime,
            st_mtime_nsec,
            st_ctime,
            st_ctime_nsec,
            ..Default::default()
        })
    }

    fn statx_timestamp_from_time(time: zx::Time) -> statx_timestamp {
        let nanos = time.into_nanos();
        statx_timestamp {
            tv_sec: nanos / NANOS_PER_SECOND,
            tv_nsec: (nanos % NANOS_PER_SECOND) as u32,
            ..Default::default()
        }
    }

    pub fn statx(
        &self,
        current_task: &CurrentTask,
        flags: StatxFlags,
        mask: u32,
    ) -> Result<statx, Errno> {
        // Ignore mask for now and fill in all of the fields.
        let info = if flags.contains(StatxFlags::AT_STATX_DONT_SYNC) {
            self.info()
        } else {
            self.fetch_and_refresh_info(current_task)?
        };
        if mask & STATX__RESERVED == STATX__RESERVED {
            return error!(EINVAL);
        }

        track_stub!(TODO("https://fxbug.dev/302594110"), "statx attributes");
        let stx_mnt_id = 0;
        let mut stx_attributes = 0;
        let stx_attributes_mask = STATX_ATTR_VERITY as u64;

        if matches!(*self.fsverity.lock(), FsVerityState::FsVerity) {
            stx_attributes |= STATX_ATTR_VERITY as u64;
        }

        Ok(statx {
            stx_mask: STATX_NLINK
                | STATX_UID
                | STATX_GID
                | STATX_ATIME
                | STATX_MTIME
                | STATX_CTIME
                | STATX_INO
                | STATX_SIZE
                | STATX_BLOCKS
                | STATX_BASIC_STATS,
            stx_blksize: info.blksize.try_into().map_err(|_| errno!(EINVAL))?,
            stx_attributes,
            stx_nlink: info.link_count.try_into().map_err(|_| errno!(EINVAL))?,
            stx_uid: info.uid,
            stx_gid: info.gid,
            stx_mode: info.mode.bits().try_into().map_err(|_| errno!(EINVAL))?,
            stx_ino: info.ino,
            stx_size: info.size.try_into().map_err(|_| errno!(EINVAL))?,
            stx_blocks: info.blocks.try_into().map_err(|_| errno!(EINVAL))?,
            stx_attributes_mask,
            stx_ctime: Self::statx_timestamp_from_time(info.time_status_change),
            stx_mtime: Self::statx_timestamp_from_time(info.time_modify),
            stx_atime: Self::statx_timestamp_from_time(info.time_access),

            stx_rdev_major: info.rdev.major(),
            stx_rdev_minor: info.rdev.minor(),

            stx_dev_major: self.fs().dev_id.major(),
            stx_dev_minor: self.fs().dev_id.minor(),
            stx_mnt_id,
            ..Default::default()
        })
    }

    /// Check that `current_task` can access the extended attributed `name`. Will return the result
    /// of `error` in case the attributed is trusted and the task has not the CAP_SYS_ADMIN
    /// capability.
    fn check_trusted_attribute_access(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        error: impl FnOnce() -> Errno,
    ) -> Result<(), Errno> {
        if name.starts_with(XATTR_TRUSTED_PREFIX.to_bytes())
            && !current_task.creds().has_capability(CAP_SYS_ADMIN)
        {
            return Err(error());
        }
        if name.starts_with(XATTR_USER_PREFIX.to_bytes()) {
            let info = self.info();
            if !info.mode.is_reg() && !info.mode.is_dir() {
                return Err(error());
            }
        }
        Ok(())
    }

    pub fn get_xattr(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        max_size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno> {
        self.check_access(current_task, mount, Access::READ)?;
        self.check_trusted_attribute_access(current_task, name, || errno!(ENODATA))?;
        self.ops().get_xattr(self, current_task, name, max_size)
    }

    pub fn set_xattr(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        value: &FsStr,
        op: XattrOp,
    ) -> Result<(), Errno> {
        self.check_access(current_task, mount, Access::WRITE)?;
        self.check_trusted_attribute_access(current_task, name, || errno!(EPERM))?;
        self.ops().set_xattr(self, current_task, name, value, op)?;
        post_setxattr(current_task, self, name, value);
        Ok(())
    }

    pub fn remove_xattr(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
    ) -> Result<(), Errno> {
        self.check_access(current_task, mount, Access::WRITE)?;
        self.check_trusted_attribute_access(current_task, name, || errno!(EPERM))?;
        self.ops().remove_xattr(self, current_task, name)
    }

    pub fn list_xattrs(
        &self,
        current_task: &CurrentTask,
        max_size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        Ok(self.ops().list_xattrs(self, current_task, max_size)?.map(|mut v| {
            v.retain(|name| {
                self.check_trusted_attribute_access(current_task, name.as_ref(), || errno!(EPERM))
                    .is_ok()
            });
            v
        }))
    }

    /// Returns current `FsNodeInfo`.
    pub fn info(&self) -> RwLockReadGuard<'_, FsNodeInfo> {
        self.info.read()
    }

    /// Refreshes the `FsNodeInfo` if necessary and returns a read guard.
    pub fn fetch_and_refresh_info(
        &self,
        current_task: &CurrentTask,
    ) -> Result<RwLockReadGuard<'_, FsNodeInfo>, Errno> {
        self.ops().fetch_and_refresh_info(self, current_task, &self.info)
    }

    pub fn update_info<F, T>(&self, mutator: F) -> T
    where
        F: FnOnce(&mut FsNodeInfo) -> T,
    {
        let mut info = self.info.write();
        mutator(&mut info)
    }

    /// Clear the SUID and SGID bits unless the `current_task` has `CAP_FSETID`
    pub fn clear_suid_and_sgid_bits(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        if !current_task.creds().has_capability(CAP_FSETID) {
            self.update_attributes(|info| {
                info.clear_suid_and_sgid_bits();
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Update the ctime and mtime of a file to now.
    pub fn update_ctime_mtime(&self) {
        if self.ops().filesystem_manages_timestamps(self) {
            return;
        }
        self.update_info(|info| {
            let now = utc::utc_now();
            info.time_status_change = now;
            info.time_modify = now;
        });
    }

    /// Update the ctime of a file to now.
    pub fn update_ctime(&self) {
        if self.ops().filesystem_manages_timestamps(self) {
            return;
        }
        self.update_info(|info| {
            let now = utc::utc_now();
            info.time_status_change = now;
        });
    }

    /// Update the atime and mtime if the `current_task` has write access, is the file owner, or
    /// holds either the CAP_DAC_OVERRIDE or CAP_FOWNER capability.
    pub fn update_atime_mtime(
        &self,
        current_task: &CurrentTask,
        mount: &MountInfo,
        atime: TimeUpdateType,
        mtime: TimeUpdateType,
    ) -> Result<(), Errno> {
        // If the filesystem is read-only, this always fail.
        mount.check_readonly_filesystem()?;

        // To set the timestamps to the current time the caller must either have write access to
        // the file, be the file owner, or hold the CAP_DAC_OVERRIDE or CAP_FOWNER capability.
        // To set the timestamps to other values the caller must either be the file owner or hold
        // the CAP_FOWNER capability.
        let creds = current_task.creds();
        let has_owner_priviledge =
            creds.fsuid == self.info().uid || creds.has_capability(CAP_FOWNER);
        let set_current_time = matches!((atime, mtime), (TimeUpdateType::Now, TimeUpdateType::Now));
        if !has_owner_priviledge {
            if set_current_time {
                self.check_access(current_task, mount, Access::WRITE)?
            } else {
                return error!(EPERM);
            }
        }

        if !matches!((atime, mtime), (TimeUpdateType::Omit, TimeUpdateType::Omit)) {
            // This function is called by `utimes(..)` which will update the access and
            // modification time. We need to call `update_attributes()` to update the mtime of
            // filesystems that manages file timestamps.
            self.update_attributes(|info| {
                let now = utc::utc_now();
                info.time_status_change = now;
                let get_time = |time: TimeUpdateType| match time {
                    TimeUpdateType::Now => Some(now),
                    TimeUpdateType::Time(t) => Some(t),
                    TimeUpdateType::Omit => None,
                };
                if let Some(time) = get_time(atime) {
                    info.time_access = time;
                }
                if let Some(time) = get_time(mtime) {
                    info.time_modify = time;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn create_write_guard(&self, mode: FileWriteGuardMode) -> Result<FileWriteGuard, Errno> {
        let handle = self.weak_handle.upgrade().ok_or_else(|| errno!(ENOENT))?;
        self.write_guard_state.lock().create_write_guard(handle, mode)
    }

    /// Sets the cached security id to `sid`. Storing the security id will cause the security id to
    /// *not* be recomputed in `FsNode::effective_sid`.
    pub(crate) fn set_cached_sid(&self, sid: SecurityId) {
        self.update_info(|info| info.sid = FsNodeSecurityId(Some(sid)));
    }

    /// Clears the cached security id. Clearing the security id will cause the security id to
    /// be be recomputed using an extended attribute value on the next `FsNode::effective_sid`.
    pub(crate) fn clear_cached_sid(&self) {
        self.update_info(|info| info.sid = FsNodeSecurityId(None));
    }

    /// Returns the security id currently stored in this node. This API should only be used by code
    /// that is responsible for controlling the cached security id; e.g., to check its current value
    /// before engaging logic that may compute a new value. Access control enforcement code should
    /// use `FsNode::effective_sid`, *not* this function.
    pub fn cached_sid(&self) -> Option<SecurityId> {
        self.info().sid.0
    }

    /// Returns the security id that should be used for SELinux access control checks at this time.
    /// If no security id is cached, it is recomputed via `get_fs_node_security_id()`. Access
    /// control enforcement code should use this function, *not* `FsNode::cached_sid`.
    pub fn effective_sid(&self, current_task: &CurrentTask) -> SecurityId {
        // Note: the sid is read before the match statement because otherwise the lock in
        // `self.info()` would be held for the duration of the match statement, leading to a
        // deadlock with `get_fs_node_security_id()`.
        let sid = self.info().sid.0;
        match sid {
            Some(sid) => sid,
            None => get_fs_node_security_id(current_task, self),
        }
    }
}

impl std::fmt::Debug for FsNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsNode")
            .field("fs", &self.fs().name())
            .field("node_id", &self.node_id)
            .field("info", &*self.info())
            .field("ops_ty", &self.ops().type_name())
            .finish()
    }
}

impl Releasable for FsNode {
    type Context<'a> = &'a CurrentTask;

    fn release(self, current_task: Self::Context<'_>) {
        if let Some(fs) = self.fs.upgrade() {
            fs.remove_node(&self);
        }
        if let Err(err) = self.ops.forget(&self, current_task) {
            log_error!("Error on FsNodeOps::forget: {err:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testing::*, vfs::buffers::VecOutputBuffer};
    use selinux::security_server::{Mode as SecurityServerMode, SecurityServer};
    use starnix_sync::Unlocked;

    const VALID_SECURITY_CONTEXT: &'static str = "u:object_r:test_valid_t:s0";

    fn create_test_file(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &AutoReleasableTask,
    ) -> NamespaceNode {
        current_task
            .fs()
            .root()
            .create_node(locked, &current_task, "file".into(), FileMode::IFREG, DeviceType::NONE)
            .expect("create_node(file)")
    }

    #[::fuchsia::test]
    async fn open_device_file() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        // Create a device file that points to the `zero` device (which is automatically
        // registered in the kernel).
        current_task
            .fs()
            .root()
            .create_node(
                &mut locked,
                &current_task,
                "zero".into(),
                mode!(IFCHR, 0o666),
                DeviceType::ZERO,
            )
            .expect("create_node");

        const CONTENT_LEN: usize = 10;
        let mut buffer = VecOutputBuffer::new(CONTENT_LEN);

        // Read from the zero device.
        let device_file = current_task
            .open_file(&mut locked, "zero".into(), OpenFlags::RDONLY)
            .expect("open device file");
        device_file.read(&mut locked, &current_task, &mut buffer).expect("read from zero");

        // Assert the contents.
        assert_eq!(&[0; CONTENT_LEN], buffer.data());
    }

    #[::fuchsia::test]
    async fn node_info_is_reflected_in_stat() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        // Create a node.
        let node = &current_task
            .fs()
            .root()
            .create_node(
                &mut locked,
                &current_task,
                "zero".into(),
                FileMode::IFCHR,
                DeviceType::ZERO,
            )
            .expect("create_node")
            .entry
            .node;
        node.update_info(|info| {
            info.mode = FileMode::IFSOCK;
            info.size = 1;
            info.blocks = 2;
            info.blksize = 4;
            info.uid = 9;
            info.gid = 10;
            info.link_count = 11;
            info.time_status_change = zx::Time::from_nanos(1);
            info.time_access = zx::Time::from_nanos(2);
            info.time_modify = zx::Time::from_nanos(3);
            info.rdev = DeviceType::new(13, 13);
        });
        let stat = node.stat(&current_task).expect("stat");

        assert_eq!(stat.st_mode, FileMode::IFSOCK.bits());
        assert_eq!(stat.st_size, 1);
        assert_eq!(stat.st_blksize, 4);
        assert_eq!(stat.st_blocks, 2);
        assert_eq!(stat.st_uid, 9);
        assert_eq!(stat.st_gid, 10);
        assert_eq!(stat.st_nlink, 11);
        assert_eq!(stat.st_ctime, 0);
        assert_eq!(stat.st_ctime_nsec, 1);
        assert_eq!(stat.st_atime, 0);
        assert_eq!(stat.st_atime_nsec, 2);
        assert_eq!(stat.st_mtime, 0);
        assert_eq!(stat.st_mtime_nsec, 3);
        assert_eq!(stat.st_rdev, DeviceType::new(13, 13).bits());
    }

    #[::fuchsia::test]
    fn test_flock_operation() {
        assert!(FlockOperation::from_flags(0).is_err());
        assert!(FlockOperation::from_flags(u32::MAX).is_err());

        let operation1 = FlockOperation::from_flags(LOCK_SH).expect("from_flags");
        assert!(!operation1.is_unlock());
        assert!(!operation1.is_lock_exclusive());
        assert!(operation1.is_blocking());

        let operation2 = FlockOperation::from_flags(LOCK_EX | LOCK_NB).expect("from_flags");
        assert!(!operation2.is_unlock());
        assert!(operation2.is_lock_exclusive());
        assert!(!operation2.is_blocking());

        let operation3 = FlockOperation::from_flags(LOCK_UN).expect("from_flags");
        assert!(operation3.is_unlock());
        assert!(!operation3.is_lock_exclusive());
        assert!(operation3.is_blocking());
    }

    #[::fuchsia::test]
    async fn test_check_access() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let mut creds = Credentials::with_ids(1, 2);
        creds.groups = vec![3, 4];
        current_task.set_creds(creds);

        // Create a node.
        let node = &current_task
            .fs()
            .root()
            .create_node(
                &mut locked,
                &current_task,
                "foo".into(),
                FileMode::IFREG,
                DeviceType::NONE,
            )
            .expect("create_node")
            .entry
            .node;
        let check_access = |uid: uid_t, gid: gid_t, perm: u32, access: Access| {
            node.update_info(|info| {
                info.mode = mode!(IFREG, perm);
                info.uid = uid;
                info.gid = gid;
            });
            node.check_access(&current_task, &MountInfo::detached(), access)
        };

        assert_eq!(check_access(0, 0, 0o700, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(0, 0, 0o700, Access::READ), error!(EACCES));
        assert_eq!(check_access(0, 0, 0o700, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(0, 0, 0o070, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(0, 0, 0o070, Access::READ), error!(EACCES));
        assert_eq!(check_access(0, 0, 0o070, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(0, 0, 0o007, Access::EXEC), Ok(()));
        assert_eq!(check_access(0, 0, 0o007, Access::READ), Ok(()));
        assert_eq!(check_access(0, 0, 0o007, Access::WRITE), Ok(()));

        assert_eq!(check_access(1, 0, 0o700, Access::EXEC), Ok(()));
        assert_eq!(check_access(1, 0, 0o700, Access::READ), Ok(()));
        assert_eq!(check_access(1, 0, 0o700, Access::WRITE), Ok(()));

        assert_eq!(check_access(1, 0, 0o100, Access::EXEC), Ok(()));
        assert_eq!(check_access(1, 0, 0o100, Access::READ), error!(EACCES));
        assert_eq!(check_access(1, 0, 0o100, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(1, 0, 0o200, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(1, 0, 0o200, Access::READ), error!(EACCES));
        assert_eq!(check_access(1, 0, 0o200, Access::WRITE), Ok(()));

        assert_eq!(check_access(1, 0, 0o400, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(1, 0, 0o400, Access::READ), Ok(()));
        assert_eq!(check_access(1, 0, 0o400, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(0, 2, 0o700, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(0, 2, 0o700, Access::READ), error!(EACCES));
        assert_eq!(check_access(0, 2, 0o700, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(0, 2, 0o070, Access::EXEC), Ok(()));
        assert_eq!(check_access(0, 2, 0o070, Access::READ), Ok(()));
        assert_eq!(check_access(0, 2, 0o070, Access::WRITE), Ok(()));

        assert_eq!(check_access(0, 3, 0o070, Access::EXEC), Ok(()));
        assert_eq!(check_access(0, 3, 0o070, Access::READ), Ok(()));
        assert_eq!(check_access(0, 3, 0o070, Access::WRITE), Ok(()));
    }

    #[::fuchsia::test]
    async fn setxattr_set_sid() {
        const BINARY_POLICY: &[u8] =
            include_bytes!("../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

        let security_server = SecurityServer::new(SecurityServerMode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(BINARY_POLICY.to_vec()).expect("policy load failed");
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid.0);
        assert_eq!(None, node.cached_sid());

        node.set_xattr(
            current_task.as_ref(),
            &current_task.fs().root().mount,
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("setxattr");

        let node_info_sid = node.info().sid.0.clone();
        let node_stored_sid = node.cached_sid();
        assert!(node_info_sid.is_some());
        assert_eq!(node_info_sid, node_stored_sid);
    }
}
