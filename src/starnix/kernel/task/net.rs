// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::Device;
use crate::fs::sysfs::DeviceDirectory;
use crate::task::Kernel;
use crate::vfs::{FsStr, FsString};
use starnix_sync::Mutex;
use std::collections::BTreeMap;

/// Keeps track of network devices and their [`Device`].
#[derive(Default)]
pub struct NetstackDevices {
    /// The known netstack devices.
    devices: Mutex<BTreeMap<FsString, Device>>,
}

impl NetstackDevices {
    pub fn add_device(&self, kernel: &Kernel, name: &FsStr) {
        let mut devices = self.devices.lock();
        let device = kernel.device_registry.add_net_device(name, DeviceDirectory::new);
        devices.insert(name.into(), device);
    }

    pub fn remove_device(&self, kernel: &Kernel, name: &FsStr) {
        let mut devices = self.devices.lock();
        if let Some(device) = devices.remove(name) {
            kernel.device_registry.remove_net_device(device);
        }
    }

    pub fn get_device(&self, name: &FsStr) -> Option<Device> {
        let devices = self.devices.lock();
        devices.get(name).cloned()
    }

    pub fn snapshot_devices(&self) -> Vec<(FsString, Device)> {
        let devices = self.devices.lock();
        devices.iter().map(|(name, device)| (name.clone(), device.clone())).collect()
    }
}
