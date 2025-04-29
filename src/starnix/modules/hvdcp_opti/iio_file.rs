// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::utils::{connect_to_device, InvalidFile};
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

pub struct IioDirectory0 {
    base_dir: DeviceDirectory,
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
}

impl IioDirectory0 {
    pub fn new(device: Device) -> Self {
        Self {
            base_dir: DeviceDirectory::new(device),
            hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service"),
        }
    }

    fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        let mut entries = self.base_dir.create_file_ops_entries();
        let entry_names = [
            "name",
            "in_current_battery_input_current_limited_input",
            "in_current_usb_input_current_settled_input",
            "in_index_battery_die_health_input",
            "in_index_usb_connector_health_input",
            "in_index_usb_real_type_input",
            "in_index_usb_typec_mode_input",
        ];
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

impl FsNodeOps for IioDirectory0 {
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

        let create_invalid = || {
            Ok(node.fs().create_node(
                current_task,
                InvalidFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ))
        };

        match &**name {
            b"name" => iio_create_file(b"qpnp-smblite\n".to_vec()),

            b"in_current_battery_input_current_limited_input" => {
                iio_create_file_from_hvdcpopti("input_current_limited")
            }
            b"in_current_usb_input_current_settled_input" => {
                iio_create_file_from_hvdcpopti("input_current_settled")
            }
            b"in_index_battery_die_health_input" => iio_create_file_from_hvdcpopti("die_health"),
            b"in_index_usb_connector_health_input" => create_invalid(),
            b"in_index_usb_real_type_input" => iio_create_file_from_hvdcpopti("real_charger_type"),
            // Because Sorrel is guaranteed to be QTI_POWER_SUPPLY_CONNECTOR_MICRO_USB, USB TypeC
            // Mode is guaranteed to be QTI_POWER_SUPPLY_TYPEC_NONE = 0
            b"in_index_usb_typec_mode_input" => iio_create_file_int(0),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}

pub struct IioDirectory1 {
    base_dir: DeviceDirectory,
    hvdcpopti: fhvdcpopti::DeviceSynchronousProxy,
}

impl IioDirectory1 {
    pub fn new(device: Device) -> Self {
        Self {
            base_dir: DeviceDirectory::new(device),
            hvdcpopti: connect_to_device().expect("Could not connect to hvdcpopti service"),
        }
    }

    fn create_file_ops_entries(&self) -> Vec<VecDirectoryEntry> {
        let mut entries = self.base_dir.create_file_ops_entries();
        let entry_names = [
            "name",
            "in_energy_charge_full_design_input",
            "in_energy_charge_full_input",
            "in_index_debug_battery_input",
            "in_index_soh_input",
            "in_resistance_resistance_id_input",
        ];
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

impl FsNodeOps for IioDirectory1 {
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
            b"name" => iio_create_file(b"1c40000.qcom,spmi:qcom,pm5100@0:qpnp,qbg@4f00\n".to_vec()),

            b"in_energy_charge_full_design_input" => {
                iio_create_file_from_hvdcpopti("design_capacity")
            }
            b"in_energy_charge_full_input" => iio_create_file_from_hvdcpopti("learned_capacity"),
            // Sorrel is not using a debug battery.
            b"in_index_debug_battery_input" => iio_create_file_int(0),
            b"in_index_soh_input" => iio_create_file_from_hvdcpopti("soh"),
            b"in_resistance_resistance_id_input" => iio_create_file_from_hvdcpopti("resistance_id"),
            _ => self.base_dir.lookup(locked, node, current_task, name),
        }
    }
}
