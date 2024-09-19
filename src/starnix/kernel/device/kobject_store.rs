// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{
    Bus, Class, Collection, Device, DeviceMetadata, KObject, KObjectBased, KObjectHandle,
};
use crate::device::DeviceMode;
use crate::fs::sysfs::{
    BusCollectionDirectory, KObjectDirectory, KObjectSymlinkDirectory, SYSFS_BLOCK, SYSFS_BUS,
    SYSFS_CLASS, SYSFS_DEV, SYSFS_DEVICES,
};
use crate::vfs::{FsNodeOps, FsStr, FsString};

pub struct KObjectStore {
    pub devices: KObjectHandle,
    pub class: KObjectHandle,
    pub block: KObjectHandle,
    pub bus: KObjectHandle,
    pub dev: KObjectHandle,

    dev_block: KObjectHandle,
    dev_char: KObjectHandle,
}

impl KObjectStore {
    /// Returns the virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> Bus {
        Bus::new(self.devices.get_or_create_child("virtual".into(), KObjectDirectory::new), None)
    }

    pub fn virtual_block_class(&self) -> Class {
        self.get_or_create_class("block".into(), self.virtual_bus())
    }

    pub fn graphics_class(&self) -> Class {
        self.get_or_create_class("graphics".into(), self.virtual_bus())
    }

    pub fn input_class(&self) -> Class {
        self.get_or_create_class("input".into(), self.virtual_bus())
    }

    pub fn mem_class(&self) -> Class {
        self.get_or_create_class("mem".into(), self.virtual_bus())
    }

    pub fn misc_class(&self) -> Class {
        self.get_or_create_class("misc".into(), self.virtual_bus())
    }

    pub fn tty_class(&self) -> Class {
        self.get_or_create_class("tty".into(), self.virtual_bus())
    }

    // This class name is exposed to userspace. It's unclear whether we should be using this name.
    pub fn starnix_class(&self) -> Class {
        self.get_or_create_class("starnix".into(), self.virtual_bus())
    }

    pub fn get_or_create_bus(&self, name: &FsStr) -> Bus {
        let collection =
            Collection::new(self.bus.get_or_create_child(name, BusCollectionDirectory::new));
        Bus::new(self.devices.get_or_create_child(name, KObjectDirectory::new), Some(collection))
    }

    pub fn get_or_create_class(&self, name: &FsStr, bus: Bus) -> Class {
        let collection =
            Collection::new(self.class.get_or_create_child(name, KObjectSymlinkDirectory::new));
        Class::new(bus.kobject().get_or_create_child(name, KObjectDirectory::new), bus, collection)
    }

    pub(super) fn create_device<F, N>(
        &self,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        create_device_sysfs_ops: F,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        let class_cloned = class.clone();
        let metadata_cloned = metadata.clone();
        let device_kobject = class.kobject().get_or_create_child(name, move |kobject| {
            create_device_sysfs_ops(Device::new(
                kobject.upgrade().unwrap(),
                class_cloned.clone(),
                metadata_cloned.clone(),
            ))
        });

        // Insert the newly created device into various views.
        class.collection.kobject().insert_child(device_kobject.clone());
        let device_number = metadata.device_type.to_string().into();
        match metadata.mode {
            DeviceMode::Block => {
                self.block.insert_child(device_kobject.clone());
                self.dev_block.insert_child_with_name(device_number, device_kobject.clone())
            }
            DeviceMode::Char => {
                self.dev_char.insert_child_with_name(device_number, device_kobject.clone())
            }
        }
        if let Some(bus_collection) = &class.bus.collection {
            bus_collection.kobject().insert_child(device_kobject.clone());
        }

        Device::new(device_kobject, class, metadata)
    }

    pub(super) fn destroy_device(&self, device: &Device) {
        let kobject = device.kobject();
        let name = kobject.name();
        // Remove the device from its views in the reverse order in which it was added.
        if let Some(bus_collection) = &device.class.bus.collection {
            bus_collection.kobject().remove_child(name);
        }
        let device_number: FsString = device.metadata.device_type.to_string().into();
        match device.metadata.mode {
            DeviceMode::Block => {
                self.dev_block.remove_child(device_number.as_ref());
                self.block.remove_child(name);
            }
            DeviceMode::Char => {
                self.dev_char.remove_child(device_number.as_ref());
            }
        }
        device.class.collection.kobject().remove_child(name);
        // Finally, remove the device from the object store.
        kobject.remove();
    }
}

impl Default for KObjectStore {
    fn default() -> Self {
        let devices = KObject::new_root(SYSFS_DEVICES.into());
        let class = KObject::new_root(SYSFS_CLASS.into());
        let block = KObject::new_root_with_dir(SYSFS_BLOCK.into(), KObjectSymlinkDirectory::new);
        let bus = KObject::new_root(SYSFS_BUS.into());
        let dev = KObject::new_root(SYSFS_DEV.into());

        let dev_block = dev.get_or_create_child("block".into(), KObjectSymlinkDirectory::new);
        let dev_char = dev.get_or_create_child("char".into(), KObjectSymlinkDirectory::new);

        Self { devices, class, block, bus, dev, dev_block, dev_char }
    }
}
