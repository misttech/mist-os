// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{kobject::DeviceMetadata, DeviceMode, DeviceOps},
    fs::sysfs::DeviceDirectory,
    mm::ProtectionFlags,
    task::CurrentTask,
    vfs::{
        default_ioctl, fileops_impl_seekable, FileObject, FileOps, FsNode, InputBuffer,
        OutputBuffer, VmoFileOperation,
    },
};
use fuchsia_zircon as zx;
use linux_uapi::{
    ASHMEM_GET_NAME, ASHMEM_GET_PIN_STATUS, ASHMEM_GET_PROT_MASK, ASHMEM_GET_SIZE, ASHMEM_PIN,
    ASHMEM_PURGE_ALL_CACHES, ASHMEM_SET_NAME, ASHMEM_SET_PROT_MASK, ASHMEM_SET_SIZE, ASHMEM_UNPIN,
};
use starnix_syscalls::{SyscallArg, SyscallResult};
use starnix_uapi::{device_type, errno, error, errors::Errno, open_flags::OpenFlags};
use std::sync::Arc;

/// Initializes the ashmem device.
pub fn ashmem_device_init(system_task: &CurrentTask) {
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let misc_class = registry.get_or_create_class(b"misc", registry.virtual_bus());
    let ashmem_device =
        registry.register_dyn_chrdev(AshmemDevice {}).expect("ashmem device register failed.");

    registry.add_device(
        system_task,
        b"ashmem",
        DeviceMetadata::new(b"ashmem", ashmem_device, DeviceMode::Char),
        misc_class,
        DeviceDirectory::new,
    );
}

#[derive(Clone)]
struct AshmemDevice {}

impl DeviceOps for AshmemDevice {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _id: device_type::DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(AshmemFileObject::new()?))
    }
}

struct AshmemFileObject {
    vmo: Arc<zx::Vmo>,
}

impl AshmemFileObject {
    pub fn new() -> Result<Self, Errno> {
        let vmo =
            zx::Vmo::create_with_opts(zx::VmoOptions::RESIZABLE, 0).map_err(|_| errno!(ENOMEM))?;
        let vmo = Arc::new(vmo);
        Ok(AshmemFileObject { vmo })
    }
}

impl FileOps for AshmemFileObject {
    fileops_impl_seekable!();

    fn read(
        &self,
        file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileOperation::read(&self.vmo, file, offset, data)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        VmoFileOperation::write(&self.vmo, file, current_task, offset, data)
    }

    fn get_vmo(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        _length: Option<usize>,
        prot: ProtectionFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        VmoFileOperation::get_vmo(&self.vmo, file, current_task, prot)
    }

    /// Implements ioctl.
    fn ioctl(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            ASHMEM_GET_NAME
            | ASHMEM_GET_PIN_STATUS
            | ASHMEM_GET_PROT_MASK
            | ASHMEM_GET_SIZE
            | ASHMEM_PIN
            | ASHMEM_PURGE_ALL_CACHES
            | ASHMEM_SET_NAME
            | ASHMEM_SET_PROT_MASK
            | ASHMEM_SET_SIZE
            | ASHMEM_UNPIN => {
                error!(EINVAL)
            }
            _ => default_ioctl(file, current_task, request, arg),
        }
    }
}
