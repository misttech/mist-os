// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::marker::PhantomData;

use fuchsia_component::client::connect_to_protocol_sync;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    fileops_impl_noop_sync, FileObject, FileOps, FsNode, FsNodeOps, InputBuffer, OutputBuffer,
};
use starnix_core::{fileops_impl_seekable, fs_node_impl_not_dir};
use starnix_logging::log_error;
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};
use {fidl_fuchsia_hardware_google_nanohub as fnanohub, zx};

/// A wrapper around `starnix_uapi::errors::Errno`.
pub struct NanohubSysfsError(Errno);

/// An equivalent to `starnix_uapi::errno!`, but for `NanohubSysfsError`.
macro_rules! nanohub_sysfs_errno {
    ($error_code:ident) => {
        NanohubSysfsError(errno!($error_code))
    };
}

/// An equivalent to `starnix_uapi::error!`, but for `NanohubSysfsError`.
macro_rules! nanohub_sysfs_error {
    ($error_code:ident) => {
        Err(NanohubSysfsError(errno!($error_code)))
    };
}

impl From<fidl::Error> for NanohubSysfsError {
    /// Generate the sysfs error response for a FIDL transport error.
    ///
    /// This allows you to handle the error case simply by using the ? operator.
    fn from(value: fidl::Error) -> Self {
        log_error!(" FIDL error while calling Nanohub service method: {value:?}");
        nanohub_sysfs_errno!(EINVAL)
    }
}

impl From<i32> for NanohubSysfsError {
    /// Generate the sysfs error response for a zx.Status error result from a FIDL method.
    ///
    /// This allows you to handle the error case simply by using the ? operator.
    fn from(value: i32) -> Self {
        let status = zx::Status::from_raw(value);
        log_error!("Nanohub service method responded with an error: {status:?}");
        nanohub_sysfs_errno!(EINVAL)
    }
}

/// Convert an `Option<T>` (usually from a FIDL table) to a `Result<T, NanohubSysfsError>`.
fn try_get<T>(o: Option<T>) -> Result<T, NanohubSysfsError> {
    o.ok_or_else(|| {
        log_error!("Missing expected value from Nanohub service method response.");
        nanohub_sysfs_errno!(EINVAL)
    })
}

/// Operations supported by sysfs files.
///
/// These are analogous to (but not identical to) the operations exposed by Linux sysfs attributes.
pub trait NanohubSysFsFileOps: Default + Send + Sync + 'static {
    /// Get the value of the attribute.
    fn show(
        &self,
        _service: &fnanohub::DeviceSynchronousProxy,
    ) -> Result<String, NanohubSysfsError> {
        nanohub_sysfs_error!(EINVAL)
    }

    /// Store a new value for the attribute.
    fn store(
        &self,
        _service: &fnanohub::DeviceSynchronousProxy,
        _value: String,
    ) -> Result<(), NanohubSysfsError> {
        nanohub_sysfs_error!(EINVAL)
    }
}

enum NanohubSysFsContentsState {
    Unarmed,
    Armed(String),
    Err(Errno),
}

impl AsRef<NanohubSysFsContentsState> for NanohubSysFsContentsState {
    fn as_ref(&self) -> &Self {
        &self
    }
}

/// A file that exposes Nanohub data via sysfs semantics.
pub struct NanohubSysFsFile<T: NanohubSysFsFileOps> {
    /// Implementations for sysfs operations.
    sysfs_ops: Box<T>,

    /// An active connection to the Nanohub FIDL service.
    service: fnanohub::DeviceSynchronousProxy,

    /// The buffered contents of the return of the underlying FIDL method.
    ///
    /// To match SysFS behavior on Linux, this buffer is only populated/refreshed when the file is
    /// read from offset position 0, so at the moment the file is opened (i.e., at the moment this
    /// struct is instantiated), the contents are `Unarmed`. Once the the buffer is populated,
    /// the result contains either the data returned by the `show` method or a Linux error code.
    contents: Mutex<NanohubSysFsContentsState>,
}

impl<T: NanohubSysFsFileOps> NanohubSysFsFile<T> {
    pub fn new(sysfs_ops: Box<T>) -> Box<Self> {
        let service = connect_to_protocol_sync::<fnanohub::DeviceMarker>()
            // TODO(https://fxbug.dev/425729841): Don't destroy Starnix.
            .expect("Error connecting to Nanohub service");

        Box::new(NanohubSysFsFile {
            sysfs_ops,
            service,
            contents: Mutex::new(NanohubSysFsContentsState::Unarmed),
        })
    }

    /// Refresh the data in the file buffer by calling `show`.
    ///
    /// This is analogous to sysfs rearming in Linux.
    fn rearm(&self) {
        let op_result = self.sysfs_ops.show(&self.service);
        let mut contents_guard = self.contents.lock();
        *contents_guard = match op_result {
            Ok(value) => NanohubSysFsContentsState::Armed(value),
            Err(error) => NanohubSysFsContentsState::Err(error.0),
        }
    }
}

impl<T: NanohubSysFsFileOps> FileOps for NanohubSysFsFile<T> {
    fileops_impl_seekable!();
    fileops_impl_noop_sync!();

    fn read(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        // Reading from offset 0 refreshes the contents buffer via the underlying `show` method.
        if offset == 0 {
            self.rearm();
        };

        let contents_guard = self.contents.lock();

        match contents_guard.as_ref() {
            NanohubSysFsContentsState::Err(error) => Err(error.clone()),
            NanohubSysFsContentsState::Unarmed => {
                log_error!("Failed to read Nanohub SysFS file with no contents");
                error!(EINVAL)
            }
            NanohubSysFsContentsState::Armed(value) => {
                // Write the slice of data requested by the caller from the contents buffer,
                // returning the number of bytes written. If the offset is at the end of the
                // contents (e.g., if the contents have already been read), this will write 0
                // bytes, indicating EOF. If an error occurred, nothing will be written and the
                // error will be returned.
                let bytes = value.as_bytes();
                let start = offset.min(bytes.len());
                let end = (offset + data.available()).min(bytes.len());
                data.write(&bytes[start..end])
            }
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // The entire contents of the input buffer are read and stored in the attribute, regardless
        // of the current file position.
        data.read_all()
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|e| {
                        log_error!("Failed convert to input buffer to string: {e:?}");
                        errno!(EINVAL)
                    })
                    .map(|v| (v.len(), v))
            })
            .and_then(|(num_bytes, value)| {
                self.sysfs_ops.store(&self.service, value).map(|_| num_bytes).map_err(|e| e.0)
            })
    }
}

/// File node for Nanohub sysfs files.
pub struct NanohubSysFsNode<T: NanohubSysFsFileOps> {
    _phantom: PhantomData<T>,
}

impl<T: NanohubSysFsFileOps> NanohubSysFsNode<T> {
    pub fn new() -> Self {
        NanohubSysFsNode::<T> { _phantom: PhantomData }
    }
}

impl<T: NanohubSysFsFileOps> FsNodeOps for NanohubSysFsNode<T> {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(NanohubSysFsFile::<T>::new(Box::new(T::default())))
    }
}

#[derive(Default)]
pub struct FirmwareNameSysFsOps {}

impl NanohubSysFsFileOps for FirmwareNameSysFsOps {
    fn show(
        &self,
        service: &fnanohub::DeviceSynchronousProxy,
    ) -> Result<String, NanohubSysfsError> {
        Ok(service.get_firmware_name(zx::MonotonicInstant::INFINITE)?)
    }
}

#[derive(Default)]
pub struct FirmwareVersionSysFsOps {}

impl NanohubSysFsFileOps for FirmwareVersionSysFsOps {
    fn show(
        &self,
        service: &fnanohub::DeviceSynchronousProxy,
    ) -> Result<String, NanohubSysfsError> {
        let v = service.get_firmware_version(zx::MonotonicInstant::INFINITE)?;
        Ok(format!(
            "hw type: {:04x} hw ver: {:04x} bl ver: {:04x} os ver: {:04x} variant ver: {:08x}\n",
            v.hardware_type,
            v.hardware_version,
            v.bootloader_version,
            v.os_version,
            v.variant_version
        ))
    }
}

#[derive(Default)]
pub struct TimeSyncSysFsOps {}

impl NanohubSysFsFileOps for TimeSyncSysFsOps {
    fn show(
        &self,
        service: &fidl_fuchsia_hardware_google_nanohub::DeviceSynchronousProxy,
    ) -> Result<String, NanohubSysfsError> {
        let response = service.get_time_sync(zx::MonotonicInstant::INFINITE)??;
        let ap = try_get(response.ap_boot_time)?;
        let mcu = try_get(response.mcu_boot_time)?;
        Ok(format!("ap time: {} mcu time: {}\n", ap, mcu))
    }
}
