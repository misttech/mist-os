// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::utils::{connect_to_device, ReadWriteBytesFile};
use fidl_fuchsia_hardware_qcom_hvdcpopti as fhvdcpopti;
use starnix_core::device::kobject::KObject;
use starnix_core::fs::sysfs::KObjectSymlinkDirectory;
use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter};
use starnix_core::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    DirectoryEntryType, FileObject, FileOps, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    InputBuffer, OutputBuffer, VecDirectory, VecDirectoryEntry,
};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_sync::{DeviceOpen, FileOpsCore, Locked, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{errno, error};
use std::sync::Weak;

pub const QBGIOCXCFG: u32 = 0x80684201;
pub const QBGIOCXEPR: u32 = 0x80304202;
pub const QBGIOCXEPW: u32 = 0xC0304203;
pub const QBGIOCXSTEPCHGCFG: u32 = 0xC0F74204;

pub fn create_qbg_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(QbgDeviceFile::new()))
}

pub struct QbgClassDirectory {
    base_dir: KObjectSymlinkDirectory,
}

impl QbgClassDirectory {
    pub fn new(kobject: Weak<KObject>) -> Self {
        Self { base_dir: KObjectSymlinkDirectory::new(kobject) }
    }

    fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        let mut entries = self.base_dir.create_file_ops_entries();
        entries.push(VecDirectoryEntry {
            entry_type: DirectoryEntryType::REG,
            name: b"qbg_context".into(),
            inode: None,
        });
        entries
    }
}

impl FsNodeOps for QbgClassDirectory {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(VecDirectory::new_file(self.create_file_ops_entries()))
    }

    fn lookup(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match &**name {
            b"qbg_context" => Ok(node.fs().create_node(
                current_task,
                ReadWriteBytesFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o666), FsCred::root()),
            )),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}

struct QbgDeviceFile {
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
    waiters: WaitQueue,
}

impl QbgDeviceFile {
    pub fn new() -> Self {
        Self {
            hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service"),
            waiters: WaitQueue::default(),
        }
    }
}

impl FileOps for QbgDeviceFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        let user_addr = UserAddress::from(arg);

        match request {
            QBGIOCXCFG => {
                let config =
                    self.hvdcpopti.get_config(zx::MonotonicInstant::INFINITE).map_err(|e| {
                        log_error!("GetConfig failed: {:?}", e);
                        errno!(EINVAL)
                    })?;
                current_task.write_object(UserRef::new(user_addr), &config).map_err(|e| {
                    log_error!("GetConfig write_object failed: {:?}", e);
                    e
                })?;
                Ok(SUCCESS)
            }
            QBGIOCXEPR => {
                let params = self
                    .hvdcpopti
                    .get_essential_params(zx::MonotonicInstant::INFINITE)
                    .map_err(|e| {
                        log_error!("Failed to GetEssentialParams: {:?}", e);
                        errno!(EINVAL)
                    })?
                    .map_err(|e| {
                        log_error!("GetEssentialParams failed: {:?}", e);
                        errno!(EINVAL)
                    })?;
                current_task.write_object(UserRef::new(user_addr), &params).map_err(|e| {
                    log_error!("GetEssentialParams write_object failed: {:?}", e);
                    e
                })?;
                Ok(SUCCESS)
            }
            QBGIOCXEPW => {
                let params: [u8; fhvdcpopti::ESSENTIAL_PARAMS_LENGTH as usize] =
                    current_task.read_object(UserRef::new(user_addr)).map_err(|e| {
                        log_error!("SetEssentialParams read_object failed: {:?}", e);
                        e
                    })?;
                self.hvdcpopti
                    .set_essential_params(&params, zx::MonotonicInstant::INFINITE)
                    .map_err(|e| {
                        log_error!("Failed to SetEssentialParams: {:?}", e);
                        errno!(EINVAL)
                    })?
                    .map_err(|e| {
                        log_error!("SetEssentialParams failed: {:?}", e);
                        errno!(EINVAL)
                    })?;
                Ok(SUCCESS)
            }
            QBGIOCXSTEPCHGCFG => {
                let params = self
                    .hvdcpopti
                    .get_step_and_jeita_params(zx::MonotonicInstant::INFINITE)
                    .map_err(|e| {
                        log_error!("GetStepAndJeitaParams failed: {:?}", e);
                        errno!(EINVAL)
                    })?;
                current_task.write_object(UserRef::new(user_addr), &params).map_err(|e| {
                    log_error!("GetStepAndJeitaParams write_object failed: {:?}", e);
                    e
                })?;
                Ok(SUCCESS)
            }
            unknown_ioctl => {
                track_stub!(TODO("https://fxbug.dev/322874368"), "qbg ioctl", unknown_ioctl);
                error!(ENOSYS)
            }
        }?;

        Ok(SUCCESS)
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        buffer: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        // Only used by qbg daemon and is not opened with O_NONBLOCK
        if file.flags().contains(OpenFlags::NONBLOCK) {
            log_error!("Non-blocking reads are not supported");
            return error!(EINVAL);
        }

        let fhvdcpopti::DeviceEvent::OnFifoData { data } =
            self.hvdcpopti.wait_for_event(zx::MonotonicInstant::INFINITE).map_err(|e| {
                log_error!("Failed ot receive event: {:?}", e);
                errno!(EINVAL)
            })?;

        if data.len() > buffer.available() {
            log_warn!("Buffer too small");
            // This means the data will only be partially written, with the rest discarded.
            // Instead of attempting this, we'll instead return error to the client.
            return error!(EINVAL);
        }

        buffer.write(&data)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        buffer: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let data = buffer.read_all()?;
        let data_len = data.len();

        self.hvdcpopti
            .set_processed_fifo_data(&data.try_into().unwrap(), zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!("SetProcessedFifoData failed: {:?}", e);
                errno!(EINVAL)
            })?;
        Ok(data_len)
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.waiters.wait_async_fd_events(waiter, events, handler))
    }
}
