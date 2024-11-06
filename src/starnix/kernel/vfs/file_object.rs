// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::memory::MemoryObject;
use crate::mm::{DesiredAddress, MappingName, MappingOptions, MemoryAccessorExt, ProtectionFlags};
use crate::power::OnWakeOps;
use crate::security;
use crate::task::{
    CurrentTask, EncryptionKeyId, EventHandler, Task, WaitCallback, WaitCanceler, Waiter,
};
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::file_server::serve_file;
use crate::vfs::fsverity::{
    FsVerityState, {self},
};
use crate::vfs::{
    ActiveNamespaceNode, CurrentTaskAndLocked, DirentSink, EpollFileObject, EpollKey, FallocMode,
    FdNumber, FdTableId, FileReleaser, FileSystemHandle, FileWriteGuard, FileWriteGuardMode,
    FileWriteGuardRef, FsNodeHandle, NamespaceNode, RecordLockCommand, RecordLockOwner,
};
use fidl::HandleBased;
use fidl_fuchsia_fxfs::CryptManagementMarker;
use fuchsia_inspect_contrib::profile_duration;
use hkdf::Hkdf;
use linux_uapi::{FSCRYPT_MODE_AES_256_CTS, FSCRYPT_MODE_AES_256_XTS};
use starnix_logging::{
    impossible_error, log_error, log_info, log_warn, trace_duration, track_stub,
    CATEGORY_STARNIX_MM,
};
use starnix_sync::{
    BeforeFsNodeAppend, FileOpsCore, LockBefore, LockEqualOrBefore, Locked, Mutex, Unlocked,
};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_types::math::round_up_to_system_page_size;
use starnix_types::ownership::Releasable;
use starnix_uapi::as_any::AsAny;
use starnix_uapi::auth::CAP_FOWNER;
use starnix_uapi::errors::{Errno, EAGAIN, ETIMEDOUT};
use starnix_uapi::file_lease::FileLeaseType;
use starnix_uapi::file_mode::Access;
use starnix_uapi::inotify_mask::InotifyMask;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::seal_flags::SealFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    errno, error, fscrypt_add_key_arg, fscrypt_identifier, fsxattr, off_t, pid_t, uapi, FIGETBSZ,
    FIONBIO, FIONREAD, FIOQSIZE, FSCRYPT_KEY_IDENTIFIER_SIZE, FSCRYPT_KEY_SPEC_TYPE_IDENTIFIER,
    FSCRYPT_POLICY_V2, FS_CASEFOLD_FL, FS_IOC_ADD_ENCRYPTION_KEY, FS_IOC_ENABLE_VERITY,
    FS_IOC_FSGETXATTR, FS_IOC_FSSETXATTR, FS_IOC_GETFLAGS, FS_IOC_MEASURE_VERITY,
    FS_IOC_READ_VERITY_METADATA, FS_IOC_REMOVE_ENCRYPTION_KEY, FS_IOC_SETFLAGS,
    FS_IOC_SET_ENCRYPTION_POLICY, FS_VERITY_FL, SEEK_CUR, SEEK_DATA, SEEK_END, SEEK_HOLE, SEEK_SET,
    TCGETS,
};
use std::collections::hash_map::Entry;
use std::collections::HashSet;
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Weak};
use syncio::zxio_node_attr_has_t;
use zerocopy::IntoBytes;

pub const MAX_LFS_FILESIZE: usize = 0x7fff_ffff_ffff_ffff;

// In this implementation of fscrypt, we use a HKDF (Hmac Key Derivation Function) to derive a
// a wrapping key and wrapping key id from the raw key bytes passed in by a user on
// FS_IOC_ADD_ENCRYPTION_KEY. HKDFs requires an input "info" string. We define constants for the
// respective "info" strings here.
const FSCRYPT_KEY_IDENTIFIER_INFO: &'static str = "fscrypt0";
const FSCRYPT_WRAPPING_KEY_INFO: &'static str = "fscrypt1";

pub fn checked_add_offset_and_length(offset: usize, length: usize) -> Result<usize, Errno> {
    let end = offset.checked_add(length).ok_or_else(|| errno!(EINVAL))?;
    if end > MAX_LFS_FILESIZE {
        return error!(EINVAL);
    }
    Ok(end)
}

#[derive(Debug)]
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

    pub fn whence(&self) -> u32 {
        match self {
            Self::Set(_) => SEEK_SET,
            Self::Cur(_) => SEEK_CUR,
            Self::End(_) => SEEK_END,
            Self::Data(_) => SEEK_DATA,
            Self::Hole(_) => SEEK_HOLE,
        }
    }

    pub fn offset(&self) -> off_t {
        match self {
            Self::Set(off)
            | Self::Cur(off)
            | Self::End(off)
            | Self::Data(off)
            | Self::Hole(off) => *off,
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

/// Uses an HKDF to derive an fscrypt wrapping key and key identifier from a raw user key.
fn derive_wrapping_key(
    raw_key: &[u8],
) -> ([u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize], [u8; AES256_KEY_SIZE]) {
    let hk = Hkdf::<sha2::Sha256>::new(None, raw_key);
    let mut key_identifier = [0u8; FSCRYPT_KEY_IDENTIFIER_SIZE as usize];
    hk.expand(FSCRYPT_KEY_IDENTIFIER_INFO.as_bytes(), &mut key_identifier)
        .expect("FSCRYPT_KEY_IDENTIFIER_SIZE is a valid length for Sha256 to output");
    let mut wrapping_key = [0u8; AES256_KEY_SIZE];
    hk.expand(FSCRYPT_WRAPPING_KEY_INFO.as_bytes(), &mut wrapping_key)
        .expect("AES256_KEY_SIZE is a valid length for Sha256 to output");
    (key_identifier, wrapping_key)
}

/// Corresponds to struct file_operations in Linux, plus any filesystem-specific data.
pub trait FileOps: Send + Sync + AsAny + 'static {
    /// Called when the FileObject is closed.
    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) {
    }

    /// Called every time close() is called on this file, even if the file is not ready to be
    /// released.
    fn flush(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) {
    }

    /// Returns whether the file has meaningful seek offsets. Returning `false` is only
    /// optimization and will makes `FileObject` never hold the offset lock when calling `read` and
    /// `write`.
    fn has_persistent_offsets(&self) -> bool {
        self.is_seekable()
    }

    /// Returns whether the file is seekable.
    fn is_seekable(&self) -> bool;

    /// Returns true if `write()` operations on the file will update the seek offset.
    fn writes_update_seek_offset(&self) -> bool {
        self.has_persistent_offsets()
    }

    /// Read from the file at an offset. If the file does not have persistent offsets (either
    /// directly, or because it is not seekable), offset will be 0 and can be ignored.
    /// Returns the number of bytes read.
    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
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
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>;

    /// Adjust the `current_offset` if the file is seekable.
    fn seek(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno>;

    /// Syncs cached state associated with the file descriptor to persistent storage.
    ///
    /// The method blocks until the synchronization is complete.
    fn sync(&self, file: &FileObject, _current_task: &CurrentTask) -> Result<(), Errno>;

    /// Syncs cached data, and only enough metadata to retrieve said data, to persistent storage.
    ///
    /// The method blocks until the synchronization is complete.
    fn data_sync(&self, file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno> {
        // TODO(https://fxbug.dev/297305634) make a default macro once data can be done separately
        self.sync(file, current_task)
    }

    /// Returns a VMO representing this file. At least the requested protection flags must
    /// be set on the VMO. Reading or writing the VMO must read or write the file. If this is not
    /// possible given the requested protection, an error must be returned.
    /// The `length` is a hint for the desired size of the VMO. The returned VMO may be larger or
    /// smaller than the requested length.
    /// This method is typically called by [`Self::mmap`].
    fn get_memory(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        error!(ENODEV)
    }

    /// Responds to an mmap call. The default implementation calls [`Self::get_memory`] to get a VMO
    /// and then maps it with [`crate::mm::MemoryManager::map`].
    /// Only implement this trait method if your file needs to control mapping, or record where
    /// a VMO gets mapped.
    fn mmap(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        profile_duration!("FileOpsDefaultMmap");
        trace_duration!(CATEGORY_STARNIX_MM, c"FileOpsDefaultMmap");
        let min_memory_size = (memory_offset as usize)
            .checked_add(round_up_to_system_page_size(length)?)
            .ok_or_else(|| errno!(EINVAL))?;
        let mut memory = if options.contains(MappingOptions::SHARED) {
            profile_duration!("GetSharedVmo");
            trace_duration!(CATEGORY_STARNIX_MM, c"GetSharedVmo");
            self.get_memory(locked, file, current_task, Some(min_memory_size), prot_flags)?
        } else {
            profile_duration!("GetPrivateVmo");
            trace_duration!(CATEGORY_STARNIX_MM, c"GetPrivateVmo");
            // TODO(tbodt): Use PRIVATE_CLONE to have the filesystem server do the clone for us.
            let base_prot_flags = (prot_flags | ProtectionFlags::READ) - ProtectionFlags::WRITE;
            let memory = self.get_memory(
                locked,
                file,
                current_task,
                Some(min_memory_size),
                base_prot_flags,
            )?;
            let mut clone_flags = zx::VmoChildOptions::SNAPSHOT_AT_LEAST_ON_WRITE;
            if !prot_flags.contains(ProtectionFlags::WRITE) {
                clone_flags |= zx::VmoChildOptions::NO_WRITE;
            }
            trace_duration!(CATEGORY_STARNIX_MM, c"CreatePrivateChildVmo");
            Arc::new(
                memory.create_child(clone_flags, 0, memory.get_size()).map_err(impossible_error)?,
            )
        };

        // Write guard is necessary only for shared mappings. Note that this doesn't depend on
        // `prot_flags` since these can be changed later with `mprotect()`.
        let file_write_guard = if options.contains(MappingOptions::SHARED) && file.can_write() {
            profile_duration!("AcquireFileWriteGuard");
            let node = &file.name.entry.node;
            let mut state = node.write_guard_state.lock();

            // `F_SEAL_FUTURE_WRITE` should allow `mmap(PROT_READ)`, but block
            // `mprotect(PROT_WRITE)`. This is different from `F_SEAL_WRITE`, which blocks
            // `mmap(PROT_READ)`. To handle this case correctly remove `WRITE` right from the
            // VMO handle to ensure `mprotect(PROT_WRITE)` fails.
            let seals = state.get_seals().unwrap_or(SealFlags::empty());
            if seals.contains(SealFlags::FUTURE_WRITE)
                && !seals.contains(SealFlags::WRITE)
                && !prot_flags.contains(ProtectionFlags::WRITE)
            {
                let mut new_rights = zx::Rights::VMO_DEFAULT - zx::Rights::WRITE;
                if prot_flags.contains(ProtectionFlags::EXEC) {
                    new_rights |= zx::Rights::EXECUTE;
                }
                memory = Arc::new(memory.duplicate_handle(new_rights).map_err(impossible_error)?);

                FileWriteGuardRef(None)
            } else {
                state.create_write_guard(node.clone(), FileWriteGuardMode::WriteMapping)?.into_ref()
            }
        } else {
            FileWriteGuardRef(None)
        };

        current_task.mm().map_memory(
            addr,
            memory,
            memory_offset,
            length,
            prot_flags,
            file.max_access_for_memory_mapping(),
            options,
            MappingName::File(filename.into_active()),
            file_write_guard,
        )
    }

    /// Respond to a `getdents` or `getdents64` calls.
    ///
    /// The `file.offset` lock will be held while entering this method. The implementation must look
    /// at `sink.offset()` to read the current offset into the file.
    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        Ok(FdEvents::POLLIN | FdEvents::POLLOUT)
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        default_ioctl(file, locked, current_task, request, arg)
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
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        serve_file(current_task, file).map(|c| Some(c.0.into_handle()))
    }

    /// Returns the associated pid_t.
    ///
    /// Used by pidfd and `/proc/<pid>`. Unlikely to be used by other files.
    fn as_pid(&self, _file: &FileObject) -> Result<pid_t, Errno> {
        error!(EBADF)
    }

    fn readahead(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _length: usize,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }
}

impl<T: FileOps, P: Deref<Target = T> + Send + Sync + 'static> FileOps for P {
    fn close(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
    ) {
        self.deref().close(locked, file, current_task)
    }

    fn flush(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
    ) {
        self.deref().flush(locked, file, current_task)
    }

    fn has_persistent_offsets(&self) -> bool {
        self.deref().has_persistent_offsets()
    }

    fn writes_update_seek_offset(&self) -> bool {
        self.deref().writes_update_seek_offset()
    }

    fn is_seekable(&self) -> bool {
        self.deref().is_seekable()
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.deref().read(locked, file, current_task, offset, data)
    }

    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.deref().write(locked, file, current_task, offset, data)
    }

    fn seek(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        self.deref().seek(locked, file, current_task, current_offset, target)
    }

    fn sync(&self, file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno> {
        self.deref().sync(file, current_task)
    }

    fn data_sync(&self, file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno> {
        self.deref().data_sync(file, current_task)
    }

    fn get_memory(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        self.deref().get_memory(locked, file, current_task, length, prot)
    }

    fn mmap(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        self.deref().mmap(
            locked,
            file,
            current_task,
            addr,
            memory_offset,
            length,
            prot_flags,
            options,
            filename,
        )
    }

    fn readdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        self.deref().readdir(locked, file, current_task, sink)
    }

    fn wait_async(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.deref().wait_async(locked, file, current_task, waiter, events, handler)
    }

    fn query_events(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        self.deref().query_events(locked, file, current_task)
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.deref().ioctl(locked, file, current_task, request, arg)
    }

    fn fcntl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        self.deref().fcntl(file, current_task, cmd, arg)
    }

    fn to_handle(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        self.deref().to_handle(file, current_task)
    }

    fn as_pid(&self, file: &FileObject) -> Result<pid_t, Errno> {
        self.deref().as_pid(file)
    }

    fn readahead(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        length: usize,
    ) -> Result<(), Errno> {
        self.deref().readahead(file, current_task, offset, length)
    }
}

pub fn default_eof_offset(file: &FileObject, current_task: &CurrentTask) -> Result<off_t, Errno> {
    Ok(file.node().stat(current_task)?.st_size as off_t)
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

#[macro_export]
macro_rules! fileops_impl_delegate_read_and_seek {
    ($self:ident, $delegate:expr) => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn read(
            &$self,
            locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            file: &FileObject,
            current_task: &starnix_core::task::CurrentTask,
            offset: usize,
            data: &mut dyn starnix_core::vfs::buffers::OutputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            $delegate.read(locked, file, current_task, offset, data)
        }

        fn seek(
            &$self,
        locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            file: &FileObject,
            current_task: &starnix_core::task::CurrentTask,
            current_offset: starnix_uapi::off_t,
            target: starnix_core::vfs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            $delegate.seek(locked, file, current_task, current_offset, target)
        }
    };
}

/// Implements [`FileOps::seek`] in a way that makes sense for seekable files.
#[macro_export]
macro_rules! fileops_impl_seekable {
    () => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn seek(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            file: &starnix_core::vfs::FileObject,
            current_task: &starnix_core::task::CurrentTask,
            current_offset: starnix_uapi::off_t,
            target: starnix_core::vfs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            starnix_core::vfs::default_seek(current_offset, target, |offset| {
                let eof_offset = starnix_core::vfs::default_eof_offset(file, current_task)?;
                offset.checked_add(eof_offset).ok_or_else(|| starnix_uapi::errno!(EINVAL))
            })
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for non-seekable files.
#[macro_export]
macro_rules! fileops_impl_nonseekable {
    () => {
        fn is_seekable(&self) -> bool {
            false
        }

        fn seek(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
            _current_offset: starnix_uapi::off_t,
            _target: starnix_core::vfs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(ESPIPE)
        }
    };
}

/// Implements [`FileOps::seek`] methods in a way that makes sense for files that ignore
/// seeking operations and always read/write at offset 0.
#[macro_export]
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
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
            _current_offset: starnix_uapi::off_t,
            _target: starnix_core::vfs::SeekTarget,
        ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
            Ok(0)
        }
    };
}

#[macro_export]
macro_rules! fileops_impl_dataless {
    () => {
        fn write(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn starnix_core::vfs::buffers::InputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EINVAL)
        }

        fn read(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn starnix_core::vfs::buffers::OutputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EINVAL)
        }
    };
}

/// Implements [`FileOps`] methods in a way that makes sense for directories. You must implement
/// [`FileOps::seek`] and [`FileOps::readdir`].
#[macro_export]
macro_rules! fileops_impl_directory {
    () => {
        fn is_seekable(&self) -> bool {
            true
        }

        fn read(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn starnix_core::vfs::buffers::OutputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EISDIR)
        }

        fn write(
            &self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            _file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
            _offset: usize,
            _data: &mut dyn starnix_core::vfs::buffers::InputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            starnix_uapi::error!(EISDIR)
        }
    };
}

#[macro_export]
macro_rules! fileops_impl_noop_sync {
    () => {
        fn sync(
            &self,
            file: &starnix_core::vfs::FileObject,
            _current_task: &starnix_core::task::CurrentTask,
        ) -> Result<(), starnix_uapi::errors::Errno> {
            if !file.node().is_reg() && !file.node().is_dir() {
                return starnix_uapi::error!(EINVAL);
            }
            Ok(())
        }
    };
}

// Public re-export of macros allows them to be used like regular rust items.

pub use {
    fileops_impl_dataless, fileops_impl_delegate_read_and_seek, fileops_impl_directory,
    fileops_impl_nonseekable, fileops_impl_noop_sync, fileops_impl_seekable, fileops_impl_seekless,
};
pub const AES256_KEY_SIZE: usize = 32;

pub fn default_ioctl(
    file: &FileObject,
    locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    request: u32,
    arg: SyscallArg,
) -> Result<SyscallResult, Errno> {
    match request {
        TCGETS => error!(ENOTTY),
        FIGETBSZ => {
            let node = file.node();
            let supported_file = node.is_reg() || node.is_dir();
            if !supported_file {
                return error!(ENOTTY);
            }

            let blocksize = file.node().stat(current_task)?.st_blksize;
            current_task.write_object(arg.into(), &blocksize)?;
            Ok(SUCCESS)
        }
        FIONBIO => {
            let arg_ref = UserAddress::from(arg).into();
            let arg: i32 = current_task.read_object(arg_ref)?;
            let val = if arg == 0 {
                // Clear the NONBLOCK flag
                OpenFlags::empty()
            } else {
                // Set the NONBLOCK flag
                OpenFlags::NONBLOCK
            };
            file.update_file_flags(val, OpenFlags::NONBLOCK);
            Ok(SUCCESS)
        }
        FIOQSIZE => {
            let node = file.node();
            let supported_file = node.is_reg() || node.is_dir();
            if !supported_file {
                return error!(ENOTTY);
            }

            let size = file.node().stat(current_task)?.st_size;
            current_task.write_object(arg.into(), &size)?;
            Ok(SUCCESS)
        }
        FIONREAD => {
            track_stub!(TODO("https://fxbug.dev/322874897"), "FIONREAD");
            if !file.name.entry.node.is_reg() {
                return error!(ENOTTY);
            }

            let size = file
                .name
                .entry
                .node
                .fetch_and_refresh_info(current_task)
                .map_err(|_| errno!(EINVAL))?
                .size;
            let offset = usize::try_from(*file.offset.lock()).map_err(|_| errno!(EINVAL))?;
            let remaining =
                if size < offset { 0 } else { i32::try_from(size - offset).unwrap_or(i32::MAX) };
            current_task.write_object(arg.into(), &remaining)?;
            Ok(SUCCESS)
        }
        FS_IOC_FSGETXATTR => {
            track_stub!(TODO("https://fxbug.dev/322875209"), "FS_IOC_FSGETXATTR");
            let arg = UserAddress::from(arg).into();
            current_task.write_object(arg, &fsxattr::default())?;
            Ok(SUCCESS)
        }
        FS_IOC_FSSETXATTR => {
            track_stub!(TODO("https://fxbug.dev/322875271"), "FS_IOC_FSSETXATTR");
            let arg = UserAddress::from(arg).into();
            let _: fsxattr = current_task.read_object(arg)?;
            Ok(SUCCESS)
        }
        FS_IOC_GETFLAGS => {
            track_stub!(TODO("https://fxbug.dev/322874935"), "FS_IOC_GETFLAGS");
            let arg = UserAddress::from(arg).into();
            let mut flags: u32 = 0;
            if matches!(*file.node().fsverity.lock(), FsVerityState::FsVerity) {
                flags |= FS_VERITY_FL;
            }
            if file.node().info().casefold {
                flags |= FS_CASEFOLD_FL;
            }
            current_task.write_object(arg, &flags)?;
            Ok(SUCCESS)
        }
        FS_IOC_SETFLAGS => {
            track_stub!(TODO("https://fxbug.dev/322875367"), "FS_IOC_SETFLAGS");
            let arg = UserAddress::from(arg).into();
            let flags: u32 = current_task.read_object(arg)?;
            file.node().update_attributes(locked, current_task, |info| {
                info.casefold = flags & FS_CASEFOLD_FL != 0;
                Ok(())
            })?;
            Ok(SUCCESS)
        }
        FS_IOC_ENABLE_VERITY => {
            Ok(fsverity::ioctl::enable(locked, current_task, UserAddress::from(arg).into(), file)?)
        }
        FS_IOC_MEASURE_VERITY => {
            Ok(fsverity::ioctl::measure(locked, current_task, UserAddress::from(arg).into(), file)?)
        }
        FS_IOC_READ_VERITY_METADATA => {
            Ok(fsverity::ioctl::read_metadata(current_task, UserAddress::from(arg).into(), file)?)
        }
        FS_IOC_ADD_ENCRYPTION_KEY => {
            let fscrypt_add_key_ref = UserRef::<fscrypt_add_key_arg>::from(arg);
            let key_ref_addr = fscrypt_add_key_ref.next().addr();
            let mut fscrypt_add_key_arg = current_task.read_object(fscrypt_add_key_ref.clone())?;
            if fscrypt_add_key_arg.key_id != 0 {
                track_stub!(TODO("https://fxbug.dev/375649227"), "non-zero key ids");
                return error!(ENOTSUP);
            }
            if fscrypt_add_key_arg.key_spec.type_ != FSCRYPT_KEY_SPEC_TYPE_IDENTIFIER {
                track_stub!(TODO("https://fxbug.dev/375648306"), "fscrypt descriptor type");
                return error!(ENOTSUP);
            }
            let key = current_task
                .read_memory_to_vec(key_ref_addr, fscrypt_add_key_arg.raw_size as usize)?;
            let root = current_task.fs().root();
            let path_from_root = file.name.path_from_root(Some(&root)).into_path();
            let user_id = current_task.creds().uid;
            let (key_identifier, wrapping_key) = derive_wrapping_key(key.as_bytes());

            match current_task
                .kernel()
                .encryption_keys
                .write()
                .entry(EncryptionKeyId::from(key_identifier))
            {
                Entry::Occupied(mut e) => {
                    e.get_mut().push(user_id);
                }
                Entry::Vacant(e) => {
                    e.insert(vec![user_id]);
                    // Connect to the fuchsia.fxfs.CryptManagement service for adding keys.
                    let crypt_management_proxy = current_task
                        .kernel()
                        .connect_to_protocol_at_container_svc::<CryptManagementMarker>()
                        .map_err(|_| errno!(ENOENT))?
                        .into_sync_proxy();

                    crypt_management_proxy
                        .add_wrapping_key(
                            &key_identifier,
                            wrapping_key.as_bytes(),
                            zx::MonotonicInstant::INFINITE,
                        )
                        .map_err(|e| errno!(EINVAL, e))?
                        .map_err(|e| {
                            log_warn!("add wrapping key failed with {:?}", e);
                            errno!(EIO, zx::Status::from_raw(e))
                        })?;
                }
            }
            log_info!(
                "Adding encryption key {:?} for {:?} for user {:?}",
                &key_identifier,
                &path_from_root,
                user_id
            );
            fscrypt_add_key_arg.key_spec.u.identifier =
                fscrypt_identifier { value: key_identifier, ..Default::default() };
            current_task.write_object(fscrypt_add_key_ref, &fscrypt_add_key_arg)?;
            Ok(SUCCESS)
        }
        FS_IOC_SET_ENCRYPTION_POLICY => {
            let fscrypt_policy_ref = UserRef::<uapi::fscrypt_policy_v2>::from(arg);
            let policy = current_task.read_object(fscrypt_policy_ref)?;
            if policy.version as u32 != FSCRYPT_POLICY_V2 {
                track_stub!(TODO("https://fxbug.dev/375649656"), "fscrypt policy v1");
                return error!(ENOTSUP);
            }
            if policy.flags != 0 {
                track_stub!(
                    TODO("https://fxbug.dev/375700939"),
                    "fscrypt policy flags",
                    policy.flags
                );
            }
            if policy.contents_encryption_mode as u32 != FSCRYPT_MODE_AES_256_XTS {
                track_stub!(
                    TODO("https://fxbug.dev/375684057"),
                    "fscrypt encryption modes",
                    policy.contents_encryption_mode
                );
            }
            if policy.filenames_encryption_mode as u32 != FSCRYPT_MODE_AES_256_CTS {
                track_stub!(
                    TODO("https://fxbug.dev/375684057"),
                    "fscrypt encryption modes",
                    policy.filenames_encryption_mode
                );
            }
            let root = current_task.fs().root();
            let path_from_root = file.name.path_from_root(Some(&root)).into_path();
            let user_id = current_task.creds().uid;
            log_info!(
                "Setting encryption policy for {:?} with key {:?} for user {:?}",
                &path_from_root,
                &policy.master_key_identifier,
                user_id,
            );

            if user_id != file.node().info().uid && !current_task.creds().has_capability(CAP_FOWNER)
            {
                return error!(EACCES);
            }

            if let Some(users) = &current_task
                .kernel()
                .encryption_keys
                .read()
                .get(&EncryptionKeyId::from(policy.master_key_identifier))
            {
                if !users.contains(&user_id) {
                    return error!(ENOKEY);
                }
            } else {
                track_stub!(
                    TODO("https://fxbug.dev/375067633"),
                    "users with CAP_FOWNER can set encryption policies with unadded keys"
                );
                return error!(ENOKEY);
            }

            let has = zxio_node_attr_has_t { wrapping_key_id: true, ..Default::default() };
            let attributes = file.node().ops().get_attr(has)?;
            if attributes.has.wrapping_key_id {
                if attributes.wrapping_key_id != policy.master_key_identifier {
                    return Err(errno!(EEXIST));
                }
            } else {
                file.node().update_info(|info| {
                    info.wrapping_key_id = Some(policy.master_key_identifier);
                });
                let has = zxio_node_attr_has_t { wrapping_key_id: true, ..Default::default() };
                file.node().ops().update_attributes(
                    &mut locked.cast_locked::<FileOpsCore>(),
                    current_task,
                    &file.node().info(),
                    has,
                )?;
            }
            Ok(SUCCESS)
        }
        FS_IOC_REMOVE_ENCRYPTION_KEY => {
            let fscrypt_remove_key_arg_ref = UserRef::<uapi::fscrypt_remove_key_arg>::from(arg);
            let fscrypt_remove_key_arg = current_task.read_object(fscrypt_remove_key_arg_ref)?;
            if fscrypt_remove_key_arg.key_spec.type_ != FSCRYPT_KEY_SPEC_TYPE_IDENTIFIER {
                track_stub!(TODO("https://fxbug.dev/375648306"), "fscrypt descriptor type");
                return error!(ENOTSUP);
            }
            let user_id = current_task.creds().uid;
            let identifier = unsafe { fscrypt_remove_key_arg.key_spec.u.identifier.value };
            match current_task
                .kernel()
                .encryption_keys
                .write()
                .entry(EncryptionKeyId::from(identifier))
            {
                Entry::Occupied(mut e) => {
                    let user_ids = e.get_mut();
                    if !user_ids.contains(&user_id) {
                        return error!(ENOKEY);
                    } else {
                        let index = user_ids.iter().position(|x: &u32| *x == user_id).unwrap();
                        user_ids.remove(index);
                        if user_ids.is_empty() {
                            // Connect to the fuchsia.fxfs.CryptManagement service for forgetting
                            // keys.
                            let crypt_management_proxy = current_task
                                .kernel()
                                .connect_to_protocol_at_container_svc::<CryptManagementMarker>()
                                .map_err(|_| errno!(ENOENT))?
                                .into_sync_proxy();
                            crypt_management_proxy
                                .forget_wrapping_key(&identifier, zx::MonotonicInstant::INFINITE)
                                .map_err(|e| errno!(EINVAL, e))?
                                .map_err(|e| errno!(EIO, zx::Status::from_raw(e)))?;
                            e.remove();
                        }
                    }
                }
                Entry::Vacant(_) => {
                    return error!(ENOKEY);
                }
            }
            log_info!("Removing encryption key {:?} for user {:?}", &identifier, user_id,);
            Ok(SUCCESS)
        }
        _ => {
            track_stub!(TODO("https://fxbug.dev/322874917"), "ioctl fallthrough", request);
            error!(ENOTTY)
        }
    }
}

pub fn default_fcntl(cmd: u32) -> Result<SyscallResult, Errno> {
    track_stub!(TODO("https://fxbug.dev/322875704"), "default fcntl", cmd);
    error!(EINVAL)
}

pub struct OPathOps {}

impl OPathOps {
    pub fn new() -> OPathOps {
        OPathOps {}
    }
}

impl FileOps for OPathOps {
    fileops_impl_noop_sync!();

    fn has_persistent_offsets(&self) -> bool {
        false
    }
    fn is_seekable(&self) -> bool {
        true
    }
    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EBADF)
    }
    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _current_offset: off_t,
        _target: SeekTarget,
    ) -> Result<off_t, Errno> {
        error!(EBADF)
    }
    fn get_memory(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        error!(EBADF)
    }
    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        error!(EBADF)
    }

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
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
        fn fcntl(
            &self,
            file: &FileObject,
            current_task: &CurrentTask,
            cmd: u32,
            arg: u64,
        ) -> Result<SyscallResult, Errno>;
        fn sync(&self, file: &FileObject, current_task: &CurrentTask) -> Result<(), Errno>;
    }
    // These don't take &FileObject making it too hard to handle them properly in the macro
    fn has_persistent_offsets(&self) -> bool {
        self.0.ops().has_persistent_offsets()
    }
    fn writes_update_seek_offset(&self) -> bool {
        self.0.ops().writes_update_seek_offset()
    }
    fn is_seekable(&self) -> bool {
        self.0.ops().is_seekable()
    }
    // These take &mut Locked<'_, L> as a second argument
    fn close(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        self.0.ops().close(locked, &self.0, current_task);
    }
    fn flush(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) {
        self.0.ops().close(locked, &self.0, current_task);
    }
    fn wait_async(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.0.ops().wait_async(locked, &self.0, current_task, waiter, events, handler)
    }
    fn query_events(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        self.0.ops().query_events(locked, &self.0, current_task)
    }
    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        self.0.ops().read(locked, &self.0, current_task, offset, data)
    }
    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        self.0.ops().write(locked, &self.0, current_task, offset, data)
    }
    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.0.ops().ioctl(locked, &self.0, current_task, request, arg)
    }
    fn readdir(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        self.0.ops().readdir(locked, &self.0, current_task, sink)
    }
    fn get_memory(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        self.0.ops.get_memory(locked, &self.0, current_task, length, prot)
    }
    fn mmap(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        self.0.ops.mmap(
            locked,
            &self.0,
            current_task,
            addr,
            memory_offset,
            length,
            prot_flags,
            options,
            filename,
        )
    }
    fn seek(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        self.0.ops.seek(locked, &self.0, current_task, offset, target)
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
            FileAsyncOwner::Thread(id) | FileAsyncOwner::Process(id) => {
                Task::from_weak(&current_task.get_task(id))?;
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
pub struct FileObjectId(u64);

/// A session with a file object.
///
/// Each time a client calls open(), we create a new FileObject from the
/// underlying FsNode that receives the open(). This object contains the state
/// that is specific to this sessions whereas the underlying FsNode contains
/// the state that is shared between all the sessions.
pub struct FileObject {
    /// Weak reference to the `FileHandle` of this `FileObject`. This allows to retrieve the
    /// `FileHandle` from a `FileObject`.
    pub weak_handle: WeakFileHandle,

    /// A unique identifier for this file object.
    pub id: FileObjectId,

    ops: Box<dyn FileOps>,

    /// The NamespaceNode associated with this FileObject.
    ///
    /// Represents the name the process used to open this file.
    pub name: ActiveNamespaceNode,

    pub fs: FileSystemHandle,

    pub offset: Mutex<off_t>,

    flags: Mutex<OpenFlags>,

    async_owner: Mutex<FileAsyncOwner>,

    /// A set of epoll file descriptor numbers that tracks which `EpollFileObject`s add this
    /// `FileObject` as the control file.
    epoll_files: Mutex<HashSet<FdNumber>>,

    /// See fcntl F_SETLEASE and F_GETLEASE.
    lease: Mutex<FileLeaseType>,

    _file_write_guard: Option<FileWriteGuard>,

    _security_state: security::FileObjectState,
}

pub type FileHandle = Arc<FileReleaser>;
pub type WeakFileHandle = Weak<FileReleaser>;

impl FileObject {
    /// Create a FileObject that is not mounted in a namespace.
    ///
    /// In particular, this will create a new unrooted entries. This should not be used on
    /// file system with persistent entries, as the created entry will be out of sync with the one
    /// from the file system.
    ///
    /// The returned FileObject does not have a name.
    pub fn new_anonymous(
        current_task: &CurrentTask,
        ops: Box<dyn FileOps>,
        node: FsNodeHandle,
        flags: OpenFlags,
    ) -> FileHandle {
        assert!(!node.fs().has_permanent_entries());
        Self::new(current_task, ops, NamespaceNode::new_anonymous_unrooted(node), flags)
            .expect("Failed to create anonymous FileObject")
    }

    /// Create a FileObject with an associated NamespaceNode.
    ///
    /// This function is not typically called directly. Instead, consider
    /// calling NamespaceNode::open.
    pub fn new(
        current_task: &CurrentTask,
        ops: Box<dyn FileOps>,
        name: NamespaceNode,
        flags: OpenFlags,
    ) -> Result<FileHandle, Errno> {
        let file_write_guard = if flags.can_write() {
            Some(name.entry.node.create_write_guard(FileWriteGuardMode::WriteFile)?)
        } else {
            None
        };
        let fs = name.entry.node.fs();
        let kernel = fs.kernel.upgrade().ok_or_else(|| errno!(ENOENT))?;
        let id = FileObjectId(kernel.next_file_object_id.next());
        let _security_state = security::file_alloc_security(current_task);
        let file = FileHandle::new_cyclic(|weak_handle| {
            Self {
                weak_handle: weak_handle.clone(),
                id,
                name: name.into_active(),
                fs,
                ops,
                offset: Mutex::new(0),
                flags: Mutex::new(flags - OpenFlags::CREAT),
                async_owner: Default::default(),
                epoll_files: Default::default(),
                lease: Default::default(),
                _file_write_guard: file_write_guard,
                _security_state,
            }
            .into()
        });
        file.notify(InotifyMask::OPEN);
        Ok(file)
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

    pub fn max_access_for_memory_mapping(&self) -> Access {
        let mut access = Access::EXEC;
        let flags = self.flags.lock();
        if flags.can_read() {
            access |= Access::READ;
        }
        if flags.can_write() {
            access |= Access::WRITE;
        }
        access
    }

    pub fn ops(&self) -> &dyn FileOps {
        self.ops.as_ref()
    }

    pub fn ops_type_name(&self) -> &'static str {
        self.ops().type_name()
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

    pub fn blocking_op<L, T, Op>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        events: FdEvents,
        deadline: Option<zx::MonotonicInstant>,
        mut op: Op,
    ) -> Result<T, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
        Op: FnMut(&mut Locked<'_, L>) -> Result<T, Errno>,
    {
        // Run the operation a first time without registering a waiter in case no wait is needed.
        match op(locked) {
            Err(errno) if errno == EAGAIN && !self.flags().contains(OpenFlags::NONBLOCK) => {}
            result => return result,
        }

        let waiter = Waiter::new();
        loop {
            // Register the waiter before running the operation to prevent a race.
            self.wait_async(locked, current_task, &waiter, events, WaitCallback::none());
            match op(locked) {
                Err(e) if e == EAGAIN => {}
                result => return result,
            }
            waiter
                .wait_until(current_task, deadline.unwrap_or(zx::MonotonicInstant::INFINITE))
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
        self.update_atime();
        if bytes_read > 0 {
            self.notify(InotifyMask::ACCESS);
        }

        Ok(bytes_read)
    }

    pub fn read<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.read_internal(|| {
            let mut locked = locked.cast_locked::<FileOpsCore>();
            if !self.ops().has_persistent_offsets() {
                if data.available() > MAX_LFS_FILESIZE {
                    return error!(EINVAL);
                }
                return self.ops.read(&mut locked, self, current_task, 0, data);
            }

            let mut offset_guard = self.offset.lock();
            let offset = *offset_guard as usize;
            checked_add_offset_and_length(offset, data.available())?;
            let read = self.ops.read(&mut locked, self, current_task, offset, data)?;
            *offset_guard += read as off_t;
            Ok(read)
        })
    }

    pub fn read_at<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }
        checked_add_offset_and_length(offset, data.available())?;
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.read_internal(|| self.ops.read(&mut locked, self, current_task, offset, data))
    }

    /// Common checks before calling ops().write.
    fn write_common<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // We need to cap the size of `data` to prevent us from growing the file too large,
        // according to <https://man7.org/linux/man-pages/man2/write.2.html>:
        //
        //   The number of bytes written may be less than count if, for example, there is
        //   insufficient space on the underlying physical medium, or the RLIMIT_FSIZE resource
        //   limit is encountered (see setrlimit(2)),
        checked_add_offset_and_length(offset, data.available())?;
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().write(&mut locked, self, current_task, offset, data)
    }

    /// Common wrapper work for `write` and `write_at`.
    fn write_fn<W, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        write: W,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
        W: FnOnce(&mut Locked<'_, L>) -> Result<usize, Errno>,
    {
        if !self.can_write() {
            return error!(EBADF);
        }
        self.node().clear_suid_and_sgid_bits(locked, current_task)?;
        let bytes_written = write(locked)?;
        self.node().update_ctime_mtime();

        if bytes_written > 0 {
            self.notify(InotifyMask::MODIFY);
        }

        Ok(bytes_written)
    }

    pub fn write<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.write_fn(locked, current_task, |locked| {
            if !self.ops().has_persistent_offsets() {
                return self.write_common(locked, current_task, 0, data);
            }
            // TODO(https://fxbug.dev/333540469): write_fn should take L: LockBefore<FsNodeAppend>,
            // but FileOpsCore must be after FsNodeAppend
            let mut locked = Unlocked::new();
            let mut offset = self.offset.lock();
            let bytes_written = if self.flags().contains(OpenFlags::APPEND) {
                let (_guard, mut locked) =
                    self.node().append_lock.write_and(&mut locked, current_task)?;
                *offset = self.ops().seek(
                    &mut locked.cast_locked::<FileOpsCore>(),
                    self,
                    current_task,
                    *offset,
                    SeekTarget::End(0),
                )?;
                self.write_common(&mut locked, current_task, *offset as usize, data)
            } else {
                let (_guard, mut locked) =
                    self.node().append_lock.read_and(&mut locked, current_task)?;
                self.write_common(&mut locked, current_task, *offset as usize, data)
            }?;
            if self.ops().writes_update_seek_offset() {
                *offset += bytes_written as off_t;
            }
            Ok(bytes_written)
        })
    }

    pub fn write_at<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        mut offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }
        self.write_fn(locked, current_task, |_locked| {
            // TODO(https://fxbug.dev/333540469): write_fn should take L: LockBefore<FsNodeAppend>,
            // but FileOpsCore must be after FsNodeAppend
            let mut locked = Unlocked::new();
            let (_guard, mut locked) =
                self.node().append_lock.read_and(&mut locked, current_task)?;

            // According to LTP test pwrite04:
            //
            //   POSIX requires that opening a file with the O_APPEND flag should have no effect on the
            //   location at which pwrite() writes data. However, on Linux, if a file is opened with
            //   O_APPEND, pwrite() appends data to the end of the file, regardless of the value of offset.
            if self.flags().contains(OpenFlags::APPEND) && self.ops().is_seekable() {
                checked_add_offset_and_length(offset, data.available())?;
                offset = default_eof_offset(self, current_task)? as usize;
            }

            self.write_common(&mut locked, current_task, offset, data)
        })
    }

    pub fn seek<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        target: SeekTarget,
    ) -> Result<off_t, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        let locked = &mut locked;

        if !self.ops().is_seekable() {
            return error!(ESPIPE);
        }

        if !self.ops().has_persistent_offsets() {
            return self.ops().seek(locked, self, current_task, 0, target);
        }

        let mut offset_guard = self.offset.lock();
        let new_offset = self.ops().seek(locked, self, current_task, *offset_guard, target)?;
        *offset_guard = new_offset;
        Ok(new_offset)
    }

    pub fn sync(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.ops().sync(self, current_task)
    }

    pub fn data_sync(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        self.ops().data_sync(self, current_task)
    }

    pub fn get_memory<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if prot.contains(ProtectionFlags::READ) && !self.can_read() {
            return error!(EACCES);
        }
        if prot.contains(ProtectionFlags::WRITE) && !self.can_write() {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().get_memory(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            length,
            prot,
        )
    }

    pub fn mmap<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        if !self.can_read() {
            return error!(EACCES);
        }
        if prot_flags.contains(ProtectionFlags::WRITE)
            && !self.can_write()
            && options.contains(MappingOptions::SHARED)
        {
            return error!(EACCES);
        }
        // TODO: Check for PERM_EXECUTE by checking whether the filesystem is mounted as noexec.
        self.ops().mmap(
            &mut locked,
            self,
            current_task,
            addr,
            memory_offset,
            length,
            prot_flags,
            options,
            filename,
        )
    }

    pub fn readdir<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut locked = locked.cast_locked::<FileOpsCore>();
        if self.name.entry.is_dead() {
            return error!(ENOENT);
        }

        self.ops().readdir(&mut locked, self, current_task, sink)?;
        self.update_atime();
        self.notify(InotifyMask::ACCESS);
        Ok(())
    }

    pub fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.ops().ioctl(locked, self, current_task, request, arg)
    }

    pub fn fcntl(
        &self,
        current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        self.ops().fcntl(self, current_task, cmd, arg)
    }

    pub fn ftruncate<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno>
    where
        L: LockBefore<BeforeFsNodeAppend>,
    {
        // The file must be opened with write permissions. Otherwise
        // truncating it is forbidden.
        if !self.can_write() {
            return error!(EINVAL);
        }
        self.node().ftruncate(locked, current_task, length)?;
        self.name.entry.notify_ignoring_excl_unlink(InotifyMask::MODIFY);
        Ok(())
    }

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

        self.node().fallocate(locked, current_task, mode, offset, length)?;
        self.notify(InotifyMask::MODIFY);
        Ok(())
    }

    pub fn to_handle(&self, current_task: &CurrentTask) -> Result<Option<zx::Handle>, Errno> {
        self.ops().to_handle(self, current_task)
    }

    pub fn as_pid(&self) -> Result<pid_t, Errno> {
        self.ops().as_pid(self)
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

    /// See fcntl(F_GETLEASE)
    pub fn get_lease(&self, _current_task: &CurrentTask) -> FileLeaseType {
        *self.lease.lock()
    }

    /// See fcntl(F_SETLEASE)
    pub fn set_lease(
        &self,
        _current_task: &CurrentTask,
        lease: FileLeaseType,
    ) -> Result<(), Errno> {
        if !self.node().is_reg() {
            return error!(EINVAL);
        }
        if lease == FileLeaseType::Read && self.can_write() {
            return error!(EAGAIN);
        }
        *self.lease.lock() = lease;
        Ok(())
    }

    /// Wait on the specified events and call the EventHandler when ready
    pub fn wait_async<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        mut handler: EventHandler,
    ) -> Option<WaitCanceler>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        handler.add_mapping(add_equivalent_fd_events);
        self.ops().wait_async(
            &mut locked.cast_locked::<FileOpsCore>(),
            self,
            current_task,
            waiter,
            events,
            handler,
        )
    }

    /// The events currently active on this file.
    pub fn query_events<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.ops()
            .query_events(&mut locked.cast_locked::<FileOpsCore>(), self, current_task)
            .map(add_equivalent_fd_events)
    }

    pub fn record_lock(
        &self,
        current_task: &CurrentTask,
        cmd: RecordLockCommand,
        flock: uapi::flock,
    ) -> Result<Option<uapi::flock>, Errno> {
        self.node().record_lock(current_task, self, cmd, flock)
    }

    pub fn flush<L>(&self, locked: &mut Locked<'_, L>, current_task: &CurrentTask, id: FdTableId)
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.name.entry.node.record_lock_release(RecordLockOwner::FdTable(id));
        self.ops().flush(&mut locked.cast_locked::<FileOpsCore>(), self, current_task)
    }

    // Notifies watchers on the current node and its parent about an event.
    pub fn notify(&self, event_mask: InotifyMask) {
        self.name.notify(event_mask)
    }

    fn update_atime(&self) {
        if !self.flags().contains(OpenFlags::NOATIME) {
            self.name.update_atime();
        }
    }

    pub fn readahead(
        &self,
        current_task: &CurrentTask,
        offset: usize,
        length: usize,
    ) -> Result<(), Errno> {
        // readfile() fails with EBADF if the file was not open for read.
        if !self.can_read() {
            return error!(EBADF);
        }
        checked_add_offset_and_length(offset, length)?;
        self.ops().readahead(self, current_task, offset, length)
    }

    /// Register the fd number of an `EpollFileObject` that listens to events from this
    /// `FileObject`.
    pub fn register_epfd(&self, fd: FdNumber) {
        self.epoll_files.lock().insert(fd);
    }

    pub fn unregister_epfd(&self, fd: FdNumber) {
        self.epoll_files.lock().remove(&fd);
    }
}

impl Releasable for FileObject {
    type Context<'a: 'b, 'b> = CurrentTaskAndLocked<'a, 'b>;

    fn release<'a: 'b, 'b>(self, context: Self::Context<'a, 'b>) {
        let (locked, current_task) = context;
        // Release all wake leases associated with this file in the corresponding `WaitObject`
        // of each registered epfd.
        for epfd in self.epoll_files.lock().drain() {
            if let Ok(file) = current_task.files.get(epfd) {
                if let Some(_epoll_object) = file.downcast_file::<EpollFileObject>() {
                    current_task
                        .kernel()
                        .suspend_resume_manager
                        .remove_epoll(self.weak_handle.as_ptr() as EpollKey);
                }
            }
        }
        let mut locked = locked.cast_locked::<FileOpsCore>();
        self.ops().close(&mut locked, &self, current_task);
        self.name.entry.node.on_file_closed(&self);
        let event =
            if self.can_write() { InotifyMask::CLOSE_WRITE } else { InotifyMask::CLOSE_NOWRITE };
        self.notify(event);
    }
}

impl fmt::Debug for FileObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileObject")
            .field("name", &self.name)
            .field("fs", &self.fs.name())
            .field("offset", &self.offset)
            .field("flags", &self.flags)
            .field("ops_ty", &self.ops().type_name())
            .finish()
    }
}

impl OnWakeOps for FileReleaser {
    /// Called when the underneath `FileOps` is waken up by the power framework.
    fn on_wake(&self, current_task: &CurrentTask, baton_lease: &zx::Handle) {
        // Activate associated wake leases in registered epfd.
        for epfd in self.epoll_files.lock().iter() {
            if let Ok(file) = current_task.files.get(*epfd) {
                if let Some(epoll_file) = file.downcast_file::<EpollFileObject>() {
                    if let Some(weak_handle) = self.weak_handle.upgrade() {
                        if let Err(e) =
                            epoll_file.activate_lease(current_task, &weak_handle, baton_lease)
                        {
                            log_error!("Failed to activate wake lease in epoll control file: {e}");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::fs::tmpfs::TmpFs;
    use crate::testing::*;
    use crate::vfs::buffers::{VecInputBuffer, VecOutputBuffer};
    use crate::vfs::MountInfo;
    use starnix_uapi::auth::FsCred;
    use starnix_uapi::device_type::DeviceType;
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::open_flags::OpenFlags;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use zerocopy::{FromBytes, IntoBytes, LE, U64};

    #[::fuchsia::test]
    async fn test_append_truncate_race() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let root_fs = TmpFs::new_fs(&kernel);
        let mount = MountInfo::detached();
        let root_node = Arc::clone(root_fs.root());
        let file = root_node
            .create_entry(
                &mut locked,
                &current_task,
                &mount,
                "test".into(),
                |locked, dir, mount, name| {
                    dir.mknod(
                        locked,
                        &current_task,
                        mount,
                        name,
                        FileMode::IFREG | FileMode::ALLOW_ALL,
                        DeviceType::NONE,
                        FsCred::root(),
                    )
                },
            )
            .expect("create_node failed");
        let file_handle = file
            .open_anonymous(&mut locked, &current_task, OpenFlags::APPEND | OpenFlags::RDWR)
            .expect("open failed");
        let done = Arc::new(AtomicBool::new(false));

        let fh = file_handle.clone();
        let done_clone = done.clone();
        let write_thread =
            kernel.kthreads.spawner().spawn_and_get_result(move |locked, current_task| {
                for i in 0..2000 {
                    fh.write(
                        locked,
                        current_task,
                        &mut VecInputBuffer::new(U64::<LE>::new(i).as_bytes()),
                    )
                    .expect("write failed");
                }
                done_clone.store(true, Ordering::SeqCst);
            });

        let fh = file_handle.clone();
        let done_clone = done.clone();
        let truncate_thread =
            kernel.kthreads.spawner().spawn_and_get_result(move |locked, current_task| {
                while !done_clone.load(Ordering::SeqCst) {
                    fh.ftruncate(locked, current_task, 0).expect("truncate failed");
                }
            });

        // If we read from the file, we should always find an increasing sequence. If there are
        // races, then we might unexpectedly see zeroes.
        while !done.load(Ordering::SeqCst) {
            let mut buffer = VecOutputBuffer::new(4096);
            let amount = file_handle
                .read_at(&mut locked, &current_task, 0, &mut buffer)
                .expect("read failed");
            let mut last = None;
            let buffer = &Vec::from(buffer)[..amount];
            for i in buffer.chunks_exact(8).map(|chunk| U64::<LE>::read_from_bytes(chunk).unwrap())
            {
                if let Some(last) = last {
                    assert!(i.get() > last, "buffer: {:?}", buffer);
                }
                last = Some(i.get());
            }
        }

        write_thread.await.expect("join");
        truncate_thread.await.expect("join");
    }
}
