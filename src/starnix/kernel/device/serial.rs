// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::DeviceMetadata;
use crate::device::terminal::{Terminal, TtyState};
use crate::device::{DeviceMode, DeviceOps};
use crate::fs::devpts::TtyFile;
use crate::fs::sysfs::DeviceDirectory;
use crate::task::{CurrentTask, EventHandler, Kernel, Waiter};
use crate::vfs::{FileOps, FsNode, FsString, VecInputBuffer, VecOutputBuffer};
use anyhow::Error;
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_hardware_serial as fserial;
use starnix_sync::{DeviceOpen, Locked, Unlocked};
use starnix_uapi::device_type::{DeviceType, TTY_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::from_status_like_fdio;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;
use std::sync::Arc;

struct ForwardTask {
    terminal: Arc<Terminal>,
    serial_proxy: Arc<fserial::DeviceSynchronousProxy>,
}

impl ForwardTask {
    fn new(terminal: Arc<Terminal>, serial_proxy: Arc<fserial::DeviceSynchronousProxy>) -> Self {
        terminal.main_open();
        Self { terminal, serial_proxy }
    }

    fn spawn_reader(&self, kernel: &Kernel) {
        let terminal = self.terminal.clone();
        let serial_proxy = self.serial_proxy.clone();
        kernel.kthreads.spawn(move |locked, current_task| {
            let _result = move || -> Result<(), Error> {
                let waiter = Waiter::new();
                loop {
                    terminal.main_wait_async(&waiter, FdEvents::POLLOUT, EventHandler::None);
                    waiter.wait(locked, current_task)?;
                    let data = serial_proxy
                        .read(zx::MonotonicInstant::INFINITE)?
                        .map_err(|e: i32| from_status_like_fdio!(zx::Status::from_raw(e)))?;
                    terminal.main_write(locked, &mut VecInputBuffer::from(data))?;
                }
            }();
        });
    }

    fn spawn_writer(&self, kernel: &Kernel) {
        let terminal = self.terminal.clone();
        let serial_proxy = self.serial_proxy.clone();
        kernel.kthreads.spawn(move |locked, current_task| {
            let _result = move || -> Result<(), Error> {
                let waiter = Waiter::new();
                loop {
                    terminal.main_wait_async(&waiter, FdEvents::POLLIN, EventHandler::None);
                    waiter.wait(locked, current_task)?;
                    let size = terminal.read().get_available_read_size(true);
                    let mut buffer = VecOutputBuffer::new(size);
                    terminal.main_read(locked, &mut buffer)?;
                    serial_proxy
                        .write(buffer.data(), zx::MonotonicInstant::INFINITE)?
                        .map_err(|e: i32| from_status_like_fdio!(zx::Status::from_raw(e)))?;
                }
            }();
        });
    }
}

impl Drop for ForwardTask {
    fn drop(&mut self) {
        self.terminal.main_close();
        // TODO: How do we terminate the spawned threads?
    }
}

pub struct SerialDevice {
    terminal: Arc<Terminal>,
    _forward_task: ForwardTask,
}

impl SerialDevice {
    /// Create a serial device attached to the given fuchsia.hardware.serial endpoint.
    ///
    /// To register the device, call `register_serial_device`.
    pub fn new(
        _locked: &mut Locked<'_, Unlocked>,
        current_task: &CurrentTask,
        serial_device: ClientEnd<fserial::DeviceMarker>,
    ) -> Result<Arc<Self>, Errno> {
        let kernel = current_task.kernel();

        let state = kernel.expando.get::<TtyState>();
        let terminal = state.get_next_terminal(current_task)?;

        let serial_proxy = Arc::new(serial_device.into_sync_proxy());
        let forward_task = ForwardTask::new(terminal.clone(), serial_proxy);
        forward_task.spawn_reader(kernel);
        forward_task.spawn_writer(kernel);

        Ok(Arc::new(Self { terminal, _forward_task: forward_task }))
    }
}

impl DeviceOps for Arc<SerialDevice> {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(TtyFile::new(self.terminal.clone())))
    }
}

/// Register the given serial device.
///
/// The `index` should be the numerical value associated with the device. For example, if you want
/// to register /dev/ttyS<n>, then `index` should be `n`.
pub fn register_serial_device(
    locked: &mut Locked<'_, Unlocked>,
    system_task: &CurrentTask,
    index: u32,
    serial_device: Arc<SerialDevice>,
) {
    // See https://www.kernel.org/doc/Documentation/admin-guide/devices.txt
    //  64 = /dev/ttyS0    First UART serial port
    const SERIAL_MINOR_BASE: u32 = 64;

    let name = FsString::from(format!("ttyS{}", index));

    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;
    registry.register_device(
        locked,
        system_task,
        name.as_ref(),
        DeviceMetadata::new(
            name.clone(),
            DeviceType::new(TTY_MAJOR, SERIAL_MINOR_BASE + index),
            DeviceMode::Char,
        ),
        registry.objects.tty_class(),
        DeviceDirectory::new,
        serial_device,
    );
}
