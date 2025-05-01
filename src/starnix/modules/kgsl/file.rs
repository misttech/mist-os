// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileObject, FileOps, FsNode};
use starnix_core::{fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync};
use starnix_sync::{Locked, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::{
    error, kgsl_device_getproperty, kgsl_devinfo, IOCTL_KGSL_DEVICE_GETPROPERTY,
    KGSL_PROP_DEVICE_INFO,
};
use std::sync::Once;

pub struct KgslFile {}

impl KgslFile {
    pub fn init() {}

    pub fn new_file(
        _current_task: &CurrentTask,
        _dev: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            Self::init();
        });
        Ok(Box::new(Self {}))
    }
}

impl FileOps for KgslFile {
    fileops_impl_dataless!();
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
        match request {
            IOCTL_KGSL_DEVICE_GETPROPERTY => {
                let user_params = UserRef::<kgsl_device_getproperty>::from(arg);
                let params = current_task.read_object(user_params)?;
                match params.type_ {
                    KGSL_PROP_DEVICE_INFO => {
                        const PLACEHOLDER_DEVICE_ID: u32 = 42;
                        let info =
                            kgsl_devinfo { device_id: PLACEHOLDER_DEVICE_ID, ..Default::default() };
                        let result_address = UserAddress::from(params.value);
                        let user_result = UserRef::<kgsl_devinfo>::from(result_address);
                        current_task.write_object(user_result, &info)?;
                        Ok(SUCCESS)
                    }
                    _ => {
                        error!(ENOTSUP)
                    }
                }
            }
            _ => {
                error!(ENOTSUP)
            }
        }
    }
}
