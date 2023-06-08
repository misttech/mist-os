// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        *,
    },
    lock::Mutex,
    task::*,
    types::*,
};
use std::collections::btree_map::BTreeMap;
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
}

struct LoopDeviceFile {
    _device: Arc<LoopDevice>,
}

impl FileOps for LoopDeviceFile {
    // TODO: Implement loop.
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }
}

#[derive(Debug, Default)]
pub struct LoopDeviceRegistry {
    devices: Mutex<BTreeMap<u32, Arc<LoopDevice>>>,
}

impl LoopDeviceRegistry {
    fn get_or_create(&self, minor: u32) -> Arc<LoopDevice> {
        self.devices.lock().entry(minor).or_insert_with(LoopDevice::new).clone()
    }
}

pub struct LoopControlDevice {
    _registry: Arc<LoopDeviceRegistry>,
}

impl LoopControlDevice {
    pub fn create_file_ops(kernel: &Kernel) -> Box<dyn FileOps> {
        Box::new(LoopControlDevice { _registry: kernel.loop_device_registry.clone() })
    }
}

impl FileOps for LoopControlDevice {
    // TODO: Implement loop-control.
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSYS)
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
