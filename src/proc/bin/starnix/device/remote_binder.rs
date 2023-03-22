// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceOps;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::{FdEvents, FileObject, FileOps, FsNode, NamespaceNode, SeekOrigin};
use crate::mm::{DesiredAddress, MappedVmo, MappingOptions};
use crate::syscalls::{SyscallResult, SUCCESS};
use crate::task::{CurrentTask, Waiter};
use crate::types::{error, off_t, uapi, DeviceType, Errno, OpenFlags, UserAddress};
use fuchsia_zircon as zx;
use std::sync::Arc;

/// Device for starting a remote fuchsia component with access to the binder drivers on the starnix
/// container.
pub struct RemoteBinderDevice {}

impl DeviceOps for RemoteBinderDevice {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(RemoteBinderConnection {}))
    }
}

/// Connection to the remote binder device. One connection is associated to one instance of a
/// remote fuchsia component.
struct RemoteBinderConnection {}

impl RemoteBinderConnection {
    /// Implementation of the REMOTE_BINDER_START ioctl.
    fn start(&self) -> Result<(), Errno> {
        // TODO(qsr): Implement
        error!(EINTR)
    }

    /// Implementation of the REMOTE_BINDER_WAIT ioctl.
    fn wait(&self, current_task: &CurrentTask) -> Result<(), Errno> {
        // TODO(qsr): Implement
        let waiter = Waiter::new();
        waiter.wait(current_task)
    }
}

impl FileOps for RemoteBinderConnection {
    fn query_events(&self, _current_task: &CurrentTask) -> FdEvents {
        FdEvents::empty()
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        _user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        match request {
            uapi::REMOTE_BINDER_START => self.start()?,
            uapi::REMOTE_BINDER_WAIT => self.wait(current_task)?,
            _ => return error!(ENOTSUP),
        }
        Ok(SUCCESS)
    }

    fn get_vmo(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _length: Option<usize>,
        _prot: zx::VmarFlags,
    ) -> Result<Arc<zx::Vmo>, Errno> {
        error!(EOPNOTSUPP)
    }

    fn mmap(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _addr: DesiredAddress,
        _vmo_offset: u64,
        _length: usize,
        _flags: zx::VmarFlags,
        _mapping_options: MappingOptions,
        _filename: NamespaceNode,
    ) -> Result<MappedVmo, Errno> {
        error!(EOPNOTSUPP)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EOPNOTSUPP)
    }

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: off_t,
        _whence: SeekOrigin,
    ) -> Result<off_t, Errno> {
        error!(EOPNOTSUPP)
    }

    fn read_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EOPNOTSUPP)
    }

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EOPNOTSUPP)
    }
}
