// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::DeviceMetadata;
use crate::device::DeviceMode;
use crate::fs::sysfs::DeviceDirectory;
use crate::mm::memory::MemoryObject;
use crate::mm::{MemoryAccessor, MemoryAccessorExt};
use crate::task::CurrentTask;
use crate::vfs::{
    default_ioctl, fileops_impl_memory, fileops_impl_noop_sync, FileObject, FileOps,
    FileSystemCreator, FsNode, FsString,
};
use fuchsia_zircon as zx;
use linux_uapi::{
    ASHMEM_GET_NAME, ASHMEM_GET_PIN_STATUS, ASHMEM_GET_PROT_MASK, ASHMEM_GET_SIZE, ASHMEM_PIN,
    ASHMEM_PURGE_ALL_CACHES, ASHMEM_SET_NAME, ASHMEM_SET_PROT_MASK, ASHMEM_SET_SIZE, ASHMEM_UNPIN,
};
use once_cell::sync::OnceCell;
use starnix_logging::track_stub;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{device_type, errno, error, ASHMEM_GET_FILE_ID, ASHMEM_NAME_LEN};
use std::sync::Arc;

/// Initializes the ashmem device.
pub fn ashmem_device_init<L>(locked: &mut Locked<'_, L>, system_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let misc_class = registry.get_or_create_class("misc".into(), registry.virtual_bus());
    let ashmem_device =
        registry.register_dyn_chrdev(Ashmem::open).expect("ashmem device register failed.");

    registry.add_device(
        locked,
        system_task,
        "ashmem".into(),
        DeviceMetadata::new("ashmem".into(), ashmem_device, DeviceMode::Char),
        misc_class,
        DeviceDirectory::new,
    );
}

pub struct Ashmem {
    memory: OnceCell<Arc<MemoryObject>>,
    state: Mutex<AshmemState>,
}

#[derive(Default)]
struct AshmemState {
    size: usize,
    name: FsString,
}

impl Ashmem {
    pub fn open(
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: device_type::DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(Ashmem { memory: OnceCell::new(), state: Mutex::default() }))
    }

    fn memory(&self) -> Result<&Arc<MemoryObject>, Errno> {
        let state = self.state.lock();
        self.memory.get_or_try_init(|| {
            if state.size == 0 {
                return error!(EINVAL);
            }
            let memory =
                MemoryObject::from(zx::Vmo::create(state.size as u64).map_err(|_| errno!(ENOMEM))?);
            Ok(Arc::new(memory))
        })
    }
}

impl FileOps for Ashmem {
    fileops_impl_memory!(self, self.memory()?);
    fileops_impl_noop_sync!();

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            ASHMEM_SET_SIZE => {
                let mut state = self.state.lock();
                if self.memory.get().is_some() {
                    return error!(EINVAL);
                }
                state.size = arg.into();
                Ok(SUCCESS)
            }
            ASHMEM_GET_SIZE => Ok(self.state.lock().size.into()),

            ASHMEM_SET_NAME => {
                let name =
                    current_task.read_c_string_to_vec(arg.into(), ASHMEM_NAME_LEN as usize)?;
                self.state.lock().name = name;
                Ok(SUCCESS)
            }
            ASHMEM_GET_NAME => {
                let state = self.state.lock();
                let mut name = &state.name[..];
                if name.len() == 0 {
                    name = b"";
                }
                current_task.write_memory(arg.into(), name)?;
                Ok(SUCCESS)
            }

            ASHMEM_SET_PROT_MASK => {
                track_stub!(TODO("https://fxbug.dev/322874231"), "ASHMEM_SET_PROT_MASK");
                error!(ENOSYS)
            }
            ASHMEM_GET_PROT_MASK => {
                track_stub!(TODO("https://fxbug.dev/322874002"), "ASHMEM_GET_PROT_MASK");
                error!(ENOSYS)
            }
            ASHMEM_PIN => {
                track_stub!(TODO("https://fxbug.dev/322873842"), "ASHMEM_PIN");
                error!(ENOSYS)
            }
            ASHMEM_UNPIN => {
                track_stub!(TODO("https://fxbug.dev/322874326"), "ASHMEM_UNPIN");
                error!(ENOSYS)
            }
            ASHMEM_GET_PIN_STATUS => {
                track_stub!(TODO("https://fxbug.dev/322873280"), "ASHMEM_GET_PIN_STATUS");
                error!(ENOSYS)
            }
            ASHMEM_PURGE_ALL_CACHES => {
                track_stub!(TODO("https://fxbug.dev/322873734"), "ASHMEM_PURGE_ALL_CACHES");
                error!(ENOSYS)
            }
            ASHMEM_GET_FILE_ID => {
                track_stub!(TODO("https://fxbug.dev/322873958"), "ASHMEM_GET_FILE_ID");
                error!(ENOSYS)
            }
            _ => default_ioctl(file, locked, current_task, request, arg),
        }
    }
}
