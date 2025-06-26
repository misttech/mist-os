// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::dma_heap_allocation_data;
use starnix_core::device::DeviceOps;
use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{default_ioctl, FdFlags, FdNumber, FileObject, FileOps, FsNode};
use starnix_core::{fileops_impl_dataless, fileops_impl_noop_sync, fileops_impl_seekless};
use starnix_logging::log_debug;
use starnix_sync::{DeviceOpen, Locked, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserRef;
use std::sync::Arc;

pub trait Alloc: Send + Sync + 'static {
    fn alloc(
        &self,
        current_task: &CurrentTask,
        locked: &mut Locked<Unlocked>,
        size: u64,
        fd_flags: FdFlags,
    ) -> Result<FdNumber, Errno>;
}

struct DmaHeapFile<A: Alloc> {
    allocator: Arc<A>,
}

impl<A: Alloc> DmaHeapFile<A> {
    fn new(allocator: Arc<A>) -> Self {
        Self { allocator }
    }
}

impl<A: Alloc> FileOps for DmaHeapFile<A> {
    fileops_impl_noop_sync!();
    fileops_impl_dataless!();
    fileops_impl_seekless!();

    fn ioctl(
        &self,
        locked: &mut Locked<Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match crate::canonicalize_ioctl_request(current_task, request) {
            linux_uapi::DMA_HEAP_IOCTL_ALLOC => {
                // UserRef note:
                // dma_heap_allocation_data is checked for check_arch_independent_layout.
                let user_info = UserRef::<dma_heap_allocation_data>::from(arg);
                let info = current_task.read_object(user_info)?;
                log_debug!("DmaHeapFile ioctl alloc {:?}", info);
                let fd = self.allocator.alloc(current_task, locked, info.len, FdFlags::CLOEXEC)?;
                let to_user = dma_heap_allocation_data {
                    len: info.len,
                    fd: fd.raw() as u32,
                    fd_flags: info.fd_flags,
                    heap_flags: info.heap_flags,
                };
                current_task.write_object(user_info, &to_user)?;
                Ok(SUCCESS)
            }
            _ => default_ioctl(file, locked, current_task, request, arg),
        }
    }
}

struct DmaHeapDevice<A: Alloc> {
    allocator: Arc<A>,
}

impl<A: Alloc> DmaHeapDevice<A> {
    fn new(allocator: A) -> Self {
        Self { allocator: Arc::new(allocator) }
    }
}

impl<A: Alloc> Clone for DmaHeapDevice<A> {
    fn clone(&self) -> Self {
        Self { allocator: self.allocator.clone() }
    }
}

impl<A: Alloc> DeviceOps for DmaHeapDevice<A> {
    fn open(
        &self,
        _locked: &mut Locked<DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(DmaHeapFile::new(self.allocator.clone())))
    }
}

// This module can eventually live in starnix/kernel if it needs to be shared by other modules.
// For now it's only used by the fastrpc module so they are alongside each other.
pub fn dma_heap_device_register<A: Alloc>(
    locked: &mut Locked<Unlocked>,
    current_task: &CurrentTask,
    name: &str,
    allocator: A,
) {
    let device = DmaHeapDevice::new(allocator);
    let registry = &current_task.kernel().device_registry;
    registry
        .register_dyn_device_with_devname(
            locked,
            current_task,
            name.into(),
            format!("dma_heap/{}", name).as_str().into(),
            registry.objects.dma_heap_class(),
            device,
        )
        .expect("Can register heap device");
}
