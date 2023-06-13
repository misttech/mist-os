// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::HandleBased;
use fuchsia_zircon as zx;
use std::{fmt, sync::Arc};

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        file_server::serve_file,
        *,
    },
    lock::Mutex,
    logging::{impossible_error, not_implemented},
    mm::{
        vmo::round_up_to_system_page_size, DesiredAddress, MappedVmo, MappingName, MappingOptions,
        ProtectionFlags,
    },
    syscalls::*,
    task::*,
    types::{as_any::*, *},
};

pub const MAX_LFS_FILESIZE: usize = 0x7fffffffffffffff;

pub enum SeekTarget {
    /// Seek to the given offset relative to the start of the file.
    Set(off_t),
    /// Seek to the given offset relative to the current position.
    Cur(off_t),
    /// Seek to the given offset relative to the end of the file.
    End(off_t),
    /// Seek for the first data after the given offset,
    Data(off_t),
    /// Seek for the first hole after the given offset,
    Hole(off_t),
}

impl SeekTarget {
    pub fn from_raw(whence: u32, offset: off_t) -> Result<SeekTarget, Errno> {
        match whence {
            SEEK_SET => Ok(SeekTarget::Set(offset)),
            SEEK_CUR => Ok(SeekTarget::Cur(offset)),
            SEEK_END => Ok(SeekTarget::End(offset)),
            SEEK_DATA => Ok(SeekTarget::Data(offset)),
            SEEK_HOLE => Ok(SeekTarget::Hole(offset)),
            _ => error!(EINVAL),
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

    /// Returns whether the file has meaningful seek offsets. Returning `false` is only
    /// optimization and will makes `FileObject` never hold the offset lock when calling `read` and
    /// `write`.
    fn has_persistent_offsets(&self) -> bool {
        self.is_seekable()
    }

    /// Returns whether the file is seekable.
    fn is_seekable(&self) -> bool;

    /// Read from the file at an offset. If the file does not have persistent offsets (either
    /// directly, or because it is not seekable), offset will be 0 and can be ignored.
    /// Returns the number of bytes read.
    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>;
    /// Write to the file with an offset. If the file does not have persistent offsets (either
    /// directly, or because it is not seekable), offset will be 0 and can be ignored.
    /// Returns the number of bytes written.
    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>;

    /// Adjust the `current_offset` if the file is seekable.
    fn seek(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
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
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<MappedVmo, Errno> {
        let min_vmo_size = (vmo_offset as usize)
            .checked_add(round_up_to_system_page_size(length)?)
            .ok_or(errno!(EINVAL))?;
        let vmo = if options.contains(MappingOptions::SHARED) {
            self.get_vmo(file, current_task, Some(min_vmo_size), prot_flags)?
        } else {
            // TODO(tbodt): Use PRIVATE_CLONE to have the filesystem server do the clone for us.
            let base_prot_flags = (prot_flags | ProtectionFlags::READ) - ProtectionFlags::WRITE;
            let vmo = self.get_vmo(file, current_task, Some(min_vmo_size), base_prot_flags)?;
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
        _arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(request)
    }

    fn fcntl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        _arg: u64,
    ) -> Result<SyscallResult, Errno> {
        default_fcntl(cmd)
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

/// Implement the seek method for a file. The computation from the end of the file must be provided
/// through a callback.
///
/// Errors if the calculated offset is invalid.
///
/// - `current_offset`: The current position
/// - `target`: The location to seek to.
/// - `compute_end`: Compute the new offset from the end. Return an error if the operation is not
///    supported.
pub fn default_seek<F>(
    current_offset: off_t,
    target: SeekTarget,
    compute_end: F,
) -> Result<off_t, Errno>
where
    F: FnOnce(off_t) -> Result<off_t, Errno>,
{
    let new_offset = match target {
        SeekTarget::Set(offset) => Some(offset),
        SeekTarget::Cur(offset) => current_offset.checked_add(offset),
        SeekTarget::End(offset) => Some(compute_end(offset)?),
        SeekTarget::Data(offset) => {
            let eof = compute_end(0).unwrap_or(off_t::MAX);
            if offset >= eof {
                return error!(ENXIO);
            }
            Some(offset)
        }
        SeekTarget::Hole(offset) => {
            let eof = compute_end(0)?;
            if offset >= eof {
                return error!(ENXIO);
            }
            Some(eof)
        }
    }
    .ok_or_else(|| errno!(EINVAL))?;

    if new_offset < 0 {
        return error!(EINVAL);
    }

    Ok(new_offset)
}

/// Implement the seek method for a file without an upper bound on the resulting offset.
///
/// This is useful for files without a defined size.
///
/// Errors if the calculated offset is invalid.
///
/// - `current_offset`: The current position
/// - `target`: The location to seek to.
pub fn unbounded_seek(current_offset: off_t, target: SeekTarget) -> Result<off_t, Errno> {
    default_seek(current_offset, target, |_| Ok(MAX_LFS_FILESIZE as off_t))
}

macro_rules! fileops_impl_delegate_read_and_seek {
    ($self:ident, $delegate:expr) => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn read(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            offset: usize,
            data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            $delegate.read(file, current_task, offset, data)
        }

        fn seek(
            &$self,
            file: &FileObject,
            current_task: &crate::task::CurrentTask,
            current_offset: crate::types::off_t,
            target: crate::fs::SeekTarget,
        ) -> Result<off_t, crate::types::Errno> {
            $delegate.seek(file, current_task, current_offset, target)
        }
    };
}

/// Implements [`FileOps::seek`] in a way that makes sense for seekable files.
macro_rules! fileops_impl_seekable {
    () => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn seek(
            &self,
            file: &crate::fs::FileObject,
            current_task: &crate::task::CurrentTask,
            current_offset: crate::types::off_t,
            target: crate::fs::SeekTarget,
        ) -> Result<crate::types::off_t, crate::types::Errno> {
            crate::fs::default_seek(current_offset, target, |offset| {
                let file_size = file.node().stat(current_task)?.st_size as off_t;
                offset.checked_add(file_size).ok_or_else(|| errno!(EINVAL))
            })
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for non-seekable files.
macro_rules! fileops_impl_nonseekable {
    () => {
        fn is_seekable(&self) -> bool {
            false
        }

        fn seek(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _current_offset: crate::types::off_t,
            _target: crate::fs::SeekTarget,
        ) -> Result<crate::types::off_t, crate::types::Errno> {
            use crate::types::errno::*;
            error!(ESPIPE)
        }
    };
}

/// Implements [`FileOps::seek`] methods in a way that makes sense for files that ignore
/// seeking operations and always read/write at offset 0.
macro_rules! fileops_impl_seekless {
    () => {
        fn has_persistent_offsets(&self) -> bool {
            false
        }

        fn is_seekable(&self) -> bool {
            true
        }

        fn seek(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _current_offset: crate::types::off_t,
            _target: crate::fs::SeekTarget,
        ) -> Result<crate::types::off_t, crate::types::Errno> {
            Ok(0)
        }
    };
}

macro_rules! fileops_impl_dataless {
    () => {
        fn write(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, Errno> {
            error!(EINVAL)
        }

        fn read(
            &self,
            _file: &crate::fs::FileObject,
            _current_task: &crate::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::OutputBuffer,
        ) -> Result<usize, Errno> {
            error!(EINVAL)
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for directories. You must implement
/// [`FileOps::seek`] and [`FileOps::readdir`].
macro_rules! fileops_impl_directory {
    () => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn read(
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
            _offset: usize,
            _data: &mut dyn crate::fs::buffers::InputBuffer,
        ) -> Result<usize, crate::types::Errno> {
            use crate::types::errno::*;
            error!(EISDIR)
        }
    };
}

// Public re-export of macros allows them to be used like regular rust items.

pub(crate) use fileops_impl_dataless;
pub(crate) use fileops_impl_delegate_read_and_seek;
pub(crate) use fileops_impl_directory;
pub(crate) use fileops_impl_nonseekable;
pub(crate) use fileops_impl_seekable;
pub(crate) use fileops_impl_seekless;

pub fn default_ioctl(request: u32) -> Result<SyscallResult, Errno> {
    match request {
        TCGETS => error!(ENOTTY),
        _ => {
            not_implemented!("ioctl: request=0x{:x}", request);
            error!(ENOTTY)
        }
    }
}

pub fn default_fcntl(cmd: u32) -> Result<SyscallResult, Errno> {
    not_implemented!("fcntl: command=0x{:x}", cmd);
    error!(EOPNOTSUPP)
}

pub struct OPathOps {}

impl OPathOps {
    pub fn new() -> OPathOps {
        OPathOps {}
    }
}

impl FileOps for OPathOps {
    fn has_persistent_offsets(&self) -> bool {
        false
    }
    fn is_seekable(&self) -> bool {
        true
    }
    fn read(
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
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _current_offset: off_t,
        _target: SeekTarget,
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
        _arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        error!(EBADF)
    }
}

pub struct ProxyFileOps(pub FileHandle);

macro_rules! delegate {
    {
        $delegate_to:expr;
        $(
            fn $name:ident(&$self:ident, $file:ident: &FileObject $(, $arg_name:ident: $arg_type:ty)*$(,)?) $(-> $ret:ty)?;
        )*
    } => {
        $(
            fn $name(&$self, _file: &FileObject $(, $arg_name: $arg_type)*) $(-> $ret)? {
                $delegate_to.ops().$name(&$delegate_to $(, $arg_name)*)
            }
        )*
    }
}

impl FileOps for ProxyFileOps {
    delegate! {
        self.0;
        fn close(&self, _file: &FileObject);
        fn flush(&self, _file: &FileObject);
        fn read(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            offset: usize,
            data: &mut dyn OutputBuffer,
        ) -> Result<usize, Errno>;
        fn write(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            offset: usize,
            data: &mut dyn InputBuffer,
        ) -> Result<usize, Errno>;
        fn seek(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            offset: off_t,
            target: SeekTarget,
        ) -> Result<off_t, Errno>;
        fn get_vmo(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            _length: Option<usize>,
            _prot: ProtectionFlags,
        ) -> Result<Arc<zx::Vmo>, Errno>;
        fn mmap(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            addr: DesiredAddress,
            vmo_offset: u64,
            length: usize,
            prot_flags: ProtectionFlags,
            options: MappingOptions,
            filename: NamespaceNode,
        ) -> Result<MappedVmo, Errno>;
        fn readdir(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            _sink: &mut dyn DirentSink,
        ) -> Result<(), Errno>;
        fn wait_async(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            _waiter: &Waiter,
            _events: FdEvents,
            _handler: EventHandler,
        ) -> Option<WaitCanceler>;
        fn ioctl(
            &self,
            _file: &FileObject,
            _current_task: &CurrentTask,
            request: u32,
            _arg: SyscallArg,
        ) -> Result<SyscallResult, Errno>;
        fn fcntl(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            cmd: u32,
            arg: u64,
        ) -> Result<SyscallResult, Errno>;
    }
    // These don't take &FileObject making it too hard to handle them properly in the macro
    fn query_events(&self, current_task: &CurrentTask) -> FdEvents {
        self.0.ops().query_events(current_task)
    }
    fn has_persistent_offsets(&self) -> bool {
        self.0.ops().has_persistent_offsets()
    }
    fn is_seekable(&self) -> bool {
        self.0.ops().is_seekable()
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

    pub fn is_non_blocking(&self) -> bool {
        self.flags().contains(OpenFlags::NONBLOCK)
    }

    pub fn blocking_op<T, Op>(
        &self,
        current_task: &CurrentTask,
        events: FdEvents,
        deadline: Option<zx::Time>,
        mut op: Op,
    ) -> Result<T, Errno>
    where
        Op: FnMut() -> Result<T, Errno>,
    {
        // Run the operation a first time without registering a waiter in case no wait is needed.
        match op() {
            Err(errno) if errno == EAGAIN && !self.flags().contains(OpenFlags::NONBLOCK) => {}
            result => return result,
        }

        let waiter = Waiter::new();
        loop {
            // Register the waiter before running the operation to prevent a race.
            self.wait_async(current_task, &waiter, events, WaitCallback::none());
            match op() {
                Err(e) if e == EAGAIN => {}
                result => return result,
            }
            waiter
                .wait_until(current_task, deadline.unwrap_or(zx::Time::INFINITE))
                .map_err(|e| if e == ETIMEDOUT { errno!(EAGAIN) } else { e })?;
        }
    }

    pub fn is_seekable(&self) -> bool {
        self.ops().is_seekable()
    }

    pub fn has_persistent_offsets(&self) -> bool {
        self.ops().has_persistent_offsets()
    }

    /// Common implementation for `read` and `read_at`.
    fn read_internal<R>(&self, read: R) -> Result<usize, Errno>
    where
        R: FnOnce() -> Result<usize, Errno>,
    {
        if !self.can_read() {
            return error!(EBADF);
        }
        let bytes_read = read()?;
        // TODO(steveaustin) - omit updating time_access to allow info to be immutable
        // and thus allow simultaneous reads.
        self.name.update_atime()?;
        Ok(bytes_read)
    }

    pub fn read(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.read_internal(|| {
            if !self.ops().has_persistent_offsets() {
                return self.ops.read(self, current_task, 0, data);
            }

            let mut offset = self.offset.lock();
            let read = self.ops.read(self, current_task, *offset as usize, data)?;
            *offset += read as off_t;
            Ok(read)
        })
    }

    pub fn read_at(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }
        self.read_raw(current_task, offset, data)
    }

    /// Delegate the read operation to FileOps after executing the common permission check. This
    /// calls does not handle any operation related to file offsets.
    pub fn read_raw(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.read_internal(|| self.ops.read(self, current_task, offset, data))
    }

    /// Common implementation for `write` and `write_at`.
    fn write_internal<W>(&self, current_task: &CurrentTask, write: W) -> Result<usize, Errno>
    where
        W: FnOnce() -> Result<usize, Errno>,
    {
        if !self.can_write() {
            return error!(EBADF);
        }
        self.node().clear_suid_and_sgid_bits(current_task)?;
        let bytes_written = write()?;
        self.node().update_ctime_mtime()?;
        Ok(bytes_written)
    }

    pub fn write(
        &self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.write_internal(current_task, || {
            if !self.ops().has_persistent_offsets() {
                return self.ops().write(self, current_task, 0, data);
            }

            let mut offset = self.offset.lock();
            let written = if self.flags().contains(OpenFlags::APPEND) {
                let _guard = self.node().append_lock.write(current_task)?;
                *offset = self.ops().seek(self, current_task, *offset, SeekTarget::End(0))?;
                self.ops().write(self, current_task, *offset as usize, data)
            } else {
                let _guard = self.node().append_lock.read(current_task)?;
                self.ops().write(self, current_task, *offset as usize, data)
            }?;
            *offset += written as off_t;
            Ok(written)
        })
    }

    pub fn write_at(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }
        self.write_raw(current_task, offset, data)
    }

    /// Delegate the write operation to FileOps after executing the common permission check. This
    /// calls does not handle any operation related to file offsets.
    pub fn write_raw(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.write_internal(current_task, || {
            let _guard = self.node().append_lock.read(current_task)?;
            self.ops().write(self, current_task, offset, data)
        })
    }

    pub fn seek(&self, current_task: &CurrentTask, target: SeekTarget) -> Result<off_t, Errno> {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }

        if !self.ops().has_persistent_offsets() {
            return self.ops().seek(self, current_task, 0, target);
        }

        let mut offset_guard = self.offset.lock();
        let new_offset = self.ops().seek(self, current_task, *offset_guard, target)?;
        *offset_guard = new_offset;
        Ok(new_offset)
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
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<MappedVmo, Errno> {
        if prot_flags.intersects(ProtectionFlags::READ | ProtectionFlags::WRITE) && !self.can_read()
        {
            return error!(EACCES);
        }
        if prot_flags.contains(ProtectionFlags::WRITE)
            && !self.can_write()
            && options.contains(MappingOptions::SHARED)
        {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().mmap(self, current_task, addr, vmo_offset, length, prot_flags, options, filename)
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

        self.name.update_atime()?;
        Ok(())
    }

    pub fn ioctl(
        &self,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.ops().ioctl(self, current_task, request, arg)
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
            .field("fs", &String::from_utf8_lossy(self.fs.name()))
            .field("offset", &self.offset)
            .field("flags", &self.flags)
            .finish()
    }
}
