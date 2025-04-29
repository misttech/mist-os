// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::utils::connect_to_device;
use fidl_fuchsia_hardware_qcom_hvdcpopti as fhvdcpopti;
use starnix_core::device::kobject::Device;
use starnix_core::fs::sysfs::DeviceDirectory;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::{
    fs_node_impl_dir_readonly, BytesFile, DirectoryEntryType, FileOps, FsNode, FsNodeHandle,
    FsNodeInfo, FsNodeOps, FsStr, VecDirectory, VecDirectoryEntry,
};
use starnix_logging::log_error;
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errno;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;

pub struct UsbPowerSupply {
    base_dir: DeviceDirectory,
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
}

impl UsbPowerSupply {
    pub fn new(device: Device) -> Self {
        Self {
            base_dir: DeviceDirectory::new(device),
            hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service"),
        }
    }

    fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        let mut entries = self.base_dir.create_file_ops_entries();
        let entry_names = ["present", "voltage_max", "voltage_now"];
        for name in entry_names.into_iter() {
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: name.into(),
                inode: None,
            });
        }
        entries
    }
}

impl FsNodeOps for UsbPowerSupply {
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
        let iio_create_file = |value| {
            Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(value),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ))
        };
        let iio_create_file_int =
            |int_value| iio_create_file(format!("{}\n", int_value).into_bytes());
        let iio_create_file_from_hvdcpopti = |iio_label| {
            let value = self
                .hvdcpopti
                .get_iio_value(iio_label, zx::MonotonicInstant::INFINITE)
                .map_err(|e| {
                    log_error!("Failed to GetIioValue: {:?}", e);
                    errno!(EINVAL)
                })?
                .map_err(|e| {
                    log_error!("GetIioValue failed: {:?}", e);
                    errno!(EINVAL)
                })?;

            iio_create_file_int(value)
        };

        match &**name {
            b"present" => iio_create_file_from_hvdcpopti("usb_present"),
            b"voltage_max" => iio_create_file_from_hvdcpopti("voltage_max"),
            b"voltage_now" => iio_create_file_from_hvdcpopti("voltage_now"),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}

pub struct BatteryPowerSupply {
    base_dir: DeviceDirectory,
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
}

impl BatteryPowerSupply {
    pub fn new(device: Device) -> Self {
        Self {
            base_dir: DeviceDirectory::new(device),
            hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service"),
        }
    }

    fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        let mut entries = self.base_dir.create_file_ops_entries();
        let entry_names = ["charge_type", "health"];
        for name in entry_names.into_iter() {
            entries.push(VecDirectoryEntry {
                entry_type: DirectoryEntryType::REG,
                name: name.into(),
                inode: None,
            });
        }
        entries
    }
}

impl FsNodeOps for BatteryPowerSupply {
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
        let iio_create_file = |value| {
            Ok(node.fs().create_node(
                current_task,
                BytesFile::new_node(value),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ))
        };
        let iio_create_file_int =
            |int_value| iio_create_file(format!("{}\n", int_value).into_bytes());
        let iio_create_file_from_hvdcpopti = |iio_label| {
            let value = self
                .hvdcpopti
                .get_iio_value(iio_label, zx::MonotonicInstant::INFINITE)
                .map_err(|e| {
                    log_error!("Failed to GetIioValue: {:?}", e);
                    errno!(EINVAL)
                })?
                .map_err(|e| {
                    log_error!("GetIioValue failed: {:?}", e);
                    errno!(EINVAL)
                })?;

            iio_create_file_int(value)
        };

        match &**name {
            b"charge_type" => iio_create_file_from_hvdcpopti("battery_charge_type"),
            b"health" => iio_create_file_from_hvdcpopti("battery_health"),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}

pub struct BmsPowerSupply {
    base_dir: DeviceDirectory,
}

impl BmsPowerSupply {
    pub fn new(device: Device) -> Self {
        Self { base_dir: DeviceDirectory::new(device) }
    }

    fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        self.base_dir.create_file_ops_entries()
    }
}

impl FsNodeOps for BmsPowerSupply {
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
        self.base_dir.lookup(locked, node, current_task, name)
    }
}
