// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::utils::{connect_to_device, connect_to_device_async, ReadWriteBytesFile};
use fidl_fuchsia_hardware_qcom_hvdcpopti as fhvdcpopti;
use futures_util::StreamExt;
use starnix_core::device::kobject::KObject;
use starnix_core::fs::sysfs::KObjectSymlinkDirectory;
use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::{CurrentTask, EventHandler, WaitCanceler, WaitQueue, Waiter};
use starnix_core::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    DirectoryEntryType, FileObject, FileOps, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    InputBuffer, OutputBuffer, VecDirectory, VecDirectoryEntry, VecInputBuffer,
};
use starnix_logging::{log_error, log_warn, track_stub};
use starnix_sync::{DeviceOpen, FileOpsCore, Locked, Mutex, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{errno, error};
use std::collections::VecDeque;
use std::sync::{Arc, Weak};

pub const QBGIOCXCFG: u32 = 0x80684201;
pub const QBGIOCXEPR: u32 = 0x80304202;
pub const QBGIOCXEPW: u32 = 0xC0304203;
pub const QBGIOCXSTEPCHGCFG: u32 = 0xC0F74204;

pub fn create_qbg_device(
    _locked: &mut Locked<'_, DeviceOpen>,
    current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(QbgDeviceFile::new(current_task)))
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

struct QbgDeviceState {
    waiters: WaitQueue,
    read_queue: Mutex<VecDeque<VecInputBuffer>>,
}

impl QbgDeviceState {
    fn new() -> Self {
        Self { waiters: WaitQueue::default(), read_queue: Mutex::new(VecDeque::new()) }
    }

    fn handle_event(&self, event: fhvdcpopti::DeviceEvent) {
        let fhvdcpopti::DeviceEvent::OnFifoData { data } = event;
        self.read_queue.lock().push_back(data.into());
        self.waiters.notify_fd_events(FdEvents::POLLIN);
    }
}

async fn run_qbg_device_event_loop(
    device_state: Arc<QbgDeviceState>,
    mut event_stream: fhvdcpopti::DeviceEventStream,
) {
    loop {
        match event_stream.next().await {
            Some(Ok(event)) => {
                device_state.handle_event(event);
            }
            Some(Err(e)) => {
                log_error!("qbg: Received error from device event stream: {}", e);
                break;
            }
            None => {
                log_warn!("qbg: Exhausted device event stream");
                break;
            }
        }
    }

    device_state.waiters.notify_fd_events(FdEvents::POLLHUP);
}

fn spawn_qbg_device_tasks(device_state: Arc<QbgDeviceState>, current_task: &CurrentTask) {
    current_task.kernel().kthreads.spawn_future(async move {
        // Connect to the device on the main thread. The thread from which the task is being
        // spawned does not yet have an executor, so it cannot make an async FIDL connection.
        let proxy = connect_to_device_async().expect("Could not connect to hvdcpopti service");
        run_qbg_device_event_loop(device_state, proxy.take_event_stream()).await;
    });
}

struct QbgDeviceFile {
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
    state: Arc<QbgDeviceState>,
}

impl QbgDeviceFile {
    pub fn new(current_task: &CurrentTask) -> Self {
        let state = Arc::new(QbgDeviceState::new());
        spawn_qbg_device_tasks(state.clone(), current_task);
        Self {
            hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service"),
            state,
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
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        buffer: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            let mut queue = self.state.read_queue.lock();
            if queue.is_empty() {
                return error!(EAGAIN);
            }

            // Try and pull as much data from the queue as possible to fill the buffer.
            while buffer.available() > 0 {
                let Some(data) = queue.front_mut() else {
                    break;
                };

                buffer.write_buffer(data)?;
                if data.available() == 0 {
                    queue.pop_front();
                }
            }

            Ok(buffer.bytes_written())
        })
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
        Some(self.state.waiters.wait_async_fd_events(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let mut events = FdEvents::POLLOUT;
        if !self.state.read_queue.lock().is_empty() {
            events |= FdEvents::POLLIN;
        }
        Ok(events)
    }
}
