// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::{kobject::DeviceMetadata, DeviceMode, DeviceOps},
    fs::sysfs::DeviceDirectory,
    task::{CurrentTask, Kernel},
    vfs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_nonseekable, FileObject, FileOps, FsNode, FsString,
    },
};
use starnix_logging::log_info;
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, WriteOps};
use starnix_uapi::{
    device_type::{DeviceType, MISC_MAJOR},
    error,
    errors::Errno,
    open_flags::OpenFlags,
};
use std::sync::Arc;

#[derive(Clone)]
#[allow(dead_code)]
pub struct TouchPowerPolicyDevice {
    touch_power_file: Arc<TouchPowerPolicyFile>,
}

#[allow(dead_code)]
impl TouchPowerPolicyDevice {
    pub fn new() -> Arc<Self> {
        Arc::new(TouchPowerPolicyDevice { touch_power_file: TouchPowerPolicyFile::new() })
    }

    pub fn register<L>(self: Arc<Self>, locked: &mut Locked<'_, L>, system_task: &CurrentTask)
    where
        L: LockBefore<FileOpsCore>,
    {
        let kernel = system_task.kernel();
        let registry = &kernel.device_registry;
        let misc_class = registry.get_or_create_class("misc".into(), registry.virtual_bus());
        registry.add_and_register_device(
            locked,
            system_task,
            FsString::from("touch_standby").as_ref(),
            DeviceMetadata::new(
                FsString::from("touch_standby"),
                DeviceType::new(MISC_MAJOR, 0),
                DeviceMode::Char,
            ),
            misc_class,
            DeviceDirectory::new,
            self,
        );
    }

    pub fn start_relay(self: &Arc<Self>, kernel: &Kernel) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            let mut prev_standby = false;
            loop {
                let standby = slf.touch_power_file.touch_standby.lock().to_owned();
                if standby != prev_standby {
                    // TODO(b/341142285): notify input pipeline that touch_standby state has changed
                    log_info!("WooF touch standby enabled: {:?}", standby);
                }
                prev_standby = standby;
            }
        });
    }
}

impl DeviceOps for TouchPowerPolicyDevice {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let touch_policy_file = self.touch_power_file.clone();
        Ok(Box::new(touch_policy_file))
    }
}

#[allow(dead_code)]
pub struct TouchPowerPolicyFile {
    // When true, Input Pipeline suspends processing of all touch events.
    touch_standby: Mutex<bool>,
}

#[allow(dead_code)]
impl TouchPowerPolicyFile {
    pub fn new() -> Arc<Self> {
        Arc::new(TouchPowerPolicyFile { touch_standby: Mutex::new(false) })
    }
}

impl FileOps for TouchPowerPolicyFile {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EOPNOTSUPP)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        error!(EOPNOTSUPP)
    }
}
