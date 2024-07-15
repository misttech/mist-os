// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::DeviceMetadata;
use crate::device::{DeviceMode, DeviceOps};
use crate::fs::sysfs::DeviceDirectory;
use crate::task::{CurrentTask, Kernel};
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, FileObject, FileOps, FsNode, FsString,
};
use starnix_logging::{log_error, log_info};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, WriteOps};
use starnix_uapi::device_type::{DeviceType, MISC_MAJOR};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use zerocopy::IntoBytes;

#[derive(Clone)]
#[allow(dead_code)]
pub struct TouchPowerPolicyDevice {
    touch_power_file: Arc<TouchPowerPolicyFile>,
}

#[allow(dead_code)]
impl TouchPowerPolicyDevice {
    pub fn new(touch_standby_sender: Sender<bool>) -> Arc<Self> {
        Arc::new(TouchPowerPolicyDevice {
            touch_power_file: TouchPowerPolicyFile::new(touch_standby_sender),
        })
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

    pub fn start_relay(self: &Arc<Self>, kernel: &Kernel, touch_standby_receiver: Receiver<bool>) {
        let slf = self.clone();
        kernel.kthreads.spawn(move |_lock_context, _current_task| {
            let mut prev_standby = false;
            while let Ok(standby) = touch_standby_receiver.recv() {
                if standby != prev_standby {
                    slf.notify_standby_state_changed(standby);
                }
                prev_standby = standby;
            }
            log_error!("touch_standby relay was terminated unexpectedly.");
        });
    }

    fn notify_standby_state_changed(self: &Arc<Self>, standby: bool) {
        // TODO(b/341142285): notify input pipeline that touch_standby state has changed
        log_info!("touch_standby enabled: {:?}", standby);
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
    // Sender used to send changes to `touch_standby` to the device relay
    touch_standby_sender: Sender<bool>,
}

#[allow(dead_code)]
impl TouchPowerPolicyFile {
    pub fn new(touch_standby_sender: Sender<bool>) -> Arc<Self> {
        Arc::new(TouchPowerPolicyFile { touch_standby: Mutex::new(false), touch_standby_sender })
    }
}

impl FileOps for TouchPowerPolicyFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        let touch_standby = self.touch_standby.lock().to_owned();
        data.write_all(touch_standby.as_bytes())
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let content = data.read_all()?;
        let standby = match &*content {
            b"0" | b"0\n" => false,
            b"1" | b"1\n" => true,
            _ => {
                log_error!("Invalid touch_standby value - must be 0 or 1");
                return error!(EINVAL);
            }
        };
        *self.touch_standby.lock() = standby;
        if let Err(e) = self.touch_standby_sender.send(standby) {
            log_error!("unable to send recent touch_standby state to device relay: {:?}", e);
        }
        Ok(content.len())
    }
}
