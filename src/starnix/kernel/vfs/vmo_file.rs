// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{vmo::round_up_to_system_page_size, ProtectionFlags, PAGE_SIZE, VMEX_RESOURCE},
    signals::{send_standard_signal, SignalInfo},
    task::CurrentTask,
    vfs::{
        anon_fs,
        buffers::{InputBuffer, OutputBuffer},
        fs_node_impl_not_dir, fs_node_impl_xattr_delegate, DirEntry, FallocMode, FileHandle,
        FileObject, FileOps, FsNode, FsNodeInfo, FsNodeOps, FsString, MemoryXattrStorage,
        MountInfo, NamespaceNode, MAX_LFS_FILESIZE,
    },
};
use fidl::HandleBased;
use fuchsia_zircon as zx;
use starnix_logging::{impossible_error, track_stub};
use starnix_sync::{DeviceOpen, FileOpsCore, FsNodeAllocate, LockBefore, Locked};
use starnix_uapi::{
    errno, error, errors::Errno, file_mode::mode, open_flags::OpenFlags, resource_limits::Resource,
    seal_flags::SealFlags, signals::SIGXFSZ,
};
use std::sync::Arc;

pub struct VmoFileNode {
    /// The memory that backs this file.
    vmo: Arc<zx::Vmo>,
    xattrs: MemoryXattrStorage,
}

impl VmoFileNode {
    /// Create a new writable file node based on a blank VMO.
    pub fn new() -> Result<Self, Errno> {
        let vmo =
            zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, 0).map_err(|_| errno!(ENOMEM))?;
        Ok(Self { vmo: Arc::new(vmo), xattrs: MemoryXattrStorage::default() })
    }

    /// Create a new file node based on an existing VMO.
    /// Attempts to open the file for writing will fail unless [`vmo`] has both
    /// the `WRITE` and `RESIZE` rights.
    pub fn from_vmo(vmo: Arc<zx::Vmo>) -> Self {
        Self { vmo, xattrs: MemoryXattrStorage::default() }
    }
}

impl FsNodeOps for VmoFileNode {
    fs_node_impl_not_dir!();
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn initial_info(&self, info: &mut FsNodeInfo) {
        if let Some(size) = self.vmo.get_content_size().ok() {
            info.size = size as usize;
        }
    }

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        if flags.contains(OpenFlags::TRUNC) {
            // Truncating to zero length must pass the shrink seal check.
            node.write_guard_state.lock().check_no_seal(SealFlags::SHRINK)?;
        }

        // Produce a VMO handle with rights reduced to those requested in |flags|.
        // TODO(b/319240806): Accumulate required rights, rather than starting from `DEFAULT`.
        let mut desired_rights = zx::Rights::VMO_DEFAULT | zx::Rights::RESIZE;
        if !flags.can_read() {
            desired_rights.remove(zx::Rights::READ);
        }
        if !flags.can_write() {
            desired_rights.remove(zx::Rights::WRITE | zx::Rights::RESIZE);
        }
        let scoped_vmo =
            Arc::new(self.vmo.duplicate_handle(desired_rights).map_err(|_e| errno!(EIO))?);
        let file_object = VmoFileObject::new(scoped_vmo);

        Ok(Box::new(file_object))
    }

    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        _current_task: &CurrentTask,
        length: u64,
    ) -> Result<(), Errno> {
        let length = length as usize;

        node.update_info(|info| {
            if info.size == length {
                // The file size remains unaffected.
                return Ok(());
            }

            // We must hold the lock till the end of the operation to guarantee that
            // there is no change to the seals.
            let state = node.write_guard_state.lock();

            if info.size > length {
                // A decrease in file size must pass the shrink seal check.
                state.check_no_seal(SealFlags::SHRINK)?;
            } else {
                // An increase in file size must pass the grow seal check.
                state.check_no_seal(SealFlags::GROW)?;
            }

            let vmo_size = update_vmo_file_size(&self.vmo, info, length)?;
            info.size = length;

            // Zero unused parts of the VMO.
            if vmo_size > length {
                self.vmo
                    .op_range(zx::VmoOp::ZERO, length as u64, (vmo_size - length) as u64)
                    .map_err(impossible_error)?;
            }

            Ok(())
        })
    }

    fn allocate(
        &self,
        _locked: &mut Locked<'_, FsNodeAllocate>,
        node: &FsNode,
        _current_task: &CurrentTask,
        mode: FallocMode,
        offset: u64,
        length: u64,
    ) -> Result<(), Errno> {
        match mode {
            FallocMode::PunchHole => {
                // Lock `info()` before acquiring the `write_guard_state` lock to ensure consistent
                // lock ordering.
                let info = node.info();

                // Check write seal. Hold the lock to ensure seals don't change.
                let state = node.write_guard_state.lock();
                state.check_no_seal(SealFlags::WRITE | SealFlags::FUTURE_WRITE)?;

                let mut end = offset.checked_add(length).ok_or_else(|| errno!(EINVAL))? as usize;

                let vmo_size = info.blksize * info.blocks;
                if offset as usize >= vmo_size {
                    return Ok(());
                }

                // If punching hole at the end of the file then zero all the
                // way to the end of the VMO to avoid keeping any pages for the tail.
                if end >= info.size {
                    end = vmo_size;
                }

                self.vmo
                    .op_range(zx::VmoOp::ZERO, offset, end as u64 - offset)
                    .map_err(impossible_error)?;

                Ok(())
            }

            FallocMode::Allocate { keep_size } => {
                node.update_info(|info| {
                    let new_size = (offset + length) as usize;
                    if new_size > info.size {
                        // Check GROW seal (even with `keep_size=true`). Hold the lock to ensure
                        // seals don't change.
                        let state = node.write_guard_state.lock();
                        state.check_no_seal(SealFlags::GROW)?;

                        update_vmo_file_size(&self.vmo, info, new_size)?;

                        if !keep_size {
                            info.size = new_size;
                        }
                    }
                    Ok(())
                })
            }

            _ => error!(EOPNOTSUPP),
        }
    }
}

pub struct VmoFileObject {
    pub vmo: Arc<zx::Vmo>,
}

impl VmoFileObject {
    /// Create a file object based on a VMO.
    pub fn new(vmo: Arc<zx::Vmo>) -> Self {
        VmoFileObject { vmo }
    }
}

impl VmoFileObject {
    pub fn read(
        vmo: &zx::Vmo,
        file: &FileObject,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let actual = {
            let info = file.node().info();
            let file_length = info.size;
            let want_read = data.available();
            if offset < file_length {
                let to_read =
                    if file_length < offset + want_read { file_length - offset } else { want_read };
                let buf =
                    vmo.read_to_vec(offset as u64, to_read as u64).map_err(|_| errno!(EIO))?;
                drop(info);
                data.write_all(&buf[..])?;
                to_read
            } else {
                0
            }
        };
        Ok(actual)
    }

    pub fn write(
        vmo: &zx::Vmo,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let mut want_write = data.available();
        let buf = data.peek_all()?;

        file.node().update_info(|info| {
            let mut write_end = offset + want_write;
            let mut update_content_size = false;

            // We must hold the lock till the end of the operation to guarantee that
            // there is no change to the seals.
            let state = file.name.entry.node.write_guard_state.lock();

            // Non-zero writes must pass the write seal check.
            if want_write != 0 {
                state.check_no_seal(SealFlags::WRITE | SealFlags::FUTURE_WRITE)?;
            }

            // Writing past the file size
            if write_end > info.size {
                // The grow seal check failed.
                if let Err(e) = state.check_no_seal(SealFlags::GROW) {
                    if offset >= info.size {
                        // Write starts outside the file.
                        // Forbid because nothing can be written without growing.
                        return Err(e);
                    } else if info.size == info.storage_size() {
                        // Write starts inside file and EOF page does not need to grow.
                        // End write at EOF.
                        write_end = info.size;
                        want_write = write_end - offset;
                    } else {
                        // Write starts inside file and EOF page needs to grow.
                        let eof_page_start = info.storage_size() - (*PAGE_SIZE as usize);

                        if offset >= eof_page_start {
                            // Write starts in EOF page.
                            // Forbid because EOF page cannot grow.
                            return Err(e);
                        }

                        // End write at page before EOF.
                        write_end = eof_page_start;
                        want_write = write_end - offset;
                    }
                }
            }

            // Check against the FSIZE limt
            let fsize_limit = current_task.thread_group.get_rlimit(Resource::FSIZE) as usize;
            if write_end > fsize_limit {
                if offset >= fsize_limit {
                    // Write starts beyond the FSIZE limt.
                    send_standard_signal(current_task, SignalInfo::default(SIGXFSZ));
                    return error!(EFBIG);
                }

                // End write at FSIZE limit.
                write_end = fsize_limit;
                want_write = write_end - offset;
            }

            if write_end > info.size {
                if write_end > info.storage_size() {
                    update_vmo_file_size(vmo, info, write_end)?;
                }
                update_content_size = true;
            }
            vmo.write(&buf[..want_write], offset as u64).map_err(|_| errno!(EIO))?;

            if update_content_size {
                info.size = write_end;
            }
            data.advance(want_write)?;
            Ok(want_write)
        })
    }

    pub fn get_vmo(
        vmo: &Arc<zx::Vmo>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        let mut vmo = Arc::clone(vmo);
        if prot.contains(ProtectionFlags::EXEC) {
            vmo = Arc::new(
                vmo.duplicate_handle(zx::Rights::SAME_RIGHTS)
                    .map_err(impossible_error)?
                    .replace_as_executable(&VMEX_RESOURCE)
                    .map_err(impossible_error)?,
            );
        }
        Ok(vmo)
    }
}

#[macro_export]
macro_rules! fileops_impl_vmo {
    ($self:ident, $vmo:expr) => {
        $crate::fileops_impl_seekable!();

        fn read(
            &$self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::FileOpsCore>,
            file: &$crate::vfs::FileObject,
            _current_task: &$crate::task::CurrentTask,
            offset: usize,
            data: &mut dyn $crate::vfs::buffers::OutputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            $crate::vfs::VmoFileObject::read($vmo, file, offset, data)
        }

        fn write(
            &$self,
            _locked: &mut starnix_sync::Locked<'_, starnix_sync::WriteOps>,
            file: &$crate::vfs::FileObject,
            current_task: &$crate::task::CurrentTask,
            offset: usize,
            data: &mut dyn $crate::vfs::buffers::InputBuffer,
        ) -> Result<usize, starnix_uapi::errors::Errno> {
            $crate::vfs::VmoFileObject::write($vmo, file, current_task, offset, data)
        }

        fn get_vmo(
            &$self,
            file: &FileObject,
            current_task: &CurrentTask,
            _length: Option<usize>,
            prot: $crate::mm::ProtectionFlags,
        ) -> Result<Arc<fuchsia_zircon::Vmo>, starnix_uapi::errors::Errno> {
            $crate::vfs::VmoFileObject::get_vmo($vmo, file, current_task, prot)
        }
    }
}
pub(crate) use fileops_impl_vmo;

impl FileOps for VmoFileObject {
    fileops_impl_vmo!(self, &self.vmo);

    fn readahead(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _length: usize,
    ) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/42082608"), "paged VMO readahead");
        Ok(())
    }
}

pub fn new_memfd<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    mut name: FsString,
    seals: SealFlags,
    flags: OpenFlags,
) -> Result<FileHandle, Errno>
where
    L: LockBefore<FileOpsCore>,
    L: LockBefore<DeviceOpen>,
{
    let fs = anon_fs(current_task.kernel());
    let node = fs.create_node(
        current_task,
        VmoFileNode::new()?,
        FsNodeInfo::new_factory(mode!(IFREG, 0o600), current_task.as_fscred()),
    );
    node.write_guard_state.lock().enable_sealing(seals);

    let ops = node.open(locked, current_task, &MountInfo::detached(), flags, false)?;

    // In /proc/[pid]/fd, the target of this memfd's symbolic link is "/memfd:[name]".
    let mut local_name = FsString::from("/memfd:");
    local_name.append(&mut name);

    let name = NamespaceNode::new_anonymous(DirEntry::new(node, None, local_name));

    FileObject::new(ops, name, flags)
}

/// Sets VMO size to `min_size` rounded to whole pages. Returns the new size of the VMO in bytes.
fn update_vmo_file_size(
    vmo: &zx::Vmo,
    node_info: &mut FsNodeInfo,
    requested_size: usize,
) -> Result<usize, Errno> {
    assert!(requested_size <= MAX_LFS_FILESIZE);
    let size = round_up_to_system_page_size(requested_size)?;
    vmo.set_size(size as u64).map_err(|status| match status {
        zx::Status::NO_MEMORY => errno!(ENOMEM),
        zx::Status::OUT_OF_RANGE => errno!(ENOMEM),
        _ => impossible_error(status),
    })?;
    node_info.blocks = size / node_info.blksize;
    Ok(size)
}
