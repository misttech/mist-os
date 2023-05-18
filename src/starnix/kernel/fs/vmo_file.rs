// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, HandleBased};
use std::sync::Arc;

use super::*;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::lock::Mutex;
use crate::logging::impossible_error;
use crate::mm::vmo::round_up_to_system_page_size;
use crate::mm::{ProtectionFlags, PAGE_SIZE};
use crate::syscalls::{SyscallResult, SUCCESS};
use crate::task::CurrentTask;
use crate::types::*;
use crate::vmex_resource::VMEX_RESOURCE;

pub struct VmoFileNode {
    /// The memory that backs this file.
    vmo: Arc<zx::Vmo>,
    xattrs: MemoryXattrStorage,

    /// Optional because not all VMO file nodes support sealing.
    /// Seals are applied to a node, not to individual file objects.
    seals: Option<SealFlagsHandle>,
}

impl VmoFileNode {
    /// Create a new file node based on a blank VMO.
    /// The `seal_flags` determine the seals set for this node at creation.
    pub fn new(seals: SealFlags) -> Result<VmoFileNode, Errno> {
        let vmo =
            zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, 0).map_err(|_| errno!(ENOMEM))?;
        Ok(VmoFileNode {
            vmo: Arc::new(vmo),
            xattrs: MemoryXattrStorage::default(),
            seals: Some(Arc::new(Mutex::new(seals))),
        })
    }

    /// Create a file node from byte data. This assumes that the node does not support
    /// sealing via `fcntl` syscall.
    pub fn from_bytes(data: &[u8]) -> Result<VmoFileNode, Errno> {
        let vmo = zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, data.len() as u64)
            .map_err(|_| errno!(ENOMEM))?;
        vmo.write(data, 0).map_err(|_| errno!(ENOMEM))?;
        Ok(VmoFileNode { vmo: Arc::new(vmo), xattrs: MemoryXattrStorage::default(), seals: None })
    }
}

impl FsNodeOps for VmoFileNode {
    fs_node_impl_xattr_delegate!(self, self.xattrs);

    fn create_file_ops(&self, _node: &FsNode, flags: OpenFlags) -> Result<Box<dyn FileOps>, Errno> {
        // Produce a VMO handle with rights reduced to those requested in |flags|.
        // self.vmo has the default VMO object rights plus the RESIZE as we create it with zx::VmoOptions::RESIZABLE.
        let mut desired_rights = zx::Rights::VMO_DEFAULT | zx::Rights::RESIZE;
        if !flags.can_read() {
            desired_rights.remove(zx::Rights::READ);
        }
        if !flags.can_write() {
            desired_rights.remove(zx::Rights::WRITE);
        }
        let scoped_vmo =
            Arc::new(self.vmo.duplicate_handle(desired_rights).map_err(|_e| errno!(EIO))?);

        let file_object = if let Some(seals) = &self.seals {
            {
                let seals = seals.lock();
                if flags.contains(OpenFlags::TRUNC) {
                    // Truncating to zero length must pass the shrink seal check.
                    seals.check_not_present(SealFlags::SHRINK)?;
                }
            }
            VmoFileObject::new_with_seals(scoped_vmo, seals.clone())
        } else {
            VmoFileObject::new(scoped_vmo)
        };

        Ok(Box::new(file_object))
    }

    fn truncate(&self, node: &FsNode, length: u64) -> Result<(), Errno> {
        let mut info = node.info_write();
        if info.size == length as usize {
            // The file size remains unaffected.
            return Ok(());
        }

        // We must hold the lock till the end of the operation to guarantee that
        // there is no change to the seals.
        let seals = self.seals.as_ref().map(|s| s.lock());

        if let Some(seals) = &seals {
            if info.size > length as usize {
                // A decrease in file size must pass the shrink seal check.
                seals.check_not_present(SealFlags::SHRINK)?;
            } else {
                // An increase in file size must pass the grow seal check.
                seals.check_not_present(SealFlags::GROW)?;
            }
        }

        self.vmo.set_size(length).map_err(|status| match status {
            zx::Status::NO_MEMORY => errno!(ENOMEM),
            zx::Status::OUT_OF_RANGE => errno!(ENOMEM),
            _ => impossible_error(status),
        })?;
        info.size = length as usize;
        info.storage_size = self.vmo.get_size().map_err(impossible_error)? as usize;
        Ok(())
    }

    fn allocate(&self, node: &FsNode, offset: u64, length: u64) -> Result<(), Errno> {
        let new_size = offset + length;

        let mut info = node.info_write();
        if info.size >= new_size as usize {
            // The file size remains unaffected.
            return Ok(());
        }

        // We must hold the lock till the end of the operation to guarantee that
        // there is no change to the seals.
        let seals = self.seals.as_ref().map(|s| s.lock());

        if let Some(seals) = &seals {
            // An increase in file size must pass the grow seal check.
            seals.check_not_present(SealFlags::GROW)?;
        }

        self.vmo.set_size(new_size).map_err(|status| match status {
            zx::Status::NO_MEMORY => errno!(ENOMEM),
            zx::Status::OUT_OF_RANGE => errno!(ENOMEM),
            _ => impossible_error(status),
        })?;
        info.size = new_size as usize;
        info.storage_size = self.vmo.get_size().map_err(impossible_error)? as usize;
        Ok(())
    }
}

pub struct VmoFileObject {
    pub vmo: Arc<zx::Vmo>,
    seals: Option<SealFlagsHandle>,
}

impl VmoFileObject {
    /// Create a file object based on a VMO, assuming that sealing is unsupported.
    pub fn new(vmo: Arc<zx::Vmo>) -> Self {
        VmoFileObject { vmo, seals: None }
    }

    /// Create a file object based on a VMO, with the given seal state.
    pub fn new_with_seals(vmo: Arc<zx::Vmo>, seals: SealFlagsHandle) -> Self {
        VmoFileObject { vmo, seals: Some(seals) }
    }

    pub fn read_at(
        vmo: &zx::Vmo,
        file: &FileObject,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let actual = {
            let info = file.node().info();
            let file_length = info.size;
            let want_read = data.available();
            if want_read > MAX_LFS_FILESIZE - offset {
                return error!(EINVAL);
            }
            if offset < file_length {
                let to_read =
                    if file_length < offset + want_read { file_length - offset } else { want_read };
                let mut buf = vec![0u8; to_read];
                vmo.read(&mut buf[..], offset as u64).map_err(|_| errno!(EIO))?;
                drop(info);
                data.write_all(&buf[..])?;
                to_read
            } else {
                0
            }
        };
        Ok(actual)
    }

    pub fn write_at(
        vmo: &zx::Vmo,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
        seals: Option<&SealFlagsHandle>,
    ) -> Result<usize, Errno> {
        let mut want_write = data.available();
        if want_write > MAX_LFS_FILESIZE - offset {
            return error!(EINVAL);
        }

        let buf = data.read_all()?;

        let mut info = file.node().info_write();
        let mut write_end = offset + want_write;
        let mut update_content_size = false;

        // We must hold the lock till the end of the operation to guarantee that
        // there is no change to the seals.
        let seals = seals.map(|s| s.lock());

        // Non-zero writes must pass the write seal check.
        if want_write != 0 {
            if let Some(seals) = &seals {
                seals.check_not_present(SealFlags::WRITE | SealFlags::FUTURE_WRITE)?;
            }
        }

        // Writing past the file size
        if write_end > info.size {
            if let Some(seals) = &seals {
                // The grow seal check failed.
                if let Err(e) = seals.check_not_present(SealFlags::GROW) {
                    if offset >= info.size {
                        // Write starts outside the file.
                        // Forbid because nothing can be written without growing.
                        return Err(e);
                    } else if info.size == info.storage_size {
                        // Write starts inside file and EOF page does not need to grow.
                        // End write at EOF.
                        write_end = info.size;
                        want_write = write_end - offset;
                    } else {
                        // Write starts inside file and EOF page needs to grow.
                        let eof_page_start = info.storage_size - (*PAGE_SIZE as usize);

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
        }

        if write_end > info.size {
            if write_end > info.storage_size {
                let new_size = round_up_to_system_page_size(write_end)?;
                vmo.set_size(new_size as u64).map_err(|_| errno!(ENOMEM))?;
                info.storage_size = new_size;
            }
            update_content_size = true;
        }
        vmo.write(&buf[..want_write], offset as u64).map_err(|_| errno!(EIO))?;

        if update_content_size {
            info.size = write_end;
        }
        Ok(want_write)
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

impl FileOps for VmoFileObject {
    fileops_impl_seekable!();

    fn read_at(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileObject::read_at(&self.vmo, file, offset, data)
    }

    fn write_at(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileObject::write_at(&self.vmo, file, current_task, offset, data, self.seals.as_ref())
    }

    fn get_vmo(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        VmoFileObject::get_vmo(&self.vmo, file, current_task, prot)
    }

    fn fcntl(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        cmd: u32,
        arg: u64,
    ) -> Result<SyscallResult, Errno> {
        match cmd {
            F_ADD_SEALS => {
                let seals = self.seals.as_ref().ok_or(errno!(EINVAL))?;
                let mut seals = seals.lock();

                let flags = SealFlags::from_bits_truncate(arg as u32);

                if !file.can_write() {
                    // Cannot add seals if the file is not writable
                    return error!(EPERM);
                }

                seals.try_add_seal(flags)?;
                Ok(SUCCESS)
            }
            F_GET_SEALS => {
                let seals = self.seals.as_ref().ok_or(errno!(EINVAL))?;
                let seals = seals.lock();
                Ok((*seals).into())
            }
            _ => default_fcntl(cmd),
        }
    }
}

pub fn new_memfd(
    current_task: &CurrentTask,
    mut name: FsString,
    seals: SealFlags,
    flags: OpenFlags,
) -> Result<FileHandle, Errno> {
    let fs = anon_fs(current_task.kernel());
    let node =
        fs.create_node(VmoFileNode::new(seals)?, mode!(IFREG, 0o600), current_task.as_fscred());

    let ops = node.open(current_task, flags, false)?;

    // In /proc/[pid]/fd, the target of this memfd's symbolic link is "/memfd:[name]".
    let mut local_name = b"/memfd:".to_vec();
    local_name.append(&mut name);

    let name = NamespaceNode::new_anonymous(DirEntry::new(node, None, local_name));

    Ok(FileObject::new(ops, name, flags))
}
