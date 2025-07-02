// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fdio::service_connect;
use magma::{
    magma_device_import, magma_device_query, magma_device_release, magma_device_t, magma_handle_t,
    magma_initialize_logging, MAGMA_QUERY_VENDOR_ID, MAGMA_STATUS_OK,
};
use starnix_core::mm::MemoryAccessorExt;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{FileObject, FileOps, FsNode};
use starnix_core::{fileops_impl_dataless, fileops_impl_nonseekable, fileops_impl_noop_sync};
use starnix_logging::{log_error, log_info, log_warn};
use starnix_sync::{Locked, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::{UserAddress, UserRef};
use starnix_uapi::{
    errno, error, kgsl_device_getproperty, kgsl_devinfo, IOCTL_KGSL_DEVICE_GETPROPERTY,
    KGSL_PROP_DEVICE_INFO,
};
use std::sync::Once;
use zx::{Channel, HandleBased};

pub struct KgslFile {
    magma_device: magma_device_t,
}

impl KgslFile {
    pub fn init() {
        match Self::init_magma_logging() {
            Ok(()) => log_info!("KGSL magma logging enabled"),
            Err(()) => log_warn!("KGSL magma logging failed to initialize"),
        };
    }

    fn init_magma_logging() -> Result<(), ()> {
        let (client, server) = Channel::create();
        service_connect("/svc/fuchsia.logger.LogSink", server).map_err(|_| ())?;
        let result = unsafe { magma_initialize_logging(client.into_raw()) };
        if result == MAGMA_STATUS_OK {
            Ok(())
        } else {
            Err(())
        }
    }

    fn import_device(path: &str) -> Result<magma_device_t, zx::Status> {
        let (client, server) = Channel::create();
        service_connect(&path, server)?;
        let mut magma_device: magma_device_t = 0;
        let result = unsafe { magma_device_import(client.into_raw(), &mut magma_device) };
        if result != MAGMA_STATUS_OK {
            return Err(zx::Status::INTERNAL);
        }
        let mut result_out: u64 = 0;
        let mut result_buffer_out: magma_handle_t = 0;
        let result = unsafe {
            magma_device_query(
                magma_device,
                MAGMA_QUERY_VENDOR_ID,
                &mut result_buffer_out,
                &mut result_out,
            )
        };
        if result != MAGMA_STATUS_OK {
            return Err(zx::Status::INTERNAL);
        }
        log_info!("Magma device at {} is vendor {:#04x}", path, result_out);
        Ok(magma_device)
    }

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
        let mut magma_devices = std::fs::read_dir("/svc/fuchsia.gpu.magma.Service")
            .map_err(|_| errno!(ENXIO))?
            .filter_map(|x| x.ok())
            .filter_map(|entry| entry.path().join("device").into_os_string().into_string().ok())
            .filter_map(|path| Self::import_device(&path).ok());
        let magma_device = magma_devices.next().ok_or_else(|| errno!(ENXIO))?;
        Ok(Box::new(Self { magma_device }))
    }
}

impl Drop for KgslFile {
    fn drop(&mut self) {
        unsafe { magma_device_release(self.magma_device) };
    }
}

impl FileOps for KgslFile {
    fileops_impl_dataless!();
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn ioctl(
        &self,
        _locked: &mut Locked<Unlocked>,
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
                        log_error!("KGSL unimplemented GetProperty type {}", params.type_);
                        error!(ENOTSUP)
                    }
                }
            }
            _ => {
                log_error!("KGSL unimplemented ioctl {}", request);
                error!(ENOTSUP)
            }
        }
    }
}
