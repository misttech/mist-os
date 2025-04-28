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
use std::sync::Weak;

/// The owner of all the KObjects in sysfs.
///
/// This structure holds strong references to the KObjects that are visible in sysfs. These
/// objects are organized into hierarchies that make it easier to implement sysfs.
pub struct KObjectStore {
    /// All of the devices added to the system.
    ///
    /// Used to populate the /sys/devices directory.
    pub devices: KObjectHandle,

    /// All of the device classes known to the system.
    ///
    /// Used to populate the /sys/class directory.
    pub class: KObjectHandle,

    /// All of the block devices known to the system.
    ///
    /// Used to populate the /ssy/block directory.
    pub block: KObjectHandle,

    /// All of the buses known to the system.
    ///
    /// Devices are organized first by bus and then by class. The more relevant bus for our
    /// purposes is the "virtual" bus, which is accessible via the `virtial_bus` method.
    pub bus: KObjectHandle,

    /// The devices in the system, organized by `DeviceMode` and `DeviceType`.
    ///
    /// Used to populate the /sys/dev directory. The KObjects descended from this KObject are
    /// the same objects referenced through the `devices` KObject. They are just organized in
    /// a different hierarchy. When populated in /sys/dev, they appear as symlinks to the
    /// canonical names in /sys/devices.
    pub dev: KObjectHandle,

    /// The block devices, organized by `DeviceType`.
    ///
    /// Used to populate the /sys/dev/block directory.
    dev_block: KObjectHandle,

    /// The char devices, organized by `DeviceType`.
    ///
    /// Used to populate the /sys/dev/char directory.
    dev_char: KObjectHandle,
}

impl KObjectStore {
    /// The virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> Bus {
        Bus::new(self.devices.get_or_create_child("virtual".into(), KObjectDirectory::new), None)
    }

    /// The device class used for virtual block devices.
    pub fn virtual_block_class(&self) -> Class {
        self.get_or_create_class("block".into(), self.virtual_bus())
    }

    /// The device class used for virtual thermal devices.
    pub fn virtual_thermal_class(&self) -> Class {
        self.get_or_create_class("thermal".into(), self.virtual_bus())
    }

    /// The device class used for virtual graphics devices.
    pub fn graphics_class(&self) -> Class {
        self.get_or_create_class("graphics".into(), self.virtual_bus())
    }

    /// The device class used for virtual input devices.
    pub fn input_class(&self) -> Class {
        self.get_or_create_class("input".into(), self.virtual_bus())
    }

    /// The device class used for virtual mem devices.
    pub fn mem_class(&self) -> Class {
        self.get_or_create_class("mem".into(), self.virtual_bus())
    }

    /// The device class used for virtual misc devices.
    pub fn misc_class(&self) -> Class {
        self.get_or_create_class("misc".into(), self.virtual_bus())
    }

    /// The device class used for virtual tty devices.
    pub fn tty_class(&self) -> Class {
        self.get_or_create_class("tty".into(), self.virtual_bus())
    }

    /// An incorrect device class.
    ///
    /// This class exposes the name "starnix" to userspace, which is incorrect. Instead, devices
    /// should use a class that represents their usage rather than their implementation.
    ///
    /// This class exists because a number of devices incorrectly use this class. We should fix
    /// those devices to report their proper class.
    pub fn starnix_class(&self) -> Class {
        self.get_or_create_class("starnix".into(), self.virtual_bus())
    }

    /// Get a bus by name.
    ///
    /// If the bus does not exist, this function will create it.
    pub fn get_or_create_bus(&self, name: &FsStr) -> Bus {
        let collection =
            Collection::new(self.bus.get_or_create_child(name, BusCollectionDirectory::new));
        Bus::new(self.devices.get_or_create_child(name, KObjectDirectory::new), Some(collection))
    }

    /// Get a class by name.
    ///
    /// If the bus does not exist, this function will create it.
    pub fn get_or_create_class(&self, name: &FsStr, bus: Bus) -> Class {
        let collection =
            Collection::new(self.class.get_or_create_child(name, KObjectSymlinkDirectory::new));
        Class::new(bus.kobject().get_or_create_child(name, KObjectDirectory::new), bus, collection)
    }

    /// Get a class by name.
    ///
    /// If the bus does not exist, this function will create it.
    pub fn get_or_create_class_with_ops<F, N>(
        &self,
        name: &FsStr,
        bus: Bus,
        create_class_sysfs_ops: F,
    ) -> Class
    where
        F: Fn(Weak<KObject>) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        let collection =
            Collection::new(self.class.get_or_create_child(name, create_class_sysfs_ops));
        Class::new(bus.kobject().get_or_create_child(name, KObjectDirectory::new), bus, collection)
    }

    /// Create a device and add that device to the store.
    ///
    /// Rather than use this function directly, you should register your device with the
    /// `DeviceRegistry`. The `DeviceRegistry` will create the KObject for the device as
    /// part of the registration process.
    ///
    /// If you create the device yourself, userspace will not be able to instantiate the
    /// device because the `DeviceType` will not be registered with the `DeviceRegistry`.
    pub(super) fn create_device<F, N>(
        &self,
        name: &FsStr,
        metadata: Option<DeviceMetadata>,
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

        if let Some(metadata) = &metadata {
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
        }

        if let Some(bus_collection) = &class.bus.collection {
            bus_collection.kobject().insert_child(device_kobject.clone());
        }

        Device::new(device_kobject, class, metadata)
    }

    /// Destroy a device.
    ///
    /// This function removes the KObject for the device from the store.
    ///
    /// Most clients hold weak references to KObjects, which means those references will become
    /// invalid shortly after this function is called.
    pub(super) fn destroy_device(&self, device: &Device) {
        let kobject = device.kobject();
        let name = kobject.name();
        // Remove the device from its views in the reverse order in which it was added.
        if let Some(bus_collection) = &device.class.bus.collection {
            bus_collection.kobject().remove_child(name);
        }
        if let Some(metadata) = &device.metadata {
            let device_number: FsString = metadata.device_type.to_string().into();
            match metadata.mode {
                DeviceMode::Block => {
                    self.dev_block.remove_child(device_number.as_ref());
                    self.block.remove_child(name);
                }
                DeviceMode::Char => {
                    self.dev_char.remove_child(device_number.as_ref());
                }
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
