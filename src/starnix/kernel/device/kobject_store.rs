// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{
    Bus, Class, Collection, Device, DeviceMetadata, KObject, KObjectBased, KObjectHandle,
};
use crate::device::DeviceMode;
use crate::fs::sysfs::{
    BusCollectionDirectory, KObjectDirectory, KObjectSymlinkDirectory, SYSFS_BLOCK, SYSFS_BUS,
    SYSFS_CLASS, SYSFS_DEVICES,
};
use crate::vfs::{FsNodeOps, FsStr};

pub struct KObjectStore {
    pub devices: KObjectHandle,
    pub class: KObjectHandle,
    pub block: KObjectHandle,
    pub bus: KObjectHandle,
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

        // Insert the created device kobject into its subsystems.
        class.collection.kobject().insert_child(device_kobject.clone());
        if metadata.mode == DeviceMode::Block {
            self.block.insert_child(device_kobject.clone());
        }
        if let Some(bus_collection) = &class.bus.collection {
            bus_collection.kobject().insert_child(device_kobject.clone());
        }

        Device::new(device_kobject, class, metadata)
    }
}

impl Default for KObjectStore {
    fn default() -> Self {
        Self {
            devices: KObject::new_root(SYSFS_DEVICES.into()),
            class: KObject::new_root(SYSFS_CLASS.into()),
            block: KObject::new_root_with_dir(SYSFS_BLOCK.into(), KObjectSymlinkDirectory::new),
            bus: KObject::new_root(SYSFS_BUS.into()),
        }
    }
}
