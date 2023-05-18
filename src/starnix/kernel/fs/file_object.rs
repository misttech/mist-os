// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::HandleBased;
use fuchsia_zircon as zx;
use std::fmt;
use std::sync::Arc;

use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::file_server::serve_file;
use crate::fs::*;
use crate::lock::Mutex;
use crate::logging::{impossible_error, not_implemented};
use crate::mm::{
    DesiredAddress, MappedVmo, MappingName, MappingOptions, MemoryAccessorExt, ProtectionFlags,
};
use crate::syscalls::*;
use crate::task::*;
use crate::types::as_any::*;
use crate::types::*;

pub const MAX_LFS_FILESIZE: usize = 0x7fffffffffffffff;

pub enum SeekOrigin {
    Set,
    Cur,
    End,
}

impl SeekOrigin {
    pub fn from_raw(whence: u32) -> Result<SeekOrigin, Errno> {
        match whence {
            SEEK_SET => Ok(SeekOrigin::Set),
            SEEK_CUR => Ok(SeekOrigin::Cur),
            SEEK_END => Ok(SeekOrigin::End),
            _ => error!(EINVAL),
        }
    }
}

pub enum BlockableOpsResult<T> {
    Done(T),
    Partial(T),
}

impl<T> BlockableOpsResult<T> {
    pub fn value(self) -> T {
        match self {
            Self::Done(v) | Self::Partial(v) => v,
        }
    }

    pub fn is_partial(&self) -> bool {
        match self {
            Self::Done(_) => false,
            Self::Partial(_) => true,
        }
    }
}

/// This function adds `POLLRDNORM` and `POLLWRNORM` to the FdEvents
/// return from the FileOps because these FdEvents are equivalent to
/// `POLLIN` and `POLLOUT`, respectively, in the Linux UAPI.
///
/// See https://linux.die.net/man/2/poll
fn add_equivalent_fd_events(mut events: FdEvents) -> FdEvents {
    if events.contains(FdEvents::POLLIN) {
        events |= FdEvents::POLLRDNORM;
    }
    if events.contains(FdEvents::POLLOUT) {
        events |= FdEvents::POLLWRNORM;
    }
    events
}

/// Corresponds to struct file_operations in Linux, plus any filesystem-specific data.
pub trait FileOps: Send + Sync + AsAny + 'static {
    /// Called when the FileObject is closed.
    fn close(&self, _file: &FileObject) {}

    /// Called every time close() is called on this file, even if the file is not ready to be
    /// released.
    fn flush(&self, _file: &FileObject) {}

    /// Read from the file without an offset. If your file is seekable, consider implementing this
    /// with [`fileops_impl_seekable`].
    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>;
    /// Read from the file at an offset. If your file is seekable, consider implementing this with
    /// [`fileops_impl_nonseekable`].
    fn read_at(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>;
    /// Write to the file without an offset. If your file is seekable, consider implementing this
    /// with [`fileops_impl_seekable`].
    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>;
    /// Write to the file at a offset. If your file is nonseekable, consider implementing this with
    /// [`fileops_impl_nonseekable`].
    fn write_at(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>;

    /// Adjust the seek offset if the file is seekable.
    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: off_t,
        whence: SeekOrigin,
    ) -> Result<off_t, Errno>;

    /// Returns a VMO representing this file. At least the requested protection flags must
    /// be set on the VMO. Reading or writing the VMO must read or write the file. If this is not
    /// possible given the requested protection, an error must be returned.
    /// The `length` is a hint for the desired size of the VMO. The returned VMO may be larger or
    /// smaller than the requested length.
    /// This method is typically called by [`Self::mmap`].
    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        error!(ENODEV)
    }

    /// Responds to an mmap call. The default implementation calls [`Self::get_vmo`] to get a VMO
    /// and then maps it with [`crate::mm::MemoryManager::map`].
    /// Only implement this trait method if your file needs to control mapping, or record where
    /// a VMO gets mapped.
    fn mmap(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        vmar_flags: zx::VmarFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<MappedVmo, Errno> {
        let vmo = if options.contains(MappingOptions::SHARED) {
            self.get_vmo(file, current_task, Some(length), prot_flags)?
        } else {
            // TODO(tbodt): Use PRIVATE_CLONE to have the filesystem server do the clone for us.
            let vmo = self.get_vmo(
                file,
                current_task,
                Some(length),
                prot_flags - ProtectionFlags::WRITE,
            )?;
            let mut clone_flags = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
            if !prot_flags.contains(ProtectionFlags::WRITE) {
                clone_flags |= zx::VmoChildOptions::NO_WRITE;
            }
            Arc::new(
                vmo.create_child(clone_flags, 0, vmo.get_size().map_err(impossible_error)?)
                    .map_err(impossible_error)?,
            )
        };
        let addr = current_task.mm.map(
            addr,
            vmo.clone(),
            vmo_offset,
            length,
            prot_flags,
            vmar_flags,
            options,
            MappingName::File(filename),
        )?;
        Ok(MappedVmo::new(vmo, addr))
    }

    /// Respond to a `getdents` or `getdents64` calls.
    ///
    /// The `file.offset` lock will be held while entering this method. The implementation must look
    /// at `sink.offset()` to read the current offset into the file.
    fn readdir(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        error!(ENOTDIR)
    }

    /// Establish a one-shot, edge-triggered, asynchronous wait for the given FdEvents for the
    /// given file and task. Returns `None` if this file does not support blocking waits.
    ///
    /// Active events are not considered. This is similar to the semantics of the
    /// ZX_WAIT_ASYNC_EDGE flag on zx_wait_async. To avoid missing events, the caller must call
    /// query_events after calling this.
    ///
    /// If your file does not support blocking waits, leave this as the default implementation.
    fn wait_async(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        None
    }

    /// The events currently active on this file.
    ///
    /// If this function returns `POLLIN` or `POLLOUT`, then FileObject will
    /// add `POLLRDNORM` and `POLLWRNORM`, respective, which are equivalent in
    /// the Linux UAPI.
    ///
    /// See https://linux.die.net/man/2/poll
    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        FdEvents::POLLIN | FdEvents::POLLOUT
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        request: u32,
        _user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(request)
    }

    fn fcntl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        default_fcntl(current_task, file, cmd, arg)
    }

    /// Return a handle that allows access to this file descritor through the zxio protocols.
    ///
    /// If None is returned, the file will act as if it was a fd to `/dev/null`.
    fn to_handle(
        &self,
        file: &FileHandle,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        serve_file(current_task, file).map(|c| Some(c.into_handle()))
    }
}

/// Implements [`FileOps`] methods in a way that makes sense for non-seekable files.
/// You must implement [`FileOps::read`] and [`FileOps::write`].
macro_rules! fileops_impl_nonseekable {
    () => {
        fn read_at(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(ESPIPE)
        }
        fn write_at(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(ESPIPE)
        }
        fn seek(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: crate::types::off_t,
            _whence: crate::fs::SeekOrigin,
        ) -> Result<crate::types::off_t, crate::types::Errno> {
            use crate::types::errno::*;
            error!(ESPIPE)
        }
    };
}

/// Implements [`FileOps::read`] that calls [`FileOps::read_at`] with the current position.
macro_rules! fileops_impl_seekable_read {
    () => {
        fn read(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            let mut offset = file.offset.lock();
            let size = self.read_at(file, current_task, *offset as usize, data)?;
            *offset += size as crate::types::off_t;
            Ok(size)
        }
    };
}

/// Implements [`FileOps::write`] that calls [`FileOps::write_at`] with the current position.
macro_rules! fileops_impl_seekable_write {
    () => {
        fn write(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            let mut offset = file.offset.lock();
            if file.flags().contains(OpenFlags::APPEND) {
                *offset = file.node().info().size as crate::types::off_t;
            }
            let size = self.write_at(file, current_task, *offset as usize, data)?;
            *offset += size as crate::types::off_t;
            Ok(size)
        }
    };
}

pub fn default_seek(
    file: &FileObject,
    _current_task: &CurrentTask,
    offset: off_t,
    whence: SeekOrigin,
) -> Result<off_t, Errno> {
    let mut current_offset = file.offset.lock();
    let new_offset = match whence {
        SeekOrigin::Set => Some(offset),
        SeekOrigin::Cur => (*current_offset).checked_add(offset),
        SeekOrigin::End => {
            let stat = file.node().stat()?;
            offset.checked_add(stat.st_size as off_t)
        }
    }
    .ok_or_else(|| errno!(EINVAL))?;

    if new_offset < 0 {
        return error!(EINVAL);
    }

    *current_offset = new_offset;
    Ok(*current_offset)
}

/// Implements [`FileOps`] methods in a way that makes sense for seekable files.
/// You must implement [`FileOps::read_at`] and [`FileOps::write_at`].
macro_rules! fileops_impl_seekable {
    () => {
        fileops_impl_seekable_read!();
        fileops_impl_seekable_write!();

        fn seek(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            offset: crate::types::off_t,
            whence: crate::fs::SeekOrigin,
        ) -> Result<crate::types::off_t, crate::types::Errno> {
            crate::fs::default_seek(file, current_task, offset, whence)
        }
    };
}

macro_rules! fileops_impl_delegate_read_and_seek {
    ($self:ident, $delegate:expr) => {
        fn read(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            $delegate.read(file, current_task, data)
        }

        fn read_at(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            offset: usize,
            data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            $delegate.read_at(file, current_task, offset, data)
        }

        fn seek(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            offset: off_t,
            whence: crate::fs::SeekOrigin,
        ) -> Result<off_t, crate::types::Errno> {
            $delegate.seek(file, current_task, offset, whence)
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for files that ignore
/// seeking operations and always read/write at offset 0.
/// You must implement [`FileOps::read_at`] and [`FileOps::write_at`].
macro_rules! fileops_impl_seekless {
    () => {
        fn read(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            self.read_at(file, current_task, 0, data)
        }
        fn write(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            self.write_at(file, current_task, 0, data)
        }
        fn seek(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: crate::types::off_t,
            _whence: crate::fs::SeekOrigin,
        ) -> Result<crate::types::off_t, crate::types::Errno> {
            Ok(0)
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for directories. You must implement
/// [`FileOps::seek`] and [`FileOps::readdir`].
macro_rules! fileops_impl_directory {
    () => {
        fn read(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(EISDIR)
        }

        fn read_at(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(EISDIR)
        }

        fn write(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(EISDIR)
        }

        fn write_at(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(EISDIR)
        }
    };
}

// Public re-export of macros allows them to be used like regular rust items.

pub(crate) use fileops_impl_delegate_read_and_seek;
pub(crate) use fileops_impl_directory;
pub(crate) use fileops_impl_nonseekable;
pub(crate) use fileops_impl_seekable;
pub(crate) use fileops_impl_seekable_read;
pub(crate) use fileops_impl_seekable_write;
pub(crate) use fileops_impl_seekless;

pub fn default_ioctl(request: u32) -> Result<SyscallResult, Errno> {
    not_implemented!("ioctl: request=0x{:x}", request);
    error!(ENOTTY)
}

pub fn default_fcntl(
    current_task: &CurrentTask,
    file: &FileObject,
    cmd: u32,
    arg: u64,
) -> Result<SyscallResult, Errno> {
    match cmd {
        F_SETLK | F_SETLKW | F_GETLK | F_OFD_GETLK | F_OFD_SETLK | F_OFD_SETLKW => {
            let flock_ref = UserRef::<uapi::flock>::new(arg.into());
            let flock = current_task.mm.read_object(flock_ref)?;
            let cmd = RecordLockCommand::from_raw(cmd).ok_or_else(|| errno!(EINVAL))?;
            if let Some(flock) = file.record_lock(current_task, cmd, flock)? {
                current_task.mm.write_object(flock_ref, &flock)?;
            }
            Ok(SUCCESS)
        }
        _ => {
            not_implemented!("fcntl: command={} not implemented", cmd);
            error!(EINVAL)
        }
    }
}

pub struct OPathOps {}

impl OPathOps {
    pub fn new() -> OPathOps {
        OPathOps {}
    }
}

impl FileOps for OPathOps {
    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn read_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: off_t,
        _whence: SeekOrigin,
    ) -> Result<off_t, Errno> {
        error!(EBADF)
    }
    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        error!(EBADF)
    }
    fn readdir(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        error!(EBADF)
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _request: u32,
        _user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        error!(EBADF)
    }

    fn fcntl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        _arg: u64,
    ) -> Result<SyscallResult, Errno> {
        match cmd {
            F_SETLK | F_SETLKW | F_GETLK => {
                error!(EBADF)
            }
            _ => {
                // Note: this can be a valid operation for files opened with O_PATH.
                not_implemented!("fcntl: command={} not implemented", cmd);
                error!(EINVAL)
            }
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub enum FileAsyncOwner {
    #[default]
    Unowned,
    Thread(pid_t),
    Process(pid_t),
    ProcessGroup(pid_t),
}

impl FileAsyncOwner {
    pub fn validate(self, current_task: &CurrentTask) -> Result<(), Errno> {
        match self {
            FileAsyncOwner::Unowned => (),
            FileAsyncOwner::Thread(tid) => {
                current_task.get_task(tid).ok_or_else(|| errno!(ESRCH))?;
            }
            FileAsyncOwner::Process(pid) => {
                current_task.get_task(pid).ok_or_else(|| errno!(ESRCH))?;
            }
            FileAsyncOwner::ProcessGroup(pgid) => {
                current_task
                    .kernel()
                    .pids
                    .read()
                    .get_process_group(pgid)
                    .ok_or_else(|| errno!(ESRCH))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileObjectId(usize);

impl FileObjectId {
    fn new(id: *const FileObject) -> Self {
        Self(id as usize)
    }
}

/// A session with a file object.
///
/// Each time a client calls open(), we create a new FileObject from the
/// underlying FsNode that receives the open(). This object contains the state
/// that is specific to this sessions whereas the underlying FsNode contains
/// the state that is shared between all the sessions.
pub struct FileObject {
    ops: Box<dyn FileOps>,

    /// The NamespaceNode associated with this FileObject.
    ///
    /// Represents the name the process used to open this file.
    pub name: NamespaceNode,

    pub fs: FileSystemHandle,

    pub offset: Mutex<off_t>,

    flags: Mutex<OpenFlags>,

    async_owner: Mutex<FileAsyncOwner>,
}

pub type FileHandle = Arc<FileObject>;

impl FileObject {
    /// Create a FileObject that is not mounted in a namespace.
    ///
    /// In particular, this will create a new unrooted entries. This should not be used on
    /// file system with persistent entries, as the created entry will be out of sync with the one
    /// from the file system.
    ///
    /// The returned FileObject does not have a name.
    pub fn new_anonymous(
        ops: Box<dyn FileOps>,
        node: FsNodeHandle,
        flags: OpenFlags,
    ) -> FileHandle {
        assert!(!node.fs().has_permanent_entries());
        Self::new(ops, NamespaceNode::new_anonymous(DirEntry::new_unrooted(node)), flags)
    }

    /// Create a FileObject with an associated NamespaceNode.
    ///
    /// This function is not typically called directly. Instead, consider
    /// calling NamespaceNode::open.
    pub fn new(ops: Box<dyn FileOps>, name: NamespaceNode, flags: OpenFlags) -> FileHandle {
        let fs = name.entry.node.fs();
        Arc::new(Self {
            name,
            fs,
            ops,
            offset: Mutex::new(0),
            flags: Mutex::new(flags - OpenFlags::CREAT),
            async_owner: Default::default(),
        })
    }

    /// A unique identifier for this file object.
    ///
    /// Identifiers can be reused after the file object has been dropped.
    pub fn id(&self) -> FileObjectId {
        FileObjectId::new(self as *const FileObject)
    }

    /// The FsNode from which this FileObject was created.
    pub fn node(&self) -> &FsNodeHandle {
        &self.name.entry.node
    }

    pub fn can_read(&self) -> bool {
        // TODO: Consider caching the access mode outside of this lock
        // because it cannot change.
        self.flags.lock().can_read()
    }

    pub fn can_write(&self) -> bool {
        // TODO: Consider caching the access mode outside of this lock
        // because it cannot change.
        self.flags.lock().can_write()
    }

    fn ops(&self) -> &dyn FileOps {
        self.ops.as_ref()
    }

    /// Returns the `FileObject`'s `FileOps` as a `&T`, or `None` if the downcast fails.
    ///
    /// This is useful for syscalls that only operate on a certain type of file.
    pub fn downcast_file<T>(&self) -> Option<&T>
    where
        T: 'static,
    {
        self.ops().as_any().downcast_ref::<T>()
    }

    pub fn blocking_op<T, Op>(
        &self,
        current_task: &CurrentTask,
        mut op: Op,
        events: FdEvents,
        deadline: Option<zx::Time>,
    ) -> Result<T, Errno>
    where
        Op: FnMut() -> Result<BlockableOpsResult<T>, Errno>,
    {
        let is_partial = |result: &Result<BlockableOpsResult<T>, Errno>| match result {
            Err(e) if e.code == EAGAIN => true,
            Ok(v) => v.is_partial(),
            _ => false,
        };

        // Run the operation a first time without registering a waiter in case no wait is needed.
        let result = op();
        if self.flags().contains(OpenFlags::NONBLOCK) || !is_partial(&result) {
            return result.map(BlockableOpsResult::value);
        }

        let waiter = Waiter::new();
        loop {
            // Register the waiter before running the operation to prevent a race.
            self.wait_async(current_task, &waiter, events, WaitCallback::none());
            let result = op();
            if !is_partial(&result) {
                return result.map(BlockableOpsResult::value);
            }
            waiter.wait_until(current_task, deadline.unwrap_or(zx::Time::INFINITE)).map_err(
                |e| {
                    if e == ETIMEDOUT {
                        errno!(EAGAIN)
                    } else {
                        e
                    }
                },
            )?;
        }
    }

    pub fn read(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if !self.can_read() {
            return error!(EBADF);
        }

        let bytes_read = self.ops().read(self, current_task, data)?;

        // TODO(steveaustin) - omit updating time_access to allow info to be immutable
        // and thus allow simultaneous reads.
        self.name.update_atime();
        Ok(bytes_read)
    }

    pub fn read_at(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if !self.can_read() {
            return error!(EBADF);
        }
        self.ops().read_at(self, current_task, offset, data)
    }

    pub fn write(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if !self.can_write() {
            return error!(EBADF);
        }
        self.node().clear_suid_and_sgid_bits(current_task);
        let bytes_written = if self.flags().contains(OpenFlags::APPEND) {
            let _guard = self.node().append_lock.write(current_task)?;
            self.ops().write(self, current_task, data)?
        } else {
            let _guard = self.node().append_lock.read(current_task)?;
            self.ops().write(self, current_task, data)?
        };
        self.node().update_ctime_mtime();
        Ok(bytes_written)
    }

    pub fn write_at(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if !self.can_write() {
            return error!(EBADF);
        }
        self.node().clear_suid_and_sgid_bits(current_task);
        let bytes_written = {
            let _guard = self.node().append_lock.read(current_task)?;
            self.ops().write_at(self, current_task, offset, data)?
        };
        self.node().update_ctime_mtime();
        Ok(bytes_written)
    }

    pub fn seek(
        &self,
        current_task: &CurrentTask,
        offset: off_t,
        whence: SeekOrigin,
    ) -> Result<off_t, Errno> {
        self.ops().seek(self, current_task, offset, whence)
    }

    pub fn get_vmo(
        &self,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        if prot.contains(ProtectionFlags::READ) && !self.can_read() {
            return error!(EACCES);
        }
        if prot.contains(ProtectionFlags::WRITE) && !self.can_write() {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().get_vmo(self, current_task, length, prot)
    }

    pub fn mmap(
        &self,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        vmar_flags: zx::VmarFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<MappedVmo, Errno> {
        if vmar_flags.contains(zx::VmarFlags::PERM_READ) && !self.can_read() {
            return error!(EACCES);
        }
        if vmar_flags.contains(zx::VmarFlags::PERM_WRITE)
            && !self.can_write()
            && options.contains(MappingOptions::SHARED)
        {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().mmap(
            self,
            current_task,
            addr,
            vmo_offset,
            length,
            prot_flags,
            vmar_flags,
            options,
            filename,
        )
    }

    pub fn readdir(
        &self,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        match self.ops().readdir(self, current_task, sink) {
            // The ENOSPC we catch here is generated by DirentSink::add. We
            // return the error to the caller only if we didn't have space for
            // the first directory entry.
            //
            // We use ENOSPC rather than EINVAL to signal this condition
            // because EINVAL is a very generic error. We only want to perform
            // this transformation in exactly the case where there was not
            // sufficient space in the DirentSink.
            Err(errno) if errno == ENOSPC && sink.actual() > 0 => Ok(()),
            Err(errno) if errno == ENOSPC => error!(EINVAL),
            result => result,
        }?;

        self.name.update_atime();
        Ok(())
    }

    pub fn ioctl(
        &self,
        current_task: &CurrentTask,
        request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        self.ops().ioctl(self, current_task, request, user_addr)
    }

    pub fn fcntl(
        &self,
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        self.ops().fcntl(self, current_task, cmd, arg)
    }

    pub fn ftruncate(&self, current_task: &CurrentTask, length: u64) -> Result<(), Errno> {
        // The file must be opened with write permissions. Otherwise
        // truncating it is forbidden.
        if !self.can_write() {
            return error!(EINVAL);
        }

        self.node().ftruncate(current_task, length)
    }

    pub fn fallocate(&self, offset: u64, length: u64) -> Result<(), Errno> {
        // If the file is a pipe or FIFO, ESPIPE is returned.
        // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
        if self.node().is_fifo() {
            return error!(ESPIPE);
        }

        // Must be a regular file or directory.
        // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
        if !self.node().is_dir() && !self.node().is_reg() {
            return error!(ENODEV);
        }

        // The file must be opened with write permissions. Otherwise operation is forbidden.
        // See https://man7.org/linux/man-pages/man2/fallocate.2.html#ERRORS
        if !self.can_write() {
            return error!(EBADF);
        }

        self.node().fallocate(offset, length)
    }

    pub fn to_handle(
        self: &Arc<Self>,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.ops().to_handle(self, current_task)
    }

    pub fn update_file_flags(&self, value: OpenFlags, mask: OpenFlags) {
        let mask_bits = mask.bits();
        let mut flags = self.flags.lock();
        let bits = (flags.bits() & !mask_bits) | (value.bits() & mask_bits);
        *flags = OpenFlags::from_bits_truncate(bits);
    }

    pub fn flags(&self) -> OpenFlags {
        *self.flags.lock()
    }

    /// Get the async owner of this file.
    ///
    /// See fcntl(F_GETOWN)
    pub fn get_async_owner(&self) -> FileAsyncOwner {
        *self.async_owner.lock()
    }

    /// Set the async owner of this file.
    ///
    /// See fcntl(F_SETOWN)
    pub fn set_async_owner(&self, owner: FileAsyncOwner) {
        *self.async_owner.lock() = owner;
    }

    /// Wait on the specified events and call the EventHandler when ready
    pub fn wait_async(
        &self,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.ops().wait_async(
            self,
            current_task,
            waiter,
            events,
            Box::new(|observed| handler(add_equivalent_fd_events(observed))),
        )
    }

    /// The events currently active on this file.
    pub fn query_events(&self, current_task: &CurrentTask) -> FdEvents {
        add_equivalent_fd_events(self.ops().query_events(current_task))
    }

    //
    /// Updates the file's seek offset without an upper bound on the resulting offset.
    ///
    /// This is useful for files without a defined size.
    ///
    /// Errors if `whence` is invalid, or the calculated offset is invalid.
    ///
    /// - `offset`: The target offset from `whence`.
    /// - `whence`: The location from which to compute the updated offset.
    pub fn unbounded_seek(&self, offset: off_t, whence: SeekOrigin) -> Result<off_t, Errno> {
        let mut current_offset = self.offset.lock();
        let new_offset = match whence {
            SeekOrigin::Set => Some(offset),
            SeekOrigin::Cur => (*current_offset).checked_add(offset),
            SeekOrigin::End => Some(MAX_LFS_FILESIZE as i64),
        }
        .ok_or_else(|| errno!(EINVAL))?;

        if new_offset < 0 {
            return error!(EINVAL);
        }

        *current_offset = new_offset;
        Ok(*current_offset)
    }

    pub fn record_lock(
        &self,
        current_task: &CurrentTask,
        cmd: RecordLockCommand,
        flock: uapi::flock,
    ) -> Result<Option<uapi::flock>, Errno> {
        self.name.entry.node.record_lock(current_task, self, cmd, flock)
    }

    pub fn flush(&self) {
        self.ops().flush(self)
    }
}

impl Drop for FileObject {
    fn drop(&mut self) {
        self.ops().close(self);
        self.name.entry.node.on_file_closed(self);
    }
}

impl fmt::Debug for FileObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileObject")
            .field("name", &self.name)
            .field("offset", &self.offset)
            .finish()
    }
}
