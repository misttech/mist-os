// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceMode;
use crate::mm::PAGE_SIZE;
use crate::security;
use crate::signals::{send_standard_signal, SignalInfo};
use crate::task::{CurrentTask, EncryptionKeyId, Kernel, WaitQueue, Waiter};
use crate::time::utc;
use crate::vfs::fsverity::FsVerityState;
use crate::vfs::pipe::{Pipe, PipeHandle};
use crate::vfs::rw_queue::{RwQueue, RwQueueReadGuard};
use crate::vfs::socket::SocketHandle;
use crate::vfs::{
    checked_add_offset_and_length, inotify, CurrentTaskAndLocked, DefaultDirEntryOps, DirEntryOps,
    FileObject, FileOps, FileSystem, FileSystemHandle, FileWriteGuard, FileWriteGuardMode,
    FileWriteGuardState, FsNodeReleaser, FsStr, FsString, MountInfo, NamespaceNode, OPathOps,
    RecordLockCommand, RecordLockOwner, RecordLocks, WeakFileHandle, MAX_LFS_FILESIZE,
};
use bitflags::bitflags;
use fuchsia_runtime::UtcInstant;
use linux_uapi::XATTR_SECURITY_PREFIX;
use starnix_logging::{log_error, track_stub};
use starnix_sync::{
    BeforeFsNodeAppend, FileOpsCore, FsNodeAppend, LockBefore, LockEqualOrBefore, Locked, Mutex,
    RwLock, RwLockReadGuard, Unlocked,
};
use starnix_types::ownership::Releasable;
use starnix_types::time::{timespec_from_time, NANOS_PER_SECOND};
use starnix_uapi::as_any::AsAny;
use starnix_uapi::auth::{
    Credentials, FsCred, UserAndOrGroupId, CAP_CHOWN, CAP_FOWNER, CAP_FSETID, CAP_MKNOD,
    CAP_SYS_ADMIN, CAP_SYS_RESOURCE,
};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::{Errno, EACCES, ENOTSUP};
use starnix_uapi::file_mode::{mode, Access, AccessCheck, FileMode};
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::signals::SIGXFSZ;
use starnix_uapi::{
    errno, error, fsverity_descriptor, gid_t, ino_t, statx, statx_timestamp, timespec, uapi, uid_t,
    FALLOC_FL_COLLAPSE_RANGE, FALLOC_FL_INSERT_RANGE, FALLOC_FL_KEEP_SIZE, FALLOC_FL_PUNCH_HOLE,
    FALLOC_FL_UNSHARE_RANGE, FALLOC_FL_ZERO_RANGE, LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, STATX_ATIME,
    STATX_ATTR_VERITY, STATX_BASIC_STATS, STATX_BLOCKS, STATX_CTIME, STATX_GID, STATX_INO,
    STATX_MTIME, STATX_NLINK, STATX_SIZE, STATX_UID, STATX__RESERVED, XATTR_USER_PREFIX,
};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock, Weak};
use syncio::{zxio_node_attr_has_t, zxio_node_attributes_t};

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

pub enum AppendLockGuard<'a> {
    Read(RwQueueReadGuard<'a, FsNodeAppend>),
    AlreadyLocked(&'a AppendLockGuard<'a>),
}

pub trait AppendLockStrategy<L> {
    /// Helper method for acquiring append lock in `truncate`/`allocate`. Acquires the lock when it's not already acquired.
    fn lock<'a>(
        &'a self,
        locked: &'a mut Locked<'a, L>,
        current_task: &CurrentTask,
        node: &'a FsNode,
    ) -> Result<(AppendLockGuard<'a>, Locked<'a, FileOpsCore>), Errno>;
}

struct RealAppendLockStrategy {}

impl AppendLockStrategy<BeforeFsNodeAppend> for RealAppendLockStrategy {
    fn lock<'a>(
        &'a self,
        locked: &'a mut Locked<'a, BeforeFsNodeAppend>,
        current_task: &CurrentTask,
        node: &'a FsNode,
    ) -> Result<(AppendLockGuard<'a>, Locked<'a, FileOpsCore>), Errno> {
        let (guard, new_locked) = node.ops().append_lock_read(locked, node, current_task)?;
        Ok((AppendLockGuard::Read(guard), Locked::cast_locked_by_value::<FileOpsCore>(new_locked)))
    }
}

pub struct AlreadyLockedAppendLockStrategy<'a> {
    // Keep the reference to the guard, which will be returned in subsequent attempts to acquire this lock.
    guard: &'a AppendLockGuard<'a>,
}

impl<'a> AlreadyLockedAppendLockStrategy<'a> {
    pub fn new(guard: &'a AppendLockGuard<'a>) -> Self {
        Self { guard }
    }
}

impl AppendLockStrategy<FileOpsCore> for AlreadyLockedAppendLockStrategy<'_> {
    fn lock<'a>(
        &'a self,
        locked: &'a mut Locked<'a, FileOpsCore>,
        _current_task: &CurrentTask,
        _node: &'a FsNode,
    ) -> Result<(AppendLockGuard<'a>, Locked<'a, FileOpsCore>), Errno> {
        Ok((AppendLockGuard::AlreadyLocked(self.guard), locked.cast_locked::<FileOpsCore>()))
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

    /// The UNIX domain socket bound to this node, if any.
    bound_socket: OnceLock<SocketHandle>,

    /// A RwLock to synchronize append operations for this node.
    ///
    /// FileObjects writing with O_APPEND should grab a write() lock on this
    /// field to ensure they operate sequentially. FileObjects writing without
    /// O_APPEND should grab read() lock so that they can operate in parallel.
    pub append_lock: RwQueue<FsNodeAppend>,

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
    link_behavior: OnceLock<FsNodeLinkBehavior>,

    /// Tracks lock state for this file.
    pub write_guard_state: Mutex<FileWriteGuardState>,

    /// Cached Fsverity state associated with this node.
    pub fsverity: Mutex<FsVerityState>,

    /// Inotify watchers on this node. See inotify(7).
    pub watchers: inotify::InotifyWatchers,

    /// The security state associated with this node. Must always be acquired last
    /// relative to other `FsNode` locks.
    pub security_state: security::FsNodeState,
}

pub type FsNodeHandle = Arc<FsNodeReleaser>;
pub type WeakFsNodeHandle = Weak<FsNodeReleaser>;

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
    pub time_status_change: UtcInstant,
    pub time_access: UtcInstant,
    pub time_modify: UtcInstant,
    pub casefold: bool,
    // If this node is fscrypt encrypted, stores the id of the user wrapping key used to encrypt
    // it.
    pub wrapping_key_id: Option<[u8; 16]>,
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
            self.mode &= !FileMode::ISUID;
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

    pub fn suid_and_sgid(&self, creds: &Credentials) -> Result<UserAndOrGroupId, Errno> {
        let uid = self.mode.contains(FileMode::ISUID).then_some(self.uid);

        // See <https://man7.org/linux/man-pages/man7/inode.7.html>:
        //
        //   For an executable file, the set-group-ID bit causes the
        //   effective group ID of a process that executes the file to change
        //   as described in execve(2).  For a file that does not have the
        //   group execution bit (S_IXGRP) set, the set-group-ID bit indicates
        //   mandatory file/record locking.
        let gid = self.mode.contains(FileMode::ISGID | FileMode::IXGRP).then_some(self.gid);

        let maybe_set_id = UserAndOrGroupId { uid, gid };
        if maybe_set_id.is_some() {
            // Check that uid and gid actually have execute access before
            // returning them as the SUID or SGID.
            creds.check_access(Access::EXEC, self.uid, self.gid, self.mode)?;
        }
        Ok(maybe_set_id)
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
        locked: &mut Locked<'_, Unlocked>,
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
                return error!(EAGAIN);
            }

            // Register a waiter to be notified when the lock is released. Release the lock on
            // FlockInfo, and wait.
            let waiter = Waiter::new();
            flock_info.wait_queue.wait_async(&waiter);
            std::mem::drop(flock_info);
            waiter.wait(locked, current_task)?;
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
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

#[derive(Debug)]
pub enum CheckAccessReason {
    Access,
    Chdir,
    Chroot,
    InternalPermissionChecks,
}

pub trait FsNodeOps: Send + Sync + AsAny + 'static {
    /// Delegate the access check to the node.
    fn check_access(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        access: Access,
        info: &RwLock<FsNodeInfo>,
        _reason: CheckAccessReason,
    ) -> Result<(), Errno> {
        node.default_check_access_impl(current_task, access, info.read())
    }

    /// Build the [`DirEntryOps`] for a new [`DirEntry`] that will be associated
    /// to this node.
    fn create_dir_entry_ops(&self) -> Box<dyn DirEntryOps> {
        Box::new(DefaultDirEntryOps)
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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

    /// Acquire the necessary append lock for the operations that depend on them.
    /// Should be done before calling `allocate` or `truncate` to avoid lock ordering issues.
    fn append_lock_read<'a>(
        &'a self,
        locked: &'a mut Locked<'a, BeforeFsNodeAppend>,
        node: &'a FsNode,
        current_task: &CurrentTask,
    ) -> Result<(RwQueueReadGuard<'a, FsNodeAppend>, Locked<'a, FsNodeAppend>), Errno> {
        return node.append_lock.read_and(locked, current_task);
    }

    /// Change the length of the file.
    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    /// Manipulate allocated disk space for the file.
    fn allocate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
        _current_task: &CurrentTask,
        _info: &FsNodeInfo,
        _has: zxio_node_attr_has_t,
    ) -> Result<(), Errno> {
        Ok(())
    }

    /// Get node attributes
    fn get_attr(&self, _has: zxio_node_attr_has_t) -> Result<zxio_node_attributes_t, Errno> {
        error!(ENOTSUP)
    }

    /// Get an extended attribute on the node.
    ///
    /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
    /// instead return the size of the attribute, and can return an ERANGE error if max_size is not
    /// 0, and lesser than the required size.
    fn get_xattr(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _max_size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno> {
        error!(ENOTSUP)
    }

    /// Called when the FsNode is freed by the Kernel.
    fn forget(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<(), Errno> {
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
#[macro_export]
macro_rules! fs_node_impl_symlink {
    () => {
        starnix_core::vfs::fs_node_impl_not_dir!();

        fn create_file_ops(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            node: &starnix_core::vfs::FsNode,
            _current_task: &CurrentTask,
            _flags: starnix_uapi::open_flags::OpenFlags,
        ) -> Result<Box<dyn starnix_core::vfs::FileOps>, starnix_uapi::errors::Errno> {
            assert!(node.is_lnk());
            unreachable!("Symlink nodes cannot be opened.");
        }
    };
}

#[macro_export]
macro_rules! fs_node_impl_dir_readonly {
    () => {
        fn mkdir(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            name: &starnix_core::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS, format!("mkdir failed: {:?}", name))
        }

        fn mknod(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            name: &starnix_core::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _dev: starnix_uapi::device_type::DeviceType,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS, format!("mknod failed: {:?}", name))
        }

        fn create_symlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            name: &starnix_core::vfs::FsStr,
            _target: &starnix_core::vfs::FsStr,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS, format!("symlink failed: {:?}", name))
        }

        fn link(
            &self,
            _locked: &mut Locked<'_, FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            name: &starnix_core::vfs::FsStr,
            _child: &starnix_core::vfs::FsNodeHandle,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS, format!("link failed: {:?}", name))
        }

        fn unlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            name: &starnix_core::vfs::FsStr,
            _child: &starnix_core::vfs::FsNodeHandle,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EROFS, format!("unlink failed: {:?}", name))
        }
    };
}

/// Trait that objects can implement if they need to handle extended attribute storage. Allows
/// delegating extended attribute operations in [`FsNodeOps`] to another object.
///
/// See [`fs_node_impl_xattr_delegate`] for usage details.
pub trait XattrStorage {
    /// Delegate for [`FsNodeOps::get_xattr`].
    fn get_xattr(&self, name: &FsStr) -> Result<FsString, Errno>;

    /// Delegate for [`FsNodeOps::set_xattr`].
    fn set_xattr(&self, name: &FsStr, value: &FsStr, op: XattrOp) -> Result<(), Errno>;

    /// Delegate for [`FsNodeOps::remove_xattr`].
    fn remove_xattr(&self, name: &FsStr) -> Result<(), Errno>;

    /// Delegate for [`FsNodeOps::list_xattrs`].
    fn list_xattrs(&self) -> Result<Vec<FsString>, Errno>;
}

/// Implements extended attribute ops for [`FsNodeOps`] by delegating to another object which
/// implements the [`XattrStorage`] trait or a similar interface. For example:
///
/// ```
/// struct Xattrs {}
///
/// impl XattrStorage for Xattrs {
///     // implement XattrStorage
/// }
///
/// struct Node {
///     xattrs: Xattrs
/// }
///
/// impl FsNodeOps for Node {
///     // Delegate extended attribute ops in FsNodeOps to self.xattrs
///     fs_node_impl_xattr_delegate!(self, self.xattrs);
///
///     // add other FsNodeOps impls here
/// }
/// ```
#[macro_export]
macro_rules! fs_node_impl_xattr_delegate {
    ($self:ident, $delegate:expr) => {
        fn get_xattr(
            &$self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &FsNode,
            _current_task: &CurrentTask,
            name: &starnix_core::vfs::FsStr,
            _size: usize,
        ) -> Result<starnix_core::vfs::ValueOrSize<starnix_core::vfs::FsString>, starnix_uapi::errors::Errno> {
            Ok($delegate.get_xattr(name)?.into())
        }

        fn set_xattr(
            &$self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &FsNode,
            _current_task: &CurrentTask,
            name: &starnix_core::vfs::FsStr,
            value: &starnix_core::vfs::FsStr,
            op: starnix_core::vfs::XattrOp,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            $delegate.set_xattr(name, value, op)
        }

        fn remove_xattr(
            &$self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &FsNode,
            _current_task: &CurrentTask,
            name: &starnix_core::vfs::FsStr,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            $delegate.remove_xattr(name)
        }

        fn list_xattrs(
            &$self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &FsNode,
            _current_task: &CurrentTask,
            _size: usize,
        ) -> Result<starnix_core::vfs::ValueOrSize<Vec<starnix_core::vfs::FsString>>, starnix_uapi::errors::Errno> {
            Ok($delegate.list_xattrs()?.into())
        }
    };
}

/// Stubs out [`FsNodeOps`] methods that only apply to directories.
#[macro_export]
macro_rules! fs_node_impl_not_dir {
    () => {
        fn lookup(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            _name: &starnix_core::vfs::FsStr,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn mknod(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            _name: &starnix_core::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _dev: starnix_uapi::device_type::DeviceType,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn mkdir(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            _name: &starnix_core::vfs::FsStr,
            _mode: starnix_uapi::file_mode::FileMode,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn create_symlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            _name: &starnix_core::vfs::FsStr,
            _target: &starnix_core::vfs::FsStr,
            _owner: starnix_uapi::auth::FsCred,
        ) -> Result<starnix_core::vfs::FsNodeHandle, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }

        fn unlink(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _node: &starnix_core::vfs::FsNode,
            _current_task: &starnix_core::task::CurrentTask,
            _name: &starnix_core::vfs::FsStr,
            _child: &starnix_core::vfs::FsNodeHandle,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ENOTDIR)
        }
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeUpdateType {
    Now,
    Omit,
    Time(UtcInstant),
}

// Public re-export of macros allows them to be used like regular rust items.
pub use {
    fs_node_impl_dir_readonly, fs_node_impl_not_dir, fs_node_impl_symlink,
    fs_node_impl_xattr_delegate,
};

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
                bound_socket: Default::default(),
                info: RwLock::new(info),
                append_lock: Default::default(),
                flock_info: Default::default(),
                record_locks: Default::default(),
                link_behavior: Default::default(),
                write_guard_state: Default::default(),
                fsverity: Mutex::new(FsVerityState::None),
                watchers: Default::default(),
                security_state: Default::default(),
            };
            #[cfg(any(test, debug_assertions))]
            {
                let mut locked = Unlocked::new();
                let _l1 = result.append_lock.read_for_lock_ordering(&mut locked);
                let _l2 = result.info.read();
                let _l3 = result.flock_info.lock();
                let _l4 = result.write_guard_state.lock();
                let _l5 = result.fsverity.lock();
                // TODO(https://fxbug.dev/367585803): Add lock levels to SELinux implementation.
                let _l6 = result.security_state.lock();
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

    /// Returns an error if this node is encrypted and locked.
    pub fn fail_if_locked<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let node_info = self.fetch_and_refresh_info(locked, current_task)?;
        if let Some(wrapping_key_id) = node_info.wrapping_key_id {
            let encryption_keys = current_task.kernel().encryption_keys.read();
            // Fail if the user tries to create a child in a locked encrypted directory.
            if !encryption_keys.contains_key(&EncryptionKeyId::from(wrapping_key_id)) {
                return error!(ENOKEY);
            }
        }
        Ok(())
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
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        file: &FileObject,
        cmd: RecordLockCommand,
        flock: uapi::flock,
    ) -> Result<Option<uapi::flock>, Errno> {
        self.record_locks.lock(locked, current_task, file, cmd, flock)
    }

    /// Release all record locks acquired by the given owner.
    pub fn record_lock_release(&self, owner: RecordLockOwner) {
        self.record_locks.release_locks(owner);
    }

    pub fn create_dir_entry_ops(&self) -> Box<dyn DirEntryOps> {
        self.ops().create_dir_entry_ops()
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

    pub fn open(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        flags: OpenFlags,
        access_check: AccessCheck,
    ) -> Result<Box<dyn FileOps>, Errno> {
        // If O_PATH is set, there is no need to create a real FileOps because
        // most file operations are disabled.
        if flags.contains(OpenFlags::PATH) {
            return Ok(Box::new(OPathOps::new()));
        }

        let access = access_check.resolve(flags);
        if access.is_nontrivial() {
            if flags.contains(OpenFlags::NOATIME) {
                self.check_o_noatime_allowed(current_task)?;
            }

            self.check_access(
                locked,
                current_task,
                mount,
                access,
                CheckAccessReason::InternalPermissionChecks,
            )?;
        }

        let (mode, rdev) = {
            // Don't hold the info lock while calling into open_device or self.ops().
            // TODO: The mode and rdev are immutable and shouldn't require a lock to read.
            let info = self.info();
            (info.mode, info.rdev)
        };

        match mode & FileMode::IFMT {
            FileMode::IFCHR => {
                if mount.flags().contains(MountFlags::NODEV) {
                    return error!(EACCES);
                }
                current_task.kernel().open_device(
                    locked,
                    current_task,
                    self,
                    flags,
                    rdev,
                    DeviceMode::Char,
                )
            }
            FileMode::IFBLK => {
                if mount.flags().contains(MountFlags::NODEV) {
                    return error!(EACCES);
                }
                current_task.kernel().open_device(
                    locked,
                    current_task,
                    self,
                    flags,
                    rdev,
                    DeviceMode::Block,
                )
            }
            FileMode::IFIFO => Pipe::open(locked, current_task, self.fifo.as_ref().unwrap(), flags),
            // UNIX domain sockets can't be opened.
            FileMode::IFSOCK => error!(ENXIO),
            _ => self.create_file_ops(locked, current_task, flags),
        }
    }

    pub fn lookup<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.check_access(
            locked,
            current_task,
            mount,
            Access::EXEC,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().lookup(&mut locked, self, current_task, name)
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
        self.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        if mode.is_reg() {
            security::check_fs_node_create_access(current_task, self, mode)?;
        } else if mode.is_dir() {
            // Even though the man page for mknod(2) says that mknod "cannot be used to create
            // directories" in starnix the mkdir syscall (`sys_mkdirat`) ends up calling mknod.
            security::check_fs_node_mkdir_access(current_task, self, mode)?;
        } else if !matches!(
            mode.fmt(),
            FileMode::IFCHR | FileMode::IFBLK | FileMode::IFIFO | FileMode::IFSOCK
        ) {
            security::check_fs_node_mknod_access(current_task, self, mode, dev)?;
        }

        self.update_metadata_for_child(current_task, &mut mode, &mut owner);

        let new_node = if mode.is_dir() {
            let mut locked = locked.cast_locked::<FileOpsCore>();
            self.ops().mkdir(&mut locked, self, current_task, name, mode, owner)?
        } else {
            // https://man7.org/linux/man-pages/man2/mknod.2.html says on error EPERM:
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
            self.ops().mknod(&mut locked, self, current_task, name, mode, dev, owner)?
        };

        self.init_new_node_security_on_create(locked, current_task, &new_node)?;

        Ok(new_node)
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
        self.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        security::check_fs_node_symlink_access(current_task, self, target)?;

        let mut locked = locked.cast_locked::<FileOpsCore>();
        let new_node =
            self.ops().create_symlink(&mut locked, self, current_task, name, target, owner)?;

        self.init_new_node_security_on_create(&mut locked, current_task, &new_node)?;

        Ok(new_node)
    }

    /// Requests that the LSM initialise a security label for the `new_node`, and optionally provide
    /// an extended attribute to write to the file to persist it.  If no LSM is enabled, no extended
    /// attribute returned, or if the filesystem does not support extended attributes, then the call
    /// returns success. All other failure modes return an `Errno` that should be early-returned.
    fn init_new_node_security_on_create<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        new_node: &FsNode,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        security::fs_node_init_on_create(current_task, &new_node, self)?
            .map(|xattr| {
                match new_node.ops().set_xattr(
                    &mut locked,
                    &new_node,
                    current_task,
                    xattr.name,
                    xattr.value.as_slice().into(),
                    XattrOp::Create,
                ) {
                    Err(e) => {
                        if e.code == ENOTSUP {
                            // This should only occur if a task has an "fscreate" context set, and
                            // creates a new file in a filesystem that does not support xattrs.
                            Ok(())
                        } else {
                            Err(e)
                        }
                    }
                    result => result,
                }
            })
            .unwrap_or_else(|| Ok(()))
    }

    pub fn create_tmpfile<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        mut mode: FileMode,
        mut owner: FsCred,
        link_behavior: FsNodeLinkBehavior,
    ) -> Result<FsNodeHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        self.update_metadata_for_child(current_task, &mut mode, &mut owner);
        let node = self.ops().create_tmpfile(self, current_task, mode, owner)?;
        security::fs_node_init_on_create(current_task, &node, self)?;
        node.link_behavior.set(link_behavior).unwrap();
        Ok(node)
    }

    // This method does not attempt to update the atime of the node.
    // Use `NamespaceNode::readlink` which checks the mount flags and updates the atime accordingly.
    pub fn readlink<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // TODO: 378864856 - Is there a permission check here other than security checks?
        security::check_fs_node_read_link_access(current_task, self)?;
        self.ops().readlink(&mut locked.cast_locked::<FileOpsCore>(), self, current_task)
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
        self.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;

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
            child
                .check_access(
                    locked,
                    current_task,
                    mount,
                    Access::READ | Access::WRITE,
                    CheckAccessReason::InternalPermissionChecks,
                )
                .map_err(|e| {
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

        security::check_fs_node_link_access(current_task, self, child)?;

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
        self.check_access(
            locked,
            current_task,
            mount,
            Access::EXEC | Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        self.check_sticky_bit(current_task, child)?;
        if child.is_dir() {
            security::check_fs_node_rmdir_access(current_task, self, child)?;
        } else {
            security::check_fs_node_unlink_access(current_task, self, child)?;
        }
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
        L: LockEqualOrBefore<BeforeFsNodeAppend>,
    {
        self.truncate_with_strategy(locked, RealAppendLockStrategy {}, current_task, mount, length)
    }

    pub fn truncate_with_strategy<L, M>(
        &self,
        locked: &mut Locked<'_, L>,
        strategy: impl AppendLockStrategy<M>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        length: u64,
    ) -> Result<(), Errno>
    where
        M: LockEqualOrBefore<FileOpsCore>,
        L: LockEqualOrBefore<M>,
    {
        if self.is_dir() {
            return error!(EISDIR);
        }

        {
            let mut locked = locked.cast_locked::<M>();
            self.check_access(
                &mut locked,
                current_task,
                mount,
                Access::WRITE,
                CheckAccessReason::InternalPermissionChecks,
            )?;
        }

        self.truncate_common(locked, strategy, current_task, length)
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
        L: LockEqualOrBefore<BeforeFsNodeAppend>,
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

        self.truncate_common(locked, RealAppendLockStrategy {}, current_task, length)
    }

    // Called by `truncate` and `ftruncate` above.
    fn truncate_common<L, M>(
        &self,
        locked: &mut Locked<'_, L>,
        strategy: impl AppendLockStrategy<M>,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno>
    where
        M: LockEqualOrBefore<FileOpsCore>,
        L: LockEqualOrBefore<M>,
    {
        if length > MAX_LFS_FILESIZE as u64 {
            return error!(EINVAL);
        }
        if length > current_task.thread_group.get_rlimit(Resource::FSIZE) {
            send_standard_signal(current_task, SignalInfo::default(SIGXFSZ));
            return error!(EFBIG);
        }
        let mut locked = locked.cast_locked::<M>();
        self.clear_suid_and_sgid_bits(&mut locked, current_task)?;
        // We have to take the append lock since otherwise it would be possible to truncate and for
        // an append to continue using the old size.
        let (guard, mut locked) = strategy.lock(&mut locked, current_task, self)?;
        self.ops().truncate(&mut locked, &guard, self, current_task, length)?;
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
        L: LockBefore<BeforeFsNodeAppend>,
    {
        self.fallocate_with_strategy(
            locked,
            RealAppendLockStrategy {},
            current_task,
            mode,
            offset,
            length,
        )
    }

    pub fn fallocate_with_strategy<L, M>(
        &self,
        locked: &mut Locked<'_, L>,
        strategy: impl AppendLockStrategy<M>,
        current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno>
    where
        M: LockEqualOrBefore<FileOpsCore>,
        L: LockEqualOrBefore<M>,
    {
        let allocate_size = checked_add_offset_and_length(offset as usize, length as usize)
            .map_err(|_| errno!(EFBIG))? as u64;
        if allocate_size > current_task.thread_group.get_rlimit(Resource::FSIZE) {
            send_standard_signal(current_task, SignalInfo::default(SIGXFSZ));
            return error!(EFBIG);
        }

        let mut locked = locked.cast_locked::<M>();
        self.clear_suid_and_sgid_bits(&mut locked, current_task)?;
        let (guard, mut locked) = strategy.lock(&mut locked, current_task, self)?;
        self.ops().allocate(&mut locked, &guard, self, current_task, mode, offset, length)?;
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
        let (node_uid, node_gid, mode) = (info.uid, info.gid, info.mode);
        std::mem::drop(info);
        current_task.creds().check_access(access, node_uid, node_gid, mode)
    }

    /// Check whether the node can be accessed in the current context with the specified access
    /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
    /// owner or is in the file's group.
    pub fn check_access<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        access: Access,
        reason: CheckAccessReason,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if access.contains(Access::WRITE) {
            mount.check_readonly_filesystem()?;
        }
        if access.contains(Access::EXEC) && !self.is_dir() {
            mount.check_noexec_filesystem()?;
        }
        self.ops.check_access(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            access,
            &self.info,
            reason,
        )
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

    /// Returns the UNIX domain socket bound to this node, if any.
    pub fn bound_socket(&self) -> Option<&SocketHandle> {
        self.bound_socket.get()
    }

    /// Register the provided socket as the UNIX domain socket bound to this node.
    ///
    /// It is a fatal error to call this method again if it has already been called on this node.
    pub fn set_bound_socket(&self, socket: SocketHandle) {
        assert!(self.bound_socket.set(socket).is_ok());
    }

    pub fn update_attributes<L, F>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mutator: F,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
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
        has.casefold = info.casefold != new_info.casefold;
        // Call `update_attributes(..)` to persist the changes for the following fields.
        if has.modification_time
            || has.access_time
            || has.mode
            || has.uid
            || has.gid
            || has.rdev
            || has.casefold
        {
            let mut locked = locked.cast_locked::<FileOpsCore>();
            self.ops().update_attributes(&mut locked, current_task, &new_info, has)?;
        }

        *info = new_info;
        Ok(())
    }

    /// Set the permissions on this FsNode to the given values.
    ///
    /// Does not change the IFMT of the node.
    pub fn chmod<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        mut mode: FileMode,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        mount.check_readonly_filesystem()?;
        self.update_attributes(locked, current_task, |info| {
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
    pub fn chown<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        owner: Option<uid_t>,
        group: Option<gid_t>,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        mount.check_readonly_filesystem()?;
        self.update_attributes(locked, current_task, |info| {
            if current_task.creds().has_capability(CAP_CHOWN) {
                info.chown(owner, group);
                return Ok(());
            }

            // Nobody can change the owner.
            if let Some(uid) = owner {
                if info.uid != uid {
                    return error!(EPERM);
                }
            }

            let creds = current_task.creds();

            // The owner can change the group.
            if info.uid == creds.euid {
                // To a group that it belongs.
                if let Some(gid) = group {
                    if !creds.is_in_group(gid) {
                        return error!(EPERM);
                    }
                }
                info.chown(None, group);
                return Ok(());
            }

            // Any other user can call chown(file, -1, -1)
            if owner.is_some() || group.is_some() {
                return error!(EPERM);
            }

            // But not on set-user-ID or set-group-ID files.
            // If we were to chown them, they would drop the set-ID bit.
            if info.mode.is_reg()
                && (info.mode.contains(FileMode::ISUID)
                    || info.mode.contains(FileMode::ISGID | FileMode::IXGRP))
            {
                return error!(EPERM);
            }

            info.chown(None, None);
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

    pub fn stat<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<uapi::stat, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let info = self.fetch_and_refresh_info(locked, current_task)?;

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

    fn statx_timestamp_from_time(time: UtcInstant) -> statx_timestamp {
        let nanos = time.into_nanos();
        statx_timestamp {
            tv_sec: nanos / NANOS_PER_SECOND,
            tv_nsec: (nanos % NANOS_PER_SECOND) as u32,
            ..Default::default()
        }
    }

    pub fn statx<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        flags: StatxFlags,
        mask: u32,
    ) -> Result<statx, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // Ignore mask for now and fill in all of the fields.
        let info = if flags.contains(StatxFlags::AT_STATX_DONT_SYNC) {
            self.info()
        } else {
            self.fetch_and_refresh_info(locked, current_task)?
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
    /// of `error` in case the attribute is not in the 'user' namespace and the task has not the
    /// CAP_SYS_ADMIN capability.
    fn check_trusted_attribute_access(
        &self,
        current_task: &CurrentTask,
        name: &FsStr,
        error: impl FnOnce() -> Errno,
    ) -> Result<(), Errno> {
        // Some irregular file types, most notably symlinks, tend to have very permissive write
        // settings since the type precludes normal data storage anyways. So only allow privileged
        // namespaces to be used on them.
        if name.starts_with(XATTR_USER_PREFIX.to_bytes()) {
            let info = self.info();
            if !info.mode.is_reg() && !info.mode.is_dir() {
                return Err(error());
            }
        } else if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
            // Non-privileged callers only have access to the user namespace.
            return Err(error());
        }
        Ok(())
    }

    pub fn get_xattr<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        max_size: usize,
    ) -> Result<ValueOrSize<FsString>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // Based on the man page for xattr(7), read access permissions to security attributes
        // depend on the security module. If there isn't any, read access is always allowed.
        if name.starts_with(XATTR_SECURITY_PREFIX.to_bytes()) {
            return security::fs_node_getsecurity(locked, current_task, self, name, max_size);
        }
        security::check_fs_node_getxattr_access(current_task, self, name)?;
        self.check_access(
            locked,
            current_task,
            mount,
            Access::READ,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        self.check_trusted_attribute_access(current_task, name, || errno!(ENODATA))?;
        self.ops().get_xattr(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            name,
            max_size,
        )
    }

    pub fn set_xattr<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
        value: &FsStr,
        op: XattrOp,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // Based on the man page for xattr(7), write access permissions to security attributes
        // depend on the security module. If there isn't any, write access is limited to
        // processed with the CAP_SYS_ADMIN capability.
        if name.starts_with(XATTR_SECURITY_PREFIX.to_bytes()) {
            return security::fs_node_setsecurity(locked, current_task, self, name, value, op);
        }
        security::check_fs_node_setxattr_access(current_task, self, name, value, op)?;
        self.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        self.check_trusted_attribute_access(current_task, name, || errno!(EPERM))?;
        self.ops().set_xattr(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            name,
            value,
            op,
        )
    }

    pub fn remove_xattr<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        name: &FsStr,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // TODO: Is removing security.* xattrs allowed at all?
        security::check_fs_node_removexattr_access(current_task, self, name)?;
        self.check_access(
            locked,
            current_task,
            mount,
            Access::WRITE,
            CheckAccessReason::InternalPermissionChecks,
        )?;
        self.check_trusted_attribute_access(current_task, name, || errno!(EPERM))?;
        self.ops().remove_xattr(&mut locked.cast_locked::<FileOpsCore>(), self, current_task, name)
    }

    pub fn list_xattrs<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        max_size: usize,
    ) -> Result<ValueOrSize<Vec<FsString>>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        security::check_fs_node_listxattr_access(current_task, self)?;
        Ok(self
            .ops()
            .list_xattrs(&mut locked.cast_locked::<FileOpsCore>(), self, current_task, max_size)?
            .map(|mut v| {
                v.retain(|name| {
                    self.check_trusted_attribute_access(current_task, name.as_ref(), || {
                        errno!(EPERM)
                    })
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
    pub fn fetch_and_refresh_info<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<RwLockReadGuard<'_, FsNodeInfo>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.ops().fetch_and_refresh_info(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            &self.info,
        )
    }

    pub fn update_info<F, T>(&self, mutator: F) -> T
    where
        F: FnOnce(&mut FsNodeInfo) -> T,
    {
        let mut info = self.info.write();
        mutator(&mut info)
    }

    /// Clear the SUID and SGID bits unless the `current_task` has `CAP_FSETID`
    pub fn clear_suid_and_sgid_bits<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if !current_task.creds().has_capability(CAP_FSETID) {
            self.update_attributes(locked, current_task, |info| {
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
    pub fn update_atime_mtime<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mount: &MountInfo,
        atime: TimeUpdateType,
        mtime: TimeUpdateType,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
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
                self.check_access(
                    locked,
                    current_task,
                    mount,
                    Access::WRITE,
                    CheckAccessReason::InternalPermissionChecks,
                )?
            } else {
                return error!(EPERM);
            }
        }

        if !matches!((atime, mtime), (TimeUpdateType::Omit, TimeUpdateType::Omit)) {
            // This function is called by `utimes(..)` which will update the access and
            // modification time. We need to call `update_attributes()` to update the mtime of
            // filesystems that manages file timestamps.
            self.update_attributes(locked, current_task, |info| {
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
    type Context<'a: 'b, 'b> = CurrentTaskAndLocked<'a, 'b>;

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        let (locked, current_task) = context;
        if let Some(fs) = self.fs.upgrade() {
            fs.remove_node(&self);
        }
        if let Err(err) =
            self.ops.forget(&mut locked.cast_locked::<FileOpsCore>(), &self, current_task)
        {
            log_error!("Error on FsNodeOps::forget: {err:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::mem::mem_device_init;
    use crate::testing::*;
    use crate::vfs::buffers::VecOutputBuffer;

    #[::fuchsia::test]
    async fn open_device_file() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        mem_device_init(&mut locked, &current_task);

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
            info.time_status_change = UtcInstant::from_nanos(1);
            info.time_access = UtcInstant::from_nanos(2);
            info.time_modify = UtcInstant::from_nanos(3);
            info.rdev = DeviceType::new(13, 13);
        });
        let stat = node.stat(&mut locked, &current_task).expect("stat");

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
        let check_access = |locked: &mut Locked<'_, Unlocked>,
                            uid: uid_t,
                            gid: gid_t,
                            perm: u32,
                            access: Access| {
            node.update_info(|info| {
                info.mode = mode!(IFREG, perm);
                info.uid = uid;
                info.gid = gid;
            });
            node.check_access(
                locked,
                &current_task,
                &MountInfo::detached(),
                access,
                CheckAccessReason::InternalPermissionChecks,
            )
        };

        assert_eq!(check_access(&mut locked, 0, 0, 0o700, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(&mut locked, 0, 0, 0o700, Access::READ), error!(EACCES));
        assert_eq!(check_access(&mut locked, 0, 0, 0o700, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(&mut locked, 0, 0, 0o070, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(&mut locked, 0, 0, 0o070, Access::READ), error!(EACCES));
        assert_eq!(check_access(&mut locked, 0, 0, 0o070, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(&mut locked, 0, 0, 0o007, Access::EXEC), Ok(()));
        assert_eq!(check_access(&mut locked, 0, 0, 0o007, Access::READ), Ok(()));
        assert_eq!(check_access(&mut locked, 0, 0, 0o007, Access::WRITE), Ok(()));

        assert_eq!(check_access(&mut locked, 1, 0, 0o700, Access::EXEC), Ok(()));
        assert_eq!(check_access(&mut locked, 1, 0, 0o700, Access::READ), Ok(()));
        assert_eq!(check_access(&mut locked, 1, 0, 0o700, Access::WRITE), Ok(()));

        assert_eq!(check_access(&mut locked, 1, 0, 0o100, Access::EXEC), Ok(()));
        assert_eq!(check_access(&mut locked, 1, 0, 0o100, Access::READ), error!(EACCES));
        assert_eq!(check_access(&mut locked, 1, 0, 0o100, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(&mut locked, 1, 0, 0o200, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(&mut locked, 1, 0, 0o200, Access::READ), error!(EACCES));
        assert_eq!(check_access(&mut locked, 1, 0, 0o200, Access::WRITE), Ok(()));

        assert_eq!(check_access(&mut locked, 1, 0, 0o400, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(&mut locked, 1, 0, 0o400, Access::READ), Ok(()));
        assert_eq!(check_access(&mut locked, 1, 0, 0o400, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(&mut locked, 0, 2, 0o700, Access::EXEC), error!(EACCES));
        assert_eq!(check_access(&mut locked, 0, 2, 0o700, Access::READ), error!(EACCES));
        assert_eq!(check_access(&mut locked, 0, 2, 0o700, Access::WRITE), error!(EACCES));

        assert_eq!(check_access(&mut locked, 0, 2, 0o070, Access::EXEC), Ok(()));
        assert_eq!(check_access(&mut locked, 0, 2, 0o070, Access::READ), Ok(()));
        assert_eq!(check_access(&mut locked, 0, 2, 0o070, Access::WRITE), Ok(()));

        assert_eq!(check_access(&mut locked, 0, 3, 0o070, Access::EXEC), Ok(()));
        assert_eq!(check_access(&mut locked, 0, 3, 0o070, Access::READ), Ok(()));
        assert_eq!(check_access(&mut locked, 0, 3, 0o070, Access::WRITE), Ok(()));
    }

    #[::fuchsia::test]
    async fn set_security_xattr_fails_without_security_module_or_root() {
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

        // Give read-write-execute access.
        node.update_info(|info| info.mode = mode!(IFREG, 0o777));

        // Without a security module, and without CAP_SYS_ADMIN capabilities, setting the xattr
        // should fail.
        assert_eq!(
            node.set_xattr(
                &mut locked,
                &current_task,
                &MountInfo::detached(),
                "security.name".into(),
                "security_label".into(),
                XattrOp::Create,
            ),
            error!(EPERM)
        );
    }

    #[::fuchsia::test]
    async fn set_non_user_xattr_fails_without_security_module_or_root() {
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

        // Give read-write-execute access.
        node.update_info(|info| info.mode = mode!(IFREG, 0o777));

        // Without a security module, and without CAP_SYS_ADMIN capabilities, setting the xattr
        // should fail.
        assert_eq!(
            node.set_xattr(
                &mut locked,
                &current_task,
                &MountInfo::detached(),
                "trusted.name".into(),
                "some data".into(),
                XattrOp::Create,
            ),
            error!(EPERM)
        );
    }

    #[::fuchsia::test]
    async fn get_security_xattr_succeeds_without_read_access() {
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

        // Only give read access to the root and give root access to the current task.
        node.update_info(|info| info.mode = mode!(IFREG, 0o100));
        current_task.set_creds(Credentials::root());

        // Setting the label should succeed even without write access to the file.
        assert_eq!(
            node.set_xattr(
                &mut locked,
                &current_task,
                &MountInfo::detached(),
                "security.name".into(),
                "security_label".into(),
                XattrOp::Create,
            ),
            Ok(())
        );

        // Remove root access from the current task.
        current_task.set_creds(Credentials::with_ids(1, 1));

        // Getting the label should succeed even without read access to the file.
        assert_eq!(
            node.get_xattr(
                &mut locked,
                &current_task,
                &MountInfo::detached(),
                "security.name".into(),
                4096
            ),
            Ok(ValueOrSize::Value("security_label".into()))
        );
    }
}
