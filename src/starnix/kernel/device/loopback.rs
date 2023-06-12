// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{fs::*, lock::Mutex, syscalls::*, task::*, types::*};
use std::collections::btree_map::{BTreeMap, Entry};
use std::sync::Arc;

#[derive(Debug, Default)]
struct LoopDevice {}

impl LoopDevice {
    fn new() -> Arc<Self> {
        Arc::new(LoopDevice {})
    }

    fn create_file_ops(self: &Arc<Self>) -> Box<dyn FileOps> {
        Box::new(LoopDeviceFile { _device: self.clone() })
    }

    fn is_busy(&self) -> bool {
        // TODO: Add more state to the loop device.
        false
    }
}

struct LoopDeviceFile {
    _device: Arc<LoopDevice>,
}

impl FileOps for LoopDeviceFile {
    // TODO: Implement loop.
    fileops_impl_seekless!();
    fileops_impl_dataless!();
}

#[derive(Debug, Default)]
pub struct LoopDeviceRegistry {
    devices: Mutex<BTreeMap<u32, Arc<LoopDevice>>>,
}

impl LoopDeviceRegistry {
    fn get_or_create(&self, minor: u32) -> Arc<LoopDevice> {
        self.devices.lock().entry(minor).or_insert_with(LoopDevice::new).clone()
    }

    fn find(&self) -> Result<u32, Errno> {
        let mut devices = self.devices.lock();
        let mut minor = 0;
        loop {
            match devices.entry(minor) {
                Entry::Vacant(e) => {
                    e.insert(LoopDevice::new());
                    return Ok(minor);
                }
                Entry::Occupied(e) => {
                    if e.get().is_busy() {
                        minor += 1;
                        continue;
                    }
                    return Ok(minor);
                }
            }
        }
    }

    fn add(&self, minor: u32) -> Result<(), Errno> {
        match self.devices.lock().entry(minor) {
            Entry::Vacant(e) => {
                e.insert(LoopDevice::new());
                Ok(())
            }
            Entry::Occupied(_) => {
                error!(EEXIST)
            }
        }
    }

    fn remove(&self, minor: u32) -> Result<(), Errno> {
        match self.devices.lock().entry(minor) {
            Entry::Vacant(_) => Ok(()),
            Entry::Occupied(e) => {
                if e.get().is_busy() {
                    return error!(EBUSY);
                }
                e.remove();
                Ok(())
            }
        }
    }

    fn ensure_initial_devices(&self) {
        for minor in 0..8 {
            self.get_or_create(minor);
        }
    }
}

pub struct LoopControlDevice {
    registry: Arc<LoopDeviceRegistry>,
}

impl LoopControlDevice {
    pub fn create_file_ops(kernel: &Kernel) -> Box<dyn FileOps> {
        let registry = kernel.loop_device_registry.clone();
        registry.ensure_initial_devices();
        Box::new(LoopControlDevice { registry })
    }
}

impl FileOps for LoopControlDevice {
    fileops_impl_seekless!();
    fileops_impl_dataless!();

    fn ioctl(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            LOOP_CTL_GET_FREE => Ok(self.registry.find()?.into()),
            LOOP_CTL_ADD => {
                let minor = arg.into_arg();
                self.registry.add(minor)?;
                Ok(minor.into())
            }
            LOOP_CTL_REMOVE => {
                let minor = arg.into_arg();
                self.registry.remove(minor)?;
                Ok(minor.into())
            }
            _ => default_ioctl(request),
        }
    }
}

pub fn create_loop_device(
    current_task: &CurrentTask,
    id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(current_task.kernel().loop_device_registry.get_or_create(id.minor()).create_file_ops())
}
