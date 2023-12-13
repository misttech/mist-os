// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::virtgralloc::{
    virtgralloc_SetVulkanModeResult, virtgralloc_SetVulkanModeResult_SUCCESS,
    virtgralloc_VulkanMode, virtgralloc_request_SetVulkanMode, virtgralloc_set_vulkan_mode,
};
use crate::{
    mm::MemoryAccessorExt,
    task::CurrentTask,
    vfs::{fileops_impl_nonseekable, FileObject, FileOps, InputBuffer, OutputBuffer},
};
use fidl_fuchsia_starnix_gralloc as fgralloc;
use starnix_logging::{log_error, log_info, log_warn};
use starnix_sync::Mutex;
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::{
    errno, error,
    errors::Errno,
    user_address::{UserAddress, UserRef},
};
use std::sync::Arc;

pub struct GrallocFile {
    mode_setter: Arc<Mutex<fgralloc::VulkanModeSetterSynchronousProxy>>,
}

impl GrallocFile {
    pub fn new_file(
        mode_setter: Arc<Mutex<fgralloc::VulkanModeSetterSynchronousProxy>>,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(Self { mode_setter }))
    }

    fn set_vulkan_mode(
        &self,
        vulkan_mode: virtgralloc_VulkanMode,
    ) -> Result<virtgralloc_SetVulkanModeResult, Errno> {
        let vulkan_mode = fgralloc::VulkanMode::from_primitive(vulkan_mode as u32)
            .ok_or_else(|| errno!(EINVAL))
            .map_err(|e| {
                log_error!("VulkanMode::from_primitive failed");
                e
            })?;

        // This must be synchronous, and it must work, since this is the only way that gralloc will
        // know which vulkan impl, which is necessary for gralloc to work properly, and the gralloc
        // feature requires gralloc to work properly. There's no meaningful way for the IOCTL caller
        // to handle an IOCTL failure other than just logging and then ignoring it, which would only
        // make the overall broken situation more confusing that just using expect() here. This is
        // only called once early in boot.
        self.mode_setter
            .lock()
            .set_vulkan_mode(
                &fgralloc::VulkanModeSetterSetVulkanModeRequest {
                    vulkan_mode: Some(vulkan_mode),
                    ..Default::default()
                },
                fuchsia_zircon::Time::INFINITE,
            )
            .expect("gralloc feature requires working VulkanModeSetter")
            .expect("gralloc feature requires VulkanModeSetter to set mode");

        log_info!("gralloc vulkan_mode set to {:?}", vulkan_mode);

        Ok(virtgralloc_SetVulkanModeResult_SUCCESS)
    }
}

impl FileOps for GrallocFile {
    fileops_impl_nonseekable!();

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        match request {
            virtgralloc_request_SetVulkanMode => {
                // switch from map_err to inspect_err when/if stable
                let user_addr = UserAddress::from(arg);
                let mut request: virtgralloc_set_vulkan_mode =
                    current_task.read_object(UserRef::new(user_addr)).map_err(|e| {
                        log_error!("virtgralloc_request_SetVulkanMode read_object failed: {:?}", e);
                        e
                    })?;
                request.result = self.set_vulkan_mode(request.vulkan_mode).map_err(|e| {
                    log_error!("set_vulkan_mode failed: {:?}", e);
                    e
                })?;
                current_task.write_object(UserRef::new(user_addr), &request)
            }
            t => {
                log_warn!("Got unknown request: {:?}", t);
                error!(ENOSYS)
            }
        }?;

        Ok(SUCCESS)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        // because fileops_impl_nonseekable!()
        debug_assert!(offset == 0);
        error!(EINVAL)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // because fileops_impl_nonseekable!()
        debug_assert!(offset == 0);
        error!(EINVAL)
    }
}
